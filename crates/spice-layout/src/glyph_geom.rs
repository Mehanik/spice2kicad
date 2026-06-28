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

use crate::net_class::NetClass;

/// One KiCad grid cell (50 mil). Power glyphs sit one cell along the
/// host pin's outward direction, so the pin meets the glyph's anchor
/// pin with no stem wire.
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
