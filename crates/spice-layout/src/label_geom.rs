//! Single source of truth for **net-label geometry** — the footprint a
//! `(label …)` / `(global_label …)` occupies outward of the body pin it
//! anchors on.
//!
//! Sibling of [`crate::glyph_geom`], and it exists for the same reason.
//! Labels are *decoration* (layout phase 5): `kicad-emitter`'s
//! `label_specs` chooses each one's anchor and rotation after routing,
//! and the placer — which runs earlier and cannot be moved afterwards —
//! had **no model of them at all**. Signal pins therefore reserved zero
//! decoration space, which is the largest remaining hole in ADR-14's
//! reservation and the identified cause of four of ADR-17 Stage 2's
//! seven Tier-1 breaches.
//!
//! # Is a placement-time label footprint computable?
//!
//! Yes, and the reason is that **every input to the label's identity is
//! netlist data, not routed geometry**:
//!
//! * *Which nets get a label* — V4 policy: signal nets only (power and
//!   ground carry `power:*` glyphs instead). [`crate::net_class`] already
//!   answers this placement-side.
//! * *What kind* — a declared `*@port` net gets one directional
//!   `(global_label …)`; a net with a single body terminal gets an
//!   interface `(global_label … (shape input))`; anything else gets a
//!   plain `(label …)`. All three tests read the netlist.
//! * *What text* — the SPICE net name.
//! * *How wide* — [`kicad_symbols::text_metrics::text_width`], real
//!   Newstroke advances, and [`kicad_symbols::text_geom`], the ONE
//!   text-box model, calibrated against real `kicad-cli sch export svg`
//!   ink by `spice2kicad/tests/rendered_text.rs`.
//!
//! What is *not* known pre-routing is which pin of a multi-terminal net
//! ends up carrying the label (the emitter takes the geometrically
//! leftmost, which depends on the very positions the placer is deciding)
//! and whether the rotation-avoidance search rotates the label off its
//! preferred outward direction. [`label_reach`] handles both
//! conservatively: it reserves the outward box at **every** candidate
//! anchor pin, and it does not model the non-preferred rotations.
//!
//! # Frames
//!
//! Reach points are returned in the same frame [`crate::world_extent`]
//! grows in — signed offsets from the element origin, `dx` positive =
//! right, `dy` positive = screen-down (the eeschema y-flip already
//! applied). Identical to [`crate::glyph_geom::glyph_reach`].

use std::collections::HashMap;

use kicad_symbols::Orientation;
use kicad_symbols::text_geom::{DEFAULT_TEXT_SIZE_MM, TextKind, text_bbox};
use spice_policy::CheckedNetlist;
use spice_resolve::{ElementRole, PortDir, ResolvedElement};

use crate::net_class::{NetClass, classify_nets};

/// What KiCad node the emitter will plant for a net, and hence which
/// text box it occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelKind {
    /// A plain `(label …)`: left-anchored, no tag lead.
    Plain,
    /// A `(global_label …)` with the given `(shape …)` token. The shape
    /// decides the tag lead (arrow-headed shapes are wider).
    Global { shape: &'static str },
}

impl LabelKind {
    fn text_kind(self) -> TextKind {
        match self {
            LabelKind::Plain => TextKind::PlainLabel,
            LabelKind::Global { shape } => TextKind::global_label(Some(shape)),
        }
    }
}

/// The label a net will carry, as decided from netlist data alone.
#[derive(Debug, Clone)]
pub struct LabelPlan {
    /// Rendered string — the SPICE net name, verbatim.
    pub text: String,
    pub kind: LabelKind,
    /// Whether the anchor pin is *determined* pre-routing. True only for
    /// single-terminal nets, where there is exactly one candidate.
    /// False means [`label_reach`] reserves the box at every candidate
    /// pin, which over-reserves by (terminals − 1) boxes per net.
    pub anchor_determined: bool,
}

