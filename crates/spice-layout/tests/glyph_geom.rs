//! Unit tests for [`spice_layout::glyph_geom::glyph_reach`] — the
//! ADR-14 placement-side power-glyph reservation geometry.
//!
//! The reach is the far tip of the reserved glyph zone (body +
//! net-name value text) outward of a rail pin, expressed in the
//! `world_extent` frame (element-origin-relative, eeschema y-flip
//! applied: +y is screen-down).
//!
//! The load-bearing property is **decoration agreement**: the reach
//! direction is the same raw-angle → screen-direction mapping the
//! decoration side uses (`kicad-emitter`'s `angle_to_direction`,
//! `spice-route::rails`' `outward_delta`), so the placer reserves the
//! zone where the glyph's value text will actually land. For a
//! *vertically*-facing transformed rail pin — the canonical GND-down /
//! VCC-up case every fixture exercises — that direction is genuinely
//! outward of the host body. For a *horizontally*-facing transformed
//! pin the shared convention degenerates (it points toward the body,
//! so the reach lands inside the body bbox and reserves nothing
//! extra); no fixture rotates a rail consumer that way, and the
//! reservation faithfully mirrors what decoration would do — see the
//! ADR-14 "Known scope limits" amendment. The golden table below pins
//! both regimes against drift.

mod common;

use std::collections::HashMap;

use common::{fixture_library, make_r};
use kicad_symbols::{Orientation, Rotation};
use spice_layout::glyph_geom::{VALUE_TEXT_OFFSET_MM, glyph_reach};
use spice_layout::net_class::VertPref;
use spice_resolve::{ElementKind, ElementRole, ResolvedElement};

/// A rail consumer: `Device:R` with terminal 2 (KiCad pin "2")
/// grounded — the `common_emitter` R2 shape ADR-14 was built for.
fn grounded_r() -> ResolvedElement {
    let mut e = make_r("R1");
    e.nodes = vec!["sig".into(), "0".into()];
    e
}

/// Prefs map for [`grounded_r`]: ground is the only rail net.
fn gnd_prefs() -> HashMap<String, VertPref> {
    HashMap::from([("0".to_owned(), VertPref::Down)])
}

