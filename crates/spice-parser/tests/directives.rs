//! Exhaustive tests for SPICE directive (`.foo`) parsing.
//!
//! Ground truth: ngspice `src/spicelib/parser/inp2dot.c` (directive switch)
//! and `src/frontend/inpcom.c` (`.subckt`/`.include`/`.lib` pre-processing).
//! All sources require a title line as line 1 per SPICE convention.

mod common;

use common::{has_annotation, parse_ok};
use spice_parser::ast::{Annotation, Axis};

// ─── .subckt ─────────────────────────────────────────────────────────────────

#[test]
fn subckt_basic() {
    let src = "* t\n.subckt MYBLOCK a b c\nR1 a b 1k\n.ends\n";
    let nl = parse_ok(src);
    let sub = nl
        .subckts
        .iter()
        .find(|s| s.name == "MYBLOCK")
        .expect("MYBLOCK");
    assert_eq!(sub.ports, ["a", "b", "c"]);
    assert_eq!(sub.body[0].designator, "R1");
}

#[test]
fn subckt_ports_with_kv_params() {
    // ngspice accepts `.subckt NAME ports KEY=val` — params go into `sub.params`.
    let src = "* t\n.subckt MYBLOCK a b c PARAM1=10\n.ends\n";
    let nl = parse_ok(src);
    let sub = nl
        .subckts
        .iter()
        .find(|s| s.name == "MYBLOCK")
        .expect("MYBLOCK");
    assert_eq!(sub.ports, ["a", "b", "c"]);
    assert!(!sub.params.is_empty(), "expected params");
    assert!(sub.params.iter().any(|(k, _)| k == "PARAM1"));
}

#[test]
fn subckt_params_keyword() {
    // ngspice also accepts `.subckt NAME ports params: KEY=val`.
    // The `params:` token currently ends up in the ports list.
    let src = "* t\n.subckt MYBLOCK a b params: VCC=5\n.ends\n";
    let nl = parse_ok(src);
    let sub = nl
        .subckts
        .iter()
        .find(|s| s.name == "MYBLOCK")
        .expect("MYBLOCK");
    assert_eq!(sub.ports, ["a", "b"]);
    assert!(sub.params.iter().any(|(k, _)| k == "VCC"));
}

#[test]
fn subckt_nested() {
    // Both outer and inner subckt must end up in nl.subckts.
    // ngspice flattens nested subckt definitions to the top level.
    let src = "* t\n.subckt OUTER a b\n.subckt INNER x y\nR1 x y 1k\n.ends\n.ends\n";
    let nl = parse_ok(src);
    assert!(
        nl.subckts.iter().any(|s| s.name == "OUTER"),
        "OUTER missing"
    );
    assert!(
        nl.subckts.iter().any(|s| s.name == "INNER"),
        "INNER missing"
    );
}

#[test]
fn subckt_unterminated_yields_warning_not_error() {
    // W900: missing `.ends`; parser returns Ok, subckt still lands.
    let src = "* t\n.subckt NEVERENDS a b\nR1 a b 1k\n";
    let nl = parse_ok(src);
    assert!(nl.subckts.iter().any(|s| s.name == "NEVERENDS"));
}

// ─── .model ──────────────────────────────────────────────────────────────────

#[test]
fn model_npn_parenless() {
    let src = "* t\n.model QM NPN BF=200 IS=1e-15\n";
    let nl = parse_ok(src);
    let m = nl.models.iter().find(|m| m.name == "QM").expect("QM");
    assert_eq!(m.model_type, "NPN");
    assert!(m.params.iter().any(|(k, _)| k == "BF"));
    assert!(m.params.iter().any(|(k, _)| k == "IS"));
}

#[test]
fn model_npn_paren_wrapped() {
    let src = "* t\n.model QM NPN (BF=200 IS=1e-15)\n";
    let nl = parse_ok(src);
    let m = nl.models.iter().find(|m| m.name == "QM").expect("QM");
    assert_eq!(m.model_type, "NPN");
    assert!(m.params.iter().any(|(k, _)| k == "BF"));
    assert!(m.params.iter().any(|(k, _)| k == "IS"));
}

#[test]
fn model_continuation() {
    // Multi-line via `+` continuation.
    let src = "* t\n.model QM NPN (BF=200\n+ IS=1e-15)\n";
    let nl = parse_ok(src);
    let m = nl.models.iter().find(|m| m.name == "QM").expect("QM");
    assert!(m.params.iter().any(|(k, _)| k == "BF"));
    assert!(m.params.iter().any(|(k, _)| k == "IS"));
}

#[test]
fn model_case_insensitive_name() {
    // `.MODEL` and `.Model` both result in a model entry.
    for src in ["* t\n.MODEL QM NPN BF=200\n", "* t\n.Model QM NPN BF=200\n"] {
        let nl = parse_ok(src);
        assert!(
            nl.models.iter().any(|m| m.name == "QM"),
            "QM missing for: {src}"
        );
    }
}

