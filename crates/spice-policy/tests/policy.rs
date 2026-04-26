//! Unit + property tests for the policy / conflict-check pass.

use kicad_symbols::Symbol;
use proptest::prelude::*;
use spice_diagnostics::Severity;
use spice_resolve::{
    AlignSpec, Axis, ElementKind, ElementRole, PlaceSpec, Relation, ResolvedElement,
    ResolvedNetlist,
};

use spice_policy::{CheckedNetlist, check};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_element(refdes: &str) -> ResolvedElement {
    ResolvedElement {
        refdes: refdes.to_owned(),
        kind: ElementKind::Resistor,
        lib_id: "Device:R".to_owned(),
        symbol: Symbol {
            lib_id: "Device:R".to_owned(),
            name: "R".to_owned(),
            pins: Vec::new(),
        },
        pin_mapping: Vec::new(),
        nodes: Vec::new(),
        value: None,
        role: ElementRole::Normal,
    }
}

fn mk_resolved(
    elements: &[&str],
    align: &[(Axis, &[&str])],
    place: &[(&str, Relation, &str)],
) -> ResolvedNetlist {
    ResolvedNetlist {
        elements: elements.iter().map(|r| make_element(r)).collect(),
        align: align
            .iter()
            .map(|(axis, refs)| AlignSpec {
                axis: *axis,
                refdes: refs.iter().map(|s| (*s).to_owned()).collect(),
                span: None,
            })
            .collect(),
        place: place
            .iter()
            .map(|(refdes, rel, anchor)| PlaceSpec {
                refdes: (*refdes).to_owned(),
                relation: *rel,
                anchor: (*anchor).to_owned(),
                span: None,
            })
            .collect(),
        subckts: vec![],
    }
}

fn codes_of(diags: &[spice_diagnostics::Diagnostic]) -> Vec<&'static str> {
    diags.iter().map(|d| d.code).collect()
}

// ---------------------------------------------------------------------------
// Case tests
// ---------------------------------------------------------------------------

#[test]
fn all_clean_yields_ok_with_no_warnings() {
    let n = mk_resolved(
        &["R1", "R2", "R3"],
        &[(Axis::Horizontal, &["R1", "R2"])],
        &[("R3", Relation::RightOf, "R1")],
    );
    let (out, warns) = check(n).expect("clean input");
    assert_eq!(out.elements.len(), 3);
    assert_eq!(out.align.len(), 1);
    assert_eq!(out.place.len(), 1);
    assert!(warns.is_empty(), "got: {:?}", codes_of(&warns));
}

#[test]
fn e001_align_unknown_refdes() {
    let n = mk_resolved(&["R1", "R2"], &[(Axis::Horizontal, &["R1", "R99"])], &[]);
    let diags = check(n).expect_err("fatal");
    assert!(codes_of(&diags).contains(&"E001"));
}

#[test]
fn e001_place_unknown_refdes() {
    let n = mk_resolved(&["R1"], &[], &[("R99", Relation::RightOf, "R1")]);
    let diags = check(n).expect_err("fatal");
    assert!(codes_of(&diags).contains(&"E001"));
}

#[test]
fn e001_place_unknown_anchor() {
    let n = mk_resolved(&["R1"], &[], &[("R1", Relation::RightOf, "R99")]);
    let diags = check(n).expect_err("fatal");
    assert!(codes_of(&diags).contains(&"E001"));
}

#[test]
fn e001_collects_multiple() {
    let n = mk_resolved(
        &["R1"],
        &[(Axis::Horizontal, &["A", "B"])],
        &[("X", Relation::RightOf, "Y")],
    );
    let diags = check(n).expect_err("fatal");
    let e001s: Vec<_> = diags.iter().filter(|d| d.code == "E001").collect();
    // 2 align unknowns + 1 place refdes + 1 place anchor = 4.
    assert_eq!(e001s.len(), 4, "got: {:?}", codes_of(&diags));
}

#[test]
fn w102_single_member_cluster() {
    let n = mk_resolved(&["R1", "R2"], &[(Axis::Horizontal, &["R1"])], &[]);
    let (out, warns) = check(n).expect("ok");
    assert!(out.align.is_empty());
    assert_eq!(codes_of(&warns), vec!["W102"]);
}

#[test]
fn w102_duplicates_collapse_to_single() {
    let n = mk_resolved(
        &["R1", "R2"],
        &[(Axis::Horizontal, &["R1", "R1", "R1"])],
        &[],
    );
    let (out, warns) = check(n).expect("ok");
    assert!(out.align.is_empty());
    assert_eq!(codes_of(&warns), vec!["W102"]);
}

