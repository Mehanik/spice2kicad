//! Stage 1 — power-symbol placement tests.

use kicad_symbols::Library;
use spice_layout::net_class::NetClass;
use spice_route::{Direction, NetSpec, PinRef, RouteRequest, route};

fn pin(idx: usize, n: u16, x: f64, y: f64, out: Direction) -> PinRef {
    PinRef {
        element_idx: idx,
        pin_number: n,
        x_mm: x,
        y_mm: y,
        outward: out,
    }
}

fn fixture_library() -> Library {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../kicad-symbols/tests/fixtures/power.kicad_sym");
    Library::from_file(&path).expect("load power.kicad_sym fixture")
}

fn count_substring(sexprs: &[lexpr::Value], needle: &str) -> usize {
    sexprs
        .iter()
        .map(std::string::ToString::to_string)
        .filter(|s| s.contains(needle))
        .count()
}

fn count_wires(sexprs: &[lexpr::Value]) -> usize {
    sexprs
        .iter()
        .filter(|s| s.to_string().trim_start_matches('(').starts_with("wire"))
        .count()
}

#[test]
fn vcc_pin_emits_power_vcc_symbol() {
    let lib = fixture_library();
    let net = NetSpec {
        name: "vcc".into(),
        class: NetClass::Power,
        pins: vec![pin(0, 1, 10.16, 20.32, Direction::Up)],
    };
    let r = route(RouteRequest {
        nets: &[net],
        scope: "root",
        library: Some(&lib),
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
    });
    assert_eq!(count_substring(&r.sexprs, "power:VCC"), 1, "{:?}", r.sexprs);
    assert_eq!(count_wires(&r.sexprs), 0, "power nets emit no wires");
    assert!(
        r.warnings.is_empty(),
        "no warnings expected: {:?}",
        r.warnings
    );
}

#[test]
fn ground_pin_emits_power_gnd_symbol() {
    let lib = fixture_library();
    let net = NetSpec {
        name: "0".into(),
        class: NetClass::Ground,
        pins: vec![pin(0, 2, 10.16, 40.64, Direction::Down)],
    };
    let r = route(RouteRequest {
        nets: &[net],
        scope: "root",
        library: Some(&lib),
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
    });
    assert_eq!(count_substring(&r.sexprs, "power:GND"), 1, "{:?}", r.sexprs);
    assert_eq!(count_wires(&r.sexprs), 0);
    assert!(r.warnings.is_empty());
}

#[test]
fn signal_net_does_not_emit_power_symbol() {
    let lib = fixture_library();
    let net = NetSpec {
        name: "out".into(),
        class: NetClass::Signal,
        pins: vec![
            pin(0, 1, 0.0, 0.0, Direction::Right),
            pin(1, 1, 10.16, 0.0, Direction::Left),
        ],
    };
    let r = route(RouteRequest {
        nets: &[net],
        scope: "root",
        library: Some(&lib),
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
    });
    assert_eq!(count_substring(&r.sexprs, "power:"), 0);
    // Stage 2a is now live: two pins on the same Y emit a single
    // (wire …) segment. Power-rail logic must still ignore Signal
    // class — that's what the `power:` tally above guards.
    assert_eq!(count_wires(&r.sexprs), 1);
    assert!(r.warnings.is_empty());
}

#[test]
fn power_symbol_rotation_extends_body_away_from_host_pin() {
    // Verifier for Issue 1 (power:GND apex pointed AT the host pin
    // before this commit). The chosen rotation must place the symbol
    // body on the far side of the anchor pin from the host's outward
    // direction. Empirical mapping (see `symbol_pose` doc):
    //
    //   host outward Down  → rotation 0   (body extends visually +Y)
    //   host outward Left  → rotation 90  (body extends -X)
    //   host outward Up    → rotation 180 (body extends -Y)
    //   host outward Right → rotation 270 (body extends +X)
    let lib = fixture_library();
    let cases = [
        (Direction::Down, "0"),
        (Direction::Left, "90"),
        (Direction::Up, "180"),
        (Direction::Right, "270"),
    ];
    for (dir, expected_rot) in cases {
        let net = NetSpec {
            name: "0".into(),
            class: NetClass::Ground,
            pins: vec![pin(0, 1, 10.16, 20.32, dir)],
        };
        let r = route(RouteRequest {
            nets: &[net],
            scope: "root",
            library: Some(&lib),
            sheet_uuid: "test-uuid",
            project_name: "test",
            obstacles: &[],
        });
        let s = r
            .sexprs
            .iter()
            .map(std::string::ToString::to_string)
            .find(|s| s.contains("power:GND"))
            .expect("power:GND present");
        // The (at x y rot) triple sits right after lib_id; just look
        // for the "10.16 20.32 <rot>)" pattern.
        let needle = format!("10.16 20.32 {expected_rot})");
        assert!(
            s.contains(&needle),
            "outward {dir:?} expected rotation {expected_rot}; got: {s}",
        );
    }
}

#[test]
fn unknown_lib_id_falls_back_to_global_label() {
    // Empty library — power:VCC will not resolve.
    let lib = Library::default();
    let net = NetSpec {
        name: "vcc".into(),
        class: NetClass::Power,
        pins: vec![pin(0, 1, 10.16, 20.32, Direction::Up)],
    };
    let r = route(RouteRequest {
        nets: &[net],
        scope: "root",
        library: Some(&lib),
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
    });
    assert_eq!(count_substring(&r.sexprs, "power:VCC"), 0);
    assert_eq!(
        count_substring(&r.sexprs, "global_label"),
        1,
        "expected fallback global_label: {:?}",
        r.sexprs
    );
    assert_eq!(r.warnings.len(), 1, "warning recorded: {:?}", r.warnings);
    assert!(
        r.warnings[0].contains("power:VCC"),
        "warning mentions lib_id: {:?}",
        r.warnings
    );
}
