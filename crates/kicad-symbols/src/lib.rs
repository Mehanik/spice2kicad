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
}

#[derive(Debug, Clone)]
pub struct TransformedPin {
    pub number: String,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub angle: u16,
}

#[derive(Debug, Clone)]
pub struct Symbol {
    /// Library identifier in `"Lib:Name"` form.
    pub lib_id: String,
    /// Bare symbol name (without library prefix).
    pub name: String,
    pub pins: Vec<Pin>,
}

impl Symbol {
    #[must_use]
    pub fn pin_count(&self) -> usize {
        self.pins.len()
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
            let lib_id = format!("{prefix}:{name}");
            by_lib_id.insert(lib_id.clone(), Symbol { lib_id, name, pins });
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
    })
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