/// Golden reach geometry for the grounded resistor at all 4 rotations
/// × mirror states. Pin "2" sits at local `(0, -3.81)` with raw inward
/// angle 90; the joint body + value-text reach is `VALUE_TEXT_OFFSET_MM`
/// = 3.81 mm past the pin tip, directly away from the body.
///
/// Every row is checked twice: against the golden pair, and from first
/// principles (correct length, and pointing away from the transformed
/// body centre). The two horizontal-facing rotations used to be exempt
/// from the second check and to expect the degenerate body centre
/// `(0, 0)` — that was not a decoration convention but a symptom of the
/// pin-angle inversion (`Symbol::pins_in` reported horizontal pins
/// pointing backwards, so the reach walked *into* the body and landed on
/// its centre). With the angle fixed, all eight rows are canonical and
/// the first-principles check applies to all of them.
///
/// `expected` is the value-text ANCHOR. On a *vertically*-facing pin
/// that is the whole reservation. On a *horizontally*-facing one the
/// reach is the text's full rendered box centred on that anchor, so the
/// anchor is its mid-point along the pin axis rather than its tip — see
/// `reserves_centred_value_text_box_on_horizontal_pins`.
#[test]
#[allow(clippy::too_many_lines)] // one golden row table; splitting it hides the pairing.
fn reach_pins_decoration_geometry_across_orientations() {
    struct Row {
        rotation: Rotation,
        mirror_y: bool,
        expected: (f64, f64),
    }
    let rows = [
        // Unmirrored: the pin sweeps down / right / up / left.
        Row {
            rotation: Rotation::R0,
            mirror_y: false,
            expected: (0.0, 7.62),
        },
        Row {
            rotation: Rotation::R90,
            mirror_y: false,
            expected: (7.62, 0.0),
        },
        Row {
            rotation: Rotation::R180,
            mirror_y: false,
            expected: (0.0, -7.62),
        },
        Row {
            rotation: Rotation::R270,
            mirror_y: false,
            expected: (-7.62, 0.0),
        },
        // Mirror-Y negates X, so only the two horizontal rows move.
        Row {
            rotation: Rotation::R0,
            mirror_y: true,
            expected: (0.0, 7.62),
        },
        Row {
            rotation: Rotation::R90,
            mirror_y: true,
            expected: (-7.62, 0.0),
        },
        Row {
            rotation: Rotation::R180,
            mirror_y: true,
            expected: (0.0, -7.62),
        },
        Row {
            rotation: Rotation::R270,
            mirror_y: true,
            expected: (7.62, 0.0),
        },
    ];

    let el = grounded_r();
    let prefs = gnd_prefs();
    for row in rows {
        let orient = Orientation {
            rotation: row.rotation,
            mirror_y: row.mirror_y,
        };
        let reach = glyph_reach(&el, orient, &prefs);
        let (left, right) = (
            reach.iter().map(|p| p.0).fold(f64::INFINITY, f64::min),
            reach.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max),
        );
        let (top, bottom) = (
            reach.iter().map(|p| p.1).fold(f64::INFINITY, f64::min),
            reach.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max),
        );
        // A vertically-facing pin reserves the bare anchor; a
        // horizontally-facing one reserves the centred value-text box
        // around it, so the anchor is its X mid-point (but NOT its Y
        // mid-point — `text_geom` hangs a descender below the baseline,
        // which is exactly the asymmetry a hand-rolled model would miss).
        let horizontal = row.expected.1 == 0.0;
        if horizontal {
            assert_eq!(reach.len(), 2, "horizontal pin reserves a box ({orient:?})");
            let mid_x = f64::midpoint(left, right);
            assert!(
                (mid_x - row.expected.0).abs() < 1e-9,
                "box X mid {mid_x} != expected anchor X {} ({orient:?})",
                row.expected.0,
            );
            assert!(
                top < row.expected.1 && bottom > row.expected.1,
                "box Y [{top}, {bottom}] must straddle the anchor ({orient:?})",
            );
        } else {
            assert_eq!(
                reach.len(),
                1,
                "vertical pin reserves the anchor ({orient:?})"
            );
            assert!(
                (left - row.expected.0).abs() < 1e-9 && (top - row.expected.1).abs() < 1e-9,
                "reach ({left}, {top}) != expected anchor {:?} ({orient:?})",
                row.expected,
            );
        }
        // Below, the first-principles checks run against the anchor.
        let (rx, ry) = (
            if horizontal {
                f64::midpoint(left, right)
            } else {
                left
            },
            if horizontal { row.expected.1 } else { top },
        );

        // Prove the "outward" claim from first principles, not just the
        // golden value: one full VALUE_TEXT_OFFSET_MM past the
        // transformed pin tip, pointing away from the body centre.
        let pins = el.symbol.pins_in(orient);
        let p = pins
            .iter()
            .find(|p| p.number == "2")
            .expect("grounded pin 2");
        let (tx, ty) = (p.x, -p.y);
        let (dx, dy) = (rx - tx, ry - ty);
        assert!(
            (dx.hypot(dy) - VALUE_TEXT_OFFSET_MM).abs() < 1e-9,
            "reach length {} != VALUE_TEXT_OFFSET_MM ({orient:?})",
            dx.hypot(dy),
        );
        // Body centre in the extent frame; outward = away from it.
        let b = el.symbol.body_bbox().expect("R body bbox");
        let (mut cx, mut cy) = (0.0, 0.0);
        for (lx, ly) in [(b.x0, b.y0), (b.x0, b.y1), (b.x1, b.y0), (b.x1, b.y1)] {
            let (wx, wy) = orient.apply_point(lx, ly);
            cx += wx / 4.0;
            cy += -wy / 4.0;
        }
        let dot = dx * (tx - cx) + dy * (ty - cy);
        assert!(
            dot > 0.0,
            "canonical reach must extend outward of the body: \
             tip=({tx}, {ty}) reach=({rx}, {ry}) centre=({cx}, {cy}) ({orient:?})"
        );
    }
}

/// A `*@power`-tagged source reserves nothing: its drawn body is
/// itself replaced by a rail glyph in decoration, so there is no host
/// body to keep a foreign-clearance zone around.
#[test]
fn power_source_is_excluded() {
    let lib = fixture_library();
    let symbol = lib
        .lookup("Simulation_SPICE:VDC")
        .expect("VDC fixture")
        .clone();
    let el = ResolvedElement {
        refdes: "VCC".to_owned(),
        kind: ElementKind::VoltageSrc,
        lib_id: "Simulation_SPICE:VDC".to_owned(),
        symbol,
        pin_mapping: vec!["1".into(), "2".into()],
        nodes: vec!["vcc".into(), "0".into()],
        value: None,
        role: ElementRole::Power("5".to_owned()),
    };
    let prefs = HashMap::from([
        ("vcc".to_owned(), VertPref::Up),
        ("0".to_owned(), VertPref::Down),
    ]);
    for orient in Orientation::ALL {
        assert!(
            glyph_reach(&el, orient, &prefs).is_empty(),
            "power source must reserve no glyph zone ({orient:?})"
        );
    }
}

/// Signal-only elements (no node in the prefs map) reserve nothing.
#[test]
fn signal_pins_reserve_nothing() {
    let el = make_r("R1"); // nodes "a"/"b": both signal
    assert!(glyph_reach(&el, Orientation::IDENTITY, &gnd_prefs()).is_empty());
}

