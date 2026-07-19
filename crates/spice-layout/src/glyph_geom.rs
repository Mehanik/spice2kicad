//! Single source of truth for **power-glyph reach geometry** — the
//! fixed, rule-derived footprint a `power:*` rail glyph (and its
//! net-name value text) occupies outward of a host rail pin.
//!
//! This geometry is consumed by two crates that must not drift:
//!
//! * `spice-route::rails` — *decoration*: it realises the glyph at
//!   emit time (anchor pose, value-text anchor, sheet-edge offset).
//! * `spice-layout` (this crate) — *placement*: it reserves the same
//!   footprint so the placer keeps foreign bodies out of the zone the
//!   glyph will later occupy (ADR-14, Option A).
//!
//! The constants live here (placement-side) and `spice-route` reads
//! them, so the dependency points the safe way (`spice-route` already
//! depends on `spice-layout`; the reverse would close a cycle). Keeping
//! the numbers in one place is what stops the placer's reserved zone and
//! the emitter's drawn glyph from disagreeing.
//!
//! All values are in millimetres on the 1.27 mm KiCad grid.

use std::collections::HashMap;

use kicad_symbols::Orientation;
use spice_resolve::{ElementRole, ResolvedElement};

use crate::net_class::{NetClass, VertPref};

/// One KiCad grid cell (50 mil). In the canonical case a power glyph's
/// anchor pin sits exactly ON the host pin (`spice-route::rails`'
/// `symbol_pose` with `glyph_offset` = `None`) — no stem wire at all.
/// The forced-sideways and sheet-edge cases instead offset the anchor a
/// whole number of these cells along the pin's outward direction and
/// DO emit a bridging stub wire (`rails::stub_wire`).
pub const GRID_MM: f64 = 1.27;

/// Outward extent (mm) of a power-glyph **body** from its anchor pin: a
/// GND triangle / VCC chevron body reaches ≈2.54 mm (two grid cells)
/// past the anchor along the glyph's canonical axis. This is the body
/// half the placer must reserve outward of a rail pin and the same
/// extent `spice-route::rails`' value-text offset clears.
pub const GLYPH_BODY_REACH_MM: f64 = 2.0 * GRID_MM;

/// Outward offset (mm) of a power glyph's net-name **value text** from
/// the anchor pin — one grid cell beyond the glyph body tip
/// (`GLYPH_BODY_REACH_MM` + one cell). Reserving this jointly with the
/// body (ADR-14's V13-joint clause) keeps the value label out of a
/// foreign body during placement, so the decoration nudge pass never has
/// to buy glyph clearance at a label-on-body cost.
pub const VALUE_TEXT_OFFSET_MM: f64 = GLYPH_BODY_REACH_MM + GRID_MM;

/// Grid-cell offset applied to a power glyph anchored on a
/// hierarchical-sheet port pin. The glyph (and its net-name label) are
/// pushed this many cells *outward* (away from the sheet body) so the
/// glyph body and label clear both the sheet body and the sheet's port
/// label, which KiCad draws at the port-pin coordinate. Two cells: the
/// glyph body extends ±1 cell about its anchor, so a 2-cell offset puts
/// the inner glyph edge one full cell clear of the sheet edge.
pub const SHEET_EDGE_GLYPH_OFFSET_CELLS: f64 = 2.0;

/// Screen-vertical axis a rail glyph's body extends along, away from its
/// host pin. Positive supply rails point the chevron **up**; ground and
/// negative supply rails point the triangle **down**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphAxis {
    Up,
    Down,
}

/// The canonical screen-vertical axis a rail glyph occupies, as a pure
/// function of the host net's class and whether it is a negative rail.
///
/// * Negative rail (`power:VEE`) → **down**, like ground: it sits in the
///   bottom band with down-facing host pins, so the glyph attaches with
///   no offset exactly as a GND glyph does.
/// * [`NetClass::Power`] → **up** (VCC chevron above the anchor).
/// * Ground (the only other class that reaches a glyph; signal nets emit
///   no glyph) → **down**.
#[must_use]
pub fn canonical_axis(class: NetClass, negative_rail: bool) -> GlyphAxis {
    if negative_rail {
        return GlyphAxis::Down;
    }
    match class {
        NetClass::Power => GlyphAxis::Up,
        _ => GlyphAxis::Down,
    }
}

