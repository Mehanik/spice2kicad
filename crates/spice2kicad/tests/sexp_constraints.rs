//! Constraint tests for the emitted KiCad schematic.
//!
//! Each fixture test runs the emitter, parses the output as an
//! S-expression via `lexpr`, and asserts a *hand-curated* set of
//! relations: which symbols must exist, which must not, library-id
//! assignments, and geometric constraints (`right-of`, horizontal/
//! vertical alignment).
//!
//! Relations are hardcoded here rather than inferred from the SPICE
//! source — keeping this file independent of the parser lets it serve
//! as a target for emitter development. Tests are `#[ignore]`d while
//! the schematic emitter is a stub; flip them on as the emitter learns
//! each fixture (`cargo test -p spice2kicad --test sexp_constraints --
//! --ignored`).
//!
//! The wrapper itself (`common::sexp::KicadSch`) is exercised by the
//! self-tests at the bottom, which run on every `cargo test`.

mod common;

use std::path::PathBuf;

use common::sexp::{
    KicadSch, assert_aligned_horizontal, assert_all_on_grid, assert_has_components,
    assert_lacks_components, assert_lib_id, assert_on_grid, assert_right_of,
};
use common::spice_to_kicad;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> PathBuf {
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("spice2kicad-sexp-{pid}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn emit_sch(name: &str) -> KicadSch {
    let src = fixtures_dir().join(format!("{name}.cir"));
    let tmp = tempdir(name);
    let sch_path = spice_to_kicad(&src, &tmp).expect("spice2kicad");
    let body = std::fs::read_to_string(&sch_path).expect("read .kicad_sch");
    KicadSch::parse(&body).expect("parse emitted schematic")
}

// --- per-fixture constraint tests ---------------------------------------

#[test]

fn rc_lowpass_constraints() {
    let sch = emit_sch("rc_lowpass");
    // Topology: one resistor, one capacitor (V1 is `;@ ignore`d).
    assert_has_components(&sch, &["R1", "C1"]);
    assert_lacks_components(&sch, &["V1"]);
    // *@symbol Device:R_US for=R* / *@symbol Device:C for=C*
    assert_lib_id(&sch, "R1", "Device:R_US");
    assert_lib_id(&sch, "C1", "Device:C");
}

#[test]

fn common_emitter_constraints() {
    let sch = emit_sch("common_emitter");
    assert_has_components(&sch, &["R1", "R2", "RC", "RE", "CE", "CIN", "COUT", "Q1"]);
    // ;@ ignore must drop these; the `;@ power=` source VCC is a rail,
    // not a drawn component (V10 / annotation-spec §4.5), so it too is
    // absent from the schematic.
    assert_lacks_components(&sch, &["VIN", "RL", "VCC"]);
    // *@symbol overrides:
    assert_lib_id(&sch, "R1", "Device:R_US");
    assert_lib_id(&sch, "RC", "Device:R_US");
    assert_lib_id(&sch, "CE", "Device:C");
    assert_lib_id(&sch, "Q1", "Device:Q_NPN_BCE");
}

#[test]
fn opamp_inverting_constraints() {
    let sch = emit_sch("opamp_inverting");
    assert_has_components(&sch, &["RIN", "RF", "X1"]);
    // VIN is `;@ ignore`d; VCC/VEE are `;@ power=` rails, not drawn.
    assert_lacks_components(&sch, &["VIN", "VCC", "VEE"]);
    assert_lib_id(&sch, "RIN", "Device:R_US");
    assert_lib_id(&sch, "RF", "Device:R_US");
    // X1 lands on its own hierarchical sheet — its body element E1 must
    // not appear at the top level of the parent schematic.
    assert_lacks_components(&sch, &["E1"]);
}

#[test]
fn multivibrator_constraints() {
    let sch = emit_sch("multivibrator");
    assert_has_components(&sch, &["RC1", "RC2", "RB1", "RB2", "C1", "C2", "Q1", "Q2"]);
    // VCC is a `;@ power=` rail, not a drawn component.
    assert_lacks_components(&sch, &["VCC"]);
    assert_lib_id(&sch, "Q1", "Device:Q_NPN_BCE");
    assert_lib_id(&sch, "Q2", "Device:Q_NPN_BCE");
    // *@align horizontal Q1 Q2
    assert_aligned_horizontal(&sch, &["Q1", "Q2"]);
    // ;@ place=right-of Q1 on Q2
    assert_right_of(&sch, "Q2", "Q1");
}

#[test]
fn diff_pair_constraints() {
    let sch = emit_sch("diff_pair");
    assert_has_components(&sch, &["RC1", "RC2", "RTAIL", "Q1", "Q2"]);
    // VIN1/VIN2 are `;@ ignore`d; VCC/VEE are `;@ power=` rails.
    assert_lacks_components(&sch, &["VIN1", "VIN2", "VCC", "VEE"]);
    // Two horizontal-align groups from *@align:
    assert_aligned_horizontal(&sch, &["Q1", "Q2"]);
    assert_aligned_horizontal(&sch, &["RC1", "RC2"]);
    // Two right-of placements:
    assert_right_of(&sch, "Q2", "Q1");
    assert_right_of(&sch, "RC2", "RC1");
}

// --- self-tests for the wrapper -----------------------------------------
//
// These run on every `cargo test` and exercise the lexpr-based query
// layer against a literal KiCad-style schematic snippet — no emitter
// dependency, so they catch wrapper bugs even while the emitter is a
// stub.

const SAMPLE_SCH: &str = r#"
(kicad_sch (version 20231120) (generator spice2kicad)
  (symbol (lib_id "Device:R") (at 50.8 25.4 0)
    (property "Reference" "R1" (at 0 0 0))
    (property "Value" "1k" (at 0 0 0)))
  (symbol (lib_id "Device:C") (at 76.2 25.4 0)
    (property "Reference" "C1" (at 0 0 0))
    (property "Value" "100n" (at 0 0 0)))
  (symbol (lib_id "Simulation_SPICE:VDC") (at 25.4 25.4 0)
    (property "Reference" "V1" (at 0 0 0))
    (property "Value" "1" (at 0 0 0))))
"#;

#[test]
fn sample_parses_and_lists_components() {
    let sch = KicadSch::parse(SAMPLE_SCH).expect("parse sample");
    let mut got = sch.refdes_set();
    got.sort();
    assert_eq!(got, vec!["C1".to_string(), "R1".into(), "V1".into()]);
}

#[test]
fn sample_lib_id_lookup() {
    let sch = KicadSch::parse(SAMPLE_SCH).unwrap();
    assert_lib_id(&sch, "R1", "Device:R");
    assert_lib_id(&sch, "C1", "Device:C");
}

#[test]
fn sample_right_of_holds() {
    let sch = KicadSch::parse(SAMPLE_SCH).unwrap();
    // C1 at x=76.2, R1 at x=50.8, both y=25.4 — C1 is right of R1.
    assert_right_of(&sch, "C1", "R1");
}

#[test]
#[should_panic(expected = "is not right of")]
fn sample_right_of_violation_panics() {
    let sch = KicadSch::parse(SAMPLE_SCH).unwrap();
    // R1 is *left* of C1, so this must fail.
    assert_right_of(&sch, "R1", "C1");
}

#[test]
fn sample_horizontal_align_holds() {
    let sch = KicadSch::parse(SAMPLE_SCH).unwrap();
    assert_aligned_horizontal(&sch, &["R1", "C1", "V1"]);
}

#[test]
#[should_panic(expected = "horizontal-align violation")]
fn horizontal_align_violation_panics() {
    let bad = r#"
        (kicad_sch (version 20231120) (generator t)
          (symbol (lib_id "Device:R") (at 10 10 0)
            (property "Reference" "R1" (at 0 0 0)))
          (symbol (lib_id "Device:R") (at 20 30 0)
            (property "Reference" "R2" (at 0 0 0))))
    "#;
    let sch = KicadSch::parse(bad).unwrap();
    assert_aligned_horizontal(&sch, &["R1", "R2"]);
}

#[test]
#[should_panic(expected = "expected components")]
fn missing_component_panics() {
    let sch = KicadSch::parse(SAMPLE_SCH).unwrap();
    assert_has_components(&sch, &["R1", "QNOPE"]);
}

#[test]
#[should_panic(expected = "leaked into schematic")]
fn ignored_leak_panics() {
    let sch = KicadSch::parse(SAMPLE_SCH).unwrap();
    assert_lacks_components(&sch, &["R1"]);
}

#[test]
fn sample_all_on_grid_passes() {
    // 50.8, 25.4, 76.2 are all integer multiples of 1.27 mm.
    let sch = KicadSch::parse(SAMPLE_SCH).unwrap();
    assert_all_on_grid(&sch);
}

#[test]
#[should_panic(expected = "not on grid")]
fn off_grid_position_panics() {
    let bad = r#"
        (kicad_sch (version 20231120) (generator t)
          (symbol (lib_id "Device:R") (at 50.5 25.4 0)
            (property "Reference" "R1" (at 0 0 0))))
    "#;
    let sch = KicadSch::parse(bad).unwrap();
    assert_on_grid(&sch, "R1");
}

#[test]
#[should_panic(expected = "incompatible orientation")]
fn assert_aligned_horizontal_mixed_orientation_panics() {
    let bad = r#"
        (kicad_sch (version 20231120) (generator t)
          (symbol (lib_id "Device:R") (at 25.4 25.4 0)
            (property "Reference" "R1" (at 0 0 0)))
          (symbol (lib_id "Device:R") (at 50.8 25.4 90)
            (property "Reference" "R2" (at 0 0 0))))
    "#;
    let sch = KicadSch::parse(bad).unwrap();
    assert_aligned_horizontal(&sch, &["R1", "R2"]);
}

#[test]
fn assert_right_of_uses_pins_not_centers() {
    // Two horizontal resistors (rotated 90°). Pins for Device:R live at
    // local (0, ±3.81); after a 90° rotation they sit at (±3.81, 0). With
    // R1 at world x=25.4 and R2 at world x=38.1, R1's right pin is at
    // x=29.21 and R2's left pin is at x=34.29 — pin-anchored "right of"
    // holds, and centers (also distinct, R2.x > R1.x) agree.
    let sch_text = r#"
        (kicad_sch (version 20231120) (generator t)
          (symbol (lib_id "Device:R") (at 25.4 25.4 90)
            (property "Reference" "R1" (at 0 0 0)))
          (symbol (lib_id "Device:R") (at 38.1 25.4 90)
            (property "Reference" "R2" (at 0 0 0))))
    "#;
    let sch = KicadSch::parse(sch_text).unwrap();
    assert_right_of(&sch, "R2", "R1");
}
