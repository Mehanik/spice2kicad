//! Public router data types.
//!
//! [`RouteRequest`] / [`RouteResult`] are the stable interface between
//! `kicad-emitter` and `spice-route`. Internal stage modules
//! (`rails`, `steiner`, `cleanup`) consume these but layer their own
//! private types on top.

use kicad_symbols::Library;
use lexpr::Value as Sexpr;

/// One pin on a routed net.
#[derive(Debug, Clone)]
pub struct PinRef {
    /// Index of the placed element this pin belongs to (caller-defined).
    pub element_idx: usize,
    /// KiCad pin number on that element.
    pub pin_number: u16,
    /// World X in millimetres, after rotation.
    pub x_mm: f64,
    /// World Y in millimetres, after rotation.
    pub y_mm: f64,
    /// Outward direction of the pin in world coordinates, post-rotation.
    pub outward: Direction,
}

/// Cardinal direction the pin's stem points away from its symbol body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// One net to route, with its pins and class hint.
#[derive(Debug, Clone)]
pub struct NetSpec {
    pub name: String,
    pub class: spice_layout::net_class::NetClass,
    pub pins: Vec<PinRef>,
}

/// Input to [`crate::route`].
#[derive(Debug, Clone)]
pub struct RouteRequest<'a> {
    pub nets: &'a [NetSpec],
    /// Sheet scope (root or hierarchical sheet name) — used for
    /// scoping junctions and labels in future stages.
    pub scope: &'a str,
    /// Symbol library, used by Stage 1 to verify a `power:*` lib_id
    /// resolves before emitting an instance. `None` skips the check
    /// (every requested `power:*` symbol is assumed to exist).
    pub library: Option<&'a Library>,
    /// Sheet UUID — used to populate the per-symbol `(instances …)`
    /// block kicad-cli requires for netlist export.
    pub sheet_uuid: &'a str,
    /// Project name — written into the `(instances (project "<name>" …))`
    /// block of every emitted symbol.
    pub project_name: &'a str,
    /// Symbol bodies the router should treat as obstacles when
    /// choosing per-net L-shapes. A wire that passes through a body
    /// is reflected to the alternate L corner; if both choices cross
    /// a body the conflict is recorded as a warning. Empty slice
    /// disables obstacle avoidance (legacy behaviour).
    pub obstacles: &'a [Bbox],
}

/// Axis-aligned bounding box in world millimetres. Used by the router
/// to detect wires crossing symbol bodies (V8/V9 readability invariant).
#[derive(Debug, Clone, Copy)]
pub struct Bbox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Bbox {
    /// Build a small bbox centred on a pin coordinate, used by V11
    /// foreign-pin avoidance to reuse the segment-vs-bbox plumbing
    /// already exercised for V12 symbol-body avoidance.
    ///
    /// The half-extent is slightly less than half a grid cell so a
    /// segment whose *endpoint* sits exactly on the pin (the
    /// owning-net case the caller is responsible for filtering out)
    /// would not be flagged — only a segment whose path strictly
    /// penetrates the box's interior triggers
    /// [`Bbox::intersects_segment`]. The 0.5 mm half-extent picks
    /// up pin coords located on the 1.27 mm grid without trapping
    /// segments routed one grid cell away.
    #[must_use]
    pub fn from_point(x_mm: f64, y_mm: f64) -> Self {
        // 0.5 mm half-extent: well inside one grid cell (1.27 mm)
        // and comfortably outside the 0.1 mm "graze" tolerance the
        // `intersects_segment` test uses for pins on the boundary.
        let h = 0.5_f64;
        Self {
            x0: x_mm - h,
            y0: y_mm - h,
            x1: x_mm + h,
            y1: y_mm + h,
        }
    }

    /// Strict interior intersection of an axis-parallel segment with
    /// this bbox, ignoring touches that only graze the boundary
    /// (where pins legitimately attach).
    #[must_use]
    pub fn intersects_segment(&self, x1: f64, y1: f64, x2: f64, y2: f64) -> bool {
        // 0.1 mm tolerance: a segment whose endpoint sits exactly on
        // the bbox edge (i.e. coincides with a pin on that edge) does
        // not count. Anything penetrating ≥ 0.1 mm interior does.
        let eps = 0.1_f64;
        let xlo = self.x0 + eps;
        let xhi = self.x1 - eps;
        let ylo = self.y0 + eps;
        let yhi = self.y1 - eps;
        if xlo >= xhi || ylo >= yhi {
            return false;
        }
        if x1.max(x2) <= xlo || x1.min(x2) >= xhi {
            return false;
        }
        if y1.max(y2) <= ylo || y1.min(y2) >= yhi {
            return false;
        }
        // For axis-aligned segments the quick reject above plus the
        // axis-band check below is exact. For diagonals a Liang-Barsky
        // clip would be needed, but the router emits only axis-aligned
        // segments by construction.
        let dx = x2 - x1;
        let dy = y2 - y1;
        if dx.abs() < f64::EPSILON {
            // Vertical: x must be inside, segment y must overlap band.
            x1 > xlo && x1 < xhi && y1.min(y2) < yhi && y1.max(y2) > ylo
        } else if dy.abs() < f64::EPSILON {
            y1 > ylo && y1 < yhi && x1.min(x2) < xhi && x1.max(x2) > xlo
        } else {
            // Liang-Barsky clip for non-axis-aligned segments.
            let mut t0 = 0.0_f64;
            let mut t1 = 1.0_f64;
            for (p, q) in [
                (-dx, x1 - xlo),
                (dx, xhi - x1),
                (-dy, y1 - ylo),
                (dy, yhi - y1),
            ] {
                if p.abs() < f64::EPSILON {
                    if q < 0.0 {
                        return false;
                    }
                    continue;
                }
                let t = q / p;
                if p < 0.0 {
                    t0 = t0.max(t);
                } else {
                    t1 = t1.min(t);
                }
            }
            t1 - t0 > 1e-3
        }
    }
}

/// Output of [`crate::route`].
///
/// `sexprs` is a flat list of `(wire …)` / `(junction …)` /
/// `(symbol …)` / `(label …)` nodes ready to splice into the
/// emitted schematic. Order is not significant — the emitter may
/// re-order before final write.
#[derive(Debug, Clone, Default)]
pub struct RouteResult {
    pub sexprs: Vec<Sexpr>,
    /// Diagnostics from rip-up failures, missing power symbols, etc.
    pub warnings: Vec<String>,
}

/// One axis-parallel wire segment in world millimetres.
///
/// Used internally by stage modules and exposed for tests / future
/// sub-crate consumers. The on-grid invariant (1.27 mm) is the
/// caller's responsibility.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

/// One routed net's intermediate result, before serialisation.
#[derive(Debug, Clone, Default)]
pub struct RoutedNet {
    pub segments: Vec<Segment>,
    pub junctions: Vec<(f64, f64)>,
}
