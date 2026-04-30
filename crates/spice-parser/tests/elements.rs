//! Per-element-kind parsing tests, grounded in ngspice source.
//!
//! Each test parses a representative SPICE line and asserts kind,
//! designator, nodes, value, and (where applicable) params.
//!
//! Where our parser diverges from ngspice canonical behaviour, the
//! passing test documents what the parser actually does, and a second
//! test marked `#[ignore = "..."]` asserts the ngspice-correct shape.

mod common;
use common::{assert_kind, assert_value_number, elem, expect_tag, parse_ok, parse_with_diags};
use spice_parser::ast::{ElementKind, PinRef, Tag, Value};

// ---------------------------------------------------------------------------
// Resistor
// ---------------------------------------------------------------------------

#[test]
fn resistor_basic() {
    let nl = parse_ok("* t\nR1 in out 1k\n");
    let e = elem(&nl, "R1");
    assert_kind(e, ElementKind::Resistor);
    assert_eq!(e.nodes, ["in", "out"]);
    assert_value_number(e, 1_000.0);
}

#[test]
fn resistor_with_model() {
    // ngspice inp2r.c: R can take a model name instead of a numeric value.
    let nl = parse_ok("* t\nR1 in out RMOD\n");
    let e = elem(&nl, "R1");
    assert_kind(e, ElementKind::Resistor);
    assert_eq!(e.nodes, ["in", "out"]);
    // Parser falls through to combine_value_tokens; non-numeric → String.
    assert!(matches!(&e.value, Some(Value::String(s)) if s == "RMOD"));
}

#[test]
fn resistor_with_params() {
    // tc is a named param; value stays numeric.
    let nl = parse_ok("* t\nR1 in out 1k tc = 0.001\n");
    let e = elem(&nl, "R1");
    assert_kind(e, ElementKind::Resistor);
    assert_value_number(e, 1_000.0);
    let tc = e.params.iter().find(|(k, _)| k == "tc");
    assert!(tc.is_some(), "tc param missing");
    assert!(matches!(tc.unwrap().1, Value::Number(n) if (n - 0.001).abs() < 1e-9));
}

// ---------------------------------------------------------------------------
// Capacitor
// ---------------------------------------------------------------------------

#[test]
fn capacitor_basic() {
    let nl = parse_ok("* t\nC1 a b 100n\n");
    let e = elem(&nl, "C1");
    assert_kind(e, ElementKind::Capacitor);
    assert_eq!(e.nodes, ["a", "b"]);
    assert_value_number(e, 100e-9);
}

// ---------------------------------------------------------------------------
// Inductor
// ---------------------------------------------------------------------------

#[test]
fn inductor_basic() {
    let nl = parse_ok("* t\nL1 a b 10u\n");
    let e = elem(&nl, "L1");
    assert_kind(e, ElementKind::Inductor);
    assert_eq!(e.nodes, ["a", "b"]);
    assert_value_number(e, 10e-6);
}

// ---------------------------------------------------------------------------
// Voltage source
// ---------------------------------------------------------------------------

#[test]
fn voltage_source_dc_only() {
    // Plain numeric value — no keyword.
    let nl = parse_ok("* t\nV1 a 0 12\n");
    let e = elem(&nl, "V1");
    assert_kind(e, ElementKind::VoltageSrc);
    assert_eq!(e.nodes, ["a", "0"]);
    assert_value_number(e, 12.0);
}

#[test]
fn voltage_source_dc_keyword() {
    // `DC 12` — two tokens, parser joins to String via combine_value_tokens.
    let nl = parse_ok("* t\nV1 a 0 DC 12\n");
    let e = elem(&nl, "V1");
    assert_kind(e, ElementKind::VoltageSrc);
    assert_eq!(e.nodes, ["a", "0"]);
    // Multi-token source spec preserved as String.
    assert!(matches!(&e.value, Some(Value::String(s)) if s.contains("DC")));
}

