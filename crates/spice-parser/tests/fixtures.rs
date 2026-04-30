//! Integration tests: parse the round-trip fixtures end-to-end and
//! assert the AST shape that downstream crates consume.

mod common;

use std::path::PathBuf;

use common::{expect_tag, fid};
use spice_parser::ast::{Annotation, Axis, ElementKind, PinRef, Relation, Tag, Value};
use spice_parser::parse;

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("spice2kicad/tests/fixtures")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn rc_lowpass() {
    let nl = parse(&fixture("rc_lowpass.cir"), fid())
        .expect("parse")
        .netlist;
    assert!(nl.title.contains("RC low-pass"));

    let refdes: Vec<_> = nl.elements.iter().map(|e| e.designator.as_str()).collect();
    assert_eq!(refdes, ["V1", "R1", "C1"]);

    // Two block-form symbol defaults at top level.
    let symbol_defaults: Vec<&Annotation> = nl
        .annotations
        .iter()
        .map(|a| &a.annotation)
        .filter(|a| matches!(a, Annotation::SymbolDefault { .. }))
        .collect();
    assert_eq!(symbol_defaults.len(), 2);

    // V1 carries `;@ ignore`.
    let v1 = nl.elements.iter().find(|e| e.designator == "V1").unwrap();
    assert_eq!(v1.kind, ElementKind::VoltageSrc);
    assert!(v1.tags.iter().any(|t| matches!(t.tag, Tag::Ignore)));

    // V1 nodes: in, 0; value preserves "AC 1".
    assert_eq!(v1.nodes, ["in", "0"]);
    assert!(matches!(&v1.value, Some(Value::String(s)) if s.contains("AC")));

    // R1 has 1k value.
    let r1 = nl.elements.iter().find(|e| e.designator == "R1").unwrap();
    assert_eq!(r1.nodes, ["in", "out"]);
    if let Some(Value::Number(n)) = &r1.value {
        assert!((n - 1000.0).abs() < 1e-6);
    } else {
        panic!("R1 value not numeric: {:?}", r1.value);
    }

    // .ac directive preserved.
    assert!(
        nl.directives
            .iter()
            .any(|d| d.name.eq_ignore_ascii_case("ac"))
    );
}

#[test]
fn common_emitter_subckt_and_align_and_power() {
    let nl = parse(&fixture("common_emitter.cir"), fid())
        .expect("parse")
        .netlist;

    // Three top-level symbol defaults.
    assert_eq!(
        nl.annotations
            .iter()
            .filter(|a| matches!(a.annotation, Annotation::SymbolDefault { .. }))
            .count(),
        3
    );

    // VCC has power tag.
    let vcc = nl.elements.iter().find(|e| e.designator == "VCC").unwrap();
    assert!(
        vcc.tags
            .iter()
            .any(|t| matches!(&t.tag, Tag::Power(rail) if rail == "+12V"))
    );

    // Q1 is a BJT; model name is the value.
    let q1 = nl.elements.iter().find(|e| e.designator == "Q1").unwrap();
    assert_eq!(q1.kind, ElementKind::Bjt);
    assert_eq!(q1.nodes, ["c", "b", "e"]);
    assert!(matches!(&q1.value, Some(Value::String(s)) if s == "QGENERIC"));

    // RL has ignore tag.
    let rl = nl.elements.iter().find(|e| e.designator == "RL").unwrap();
    assert!(rl.tags.iter().any(|t| matches!(t.tag, Tag::Ignore)));

    // Model captured.
    let m = nl.models.iter().find(|m| m.name == "QGENERIC").unwrap();
    assert_eq!(m.model_type, "NPN");
    let bf = m.params.iter().find(|(k, _)| k == "BF").unwrap();
    assert!(matches!(bf.1, Value::Number(n) if (n - 200.0).abs() < 1e-6));
}

