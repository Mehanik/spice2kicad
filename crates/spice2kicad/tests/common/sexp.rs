//! Thin query wrapper over a parsed KiCad `.kicad_sch` S-expression.
//!
//! KiCad schematics are deeply nested s-exprs. Tests don't want to walk
//! `lexpr::Value` by hand every time, so this module exposes a few
//! domain-shaped accessors (symbols by refdes, library id, position) plus
//! a small set of relation predicates used by `sexp_constraints.rs`.
//!
//! Coordinate convention: KiCad stores `(at X Y [angle])` in millimetres.
//! `Position` keeps them as `f64`; comparisons that need tolerance use
//! [`approx_eq`].
//!
//! Predicates that talk about *placement* are pin-anchored. They look up
//! the symbol's pin geometry from the bundled fixture
//! [`Library`](kicad_symbols::Library), apply the schematic's
//! `(at … angle)` plus optional `(mirror …)` to derive an
//! [`Orientation`](kicad_symbols::Orientation), and compare *world pin
//! coordinates*, not symbol origins. This is what
//! `docs/CLAUDE.md` ("Layout invariants") and `docs/layout-roadmap.md` §2
//! require: alignment is a pin-to-pin relation, and mixing in symbol
//! orientation makes the center-based shortcut break silently.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::OnceLock;

use kicad_symbols::{Library, Orientation, Rotation, Symbol as LibSymbol, TransformedPin};
use lexpr::Value;

const POS_TOL_MM: f64 = 0.01;
/// KiCad schematic grid: 50 mil = 1.27 mm.
const GRID_MM: f64 = 1.27;

#[derive(Debug)]
pub struct KicadSch {
    root: Value,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub angle: f64,
}

#[derive(Debug)]
pub struct Symbol<'a> {
    node: &'a Value,
}

impl KicadSch {
    pub fn parse(src: &str) -> Result<Self, String> {
        let root = lexpr::from_str(src).map_err(|e| format!("lexpr parse: {e}"))?;
        if head(&root) != Some("kicad_sch") {
            return Err(format!(
                "expected (kicad_sch ...), got head {:?}",
                head(&root)
            ));
        }
        Ok(Self { root })
    }

    /// Every `(symbol ...)` instance directly under the root.
    pub fn symbols(&self) -> Vec<Symbol<'_>> {
        children(&self.root, "symbol")
            .into_iter()
            .map(|node| Symbol { node })
            .collect()
    }

    /// Every hierarchical-`(sheet …)` block directly under the root,
    /// surfaced as a [`Symbol`] view whose `Reference` is the
    /// `Sheetname` property and whose `lib_id` is the `Sheetfile`.
    /// Combined with [`Self::symbols`] in [`Self::refdes_set`] so test
    /// assertions can treat sheet instances and flat symbols
    /// uniformly.
    pub fn sheets(&self) -> Vec<Symbol<'_>> {
        children(&self.root, "sheet")
            .into_iter()
            .map(|node| Symbol { node })
            .collect()
    }

    pub fn symbol(&self, refdes: &str) -> Option<Symbol<'_>> {
        self.symbols()
            .into_iter()
            .chain(self.sheets())
            .find(|s| s.refdes() == Some(refdes))
    }

    pub fn refdes_set(&self) -> Vec<String> {
        self.symbols()
            .iter()
            .chain(self.sheets().iter())
            .filter_map(|s| s.refdes().map(str::to_string))
            .collect()
    }
}

