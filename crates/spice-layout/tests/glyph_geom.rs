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
/// × mirror states. Pin "2" sits at local `(0, -3.81)`, angle 90; the
/// joint body + value-text reach is `VALUE_TEXT_OFFSET_MM` = 3.81 mm.
///
/// Rows where the transformed pin faces vertically (`outward: true`)
/// reach one full offset PAST the pin tip, away from the body — the
/// canonical case (identity row `(0, 7.62)` matches the emitted
/// `common_emitter` R2 geometry: GND glyph on the pin, net name one
/// offset below). Rows where it faces horizontally (`outward: false`)
/// degenerate to the body centre `(0, 0)` — the decoration-convention
/// blind spot documented in ADR-14's scope-limits amendment.
#[test]
fn reach_pins_decoration_geometry_across_orientations() {
    struct Row {
        rotation: Rotation,
        mirror_y: bool,
        expected: (f64, f64),
        outward: bool,
    }
    let rows = [
        Row {
            rotation: Rotation::R0,
            mirror_y: false,
            expected: (0.0, 7.62),
            outward: true,
        },
        Row {
            rotation: Rotation::R90,
            mirror_y: false,
            expected: (0.0, 0.0),
            outward: false,
        },
        Row {
            rotation: Rotation::R180,
            mirror_y: false,
            expected: (0.0, -7.62),
            outward: true,
        },
        Row {
            rotation: Rotation::R270,
            mirror_y: false,
            expected: (0.0, 0.0),
            outward: false,
        },
        Row {
            rotation: Rotation::R0,
            mirror_y: true,
            expected: (0.0, 7.62),
            outward: true,
        },
        Row {
            rotation: Rotation::R90,
            mirror_y: true,
            expected: (0.0, 0.0),
            outward: false,
        },
        Row {
            rotation: Rotation::R180,
            mirror_y: true,
            expected: (0.0, -7.62),
            outward: true,
        },
        Row {
            rotation: Rotation::R270,
            mirror_y: true,
            expected: (0.0, 0.0),
            outward: false,
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
        assert_eq!(reach.len(), 1, "one rail pin, one reach ({orient:?})");
        let (rx, ry) = reach[0];
        assert!(
            (rx - row.expected.0).abs() < 1e-9 && (ry - row.expected.1).abs() < 1e-9,
            "reach ({rx}, {ry}) != expected {:?} ({orient:?})",
            row.expected,
        );

        if !row.outward {
            continue;
        }
        // Canonical (vertically-facing) rows: additionally prove the
        // "outward" claims from first principles, not just the golden
        // values — one full VALUE_TEXT_OFFSET_MM past the transformed
        // pin tip, pointing away from the body centre.
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
