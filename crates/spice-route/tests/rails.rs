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
        drives: false,
        requires_driver: false,
        on_sheet_edge: false,
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
        negative_rail: false,
    };
    let r = route(RouteRequest {
        nets: &[net],
        scope: "root",
        library: Some(&lib),
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
        bounds: None,
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
        negative_rail: false,
    };
    let r = route(RouteRequest {
        nets: &[net],
        scope: "root",
        library: Some(&lib),
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
        bounds: None,
    });
    assert_eq!(count_substring(&r.sexprs, "power:GND"), 1, "{:?}", r.sexprs);
    assert_eq!(count_wires(&r.sexprs), 0);
    assert!(r.warnings.is_empty());
}

#[test]
fn negative_rail_emits_power_vee_not_gnd() {
    // A negative supply rail (Ground-class for layout, but flagged
    // `negative_rail`) must render with the distinct `power:VEE` glyph,
    // never the ground triangle `power:GND` (V10). The pin faces down
    // (a negative rail sits in the bottom band like ground); the VEE
    // glyph attaches just like a GND glyph (canonical axis Down), so a
    // down-facing pin is *not* forced-sideways — same geometry as the
    // GND glyph it replaces, only the lib_id differs.
    let lib = fixture_library();
    let net = NetSpec {
        name: "vee".into(),
        class: NetClass::Ground,
        pins: vec![pin(0, 1, 10.16, 40.64, Direction::Down)],
        negative_rail: true,
    };
    let r = route(RouteRequest {
        nets: &[net],
        scope: "root",
        library: Some(&lib),
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
        bounds: None,
    });
    assert_eq!(count_substring(&r.sexprs, "power:VEE"), 1, "{:?}", r.sexprs);
    assert_eq!(
        count_substring(&r.sexprs, "power:GND"),
        0,
        "negative rail must not emit a ground glyph: {:?}",
        r.sexprs
    );
    assert!(r.warnings.is_empty(), "no warnings: {:?}", r.warnings);
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
        negative_rail: false,
    };
    let r = route(RouteRequest {
        nets: &[net],
        scope: "root",
        library: Some(&lib),
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
        bounds: None,
    });
    assert_eq!(count_substring(&r.sexprs, "power:"), 0);
    // Stage 2a is now live: two pins on the same Y emit a single
    // (wire …) segment. Power-rail logic must still ignore Signal
    // class — that's what the `power:` tally above guards.
    assert_eq!(count_wires(&r.sexprs), 1);
    assert!(r.warnings.is_empty());
}

#[test]
fn power_symbol_rotation_always_zero_v14() {
    // V14 — GND glyphs always render at rot 0 (triangle points
    // visually DOWN); VCC glyphs always render at rot 0 (chevron
    // points visually UP). Host pin's outward direction does not
    // alter rotation. Cases where the locked orientation overlaps
    // the host body are quality defects flagged by V13's verifier;
    // V14's contract is purely "no surprising rotations".
    let lib = fixture_library();
    for dir in [
        Direction::Down,
        Direction::Left,
        Direction::Up,
        Direction::Right,
    ] {
        let net = NetSpec {
            name: "0".into(),
            class: NetClass::Ground,
            pins: vec![pin(0, 1, 10.16, 20.32, dir)],
            negative_rail: false,
        };
        let r = route(RouteRequest {
            nets: &[net],
            scope: "root",
            library: Some(&lib),
            sheet_uuid: "test-uuid",
            project_name: "test",
            obstacles: &[],
            bounds: None,
        });
        let s = r
            .sexprs
            .iter()
            .map(std::string::ToString::to_string)
            .find(|s| s.contains("power:GND"))
            .expect("power:GND present");
        // V14: rotation is always 0. The glyph's *anchor* sits on the
        // pin in every case except the *forced-sideways* one (a GND pin
        // pointing screen-up, into the host body), where it is offset
        // one grid cell along the pin's outward direction (Up → file-Y
        // 20.32 - 1.27 = 19.05). Horizontal pins keep the on-pin anchor
        // (the glyph hangs off to the side, clear of the body). The
        // trailing rotation token is `0` in every case.
        let expected_anchor = if dir == Direction::Up {
            "10.16 19.05 0)" // forced-sideways: offset up along outward
        } else {
            "10.16 20.32 0)" // on-pin (Down / Left / Right)
        };
        assert!(
            s.contains(expected_anchor),
            "outward {dir:?}: expected anchor `{expected_anchor}` at rot 0; got: {s}",
        );
    }
}

#[test]
fn forced_sideways_ground_glyph_offsets_with_stub_wire() {
    // V14 forced-sideways fallback: a GND pin facing *up* (into the
    // host body) gets its glyph offset one cell along the pin's outward
    // direction (up → file-Y 19.05) plus a one-cell stub wire, so the
    // rot-0 triangle clears the host body and the stub doubles as the
    // pin's V5 outward first segment.
    let lib = fixture_library();
    let net = NetSpec {
        name: "0".into(),
        class: NetClass::Ground,
        pins: vec![pin(0, 1, 10.16, 20.32, Direction::Up)],
        negative_rail: false,
    };
    let r = route(RouteRequest {
        nets: &[net],
        scope: "root",
        library: Some(&lib),
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
        bounds: None,
    });
    // Exactly one stub wire from (10.16, 20.32) to (10.16, 19.05).
    assert_eq!(count_wires(&r.sexprs), 1, "expected one stub wire");
    let wire = r
        .sexprs
        .iter()
        .map(std::string::ToString::to_string)
        .find(|s| s.trim_start_matches('(').starts_with("wire"))
        .expect("stub wire present");
    assert!(
        wire.contains("10.16 20.32") && wire.contains("10.16 19.05"),
        "stub wire endpoints: {wire}",
    );
}