impl Symbol<'_> {
    pub fn lib_id(&self) -> Option<&str> {
        first_string_arg(self.node, "lib_id")
    }

    pub fn position(&self) -> Option<Position> {
        let at = find_child(self.node, "at")?;
        let mut nums = list_iter(at).skip(1).filter_map(as_f64);
        let x = nums.next()?;
        let y = nums.next()?;
        let angle = nums.next().unwrap_or(0.0);
        Some(Position { x, y, angle })
    }

    /// Schematic-level orientation: rotation from `(at … angle)` plus
    /// optional sibling `(mirror x|y)`.
    ///
    /// Panics if both `(mirror x)` and `(mirror y)` are present (would be a
    /// malformed schematic in practice — KiCad emits at most one).
    pub fn orientation(&self) -> Option<Orientation> {
        let pos = self.position()?;
        let rotation = rotation_from_degrees(pos.angle).unwrap_or_else(|| {
            panic!(
                "{}: rotation {} is not a multiple of 90",
                self.refdes().unwrap_or("<?>"),
                pos.angle
            )
        });
        let mut has_x = false;
        let mut has_y = false;
        for m in children(self.node, "mirror") {
            match list_iter(m).nth(1).and_then(as_str) {
                Some("x") => has_x = true,
                Some("y") => has_y = true,
                _ => {}
            }
        }
        assert!(
            !(has_x && has_y),
            "{}: schematic has both (mirror x) and (mirror y); ambiguous",
            self.refdes().unwrap_or("<?>"),
        );
        // KiCad's `(mirror y)` flips horizontally (across the Y axis), which
        // is exactly `Orientation::mirror_y`. `(mirror x)` flips vertically;
        // we model it as `mirror_y` combined with a 180° rotation, since
        // `flip_y ∘ flip_x = R180`.
        let orient = if has_x {
            Orientation {
                rotation: rotate_180(rotation),
                mirror_y: true,
            }
        } else {
            Orientation {
                rotation,
                mirror_y: has_y,
            }
        };
        Some(orient)
    }

    /// Read a `(property "Name" "Value" ...)` slot. KiCad stores Reference,
    /// Value, Footprint, etc. this way.
    pub fn property(&self, name: &str) -> Option<&str> {
        for prop in children(self.node, "property") {
            let mut it = list_iter(prop).skip(1);
            let key = it.next().and_then(as_str);
            let val = it.next().and_then(as_str);
            if key == Some(name) {
                return val;
            }
        }
        None
    }

    pub fn refdes(&self) -> Option<&str> {
        // For `(symbol …)` we read `Reference`; for `(sheet …)` we
        // surface `Sheetname` so hierarchical-sheet instances appear
        // as components in test assertions.
        self.property("Reference")
            .or_else(|| self.property("Sheetname"))
    }
}

// --- relation predicates -------------------------------------------------

/// All listed refdes appear as `(symbol ...)` instances.
pub fn assert_has_components(sch: &KicadSch, refdes: &[&str]) {
    let present: std::collections::BTreeSet<String> = sch.refdes_set().into_iter().collect();
    let missing: Vec<&&str> = refdes.iter().filter(|r| !present.contains(**r)).collect();
    assert!(
        missing.is_empty(),
        "expected components {missing:?} not found in schematic; have {present:?}"
    );
}

/// None of the listed refdes appear (e.g. `;@ ignore`d sim scaffolding).
pub fn assert_lacks_components(sch: &KicadSch, refdes: &[&str]) {
    let present: std::collections::BTreeSet<String> = sch.refdes_set().into_iter().collect();
    let leaked: Vec<&&str> = refdes.iter().filter(|r| present.contains(**r)).collect();
    assert!(
        leaked.is_empty(),
        "ignored components {leaked:?} leaked into schematic"
    );
}

pub fn assert_lib_id(sch: &KicadSch, refdes: &str, expected: &str) {
    let sym = sch
        .symbol(refdes)
        .unwrap_or_else(|| panic!("symbol {refdes} not in schematic"));
    let got = sym.lib_id().unwrap_or("<no lib_id>");
    assert_eq!(got, expected, "{refdes}: lib_id mismatch");
}

/// Assert the symbol's origin lies on the 1.27 mm KiCad schematic grid.
pub fn assert_on_grid(sch: &KicadSch, refdes: &str) {
    let p = pos(sch, refdes);
    assert!(
        on_grid(p.x) && on_grid(p.y),
        "{refdes} not on grid: ({}, {}) is not a multiple of {GRID_MM} mm",
        p.x,
        p.y
    );
}

/// Assert every symbol in the schematic lies on the 1.27 mm grid.
pub fn assert_all_on_grid(sch: &KicadSch) {
    for sym in sch.symbols() {
        if let Some(refdes) = sym.refdes() {
            assert_on_grid(sch, refdes);
        }
    }
}

