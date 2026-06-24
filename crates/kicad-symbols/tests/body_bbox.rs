//! Unit tests for [`Symbol::body_bbox`].
//!
//! These tests pin the local-frame body extent for the standard
//! Device-library symbols used across `spice2kicad`'s v0.1 fixtures.
//! The router treats the bbox (translated to world frame) as an
//! obstacle, so a regression in this geometry directly degrades
//! V12 wire-vs-body avoidance.

use kicad_symbols::Library;
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn dev_library() -> Library {
    Library::from_file(fixtures().join("Device.kicad_sym")).expect("parse Device library")
}

#[test]
fn body_bbox_q_npn_bce_covers_circle() {
    // Q_NPN_BCE's body is a circle centred at (1.27, 0) with radius
    // 2.8194 mm (the transistor outline), plus the BCE polylines.
    // The bbox should cover at least the circle's [(-1.5494, -2.8194)
    // .. (4.0894, 2.8194)] extent.
    let lib = dev_library();
    let sym = lib.lookup("Device:Q_NPN_BCE").expect("Q_NPN_BCE present");
    let bb = sym.body_bbox().expect("body_bbox should be Some");
    // Circle bbox lower-left corner (with floating tolerance).
    assert!(bb.x0 <= -1.5494 + 0.01, "x0={}", bb.x0);
    assert!(bb.y0 <= -2.8194 + 0.01, "y0={}", bb.y0);
    assert!(bb.x1 >= 4.0894 - 0.01, "x1={}", bb.x1);
    assert!(bb.y1 >= 2.8194 - 0.01, "y1={}", bb.y1);
}

#[test]
fn body_bbox_r_us_covers_zigzag() {
    // R_US's body is a single 8-vertex polyline spanning roughly
    // x ∈ [-1.016, 1.016], y ∈ [-2.54, 2.54].
    let lib = dev_library();
    let sym = lib.lookup("Device:R_US").expect("R_US present");
    let bb = sym.body_bbox().expect("body_bbox should be Some");
    assert!(bb.x0 <= -1.016 + 0.01 && bb.x1 >= 1.016 - 0.01);
    assert!(bb.y0 <= -2.54 + 0.01 && bb.y1 >= 2.54 - 0.01);
}

#[test]
fn body_bbox_c_covers_plates() {
    // C is two horizontal plate polylines at y = ±0.762, extending
    // x ∈ [-2.032, 2.032].
    let lib = dev_library();
    let sym = lib.lookup("Device:C").expect("C present");
    let bb = sym.body_bbox().expect("body_bbox should be Some");
    assert!(bb.x0 <= -2.032 + 0.01 && bb.x1 >= 2.032 - 0.01);
    assert!(bb.y0 <= -0.762 + 0.01 && bb.y1 >= 0.762 - 0.01);
}

#[test]
fn body_bbox_q_npn_bce_x_excludes_base_pin_stem() {
    // Q_NPN_BCE's base pin protrudes to local x = -5.08 (the long
    // horizontal pin stem). The body bbox must NOT cover that x —
    // pin nodes are explicitly excluded from the walk.
    // (The collector/emitter "stems" at y = ±5.08 are drawn as
    // polylines inside the body, so they DO count — those vertices
    // are part of the schematic glyph, not pin geometry.)
    let lib = dev_library();
    let sym = lib.lookup("Device:Q_NPN_BCE").expect("Q_NPN_BCE present");
    let bb = sym.body_bbox().expect("body_bbox");
    assert!(
        bb.x0 > -5.08 + 0.5,
        "base pin tip should not be inside body bbox: x0={}",
        bb.x0,
    );
}

#[test]
fn pin_text_headers_parsed() {
    // Q_NPN_BCE: names + numbers shown, offset 0 (over the pin).
    let lib = dev_library();
    let q = lib.lookup("Device:Q_NPN_BCE").expect("Q_NPN_BCE");
    assert!(q.show_pin_names);
    assert!(q.show_pin_numbers);
    assert!((q.pin_name_offset - 0.0).abs() < 1e-9);

    // R_US: `(pin_numbers (hide yes)) (pin_names (offset 0))` — numbers
    // hidden, names shown but `~` (no glyph), offset 0.
    let r = lib.lookup("Device:R_US").expect("R_US");
    assert!(r.show_pin_names);
    assert!(!r.show_pin_numbers);
    assert!((r.pin_name_offset - 0.0).abs() < 1e-9);

    // C: `(pin_names (offset 0.254))`, no pin_numbers token → names
    // shown (offset 0.254, inside; but `~`), numbers shown.
    let c = lib.lookup("Device:C").expect("C");
    assert!(c.show_pin_names);
    assert!(c.show_pin_numbers);
    assert!((c.pin_name_offset - 0.254).abs() < 1e-9);
}

#[test]
fn pin_text_local_bboxes_skip_tilde_names_and_hidden_classes() {
    let lib = dev_library();

    // C: names are `~` (no glyph) but numbers "1"/"2" are shown → two
    // boxes (one per pin number), none for the tilde names.
    let c = lib.lookup("Device:C").expect("C");
    assert_eq!(c.pin_text_local_bboxes().len(), 2);

    // R_US: numbers hidden, names `~` (no glyph) → zero boxes.
    let r = lib.lookup("Device:R_US").expect("R_US");
    assert_eq!(r.pin_text_local_bboxes().len(), 0);

    // Q_NPN_BCE: 3 pins, names {B,C,E} + numbers {1,2,3} all visible →
    // six boxes. The base pin (tip -5.08, angle 0, length 5.715) puts
    // its name/number near the shaft midpoint (x ≈ -2.22), well clear
    // of the connection tip.
    let q = lib.lookup("Device:Q_NPN_BCE").expect("Q_NPN_BCE");
    let boxes = q.pin_text_local_bboxes();
    assert_eq!(boxes.len(), 6);
    // At least one box should straddle x ≈ -2.22 (base pin shaft mid).
    assert!(
        boxes.iter().any(|b| b.x0 < -2.0 && b.x1 > -2.4),
        "expected a base-pin text box near shaft midpoint x≈-2.22: {boxes:?}",
    );
}
