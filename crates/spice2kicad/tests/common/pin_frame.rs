//! The **pin frame**: where each drawn element presents each of its nets
//! on the emitted sheet.
//!
//! # Why this module exists
//!
//! CLAUDE.md § "Layout invariants" states the project's placement
//! contract in one sentence:
//!
//! > **Constraints are pin-anchored.** `place` and `align` describe
//! > relationships between *connecting pins*, not symbol centers.
//!
//! Almost every verifier in this suite already measures that way — it
//! reads world pin positions (`electrical_safety::world_pins_for_sheet`,
//! `placement_quality::world_pins_by_net`, `flow_geometry`'s `BodyPin`)
//! or the emitted ink. Two did not: **Q5**
//! (`alignment_quality.rs`) and **Q3** (`flow_monotonicity.rs`) compared
//! symbol `(at x y)` ORIGINS, and ADR-40 measured what that costs — a
//! DC-series column that puts its members' *shared pins* on one x (i.e.
//! does exactly what the invariant demands) offsets their *origins* by
//! the symbol's own pin offset, and both metrics scored the correct
//! drawing as a defect.
//!
//! This module is the one join both metrics' pin-frame twins read, so
//! there is a single definition of "where is `R1`'s pin on net `out`".
//! It is deliberately shared rather than copied into each binary: the
//! suite already carries four independently-drifted copies of this join,
//! and MEMORY "verify what a number measures" is the record of what that
//! costs.
//!
//! # What it joins
//!
//! * the **emitted** `.kicad_sch` — each top-level `(symbol …)`'s pose
//!   `(at x y rot)` plus `(mirror y)`; and
//! * the **resolved** netlist — each element's `pin_mapping` (SPICE
//!   terminal → KiCad pin number), its `nodes`, and its own owned clone
//!   of the library `Symbol`, which carries the pin geometry.
//!
//! Pin geometry comes from `ResolvedElement::symbol`, not from a
//! re-`lookup` of the emitted `lib_id`: that is the same symbol the
//! placer and emitter posed, so the join cannot disagree with them about
//! which pin is which.
//!
//! # What takes part
//!
//! Only elements actually DRAWN as a non-glyph body. `power:*` symbols
//! and `#`-prefixed refdes (rail glyphs, `PWR_FLAG` markers) are
//! decoration hung off a rail pin; `;@ ignore`d elements and `.subckt`
//! instances lowered to a `(sheet …)` never appear as a top-level
//! `(symbol …)` and so drop out naturally.
//!
//! # Coverage is asserted, not assumed
//!
//! A metric that silently skips the geometry it cannot resolve reports a
//! smaller number and looks like an improvement — ADR-23 D9's "a blind
//! cell is not conservatively blind", one level down. [`PinFrame`]
//! therefore records every `(refdes, net)` whose pin it could not place
//! in [`PinFrame::unresolved`], and both callers assert it is empty.

use std::collections::HashMap;

use kicad_symbols::{Orientation, Rotation};
use lexpr::Value;
use spice_resolve::{ElementRole, ResolvedElement};

/// The KiCad schematic grid pitch, in micrometres (1.27 mm = 50 mil).
pub const CELL_UM: i64 = 1270;

/// Quantise a millimetre coordinate to micrometres.
///
/// Every emitted origin is grid-snapped and every pin offset is a
/// multiple of 0.01 mm, so µm integers make `dx == 0` an exact test
/// rather than a float comparison with a tolerance nobody calibrated.
#[allow(clippy::cast_possible_truncation)]
fn q(mm: f64) -> i64 {
    (mm * 1000.0).round() as i64
}

// --- lexpr helpers -------------------------------------------------------

fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    v.list_iter().map_or_else(
        || Box::new(std::iter::empty()) as Box<dyn Iterator<Item = &Value>>,
        |it| Box::new(it),
    )
}

fn head(v: &Value) -> Option<&str> {
    list_iter(v).next().and_then(as_str)
}

fn as_str(v: &Value) -> Option<&str> {
    v.as_symbol()
        .or_else(|| v.as_str())
        .or_else(|| v.as_keyword())
}

fn as_f64(v: &Value) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))
}

fn find_child<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    list_iter(v).find(|c| c.is_list() && head(c) == Some(name))
}

fn children<'a>(v: &'a Value, name: &str) -> Vec<&'a Value> {
    list_iter(v)
        .filter(|c| c.is_list() && head(c) == Some(name))
        .collect()
}

