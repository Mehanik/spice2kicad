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

/// Placed coordinates of the anchor (`R1`) and the target (`R2`).
struct Pair {
    anchor_x: f64,
    anchor_y: f64,
    target_x: f64,
    target_y: f64,
}

/// Place `R2` (the target) relative to `R1` (the anchor).
fn solve(rel: Relation) -> Pair {
    let resolved = mk_resolved(
        &["R1", "R2"],
        &[] as &[(Axis, &[&str])],
        &[("R2", rel, "R1")],
    );
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
    let (anchor_x, anchor_y) = r1.origin.to_mm();
    let (target_x, target_y) = r2.origin.to_mm();
    Pair {
        anchor_x,
        anchor_y,
        target_x,
        target_y,
    }
}

#[test]
fn above_puts_the_element_above_the_anchor() {
    let p = solve(Relation::Above);
    assert!(
        p.target_y < p.anchor_y,
        "spec §4.3: `R2 place=above R1` must put R2 ABOVE R1 \
         (smaller screen y); got R2 y={}, R1 y={}",
        p.target_y,
        p.anchor_y
    );
    assert!(
        (p.target_x - p.anchor_x).abs() < 1e-9,
        "`above` shares a column: R2 x={}, R1 x={}",
        p.target_x,
        p.anchor_x
    );
}

#[test]
fn below_puts_the_element_below_the_anchor() {
    let p = solve(Relation::Below);
    assert!(
        p.target_y > p.anchor_y,
        "spec §4.3: `R2 place=below R1` must put R2 BELOW R1 \
         (larger screen y); got R2 y={}, R1 y={}",
        p.target_y,
        p.anchor_y
    );
    assert!(
        (p.target_x - p.anchor_x).abs() < 1e-9,
        "`below` shares a column: R2 x={}, R1 x={}",
        p.target_x,
        p.anchor_x
    );
}

#[test]
fn above_and_below_are_mirror_images() {
    let up = solve(Relation::Above);
    let down = solve(Relation::Below);
    let up_delta = up.target_y - up.anchor_y;
    let down_delta = down.target_y - down.anchor_y;
    assert!(
        (up_delta + down_delta).abs() < 1e-9,
        "spec §4.3 calls `below` the mirror of `above`: \
         above Δy={up_delta}, below Δy={down_delta}"
    );
}

#[test]
fn right_of_puts_the_element_right_of_the_anchor() {
    let p = solve(Relation::RightOf);
    assert!(
        p.target_x > p.anchor_x,
        "R2 x={} must be right of R1 x={}",
        p.target_x,
        p.anchor_x
    );
    assert!(
        (p.target_y - p.anchor_y).abs() < 1e-9,
        "`right-of` shares a row: R2 y={}, R1 y={}",
        p.target_y,
        p.anchor_y
    );
}

#[test]
fn left_of_puts_the_element_left_of_the_anchor() {
    let p = solve(Relation::LeftOf);
    assert!(
        p.target_x < p.anchor_x,
        "R2 x={} must be left of R1 x={}",
        p.target_x,
        p.anchor_x
    );
    assert!(
        (p.target_y - p.anchor_y).abs() < 1e-9,
        "`left-of` shares a row: R2 y={}, R1 y={}",
        p.target_y,
        p.anchor_y
    );
}
