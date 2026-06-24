//! Parse KiCad `.kicad_sym` libraries and expose pin geometry.
//!
//! This crate is the data model for a *bundled* view of KiCad symbol
//! libraries. It is intentionally decoupled from the user's local KiCad
//! install: callers (typically the CLI) decide which `.kicad_sym` files
//! to feed in. Tests use a small hand-written fixture library checked
//! into the crate.
//!
//! See `docs/layout-adr.md` ADR-1 (library access) and ADR-3
//! (orientation / mirroring).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lexpr::Value;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Orientation
// ---------------------------------------------------------------------------

/// One of the four 90 degree rotations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rotation {
    R0,
    R90,
    R180,
    R270,
}

impl Rotation {
    #[must_use]
    pub fn next_ccw(self) -> Self {
        match self {
            Self::R0 => Self::R90,
            Self::R90 => Self::R180,
            Self::R180 => Self::R270,
            Self::R270 => Self::R0,
        }
    }

    #[must_use]
    pub fn degrees(self) -> u16 {
        match self {
            Self::R0 => 0,
            Self::R90 => 90,
            Self::R180 => 180,
            Self::R270 => 270,
        }
    }
}

/// Rigid-motion orientation of a placed symbol.
///
/// Eight states: four rotations crossed with an optional mirror across the Y
/// axis (i.e. `x -> -x`). The mirror is applied *after* the rotation. This
/// matches the convention used in ADR-3 ("Orientation and mirroring in the
/// search space"). All eight states are reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Orientation {
    pub rotation: Rotation,
    /// Mirror across the Y axis (horizontal flip), applied *after* rotation.
    pub mirror_y: bool,
}

impl Orientation {
    /// The identity orientation: no rotation, no mirror.
    pub const IDENTITY: Self = Self {
        rotation: Rotation::R0,
        mirror_y: false,
    };

    /// All eight distinct orientations, in a stable order.
    pub const ALL: [Self; 8] = [
        Self {
            rotation: Rotation::R0,
            mirror_y: false,
        },
        Self {
            rotation: Rotation::R90,
            mirror_y: false,
        },
        Self {
            rotation: Rotation::R180,
            mirror_y: false,
        },
        Self {
            rotation: Rotation::R270,
            mirror_y: false,
        },
        Self {
            rotation: Rotation::R0,
            mirror_y: true,
        },
        Self {
            rotation: Rotation::R90,
            mirror_y: true,
        },
        Self {
            rotation: Rotation::R180,
            mirror_y: true,
        },
        Self {
            rotation: Rotation::R270,
            mirror_y: true,
        },
    ];

    /// Rotate by an additional 90 degrees CCW.
    #[must_use]
    pub fn rotate_90(self) -> Self {
        Self {
            rotation: self.rotation.next_ccw(),
            mirror_y: self.mirror_y,
        }
    }

    /// Toggle the mirror-Y flag.
    #[must_use]
    pub fn flip(self) -> Self {
        Self {
            rotation: self.rotation,
            mirror_y: !self.mirror_y,
        }
    }

    /// Apply the orientation to a local-frame point.
    #[must_use]
    pub fn apply_point(self, x: f64, y: f64) -> (f64, f64) {
        let (rx, ry) = match self.rotation {
            Rotation::R0 => (x, y),
            Rotation::R90 => (-y, x),
            Rotation::R180 => (-x, -y),
            Rotation::R270 => (y, -x),
        };
        if self.mirror_y { (-rx, ry) } else { (rx, ry) }
    }