/// Per-net label plans, mirroring `kicad-emitter`'s `label_specs` V4
/// policy from netlist data alone.
///
/// Deliberately **not** modelled here, because each needs routed
/// geometry or an emitter-side decision the placer cannot anticipate:
/// the second name-jump label on a port-touching net, the
/// rotation-avoidance fallback, the anchor-search fallback, and the
/// suppression of a label whose anchor coincides with a port marker.
/// Every one of those *reduces or relocates* the reserved footprint
/// rather than growing it past this model, except the name-jump second
/// label — which lands on a pin this model already reserves (the net's
/// rightmost), because multi-terminal nets are reserved at every pin.
#[must_use]
pub fn label_plans(checked: &CheckedNetlist) -> HashMap<String, LabelPlan> {
    let classes = classify_nets(checked);
    let ports: HashMap<&str, PortDir> = checked
        .ports
        .iter()
        .map(|p| (p.net.as_str(), p.dir))
        .collect();

    // Body terminals per net. A power *source* contributes no pins to
    // the emitted schematic (`collect_net_pins` skips it — its body is
    // replaced by rail glyphs), so it must not be counted here either,
    // or a one-terminal signal net hanging off one would be mis-typed as
    // multi-terminal and lose its interface global label.
    let mut terminals: HashMap<&str, usize> = HashMap::new();
    for el in &checked.elements {
        if matches!(el.role, ElementRole::Power(_)) {
            continue;
        }
        for node in &el.nodes {
            *terminals.entry(node.as_str()).or_default() += 1;
        }
    }

    let mut out = HashMap::new();
    for (net, class) in &classes {
        if *class != NetClass::Signal {
            continue; // power / ground carry `power:*` glyphs, not labels
        }
        let n = terminals.get(net.as_str()).copied().unwrap_or(0);
        if n == 0 {
            continue;
        }
        let kind = if let Some(dir) = ports.get(net.as_str()) {
            LabelKind::Global {
                shape: port_shape_token(*dir),
            }
        } else if n == 1 {
            LabelKind::Global { shape: "input" }
        } else {
            LabelKind::Plain
        };
        out.insert(
            net.clone(),
            LabelPlan {
                text: net.clone(),
                kind,
                anchor_determined: n == 1,
            },
        );
    }
    out
}

/// TEMPORARY ablation switch. `S2K_LABEL_RESERVE` is a comma-separated
/// subset of `seed` / `legalize` / `sa`; `S2K_LABEL_SCOPE=global`
/// narrows the reservation to nets whose anchor pin is *determined*
/// pre-routing (single-terminal / global-label nets). Unset = the
/// measured default. Delete once the scope is settled.
#[must_use]
pub fn plans_for(checked: &CheckedNetlist, stage: &str) -> HashMap<String, LabelPlan> {
    let stages = std::env::var("S2K_LABEL_RESERVE").unwrap_or_else(|_| "seed".into());
    if !stages.split(',').any(|s| s.trim() == stage) {
        return HashMap::new();
    }
    let mut plans = label_plans(checked);
    if std::env::var("S2K_LABEL_SCOPE").as_deref() == Ok("global") {
        plans.retain(|_, p| p.anchor_determined);
    }
    plans
}

/// The `(shape …)` token a declared `*@port` direction renders as. Must
/// agree with `kicad-emitter`'s `port_shape_token`; the arrow-headed
/// shapes carry a wider tag lead, so a disagreement would reserve a
/// differently-sized box than the emitter draws.
fn port_shape_token(dir: PortDir) -> &'static str {
    match dir {
        PortDir::Input => "input",
        PortDir::Output => "output",
        PortDir::Bidir => "bidirectional",
    }
}

/// The label `(at … rot)` token whose text reads **outward** from a pin
/// whose world-outward angle is `pin_angle`.
///
/// Must agree with `kicad-emitter`'s `outward_label_rot`: the placer
/// reserving a box on one side of a pin while the emitter draws it on
/// the other is exactly the drift ADR-14's single-source rule exists to
/// prevent.
#[must_use]
pub fn outward_label_rot(pin_angle: u16) -> u16 {
    (360 - pin_angle % 360) % 360
}

