//! Signed, directional decoration footprint (ADR-19 Milestones 2–3).
//!
//! **Partly wired (M3).** Where it stands after Milestone 3:
//!
//! * [`crate::legalize`]'s `extent_of` — the tight legality geometry —
//!   *is* [`body_and_pins`]. One definition of "body ∪ pin reach"
//!   instead of three. Zero behaviour change; verified by an empty
//!   `baseline_lock` diff.
//! * [`property_text`] now reserves the box the emitter actually draws:
//!   [`TextKind::LeftProperty`] at
//!   [`kicad_symbols::text_geom::field_render_rotation`], the same
//!   shared model `kicad-emitter`'s `placement_property_bboxes` uses.
//!   (The M2 draft used `CenteredProperty` at rot 0 — wrong on both
//!   axes, and wrong for every mirrored or rotated symbol.)
//! * **NOT wired** into the SA overlap gate, and **not** into
//!   `legalize`'s roomy shove preference. Both were implemented and
//!   measured; both regress. See `docs/layout-adr.md` § "M3 blocked" for
//!   the three-way ablation table, and
//!   `solver::anneal::symbol_overlap_count`'s own note.
//!
//! The short version of the block: this footprint is a strict **subset**
//! of the halo on the classes both model, so honesty *frees* space — and
//! the freed space is claimed by the one decoration class still
//! unreserved, routed net labels (see "Known residual"). Completing the
//! reservation is a precondition for consuming it, not a follow-up; that
//! is ADR-17's Stage-2 lesson restated with a second measurement.
//!
//! Phase 4.5's own V13 model needed no change: it already scores real
//! body bboxes and real property bboxes. It is the *more* faithful
//! model, so M3 aligned this one to it, not the reverse.
//!
//! # Why it exists (placer-redesign.md R-B)
//!
//! The SA overlap gate reserves decoration space with a **symmetric**
//! `.abs()` halo (`solver::anneal::footprint_half_extents`): a genuinely
//! one-sided reach — a GND glyph *below* a down-facing pin — is folded via
//! `hh.max(dy.abs())` into a block on **both** sides. That halo is
//!
//! * **not honest** — it over-reserves the empty side; and
//! * **not complete** — it omits the directional property text a symbol
//!   actually draws (Reference above, Value below).
//!
//! This module models the reach as a **signed AABB** and adds the missing
//! property-text class, so the honest footprint is, provably:
//!
//! * a **subset** of the halo on the classes both model
//!   (body ∪ pins ∪ glyph) — making it directional *relaxes* the gate,
//!   which is exactly why M3 must re-calibrate the ratchets
//!   ([`tests::directional_is_a_subset_of_the_symmetric_halo`]); and
//! * **larger** than the halo where the halo is incomplete (directional
//!   property text) — which is why the reservation must be *completed*
//!   before any spacing change leans on it
//!   ([`tests::property_text_reserves_beyond_the_symmetric_halo`]).
//!
//! # Frame
//!
//! Offsets are from the element origin in the emitter **page frame**
//! (screen Y increases downward), identical to `world_extent`: a symbol-
//! local body point `(lx, ly)` contributes `(rx, -ry)` where
//! `(rx, ry) = orientation.apply_point(lx, ly)`; power-glyph reach points
//! arrive already in this frame from [`crate::glyph_geom::glyph_reach`];
//! property-text anchors match the emitter's `property_anchor`
//! (`origin + apply_point((2.54, ∓2.54))`, page frame, *no* extra y-flip —
//! the offset sign already encodes "Reference above / Value below").
//!
//! # Known residual (documented, not silently faked)
//!
//! Routing-dependent **label** text and the **`PWR_FLAG`** body are placed
//! by the emitter/router *after* placement (phase 4.5's V13 model is
//! deliberately upstream of real decoration), and the forced-sideways
//! glyph body / one-cell outward offset are emitter-side. The placer
//! cannot predict them without the router; modelling them belongs to the
//! M6 phase-4.5 promotion, not here. This mirrors ADR-14's "Known scope
//! limits" honesty — the footprint is as complete as the *placer* can be.

use std::collections::HashMap;

use kicad_symbols::text_geom::{self, TextKind};
use kicad_symbols::{Orientation, Symbol};
use spice_resolve::ResolvedElement;