#[test]
fn diff_pair_align_and_place() {
    let nl = parse(&fixture("diff_pair.cir"), fid())
        .expect("parse")
        .netlist;

    // Two align directives at top level, both horizontal.
    let aligns: Vec<_> = nl
        .annotations
        .iter()
        .filter_map(|a| match &a.annotation {
            Annotation::Align { axis, refdes } => Some((axis, refdes.clone())),
            Annotation::SymbolDefault { .. } => None,
        })
        .collect();
    assert_eq!(aligns.len(), 2);
    assert!(aligns.iter().all(|(ax, _)| **ax == Axis::Horizontal));

    // RC2 carries place=right-of RC1.
    let rc2 = nl.elements.iter().find(|e| e.designator == "RC2").unwrap();
    let place_tag = expect_tag(rc2, |t| match t {
        Tag::Place { relation, anchor } => Some((*relation, anchor.clone())),
        _ => None,
    });
    assert_eq!(place_tag, (Relation::RightOf, "RC1".to_owned()));
}

#[test]
fn opamp_subckt_definition() {
    let nl = parse(&fixture("opamp_inverting.cir"), fid())
        .expect("parse")
        .netlist;

    let opamp = nl
        .subckts
        .iter()
        .find(|s| s.name == "OPAMP")
        .expect("OPAMP subckt");
    assert_eq!(opamp.ports, ["inp", "inn", "out", "vcc", "vee"]);
    // Body has E1 (a VCVS — kind 'Other' since 'E' isn't placeable in our enum).
    assert_eq!(opamp.body.len(), 1);
    assert_eq!(opamp.body[0].designator, "E1");

    // X1 instance: nodes are everything before OPAMP, value = OPAMP.
    let x1 = nl.elements.iter().find(|e| e.designator == "X1").unwrap();
    assert_eq!(x1.kind, ElementKind::Subckt);
    assert_eq!(x1.nodes, ["0", "inv", "out", "vcc", "vee"]);
    assert!(matches!(&x1.value, Some(Value::String(s)) if s == "OPAMP"));
}

#[test]
fn multivibrator_parses_cleanly() {
    let nl = parse(&fixture("multivibrator.cir"), fid())
        .expect("parse")
        .netlist;
    assert!(nl.elements.iter().any(|e| e.designator == "Q1"));
    assert!(nl.elements.iter().any(|e| e.designator == "Q2"));
    let q2 = nl.elements.iter().find(|e| e.designator == "Q2").unwrap();
    assert!(
        q2.tags
            .iter()
            .any(|t| matches!(&t.tag, Tag::Place { anchor, .. } if anchor == "Q1"))
    );
}

#[test]
fn pinmap_tag_parses() {
    let src = "* t\nD1 a k DMOD ;@ pinmap=1:A,2:K\n";
    let nl = parse(src, fid()).expect("parse").netlist;
    let d1 = &nl.elements[0];
    let pm = expect_tag(d1, |t| match t {
        Tag::Pinmap(v) => Some(v.clone()),
        _ => None,
    });
    assert_eq!(pm.len(), 2);
    assert_eq!(pm[0].spice_index, 1);
    assert!(matches!(&pm[0].kicad_pin, PinRef::Name(s) if s == "A"));
    assert_eq!(pm[1].spice_index, 2);
    assert!(matches!(&pm[1].kicad_pin, PinRef::Name(s) if s == "K"));
}

#[test]
fn mosfet_continuation() {
    let src = "* t\nM1 d g s b NMOS L=1u\n+ W=10u\n";
    let nl = parse(src, fid()).expect("parse").netlist;
    let m1 = &nl.elements[0];
    assert_eq!(m1.kind, ElementKind::Mosfet);
    assert_eq!(m1.nodes, ["d", "g", "s", "b"]);
    assert!(matches!(&m1.value, Some(Value::String(s)) if s == "NMOS"));
    let by_key: std::collections::HashMap<_, _> =
        m1.params.iter().map(|(k, v)| (k.as_str(), v)).collect();
    assert!(by_key.contains_key("L"));
    assert!(by_key.contains_key("W"));
}
