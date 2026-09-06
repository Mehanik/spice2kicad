//! Property tests for the **rail-direction drawing convention**, the
//! rule a reader uses to orient themselves on any analog schematic:
//!
//! > Signals connected to a positive supply go **up**; signals connected
//! > to ground go **down**; a negative rail goes lower still.
//!
//! These are position-only assertions over the public `Placement` (the
//! ADR-7 strategy), read from live pin geometry via `world_pin_mm`, so
//! they hold wherever the anchor devices finally land.
//!
//! # Why these exist
//!
//! The convention was encoded in `cost::rail_direction`, but **inverted**:
//! the term read `pin_extents_y`'s `(y_min, y_max)` return as
//! `(y_top, y_bot)`, calling the *largest* screen Y "top". Screen Y
//! increases downward, so the term pulled positive rails to the bottom of
//! the sheet and ground to the top — and at weight 200 over squared
//! millimetres it was ~97% of the whole objective, so it comfortably
//! overpowered the band terms that were pulling the right way. Every
//! part with a single rail connection ended up on the wrong side of the
//! device it serves. Nothing caught it, because the one test that
//! covered the term (`tests/cost.rs::rail_direction_power_above_zero_below`)
//! asserted the inverted expectation too.
//!
//! The properties below are the falsifiable form of the convention, so a
//! sign error cannot come back silently.
//!
//! # Where the assertions live
//!
//! In `tests/common/structural.rs`, not here. They are **structural**
//! properties in CLAUDE.md's constraints-vs-costs sense — yes/no
//! geometric facts with one correct answer — so every registered
//! `--placer` arm owes them, not just [`Placer::default`]. This file is
//! the default-placer view; `tests/challenger_structural.rs` sweeps the
//! same functions across every registered arm (ADR-40 follow-up,
//! "Challenger blindness"). Keeping one copy of each assertion is the
//! point: a duplicated one drifts, and then the arm sweep and the
//! default suite quietly grade different properties.

mod common;

use common::structural;
use spice_layout::Placer;

/// Panic with the check's own message if it fails on the shipping
/// default placer.
fn must(outcome: Result<(), String>) {
    if let Err(msg) = outcome {
        panic!("{msg}");
    }
}

// ---------------------------------------------------------------------------
// common_emitter — the fixture the defects were reported on
// ---------------------------------------------------------------------------

/// Defect 1: `R2` connects the base to ground, so it belongs UNDER `Q1`,
/// not above it. Measured on the base net `b`, which both share.
#[test]
fn common_emitter_r2_below_q1_seed() {
    must(structural::ce_r2_below_q1(Placer::default(), false));
}

#[test]
fn common_emitter_r2_below_q1_refined() {
    must(structural::ce_r2_below_q1(Placer::default(), true));
}

/// Defect 2 (part 1): `RE` and `CE` both return the emitter to ground, so
/// both belong UNDER `Q1`.
#[test]
fn common_emitter_emitter_loads_below_q1_seed() {
    must(structural::ce_emitter_loads_below_q1(
        Placer::default(),
        false,
    ));
}

#[test]
fn common_emitter_emitter_loads_below_q1_refined() {
    must(structural::ce_emitter_loads_below_q1(
        Placer::default(),
        true,
    ));
}

// Defect 2 (part 2): `RE` and `CE` are in parallel across the same two
// nets, so they read as a pair only if they sit at the SAME level.
//
// Note this is the *horizontal* reading of "aligned" — side by side on a
// shared row. The vertical single-column stack that
// `idiom_placement.rs`'s deferred Idiom 1 asserts is deliberately NOT
// what this expects: a same-column stack shorts one shared net past the
// other's pin (V11, Tier 0) under the orientation wall, which is exactly
// why that idiom is still deferred.
//
// RED. The refined case passes; the SEED does not level the pair,
// because `apply_rail_stub_columns` adjusts X only. Levelling Y within a
// stub group was implemented and MEASURED: on its own it is fine, but
// together with the `bands.rs` VertPref switch it regressed Tier-1
// `v13_labels_no_mutual_overlap`, `v13_labels_dont_overlap_symbol_body`
// and `no_symbol_symbol_overlap_across_fixtures`. Both were reverted
// under the tier rule. Re-land the levelling alone and re-measure.
#[test]
#[ignore = "seed does not level parallel stubs; Y-levelling reverted after a Tier-1 V13 regression"]
fn common_emitter_emitter_loads_share_a_row_seed() {
    must(structural::ce_emitter_loads_share_a_row(
        Placer::default(),
        false,
    ));
}

