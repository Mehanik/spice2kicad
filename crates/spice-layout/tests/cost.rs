//! Stage-2 cost-function tests: case tests + properties.

mod common;

use common::{fixture_library, make_r, mk_resolved};
use kicad_symbols::Orientation;
use proptest::prelude::*;
use spice_layout::cost::{CostWeights, breakdown, total};
use spice_layout::{GridPoint, PlacedElement, Placement, place};
use spice_policy::{CheckedNetlist, check};
use spice_resolve::{
    AlignSpec, Axis, ElementKind, ElementRole, PlaceSpec, Relation, ResolvedElement,
    ResolvedNetlist, SubcktPorts,
};

const STEP_MM: f64 = 1.27;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a `ResolvedElement` for `R<n>` with caller-chosen node names.
fn make_r_with_nodes(refdes: &str, nodes: &[&str]) -> ResolvedElement {
    let mut e = make_r(refdes);
    e.nodes = nodes.iter().map(|s| (*s).to_owned()).collect();
    e
}

/// Build a `Simulation_SPICE:VDC` voltage source flagged as a power
/// rail. Terminal 1 is the rail node; terminal 2 is ground.
fn make_v_power(refdes: &str, rail: &str) -> ResolvedElement {
    let lib = fixture_library();
    let symbol = lib
        .lookup("Simulation_SPICE:VDC")
        .expect("VDC fixture")
        .clone();
    ResolvedElement {
        refdes: refdes.to_owned(),
        kind: ElementKind::VoltageSrc,
        lib_id: "Simulation_SPICE:VDC".to_owned(),
        symbol,
        pin_mapping: vec!["1".into(), "2".into()],
        nodes: vec![rail.to_owned(), "0".to_owned()],
        value: None,
        role: ElementRole::Power(rail.to_owned()),
    }
}

fn checked_from_resolved(rn: ResolvedNetlist) -> CheckedNetlist {
    let (c, _w) = check(rn).expect("policy");
    c
}

/// Build a manually-positioned `Placement` matching `checked.elements` index order.
fn manual_placement(checked: &CheckedNetlist, origins: &[(i32, i32)]) -> Placement {
    let elements = checked
        .elements
        .iter()
        .zip(origins)
        .map(|(e, &(x, y))| PlacedElement {
            refdes: e.refdes.clone(),
            lib_id: e.lib_id.clone(),
            origin: GridPoint::new(x, y),
            orientation: Orientation::IDENTITY,
        })
        .collect();
    Placement { elements }
}

// ---------------------------------------------------------------------------
// HPWL case tests
// ---------------------------------------------------------------------------

#[test]
fn hpwl_two_pin_net_is_manhattan_distance() {
    // Two resistors share net "n1" at terminal 2 of R1 and terminal 1
    // of R2. Their other terminals are unique (so those nets have just
    // one pin and contribute 0).
    let rn = ResolvedNetlist {
        elements: vec![
            make_r_with_nodes("R1", &["a", "n1"]),
            make_r_with_nodes("R2", &["n1", "b"]),
        ],
        align: vec![],
        place: vec![],
        subckts: vec![],
    };
    let checked = checked_from_resolved(rn);
    // R1 at origin (0,0), R2 placed 6 grid cells right + 4 cells up.
    let p = manual_placement(&checked, &[(0, 0), (6, 4)]);
    let bd = breakdown(&p, &checked, fixture_library());

    // Pin "2" of Device:R is at local (0, -3.81). Pin "1" of Device:R
    // is at local (0, +3.81).
    // R1 pin 2 world: (0, -3.81). R2 pin 1 world:
    //   x = 6 * 1.27 = 7.62, y = 4 * 1.27 + 3.81 = 8.89.
    // HPWL = (7.62 - 0) + (8.89 - (-3.81)) = 7.62 + 12.70 = 20.32 mm.
    let expected = 7.62 + 12.70;
    assert!(
        (bd.hpwl - expected).abs() < 1e-9,
        "hpwl {} expected {}",
        bd.hpwl,
        expected
    );
}