    /// Apply the orientation to a pin direction (degrees, multiple of 90).
    ///
    /// Rotation adds the rotation amount mod 360. Mirror-Y reflects a
    /// direction by `angle' = (180 - angle) mod 360`, leaving 90 / 270
    /// unchanged and swapping 0 / 180.
    #[must_use]
    pub fn apply_angle(self, angle_deg: u16) -> u16 {
        let rotated = (u32::from(angle_deg) + u32::from(self.rotation.degrees())) % 360;
        let final_angle = if self.mirror_y {
            (360 + 180 - rotated) % 360
        } else {
            rotated
        };
        // safe: final_angle in [0, 360)
        u16::try_from(final_angle).unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Pin / Symbol / Library
// ---------------------------------------------------------------------------

/// KiCad pin electrical type (the first token of a `(pin <electrical>
/// …)` node). Drives ERC's driver analysis: a net with no driving pin
/// trips `power_pin_not_driven` / `pin_not_driven`, which the emitter
/// resolves with a `PWR_FLAG`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinElectrical {
    Input,
    Output,
    Bidirectional,
    TriState,
    Passive,
    Free,
    Unspecified,
    PowerIn,
    PowerOut,
    OpenCollector,
    OpenEmitter,
    NoConnect,
}

impl PinElectrical {
    /// Parse the electrical-type token KiCad writes after `pin`.
    /// Unknown tokens map to [`PinElectrical::Unspecified`].
    #[must_use]
    pub fn from_token(tok: &str) -> Self {
        match tok {
            "input" => Self::Input,
            "output" => Self::Output,
            "bidirectional" => Self::Bidirectional,
            "tri_state" => Self::TriState,
            "passive" => Self::Passive,
            "free" => Self::Free,
            "power_in" => Self::PowerIn,
            "power_out" => Self::PowerOut,
            "open_collector" => Self::OpenCollector,
            "open_emitter" => Self::OpenEmitter,
            "no_connect" => Self::NoConnect,
            // "unspecified" and anything unrecognised.
            _ => Self::Unspecified,
        }
    }

    /// True when ERC treats this pin as *driving* a net (i.e. it can
    /// satisfy a `power_in` / `input` pin's driver requirement). Mirrors
    /// KiCad's connectivity rules: an Output / Power-output / open-
    /// collector / open-emitter / tri-state / bidirectional pin drives;
    /// inputs, passives and power inputs do not.
    #[must_use]
    pub fn drives(self) -> bool {
        matches!(
            self,
            Self::Output
                | Self::PowerOut
                | Self::Bidirectional
                | Self::TriState
                | Self::OpenCollector
                | Self::OpenEmitter
        )
    }