#[test]
fn common_emitter_emitter_loads_share_a_row_refined() {
    must(structural::ce_emitter_loads_share_a_row(
        Placer::default(),
        true,
    ));
}

/// Defect 3: `RC` pulls the collector UP to VCC, so it belongs above
/// `Q1`, in the collector's own column.
#[test]
fn common_emitter_rc_above_q1_collector_seed() {
    must(structural::ce_rc_above_q1_collector(
        Placer::default(),
        false,
    ));
}

// Was `#[ignore]`d as "the SA drifts the stub column ~3 cells off". It
// **passes** on the shipping default, and has since the ADR-40
// promotion pinned the DC-series column: the pin-anchored column holds
// RC on Q1's collector X through the SA. Nothing announced that,
// because `#[ignore]` is not a tripwire — which is precisely the
// argument `crates/spice2kicad/tests/common/xfail.rs` opens with. The
// arm sweep found it (13 of 25 registered arms pass this check, the
// default among them); it is un-ignored here so a re-drift is a failure
// rather than a silence, and the 11 arms that still fail it are
// registered as expected failures in `challenger_structural.rs`.
#[test]
fn common_emitter_rc_above_q1_collector_refined() {
    must(structural::ce_rc_above_q1_collector(
        Placer::default(),
        true,
    ));
}

// ---------------------------------------------------------------------------
// named_rails — the convention must key off `*@power=`, not net NAMES
// ---------------------------------------------------------------------------

// The whole point of the `named_rails` fixture: its rails are called
// `p5` and `n5`, which match none of the canonical supply names
// (`vcc` / `vdd` / `gnd` / `vee` / …). The only thing that can classify
// them is `;@ power=+5V` / `;@ power=-5V`.
//
// RED. The refined case passes. The SEED puts the `-5V` stub UP, because
// `bands::assign_y_bands` bands off `NetClass` (a `*@power=-5V` net is
// `NetClass::Power` -> Top band) while `cost::rail_direction` bands off
// `vertical_prefs` (negative rail -> Down). Switching `assign_y_bands` to
// `vertical_prefs` fixes exactly this and is the right generalisation,
// but MEASURED it regressed Tier-1 V13 + symbol-overlap, so it was
// reverted pending sign-off.
#[test]
#[ignore = "bands.rs bands off NetClass, so a negative rail seeds UP; the VertPref switch regressed Tier-1"]
fn named_rails_convention_seed() {
    must(structural::named_rails_convention(Placer::default(), false));
}

#[test]
fn named_rails_convention_refined() {
    must(structural::named_rails_convention(Placer::default(), true));
}

/// The generalisation, stated directly on the classifier rather than on a
/// placement: a `*@power=`-tagged rail gets a vertical preference from its
/// TAG, and the sign of the tagged voltage decides the direction.
///
/// Placer-independent by construction — it never runs a placer — so it
/// stays here rather than in the cross-arm sweep.
#[test]
fn named_rail_polarity_comes_from_the_power_tag() {
    use spice_diagnostics::FileId;
    use spice_layout::net_class::{VertPref, vertical_prefs};
    use spice_policy::check;

    let src = "test\n\
               VPOS p5 0 DC  5 ;@ power=+5V\n\
               VNEG n5 0 DC -5 ;@ power=-5V\n\
               RPU p5 out 10k\n\
               RPD out n5 10k\n\
               .end\n";
    let parsed = spice_parser::parse(src, FileId(0))
        .expect("parse failed")
        .netlist;
    let resolved = spice_resolve::resolve(&parsed, structural::library()).expect("resolve failed");
    let (checked, _w) = check(resolved).expect("policy check failed");
    let prefs = vertical_prefs(&checked);

    assert_eq!(
        prefs.get("p5"),
        Some(&VertPref::Up),
        "`;@ power=+5V` must make `p5` an up-rail despite its non-canonical name"
    );
    assert_eq!(
        prefs.get("n5"),
        Some(&VertPref::Down),
        "`;@ power=-5V` must make `n5` a down-rail despite its non-canonical name"
    );
    assert_eq!(prefs.get("0"), Some(&VertPref::Down), "ground goes down");
    assert_eq!(prefs.get("out"), None, "a signal net has no preference");
}