use crate::net_class::VertPref;

/// Local offset (mm) at which the emitter anchors a host's Reference /
/// Value property text, before orientation. Mirrors
/// `kicad-emitter/src/schematic.rs`'s `property_anchor` calls
/// (`(2.54, -2.54)` Reference, `(2.54, 2.54)` Value).
const PROP_ANCHOR_X_MM: f64 = 2.54;
const PROP_ANCHOR_Y_MM: f64 = 2.54;

/// Signed world-frame AABB of an element's decoration footprint, as
/// offsets from the element origin (page frame; see the module docs).
/// `min_*` are `≤ 0` and `max_*` are `≥ 0` by construction — the origin is
/// always included.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignedFootprint {
    pub min_x: f64,
    pub max_x: f64,
    pub min_y: f64,
    pub max_y: f64,
}

impl SignedFootprint {
    /// The degenerate footprint at the origin.
    #[must_use]
    pub fn origin() -> Self {
        Self {
            min_x: 0.0,
            max_x: 0.0,
            min_y: 0.0,
            max_y: 0.0,
        }
    }

    fn grow(&mut self, dx: f64, dy: f64) {
        self.min_x = self.min_x.min(dx);
        self.max_x = self.max_x.max(dx);
        self.min_y = self.min_y.min(dy);
        self.max_y = self.max_y.max(dy);
    }

    /// Union of two footprints (the enclosing signed AABB).
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        Self {
            min_x: self.min_x.min(other.min_x),
            max_x: self.max_x.max(other.max_x),
            min_y: self.min_y.min(other.min_y),
            max_y: self.max_y.max(other.max_y),
        }
    }

    /// The symmetric half-extents `(hw, hh)` this signed box collapses to
    /// under the `.abs()` halo — `max(|min|, |max|)` per axis. The halo box
    /// `[-hw, hw] × [-hh, hh]` is always a **superset** of `self`, so this
    /// is exactly the information the symmetric halo keeps and the signed
    /// footprint refines.
    #[must_use]
    pub fn halo_half_extents(&self) -> (f64, f64) {
        (
            self.min_x.abs().max(self.max_x.abs()),
            self.min_y.abs().max(self.max_y.abs()),
        )
    }

    /// Is `self` contained in the symmetric halo box `[-hw,hw]×[-hh,hh]`?
    #[must_use]
    pub fn within_halo(&self, hw: f64, hh: f64) -> bool {
        const EPS: f64 = 1e-9;
        self.min_x >= -hw - EPS
            && self.max_x <= hw + EPS
            && self.min_y >= -hh - EPS
            && self.max_y <= hh + EPS
    }

    /// Axis-aligned area (mm²) — a scalar handle on how much the honest
    /// footprint differs from the halo.
    #[must_use]
    pub fn area(&self) -> f64 {
        (self.max_x - self.min_x) * (self.max_y - self.min_y)
    }
}

/// Body bbox ∪ pin-stem reach of `symbol` in `orientation`, signed. These
/// are exactly the classes the SA halo also models (body + pins), so this
/// component is provably a subset of the halo.
#[must_use]
pub fn body_and_pins(symbol: &Symbol, orientation: Orientation) -> SignedFootprint {
    let mut fp = SignedFootprint::origin();
    if let Some(b) = symbol.body_bbox() {
        for (lx, ly) in [(b.x0, b.y0), (b.x0, b.y1), (b.x1, b.y0), (b.x1, b.y1)] {
            let (rx, ry) = orientation.apply_point(lx, ly);
            fp.grow(rx, -ry);
        }
    }
    for p in symbol.pins_in(orientation) {
        fp.grow(p.x, -p.y);
    }
    fp
}

fn add_property(fp: &mut SignedFootprint, text: &str, orientation: Orientation, local_dy: f64) {
    if text.is_empty() {
        return;
    }
    let (ax, ay) = orientation.apply_point(PROP_ANCHOR_X_MM, local_dy);
    let b = text_geom::text_bbox(
        text,
        (ax, ay),
        text_geom::DEFAULT_TEXT_SIZE_MM,
        text_geom::field_render_rotation(orientation),
        TextKind::LeftProperty,
    );
    fp.grow(b.x0, b.y0);
    fp.grow(b.x1, b.y1);
}

