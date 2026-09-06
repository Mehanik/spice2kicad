//! End-to-end POSITION assertions for the canonical-placement idioms
//! (Tier-2 V6/V7). Each test drives the *full* placer (`place_with`,
//! seed → symmetry → idiom → orientation → SA refine) on a real fixture
//! and asserts a **pin-anchored** geometric outcome — the placement a
//! human would draw — without asserting anything about orientation
//! (orientation flow is walled; these idioms are POSITION-only).
//!
//! The idioms:
//!
//!   1. PARALLEL two-terminal pair  → `common_emitter` RE‖CE stacked in
//!      one vertical column, adjacent. (Deferred; see below.)
//!   2. COLLECTOR-LOAD above BJT    → `diff_pair` RC1 above Q1's
//!      collector (same X column, pin-anchored), RC2 above Q2 likewise.
//!   3. SHARED-NODE centering       → `diff_pair` RTAIL centered under
//!      Q1/Q2's shared tail node, one band below.
//!
//! Every assertion reads live pin geometry via `world_pin_mm`, so it is
//! robust to wherever the anchor transistors finally land — and, being
//! pin-anchored rather than origin-anchored, it cannot be satisfied by a
//! column that merely lines up *bodies*. That distinction is the whole
//! reason these assertions exist: ADR-40's promotion follow-up found the
//! DC-series column choosing its X as the barycenter of its members'
//! **origins**, leaving a 2.54 mm jog in the collector wire of every
//! column containing a BJT.
//!
//! # Where the assertions live
//!
//! In `tests/common/structural.rs`, not here — see the same note in
//! `rail_convention.rs`. This file is the default-placer view;
//! `tests/challenger_structural.rs` sweeps the identical functions over
//! every registered `--placer` arm, which is the coverage ADR-40 found
//! missing.

mod common;

use common::structural;
use spice_layout::Placer;

fn must(outcome: Result<(), String>) {
    if let Err(msg) = outcome {
        panic!("{msg}");
    }
}

// ---------------------------------------------------------------------------
// Idiom 1 — PARALLEL two-terminal pair: common_emitter RE ‖ CE
// (both connect nets `e` and `0`) must sit vertically aligned + adjacent.
//
// DEFERRED: a position-only same-column stack shorts the non-ground net
// past the ground pin (V11, Tier 0) when a shared net is ground, and the
// clean fix is an orientation flip that the left→right flow-wall
// forbids. Re-enable when a v0.2 owns the flip. See
// `spice_layout::idioms::ParallelPair` and `apply_position_idioms`.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "Idiom 1 deferred: position-only parallel stack is a V11 short under the orientation wall"]
fn common_emitter_re_ce_parallel_seed() {
    must(structural::ce_re_ce_parallel(Placer::default(), false));
}

#[test]
#[ignore = "Idiom 1 deferred: position-only parallel stack is a V11 short under the orientation wall"]
fn common_emitter_re_ce_parallel_refined() {
    must(structural::ce_re_ce_parallel(Placer::default(), true));
}

// ---------------------------------------------------------------------------
// Idiom 2 — COLLECTOR-LOAD above transistor: diff_pair RC1 above Q1's
// collector (net c1), RC2 above Q2's collector (net c2). Pin-anchored on
// the shared collector net's X column.
//
// LIVE, generalised as the RAIL-STUB COLUMN idiom
// (`spice_layout::idioms::{detect_rail_stubs, apply_rail_stub_columns}`,
// wired from `lib::apply_rail_stub_columns`).
//
// It was previously deferred as "ripples the busiest ratchets + fights
// the V7 RC1/RC2 symmetry pin". Neither held once `cost::rail_direction`
// was un-inverted: the fixtures' crossing / wire-length / body-overlap
// budgets are all still green, and the symmetry pin is not the obstacle
// — `RC1`/`RC2` are pinned by the fixture's own `*@align horizontal`,
// whose constraint is a shared *row*, so correcting their column is
// orthogonal to it (and is exactly the reported defect).
//
// Note the detector is no longer BJT-specific: a stub is any two-terminal
// element with one pin on a rail, and its column is the vertically-facing
// pin of the multi-terminal device it terminates. A collector load is
// simply the case where that device is a transistor.
// ---------------------------------------------------------------------------

#[test]
fn diff_pair_rc1_over_q1_collector_seed() {
    must(structural::diff_pair_collector_load_column(
        Placer::default(),
        false,
        "RC1",
        "Q1",
        "c1",
    ));
}

#[test]
fn diff_pair_rc1_over_q1_collector_refined() {
    must(structural::diff_pair_collector_load_column(
        Placer::default(),
        true,
        "RC1",
        "Q1",
        "c1",
    ));
}

#[test]
fn diff_pair_rc2_over_q2_collector_seed() {
    must(structural::diff_pair_collector_load_column(
        Placer::default(),
        false,
        "RC2",
        "Q2",
        "c2",
    ));
}

#[test]
fn diff_pair_rc2_over_q2_collector_refined() {
    must(structural::diff_pair_collector_load_column(
        Placer::default(),
        true,
        "RC2",
        "Q2",
        "c2",
    ));
}

// ---------------------------------------------------------------------------
// Idiom 3 — SHARED-NODE centering: diff_pair RTAIL sits centered under
// the shared tail node of Q1/Q2 (both emitters on net `tail`) and one
// band below them.
// ---------------------------------------------------------------------------

#[test]
fn diff_pair_rtail_centered_below_seed() {
    must(structural::diff_pair_rtail_centered(
        Placer::default(),
        false,
    ));
}

#[test]
fn diff_pair_rtail_centered_below_refined() {
    must(structural::diff_pair_rtail_centered(
        Placer::default(),
        true,
    ));
}