#[test]
fn voltage_source_ac_dc() {
    // `DC 0 AC 1` — mixed spec preserved verbatim.
    let nl = parse_ok("* t\nV1 a 0 DC 0 AC 1\n");
    let e = elem(&nl, "V1");
    assert_kind(e, ElementKind::VoltageSrc);
    assert_eq!(e.nodes, ["a", "0"]);
    let s = match &e.value {
        Some(Value::String(s)) => s.clone(),
        other => panic!("expected String, got {other:?}"),
    };
    assert!(s.contains("DC") && s.contains("AC"), "got: {s}");
}

#[test]
fn voltage_source_sin() {
    // SIN( ) spec — tokens joined as String.  ngspice inp2v.c.
    let nl = parse_ok("* t\nV1 a 0 SIN ( 0 1 1k )\n");
    let e = elem(&nl, "V1");
    assert_kind(e, ElementKind::VoltageSrc);
    assert_eq!(e.nodes, ["a", "0"]);
    let s = match &e.value {
        Some(Value::String(s)) => s.clone(),
        other => panic!("expected String, got {other:?}"),
    };
    assert!(s.contains("SIN"), "got: {s}");
}

#[test]
fn voltage_source_pulse() {
    // PULSE( ) spec — seven parameters.  ngspice inp2v.c.
    let nl = parse_ok("* t\nV1 a 0 PULSE ( 0 5 0 1n 1n 10n 20n )\n");
    let e = elem(&nl, "V1");
    assert_kind(e, ElementKind::VoltageSrc);
    assert_eq!(e.nodes, ["a", "0"]);
    assert!(matches!(&e.value, Some(Value::String(s)) if s.contains("PULSE")));
}

// ---------------------------------------------------------------------------
// Current source
// ---------------------------------------------------------------------------

#[test]
fn current_source_basic() {
    // ngspice inp2i.c: identical syntax to V.
    let nl = parse_ok("* t\nI1 a 0 1m\n");
    let e = elem(&nl, "I1");
    assert_kind(e, ElementKind::CurrentSrc);
    assert_eq!(e.nodes, ["a", "0"]);
    assert_value_number(e, 1e-3);
}

// ---------------------------------------------------------------------------
// Diode
// ---------------------------------------------------------------------------

#[test]
fn diode_basic() {
    // ngspice inp2d.c: D <anode> <cathode> <model>.
    let nl = parse_ok("* t\nD1 a k DMOD\n");
    let e = elem(&nl, "D1");
    assert_kind(e, ElementKind::Diode);
    assert_eq!(e.nodes, ["a", "k"]);
    assert!(matches!(&e.value, Some(Value::String(s)) if s == "DMOD"));
}

// ---------------------------------------------------------------------------
// BJT
// ---------------------------------------------------------------------------

#[test]
fn bjt_3_terminal() {
    // ngspice inp2q.c: Q <c> <b> <e> <model>.
    let nl = parse_ok("* t\nQ1 c b e QGENERIC\n");
    let e = elem(&nl, "Q1");
    assert_kind(e, ElementKind::Bjt);
    assert_eq!(e.nodes.len(), 3);
    assert_eq!(e.nodes, ["c", "b", "e"]);
    assert!(matches!(&e.value, Some(Value::String(s)) if s == "QGENERIC"));
}

/// ngspice-correct shape for 4-terminal BJT.
/// ngspice inp2q.c: Q accepts 3 or 4 nodes; last positional token is model.
#[test]
fn bjt_4_terminal_ngspice_correct() {
    let nl = parse_ok("* t\nQ1 c b e s QGENERIC\n");
    let e = elem(&nl, "Q1");
    assert_eq!(e.nodes.len(), 4);
    assert_eq!(e.nodes, ["c", "b", "e", "s"]);
    assert!(matches!(&e.value, Some(Value::String(s)) if s == "QGENERIC"));
}

/// Alias test asserting the 4-node substrate form with explicit name.
#[test]
fn bjt_4_terminal_substrate() {
    let nl = parse_ok("* t\nQ1 c b e sub QGENERIC\n");
    let e = elem(&nl, "Q1");
    assert_kind(e, ElementKind::Bjt);
    assert_eq!(e.nodes.len(), 4);
    assert_eq!(e.nodes, ["c", "b", "e", "sub"]);
    assert!(matches!(&e.value, Some(Value::String(s)) if s == "QGENERIC"));
}