/// Absent pin bindings are skipped safely: a rail terminal whose
/// mapped pin number is missing from the symbol, or that has no
/// mapping entry at all, contributes no reach (and no panic).
#[test]
fn missing_pin_binding_is_skipped() {
    let prefs = gnd_prefs();

    // Mapped to a pin number the symbol does not have.
    let mut el = grounded_r();
    el.pin_mapping = vec!["1".into(), "99".into()];
    assert!(glyph_reach(&el, Orientation::IDENTITY, &prefs).is_empty());

    // No mapping entry for the rail terminal at all.
    let mut el = grounded_r();
    el.pin_mapping = vec!["1".into()];
    assert!(glyph_reach(&el, Orientation::IDENTITY, &prefs).is_empty());
}

/// A non-cardinal pin angle is skipped safely (no reservation, no
/// panic). No fixture symbol carries one, so we synthesize it by
/// tilting the grounded pin.
#[test]
fn non_cardinal_pin_is_skipped() {
    let mut el = grounded_r();
    let pin = el
        .symbol
        .pins
        .iter_mut()
        .find(|p| p.number == "2")
        .expect("pin 2");
    pin.angle = 45;
    assert!(glyph_reach(&el, Orientation::IDENTITY, &gnd_prefs()).is_empty());
}

/// A rail glyph's net-name Value text is **centred** on its anchor —
/// the emitted property carries no `justify`, so KiCad centres it (the
/// same `TextKind::CenteredProperty` the V13 verifier grades against,
/// and confirmed against real `kicad-cli sch export svg` ink: a "GND"
/// label anchored at x = 25.40 renders x ∈ [23.71, 27.09]).
///
/// On a HORIZONTALLY-facing rail pin that centring puts half the string
/// along the pin's own outward axis, so reserving only the anchor left
/// ~1.7 mm of label unreserved — the gap that let a foreign body sit
/// under a rail label (ADR-14 "Known scope limits"). This pins the fix
/// directly.
///
/// It has to be a *model* test. Per ADR-14's completion finding, a
/// decoration reservation buys no observable quality until something
/// removes the layout's existing spacing slack, so no fixture-level
/// ratchet can distinguish this reservation from its absence — without
/// this test the term would be silently deletable.
#[test]
fn reserves_centred_value_text_box_on_horizontal_pins() {
    let el = grounded_r();
    let prefs = gnd_prefs();
    // R90 unmirrored: pin "2" faces screen-right (see the golden table
    // above — anchor at (7.62, 0.0)).
    let orient = Orientation {
        rotation: Rotation::R90,
        mirror_y: false,
    };
    let reach = glyph_reach(&el, orient, &prefs);
    assert_eq!(reach.len(), 2, "a horizontal rail pin reserves a box");

    let (x0, x1) = (
        reach.iter().map(|p| p.0).fold(f64::INFINITY, f64::min),
        reach.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max),
    );
    let (y0, y1) = (
        reach.iter().map(|p| p.1).fold(f64::INFINITY, f64::min),
        reach.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max),
    );

    // The reserved box is the rendered "GND" string, centred on the
    // anchor. Width comes from the ONE text model (`text_geom`), which
    // `spice2kicad/tests/rendered_text.rs` holds to real SVG ink.
    let w = kicad_symbols::text_metrics::text_width(
        &spice_layout::glyph_geom::rail_value_text("0"),
        kicad_symbols::text_geom::DEFAULT_TEXT_SIZE_MM,
    );
    // Pin "2" tips at 3.81 mm from the origin; the anchor sits one full
    // VALUE_TEXT_OFFSET_MM further out.
    let anchor_x = 3.81 + VALUE_TEXT_OFFSET_MM;
    assert!(
        (x0 - (anchor_x - w / 2.0)).abs() < 1e-9 && (x1 - (anchor_x + w / 2.0)).abs() < 1e-9,
        "reserved X [{x0}, {x1}] should be the anchor {anchor_x} ± half-width {}",
        w / 2.0,
    );
    // Crucially the OUTWARD edge now sits a real half-width past the
    // anchor: that extra span is the whole point of the fix. Before it,
    // the reservation stopped dead at `anchor_x`.
    assert!(
        x1 > anchor_x + 1.0,
        "outward edge {x1} must clear the anchor {anchor_x} by the text half-width",
    );
    // And the box has genuine perpendicular height (it is a box, not a
    // ray), straddling the pin axis.
    assert!(
        y0 < 0.0 && y1 > 0.0,
        "reserved Y [{y0}, {y1}] should straddle the pin axis",
    );
}

/// The SPICE ground net `"0"` renders as `GND`; every other rail name is
/// uppercased. Single-sourced so the placer reserves the same string
/// `spice-route::rails` draws — if these drifted, the reserved box would
/// be sized for a different label than the one KiCad renders.
#[test]
fn rail_value_text_matches_drawn_net_name() {
    use spice_layout::glyph_geom::rail_value_text;
    assert_eq!(rail_value_text("0"), "GND");
    assert_eq!(rail_value_text("vcc"), "VCC");
    assert_eq!(rail_value_text("vee"), "VEE");
    assert_eq!(rail_value_text("v+"), "V+");
}