#[test]
fn hpwl_skips_ground_net_zero() {
    // Two resistors both tied to ground; second terminal connected by
    // net "sig". Without ground filtering, ground HPWL would be ~0 too
    // here, so we set up an asymmetric layout to make sure a hypothetical
    // ground HPWL would be huge — and assert HPWL only reflects "sig".
    //
    // R1 nodes: ("0", "sig")  ;  R2 nodes: ("0", "sig")
    let rn = ResolvedNetlist {
        elements: vec![
            make_r_with_nodes("R1", &["0", "sig"]),
            make_r_with_nodes("R2", &["0", "sig"]),
        ],
        align: vec![],
        place: vec![],
        subckts: vec![],
    };
    let checked = checked_from_resolved(rn);
    let p = manual_placement(&checked, &[(0, 0), (10, 0)]);

    // Pin 1 (terminal 1, node "0") of R1 at (0, +3.81), R2 at (12.7, +3.81).
    // Pin 2 (terminal 2, node "sig") of R1 at (0, -3.81), R2 at (12.7, -3.81).
    // sig HPWL = 12.7 + 0 = 12.7. Ground HPWL would also be 12.7 — so
    // total without filter would be 25.4, with filter just 12.7.
    let bd = breakdown(&p, &checked, fixture_library());
    let expected_sig = 12.7;
    assert!(
        (bd.hpwl - expected_sig).abs() < 1e-6,
        "hpwl {} expected {}",
        bd.hpwl,
        expected_sig
    );
}

// ---------------------------------------------------------------------------
// Overlap case tests
// ---------------------------------------------------------------------------

#[test]
fn overlap_zero_when_far_apart() {
    let rn = ResolvedNetlist {
        elements: vec![make_r("R1"), make_r("R2")],
        align: vec![],
        place: vec![],
        subckts: vec![],
    };
    let checked = checked_from_resolved(rn);
    // CELL_W = 6 grid units; place 6 units apart.
    let p = manual_placement(&checked, &[(0, 0), (6, 0)]);
    let bd = breakdown(&p, &checked, fixture_library());
    assert!((bd.overlap - 0.0).abs() < 1e-12, "overlap {}", bd.overlap);
}

#[test]
fn overlap_full_when_origins_coincide() {
    let rn = ResolvedNetlist {
        elements: vec![make_r("R1"), make_r("R2")],
        align: vec![],
        place: vec![],
        subckts: vec![],
    };
    let checked = checked_from_resolved(rn);
    let p = manual_placement(&checked, &[(0, 0), (0, 0)]);
    let bd = breakdown(&p, &checked, fixture_library());
    // CELL_W * CELL_H * STEP_MM² = 6 * 6 * 1.27² = 58.0644 mm².
    let expected = 6.0 * 6.0 * STEP_MM * STEP_MM;
    assert!(
        (bd.overlap - expected).abs() < 1e-9,
        "overlap {} expected {}",
        bd.overlap,
        expected
    );
}

// ---------------------------------------------------------------------------
// Crossings case tests
// ---------------------------------------------------------------------------

#[test]
fn crossings_zero_for_parallel_nets() {
    // Two resistors stacked vertically, two distinct signal nets — no
    // crossings.
    let rn = ResolvedNetlist {
        elements: vec![
            make_r_with_nodes("R1", &["a", "b"]),
            make_r_with_nodes("R2", &["c", "d"]),
        ],
        align: vec![],
        place: vec![],
        subckts: vec![],
    };
    let checked = checked_from_resolved(rn);
    let p = manual_placement(&checked, &[(0, 0), (10, 0)]);
    let bd = breakdown(&p, &checked, fixture_library());
    // Each net has one pin → MST is empty → 0 crossings.
    assert!((bd.crossings - 0.0).abs() < 1e-12);
}