// ---------------------------------------------------------------------------
// MOSFET
// ---------------------------------------------------------------------------

#[test]
fn mosfet_with_params() {
    // ngspice inp2m.c: M <d> <g> <s> <b> <model> [L=…] [W=…].
    let nl = parse_ok("* t\nM1 d g s b NMOS L = 1u W = 10u\n");
    let e = elem(&nl, "M1");
    assert_kind(e, ElementKind::Mosfet);
    assert_eq!(e.nodes, ["d", "g", "s", "b"]);
    assert!(matches!(&e.value, Some(Value::String(s)) if s == "NMOS"));
    let by_key: std::collections::HashMap<_, _> =
        e.params.iter().map(|(k, v)| (k.as_str(), v)).collect();
    assert!(by_key.contains_key("L"), "missing L param");
    assert!(by_key.contains_key("W"), "missing W param");
    assert!(matches!(by_key["L"], Value::Number(n) if (n - 1e-6).abs() < 1e-12));
    assert!(matches!(by_key["W"], Value::Number(n) if (n - 10e-6).abs() < 1e-12));
}

// ---------------------------------------------------------------------------
// JFET
// ---------------------------------------------------------------------------

#[test]
fn jfet_basic() {
    // ngspice inp2j.c: J <d> <g> <s> <model>.
    let nl = parse_ok("* t\nJ1 d g s JMOD\n");
    let e = elem(&nl, "J1");
    assert_kind(e, ElementKind::Jfet);
    assert_eq!(e.nodes, ["d", "g", "s"]);
    assert!(matches!(&e.value, Some(Value::String(s)) if s == "JMOD"));
}

// ---------------------------------------------------------------------------
// Subcircuit instance (X)
// ---------------------------------------------------------------------------

#[test]
fn subckt_instance_variable_ports() {
    // ngspice: X <node>... <subckt-name>
    let nl = parse_ok("* t\nX1 a b c MYBLOCK\n");
    let e = elem(&nl, "X1");
    assert_kind(e, ElementKind::Subckt);
    assert_eq!(e.nodes, ["a", "b", "c"]);
    assert!(matches!(&e.value, Some(Value::String(s)) if s == "MYBLOCK"));
}

#[test]
fn subckt_instance_with_params() {
    // key=value pairs peeled before the last-token heuristic.
    let nl = parse_ok("* t\nX1 a b MYBLOCK PARAM1 = 10\n");
    let e = elem(&nl, "X1");
    assert_kind(e, ElementKind::Subckt);
    assert_eq!(e.nodes, ["a", "b"]);
    assert!(matches!(&e.value, Some(Value::String(s)) if s == "MYBLOCK"));
    let p1 = e.params.iter().find(|(k, _)| k == "PARAM1");
    assert!(p1.is_some(), "PARAM1 missing from params");
    assert!(matches!(p1.unwrap().1, Value::Number(n) if (n - 10.0).abs() < 1e-9));
}

// ---------------------------------------------------------------------------
// VCVS (E) — ElementKind::Vcvs
// ---------------------------------------------------------------------------

/// ngspice inp2e.c: E <out+> <out-> <ctrl+> <ctrl-> <gain>.
/// Fixed 4 nodes (all are nets) then a numeric gain.
#[test]
fn vcvs_e_basic() {
    let nl = parse_ok("* t\nE1 out+ out- in+ in- 1e5\n");
    let e = elem(&nl, "E1");
    assert_kind(e, ElementKind::Vcvs);
    assert_eq!(e.nodes, ["out+", "out-", "in+", "in-"]);
    assert_value_number(e, 1e5);
}

// ---------------------------------------------------------------------------
// CCCS (F) — ElementKind::Cccs
// ---------------------------------------------------------------------------

/// ngspice-correct: F1 out 0 Vsense 100.
/// ngspice inp2f.c: 2 output nets, then voltage-source refdes (control),
/// then numeric gain.
#[test]
fn cccs_f_basic_ngspice_correct() {
    let nl = parse_ok("* t\nF1 out 0 Vsense 100\n");
    let e = elem(&nl, "F1");
    assert_kind(e, ElementKind::Cccs);
    assert_eq!(e.nodes, ["out", "0"]);
    assert_eq!(e.control.as_deref(), Some("Vsense"));
    assert_value_number(e, 100.0);
}