/// `(at x y rot)` + `(mirror y)` of one emitted `(symbol …)`.
fn placed_pose(sym: &Value) -> Option<(f64, f64, Orientation)> {
    let at = find_child(sym, "at")?;
    let mut it = list_iter(at);
    it.next();
    let x = it.next().and_then(as_f64)?;
    let y = it.next().and_then(as_f64)?;
    let rot_deg = it.next().and_then(as_f64).unwrap_or(0.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rot_u = ((rot_deg.round() as i64).rem_euclid(360)) as u16;
    let rotation = match rot_u {
        0 => Rotation::R0,
        90 => Rotation::R90,
        180 => Rotation::R180,
        270 => Rotation::R270,
        _ => return None,
    };
    let mirror_y = find_child(sym, "mirror")
        .and_then(|m| list_iter(m).nth(1).and_then(as_str))
        .is_some_and(|s| s == "y");
    Some((x, y, Orientation { rotation, mirror_y }))
}

fn refdes_of(sym: &Value) -> Option<String> {
    for prop in children(sym, "property") {
        let mut it = list_iter(prop);
        it.next();
        if it.next().and_then(as_str) == Some("Reference") {
            return it.next().and_then(as_str).map(str::to_owned);
        }
    }
    None
}

// --- the join ------------------------------------------------------------

/// Where every drawn body presents every net it carries, in world µm.
pub struct PinFrame {
    /// `refdes → net → world pin positions (µm)`. An element with two
    /// pins on one net (a shorted terminal pair) contributes both.
    by_elem: HashMap<String, HashMap<String, Vec<(i64, i64)>>>,
    /// `(refdes, net)` pairs the join could not place: the element is
    /// drawn, but its `pin_mapping` names a KiCad pin the posed symbol
    /// does not have. Callers MUST assert this is empty — see the module
    /// header.
    pub unresolved: Vec<(String, String)>,
}

impl PinFrame {
    /// Join the emitted poses to the resolved pin mapping.
    ///
    /// `elements` is the resolved/checked element list; `root` the parsed
    /// root sheet.
    pub fn build(root: &Value, elements: &[ResolvedElement]) -> Self {
        // Emitted pose of every DRAWN, non-glyph body.
        let mut poses: HashMap<String, (f64, f64, Orientation)> = HashMap::new();
        for sym in children(root, "symbol") {
            let lib_id = find_child(sym, "lib_id")
                .and_then(|l| list_iter(l).nth(1).and_then(as_str))
                .unwrap_or_default();
            if lib_id.starts_with("power:") {
                continue;
            }
            let Some(refdes) = refdes_of(sym) else {
                continue;
            };
            if refdes.starts_with('#') {
                continue;
            }
            let Some(pose) = placed_pose(sym) else {
                continue;
            };
            poses.insert(refdes, pose);
        }

        let mut by_elem: HashMap<String, HashMap<String, Vec<(i64, i64)>>> = HashMap::new();
        let mut unresolved = Vec::new();
        for el in elements {
            // Power sources are lowered to rail glyphs, not flow bodies.
            if matches!(el.role, ElementRole::Power(_)) {
                continue;
            }
            let Some(&(ox, oy, orient)) = poses.get(&el.refdes) else {
                continue;
            };
            let pins = el.symbol.pins_in(orient);
            let slot = by_elem.entry(el.refdes.clone()).or_default();
            for (i, net) in el.nodes.iter().enumerate() {
                let Some(number) = el.pin_mapping.get(i) else {
                    unresolved.push((el.refdes.clone(), net.clone()));
                    continue;
                };
                let Some(tp) = pins.iter().find(|p| &p.number == number) else {
                    unresolved.push((el.refdes.clone(), net.clone()));
                    continue;
                };
                // `TransformedPin` is Y-up in the symbol frame; the world
                // is Y-down, hence `oy - tp.y` (the convention every other
                // verifier in this suite uses).
                slot.entry(net.clone())
                    .or_default()
                    .push((q(ox + tp.x), q(oy - tp.y)));
            }
        }
        Self {
            by_elem,
            unresolved,
        }
    }

    /// Where `refdes` presents `net`, or `None` when it is not drawn.
    pub fn pins(&self, refdes: &str, net: &str) -> Option<&[(i64, i64)]> {
        self.by_elem
            .get(refdes)
            .and_then(|m| m.get(net))
            .map(Vec::as_slice)
    }

    /// Total pins placed — the non-vacuity control for a fixture.
    pub fn pin_count(&self) -> usize {
        self.by_elem
            .values()
            .flat_map(HashMap::values)
            .map(Vec::len)
            .sum()
    }
}
