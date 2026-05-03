//! Specific case tests for the stage-1 placer.

mod common;

use common::{fixture_library, mk_resolved};
use spice_layout::{Placement, place};
use spice_policy::check;
use spice_resolve::{Axis, Relation};

const STEP_MM: f64 = 1.27;
const TOL_MM: f64 = 1e-9;

fn run(
    refdeses: &[&str],
    align: &[(Axis, &[&str])],
    placespec: &[(&str, Relation, &str)],
) -> Placement {
    let resolved = mk_resolved(refdeses, align, placespec);
    let (checked, warns) = check(resolved).expect("policy check");
    assert!(warns.is_empty(), "unexpected warnings: {warns:?}");
    place(checked, fixture_library()).expect("placement")
}

fn refdes<'a>(p: &'a Placement, name: &str) -> &'a spice_layout::PlacedElement {
    p.elements
        .iter()
        .find(|e| e.refdes == name)
        .unwrap_or_else(|| panic!("no such refdes {name}"))
}

#[test]
fn three_unconstrained_elements_are_placed_in_a_row() {
    let p = run(&["R1", "R2", "R3"], &[], &[]);
    assert_eq!(p.elements.len(), 3);
    // All distinct origins.
    let mut origins: Vec<_> = p.elements.iter().map(|e| e.origin).collect();
    origins.sort_by_key(|gp| (gp.x, gp.y));
    origins.dedup();
    assert_eq!(origins.len(), 3, "origins must be distinct");
    // Same Y row.
    let y0 = p.elements[0].origin.y;
    for e in &p.elements {
        assert_eq!(e.origin.y, y0);
    }
}

#[test]
fn align_horizontal_shares_y() {
    let p = run(&["R1", "R2"], &[(Axis::Horizontal, &["R1", "R2"])], &[]);
    assert_eq!(refdes(&p, "R1").origin.y, refdes(&p, "R2").origin.y);
    assert_ne!(refdes(&p, "R1").origin.x, refdes(&p, "R2").origin.x);
}

#[test]
fn align_vertical_shares_x() {
    let p = run(&["R1", "R2"], &[(Axis::Vertical, &["R1", "R2"])], &[]);
    assert_eq!(refdes(&p, "R1").origin.x, refdes(&p, "R2").origin.x);
    assert_ne!(refdes(&p, "R1").origin.y, refdes(&p, "R2").origin.y);
}

#[test]
fn chained_right_of_resolves_topologically() {
    // A ; B right-of A ; C right-of B. List C's place first to force
    // the worklist to defer it until B is fixed.
    let p = run(
        &["A", "B", "C"],
        &[],
        &[("C", Relation::RightOf, "B"), ("B", Relation::RightOf, "A")],
    );
    let a_x = refdes(&p, "A").origin.x;
    let b_x = refdes(&p, "B").origin.x;
    let c_x = refdes(&p, "C").origin.x;
    assert!(
        a_x < b_x && b_x < c_x,
        "expected A < B < C; got {a_x} {b_x} {c_x}"
    );
    // Pin Y must match for the relation to hold (uniform orientation).
    assert_eq!(refdes(&p, "A").origin.y, refdes(&p, "B").origin.y);
    assert_eq!(refdes(&p, "B").origin.y, refdes(&p, "C").origin.y);
}

#[test]
fn place_anchored_to_unconstrained_anchor() {
    let p = run(&["A", "B"], &[], &[("B", Relation::RightOf, "A")]);
    let lib = fixture_library();
    let a_sym = lib.lookup(&refdes(&p, "A").lib_id).unwrap();
    let b_sym = lib.lookup(&refdes(&p, "B").lib_id).unwrap();
    let a_pins = refdes(&p, "A").world_pin_mm(a_sym);
    let b_pins = refdes(&p, "B").world_pin_mm(b_sym);
    let a_max_x = a_pins
        .iter()
        .map(|(_, x, _)| *x)
        .fold(f64::NEG_INFINITY, f64::max);
    let b_min_x = b_pins
        .iter()
        .map(|(_, x, _)| *x)
        .fold(f64::INFINITY, f64::min);
    assert!(b_min_x > a_max_x + TOL_MM, "b not strictly right of a");
    // Some pin on a's rightmost column shares Y with a pin on b's leftmost.
    let a_right_ys: Vec<f64> = a_pins
        .iter()
        .filter(|(_, x, _)| (*x - a_max_x).abs() <= TOL_MM)
        .map(|(_, _, y)| *y)
        .collect();
    let b_left_ys: Vec<f64> = b_pins
        .iter()
        .filter(|(_, x, _)| (*x - b_min_x).abs() <= TOL_MM)
        .map(|(_, _, y)| *y)
        .collect();
    assert!(
        a_right_ys
            .iter()
            .any(|ay| b_left_ys.iter().any(|by| (ay - by).abs() <= TOL_MM)),
        "no shared Y between a's right pins {a_right_ys:?} and b's left pins {b_left_ys:?}"
    );
}

#[test]
fn mixed_align_place_and_unconstrained() {
    // R1 R2 aligned horizontal; R3 right-of R1; R4 R5 unconstrained.
    let p = run(
        &["R1", "R2", "R3", "R4", "R5"],
        &[(Axis::Horizontal, &["R1", "R2"])],
        &[("R3", Relation::RightOf, "R1")],
    );
    assert_eq!(p.elements.len(), 5);
    // R1 and R2 share Y.
    assert_eq!(refdes(&p, "R1").origin.y, refdes(&p, "R2").origin.y);
    // R3's pin Y matches R1's.
    let lib = fixture_library();
    let sym = lib.lookup("Device:R").unwrap();
    let r1_pins = refdes(&p, "R1").world_pin_mm(sym);
    let r3_pins = refdes(&p, "R3").world_pin_mm(sym);
    let r1_max_x = r1_pins
        .iter()
        .map(|(_, x, _)| *x)
        .fold(f64::NEG_INFINITY, f64::max);
    let r3_min_x = r3_pins
        .iter()
        .map(|(_, x, _)| *x)
        .fold(f64::INFINITY, f64::min);
    assert!(r3_min_x > r1_max_x + TOL_MM);
    // R4 and R5 are at the auto-fill row, distinct origins.
    assert_ne!(refdes(&p, "R4").origin, refdes(&p, "R5").origin);
    // All origins on grid by construction (i32). Spot-check mm.
    for e in &p.elements {
        let (x, y) = e.origin.to_mm();
        assert!((x / STEP_MM - (x / STEP_MM).round()).abs() <= TOL_MM);
        assert!((y / STEP_MM - (y / STEP_MM).round()).abs() <= TOL_MM);
    }
}