/// CCCS (F) with too few tokens (control but no gain). Documents that the
/// parser currently does NOT emit an E-code diagnostic; tracked as a gap.
#[test]
#[ignore = "no diagnostic emitted; documented gap (parser.rs:281-302 Cccs branch)"]
fn cccs_f_too_few_tokens() {
    let out = parse_with_diags("* t\nF1 out 0 Vsense\n");
    assert!(
        out.diagnostics.iter().any(|d| d.code.starts_with('E')),
        "expected E-code diagnostic; got: {:?}",
        out.diagnostics
    );
}

/// CCCS (F) with no control reference at all (only nodes). Documents the gap.
#[test]
#[ignore = "no diagnostic emitted; documented gap (parser.rs:281-302 Cccs branch)"]
fn cccs_f_no_control() {
    let out = parse_with_diags("* t\nF1 out 0\n");
    assert!(
        out.diagnostics.iter().any(|d| d.code.starts_with('E')),
        "expected E-code diagnostic; got: {:?}",
        out.diagnostics
    );
}

/// CCVS (H): same syntax as CCCS (F). ngspice inp2h.c.
#[test]
fn ccvs_h_basic() {
    let nl = parse_ok("* t\nH1 out 0 Vsense 1k\n");
    let e = elem(&nl, "H1");
    assert_kind(e, ElementKind::Ccvs);
    assert_eq!(e.nodes, ["out", "0"]);
    assert_eq!(e.control.as_deref(), Some("Vsense"));
    assert_value_number(e, 1_000.0);
}

// ---------------------------------------------------------------------------
// Mutual inductance (K)
// ---------------------------------------------------------------------------

/// ngspice-correct: K1 L1 L2 coupling.
/// ngspice inp2k.c: L1/L2 are inductor refdes references (not nets);
/// stored in `coupled` — `nodes` stays empty per the post-D1 invariant.
#[test]
fn mutual_inductance_k_ngspice_correct() {
    let nl = parse_ok("* t\nK1 L1 L2 0.999\n");
    let e = elem(&nl, "K1");
    assert_kind(e, ElementKind::MutualInductance);
    assert_eq!(e.coupled, ["L1", "L2"]);
    assert!(e.nodes.is_empty());
    assert_value_number(e, 0.999);
}

#[test]
fn mutual_k_with_decimal_coupling() {
    let nl = parse_ok("* t\nK1 L1 L2 0.999\n");
    let e = elem(&nl, "K1");
    assert_kind(e, ElementKind::MutualInductance);
    assert_eq!(e.coupled, ["L1", "L2"]);
    assert!(e.nodes.is_empty());
    assert!(matches!(&e.value, Some(Value::Number(n)) if (n - 0.999).abs() < 1e-9));
}

// ---------------------------------------------------------------------------
// Case insensitivity
// ---------------------------------------------------------------------------

#[test]
fn case_insensitive_refdes() {
    // Lower-case prefix must still map to the correct kind.
    let nl = parse_ok("* t\nr1 a b 1k\n");
    let e = elem(&nl, "r1");
    assert_kind(e, ElementKind::Resistor);
    assert_eq!(e.nodes, ["a", "b"]);
    assert_value_number(e, 1_000.0);
}

// ---------------------------------------------------------------------------
// A6–A7. Pinmap with numeric and mixed pin references
// ---------------------------------------------------------------------------

/// §4.2 pinmap with numeric KiCad pin references (`pinmap=1:1,2:2`).
#[test]
fn pinmap_numeric_pin() {
    let nl = parse_ok("* t\nD1 a k DMOD ;@ pinmap=1:1,2:2\n");
    let d1 = elem(&nl, "D1");
    let pm = expect_tag(d1, |t| match t {
        Tag::Pinmap(v) => Some(v.clone()),
        _ => None,
    });
    assert_eq!(pm.len(), 2);
    assert_eq!(pm[0].spice_index, 1);
    assert!(
        matches!(&pm[0].kicad_pin, PinRef::Number(s) if s == "1"),
        "expected Number(1), got {:?}",
        pm[0].kicad_pin
    );
    assert_eq!(pm[1].spice_index, 2);
    assert!(
        matches!(&pm[1].kicad_pin, PinRef::Number(s) if s == "2"),
        "expected Number(2), got {:?}",
        pm[1].kicad_pin
    );
}