// ─── .include / .lib ─────────────────────────────────────────────────────────

#[test]
fn include_preserved() {
    // Parser does not follow the include; it preserves it as a directive.
    let src = "* t\n.include \"path/to/file.cir\"\n";
    let nl = parse_ok(src);
    let d = nl
        .directives
        .iter()
        .find(|d| d.name.eq_ignore_ascii_case("include"))
        .expect("include directive");
    // Args must contain the quoted path token.
    assert!(
        d.args.iter().any(|a| a.contains("path/to/file.cir")),
        "path not in args: {:?}",
        d.args
    );
}

#[test]
fn lib_preserved() {
    let src = "* t\n.lib \"models.lib\" tt\n";
    let nl = parse_ok(src);
    let d = nl
        .directives
        .iter()
        .find(|d| d.name.eq_ignore_ascii_case("lib"))
        .expect("lib directive");
    assert!(d.args.iter().any(|a| a.contains("models.lib")));
    assert!(d.args.iter().any(|a| a == "tt"));
}

// ─── Simulation / analysis directives ────────────────────────────────────────

#[test]
fn param_preserved() {
    let src = "* t\n.param VCC=12\n";
    let nl = parse_ok(src);
    assert!(
        nl.directives
            .iter()
            .any(|d| d.name.eq_ignore_ascii_case("param"))
    );
}

#[test]
fn global_preserved() {
    let src = "* t\n.global VCC GND\n";
    let nl = parse_ok(src);
    let d = nl
        .directives
        .iter()
        .find(|d| d.name.eq_ignore_ascii_case("global"))
        .expect("global");
    assert!(d.args.iter().any(|a| a == "VCC"));
    assert!(d.args.iter().any(|a| a == "GND"));
}

#[test]
fn tran_preserved() {
    let src = "* t\n.tran 1n 100n\n";
    let nl = parse_ok(src);
    assert!(
        nl.directives
            .iter()
            .any(|d| d.name.eq_ignore_ascii_case("tran"))
    );
}

#[test]
fn ac_preserved() {
    let src = "* t\n.ac dec 10 1 1Meg\n";
    let nl = parse_ok(src);
    let d = nl
        .directives
        .iter()
        .find(|d| d.name.eq_ignore_ascii_case("ac"))
        .expect("ac");
    assert!(d.args.iter().any(|a| a == "dec"));
}

#[test]
fn dc_preserved() {
    let src = "* t\n.dc V1 0 5 0.1\n";
    let nl = parse_ok(src);
    assert!(
        nl.directives
            .iter()
            .any(|d| d.name.eq_ignore_ascii_case("dc"))
    );
}

#[test]
fn op_preserved() {
    let src = "* t\n.op\n";
    let nl = parse_ok(src);
    assert!(
        nl.directives
            .iter()
            .any(|d| d.name.eq_ignore_ascii_case("op"))
    );
}

#[test]
fn print_preserved() {
    let src = "* t\n.print tran v(out)\n";
    let nl = parse_ok(src);
    assert!(
        nl.directives
            .iter()
            .any(|d| d.name.eq_ignore_ascii_case("print"))
    );
}

#[test]
fn ic_preserved() {
    let src = "* t\n.ic v(a)=0\n";
    let nl = parse_ok(src);
    assert!(
        nl.directives
            .iter()
            .any(|d| d.name.eq_ignore_ascii_case("ic"))
    );
}

#[test]
fn measure_preserved() {
    let src = "* t\n.measure tran tdelay TRIG v(out) VAL=0.5\n";
    let nl = parse_ok(src);
    assert!(
        nl.directives
            .iter()
            .any(|d| d.name.eq_ignore_ascii_case("measure"))
    );
}

#[test]
fn option_preserved() {
    let src = "* t\n.option noacct\n";
    let nl = parse_ok(src);
    assert!(
        nl.directives
            .iter()
            .any(|d| d.name.eq_ignore_ascii_case("option"))
    );
}

#[test]
fn end_does_not_crash() {
    // `.end` stops the deck; subsequent tokens may be ignored. Must not panic.
    let src = "* t\nR1 a b 1k\n.end\nR2 c d 2k\n";
    // R1 before `.end` should parse; R2 after is irrelevant.
    // The parser currently keeps iterating but `.end` is a no-op match arm.
    let nl = parse_ok(src);
    // At minimum R1 must be present.
    assert!(nl.elements.iter().any(|e| e.designator == "R1"));
}

// ─── Unknown / generic directives ────────────────────────────────────────────

#[test]
fn unknown_directive_preserved() {
    // Unrecognised `.foo` should land in `nl.directives` as-is, not error.
    let src = "* t\n.frobnitz alpha beta\n";
    let nl = parse_ok(src);
    let d = nl
        .directives
        .iter()
        .find(|d| d.name.eq_ignore_ascii_case("frobnitz"))
        .expect("frobnitz directive");
    assert!(d.args.iter().any(|a| a == "alpha"));
}

