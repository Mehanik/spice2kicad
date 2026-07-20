//! `place` relation direction, pinned against annotation-spec §4.3.
//!
//! Spec §4.3 defines:
//!
//! | keyword    | meaning                                       |
//! | ---------- | --------------------------------------------- |
//! | `right-of` | anchor's right edge → element's left edge     |
//! | `left-of`  | mirror of `right-of`                          |
//! | `above`    | anchor's top edge → element's bottom edge     |
//! | `below`    | mirror of `above`                             |
//!
//! i.e. `X ;@ place=above A` puts **X above A** on the sheet. KiCad
//! screen Y grows *downward*, so "above" means X gets the SMALLER y.
//!
//! `solve_place` (and `cost::place_residual`) once had this backwards:
//! both picked the anchor's MAX-y pin as its "top", so `above` emitted
//! the element BELOW the anchor and `below` emitted it above. Same
//! screen-Y-sign class of bug as the `cost::rail_direction` inversion
//! covered by `rail_convention.rs`. These tests pin the direction so a
//! re-inversion fails here rather than surfacing as a mystery layout.

mod common;

use common::{fixture_library, mk_resolved};
use spice_layout::place;
use spice_policy::check;
use spice_resolve::{Axis, Relation};

/// Place `R2` relative to `R1` and return `(r1_y, r2_y)` (or x's).
fn solve(rel: Relation) -> (f64, f64, f64, f64) {
    let resolved = mk_resolved(&["R1", "R2"], &[] as &[(Axis, &[&str])], &[("R2", rel, "R1")]);
    let (checked, _warns) = check(resolved).expect("policy check");
    let p = place(checked, fixture_library()).expect("placement");
    let r1 = p
        .elements
        .iter()
        .find(|e| e.refdes == "R1")
        .expect("R1 placed");
    let r2 = p
        .elements
        .iter()
        .find(|e| e.refdes == "R2")
        .expect("R2 placed");
    let (r1x, r1y) = r1.origin.to_mm();
    let (r2x, r2y) = r2.origin.to_mm();
    (r1x, r1y, r2x, r2y)
}

#[test]
fn above_puts_the_element_above_the_anchor() {
    let (r1x, r1y, r2x, r2y) = solve(Relation::Above);
    assert!(
        r2y < r1y,
        "spec §4.3: `R2 place=above R1` must put R2 ABOVE R1 \
         (smaller screen y); got R2 y={r2y}, R1 y={r1y}"
    );
    assert!(
        (r2x - r1x).abs() < 1e-9,
        "`above` shares a column: R2 x={r2x}, R1 x={r1x}"
    );
}

#[test]
fn below_puts_the_element_below_the_anchor() {
    let (r1x, r1y, r2x, r2y) = solve(Relation::Below);
    assert!(
        r2y > r1y,
        "spec §4.3: `R2 place=below R1` must put R2 BELOW R1 \
         (larger screen y); got R2 y={r2y}, R1 y={r1y}"
    );
    assert!(
        (r2x - r1x).abs() < 1e-9,
        "`below` shares a column: R2 x={r2x}, R1 x={r1x}"
    );
}

#[test]
fn above_and_below_are_mirror_images() {
    let (_, a_r1y, _, a_r2y) = solve(Relation::Above);
    let (_, b_r1y, _, b_r2y) = solve(Relation::Below);
    assert!(
        ((a_r2y - a_r1y) + (b_r2y - b_r1y)).abs() < 1e-9,
        "spec §4.3 calls `below` the mirror of `above`: \
         above Δy={}, below Δy={}",
        a_r2y - a_r1y,
        b_r2y - b_r1y
    );
}

#[test]
fn right_of_puts_the_element_right_of_the_anchor() {
    let (r1x, r1y, r2x, r2y) = solve(Relation::RightOf);
    assert!(r2x > r1x, "R2 x={r2x} must be right of R1 x={r1x}");
    assert!(
        (r2y - r1y).abs() < 1e-9,
        "`right-of` shares a row: R2 y={r2y}, R1 y={r1y}"
    );
}

#[test]
fn left_of_puts_the_element_left_of_the_anchor() {
    let (r1x, r1y, r2x, r2y) = solve(Relation::LeftOf);
    assert!(r2x < r1x, "R2 x={r2x} must be left of R1 x={r1x}");
    assert!(
        (r2y - r1y).abs() < 1e-9,
        "`left-of` shares a row: R2 y={r2y}, R1 y={r1y}"
    );
}