#[test]
fn canonical_ground_glyph_has_no_stub_wire() {
    // A GND pin facing down (canonical) needs no stub: the glyph sits
    // on the pin coordinate.
    let lib = fixture_library();
    let net = NetSpec {
        name: "0".into(),
        class: NetClass::Ground,
        pins: vec![pin(0, 1, 10.16, 20.32, Direction::Down)],
        negative_rail: false,
    };
    let r = route(RouteRequest {
        nets: &[net],
        scope: "root",
        library: Some(&lib),
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
        bounds: None,
    });
    assert_eq!(count_wires(&r.sexprs), 0, "canonical glyph emits no stub");
}

#[test]
fn unknown_lib_id_falls_back_to_global_label() {
    // Empty library — power:VCC will not resolve.
    let lib = Library::default();
    let net = NetSpec {
        name: "vcc".into(),
        class: NetClass::Power,
        pins: vec![pin(0, 1, 10.16, 20.32, Direction::Up)],
        negative_rail: false,
    };
    let r = route(RouteRequest {
        nets: &[net],
        scope: "root",
        library: Some(&lib),
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
        bounds: None,
    });
    assert_eq!(count_substring(&r.sexprs, "power:VCC"), 0);
    assert_eq!(
        count_substring(&r.sexprs, "global_label"),
        1,
        "expected fallback global_label: {:?}",
        r.sexprs
    );
    // Two warnings now: the VCC-glyph fallback AND the PWR_FLAG driver
    // that can't be inlined from the empty library (a Power net always
    // requires a driver). Both are legitimate "missing lib_id"
    // diagnostics; neither is faked.
    assert!(
        r.warnings.iter().any(|w| w.contains("power:VCC")),
        "warning mentions VCC lib_id: {:?}",
        r.warnings
    );
    assert!(
        r.warnings.iter().any(|w| w.contains("PWR_FLAG")),
        "warning mentions PWR_FLAG lib_id: {:?}",
        r.warnings
    );
}

/// Extract the `(property "Value" "<text>" …)` argument from the first
/// power-symbol s-expr containing `lib_id`.
fn power_glyph_value(sexprs: &[lexpr::Value], lib_id: &str) -> String {
    let s = sexprs
        .iter()
        .map(std::string::ToString::to_string)
        .find(|s| s.contains(lib_id) && s.contains("(symbol"))
        .unwrap_or_else(|| panic!("no power symbol with {lib_id}: {sexprs:?}"));
    let marker = "(property \"Value\" \"";
    let start = s.find(marker).expect("Value property present") + marker.len();
    let rest = &s[start..];
    let end = rest.find('"').expect("closing quote");
    rest[..end].to_string()
}

#[test]
fn power_glyph_value_is_uppercase_rail_name() {
    // R-6: a power glyph's Value IS its KiCad net name. Render the
    // canonical UPPERCASE rail name so the schematic reads `VCC`/`VEE`,
    // not the raw lowercase SPICE token, while preserving net identity
    // (distinct rails stay distinct).
    let lib = fixture_library();
    let net = NetSpec {
        name: "vcc".into(),
        class: NetClass::Power,
        pins: vec![pin(0, 1, 10.16, 20.32, Direction::Up)],
        negative_rail: false,
    };
    let r = route(RouteRequest {
        nets: &[net],
        scope: "root",
        library: Some(&lib),
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
        bounds: None,
    });
    assert_eq!(power_glyph_value(&r.sexprs, "power:VCC"), "VCC");
}

#[test]
fn negative_rail_glyph_value_is_uppercase() {
    let lib = fixture_library();
    let net = NetSpec {
        name: "vee".into(),
        class: NetClass::Ground,
        pins: vec![pin(0, 1, 10.16, 40.64, Direction::Down)],
        negative_rail: true,
    };
    let r = route(RouteRequest {
        nets: &[net],
        scope: "root",
        library: Some(&lib),
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
        bounds: None,
    });
    assert_eq!(power_glyph_value(&r.sexprs, "power:VEE"), "VEE");
}

#[test]
fn ground_net_zero_glyph_value_is_gnd() {
    // The SPICE ground net is named "0"; conventionally its glyph Value
    // is `GND` (ground is a single net, so the rename is safe).
    let lib = fixture_library();
    let net = NetSpec {
        name: "0".into(),
        class: NetClass::Ground,
        pins: vec![pin(0, 2, 10.16, 40.64, Direction::Down)],
        negative_rail: false,
    };
    let r = route(RouteRequest {
        nets: &[net],
        scope: "root",
        library: Some(&lib),
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
        bounds: None,
    });
    assert_eq!(power_glyph_value(&r.sexprs, "power:GND"), "GND");
}

#[test]
fn plus_rail_name_preserves_distinct_identity() {
    // `v+` uppercases to `V+` (distinct from VCC) — net identity must
    // survive the case fold.
    let lib = fixture_library();
    let net = NetSpec {
        name: "v+".into(),
        class: NetClass::Power,
        pins: vec![pin(0, 1, 10.16, 20.32, Direction::Up)],
        negative_rail: false,
    };
    let r = route(RouteRequest {
        nets: &[net],
        scope: "root",
        library: Some(&lib),
        sheet_uuid: "test-uuid",
        project_name: "test",
        obstacles: &[],
        bounds: None,
    });
    // lib_id maps to VCC (default positive), but Value preserves "V+".
    assert_eq!(power_glyph_value(&r.sexprs, "power:VCC"), "V+");
}
