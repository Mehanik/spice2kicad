//! **Every registered `--placer` arm, against the structural
//! invariants** (ADR-40 follow-up, "Challenger blindness").
//!
//! # The gap this closes
//!
//! Before this file, every `spice-layout` integration test built its
//! `LayoutOptions` with `..LayoutOptions::default()`, so the whole crate
//! only ever exercised [`Placer::default`]. A registered challenger was
//! graded end-to-end by the scoreboard (22 fixtures × ~60 verifiers, a
//! k = 9 multi-seed replay, a 1320-conversion seed sweep) and *still*
//! could not be seen to violate a stated layout invariant, because no
//! test ran it. `dc-series-column-pinned` shipped an origin-anchored
//! column that way — CLAUDE.md § "Layout invariants" says constraints
//! are **pin-anchored** — and it surfaced only when promotion made the
//! arm the default, i.e. at the moment the most geometry is moving and
//! the least attention is available per fixture.
//!
//! # What runs here, and what deliberately does not
//!
//! Only the **structural** properties from
//! `tests/common/structural.rs`: yes/no geometric facts with one correct
//! answer, in CLAUDE.md's constraints-vs-costs sense. Continuous quality
//! gradients (wire length, crossings, bends) stay out — the scoreboard
//! judges those in aggregate, and re-running them per-fixture per-arm
//! would re-measure the scoreboard in the conjunctive frame ADR-23
//! established is satisfiable essentially only by a no-op.
//!
//! # Reading a failure
//!
//! One `#[test]` per arm, named `arm_<name with '-' as '_'>`, so a red
//! run names the arm in the test name *and* in every message. Failures
//! are collected per arm rather than fail-fast, so one broken check does
//! not hide the other twenty-four.
//!
//! # Expected failures
//!
//! [`XFAIL`] is a tripwire, not a mute button, modelled on
//! `crates/spice2kicad/tests/common/xfail.rs`: a registered
//! `(arm, check)` pair is excluded from the gate **and the test fails if
//! it starts passing**, so an entry expires the day the defect it names
//! is fixed. A stale entry — one naming a check the sweep no longer runs
//! — fails too.

mod common;

use std::fmt::Write as _;

use common::structural::{self, CHECKS};
use spice_layout::Placer;

/// Checks the **shipping default itself** fails: pre-existing, deferred
/// defects, each already tracked by its own `#[ignore]`d test in
/// `rail_convention.rs` / `idiom_placement.rs`.
///
/// They are not gated on *any* arm, and an arm that passes one is NOT a
/// failure — it is a repair, reported to stderr. Holding a challenger to
/// a property the default violates would be a false failure of exactly
/// the kind that teaches people to stop reading this suite.
///
/// The tripwire for these lives in
/// [`deferred_checks_are_still_red_on_the_default`], the one
/// configuration where "it started passing" means the defect is fixed.
const DEFERRED_ON_THE_DEFAULT: &[(&str, &str)] = &[
    (
        "rail::named_rails_convention[seed]",
        "`bands::assign_y_bands` bands off `NetClass`, so a `*@power=-5V` net seeds UP; the \
         VertPref switch that fixes it regressed Tier-1 V13 + symbol overlap",
    ),
    (
        "rail::emitter_loads_share_a_row[seed]",
        "`apply_rail_stub_columns` adjusts X only, so the seed does not level a parallel stub \
         group; Y-levelling was measured and reverted after a Tier-1 V13 regression",
    ),
    (
        "idiom::re_ce_parallel[seed]",
        "Idiom 1 deferred: a position-only parallel stack shorts the non-ground net past the \
         ground pin (V11, Tier 0) under the orientation wall",
    ),
    (
        "idiom::re_ce_parallel[refined]",
        "Idiom 1 deferred: same V11 short as the seed case",
    ),
];

