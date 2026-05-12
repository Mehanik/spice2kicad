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