/// The two host property texts — **Reference above, Value below** — as
/// signed boxes on their real sides. This is the directional model neither
/// the symmetric halo nor `world_extent` expresses: `world_extent`
/// reserves a property band on *both* ±Y (the conservative reading), while
/// the emitter draws Reference on one side and Value on the other. Boxes
/// come from the calibrated `text_geom` model, anchored exactly where
/// `property_anchor` places them.
///
/// **Kind and rotation match the emitter exactly** (ADR-19 M3). The
/// emitter writes host fields with `(justify left)`
/// (`schematic::property_effects`) and grades them as
/// [`TextKind::LeftProperty`] at
/// [`text_geom::field_render_rotation`] — so this reserves the same
/// thing. The M2 draft used `CenteredProperty` at rot 0, which both
/// over-reserved behind the anchor and under-reserved ahead of it, and
/// pointed every mirrored/rotated symbol's text the wrong way; a
/// reservation that is not the box the emitter draws is not a
/// reservation. `spice2kicad`'s V13 suite is what holds the two
/// together — it grades the emitted ink, not this model.
#[must_use]
pub fn property_text(
    refdes: &str,
    value: Option<&str>,
    orientation: Orientation,
) -> SignedFootprint {
    let mut fp = SignedFootprint::origin();
    add_property(&mut fp, refdes, orientation, -PROP_ANCHOR_Y_MM);
    if let Some(v) = value {
        add_property(&mut fp, v, orientation, PROP_ANCHOR_Y_MM);
    }
    fp
}

/// The string the emitter actually draws in a host's **Value** field:
/// the rendered value if there is one, else the refdes
/// (`schematic.rs`'s `el.value.as_deref().unwrap_or(&el.refdes)`, at
/// both the root and sheet emit sites). A KiCad symbol instance always
/// carries a Value property, so "no value" means "the field reads the
/// refdes", not "the field is absent" — reserving nothing for it would
/// under-reserve every valueless part (`Q1`, an opamp instance).
///
/// [`property_text`] keeps its `Option` so the *absence* case stays
/// expressible and testable; this is the rule callers modelling the
/// emitter should apply before calling it.
#[must_use]
pub fn drawn_value<'a>(refdes: &'a str, value: Option<&'a str>) -> &'a str {
    value.unwrap_or(refdes)
}

/// Power-glyph reach (body + net-name value text) of every rail pin,
/// signed and **one-sided** — a GND glyph below a down-facing pin grows
/// `max_y` (screen-down) only, never `min_y`. Reuses
/// [`crate::glyph_geom::glyph_reach`], whose points are already in this
/// page frame; the halo's `.abs()` is what makes the same reach two-sided.
#[must_use]
#[allow(clippy::implicit_hasher)] // callers always pass the default-hasher prefs map.
pub fn glyph(
    elem: &ResolvedElement,
    orientation: Orientation,
    prefs: &HashMap<String, VertPref>,
) -> SignedFootprint {
    let mut fp = SignedFootprint::origin();
    for (dx, dy) in crate::glyph_geom::glyph_reach(elem, orientation, prefs) {
        fp.grow(dx, dy);
    }
    fp
}

