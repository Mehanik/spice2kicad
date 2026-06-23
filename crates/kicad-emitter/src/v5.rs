//! V5 first-segment-outward measurement, shared between the
//! routing-aware orientation-refinement phase ([`crate::refine`]) and
//! the V5 verifier (`spice2kicad/tests/electrical_safety.rs`).
//!
//! V5 (CLAUDE.md "Visual quality invariants") asks that the first wire
//! segment leaving every pin extends in the pin's *outward* direction
//! (away from the symbol body), so the schematic reads the way an
//! engineer would draw it. A V5 violation is born in the router's
//! post-construction conflict-resolution passes
//! (`spice_route::conflict::{avoid_foreign_pins, avoid_obstacles}`),
//! which are invisible to any pre-route placement model — so the only
//! faithful way to count V5 is to route for real and measure the
//! emitted wires.
//!
//! This module is the single source of truth for that measurement.
//! Both the refinement (which uses it as the router-in-the-loop oracle
//! that selects element orientations) and the verifier call
//! [`count_outward_violations`], so the two can never drift. The rule
//! here mirrors `electrical_safety::v5_first_segment_extends_outward`
//! exactly: KEEP THE TWO IN SYNC — if you change the rule here, update
//! the verifier's documentation, and vice versa.

/// Outward grid step in micrometres, one KiCad grid cell (1.27 mm).
const STEP_UM: i64 = 1270;

/// An axis-aligned wire segment in world millimetres:
/// `((x1, y1), (x2, y2))`.
pub type WireSegment = ((f64, f64), (f64, f64));

/// Quantise a millimetre coordinate to integer micrometres. Inputs sit
/// on the 1.27 mm grid, so 1 µm resolution is exact for the
/// coord-equality model V5/V11 use.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn qkey(x: f64, y: f64) -> (i64, i64) {
    ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64)
}

/// A pin to test, in world millimetres, with its outward direction in
/// the world (Y-down) frame: degrees `0`=Right, `90`=Down, `180`=Left,
/// `270`=Up. A power-glyph sentinel (`u16::MAX`) is skipped by
/// [`count_outward_violations`].
#[derive(Debug, Clone)]
pub struct PinProbe {
    pub refdes: String,
    pub pin_number: String,
    pub x_mm: f64,
    pub y_mm: f64,
    pub angle: u16,
}

/// One V5 violation: a pin with an incident wire whose first segment
/// does not extend outward, *and* which is not a pure interior-trunk
/// coincidence (those are a tracked v0.2 limitation, not counted).
#[derive(Debug, Clone)]
pub struct Violation {
    pub refdes: String,
    pub pin_number: String,
    pub x_mm: f64,
    pub y_mm: f64,
    pub angle: u16,
}

/// Count V5 first-segment-outward violations over a set of pins and the
/// router's emitted wire segments. Mirrors
/// `electrical_safety::v5_first_segment_extends_outward` exactly:
///
/// * Power-glyph pins (`angle == u16::MAX`) and non-cardinal angles are
///   skipped (out of scope).
/// * A pin with no incident or interior wire is skipped (one-pin nets,
///   label-anchored, or unconnected).
/// * A pin is satisfied if *any* incident wire endpoint extends in the
///   outward direction.
/// * A pin sitting purely on a wire's interior (trunk pass-through) with
///   no incident endpoint is a known limitation — reported via the
///   returned flag, not counted as a violation.
///
/// Returns the counted violations. Pure interior-trunk pins are *not*
/// included (matching the verifier's "report but don't fail" bucket).
#[must_use]
pub fn count_outward_violations(pins: &[PinProbe], segments: &[WireSegment]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for p in pins {
        if p.angle == u16::MAX {
            continue;
        }
        let pk = qkey(p.x_mm, p.y_mm);
        let (dx, dy) = match p.angle % 360 {
            0 => (STEP_UM, 0),
            90 => (0, STEP_UM),
            180 => (-STEP_UM, 0),
            270 => (0, -STEP_UM),
            _ => continue,
        };
        let mut endpoint_dirs: Vec<(i64, i64)> = Vec::new();
        let mut interior_through = false;
        for (a, b) in segments {
            let ka = qkey(a.0, a.1);
            let kb = qkey(b.0, b.1);
            if ka == pk {
                endpoint_dirs.push((kb.0 - ka.0, kb.1 - ka.1));
            } else if kb == pk {
                endpoint_dirs.push((ka.0 - kb.0, ka.1 - kb.1));
            } else if interior_grid_coords((*a, *b)).contains(&pk) {
                interior_through = true;
            }
        }
        if endpoint_dirs.is_empty() && !interior_through {
            continue;
        }
        let outward_ok = endpoint_dirs.iter().any(|&(ex, ey)| {
            if ex != 0 && ey != 0 {
                return false;
            }
            let nx = ex.signum() * STEP_UM;
            let ny = ey.signum() * STEP_UM;
            (nx, ny) == (dx, dy)
        });
        if outward_ok {
            continue;
        }
        // Pure interior-trunk pin: a known limitation, not counted.
        if interior_through && endpoint_dirs.is_empty() {
            continue;
        }
        violations.push(Violation {
            refdes: p.refdes.clone(),
            pin_number: p.pin_number.clone(),
            x_mm: p.x_mm,
            y_mm: p.y_mm,
            angle: p.angle,
        });
    }
    violations
}

/// Quantised interior coords of an axis-aligned segment (exclusive of
/// the two endpoints). Mirrors `electrical_safety::interior_grid_coords`.
fn interior_grid_coords(seg: WireSegment) -> Vec<(i64, i64)> {
    let (a, b) = seg;
    let ka = qkey(a.0, a.1);
    let kb = qkey(b.0, b.1);
    if ka == kb {
        return Vec::new();
    }
    let dx = kb.0 - ka.0;
    let dy = kb.1 - ka.1;
    if dx != 0 && dy != 0 {
        // Router emits axis-aligned segments only.
        return Vec::new();
    }
    let mut out = Vec::new();
    if dx == 0 {
        let step = if dy > 0 { STEP_UM } else { -STEP_UM };
        let mut y = ka.1 + step;
        while (step > 0 && y < kb.1) || (step < 0 && y > kb.1) {
            out.push((ka.0, y));
            y += step;
        }
    } else {
        let step = if dx > 0 { STEP_UM } else { -STEP_UM };
        let mut x = ka.0 + step;
        while (step > 0 && x < kb.0) || (step < 0 && x > kb.0) {
            out.push((x, ka.1));
            x += step;
        }
    }
    out
}