    /// True when KiCad ERC *requires* this pin's net to carry a driver
    /// (else it reports `power_pin_not_driven` for a `power_in` pin, or
    /// `pin_not_driven` for an `input` pin). Passive / free /
    /// unspecified / no-connect / output pins impose no such
    /// requirement, so a net of only those needs no `PWR_FLAG`.
    #[must_use]
    pub fn requires_driver(self) -> bool {
        matches!(self, Self::PowerIn | Self::Input)
    }
}

#[derive(Debug, Clone)]
pub struct Pin {
    pub number: String,
    pub name: String,
    /// Pin-tip X in symbol-local frame (millimetres).
    pub x: f64,
    /// Pin-tip Y in symbol-local frame (millimetres).
    pub y: f64,
    /// Direction the pin points outward, in degrees. Always a multiple of 90.
    pub angle: u16,
    /// KiCad electrical type (`(pin <electrical> …)`).
    pub electrical: PinElectrical,
}

#[derive(Debug, Clone)]
pub struct TransformedPin {
    pub number: String,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub angle: u16,
    pub electrical: PinElectrical,
}

/// Verbatim mirror of an `lexpr::Value` sub-tree using the same three
/// shapes as the emitter's own `Sexpr` writer.
///
/// Used to stash the entire `(symbol …)` body from a parsed
/// `.kicad_sym` so the emitter can re-serialise it byte-for-byte under
/// `(lib_symbols)` (see CLAUDE.md § Visual quality invariants V3).
/// Kept opaque on purpose — we deliberately do *not* model graphical
/// primitives.
#[derive(Debug, Clone, PartialEq)]
pub enum RawSexpr {
    Atom(String),
    QString(String),
    List(Vec<RawSexpr>),
}

impl RawSexpr {
    /// Recursive conversion from a parsed `lexpr::Value`.
    ///
    /// `.kicad_sym` files only use lists of atoms, quoted strings, and
    /// numbers in practice; the other lexpr shapes are converted on a
    /// best-effort basis.
    #[must_use]
    pub fn from_lexpr(value: &Value) -> Self {
        match value {
            Value::Nil | Value::Null => Self::List(Vec::new()),
            Value::Bool(b) => Self::Atom(if *b { "true".into() } else { "false".into() }),
            Value::Number(n) => Self::Atom(format!("{n}")),
            Value::Char(c) => Self::Atom(c.to_string()),
            Value::String(s) => Self::QString(s.to_string()),
            Value::Symbol(s) => Self::Atom(s.to_string()),
            Value::Keyword(k) => Self::Atom(k.to_string()),
            Value::Bytes(b) => Self::Atom(format!("{b:?}")),
            Value::Cons(_) => {
                // Walk the proper list with `list_iter` so improper
                // tails (rare in .kicad_sym) just stop at the first
                // non-cons cdr.
                let items = list_iter(value).map(Self::from_lexpr).collect();
                Self::List(items)
            }
            Value::Vector(items) => Self::List(items.iter().map(Self::from_lexpr).collect()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Symbol {
    /// Library identifier in `"Lib:Name"` form.
    pub lib_id: String,
    /// Bare symbol name (without library prefix).
    pub name: String,
    pub pins: Vec<Pin>,
    /// Raw `(symbol …)` body captured at parse time, used by the
    /// emitter for verbatim `lib_symbols` passthrough. The second
    /// element is the bare symbol name; emitters rewrite it to the
    /// full `lib_id` before serialising.
    pub body: RawSexpr,
}

/// Axis-aligned bounding box of a symbol's graphical body in
/// symbol-local frame, millimetres.
///
/// This is the union of every drawn primitive (rectangles, polylines,
/// circles, arcs, beziers) inside the symbol's `(symbol "Name_0_1" …)`
/// sub-units. Pin stems are intentionally **excluded** — the body
/// bbox stops at the pin roots so a wire entering through a pin does
/// not register as crossing the body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalBbox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Symbol {
    #[must_use]
    pub fn pin_count(&self) -> usize {
        self.pins.len()
    }

    /// Axis-aligned bounding box of the symbol's graphical body in
    /// symbol-local frame (millimetres). Returns `None` when the
    /// symbol carries no drawable primitives (e.g. the abstract
    /// `power:*` glyphs whose body is a single polyline that is
    /// nonetheless captured — see specifics below).
    ///
    /// Walks the captured `(symbol …)` body's `RawSexpr` tree,
    /// extracting corner points from:
    /// * `(polyline (pts (xy x y) …))` — every `xy`.
    /// * `(rectangle (start x y) (end x y))` — both corners.
    /// * `(circle (center cx cy) (radius r))` — the four bbox corners
    ///   `(cx ± r, cy ± r)`.
    /// * `(arc (start) (mid) (end))` — three sampled points (a
    ///   conservative under-approximation; the arc may bulge slightly
    ///   beyond, but for the v0.1 fixture geometries this is below
    ///   the 0.5 mm router margin).
    /// * `(bezier (pts …))` — control hull (an over-approximation,
    ///   correct upper bound).
    ///
    /// Pin sub-trees are skipped. Sub-units (`(symbol "Name_0_1" …)`)
    /// are recursively walked.
    #[must_use]
    pub fn body_bbox(&self) -> Option<LocalBbox> {
        let mut x0 = f64::INFINITY;
        let mut y0 = f64::INFINITY;
        let mut x1 = f64::NEG_INFINITY;
        let mut y1 = f64::NEG_INFINITY;
        body_bbox_walk(&self.body, &mut x0, &mut y0, &mut x1, &mut y1);
        if x0.is_finite() && x1.is_finite() && y0.is_finite() && y1.is_finite() {
            Some(LocalBbox { x0, y0, x1, y1 })
        } else {
            None
        }
    }

    #[must_use]
    pub fn pin_by_name(&self, name: &str) -> Option<&Pin> {
        self.pins.iter().find(|p| p.name == name)
    }

    #[must_use]
    pub fn pin_by_number(&self, number: &str) -> Option<&Pin> {
        self.pins.iter().find(|p| p.number == number)
    }

    /// Pins in the given orientation, with positions and angles transformed.
    pub fn pins_in(&self, orient: Orientation) -> Vec<TransformedPin> {
        self.pins
            .iter()
            .map(|p| {
                let (x, y) = orient.apply_point(p.x, p.y);
                TransformedPin {
                    number: p.number.clone(),
                    name: p.name.clone(),
                    x,
                    y,
                    angle: orient.apply_angle(p.angle),
                    electrical: p.electrical,
                }
            })
            .collect()
    }
}

#[derive(Debug, Default, Clone)]
pub struct Library {
    by_lib_id: BTreeMap<String, Symbol>,
}

impl Library {
    /// Parse a single `.kicad_sym` file. The library prefix used in
    /// `lib_id` is the file stem (e.g. `"Device"` for `Device.kicad_sym`).
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, LoadError> {
        let path = path.as_ref();
        let prefix = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| LoadError::Structure {
                path: path.to_path_buf(),
                message: "could not derive library prefix from filename".into(),
            })?
            .to_owned();

        let text = std::fs::read_to_string(path).map_err(|source| LoadError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        let root = lexpr::from_str(&text).map_err(|e| LoadError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;

        Self::from_root_value(&root, &prefix, path)
    }

    fn from_root_value(root: &Value, prefix: &str, path: &Path) -> Result<Self, LoadError> {
        if head(root) != Some("kicad_symbol_lib") {
            return Err(LoadError::Structure {
                path: path.to_path_buf(),
                message: format!(
                    "expected top-level (kicad_symbol_lib ...), got head {:?}",
                    head(root)
                ),
            });
        }

        let mut by_lib_id = BTreeMap::new();
        for child in children_named(root, "symbol") {
            let name = list_iter(child)
                .nth(1)
                .and_then(as_str)
                .ok_or_else(|| LoadError::Structure {
                    path: path.to_path_buf(),
                    message: "(symbol ...) without a name".into(),
                })?
                .to_owned();
            let pins = collect_pins(child, path)?;
            let body = RawSexpr::from_lexpr(child);
            let lib_id = format!("{prefix}:{name}");
            by_lib_id.insert(
                lib_id.clone(),
                Symbol {
                    lib_id,
                    name,
                    pins,
                    body,
                },
            );
        }
        Ok(Self { by_lib_id })
    }

    /// Merge `other` into `self`. On `lib_id` collision, `other` wins
    /// (last-write-wins). Returns `self` so calls can be chained.
    #[must_use]
    pub fn merge(mut self, other: Library) -> Library {
        for (k, v) in other.by_lib_id {
            self.by_lib_id.insert(k, v);
        }
        self
    }

    pub fn lookup(&self, lib_id: &str) -> Option<&Symbol> {
        self.by_lib_id.get(lib_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Symbol)> {
        self.by_lib_id.iter().map(|(k, v)| (k.as_str(), v))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.by_lib_id.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_lib_id.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("io error reading {path}: {source}", path = path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse error in {path}: {message}", path = path.display())]
    Parse { path: PathBuf, message: String },
    #[error("structural error in {path}: {message}", path = path.display())]
    Structure { path: PathBuf, message: String },
}

// ---------------------------------------------------------------------------
// Pin extraction
// ---------------------------------------------------------------------------

fn collect_pins(symbol: &Value, path: &Path) -> Result<Vec<Pin>, LoadError> {
    let mut out = Vec::new();
    walk_pins(symbol, path, &mut out)?;
    Ok(out)
}

fn walk_pins(node: &Value, path: &Path, out: &mut Vec<Pin>) -> Result<(), LoadError> {
    for child in list_iter(node) {
        if !child.is_list() {
            continue;
        }
        match head(child) {
            Some("pin") => {
                out.push(parse_pin(child, path)?);
            }
            Some("symbol") => {
                // KiCad nests pins inside (symbol "Name_0_1" ...) sub-units.
                walk_pins(child, path, out)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_pin(node: &Value, path: &Path) -> Result<Pin, LoadError> {
    // (pin <electrical> <shape> (at X Y angle) (length L) (name "..") (number ".."))
    // First positional atom after `pin` is the electrical type.
    let electrical = list_iter(node)
        .nth(1)
        .and_then(as_str)
        .map_or(PinElectrical::Unspecified, PinElectrical::from_token);
    let at = find_child(node, "at").ok_or_else(|| LoadError::Structure {
        path: path.to_path_buf(),
        message: "pin without (at ...)".into(),
    })?;
    let mut nums = list_iter(at).skip(1).filter_map(as_f64);
    let x = nums.next().ok_or_else(|| LoadError::Structure {
        path: path.to_path_buf(),
        message: "pin (at ...) missing X".into(),
    })?;
    let y = nums.next().ok_or_else(|| LoadError::Structure {
        path: path.to_path_buf(),
        message: "pin (at ...) missing Y".into(),
    })?;
    let angle_f = nums.next().ok_or_else(|| LoadError::Structure {
        path: path.to_path_buf(),
        message: "pin (at ...) missing angle".into(),
    })?;

    if !angle_f.is_finite() || (angle_f - angle_f.round()).abs() > 1e-6 {
        return Err(LoadError::Structure {
            path: path.to_path_buf(),
            message: format!("pin angle {angle_f} is not an integer degree value"),
        });
    }
    // We've already verified angle_f is finite and integer-valued, but the
    // value could in principle be huge. Reject anything that doesn't fit a
    // reasonable degree range as a structural error.
    let rounded = angle_f.round();
    if !(-3600.0..=3600.0).contains(&rounded) {
        return Err(LoadError::Structure {
            path: path.to_path_buf(),
            message: format!("pin angle {rounded} is out of range"),
        });
    }
    // Safe: rounded is finite, integer-valued, within +/-3600.
    #[allow(clippy::cast_possible_truncation)] // bounded above
    let angle_int = rounded as i64;
    let angle_norm = angle_int.rem_euclid(360);
    if angle_norm % 90 != 0 {
        return Err(LoadError::Structure {
            path: path.to_path_buf(),
            message: format!("pin angle {angle_int} is not a multiple of 90"),
        });
    }
    // angle_norm is in 0..360 by rem_euclid; one of {0, 90, 180, 270}.
    let angle = u16::try_from(angle_norm).unwrap_or(0);

    let name = first_string_arg(node, "name").ok_or_else(|| LoadError::Structure {
        path: path.to_path_buf(),
        message: "pin without (name ...)".into(),
    })?;
    let number = first_string_arg(node, "number").ok_or_else(|| LoadError::Structure {
        path: path.to_path_buf(),
        message: "pin without (number ...)".into(),
    })?;

    Ok(Pin {
        number: number.to_owned(),
        name: name.to_owned(),
        x,
        y,
        angle,
        electrical,
    })
}

// ---------------------------------------------------------------------------
// body_bbox helpers
// ---------------------------------------------------------------------------

/// Recursively walk a `RawSexpr` subtree, expanding the running bbox
/// (`x0, y0, x1, y1`) to cover every graphical primitive's extent.
/// Pin sub-trees are skipped; sub-`(symbol …)` nodes recurse so
/// `Name_0_1` unit bodies are included.
fn body_bbox_walk(node: &RawSexpr, x0: &mut f64, y0: &mut f64, x1: &mut f64, y1: &mut f64) {
    let RawSexpr::List(items) = node else {
        return;
    };
    let head = items.first().and_then(raw_atom);
    match head {
        Some("pin") => {
            // Skip pin stems — body bbox stops at pin roots.
        }
        Some("polyline" | "bezier") => {
            if let Some(pts) = find_raw_child(items, "pts") {
                for it in pts {
                    if raw_head_of(it) == Some("xy") {
                        if let Some((x, y)) = raw_xy(it) {
                            extend_bbox(x0, y0, x1, y1, x, y);
                        }
                    }
                }
            }
        }
        Some("rectangle") => {
            if let Some(s) = raw_named_xy(items, "start") {
                extend_bbox(x0, y0, x1, y1, s.0, s.1);
            }
            if let Some(e) = raw_named_xy(items, "end") {
                extend_bbox(x0, y0, x1, y1, e.0, e.1);
            }
        }
        Some("circle") => {
            let center = raw_named_xy(items, "center");
            let radius = items.iter().find_map(|it| {
                if raw_head_of(it) == Some("radius") {
                    raw_first_f64_arg(it)
                } else {
                    None
                }
            });
            if let (Some((cx, cy)), Some(r)) = (center, radius) {
                extend_bbox(x0, y0, x1, y1, cx - r, cy - r);
                extend_bbox(x0, y0, x1, y1, cx + r, cy + r);
            }
        }
        Some("arc") => {
            for tag in ["start", "mid", "end"] {
                if let Some((x, y)) = raw_named_xy(items, tag) {
                    extend_bbox(x0, y0, x1, y1, x, y);
                }
            }
        }
        _ => {
            // Recurse into anything else: nested (symbol …) sub-units
            // or unknown wrapper nodes that may still contain graphics.
            for child in items.iter().skip(1) {
                body_bbox_walk(child, x0, y0, x1, y1);
            }
        }
    }
}

fn extend_bbox(x0: &mut f64, y0: &mut f64, x1: &mut f64, y1: &mut f64, x: f64, y: f64) {
    if x < *x0 {
        *x0 = x;
    }
    if x > *x1 {
        *x1 = x;
    }
    if y < *y0 {
        *y0 = y;
    }
    if y > *y1 {
        *y1 = y;
    }
}

fn raw_atom(node: &RawSexpr) -> Option<&str> {
    match node {
        RawSexpr::Atom(s) | RawSexpr::QString(s) => Some(s.as_str()),
        RawSexpr::List(_) => None,
    }
}

fn raw_head_of(node: &RawSexpr) -> Option<&str> {
    if let RawSexpr::List(items) = node {
        items.first().and_then(raw_atom)
    } else {
        None
    }
}

fn find_raw_child<'a>(items: &'a [RawSexpr], name: &str) -> Option<&'a [RawSexpr]> {
    for it in items.iter().skip(1) {
        if let RawSexpr::List(list) = it {
            if list.first().and_then(raw_atom) == Some(name) {
                return Some(&list[1..]);
            }
        }
    }
    None
}

/// Locate the first `(name x y …)` child under `items` and return
/// `(x, y)` if found.
fn raw_named_xy(items: &[RawSexpr], name: &str) -> Option<(f64, f64)> {
    items.iter().find_map(|it| {
        if raw_head_of(it) == Some(name) {
            raw_xy(it)
        } else {
            None
        }
    })
}

fn raw_xy(node: &RawSexpr) -> Option<(f64, f64)> {
    let RawSexpr::List(items) = node else {
        return None;
    };
    let mut nums = items.iter().skip(1).filter_map(raw_as_f64);
    let x = nums.next()?;
    let y = nums.next()?;
    Some((x, y))
}

fn raw_first_f64_arg(node: &RawSexpr) -> Option<f64> {
    let RawSexpr::List(items) = node else {
        return None;
    };
    items.iter().skip(1).find_map(raw_as_f64)
}

fn raw_as_f64(n: &RawSexpr) -> Option<f64> {
    match n {
        RawSexpr::Atom(s) | RawSexpr::QString(s) => s.parse::<f64>().ok(),
        RawSexpr::List(_) => None,
    }
}

// ---------------------------------------------------------------------------
// lexpr helpers (modelled on crates/spice2kicad/tests/common/sexp.rs)
// ---------------------------------------------------------------------------

fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    if let Some(it) = v.list_iter() {
        Box::new(it)
    } else {
        Box::new(std::iter::empty())
    }
}

fn head(v: &Value) -> Option<&str> {
    list_iter(v).next().and_then(as_str)
}

fn children_named<'a>(v: &'a Value, name: &str) -> Vec<&'a Value> {
    list_iter(v)
        .filter(|c| c.is_list() && head(c) == Some(name))
        .collect()
}

fn find_child<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    children_named(v, name).into_iter().next()
}

fn first_string_arg<'a>(v: &'a Value, name: &str) -> Option<&'a str> {
    let node = find_child(v, name)?;
    list_iter(node).nth(1).and_then(as_str)
}

fn as_str(v: &Value) -> Option<&str> {
    if let Some(s) = v.as_symbol() {
        return Some(s);
    }
    if let Some(s) = v.as_str() {
        return Some(s);
    }
    if let Some(k) = v.as_keyword() {
        return Some(k);
    }
    None
}

fn as_f64(v: &Value) -> Option<f64> {
    // KiCad files mix integer and float atoms (e.g. "0" vs "5.08").
    #[allow(clippy::cast_precision_loss)] // i64 -> f64 widening for small numerics is fine here.
    let from_int = v.as_i64().map(|i| i as f64);
    v.as_f64().or(from_int)
}
