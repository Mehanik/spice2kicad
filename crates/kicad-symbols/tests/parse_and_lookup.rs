//! Parse fixture libraries and verify lookup + pin geometry.

use std::path::PathBuf;

use kicad_symbols::Library;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn load_merged() -> Library {
    let device = Library::from_file(fixtures_dir().join("Device.kicad_sym"))
        .expect("parse Device.kicad_sym");
    let sim = Library::from_file(fixtures_dir().join("Simulation_SPICE.kicad_sym"))
        .expect("parse Simulation_SPICE.kicad_sym");
    device.merge(sim)
}

const GRID_MM: f64 = 1.27;

fn on_grid(v: f64) -> bool {
    let units = v / GRID_MM;
    (units - units.round()).abs() < 1e-6
}

#[test]
fn lookup_finds_each_expected_symbol() {
    let lib = load_merged();
    let cases = [
        ("Device:R", 2),
        ("Device:C", 2),
        ("Device:Q_NPN_BCE", 3),
        ("Simulation_SPICE:VDC", 2),
    ];
    for (lib_id, expected_count) in cases {
        let sym = lib
            .lookup(lib_id)
            .unwrap_or_else(|| panic!("missing {lib_id}"));
        assert_eq!(sym.pin_count(), expected_count, "{lib_id} pin count");
        assert_eq!(sym.lib_id, lib_id);
        for pin in &sym.pins {
            assert!(
                on_grid(pin.x) && on_grid(pin.y),
                "{lib_id} pin {} at ({}, {}) is off the 1.27 mm grid",
                pin.number,
                pin.x,
                pin.y
            );
            assert_eq!(
                pin.angle % 90,
                0,
                "{lib_id} pin {} has non-90 angle",
                pin.number
            );
        }
    }
}

#[test]
fn device_r_has_specific_pin_positions() {
    let lib = load_merged();
    let r = lib.lookup("Device:R").expect("Device:R");
    // Two pins: (0, 3.81) angle 270, number "1"; (0, -3.81) angle 90, number "2".
    let mut pins = r.pins.clone();
    pins.sort_by(|a, b| a.number.cmp(&b.number));
    assert_eq!(pins[0].number, "1");
    assert!((pins[0].x - 0.0).abs() < 1e-9);
    assert!((pins[0].y - 3.81).abs() < 1e-9);
    assert_eq!(pins[0].angle, 270);
    assert_eq!(pins[1].number, "2");
    assert!((pins[1].x - 0.0).abs() < 1e-9);
    assert!((pins[1].y - (-3.81)).abs() < 1e-9);
    assert_eq!(pins[1].angle, 90);
}

#[test]
fn lookup_unknown_returns_none() {
    let lib = load_merged();
    assert!(lib.lookup("Device:DoesNotExist").is_none());
    assert!(lib.lookup("NoSuchLib:R").is_none());
}

#[test]
fn iter_yields_all_symbols() {
    let lib = load_merged();
    let ids: Vec<&str> = lib.iter().map(|(k, _)| k).collect();
    assert_eq!(ids.len(), 4);
}