/// World-extent-frame reach points of every net label an element's
/// **signal pins** carry, for `world_extent` / the SA gate to reserve.
///
/// Returns the two opposite corners of each reserved label box, as
/// signed offsets from the element origin in the extent frame (`dy`
/// positive = screen-down). Union them into the element's `WorldExtent`
/// and the placer keeps foreign bodies out of the space the label will
/// later occupy — the foreign element is repelled, the label never
/// moves, and decoration stays a strict consumer of placement.
///
/// Scope, stated precisely because the reservation is deliberately
/// partial (ADR-14 "Known scope limits" applies here too):
///
/// * **Over-reserves** multi-terminal nets: the emitter labels exactly
///   one pin (the leftmost) but which pin that is depends on the
///   positions being decided, so every candidate is reserved.
/// * **Under-reserves** the rotation-avoidance fallback: only the
///   preferred outward rotation is modelled, so a label the emitter
///   rotates 90° off it lands partly outside the reserved box.
/// * Power sources reserve nothing (their bodies are replaced by
///   glyphs), matching [`crate::glyph_geom::glyph_reach`].
#[must_use]
#[allow(clippy::implicit_hasher)] // callers always pass the default-hasher plans map.
pub fn label_reach(
    elem: &ResolvedElement,
    orientation: Orientation,
    plans: &HashMap<String, LabelPlan>,
) -> Vec<(f64, f64)> {
    if matches!(elem.role, ElementRole::Power(_)) {
        return Vec::new();
    }
    let pins = elem.symbol.pins_in(orientation);
    let mut out = Vec::new();
    for (term_idx, node) in elem.nodes.iter().enumerate() {
        let Some(plan) = plans.get(node) else {
            continue; // rail net, or a net that carries no label
        };
        let Some(kicad_pin) = elem.pin_mapping.get(term_idx) else {
            continue;
        };
        let Some(p) = pins.iter().find(|p| &p.number == kicad_pin) else {
            continue;
        };
        // Pin tip in the extent frame (eeschema y-flip, matching
        // `world_extent`'s `grow(p.x, -p.y)`).
        let anchor = (p.x, -p.y);
        let b = text_bbox(
            &plan.text,
            anchor,
            DEFAULT_TEXT_SIZE_MM,
            outward_label_rot(p.angle),
            plan.kind.text_kind(),
        );
        out.push((b.x0, b.y0));
        out.push((b.x1, b.y1));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rotation rule must be the *outward* one, not the stale
    /// `angle + 180`. Both agree on the vertical pins and disagree by
    /// 180° on the horizontal ones — the exact shape of the bug this
    /// module was written alongside.
    #[test]
    fn outward_rot_is_the_reflection_not_the_opposite() {
        // Vertical pins: the two rules coincide.
        assert_eq!(outward_label_rot(90), 270);
        assert_eq!(outward_label_rot(270), 90);
        // Horizontal pins: they are exact opposites.
        assert_eq!(outward_label_rot(0), 0);
        assert_eq!(outward_label_rot(180), 180);
    }

    /// A reserved label box must sit on the pin's OUTWARD side. A pin
    /// facing screen-left reserves space to the left of its tip and
    /// none to the right, or the reservation would push neighbours into
    /// the label rather than away from it.
    #[test]
    fn reserved_box_extends_outward_only() {
        let b = text_bbox(
            "out",
            (0.0, 0.0),
            DEFAULT_TEXT_SIZE_MM,
            outward_label_rot(180),
            TextKind::PlainLabel,
        );
        assert!(b.x0 < -1.0, "no leftward reach: {b:?}");
        assert!(b.x1 <= 1e-9, "reserved space behind the anchor: {b:?}");
    }

    /// A global label is materially wider than a plain one — it carries
    /// the tag lead on top of the string. This is why label text is the
    /// class expected to move layouts where property text could not:
    /// the plain box already exceeds the align path's 3.81 mm floor.
    #[test]
    fn global_label_is_wider_than_plain_and_exceeds_the_align_floor() {
        let plain = text_bbox(
            "out",
            (0.0, 0.0),
            DEFAULT_TEXT_SIZE_MM,
            0,
            TextKind::PlainLabel,
        );
        let global = text_bbox(
            "out",
            (0.0, 0.0),
            DEFAULT_TEXT_SIZE_MM,
            0,
            TextKind::global_label(Some("input")),
        );
        assert!(global.x1 > plain.x1);
        assert!(
            plain.x1 > 2.54,
            "a plain label already exceeds two grid cells: {}",
            plain.x1
        );
    }
}