/// `(placer name, check id, one-line reason)` — a structural check a
/// *specific* registered arm fails while the shipping default passes it.
///
/// Strict tripwire: a registered pair that starts passing FAILS, so an
/// entry expires the day the defect it names is fixed. Add a row only
/// for a defect you have diagnosed and reported. Never to silence a new
/// regression — a challenger that newly breaks a structural invariant is
/// the finding this file exists to produce.
const XFAIL: &[(&str, &str, &str)] = &[
    // --- FINDING, 2026-09-06: `divider-rails` inverts the rail
    // convention on `common_emitter`'s base bias resistor.
    //
    // `R2` returns the base net `b` to ground, so the convention puts it
    // UNDER `Q1`. Under this arm the seed puts it above (Q1.b y=40.64,
    // R2.b y=36.83) and the SA leaves the two exactly level
    // (36.83/36.83), which is neither reading. Unique to this arm: the
    // other 24 registered arms all pass both stages.
    //
    // Diagnosis (reported, NOT fixed here — this is a test-coverage
    // change): `divider-rails` is the arm that drops the tap-degree-2
    // gate from `idioms::detect_dividers` without adding anything in its
    // place, so `[R1 R2]` — whose tap `b` is *loaded*, by Q1's base — is
    // detected as a divider and pinned into a stacked column that
    // straddles the transistor. `divider-rails-strict`, the arm that
    // keeps the degree-2 gate on top of the rail test so the predicate
    // only ever narrows, passes both stages; and `readable-v1` and every
    // arm downstream of it (the shipping default included) compose
    // `divider_tap_must_be_unloaded`, i.e. the strict reading. So the
    // defect is real and it is confined to the one arm that was
    // registered to measure the loose reading — which is what an
    // attribution arm is for.
    (
        "divider-rails",
        "rail::r2_below_q1[seed]",
        "the un-gated divider predicate matches `common_emitter`'s loaded-tap [R1 R2] and \
         stacks it across Q1, putting the base-to-ground resistor above the transistor",
    ),
    (
        "divider-rails",
        "rail::r2_below_q1[refined]",
        "same un-gated divider match as the seed case; the SA leaves R2 exactly level with \
         Q1's base rather than below it",
    ),
    // --- `y-sign` (ADR-30: graded NOT promotable, kept as an
    // instrument). It is the only arm that fails the *refined* stage of
    // the parallel-stub levelling; consistent with ADR-31's finding that
    // the corrected objective prefers the layout that routes worse, and
    // with 77% of its loss sitting on one fixture.
    (
        "y-sign",
        "rail::emitter_loads_share_a_row[refined]",
        "ADR-30: the page-frame objective re-scores the emitter stubs and the SA no longer \
         levels RE/CE; this arm is already graded NOT promotable",
    ),
    // --- `rail::rc_above_q1_collector[refined]`: the "SA drifts the
    // stub column ~3 cells off" defect, deferred long before any of
    // these arms existed (holding the column with a larger
    // `rail_stub_alignment` weight was measured and regressed Tier-1 V13
    // plus `opamp_inverting_real` V5 1 -> 3).
    //
    // It is REPAIRED on the shipping default and on 13 of 25 arms — the
    // ADR-40 DC-series column pins RC onto Q1's collector X so the SA
    // cannot drift it — which is why `rail_convention.rs`'s own test is
    // no longer `#[ignore]`d. The 11 arms below predate that
    // construction (or, in `y-sign`'s case, re-frame the objective under
    // it) and still drift.
    (
        "flow-seed",
        "rail::rc_above_q1_collector[refined]",
        "pre-DC-column arm: the SA drifts the collector-load stub off Q1's collector column",
    ),
    (
        "flow-seed-v2",
        "rail::rc_above_q1_collector[refined]",
        "pre-DC-column arm: the SA drifts the collector-load stub off Q1's collector column",
    ),
    (
        "flow-seed-v4",
        "rail::rc_above_q1_collector[refined]",
        "pre-DC-column arm: the SA drifts the collector-load stub off Q1's collector column",
    ),
    (
        "m3-signed-gate",
        "rail::rc_above_q1_collector[refined]",
        "pre-DC-column arm: the SA drifts the collector-load stub off Q1's collector column",
    ),
    (
        "m3-signed-full",
        "rail::rc_above_q1_collector[refined]",
        "pre-DC-column arm: the SA drifts the collector-load stub off Q1's collector column",
    ),
    (
        "m5-streams",
        "rail::rc_above_q1_collector[refined]",
        "pre-DC-column arm: the SA drifts the collector-load stub off Q1's collector column",
    ),
    (
        "divider-rails-strict",
        "rail::rc_above_q1_collector[refined]",
        "pre-DC-column arm: the SA drifts the collector-load stub off Q1's collector column",
    ),
    (
        "facing-trigger",
        "rail::rc_above_q1_collector[refined]",
        "pre-DC-column arm: the SA drifts the collector-load stub off Q1's collector column",
    ),
    (
        "terminal-series",
        "rail::rc_above_q1_collector[refined]",
        "pre-DC-column arm: the SA drifts the collector-load stub off Q1's collector column",
    ),
    (
        "signal-direction",
        "rail::rc_above_q1_collector[refined]",
        "pre-DC-column arm: the SA drifts the collector-load stub off Q1's collector column",
    ),
    (
        "y-sign",
        "rail::rc_above_q1_collector[refined]",
        "ADR-30 arm: the page-frame objective drifts the collector-load stub off Q1's column",
    ),
];