#[test]
fn crossings_one_for_diagonal_pair() {
    // Build a square-corner placement with two 2-pin nets crossing.
    //
    // We use four resistors. Each resistor contributes one pin to a
    // shared net via terminal 1 (node = "x_top" / "y_top") and a
    // unique singleton ground via terminal 2.
    //
    // Pin 1 (terminal 1) is at local (0, +3.81); origins at the four
    // corners place pin 1 at:
    //   R1 (0,0)   -> (0, 3.81)        net "diag1"
    //   R2 (10,10) -> (12.7, 16.51)    net "diag1"
    //   R3 (10,0)  -> (12.7, 3.81)     net "diag2"
    //   R4 (0,10)  -> (0, 16.51)       net "diag2"
    // diag1 connects (0, 3.81) ↔ (12.7, 16.51); diag2 connects
    // (12.7, 3.81) ↔ (0, 16.51). They cross at the center.
    let rn = ResolvedNetlist {
        elements: vec![
            make_r_with_nodes("R1", &["diag1", "g1"]),
            make_r_with_nodes("R2", &["diag1", "g2"]),
            make_r_with_nodes("R3", &["diag2", "g3"]),
            make_r_with_nodes("R4", &["diag2", "g4"]),
        ],
        align: vec![],
        place: vec![],
        subckts: vec![],
    };
    let checked = checked_from_resolved(rn);
    let p = manual_placement(&checked, &[(0, 0), (10, 10), (10, 0), (0, 10)]);
    let bd = breakdown(&p, &checked, fixture_library());
    assert!(
        (bd.crossings - 1.0).abs() < 1e-12,
        "expected 1 crossing, got {}",
        bd.crossings
    );
}

// ---------------------------------------------------------------------------
// Constraint-violation case tests
// ---------------------------------------------------------------------------

#[test]
fn constraint_violation_zero_for_clean_stage1_output() {
    let rn = mk_resolved(
        &["R1", "R2", "R3"],
        &[(Axis::Horizontal, &["R1", "R2"])],
        &[("R3", Relation::RightOf, "R1")],
    );
    let checked = checked_from_resolved(rn);
    let p = place(checked.clone(), fixture_library()).expect("placement");
    let bd = breakdown(&p, &checked, fixture_library());
    assert!(
        bd.constraint_violation < 1e-9,
        "expected ~0, got {}",
        bd.constraint_violation
    );
}

#[test]
fn constraint_violation_align_horizontal_broken() {
    let rn = mk_resolved(&["R1", "R2"], &[(Axis::Horizontal, &["R1", "R2"])], &[]);
    let checked = checked_from_resolved(rn);
    // Hand-break the alignment: R2 is 5 grid units below.
    let p = manual_placement(&checked, &[(0, 0), (10, 5)]);
    let bd = breakdown(&p, &checked, fixture_library());
    // Variance of [0, 5*STEP_MM] around mean 2.5*STEP_MM:
    // 2 * (2.5 * 1.27)² ≈ 2 * 3.175² ≈ 20.16 mm².
    let expected = 2.0 * (2.5 * STEP_MM).powi(2);
    assert!(
        (bd.constraint_violation - expected).abs() < 1e-6,
        "cv {} expected {}",
        bd.constraint_violation,
        expected
    );
}

#[test]
fn constraint_violation_right_of_satisfied_is_zero() {
    let rn = mk_resolved(&["A", "B"], &[], &[("B", Relation::RightOf, "A")]);
    let checked = checked_from_resolved(rn);
    let p = place(checked.clone(), fixture_library()).expect("placement");
    let bd = breakdown(&p, &checked, fixture_library());
    assert!(
        bd.constraint_violation < 1e-9,
        "stage-1 placement should satisfy place; got {}",
        bd.constraint_violation
    );
}