/// The honest, as-complete-as-the-placer-can-know footprint:
/// body ∪ pins ∪ one-sided glyph ∪ directional property text.
///
/// **No production consumer.** M3 wired this into `legalize`'s roomy
/// shove preference and into the SA overlap gate; both regressed and
/// both are reverted (module docs, and `docs/layout-adr.md` §
/// "M3 blocked"). It stays computable — and unit-tested against the
/// halo — because the ablation table is the evidence a future stage
/// starts from, and because M6 consumes it once `label_geom` closes the
/// residual below.
///
/// `value` is the string the emitter draws in the **Value** field — use
/// [`drawn_value`] to derive it — or `None` when the element draws **no
/// property text at all**, which is exactly the suppressed `;@ power`
/// source (`schematic::placement_property_bboxes` skips those too).
/// `None` therefore suppresses the Reference box as well: a part that
/// renders nothing reserves nothing.
///
/// The glyph term here is unconditional. The SA gate scopes it to
/// *non-oversized* hosts (ADR-14); that is a deliberate gate property,
/// not a footprint property, so it does not belong in here.
#[must_use]
#[allow(clippy::implicit_hasher)] // callers always pass the default-hasher prefs map.
pub fn element_footprint(
    elem: &ResolvedElement,
    orientation: Orientation,
    value: Option<&str>,
    prefs: &HashMap<String, VertPref>,
) -> SignedFootprint {
    let mut fp = body_and_pins(&elem.symbol, orientation).union(&glyph(elem, orientation, prefs));
    if let Some(v) = value {
        fp = fp.union(&property_text(&elem.refdes, Some(v), orientation));
    }
    fp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net_class::vertical_prefs;
    use crate::solver::anneal::footprint_half_extents;
    use spice_diagnostics::FileId;
    use spice_policy::{CheckedNetlist, check};

    use kicad_symbols::Library;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    fn fixture_library() -> &'static Library {
        static LIB: OnceLock<Library> = OnceLock::new();
        LIB.get_or_init(|| {
            let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(std::path::Path::parent)
                .expect("workspace root")
                .join("crates/kicad-symbols/tests/fixtures");
            let device = Library::from_file(fixture_dir.join("Device.kicad_sym"))
                .expect("load Device fixture library");
            let spice = Library::from_file(fixture_dir.join("Simulation_SPICE.kicad_sym"))
                .expect("load Simulation_SPICE fixture library");
            let power = Library::from_file(fixture_dir.join("power.kicad_sym"))
                .expect("load power fixture library");
            device.merge(spice).merge(power)
        })
    }

    fn checked(src: &str) -> CheckedNetlist {
        let parsed = spice_parser::parse(src, FileId(0))
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        check(resolved).expect("policy check failed").0
    }

    fn element<'a>(net: &'a CheckedNetlist, refdes: &str) -> &'a ResolvedElement {
        net.elements
            .iter()
            .find(|e| e.refdes == refdes)
            .unwrap_or_else(|| panic!("no element {refdes}"))
    }

    /// R-B, half one: on the classes the SA halo *also* models
    /// (body ∪ pins ∪ glyph), the signed footprint is contained in the
    /// symmetric halo the gate uses today. Making the reach directional
    /// can only *relax* the gate — which is why M3 must recalibrate.
    #[test]
    fn directional_is_a_subset_of_the_symmetric_halo() {
        // A grounded RC: `C1 out 0` gives a ground pin, so the glyph class
        // is exercised, not just body/pins.
        let net = checked("test\nV1 in 0 AC 1 ;@ ignore\nR1 in out 1k\nC1 out 0 100n\n.end\n");
        let prefs = vertical_prefs(&net);
        let orient = Orientation::IDENTITY;
        for el in &net.elements {
            let shared = body_and_pins(&el.symbol, orient).union(&glyph(el, orient, &prefs));
            let (hw, hh) = footprint_half_extents(el, orient, Some(&prefs));
            assert!(
                shared.within_halo(hw, hh),
                "{}: signed body∪pins∪glyph {shared:?} escapes the symmetric halo \
                 (hw={hw}, hh={hh}) — it must be a subset",
                el.refdes,
            );
        }
    }

    /// R-B, half two: the halo is *incomplete*. The complete footprint
    /// reserves directional property text the halo never models, so it
    /// escapes the halo on at least one element — proving the reservation
    /// must be *completed*, not merely made directional.
    #[test]
    fn property_text_reserves_beyond_the_symmetric_halo() {
        let net = checked("test\nV1 in 0 AC 1 ;@ ignore\nR1 in out 1k\nC1 out 0 100n\n.end\n");
        let prefs = vertical_prefs(&net);
        let orient = Orientation::IDENTITY;
        let r1 = element(&net, "R1");
        let full = element_footprint(r1, orient, Some("1k"), &prefs);
        let (hw, hh) = footprint_half_extents(r1, orient, Some(&prefs));
        assert!(
            !full.within_halo(hw, hh),
            "R1's complete footprint {full:?} should escape the property-text-blind \
             halo (hw={hw}, hh={hh}); if it does not, the halo already covers the \
             text and M3's completion is a no-op here",
        );
    }

    /// The signed glyph reach is genuinely **one-sided**: for a ground pin
    /// exactly one of `min_y` / `max_y` is zero, where the halo would
    /// reserve both. This is the directionality the redesign buys.
    #[test]
    fn ground_glyph_reach_is_one_sided() {
        let net = checked("test\nV1 in 0 AC 1 ;@ ignore\nR1 in out 1k\nC1 out 0 100n\n.end\n");
        let prefs = vertical_prefs(&net);
        let g = glyph(element(&net, "C1"), Orientation::IDENTITY, &prefs);
        let min_zero = g.min_y.abs() < 1e-9;
        let max_zero = g.max_y.abs() < 1e-9;
        assert!(
            min_zero != max_zero,
            "the ground glyph reach must extend on exactly one Y side, got {g:?}",
        );
        // ...and the halo of that same reach is two-sided (hh > 0 both ways
        // by definition), i.e. the halo blocks the empty side the signed
        // box leaves free.
        let (_, hh) = g.halo_half_extents();
        assert!(
            hh > 1.0,
            "glyph reach should be a real distance, got hh={hh}"
        );
    }

    /// An element with no drawn value reserves no Value box — the property
    /// class must not become an unconditional halo on every symbol.
    #[test]
    fn absent_value_reserves_no_value_box() {
        let orient = Orientation::IDENTITY;
        let with = property_text("R1", Some("1k"), orient);
        let without = property_text("R1", None, orient);
        assert!(
            without.max_x < with.max_x || without.min_y > with.min_y || without.max_y < with.max_y,
            "dropping the value must shrink the reserved box: with={with:?} without={without:?}",
        );
    }

    /// [`drawn_value`] pins one non-obvious emitter fact: a KiCad symbol
    /// instance ALWAYS carries a Value property, so a part with no SPICE
    /// value renders its **refdes** there
    /// (`schematic.rs`: `el.value.as_deref().unwrap_or(&el.refdes)`).
    /// Reserving nothing for it would under-reserve every valueless part.
    #[test]
    fn a_valueless_part_still_draws_its_refdes_as_the_value() {
        assert_eq!(drawn_value("R1", Some("1k")), "1k");
        assert_eq!(drawn_value("Q1", None), "Q1");
    }

    /// The property box must be the box the emitter DRAWS, not a
    /// centred guess. The emitter left-justifies host fields, so the
    /// text sits entirely on the +reading-direction side of its anchor;
    /// at rot 0 that means the box starts at the anchor's X and never
    /// reaches behind it. The M2 draft used `CenteredProperty`, which
    /// straddles — over-reserving behind and under-reserving ahead.
    #[test]
    fn property_text_is_left_anchored_like_the_emitter_draws_it() {
        let fp = property_text("R1", Some("1k"), Orientation::IDENTITY);
        // Anchors are local (2.54, ∓2.54); left-justified text extends
        // only to +X from there, so nothing may reach left of the anchor.
        assert!(
            fp.min_x > -1e-9,
            "left-justified property text must not reach behind its +X anchor: {fp:?}",
        );
        assert!(
            fp.max_x > PROP_ANCHOR_X_MM,
            "the string must extend past its own anchor: {fp:?}",
        );
    }

    /// ...and the reading direction follows the placed orientation, not
    /// a hardcoded 0. A 90°-rotated symbol draws its fields vertically,
    /// so the string's own length lands on the OTHER axis. Modelling
    /// every field as rot 0 (the M2 draft) is wrong for half of all
    /// placed symbols — a Y-mirrored resistor's Value extends left of
    /// its anchor, a 270° one downward.
    ///
    /// Uses a deliberately long string so the reading direction dominates
    /// the anchor offsets, and compares the two orientations against each
    /// other rather than asserting an absolute aspect (the union of the
    /// Reference and Value boxes straddles ±Y at every rotation).
    #[test]
    fn property_text_follows_the_placed_orientation() {
        let long = "4700000000";
        let flat = property_text("R1", Some(long), Orientation::IDENTITY);
        let turned = property_text(
            "R1",
            Some(long),
            Orientation {
                rotation: kicad_symbols::Rotation::R90,
                mirror_y: false,
            },
        );
        let width = |f: &SignedFootprint| f.max_x - f.min_x;
        let height = |f: &SignedFootprint| f.max_y - f.min_y;
        assert!(
            width(&flat) > width(&turned) && height(&turned) > height(&flat),
            "rotating the symbol must move the string's length onto the other \
             axis; got flat={flat:?} turned={turned:?}",
        );
    }
}