fn is_deferred(check: &str) -> bool {
    DEFERRED_ON_THE_DEFAULT.iter().any(|(c, _)| *c == check)
}

fn is_registered(arm: &str, check: &str) -> bool {
    XFAIL.iter().any(|(a, c, _)| *a == arm && *c == check)
}

fn reason(arm: &str, check: &str) -> &'static str {
    XFAIL
        .iter()
        .find(|(a, c, _)| *a == arm && *c == check)
        .map_or("<unregistered>", |(_, _, r)| *r)
}

/// Sweep every structural check under one arm.
fn sweep(arm_name: &str) {
    let placer = Placer::from_name(arm_name).unwrap_or_else(|| {
        panic!(
            "unregistered placer {arm_name}; known: {}",
            Placer::known_names()
        )
    });

    let mut failures = Vec::new();
    let mut unexpected_passes = Vec::new();

    for check in CHECKS {
        let outcome = structural::run_check(check, placer);
        if is_deferred(check.id) {
            // Not gated on any arm; report the direction of travel so a
            // promotion candidate's repairs are visible.
            match outcome {
                Some(msg) => eprintln!(
                    "deferred: [{arm_name}] {} fails as it does on the default — {msg}",
                    check.id
                ),
                None => eprintln!(
                    "deferred: [{arm_name}] {} PASSES — this arm repairs a defect the \
                     shipping default still has",
                    check.id
                ),
            }
            continue;
        }
        match (is_registered(arm_name, check.id), outcome) {
            (true, Some(msg)) => eprintln!(
                "xfail: [{arm_name}] {} failed as registered ({}) — {msg}",
                check.id,
                reason(arm_name, check.id),
            ),
            (true, None) => unexpected_passes.push(check.id),
            (false, Some(msg)) => failures.push(format!("[{arm_name}] {}: {msg}", check.id)),
            (false, None) => {}
        }
    }

    // A registry row is stale when the check it names no longer exists,
    // or when it has since been recorded as failing on the DEFAULT too
    // (in which case it belongs in `DEFERRED_ON_THE_DEFAULT`, which
    // gates no arm, not in a per-arm row that claims the arm is the
    // odd one out).
    let stale: Vec<&str> = XFAIL
        .iter()
        .filter(|(a, c, _)| {
            *a == arm_name && (!CHECKS.iter().any(|k| k.id == *c) || is_deferred(c))
        })
        .map(|(_, c, _)| *c)
        .collect();

    let mut report = String::new();
    if !failures.is_empty() {
        let _ = writeln!(
            report,
            "{} structural failure(s) under --placer={arm_name}:\n  {}",
            failures.len(),
            failures.join("\n  ")
        );
    }
    for id in unexpected_passes {
        let _ = writeln!(
            report,
            "UNEXPECTED PASS: [{arm_name}] {id} is registered in \
             tests/challenger_structural.rs XFAIL (\"{}\") but now PASSES. \
             DELETE that row so the arm is graded again.",
            reason(arm_name, id),
        );
    }
    for id in stale {
        let _ = writeln!(
            report,
            "STALE XFAIL: [{arm_name}] {id} names a check this sweep does not gate \
             (it no longer exists, or it is now listed in DEFERRED_ON_THE_DEFAULT). \
             DELETE that row.",
        );
    }
    assert!(report.is_empty(), "{report}");
}

/// One test per registered arm. The list is checked against
/// [`Placer::ALL`] by [`every_registered_arm_is_swept`], so registering
/// a new challenger without covering it here is a test failure, not a
/// silent hole — which is the whole point of the file.
macro_rules! arms {
    ($($ident:ident => $name:literal),* $(,)?) => {
        $(
            #[test]
            fn $ident() { sweep($name); }
        )*
        const SWEPT: &[&str] = &[$($name),*];
    };
}