#[test]
fn constraint_violation_right_of_violated_when_target_left() {
    // PlaceSpec says B right-of A, but we place B to the left of A.
    let rn = ResolvedNetlist {
        elements: vec![make_r("A"), make_r("B")],
        align: vec![],
        place: vec![PlaceSpec {
            refdes: "B".into(),
            relation: Relation::RightOf,
            anchor: "A".into(),
            span: None,
        }],
        subckts: vec![],
    };
    let checked = checked_from_resolved(rn);
    // A at x=10, B at x=0 (so B is left of A → hinged X term active).
    // Also offset Y so the always-Y term is positive.
    let p = manual_placement(&checked, &[(10, 0), (0, 3)]);
    let bd = breakdown(&p, &checked, fixture_library());
    assert!(
        bd.constraint_violation > 1.0,
        "expected large positive violation, got {}",
        bd.constraint_violation
    );
}

// ---------------------------------------------------------------------------
// total / weights case tests
// ---------------------------------------------------------------------------

#[test]
fn total_uses_default_weights_linearly() {
    let rn = mk_resolved(&["R1", "R2", "R3"], &[], &[]);
    let checked = checked_from_resolved(rn);
    let p = place(checked.clone(), fixture_library()).expect("placement");
    let bd = breakdown(&p, &checked, fixture_library());
    let t = total(&bd, &CostWeights::DEFAULT);
    let manual = bd.crossings * 100.0
        + bd.constraint_violation * 1000.0
        + bd.overlap * 50.0
        + bd.hpwl * 1.0
        + bd.rail_direction * 50.0
        + bd.signal_flow * 25.0;
    assert!((t - manual).abs() < 1e-9, "total {t} manual {manual}");
}

// ---------------------------------------------------------------------------
// Rail direction (ζ)
// ---------------------------------------------------------------------------

#[test]
fn rail_direction_power_above_zero_below() {
    // V1 is a Vcc-tagged power source; R1 has a ground pin. The
    // "correct" placement puts V1 above R1; the swapped placement
    // inverts them. Both cases share the same pin extents, so only
    // the rail-direction term should change.
    // R1 is connected only to ground (and an unrelated signal node);
    // V1 is the power source. With the rail and the ground tied to
    // distinct elements, the ordering is no longer mirror-symmetric.
    let rn = ResolvedNetlist {
        elements: vec![
            make_v_power("V1", "vcc"),
            make_r_with_nodes("R1", &["0", "sig"]),
        ],
        align: vec![],
        place: vec![],
        subckts: vec![],
    };
    let checked = checked_from_resolved(rn);

    let p_correct = manual_placement(&checked, &[(0, 8), (0, 0)]);
    let p_swapped = manual_placement(&checked, &[(0, 0), (0, 8)]);

    let bd_correct = breakdown(&p_correct, &checked, fixture_library());
    let bd_swapped = breakdown(&p_swapped, &checked, fixture_library());

    assert!(
        bd_swapped.rail_direction > bd_correct.rail_direction,
        "expected swapped > correct, got swapped={} correct={}",
        bd_swapped.rail_direction,
        bd_correct.rail_direction
    );
}

// ---------------------------------------------------------------------------
// Signal flow (η)
// ---------------------------------------------------------------------------

#[test]
fn signal_flow_left_to_right_better_than_right_to_left() {
    // A subckt with ports = ["vin", "vout"] and a single resistor
    // connecting them. Left→right places the input pin on the left
    // edge; right→left swaps it.
    let rn = ResolvedNetlist {
        elements: vec![make_r_with_nodes("R1", &["vin", "vout"])],
        align: vec![],
        place: vec![],
        subckts: vec![SubcktPorts {
            name: "amp".into(),
            ports: vec!["vin".into(), "vout".into()],
        }],
    };
    let checked = checked_from_resolved(rn);

    // Single element — extents collapse, so use two elements to give
    // a non-degenerate x-range.
    let rn2 = ResolvedNetlist {
        elements: vec![
            make_r_with_nodes("R1", &["vin", "mid"]),
            make_r_with_nodes("R2", &["mid", "vout"]),
        ],
        align: vec![],
        place: vec![],
        subckts: vec![SubcktPorts {
            name: "amp".into(),
            ports: vec!["vin".into(), "vout".into()],
        }],
    };
    let checked2 = checked_from_resolved(rn2);

    // Correct: R1 (carries vin) on the left, R2 (carries vout) on the right.
    let p_correct = manual_placement(&checked2, &[(0, 0), (10, 0)]);
    // Reversed: R1 on the right, R2 on the left.
    let p_reversed = manual_placement(&checked2, &[(10, 0), (0, 0)]);

    let bd_correct = breakdown(&p_correct, &checked2, fixture_library());
    let bd_reversed = breakdown(&p_reversed, &checked2, fixture_library());

    assert!(
        bd_reversed.signal_flow > bd_correct.signal_flow,
        "expected reversed > correct, got reversed={} correct={}",
        bd_reversed.signal_flow,
        bd_correct.signal_flow
    );

    // Sanity: the single-element placement should produce zero
    // signal-flow because both extents collapse.
    let p_single = manual_placement(&checked, &[(0, 0)]);
    let bd_single = breakdown(&p_single, &checked, fixture_library());
    assert!(bd_single.signal_flow >= 0.0);
}