/// World-extent-frame outward reach points of every power-glyph zone an
/// element's **rail pins** carry, for `world_extent` / the SA gate to
/// reserve (ADR-14 Option A, Phase 2/3).
///
/// Each returned `(dx, dy)` is the far tip of a reserved glyph zone,
/// expressed as a signed offset from the element origin in the *same
/// frame `world_extent` grows in* — `dx` positive = right, `dy` positive
/// = screen-down (the eeschema y-flip is already applied, matching
/// `world_extent`'s `grow(rx, -ry)`). Unioning these into an element's
/// `WorldExtent` reserves, outward of every rail pin, the cell(s) the
/// `power:*` glyph **body and its net-name value text** will occupy in
/// decoration. The placer then keeps foreign bodies that whole zone
/// clear — the foreign element is repelled, the glyph never moves.
///
/// The reach direction maps the pin's *transformed* angle exactly as
/// the decoration side does (`angle`: 270 → up, 90 → down, 180 → left,
/// 0 → right — the same table as `kicad-emitter`'s `angle_to_direction`
/// and `rails::outward_delta`), so the reservation lands where the
/// glyph's value text will actually be drawn — the drift-free property
/// ADR-14 needs. `elem.symbol.pins_in` reports the world-outward angle
/// for every orientation (the `pins_in` fix that corrected the raw
/// `.kicad_sym` angle, which pointed inward), so this is the true
/// outward direction for both a *vertically*-facing transformed pin
/// (the canonical GND-down / VCC-up case) and a *horizontally*-facing
/// one — a rail consumer rotated sideways now reserves real space
/// instead of degenerating into the body bbox. Its length is
/// [`VALUE_TEXT_OFFSET_MM`] — the joint body + value-text reach — which
/// covers the decoration footprint in the *canonical* case: a pin
/// facing its glyph's canonical direction, with the glyph body and
/// value-text anchor stacked along the pin's outward axis. NOT
/// modelled: the value text's *width* (its extent perpendicular to the
/// axis), a forced-sideways glyph's body, the forced-sideways /
/// sheet-edge one-cell outward offset, and the co-located `PWR_FLAG`
/// body (which points anti-outward) — see ADR-14's "Known scope
/// limits" amendment.
///
/// A rail pin is a terminal whose net carries a [`VertPref`] (i.e. a
/// Power/Ground/negative-rail net). Power *sources* (`ElementRole::Power`)
/// are excluded: their body is itself replaced by a rail glyph in
/// decoration, so there is no separate host body to reserve around — the
/// V14 detached-glyph fallback already governs them.
///
/// This adds only outward **spacing**; it never restricts the
/// orientation set (V5 untouched).
#[must_use]
#[allow(clippy::implicit_hasher)] // callers always pass the default-hasher prefs map.
pub fn glyph_reach(
    elem: &ResolvedElement,
    orientation: Orientation,
    prefs: &HashMap<String, VertPref>,
) -> Vec<(f64, f64)> {
    // A power source's drawn body is replaced by its own glyph; nothing
    // to reserve a foreign-clearance zone around.
    if matches!(elem.role, ElementRole::Power(_)) {
        return Vec::new();
    }

    let pins = elem.symbol.pins_in(orientation);
    let mut out = Vec::new();
    for (term_idx, node) in elem.nodes.iter().enumerate() {
        if !prefs.contains_key(node) {
            continue; // signal pin: no glyph, no reservation
        }
        let Some(kicad_pin) = elem.pin_mapping.get(term_idx) else {
            continue;
        };
        let Some(p) = pins.iter().find(|p| &p.number == kicad_pin) else {
            continue;
        };
        // Pin tip in the extent frame (eeschema y-flip, matching
        // `world_extent`'s `grow(p.x, -p.y)`).
        let (tip_x, tip_y) = (p.x, -p.y);
        // Outward unit along the pin's transformed direction, in the
        // same (y-down screen) frame.
        let (ux, uy) = match p.angle % 360 {
            270 => (0.0, -1.0), // screen up
            90 => (0.0, 1.0),   // screen down
            180 => (-1.0, 0.0), // screen left
            0 => (1.0, 0.0),    // screen right
            _ => continue,      // non-cardinal: no glyph reservation
        };
        out.push((
            tip_x + ux * VALUE_TEXT_OFFSET_MM,
            tip_y + uy * VALUE_TEXT_OFFSET_MM,
        ));
    }
    out
}