/// `b` is to the right of `a`, pin-anchored on the X axis.
///
/// Picks `a`'s rightmost world-X pin and `b`'s leftmost world-X pin, then
/// asserts `b.left.x > a.right.x`. Per `docs/annotation-spec.md`,
/// `place=right-of` describes horizontal *direction*, not pin-to-pin
/// alignment — it does not promise that any pair of facing pins shares
/// a Y row, and for multi-pin asymmetric symbols (e.g. `Q_NPN_BCE`,
/// whose B sits at one Y and C/E at another) such a row literally
/// cannot exist while the elements are also `align horizontal`. So we
/// only enforce the X-ordering and leave Y matters to `align`.
pub fn assert_right_of(sch: &KicadSch, b: &str, a: &str) {
    let (a_pins, _a_orient) = world_pins(sch, a);
    let (b_pins, _b_orient) = world_pins(sch, b);

    let a_max_x = max_by(&a_pins, |p| p.x);
    let b_min_x = min_by(&b_pins, |p| p.x);

    assert!(
        b_min_x > a_max_x + POS_TOL_MM,
        "{b} is not right of {a}: {b}.min_pin_x={b_min_x} <= {a}.max_pin_x={a_max_x}"
    );
}

/// All listed refdes share a connecting-pin Y row. v0.1 requires uniform
/// orientation across the group (see `docs/annotation-spec.md` §9 — mixed
/// orientation under `align` is an open question). Under uniform
/// orientation, equality of pin Y across symbols reduces to equality of
/// origin Y.
pub fn assert_aligned_horizontal(sch: &KicadSch, refdes: &[&str]) {
    require_uniform_orientation(sch, refdes, "horizontal");
    let positions: Vec<(String, Position)> =
        refdes.iter().map(|r| ((*r).into(), pos(sch, r))).collect();
    let (_, first) = &positions[0];
    for (name, p) in &positions[1..] {
        assert!(
            approx_eq(first.y, p.y, POS_TOL_MM),
            "horizontal-align violation: {name}.y={} vs reference y={}",
            p.y,
            first.y
        );
    }
}

pub fn assert_aligned_vertical(sch: &KicadSch, refdes: &[&str]) {
    require_uniform_orientation(sch, refdes, "vertical");
    let positions: Vec<(String, Position)> =
        refdes.iter().map(|r| ((*r).into(), pos(sch, r))).collect();
    let (_, first) = &positions[0];
    for (name, p) in &positions[1..] {
        assert!(
            approx_eq(first.x, p.x, POS_TOL_MM),
            "vertical-align violation: {name}.x={} vs reference x={}",
            p.x,
            first.x
        );
    }
}

// --- helpers used by predicates ------------------------------------------

fn pos(sch: &KicadSch, refdes: &str) -> Position {
    sch.symbol(refdes)
        .unwrap_or_else(|| panic!("symbol {refdes} not in schematic"))
        .position()
        .unwrap_or_else(|| panic!("symbol {refdes} has no (at ...) position"))
}

fn require_uniform_orientation(sch: &KicadSch, refdes: &[&str], axis: &str) {
    let orientations: Vec<(String, Orientation)> = refdes
        .iter()
        .map(|r| {
            let sym = sch
                .symbol(r)
                .unwrap_or_else(|| panic!("symbol {r} not in schematic"));
            let o = sym
                .orientation()
                .unwrap_or_else(|| panic!("symbol {r} has no (at ...) position"));
            ((*r).to_string(), o)
        })
        .collect();
    if orientations.is_empty() {
        return;
    }
    // Compatibility relation per axis. V7 (symmetry-aware placement)
    // pairs aligned elements with mirrored orientations: same rotation,
    // but one half flipped about the symmetry axis. Such mirrors do
    // not break the underlying alignment invariant — the origin row /
    // column still maps to the same pin row / column — so we accept
    // them here. The previous "strict equality" rule is documented in
    // docs/annotation-spec.md §9 (open question on align + mixed
    // orientation).
    let first = orientations[0].1;
    let compatible = |a: Orientation, b: Orientation| -> bool {
        if a == b {
            return true;
        }
        if a.rotation != b.rotation {
            return false;
        }
        match axis {
            // horizontal-align: pins share Y; mirror across Y axis
            // (mirror_y differs) preserves Y of every pin.
            // vertical-align: pins share X; the same flip preserves X
            // of every pin (mirror is across the symbol's Y axis,
            // i.e. swaps left/right; X positions per row stay equal
            // when both halves rotate identically).
            "horizontal" | "vertical" => a.mirror_y != b.mirror_y,
            _ => false,
        }
    };
    for (name, o) in &orientations[1..] {
        assert!(
            compatible(first, *o),
            "{axis}-align with incompatible orientation: \
             {} has {:?}, {} has {:?}",
            orientations[0].0,
            first,
            name,
            o
        );
    }
}