#[test]
fn zero_annotations_zero_rail_and_flow() {
    let rn = ResolvedNetlist {
        elements: vec![
            make_r_with_nodes("R1", &["a", "b"]),
            make_r_with_nodes("R2", &["b", "c"]),
        ],
        align: vec![],
        place: vec![],
        subckts: vec![],
    };
    let checked = checked_from_resolved(rn);
    let p = manual_placement(&checked, &[(0, 0), (10, 0)]);
    let bd = breakdown(&p, &checked, fixture_library());
    // Both terms are exactly 0.0 by construction (no rail/ground/subckt
    // pins to penalise), so a strict-equality compare is intentional.
    #[allow(clippy::float_cmp)]
    {
        assert!(
            bd.rail_direction == 0.0,
            "rail_direction should be 0, got {}",
            bd.rail_direction
        );
        assert!(
            bd.signal_flow == 0.0,
            "signal_flow should be 0, got {}",
            bd.signal_flow
        );
    }
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Scenario {
    n: usize,
    align: Vec<(Axis, Vec<usize>)>,
    place: Vec<(usize, Relation, usize)>,
}

fn refdes(i: usize) -> String {
    format!("R{}", i + 1)
}

fn build_scenario(scenario: &Scenario) -> (Placement, CheckedNetlist) {
    // Same construction as `properties.rs`, but with non-trivial node
    // names so HPWL / crossings actually exercise non-zero paths:
    // each resistor uses ("n<i>a", "n<i>b") nodes by default; a couple
    // of forced-shared nets create multi-pin nets.
    let names: Vec<String> = (0..scenario.n).map(refdes).collect();

    // Build resolved elements with shared nets where index pairs match.
    let mut elements: Vec<ResolvedElement> = Vec::with_capacity(scenario.n);
    for (i, name) in names.iter().enumerate() {
        let nodes = if i + 1 < scenario.n {
            // chain: terminal 2 of R_i shares with terminal 1 of R_{i+1}
            vec![format!("net{i}"), format!("net{}", i + 1)]
        } else {
            vec![format!("net{i}"), "0".to_owned()]
        };
        elements.push(make_r_with_nodes(
            name,
            &nodes.iter().map(String::as_str).collect::<Vec<_>>(),
        ));
    }

    let align_specs: Vec<AlignSpec> = scenario
        .align
        .iter()
        .map(|(axis, idxs)| AlignSpec {
            axis: *axis,
            refdes: idxs.iter().map(|i| names[*i].clone()).collect(),
            span: None,
        })
        .collect();
    let place_specs: Vec<PlaceSpec> = scenario
        .place
        .iter()
        .map(|(t, rel, a)| PlaceSpec {
            refdes: names[*t].clone(),
            relation: *rel,
            anchor: names[*a].clone(),
            span: None,
        })
        .collect();
    let rn = ResolvedNetlist {
        elements,
        align: align_specs,
        place: place_specs,
        subckts: vec![],
    };
    let (checked, _w) = check(rn).expect("policy");
    let p = place(checked.clone(), fixture_library()).expect("placement");
    (p, checked)
}

fn arb_scenario() -> impl Strategy<Value = Scenario> {
    (2usize..=6).prop_flat_map(|n| {
        let align = proptest::collection::vec(
            (
                prop_oneof![Just(Axis::Horizontal), Just(Axis::Vertical)],
                proptest::collection::vec(0..n, 2..=n.min(4)),
            ),
            0..=2,
        );
        let place = proptest::collection::vec(
            (0..n.saturating_sub(1)).prop_flat_map(move |a| {
                let target = (a + 1)..n;
                let rel = prop_oneof![Just(Relation::RightOf), Just(Relation::Above)];
                (target, rel, Just(a)).prop_map(|(t, r, a)| (t, r, a))
            }),
            0..=3,
        );
        (Just(n), align, place).prop_map(|(n, align, place)| {
            // Same de-conflict pattern as properties.rs: drop dups,
            // align-fixed targets, multi-cluster membership.
            let mut seen_targets = std::collections::HashSet::new();
            let mut seen_anchors = std::collections::HashSet::new();
            let mut place_clean: Vec<(usize, Relation, usize)> = Vec::new();
            for (t, r, a) in place {
                if seen_targets.insert(t) && seen_anchors.insert(a) {
                    place_clean.push((t, r, a));
                }
            }
            let mut already_aligned = std::collections::HashSet::new();
            let mut align_clean: Vec<(Axis, Vec<usize>)> = Vec::new();
            for (axis, members) in align {
                let mut seen = std::collections::HashSet::new();
                let unique: Vec<usize> = members
                    .into_iter()
                    .filter(|i| seen.insert(*i) && !already_aligned.contains(i))
                    .collect();
                if unique.len() >= 2 {
                    for i in &unique {
                        already_aligned.insert(*i);
                    }
                    align_clean.push((axis, unique));
                }
            }
            let place_clean: Vec<_> = place_clean
                .into_iter()
                .filter(|(t, _, _)| !already_aligned.contains(t))
                .collect();
            Scenario {
                n,
                align: align_clean,
                place: place_clean,
            }
        })
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

    #[test]
    fn cost_finite_and_nonneg(scenario in arb_scenario()) {
        let (p, checked) = build_scenario(&scenario);
        let bd = breakdown(&p, &checked, fixture_library());
        for v in [
            bd.hpwl,
            bd.overlap,
            bd.crossings,
            bd.constraint_violation,
            bd.rail_direction,
            bd.signal_flow,
        ] {
            prop_assert!(v.is_finite(), "non-finite component: {v}");
            prop_assert!(v >= 0.0, "negative component: {v}");
        }
    }

    #[test]
    fn default_weights_total_matches_components(scenario in arb_scenario()) {
        let (p, checked) = build_scenario(&scenario);
        let bd = breakdown(&p, &checked, fixture_library());
        let t = total(&bd, &CostWeights::DEFAULT);
        let manual = bd.crossings * 100.0
            + bd.constraint_violation * 1000.0
            + bd.overlap * 50.0
            + bd.hpwl * 1.0
            + bd.rail_direction * 50.0
            + bd.signal_flow * 25.0;
        prop_assert!((t - manual).abs() < 1e-9);
    }

    #[test]
    fn stage1_clean_placement_has_zero_constraint_violation(scenario in arb_scenario()) {
        let (p, checked) = build_scenario(&scenario);
        let bd = breakdown(&p, &checked, fixture_library());
        prop_assert!(
            bd.constraint_violation < 1e-6,
            "stage-1 placement should satisfy all align/place; got {}",
            bd.constraint_violation
        );
    }
}

// Silence unused import warning for `ElementKind`/`ElementRole` if no
// test uses them directly — they're brought in for symmetry with
// `common::mk_resolved` callers and may become useful as more tests
// are added.
#[allow(dead_code)]
fn _unused(_e: ElementKind, _r: ElementRole) {}