// ─── Case-insensitive directive names ────────────────────────────────────────

#[test]
fn directive_names_case_insensitive() {
    for src in [
        "* t\n.SUBCKT BLK a b\n.ENDS\n",
        "* t\n.subckt BLK a b\n.ends\n",
        "* t\n.SubCkt BLK a b\n.Ends\n",
    ] {
        let nl = parse_ok(src);
        assert!(
            nl.subckts.iter().any(|s| s.name == "BLK"),
            "BLK missing for: {src}"
        );
    }
}

// ─── Block annotations ───────────────────────────────────────────────────────

#[test]
fn block_annotation_symbol_default() {
    let src = "* t\n*@symbol Device:R_US for=R*\n";
    let nl = parse_ok(src);
    assert!(
        has_annotation(&nl, |a| matches!(
            a,
            Annotation::SymbolDefault { lib_id, for_glob, .. }
            if lib_id == "Device:R_US" && for_glob == "R*"
        )),
        "SymbolDefault not found"
    );
}

#[test]
fn block_annotation_align_horizontal() {
    let src = "* t\n*@align horizontal R1 R2\n";
    let nl = parse_ok(src);
    assert!(
        has_annotation(&nl, |a| matches!(
            a,
            Annotation::Align { axis: Axis::Horizontal, refdes }
            if refdes.contains(&"R1".to_owned()) && refdes.contains(&"R2".to_owned())
        )),
        "Align not found"
    );
}

#[test]
fn block_annotation_inside_subckt_lands_in_subckt() {
    let src = "* t\n.subckt MYBLOCK a b\n*@align vertical R1 R2\n.ends\n";
    let nl = parse_ok(src);
    // Annotation must be in the subckt, not at top level.
    assert!(
        nl.annotations.is_empty(),
        "top-level annotations must be empty"
    );
    let sub = nl
        .subckts
        .iter()
        .find(|s| s.name == "MYBLOCK")
        .expect("MYBLOCK");
    assert!(
        sub.annotations.iter().any(|a| matches!(
            &a.annotation,
            Annotation::Align {
                axis: Axis::Vertical,
                ..
            }
        )),
        "subckt annotation not found"
    );
}

// ---------------------------------------------------------------------------
// Less-common directives (ngspice inp2dot.c)
//
// All preserved as generic `Directive` entries; we don't interpret them.
// ---------------------------------------------------------------------------

fn has_dir(nl: &spice_parser::Netlist, name: &str) -> bool {
    nl.directives
        .iter()
        .any(|d| d.name.eq_ignore_ascii_case(name))
}

#[test]
fn probe_preserved() {
    let nl = parse_ok("* t\n.probe v(out)\n");
    assert!(has_dir(&nl, "probe"));
}

#[test]
fn nodeset_preserved() {
    let nl = parse_ok("* t\n.nodeset v(a)=0.7 v(b)=0.3\n");
    assert!(has_dir(&nl, "nodeset"));
}

#[test]
fn save_preserved() {
    let nl = parse_ok("* t\n.save v(out) i(v1)\n");
    assert!(has_dir(&nl, "save"));
}

#[test]
fn func_preserved() {
    let nl = parse_ok("* t\n.func sq(x) {x*x}\n");
    assert!(has_dir(&nl, "func"));
}

#[test]
fn temp_preserved() {
    let nl = parse_ok("* t\n.temp 27 50 75\n");
    assert!(has_dir(&nl, "temp"));
}

// .if/.endif: ngspice does conditional preprocessing; our parser does not.
// We drop the entire block (both branches) at lex time and emit W911 once
// per top-level `.if`. Updated by P2B (F15) — previously both branches
// survived as elements with conflicting refdeses.

#[test]
fn if_endif_preserved_with_body() {
    let outcome = common::parse_with_diags("* t\n.if (vcc>5)\nR1 a b 1k\n.endif\n");
    let nl = &outcome.netlist;
    assert!(!has_dir(nl, "if"));
    assert!(!has_dir(nl, "endif"));
    assert!(nl.elements.iter().all(|e| e.designator != "R1"));
    assert!(outcome.diagnostics.iter().any(|d| d.code == "W911"));
}

#[test]
fn if_else_endif_preserves_both_branches() {
    let outcome =
        common::parse_with_diags("* t\n.if (vcc>5)\nR1 a b 1k\n.else\nR2 a b 2k\n.endif\n");
    let nl = &outcome.netlist;
    assert!(!has_dir(nl, "if"));
    assert!(!has_dir(nl, "else"));
    assert!(!has_dir(nl, "endif"));
    assert!(nl.elements.iter().all(|e| e.designator != "R1"));
    assert!(nl.elements.iter().all(|e| e.designator != "R2"));
    assert!(outcome.diagnostics.iter().any(|d| d.code == "W911"));
}