/// §4.2 pinmap with one numeric and one named KiCad pin reference.
#[test]
fn pinmap_mixed_number_and_name() {
    let nl = parse_ok("* t\nD1 a k DMOD ;@ pinmap=1:A,2:2\n");
    let d1 = elem(&nl, "D1");
    let pm = expect_tag(d1, |t| match t {
        Tag::Pinmap(v) => Some(v.clone()),
        _ => None,
    });
    assert_eq!(pm.len(), 2);
    assert!(
        matches!(&pm[0].kicad_pin, PinRef::Name(s) if s == "A"),
        "expected Name(A), got {:?}",
        pm[0].kicad_pin
    );
    assert!(
        matches!(&pm[1].kicad_pin, PinRef::Number(s) if s == "2"),
        "expected Number(2), got {:?}",
        pm[1].kicad_pin
    );
}

// ---------------------------------------------------------------------------
// B. Element optional-field tests
// ---------------------------------------------------------------------------

/// Resistor with `ac=` named param — expect ac in params.
#[test]
fn resistor_with_ac_param() {
    let nl = parse_ok("* t\nR1 a b 1k ac=1k\n");
    let e = elem(&nl, "R1");
    assert_kind(e, ElementKind::Resistor);
    assert!(
        e.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("ac")),
        "ac param not found; params: {:?}",
        e.params
    );
}

/// Resistor with model name and W/L params (semiconductor resistor form).
/// ngspice inp2r.c: R <n+> <n-> <model> [W=…] [L=…].
#[test]
fn resistor_with_w_l_params() {
    let nl = parse_ok("* t\nR1 a b RMOD W=10u L=1u\n");
    let e = elem(&nl, "R1");
    assert_kind(e, ElementKind::Resistor);
    assert!(matches!(&e.value, Some(Value::String(s)) if s == "RMOD"));
    assert!(
        e.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("W")),
        "W missing; params: {:?}",
        e.params
    );
    assert!(
        e.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("L")),
        "L missing; params: {:?}",
        e.params
    );
}

/// Capacitor with IC= initial condition — expect IC in params.
/// ngspice inp2c.c: optional IC= keyword.
#[test]
fn capacitor_with_ic() {
    let nl = parse_ok("* t\nC1 a b 100n IC=0.5\n");
    let e = elem(&nl, "C1");
    assert_kind(e, ElementKind::Capacitor);
    assert!(
        e.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("IC")),
        "IC param not found; params: {:?}",
        e.params
    );
}

/// Capacitor with model name form (CMOD becomes the value as String).
/// ngspice inp2c.c: C <n+> <n-> <model> — non-numeric value.
#[test]
fn capacitor_with_model_form() {
    let nl = parse_ok("* t\nC1 a b CMOD\n");
    let e = elem(&nl, "C1");
    assert_kind(e, ElementKind::Capacitor);
    // CMOD is non-numeric; parser combines remaining tokens as String.
    assert!(
        matches!(&e.value, Some(Value::String(s)) if s == "CMOD"),
        "expected String(CMOD), got {:?}",
        e.value
    );
}

/// Inductor with IC= initial condition.
/// ngspice inp2l.c: optional IC= keyword.
#[test]
fn inductor_with_ic() {
    let nl = parse_ok("* t\nL1 a b 10u IC=0.1\n");
    let e = elem(&nl, "L1");
    assert_kind(e, ElementKind::Inductor);
    assert!(
        e.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("IC")),
        "IC param not found; params: {:?}",
        e.params
    );
}

