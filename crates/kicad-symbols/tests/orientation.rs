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

fn load_device_q_npn_bce() -> kicad_symbols::Symbol {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("Device.kicad_sym");
    let lib = Library::from_file(path).expect("parse Device fixture");
    lib.lookup("Device:Q_NPN_BCE")
        .expect("Device:Q_NPN_BCE")
        .clone()
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
    // Pin 1 is at (0, 3.81) with raw (inward, symbol-frame) angle 270.
    // Under R90 (CCW 90 deg), (x, y) -> (-y, x): (0, 3.81) -> (-3.81, 0),
    // so the pin now sits to the LEFT of the body.
    //
    // `pins_in` reports `angle` in the world OUTWARD convention, so the
    // expected value is read off that geometry: the body is to the right
    // of the pin, hence outward is Left = 180. (The raw inward angle
    // rotates 270 + 90 = 0; the conversion is `(180 - 0) % 360 = 180`.)
    let pins = r.pins_in(Orientation {
        rotation: Rotation::R90,
        mirror_y: false,
    });
    let p1 = pins.iter().find(|p| p.number == "1").expect("pin 1");
    assert!(approx_eq(p1.x, -3.81));
    assert!(approx_eq(p1.y, 0.0));
    assert_eq!(p1.angle, 180);
}

/// `pins_in` reports each pin's OUTWARD direction in the world (Y-down)
/// frame, not the raw inward `.kicad_sym` angle.
///
/// This is the regression guard for a real inversion: because the
/// conversion `θ ↦ 180 - θ` fixes 90 and 270, the bug was invisible on
/// every vertical pin (resistors, capacitors, power glyphs) and showed up
/// only on horizontal ones. The router builds its V5 outward stubs from
/// this angle, so a horizontal pin was routed deliberately *inward*.
///
/// The assertions below are derived from geometry alone, never from the
/// implementation: for each pin, the body lies on the opposite side from
/// the outward direction.
#[test]
fn pin_angle_is_world_outward_including_horizontal_pins() {
    let q = load_device_q_npn_bce();

    // Identity. Ground truth from the library: pin 1 (base) sits at
    // (-5.08, 0) with the transistor body to its right, so outward =
    // Left. Pin 2 (collector) is at (2.54, 5.08) — above the body in
    // symbol Y-up coords, i.e. world-up — so outward = Up. Pin 3
    // (emitter) mirrors it downward.
    let pins = q.pins_in(Orientation::IDENTITY);
    let by = |n: &str| pins.iter().find(|p| p.number == n).expect("pin").angle;
    assert_eq!(by("1"), 180, "base points left at identity");
    assert_eq!(by("2"), 270, "collector points up at identity");
    assert_eq!(by("3"), 90, "emitter points down at identity");

    // Mirror-Y flips the base to the right-hand side of the body, so its
    // outward direction must flip with it. This is precisely the case
    // `common_emitter` exercises, and precisely the one the old code got
    // backwards.
    let mirrored = q.pins_in(Orientation {
        rotation: Rotation::R0,
        mirror_y: true,
    });
    let mp1 = mirrored.iter().find(|p| p.number == "1").expect("pin 1");
    assert!(approx_eq(mp1.x, 5.08), "base mirrors to the right");
    assert_eq!(mp1.angle, 0, "a base on the right must point right");

    // Vertical pins are the fixed points of the conversion: unchanged.
    let mby = |n: &str| mirrored.iter().find(|p| p.number == n).expect("pin").angle;
    assert_eq!(mby("2"), 270);
    assert_eq!(mby("3"), 90);
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