#[test]
fn w104_place_on_align_fixed_element() {
    let n = mk_resolved(
        &["R1", "R2", "R3"],
        &[(Axis::Horizontal, &["R1", "R2"])],
        &[("R1", Relation::RightOf, "R3")],
    );
    let (out, warns) = check(n).expect("ok");
    assert!(out.place.is_empty());
    // Element survives.
    assert!(out.elements.iter().any(|e| e.refdes == "R1"));
    assert_eq!(codes_of(&warns), vec!["W104"]);
}

#[test]
fn w101_duplicate_place_keeps_first() {
    let n = mk_resolved(
        &["R1", "R2", "R3"],
        &[],
        &[
            ("R1", Relation::RightOf, "R2"),
            ("R1", Relation::Above, "R3"),
        ],
    );
    let (out, warns) = check(n).expect("ok");
    assert_eq!(out.place.len(), 1);
    assert_eq!(out.place[0].relation, Relation::RightOf);
    assert_eq!(out.place[0].anchor, "R2");
    assert_eq!(codes_of(&warns), vec!["W101"]);
}

#[test]
fn w104_alone_when_place_overlaps_align() {
    // Element is align-fixed AND has *one* place — only W104 fires
    // (no spurious W101 from a single entry).
    let n = mk_resolved(
        &["R1", "R2", "R3"],
        &[(Axis::Horizontal, &["R1", "R2"])],
        &[("R1", Relation::RightOf, "R3")],
    );
    let (_, warns) = check(n).expect("ok");
    let codes = codes_of(&warns);
    assert!(codes.contains(&"W104"));
    assert!(!codes.contains(&"W101"));
}

#[test]
fn e006_two_cycle_same_axis() {
    let n = mk_resolved(
        &["A", "B"],
        &[],
        &[("A", Relation::RightOf, "B"), ("B", Relation::RightOf, "A")],
    );
    let diags = check(n).expect_err("fatal");
    assert!(codes_of(&diags).contains(&"E006"));
}

#[test]
fn e006_three_cycle_same_axis() {
    let n = mk_resolved(
        &["A", "B", "C"],
        &[],
        &[
            ("A", Relation::RightOf, "B"),
            ("B", Relation::RightOf, "C"),
            ("C", Relation::RightOf, "A"),
        ],
    );
    let diags = check(n).expect_err("fatal");
    let e006s: Vec<_> = diags.iter().filter(|d| d.code == "E006").collect();
    assert_eq!(e006s.len(), 1);
}

#[test]
fn cross_axis_loop_is_not_a_cycle() {
    // A right-of B, B above A — different axes.
    let n = mk_resolved(
        &["A", "B"],
        &[],
        &[("A", Relation::RightOf, "B"), ("B", Relation::Above, "A")],
    );
    let (_, warns) = check(n).expect("ok");
    assert!(warns.is_empty());
}

#[test]
fn e006_disjoint_cycles_each_reported() {
    let n = mk_resolved(
        &["A", "B", "C", "D"],
        &[],
        &[
            // X-axis cycle A↔B
            ("A", Relation::RightOf, "B"),
            ("B", Relation::RightOf, "A"),
            // Y-axis cycle C↔D
            ("C", Relation::Above, "D"),
            ("D", Relation::Above, "C"),
        ],
    );
    let diags = check(n).expect_err("fatal");
    let e006s: Vec<_> = diags.iter().filter(|d| d.code == "E006").collect();
    assert_eq!(e006s.len(), 2);
}

#[test]
fn errors_carry_warnings_too() {
    // R1 align-fixed; another place creates a cycle. We should see
    // BOTH the W104 (warning) and the E006 (error) in the failure.
    let n = mk_resolved(
        &["R1", "R2", "A", "B"],
        &[(Axis::Horizontal, &["R1", "R2"])],
        &[
            ("R1", Relation::RightOf, "R2"), // dropped W104
            ("A", Relation::RightOf, "B"),
            ("B", Relation::RightOf, "A"),
        ],
    );
    let diags = check(n).expect_err("fatal");
    let codes = codes_of(&diags);
    assert!(codes.contains(&"W104"), "{codes:?}");
    assert!(codes.contains(&"E006"), "{codes:?}");
}

#[test]
fn idempotence_after_cleanup() {
    let n = mk_resolved(
        &["R1", "R2", "R3"],
        &[(Axis::Horizontal, &["R1", "R2"])],
        &[("R3", Relation::RightOf, "R1")],
    );
    let (out, _) = check(n).expect("ok");
    let again = ResolvedNetlist {
        elements: out.elements.clone(),
        align: out.align.clone(),
        place: out.place.clone(),
        subckts: out.subckts.clone(),
    };
    let (out2, warns) = check(again).expect("idempotent");
    assert!(warns.is_empty());
    assert_eq!(out2.elements.len(), 3);
    assert_eq!(out2.align.len(), 1);
    assert_eq!(out2.place.len(), 1);
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

/// Strategy: pick `n` element refdeses (R0..Rn), then build:
///   - some `align` clusters whose members are valid refdeses,
///   - some `place` directives whose endpoints are valid refdeses
///     and (importantly) acyclic by construction.
fn refdeses(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("R{i}")).collect()
}

