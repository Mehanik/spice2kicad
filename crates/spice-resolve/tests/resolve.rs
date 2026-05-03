//! End-to-end tests for `spice_resolve::resolve`.
//!
//! Tests build [`Netlist`] values by hand (the SPICE parser's
//! annotation support is not wired up yet) and resolve against the
//! `kicad-symbols` fixture libraries.

use std::path::PathBuf;
use std::sync::OnceLock;

use kicad_symbols::Library;
use spice_diagnostics::{FileId, Severity};
use spice_parser::ast::{
    Annotation, Axis, Element, ElementKind, Netlist, PinRef, PinmapEntry, Relation,
    SpannedAnnotation, SpannedTag, Subckt, Tag, Value,
};
use spice_resolve::{ElementRole, ResolvedNetlist, resolve};

// ---------------------------------------------------------------------------
// Fixture loading
// ---------------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    // The kicad-symbols crate ships the canonical fixtures used by
    // every downstream test. Reach into it from here so we don't
    // have to keep two copies in sync.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace crates dir")
        .join("kicad-symbols")
        .join("tests")
        .join("fixtures")
}

fn library() -> &'static Library {
    static LIB: OnceLock<Library> = OnceLock::new();
    LIB.get_or_init(|| {
        let device = Library::from_file(fixtures_dir().join("Device.kicad_sym"))
            .expect("parse Device.kicad_sym");
        let sim = Library::from_file(fixtures_dir().join("Simulation_SPICE.kicad_sym"))
            .expect("parse Simulation_SPICE.kicad_sym");
        device.merge(sim)
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn elem(refdes: &str, kind: ElementKind, nodes: &[&str]) -> Element {
    Element::new(
        refdes,
        kind,
        nodes.iter().map(|s| (*s).to_owned()).collect(),
    )
}

fn nl_with(elements: Vec<Element>) -> Netlist {
    Netlist {
        elements,
        ..Netlist::default()
    }
}

fn ok(n: &Netlist) -> ResolvedNetlist {
    resolve(n, library()).expect("resolve should succeed")
}

fn err_codes(n: &Netlist) -> Vec<String> {
    let diags = resolve(n, library()).expect_err("resolve should fail");
    diags.iter().map(|d| d.code.to_string()).collect()
}

fn parse_netlist(source: &str) -> Netlist {
    spice_parser::parse(source, FileId(0))
        .expect("parse should succeed")
        .netlist
}

fn parse_and_resolve(source: &str) -> ResolvedNetlist {
    resolve(&parse_netlist(source), library()).expect("resolve should succeed")
}

fn parse_and_resolve_codes(source: &str) -> Vec<String> {
    resolve(&parse_netlist(source), library())
        .expect_err("expected resolve error")
        .iter()
        .map(|d| d.code.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn resistor_default_resolution() {
    let n = nl_with(vec![elem("R1", ElementKind::Resistor, &["in", "out"])]);
    let r = ok(&n);
    assert_eq!(r.elements.len(), 1);
    let e = &r.elements[0];
    assert_eq!(e.refdes, "R1");
    assert_eq!(e.lib_id, "Device:R");
    assert_eq!(e.pin_mapping, vec!["1".to_owned(), "2".to_owned()]);
    assert_eq!(e.nodes, vec!["in".to_owned(), "out".to_owned()]);
    assert_eq!(e.role, ElementRole::Normal);
    assert_eq!(e.symbol.lib_id, "Device:R");
}

#[test]
fn trailing_symbol_tag_overrides_default() {
    let mut e = elem("R1", ElementKind::Resistor, &["in", "out"]);
    e.tags
        .push(SpannedTag::bare(Tag::Symbol("Device:C".to_owned())));
    let r = ok(&nl_with(vec![e]));
    assert_eq!(r.elements[0].lib_id, "Device:C");
}

#[test]
fn block_symbol_default_with_glob() {
    let n = Netlist {
        elements: vec![
            elem("R10", ElementKind::Resistor, &["a", "b"]),
            elem("R20", ElementKind::Resistor, &["b", "c"]),
            elem("C1", ElementKind::Capacitor, &["c", "d"]),
        ],
        annotations: vec![SpannedAnnotation::bare(Annotation::SymbolDefault {
            lib_id: "Device:R".to_owned(),
            for_glob: "R*".to_owned(),
            pinmap: None,
        })],
        ..Netlist::default()
    };
    let r = ok(&n);
    let by_refdes: std::collections::HashMap<_, _> = r
        .elements
        .iter()
        .map(|e| (e.refdes.as_str(), e.lib_id.as_str()))
        .collect();
    assert_eq!(by_refdes["R10"], "Device:R");
    assert_eq!(by_refdes["R20"], "Device:R");
    assert_eq!(by_refdes["C1"], "Device:C");
}

#[test]
fn later_block_annotation_wins_for_matches() {
    // Two block annotations both match R10. Spec uses
    // last-match-wins (no specificity yet — see resolver doc).
    let n = Netlist {
        elements: vec![
            elem("R10", ElementKind::Resistor, &["a", "b"]),
            elem("R20", ElementKind::Resistor, &["b", "c"]),
        ],
        annotations: vec![
            SpannedAnnotation::bare(Annotation::SymbolDefault {
                lib_id: "Device:R".to_owned(),
                for_glob: "R*".to_owned(),
                pinmap: None,
            }),
            SpannedAnnotation::bare(Annotation::SymbolDefault {
                lib_id: "Device:C".to_owned(),
                for_glob: "R10".to_owned(),
                pinmap: None,
            }),
        ],
        ..Netlist::default()
    };
    let r = ok(&n);
    let by_refdes: std::collections::HashMap<_, _> = r
        .elements
        .iter()
        .map(|e| (e.refdes.as_str(), e.lib_id.as_str()))
        .collect();
    assert_eq!(by_refdes["R10"], "Device:C");
    assert_eq!(by_refdes["R20"], "Device:R");
}

#[test]
fn trailing_tag_beats_block_annotation() {
    let mut r10 = elem("R10", ElementKind::Resistor, &["a", "b"]);
    r10.tags
        .push(SpannedTag::bare(Tag::Symbol("Device:C".to_owned())));
    let n = Netlist {
        elements: vec![r10],
        annotations: vec![SpannedAnnotation::bare(Annotation::SymbolDefault {
            lib_id: "Device:R".to_owned(),
            for_glob: "R*".to_owned(),
            pinmap: None,
        })],
        ..Netlist::default()
    };
    let r = ok(&n);
    assert_eq!(r.elements[0].lib_id, "Device:C");
}

#[test]
fn pinmap_swaps_terminals() {
    let mut e = elem("R1", ElementKind::Resistor, &["a", "b"]);
    e.tags.push(SpannedTag::bare(Tag::Pinmap(vec![
        PinmapEntry {
            spice_index: 1,
            kicad_pin: PinRef::Number("2".to_owned()),
        },
        PinmapEntry {
            spice_index: 2,
            kicad_pin: PinRef::Number("1".to_owned()),
        },
    ])));
    let r = ok(&nl_with(vec![e]));
    assert_eq!(
        r.elements[0].pin_mapping,
        vec!["2".to_owned(), "1".to_owned()]
    );
}

#[test]
fn pinmap_can_reference_pin_by_name() {
    let mut e = elem("Q1", ElementKind::Bjt, &["b", "c", "e"]);
    // Identity, but referenced by names.
    e.tags.push(SpannedTag::bare(Tag::Pinmap(vec![
        PinmapEntry {
            spice_index: 1,
            kicad_pin: PinRef::Name("B".to_owned()),
        },
        PinmapEntry {
            spice_index: 2,
            kicad_pin: PinRef::Name("C".to_owned()),
        },
        PinmapEntry {
            spice_index: 3,
            kicad_pin: PinRef::Name("E".to_owned()),
        },
    ])));
    let r = ok(&nl_with(vec![e]));
    assert_eq!(
        r.elements[0].pin_mapping,
        vec!["1".to_owned(), "2".to_owned(), "3".to_owned()]
    );
}

#[test]
fn pinmap_with_unknown_pin_is_e005() {
    let mut e = elem("R1", ElementKind::Resistor, &["a", "b"]);
    e.tags.push(SpannedTag::bare(Tag::Pinmap(vec![
        PinmapEntry {
            spice_index: 1,
            kicad_pin: PinRef::Number("99".to_owned()),
        },
        PinmapEntry {
            spice_index: 2,
            kicad_pin: PinRef::Number("1".to_owned()),
        },
    ])));
    let codes = err_codes(&nl_with(vec![e]));
    assert!(codes.iter().any(|c| c == "E005"), "got {codes:?}");
}

#[test]
fn pinmap_duplicate_spice_index_is_e005() {
    let mut e = elem("R1", ElementKind::Resistor, &["a", "b"]);
    e.tags.push(SpannedTag::bare(Tag::Pinmap(vec![
        PinmapEntry {
            spice_index: 1,
            kicad_pin: PinRef::Number("1".to_owned()),
        },
        PinmapEntry {
            spice_index: 1,
            kicad_pin: PinRef::Number("2".to_owned()),
        },
    ])));
    let codes = err_codes(&nl_with(vec![e]));
    assert!(codes.iter().any(|c| c == "E005"), "got {codes:?}");
}

#[test]
fn pinmap_duplicate_kicad_pin_is_e005() {
    let mut e = elem("R1", ElementKind::Resistor, &["a", "b"]);
    e.tags.push(SpannedTag::bare(Tag::Pinmap(vec![
        PinmapEntry {
            spice_index: 1,
            kicad_pin: PinRef::Number("1".to_owned()),
        },
        PinmapEntry {
            spice_index: 2,
            kicad_pin: PinRef::Number("1".to_owned()),
        },
    ])));
    let codes = err_codes(&nl_with(vec![e]));
    assert!(codes.iter().any(|c| c == "E005"), "got {codes:?}");
}

#[test]
fn pinmap_out_of_range_index_is_e005() {
    let mut e = elem("R1", ElementKind::Resistor, &["a", "b"]);
    e.tags.push(SpannedTag::bare(Tag::Pinmap(vec![
        PinmapEntry {
            spice_index: 7,
            kicad_pin: PinRef::Number("1".to_owned()),
        },
        PinmapEntry {
            spice_index: 2,
            kicad_pin: PinRef::Number("2".to_owned()),
        },
    ])));
    let codes = err_codes(&nl_with(vec![e]));
    assert!(codes.iter().any(|c| c == "E005"), "got {codes:?}");
}

#[test]
fn pin_count_mismatch_no_pinmap_is_e002() {
    // Resistor with three terminals should fail E002 against Device:R.
    let e = elem("R1", ElementKind::Resistor, &["a", "b", "c"]);
    let codes = err_codes(&nl_with(vec![e]));
    assert!(codes.iter().any(|c| c == "E002"), "got {codes:?}");
}

#[test]
fn unknown_lib_id_is_e003() {
    let mut e = elem("R1", ElementKind::Resistor, &["a", "b"]);
    e.tags.push(SpannedTag::bare(Tag::Symbol(
        "Device:NONEXISTENT".to_owned(),
    )));
    let diags = resolve(&nl_with(vec![e]), library()).expect_err("must fail");
    assert!(
        diags
            .iter()
            .any(|d| d.code == "E003" && d.severity == Severity::Error),
        "got {:?}",
        diags.iter().map(|d| d.code).collect::<Vec<_>>()
    );
}

#[test]
fn power_tag_marks_role() {
    let mut e = elem("V1", ElementKind::VoltageSrc, &["vcc", "0"]);
    e.tags.push(SpannedTag::bare(Tag::Power("vcc".to_owned())));
    let r = ok(&nl_with(vec![e]));
    assert_eq!(r.elements.len(), 1);
    assert_eq!(r.elements[0].role, ElementRole::Power("vcc".to_owned()));
}

#[test]
fn ignore_tag_drops_element() {
    let mut e = elem("V1", ElementKind::VoltageSrc, &["a", "0"]);
    e.tags.push(SpannedTag::bare(Tag::Ignore));
    let r = ok(&nl_with(vec![e]));
    assert!(r.elements.is_empty());
}

#[test]
fn subckt_instance_without_symbol_is_error() {
    let e = elem("X1", ElementKind::Subckt, &["a", "b", "opamp_5532"]);
    let codes = err_codes(&nl_with(vec![e]));
    assert!(codes.iter().any(|c| c == "E003"), "got {codes:?}");
}

#[test]
fn place_tag_passes_through() {
    let mut r1 = elem("R1", ElementKind::Resistor, &["a", "b"]);
    r1.tags.push(SpannedTag::bare(Tag::Place {
        relation: Relation::RightOf,
        anchor: "V1".to_owned(),
    }));
    let v1 = elem("V1", ElementKind::VoltageSrc, &["a", "0"]);
    let r = ok(&nl_with(vec![r1, v1]));
    assert_eq!(r.place.len(), 1);
    assert_eq!(r.place[0].refdes, "R1");
    assert_eq!(r.place[0].relation, Relation::RightOf);
    assert_eq!(r.place[0].anchor, "V1");
}

#[test]
fn align_annotation_passes_through_unvalidated() {
    let n = Netlist {
        elements: vec![elem("R1", ElementKind::Resistor, &["a", "b"])],
        annotations: vec![SpannedAnnotation::bare(Annotation::Align {
            axis: Axis::Horizontal,
            // References R1 (exists) and ZZZ99 (does not exist).
            // The resolver does NOT validate refdes references in
            // align — that's the policy pass's job.
            refdes: vec!["R1".to_owned(), "ZZZ99".to_owned()],
        })],
        ..Netlist::default()
    };
    let r = ok(&n);
    assert_eq!(r.align.len(), 1);
    assert_eq!(r.align[0].axis, Axis::Horizontal);
    assert_eq!(r.align[0].refdes, vec!["R1".to_owned(), "ZZZ99".to_owned()]);
}

#[test]
fn subckt_body_resolves() {
    // Element inside a subckt should be resolved with the subckt's
    // own annotations as the block-symbol scope.
    let n = Netlist {
        subckts: vec![Subckt {
            name: "amp".to_owned(),
            ports: vec!["in".to_owned(), "out".to_owned()],
            params: Vec::new(),
            body: vec![elem("R1", ElementKind::Resistor, &["in", "out"])],
            annotations: Vec::new(),
        }],
        ..Netlist::default()
    };
    let r = ok(&n);
    // Body elements live on the subckt's child hierarchical sheet, not
    // on the top-level element list.
    assert!(r.elements.is_empty());
    assert_eq!(r.subckts.len(), 1);
    assert_eq!(r.subckts[0].elements.len(), 1);
    assert_eq!(r.subckts[0].elements[0].refdes, "R1");
    assert_eq!(r.subckts[0].elements[0].lib_id, "Device:R");
}

#[test]
fn vcvs_default_resolves_to_esource() {
    // E1 with no annotation defaults to Simulation_SPICE:ESOURCE (4-pin VCVS).
    let n = nl_with(vec![elem(
        "E1",
        ElementKind::Vcvs,
        &["out+", "out-", "in+", "in-"],
    )]);
    let r = ok(&n);
    assert_eq!(r.elements.len(), 1);
    assert_eq!(r.elements[0].lib_id, "Simulation_SPICE:ESOURCE");
}

#[test]
fn vccs_default_resolves_to_gsource() {
    // G1 with no annotation defaults to Simulation_SPICE:GSOURCE (4-pin VCCS).
    let n = nl_with(vec![elem(
        "G1",
        ElementKind::Vccs,
        &["out+", "out-", "in+", "in-"],
    )]);
    let r = ok(&n);
    assert_eq!(r.elements.len(), 1);
    assert_eq!(r.elements[0].lib_id, "Simulation_SPICE:GSOURCE");
}

// ---------------------------------------------------------------------------
// End-to-end pipeline tests: raw SPICE source -> parse -> resolve.
// Complements the hand-built-AST tests above by exercising parser + resolver
// together for the controlled-source kinds (E/G/F/H/K).
// ---------------------------------------------------------------------------

#[test]
fn vcvs_pipeline_no_annotation_resolves_to_esource() {
    let src = "* t\nE1 out 0 in 0 1e5\n";
    let r = parse_and_resolve(src);
    assert_eq!(r.elements.len(), 1);
    let e = &r.elements[0];
    assert_eq!(e.refdes, "E1");
    assert_eq!(e.lib_id, "Simulation_SPICE:ESOURCE");
    assert_eq!(e.kind, ElementKind::Vcvs);
    assert_eq!(e.nodes, vec!["out", "0", "in", "0"]);

    // Confirm the parsed AST carried the gain through as a numeric value.
    let nl = parse_netlist(src);
    match nl.elements[0].value {
        Some(Value::Number(n)) => assert!((n - 1e5).abs() < 1e-6),
        ref other => panic!("expected Value::Number(1e5), got {other:?}"),
    }
}

#[test]
fn vccs_pipeline_no_annotation_resolves_to_gsource() {
    let src = "* t\nG1 out 0 in 0 1e-3\n";
    let r = parse_and_resolve(src);
    assert_eq!(r.elements.len(), 1);
    let e = &r.elements[0];
    assert_eq!(e.refdes, "G1");
    assert_eq!(e.lib_id, "Simulation_SPICE:GSOURCE");
    assert_eq!(e.kind, ElementKind::Vccs);
    assert_eq!(e.nodes, vec!["out", "0", "in", "0"]);
}

#[test]
fn vcvs_pipeline_with_explicit_symbol_annotation_overrides_default() {
    // Trailing `;@ symbol=` should win over the kind-based default.
    // Device:R has 2 pins while VCVS supplies 4 nodes, so we expect
    // the override to take effect and the resolver to flag E002
    // (pin-count mismatch) rather than silently reverting to ESOURCE.
    let src = "* t\nE1 out 0 in 0 1e5 ;@ symbol=Device:R\n";
    let codes = parse_and_resolve_codes(src);
    assert!(codes.iter().any(|c| c == "E002"), "got {codes:?}");
}

#[test]
fn cccs_pipeline_no_annotation_yields_e003() {
    let src = "* t\nF1 out 0 V1 100\n";
    let codes = parse_and_resolve_codes(src);
    assert!(codes.iter().any(|c| c == "E003"), "got {codes:?}");
}

#[test]
fn ccvs_pipeline_no_annotation_yields_e003() {
    let src = "* t\nH1 out 0 V1 100\n";
    let codes = parse_and_resolve_codes(src);
    assert!(codes.iter().any(|c| c == "E003"), "got {codes:?}");
}

#[test]
fn k_pipeline_no_annotation_yields_e003() {
    let src = "* t\nK1 L1 L2 0.999\n";
    let codes = parse_and_resolve_codes(src);
    assert!(codes.iter().any(|c| c == "E003"), "got {codes:?}");
}

#[test]
fn cccs_pipeline_with_explicit_symbol_resolves() {
    // F has nodes=[out+, out-]; pair with a 2-pin override so the
    // resolver accepts it. VDC is semantically wrong but pin-shaped right.
    let src = "* t\nF1 out 0 V1 100 ;@ symbol=Simulation_SPICE:VDC\n";
    let r = parse_and_resolve(src);
    assert_eq!(r.elements.len(), 1);
    assert_eq!(r.elements[0].refdes, "F1");
    assert_eq!(r.elements[0].lib_id, "Simulation_SPICE:VDC");
}

#[test]
fn cccs_pipeline_preserves_control_field() {
    // The parser should attach the controlling-source refdes (V1) to
    // Element.control, regardless of any trailing annotation.
    let src = "* t\nF1 out 0 V1 100 ;@ symbol=Simulation_SPICE:VDC\n";
    let nl = parse_netlist(src);
    assert_eq!(nl.elements.len(), 1);
    let e = &nl.elements[0];
    assert_eq!(e.kind, ElementKind::Cccs);
    assert_eq!(e.control.as_deref(), Some("V1"));
    assert_eq!(e.nodes, vec!["out", "0"]);
}

#[test]
fn mutual_k_pipeline_preserves_coupled_field() {
    // K's two operands are inductor refdes refs, not nets — they live
    // in `coupled`, and `nodes` stays empty.
    let src = "* t\nK1 L1 L2 0.999 ;@ symbol=Device:L\n";
    let nl = parse_netlist(src);
    assert_eq!(nl.elements.len(), 1);
    let e = &nl.elements[0];
    assert_eq!(e.kind, ElementKind::MutualInductance);
    assert_eq!(e.coupled, vec!["L1".to_owned(), "L2".to_owned()]);
    assert!(e.nodes.is_empty(), "K nodes should be empty: {:?}", e.nodes);
}

#[test]
fn vcvs_lowercase_e_resolves_same_as_uppercase() {
    // Lowercase refdes prefix should still classify as Vcvs.
    let src = "* t\ne1 out 0 in 0 1e5\n";
    let r = parse_and_resolve(src);
    assert_eq!(r.elements.len(), 1);
    assert_eq!(r.elements[0].kind, ElementKind::Vcvs);
    assert_eq!(r.elements[0].lib_id, "Simulation_SPICE:ESOURCE");
}