/// Symbol's pins in world coordinates (origin + transformed pin offset).
fn world_pins(sch: &KicadSch, refdes: &str) -> (Vec<TransformedPin>, Orientation) {
    let sym = sch
        .symbol(refdes)
        .unwrap_or_else(|| panic!("symbol {refdes} not in schematic"));
    let lib_id = sym
        .lib_id()
        .unwrap_or_else(|| panic!("symbol {refdes} has no lib_id"));
    let lib = fixture_library();
    let lib_sym: &LibSymbol = lib.lookup(lib_id).unwrap_or_else(|| {
        panic!(
            "symbol {refdes}: lib_id {lib_id:?} not found in fixture library; \
             add it to crates/kicad-symbols/tests/fixtures/"
        )
    });
    let p = sym
        .position()
        .unwrap_or_else(|| panic!("symbol {refdes} has no (at ...) position"));
    let orient = sym.orientation().expect("orientation");
    let pins = lib_sym
        .pins_in(orient)
        .into_iter()
        .map(|tp| TransformedPin {
            number: tp.number,
            name: tp.name,
            x: p.x + tp.x,
            y: p.y + tp.y,
            angle: tp.angle,
            electrical: tp.electrical,
        })
        .collect();
    (pins, orient)
}

fn fixture_library() -> &'static Library {
    static LIB: OnceLock<Library> = OnceLock::new();
    LIB.get_or_init(|| {
        let dir = fixture_dir();
        let device =
            Library::from_file(dir.join("Device.kicad_sym")).expect("load Device fixture library");
        let spice = Library::from_file(dir.join("Simulation_SPICE.kicad_sym"))
            .expect("load Simulation_SPICE fixture library");
        device.merge(spice)
    })
}

/// Workspace-relative path to the kicad-symbols fixture directory.
///
/// `CARGO_MANIFEST_DIR` for this crate is `<root>/crates/spice2kicad`; go
/// up two levels to reach the workspace root, then back down into the
/// fixture directory.
fn fixture_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent() // crates/
        .and_then(std::path::Path::parent) // workspace root
        .expect("workspace root")
        .join("crates/kicad-symbols/tests/fixtures")
}

fn max_by<T, F: Fn(&T) -> f64>(xs: &[T], f: F) -> f64 {
    xs.iter().map(&f).fold(f64::NEG_INFINITY, f64::max)
}

fn min_by<T, F: Fn(&T) -> f64>(xs: &[T], f: F) -> f64 {
    xs.iter().map(&f).fold(f64::INFINITY, f64::min)
}

fn approx_eq(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

fn on_grid(v: f64) -> bool {
    let q = (v / GRID_MM).round();
    (v - q * GRID_MM).abs() <= POS_TOL_MM
}

fn rotation_from_degrees(deg: f64) -> Option<Rotation> {
    if !deg.is_finite() {
        return None;
    }
    let rounded = deg.round();
    if (deg - rounded).abs() > 1e-6 {
        return None;
    }
    if !(-3600.0..=3600.0).contains(&rounded) {
        return None;
    }
    // Safe: bounded above; finite; integer-valued.
    #[allow(clippy::cast_possible_truncation)]
    let as_int = rounded as i64;
    match as_int.rem_euclid(360) {
        0 => Some(Rotation::R0),
        90 => Some(Rotation::R90),
        180 => Some(Rotation::R180),
        270 => Some(Rotation::R270),
        _ => None,
    }
}

fn rotate_180(r: Rotation) -> Rotation {
    match r {
        Rotation::R0 => Rotation::R180,
        Rotation::R90 => Rotation::R270,
        Rotation::R180 => Rotation::R0,
        Rotation::R270 => Rotation::R90,
    }
}

// --- lexpr helpers --------------------------------------------------------

/// First atom of a list, if it's a symbol/keyword/string.
fn head(v: &Value) -> Option<&str> {
    let first = list_iter(v).next()?;
    as_str(first)
}

/// Iterate children of a list (any cons-list / proper list / vector).
fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    if let Some(it) = v.list_iter() {
        Box::new(it)
    } else {
        Box::new(std::iter::empty())
    }
}

/// Direct children whose head matches `name`. Skips the head itself.
fn children<'a>(v: &'a Value, name: &str) -> Vec<&'a Value> {
    list_iter(v)
        .filter(|c| c.is_list() && head(c) == Some(name))
        .collect()
}

fn find_child<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    children(v, name).into_iter().next()
}

/// `(name "value" ...)` -> `Some("value")`.
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
    #[allow(clippy::cast_precision_loss)]
    v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))
}