arms! {
    arm_readable_v1              => "readable-v1",
    arm_flow_seed_v4             => "flow-seed-v4",
    arm_flow_seed                => "flow-seed",
    arm_champion                 => "champion",
    arm_m4_ydatum                => "m4-ydatum",
    arm_m3_signed_gate           => "m3-signed-gate",
    arm_m3_signed_full           => "m3-signed-full",
    arm_m5_streams               => "m5-streams",
    arm_flow_seed_v2             => "flow-seed-v2",
    arm_flow_seed_v3             => "flow-seed-v3",
    arm_divider_rails            => "divider-rails",
    arm_divider_rails_strict     => "divider-rails-strict",
    arm_facing_trigger           => "facing-trigger",
    arm_terminal_series          => "terminal-series",
    arm_terminal_series_divider  => "terminal-series-divider",
    arm_y_sign                   => "y-sign",
    arm_signal_direction         => "signal-direction",
    arm_dc_series_column         => "dc-series-column",
    arm_dc_series_column_pinned  => "dc-series-column-pinned",
    arm_conet_layer_collapse     => "conet-layer-collapse",
    arm_dc_column_node_stubs     => "dc-column-node-stubs",
    arm_column_stubs_conet       => "column-stubs-conet",
    arm_series_midspan           => "series-midspan",
    arm_chain_interior_pose      => "chain-interior-pose",
    arm_column_stubs_conet_chain => "column-stubs-conet-chain",
}

/// The coverage tripwire: registering a new `--placer` arm without a
/// sweep test is exactly the blindness ADR-40 recorded, so it fails
/// here rather than at the arm's promotion.
#[test]
fn every_registered_arm_is_swept() {
    let missing: Vec<&str> = Placer::ALL
        .iter()
        .map(|p| p.name())
        .filter(|n| !SWEPT.contains(n))
        .collect();
    assert!(
        missing.is_empty(),
        "newly registered placer arm(s) {missing:?} have no structural sweep test. \
         Add `arm_<name with '-' as '_'> => \"<name>\"` to the `arms!` list in \
         tests/challenger_structural.rs — an arm no test runs is the ADR-40 \
         challenger-blindness hole.",
    );
    let unknown: Vec<&&str> = SWEPT
        .iter()
        .filter(|n| Placer::from_name(n).is_none())
        .collect();
    assert!(
        unknown.is_empty(),
        "sweep list names unregistered placer(s) {unknown:?}; delete those rows",
    );
    // An XFAIL row for a de-registered arm is unreachable — `sweep` only
    // ever runs for arms that exist — so its staleness has to be caught
    // here rather than in the per-arm report.
    let orphaned: Vec<&str> = XFAIL
        .iter()
        .map(|(a, _, _)| *a)
        .filter(|a| Placer::from_name(a).is_none())
        .collect();
    assert!(
        orphaned.is_empty(),
        "XFAIL rows name unregistered placer(s) {orphaned:?}; delete those rows",
    );
}

/// The tripwire for [`DEFERRED_ON_THE_DEFAULT`].
///
/// Those rows exist because the *shipping default* fails the check, so
/// no challenger can be held to it. The day the default is fixed, the
/// row must go — and every arm starts being graded on it. `#[ignore]`
/// cannot say that (it is not a tripwire; see
/// `crates/spice2kicad/tests/common/xfail.rs`'s opening argument), which
/// is how `rail::rc_above_q1_collector[refined]` stayed ignored for a
/// promotion after the DC-series column had already repaired it.
#[test]
fn deferred_checks_are_still_red_on_the_default() {
    let mut fixed = Vec::new();
    for (id, why) in DEFERRED_ON_THE_DEFAULT {
        let check = CHECKS
            .iter()
            .find(|c| c.id == *id)
            .unwrap_or_else(|| panic!("DEFERRED_ON_THE_DEFAULT names unknown check {id}"));
        if structural::run_check(check, Placer::default()).is_none() {
            fixed.push(format!("{id} (\"{why}\")"));
        }
    }
    assert!(
        fixed.is_empty(),
        "{} deferred check(s) now PASS on the shipping default:\n  {}\n\
         The defect is fixed. DELETE the row(s) from DEFERRED_ON_THE_DEFAULT so every \
         registered arm is graded on the property, and un-`#[ignore]` the matching test in \
         rail_convention.rs / idiom_placement.rs.",
        fixed.len(),
        fixed.join("\n  "),
    );
}