/// Diode with `OFF` positional keyword after model.
/// ngspice inp2d.c: optional `OFF` token. Documents current behaviour
/// (OFF lands as a keyless positional param).
#[test]
fn diode_off() {
    let nl = parse_ok("* t\nD1 a k DMOD OFF\n");
    let e = elem(&nl, "D1");
    assert_kind(e, ElementKind::Diode);
    assert!(matches!(&e.value, Some(Value::String(s)) if s == "DMOD"));
    // OFF is a bare extra positional after the model; lands as keyless param.
    let has_off = e
        .params
        .iter()
        .any(|(_, v)| matches!(v, Value::String(s) if s.eq_ignore_ascii_case("OFF")));
    assert!(has_off, "OFF positional not found; params: {:?}", e.params);
}

/// Diode with IC= initial condition.
#[test]
fn diode_with_ic_param() {
    let nl = parse_ok("* t\nD1 a k DMOD IC=0.7\n");
    let e = elem(&nl, "D1");
    assert_kind(e, ElementKind::Diode);
    assert!(
        e.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("IC")),
        "IC not found; params: {:?}",
        e.params
    );
}

/// BJT with IC= (two values comma-separated). The comma is not a separator
/// in our lexer for params — documents actual parser behaviour.
#[test]
fn bjt_with_ic() {
    // ngspice inp2q.c: IC=Vbe,Vce comma-separated. The tokeniser treats
    // `IC=0.6,0.7` as two tokens after `=` split on comma, or a single
    // token with comma — behaviour depends on lexer.  Assert what we get.
    let nl = parse_ok("* t\nQ1 c b e QMOD IC=0.6,0.7\n");
    let e = elem(&nl, "Q1");
    assert_kind(e, ElementKind::Bjt);
    assert_eq!(e.nodes, ["c", "b", "e"]);
    assert!(matches!(&e.value, Some(Value::String(s)) if s == "QMOD"));
    // IC ends up as a param (possibly with value "0.6,0.7" as String).
    assert!(
        e.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("IC")),
        "IC param not found; params: {:?}",
        e.params
    );
}

/// Current source with `DC … AC …` multi-token spec (mirror of voltage_source_ac_dc).
/// ngspice inp2i.c: same syntax as V.
#[test]
fn current_source_ac_dc() {
    let nl = parse_ok("* t\nI1 a 0 DC 1m AC 0.5\n");
    let e = elem(&nl, "I1");
    assert_kind(e, ElementKind::CurrentSrc);
    assert_eq!(e.nodes, ["a", "0"]);
    let s = match &e.value {
        Some(Value::String(s)) => s.clone(),
        other => panic!("expected String, got {other:?}"),
    };
    assert!(s.contains("DC") && s.contains("AC"), "got: {s}");
}

/// Voltage source with PWL waveform spec — spec preserved as String.
#[test]
fn voltage_source_pwl() {
    let nl = parse_ok("* t\nV1 a 0 PWL ( 0 0 1n 5 2n 5 )\n");
    let e = elem(&nl, "V1");
    assert_kind(e, ElementKind::VoltageSrc);
    assert!(matches!(&e.value, Some(Value::String(s)) if s.contains("PWL")));
}

/// Voltage source with EXP waveform spec — spec preserved as String.
#[test]
fn voltage_source_exp() {
    let nl = parse_ok("* t\nV1 a 0 EXP ( 0 5 0 1n 1u 2u )\n");
    let e = elem(&nl, "V1");
    assert_kind(e, ElementKind::VoltageSrc);
    assert!(matches!(&e.value, Some(Value::String(s)) if s.contains("EXP")));
}

/// Voltage source with SFFM waveform spec — spec preserved as String.
#[test]
fn voltage_source_sffm() {
    let nl = parse_ok("* t\nV1 a 0 SFFM ( 0 1 1k 0.1 100 )\n");
    let e = elem(&nl, "V1");
    assert_kind(e, ElementKind::VoltageSrc);
    assert!(matches!(&e.value, Some(Value::String(s)) if s.contains("SFFM")));
}

// ---------------------------------------------------------------------------
// C. VCCS G coverage
// ---------------------------------------------------------------------------

/// G (VCCS) maps to ElementKind::Vccs with the same 4-node + gain shape
/// as VCVS. ngspice inp2g.c.
#[test]
fn vccs_g_basic() {
    let nl = parse_ok("* t\nG1 out 0 in 0 1m\n");
    let e = elem(&nl, "G1");
    assert_kind(e, ElementKind::Vccs);
    assert_eq!(e.nodes, ["out", "0", "in", "0"]);
    assert_value_number(e, 1e-3);
}

