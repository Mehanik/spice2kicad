//! Exhaustive orientation transform tests over the 8-state group.

use std::path::PathBuf;

use kicad_symbols::{Library, Orientation, Rotation};

fn load_device_r() -> kicad_symbols::Symbol {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("Device.kicad_sym");
    let lib = Library::from_file(path).expect("parse Device fixture");
    lib.lookup("Device:R").expect("Device:R").clone()
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn pin_layout_key(pins: &[kicad_symbols::TransformedPin]) -> Vec<(String, i64, i64, u16)> {
    // Quantise to 1e-6 mm so positions become hashable / orderable.
    // Fixture pins are bounded by a few mm; the cast is exact in this range.
    #[allow(clippy::cast_possible_truncation)] // values bounded by fixture geometry
    fn q(v: f64) -> i64 {
        (v * 1_000_000.0).round() as i64
    }
    let mut v: Vec<_> = pins
        .iter()
        .map(|p| (p.number.clone(), q(p.x), q(p.y), p.angle))
        .collect();
    v.sort();
    v
}

#[test]
fn identity_is_identity() {
    let r = load_device_r();
    let pins = r.pins_in(Orientation::IDENTITY);
    assert_eq!(pins.len(), 2);
    for (orig, t) in r.pins.iter().zip(pins.iter()) {
        assert!(approx_eq(orig.x, t.x));
        assert!(approx_eq(orig.y, t.y));
        assert_eq!(orig.angle, t.angle);
    }
}

#[test]
fn rotate_90_four_times_is_identity() {
    let r = load_device_r();
    let original = pin_layout_key(&r.pins_in(Orientation::IDENTITY));
    let mut o = Orientation::IDENTITY;
    for _ in 0..4 {
        o = o.rotate_90();
    }
    assert_eq!(o, Orientation::IDENTITY);
    assert_eq!(pin_layout_key(&r.pins_in(o)), original);
}

#[test]
fn flip_twice_is_identity() {
    let r = load_device_r();
    let original = pin_layout_key(&r.pins_in(Orientation::IDENTITY));
    let o = Orientation::IDENTITY.flip().flip();
    assert_eq!(o, Orientation::IDENTITY);
    assert_eq!(pin_layout_key(&r.pins_in(o)), original);
}

#[test]
fn rotate_90_moves_pin_predictably() {
    let r = load_device_r();
    // Pin 1 is at (0, 3.81) angle 270.
    // Under R90 (CCW 90 deg), (x, y) -> (-y, x): (0, 3.81) -> (-3.81, 0).
    // Angle 270 rotated +90 = 360 % 360 = 0.
    let pins = r.pins_in(Orientation {
        rotation: Rotation::R90,
        mirror_y: false,
    });
    let p1 = pins.iter().find(|p| p.number == "1").expect("pin 1");
    assert!(approx_eq(p1.x, -3.81));
    assert!(approx_eq(p1.y, 0.0));
    assert_eq!(p1.angle, 0);
}

#[test]
fn mirror_swaps_x_axis_pin_angles() {
    // For Device:R, the pins lie on the Y axis so position is unchanged
    // by mirror-Y, but a hypothetical pin pointing 0 deg would become 180.
    // Instead we exercise apply_angle directly.
    assert_eq!(
        Orientation {
            rotation: Rotation::R0,
            mirror_y: true,
        }
        .apply_angle(0),
        180
    );
    assert_eq!(
        Orientation {
            rotation: Rotation::R0,
            mirror_y: true,
        }
        .apply_angle(180),
        0
    );
    assert_eq!(
        Orientation {
            rotation: Rotation::R0,
            mirror_y: true,
        }
        .apply_angle(90),
        90
    );
    assert_eq!(
        Orientation {
            rotation: Rotation::R0,
            mirror_y: true,
        }
        .apply_angle(270),
        270
    );
}

#[test]
fn all_eight_orientations_are_listed() {
    use std::collections::HashSet;
    let set: HashSet<_> = Orientation::ALL.iter().copied().collect();
    assert_eq!(
        set.len(),
        8,
        "Orientation::ALL should hold 8 distinct values"
    );
}

#[test]
fn device_r_collapses_to_four_distinct_layouts() {
    // Device:R is symmetric on the X axis (pins on the Y axis, opposite
    // angles), so mirror-Y is a no-op for its pin set. The 8 orientations
    // therefore produce only 4 distinct pin layouts, with pairs collapsing
    // (mirror, rotation R) ~ (no-mirror, rotation R). Documenting this
    // collision is the purpose of this test.
    use std::collections::HashSet;
    let r = load_device_r();
    let layouts: HashSet<_> = Orientation::ALL
        .iter()
        .map(|&o| pin_layout_key(&r.pins_in(o)))
        .collect();
    assert_eq!(
        layouts.len(),
        4,
        "Device:R is symmetric across the Y axis; expect 4 distinct layouts, got {}",
        layouts.len()
    );
}