/// Build an acyclic `place` set by construction: every edge points
/// from a higher-indexed refdes to a lower-indexed one *within an
/// axis*. Cross-axis edges are unrestricted.
fn arb_acyclic_input() -> impl Strategy<Value = ResolvedNetlist> {
    (3usize..=6usize).prop_flat_map(|n| {
        let refs = refdeses(n);
        // Up to n*2 place edges, each acyclic on its axis by index.
        let place_strat =
            proptest::collection::vec((0usize..n, 0usize..n, any::<u8>()), 0..=(n * 2));
        let align_strat = proptest::collection::vec(
            (any::<bool>(), proptest::collection::vec(0usize..n, 0..=n)),
            0..=2,
        );
        (Just(refs), place_strat, align_strat).prop_map(|(refs, places, aligns)| {
            let mut place = Vec::new();
            for (a, b, kind) in places {
                if a == b {
                    continue;
                }
                let (lo, hi) = if a < b { (a, b) } else { (b, a) };
                // hi -> lo: keep the graph DAG within the chosen axis.
                let rel = match kind % 4 {
                    0 => Relation::RightOf,
                    1 => Relation::LeftOf,
                    2 => Relation::Above,
                    _ => Relation::Below,
                };
                place.push(PlaceSpec {
                    refdes: refs[hi].clone(),
                    relation: rel,
                    anchor: refs[lo].clone(),
                    span: None,
                });
            }
            let align = aligns
                .into_iter()
                .map(|(horiz, members)| AlignSpec {
                    axis: if horiz {
                        Axis::Horizontal
                    } else {
                        Axis::Vertical
                    },
                    refdes: members.into_iter().map(|i| refs[i].clone()).collect(),
                    span: None,
                })
                .collect();
            ResolvedNetlist {
                elements: refs.iter().map(|r| make_element(r)).collect(),
                align,
                place,
                subckts: vec![],
            }
        })
    })
}

/// Build an input that *guarantees* at least one X-axis cycle by
/// inserting a cycle pair, on top of arbitrary edges.
fn arb_input_with_x_cycle() -> impl Strategy<Value = ResolvedNetlist> {
    arb_acyclic_input().prop_map(|mut n| {
        // Use the first two refdeses for the cycle; strip them from
        // any align cluster so W104 doesn't quietly absorb the cycle
        // edges before E006 detection runs.
        let a = n.elements[0].refdes.clone();
        let b = n.elements[1].refdes.clone();
        for cluster in &mut n.align {
            cluster.refdes.retain(|r| r != &a && r != &b);
        }
        // Drop any pre-existing place edges touching A or B so we
        // know exactly which edges form the cycle.
        n.place.retain(|p| p.refdes != a && p.refdes != b);
        n.place.push(PlaceSpec {
            refdes: a.clone(),
            relation: Relation::RightOf,
            anchor: b.clone(),
            span: None,
        });
        n.place.push(PlaceSpec {
            refdes: b,
            relation: Relation::RightOf,
            anchor: a,
            span: None,
        });
        n
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn acyclic_inputs_check_ok(input in arb_acyclic_input()) {
        let result = check(input);
        prop_assert!(result.is_ok(), "expected Ok, got {:?}", result.err().map(|d| d.iter().map(|x| x.code).collect::<Vec<_>>()));
    }

    #[test]
    fn cyclic_inputs_emit_e006(input in arb_input_with_x_cycle()) {
        let diags = match check(input) {
            Ok((_, _)) => return Err(TestCaseError::fail("expected Err")),
            Err(d) => d,
        };
        prop_assert!(
            diags.iter().any(|d| d.code == "E006" && d.severity == Severity::Error),
            "expected at least one E006"
        );
    }

    #[test]
    fn idempotent(input in arb_acyclic_input()) {
        let (out, _) = check(input).expect("acyclic ok");
        let again = ResolvedNetlist {
            elements: out.elements,
            align: out.align,
            place: out.place,
            subckts: out.subckts,
        };
        let (_, warns) = check(again).expect("re-check ok");
        prop_assert!(warns.is_empty(), "stray warnings on re-check: {:?}", warns.iter().map(|w| w.code).collect::<Vec<_>>());
    }
}

// Sanity: ensure CheckedNetlist is usable via re-export.
fn _type_used(_: CheckedNetlist) {}
