//! Stage-3 (FR + SA) tests.
//!
//! These tests opt into refinement via `LayoutOptions { refine: true,
//! .. }`. They establish the contract the refined placer must hold:
//!
//! 1. Hard constraints (`align`, `place`) still satisfied — pinned
//!    elements never moved.
//! 2. Determinism: same seed → identical placement.
//! 3. No regression: refined cost ≤ stage-1 cost on the connected
//!    chains where the refiner has freedom to act.
//! 4. Grid: every origin still on the integer grid.

mod common;

use common::{fixture_library, mk_resolved};
use kicad_symbols::Orientation;
use spice_layout::{
    LayoutOptions, Placement,
    cost::{self, CostWeights},
    place, place_with,
};
use spice_policy::check;
use spice_resolve::{Axis, Relation, ResolvedNetlist};

const TOL_MM: f64 = 1e-9;

fn refined(resolved: ResolvedNetlist, opts: LayoutOptions) -> Placement {
    let (checked, _warns) = check(resolved).expect("policy check");
    place_with(checked, fixture_library(), &opts).expect("placement")
}

fn refine_opts(seed: u64) -> LayoutOptions {
    LayoutOptions {
        refine: true,
        seed,
        // Small budgets keep tests fast; the algorithms are exercised,
        // tuning happens against `examples/` separately.
        fr_iters: 30,
        refine_iterations: 500,
    }
}

#[test]
fn pinned_elements_never_move_under_refinement() {
    let resolved = mk_resolved(
        &["R1", "R2", "R3", "R4"],
        &[(Axis::Horizontal, &["R1", "R2"])],
        &[("R3", Relation::RightOf, "R1")],
    );
    let (checked, _) = check(resolved).expect("policy check");

    let stage1 = place(checked.clone(), fixture_library()).expect("stage1");
    let refined = place_with(checked, fixture_library(), &refine_opts(1)).expect("stage3");

    for s1 in &stage1.elements {
        // R1, R2 are aligned; R3 is placed; only R4 is unconstrained
        // and may move. The other three must keep their stage-1
        // origins exactly.
        if matches!(s1.refdes.as_str(), "R1" | "R2" | "R3") {
            let r = refined
                .elements
                .iter()
                .find(|e| e.refdes == s1.refdes)
                .unwrap();
            assert_eq!(r.origin, s1.origin, "{} moved", s1.refdes);
            assert_eq!(r.orientation, s1.orientation, "{} rotated", s1.refdes);
        }
    }
}

#[test]
fn determinism_same_seed_same_placement() {
    let resolved = mk_resolved(&["R1", "R2", "R3", "R4", "R5"], &[], &[]);
    let a = refined(resolved.clone(), refine_opts(42));
    let b = refined(resolved, refine_opts(42));
    assert_eq!(a.elements.len(), b.elements.len());
    for (ea, eb) in a.elements.iter().zip(b.elements.iter()) {
        assert_eq!(ea.refdes, eb.refdes);
        assert_eq!(ea.origin, eb.origin, "{} moved between runs", ea.refdes);
        assert_eq!(ea.orientation, eb.orientation);
    }
}

#[test]
fn refined_origins_remain_on_grid() {
    let resolved = mk_resolved(&["R1", "R2", "R3"], &[], &[]);
    let p = refined(resolved, refine_opts(7));
    for e in &p.elements {
        let (x, y) = e.origin.to_mm();
        let step = spice_layout::GridPoint::STEP_MM;
        assert!((x / step - (x / step).round()).abs() <= TOL_MM);
        assert!((y / step - (y / step).round()).abs() <= TOL_MM);
    }
}

#[test]
fn refined_cost_no_worse_than_stage1_on_connected_chain() {
    // R1-R2-R3 share nodes pairwise so FR has work to do; no
    // align/place constraints means SA is free to move all three.
    // Build a hand-rolled resolved netlist with shared nodes.
    use common::make_r;
    let mut elements: Vec<_> = ["R1", "R2", "R3"].iter().map(|r| make_r(r)).collect();
    // R1: a-b, R2: b-c, R3: c-d (shared interior nodes b, c).
    elements[0].nodes = vec!["a".into(), "b".into()];
    elements[1].nodes = vec!["b".into(), "c".into()];
    elements[2].nodes = vec!["c".into(), "d".into()];
    let resolved = ResolvedNetlist {
        elements,
        align: vec![],
        place: vec![],
        subckts: vec![],
        sheet_instances: vec![],
    };
    let (checked, _) = check(resolved).expect("policy check");

    // `stage1` here is the deterministic seed *without* refinement,
    // so the SA-refined cost is meaningfully comparable.
    let stage1_opts = LayoutOptions {
        refine: false,
        ..LayoutOptions::default()
    };
    let stage1 = place_with(checked.clone(), fixture_library(), &stage1_opts).expect("stage1");
    let refined =
        place_with(checked.clone(), fixture_library(), &refine_opts(123)).expect("stage3");

    let lib = fixture_library();
    let weights = CostWeights::DEFAULT;
    let stage1_cost = cost::total(&cost::breakdown(&stage1, &checked, lib), &weights);
    let refined_cost = cost::total(&cost::breakdown(&refined, &checked, lib), &weights);
    assert!(
        refined_cost <= stage1_cost + 1e-6,
        "refined cost {refined_cost} > stage1 {stage1_cost}; refinement regressed"
    );
}

#[test]
fn refine_disabled_matches_stage1_exactly() {
    // Two `place_with` calls with refine disabled must produce
    // byte-identical output — no RNG, no SA, just the deterministic
    // stage-1 placer.
    let resolved = mk_resolved(&["R1", "R2", "R3"], &[], &[]);
    let (checked, _) = check(resolved).expect("policy check");
    let opts = LayoutOptions {
        refine: false,
        ..LayoutOptions::default()
    };
    let s1 = place_with(checked.clone(), fixture_library(), &opts).expect("place_with #1");
    let s1_via_with = place_with(checked, fixture_library(), &opts).expect("place_with #2");
    assert_eq!(s1.elements.len(), s1_via_with.elements.len());
    for (a, b) in s1.elements.iter().zip(s1_via_with.elements.iter()) {
        assert_eq!(a.refdes, b.refdes);
        assert_eq!(a.origin, b.origin);
        assert_eq!(a.orientation, b.orientation);
    }
}

#[test]
fn rotation_proposed_but_pinned_unchanged() {
    // Make every element pinned via align — SA's rotation move can
    // pick any movable, but movable is empty, so orientation stays
    // identity for everyone.
    let resolved = mk_resolved(
        &["R1", "R2", "R3"],
        &[(Axis::Horizontal, &["R1", "R2", "R3"])],
        &[],
    );
    let p = refined(resolved, refine_opts(99));
    for e in &p.elements {
        assert_eq!(e.orientation, Orientation::IDENTITY);
    }
}