// ---------------------------------------------------------------------------
// AM / TRRANDOM voltage waveforms (ngspice inp2v.c)
// ---------------------------------------------------------------------------

#[test]
fn voltage_source_am() {
    let nl = parse_ok("* t\nV1 a 0 AM(0.5 0.5 100k 1k 0)\n");
    let e = elem(&nl, "V1");
    assert_kind(e, ElementKind::VoltageSrc);
    assert_eq!(e.nodes, ["a", "0"]);
    assert!(matches!(&e.value, Some(Value::String(s)) if s.contains("AM")));
}

#[test]
fn voltage_source_trrandom() {
    let nl = parse_ok("* t\nV1 a 0 TRRANDOM(2 10ms 0 1)\n");
    let e = elem(&nl, "V1");
    assert!(matches!(&e.value, Some(Value::String(s)) if s.contains("TRRANDOM")));
}

#[test]
fn current_source_pwl() {
    let nl = parse_ok("* t\nI1 a 0 PWL(0 0 1n 5m 2n 5m)\n");
    let e = elem(&nl, "I1");
    assert_kind(e, ElementKind::CurrentSrc);
    assert!(matches!(&e.value, Some(Value::String(s)) if s.contains("PWL")));
}

#[test]
fn current_source_sin() {
    let nl = parse_ok("* t\nI1 a 0 SIN(0 1m 1k)\n");
    let e = elem(&nl, "I1");
    assert!(matches!(&e.value, Some(Value::String(s)) if s.contains("SIN")));
}

// ---------------------------------------------------------------------------
// B-source (arbitrary behavioural source, ngspice inp2b.c)
// ---------------------------------------------------------------------------
//
// Our parser has no dedicated handling for `B`; it falls into Other and
// uses the variable-port heuristic. The `V=…`/`I=…` form gets fragmented
// because the tokeniser splits on `=`, `(`, `)`. These tests document that.

#[test]
fn b_source_v_expression_fragmented() {
    let nl = parse_ok("* t\nB1 out 0 V=v(in)+1\n");
    let e = elem(&nl, "B1");
    assert_kind(e, ElementKind::Other);
    let v_param = e.params.iter().find(|(k, _)| k == "V");
    assert!(
        v_param.is_some(),
        "B-source V= should be picked up as param key"
    );
}

#[test]
fn b_source_i_expression_fragmented() {
    let nl = parse_ok("* t\nB1 out 0 I=2*v(ctrl)\n");
    let e = elem(&nl, "B1");
    assert!(e.params.iter().any(|(k, _)| k == "I"));
}

#[test]
#[ignore = "braced expressions {…} fragment through the tokeniser; ngspice handles them via numparam"]
fn b_source_braced_expression() {
    let nl = parse_ok("* t\nB1 out 0 V={v(in)*2}\n");
    let e = elem(&nl, "B1");
    let v = e.params.iter().find(|(k, _)| k == "V").expect("V= param");
    assert!(matches!(&v.1, Value::String(s) if s.contains('{')));
}

// ---------------------------------------------------------------------------
// N MOSFET alt dispatch (ngspice inppas2.c case 'N' → INP2N)
//
// Our parser does not map 'N' to a MOSFET kind; it falls to Other. These
// tests document that conformance gap.
// ---------------------------------------------------------------------------

#[test]
fn n_mosfet_falls_into_other() {
    let nl = parse_ok("* t\nN1 d g s b NMOS L=1u W=10u\n");
    let e = elem(&nl, "N1");
    assert_kind(e, ElementKind::Other);
}

#[test]
fn n_mosfet_designator_and_params_preserved() {
    let nl = parse_ok("* t\nN1 d g s b NMOS L=1u W=10u\n");
    let e = elem(&nl, "N1");
    assert_eq!(e.designator, "N1");
    assert!(e.params.iter().any(|(k, _)| k == "L"));
    assert!(e.params.iter().any(|(k, _)| k == "W"));
}
