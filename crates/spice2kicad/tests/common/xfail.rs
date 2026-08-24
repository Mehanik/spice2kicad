//! Expected-failure registry for the fixture-enumerating verifiers.
//!
//! # Why this exists
//!
//! A fixture that re-exposes a *known, deferred* placer defect cannot be
//! graded by the verifier that catches it — but the two obvious ways to
//! skip it are both silent:
//!
//!   * a bare `continue` inside the fixture loop reports the test as
//!     **passed**, so the tally says "768 passed" whether the fixture is
//!     graded or not; and
//!   * `#[ignore]` removes the whole test, not one fixture, and neither
//!     mechanism notices when the defect is *fixed* — the exclusion
//!     survives forever and the fixture is never re-graded.
//!
//! This registry is the missing tripwire. A registered `(test, fixture)`
//! pair is excluded from the assertion, **and the test FAILS if that pair
//! starts passing**, telling the reader to delete the entry. Deferred
//! exclusions therefore expire the moment the defect they name is fixed.
//!
//! # Using it
//!
//! Replace an in-loop `assert!` with a per-fixture `Option<String>`
//! failure and hand it to the guard:
//!
//! ```ignore
//! let mut xf = xfail::Guard::new("rails_correctly_ordered_across_fixtures");
//! for (name, path) in fixtures() {
//!     // …measure…
//!     xf.record(name, (!ok).then(|| format!("{name}: rails not ordered …")));
//! }
//! xf.finish();
//! ```
//!
//! `finish()` panics on either a genuine failure of an *unregistered*
//! fixture or an unexpected pass of a registered one, so the guard is a
//! strict superset of the assertion it replaces.
//!
//! # Adding an entry
//!
//! Add a row to [`XFAIL`] naming the deferred defect in one line. Do not
//! register a fixture to make a *new* regression go away — this registry
//! is for pre-existing deferred defects that a new fixture re-exposes.
//! Existing v0.1 fixtures are never registered; every one of them stays
//! fully graded.

/// `(test name, fixture name, one-line reason naming the deferred defect)`.
///
/// Keep it greppable: the test name is the exact `fn` name of the
/// verifier, the fixture name the `.cir` stem.
const XFAIL: &[(&str, &str, &str)] = &[
    // EXPIRED 2026-08 by the rail-stub SIDE fix in
    // `idioms::apply_series_horizontal` (four entries deleted, not
    // relaxed): the pass re-columned every downstream shunt BELOW its
    // node with the rail pin forced screen-down — a helper written for
    // ground and never parameterised on `RailStub::side` — so a
    // positive-supply bias resistor was pinned upside-down with its
    // `+12V` glyph under the body. That accounted for
    // `v14_rail_pin_faces_rail` on BOTH `rc_phase_shift` and
    // `shunt_feedback_amp` (the R-5 residual ADR-20 diagnosed), the
    // V14 [3] glyph/foreign-body overlap on `rc_phase_shift`, and its
    // rail-band ordering defect. All four now grade normally.
    //
    // --- F0 benchmark expansion (v0.2 roadmap) ------------------------
    // `rc_phase_shift` is denser than any v0.1 fixture and re-exposes
    // Tier-1 placer / decoration defects that were already deferred
    // before F0. They are recorded here, not hidden: each entry names
    // the defect and expires automatically when it is fixed.
    // EXPIRED 2026-08-18 by the ADR-23 PROMOTION of `--placer=flow-seed`
    // to the default. THREE entries deleted, all on `two_stage_amp`, all
    // reported by their own tripwires as UNEXPECTED PASS:
    //
    //   * `no_cross_net_collinear_wire_overlap` — the **Tier-0** entry,
    //     the b2/c2 collinear run at x=57.15 and the c2/e2 run at
    //     y=87.63. It was registered as a "v0.2 channel-router wall".
    //     It was not a router defect at all: the two trunks shared a
    //     column because the rail-hop layering collapsed
    //     `in->b1->c1->b2->c2->out` into three columns. Give the chain
    //     its five columns and the trunks have nothing to share.
    //   * `v13_labels_dont_overlap_property_text` (the `b1` label over
    //     Q2's Value) and
    //   * `v13_power_glyph_value_text_clear_of_bodies_and_pintext`
    //     (#PWR4's GND value text over RE1) — both registered as
    //     *decoration nudge* defects. Also layering.
    //
    // That is the D9 finding landing for real: **a defect attributed to
    // a downstream stage can be a symptom of the skeleton**, and a
    // deferral written against the wrong stage never expires on its own.
    // Three of the eight D9 predicted are here; the other five had
    // already expired to ADR-24/ADR-26 before the promotion ran.
    // --- F2: the second benchmark wave (v0.2 roadmap) -----------------
    // Four new fixtures — `cascode_amp`, `lc_ladder_lpf`,
    // `sallen_key_lpf`, `wien_bridge_osc` — chosen because the current
    // placer draws them BADLY, so a better placer has room to win (the
    // ADR-23 D7 finding: with one Tier-1 defect left in the whole suite,
    // "Tier 1 improved" had degenerated into "did you perturb
    // `rc_phase_shift`?").
    //
    // Every one of them is Tier-0 CLEAN — ADR-22 partition certificate,
    // V11 pin coincidence, V11 wire/label-on-foreign-pin, symbol overlap,
    // cross-net collinear overlap and ERC all measure 0 on all four. The
    // entries below are all Tier-1 *decoration/placer* defects that were
    // deferred long before F2; not one of them is a new regression, and
    // not one is a budget. `sallen_key_lpf` needs no entry at all.
    //
    // Three of them expired together — `v13_labels_clear_pin_text`
    // [cascode_amp] and `v13_property_text_no_mutual_overlap`
    // [lc_ladder_lpf, wien_bridge_osc] — when the pin-text and
    // sheet-port-name models were corrected to the side KiCad actually
    // draws on, giving the decoration nudge passes a true picture of
    // what they were dodging.
    (
        "no_power_glyph_foreign_body_overlap_across_fixtures",
        "wien_bridge_osc",
        "deferred V14 issue-[3]: two power-glyph bodies clip foreign bodies (ADR-14 known scope \
         limit — the same defect `rc_phase_shift` carries, here twice over)",
    ),
    // --- F3: the two fixtures promoted out of `tests/f0_defects.rs` by
    // the ADR-24 Tier-0 router fix — `sallen_key_driven` and
    // `shunt_feedback_amp`.
    //
    // Both are now Tier-0 CLEAN: ADR-22 partition certificate, V11 pin
    // coincidence, V11 wire/label-on-foreign-pin, symbol overlap,
    // cross-net collinear overlap and ERC all measure 0 on both. The
    // entries below are Tier-1 *decoration / placer* defects that were
    // deferred long before this work — each is the SAME defect an
    // existing fixture already carries an entry for, not a new one.
    // `sallen_key_driven`'s rendered-ink entry expired with the pin-text
    // / sheet-port-name model correction; its model-side half then
    // expired too — see below.
    //
    // EXPIRED 2026-08-24 by the SECOND ADR-23 PROMOTION
    // (`--placer=flow-seed-v4` becomes the default). ONE entry deleted,
    // reported by its own tripwire as UNEXPECTED PASS:
    //
    //   * `v13_property_text_no_mutual_overlap` [sallen_key_driven] —
    //     "RA's Reference and Value both clip #PWR5's rail text … RA is
    //     boxed in on every candidate anchor, so the nudge keeps its
    //     least-bad one". It was registered as a *decoration nudge*
    //     defect. It was a ROOT-POLICY defect: `sallen_key_driven` draws
    //     its stimulus, so the old depth map came back EMPTY and
    //     `apply_series_horizontal` declined the whole circuit, leaving
    //     RA boxed in. Give the fixture a real signal root and RA is not
    //     boxed in, so the nudge has somewhere to go — `v13.4_text_mutual`
    //     falls 2 -> 0.
    //
    // That is D9's finding landing a second time, on a second stage: **a
    // defect attributed to a downstream stage can be a symptom of the
    // skeleton, and a deferral written against the wrong stage never
    // expires on its own.**
    // --- REGRESSIONS INTRODUCED BY THE ADR-23 PROMOTION of
    // `--placer=flow-seed` to the default (owner-approved, 2026-08-18).
    //
    // Read this block as an owner-facing report, not as housekeeping.
    // The registry's own rule above says "do not register a fixture to
    // make a NEW regression go away", and these two ARE new. They are
    // registered anyway, deliberately and visibly, because the
    // alternative mechanisms are worse: a budget rise would hide the
    // count inside a number that only ratchets, and `#[ignore]` would
    // drop the whole verifier. A tripwire is the only exclusion that
    // announces itself the day it is fixed. **The owner approved a
    // whole-placer promotion, not these two specific losses** — both are
    // listed in the promotion's commit message and in ADR-23's promotion
    // section with fixture and magnitude, for exactly that reason.
    //
    // Both were INVISIBLE to the scoreboard that graded the promotion:
    // neither verifier reported to the measurement sink. Both now do
    // (`v13.9_foreign_over_glyph`, `p11.cache_out_of_step`), so no
    // future placer comparison can be blind to them in the same way.
    (
        "no_power_glyph_foreign_body_overlap_across_fixtures",
        "sallen_key_lpf",
        "TIER-1 REGRESSION introduced by the ADR-23 flow-seed promotion: #PWR4's GND glyph (host \
         X1) now clips C1's body (0 -> 1). This one WAS visible on the scoreboard — it is the \
         single +1.00 Tier-1 cell in the promotion's table, weighed against -2.00 and accepted by \
         the owner. Same deferred issue-[3] class `wien_bridge_osc` carries directly below (ADR-14 \
         known scope limit: the SA reserves the glyph footprint hard only for oversized-involving \
         pairs, and X1's opamp triangle is the oversized body here)",
    ),
    (
        "no_foreign_label_or_wire_over_power_glyph_body",
        "named_rails",
        "TIER-1 REGRESSION introduced by the ADR-23 flow-seed promotion: the `in` global label now \
         overlaps the `n5` (-5V) rail glyph body (0 -> 1). Decoration-fixable exactly as this \
         verifier's own budget doc says — the label-nudge pass does not treat power-glyph bodies \
         as obstacles — so it is one fixture, one label, and it expires the day that pass learns \
         about glyphs",
    ),
    // --- REGRESSION INTRODUCED BY THE SECOND ADR-23 PROMOTION of
    // `--placer=flow-seed-v4` to the default (owner-authorised,
    // 2026-08-24).
    //
    // Read this as an owner-facing report, exactly like the block above.
    // The owner authorised a **promotion**, not this specific Tier-1
    // loss. It is the single `+1.00` Tier-1 cell on the promotion's
    // table, weighed against the `-2.00` the same fixture gives back
    // (`v13.4_text_mutual` 2 -> 0), for a net Tier-1 of -1.00. It was
    // VISIBLE on the scoreboard before the decision, not discovered
    // after it.
    //
    // Registered as a tripwire rather than given budget headroom, for
    // the reason the block above states: a budget hides a count inside a
    // number that only ratchets, and `#[ignore]` would drop the whole
    // verifier. A tripwire is the only exclusion that announces itself
    // the day it is fixed.
    (
        "no_foreign_label_or_wire_over_power_glyph_body",
        "sallen_key_driven",
        "TIER-1 REGRESSION introduced by the ADR-23 flow-seed-v4 promotion: the `out` net's wire \
         (77.47,45.72)->(101.60,45.72) now crosses the `VEE` rail-glyph body (0 -> 1). Same \
         deferred class as the `named_rails` entry directly above and fixable in the same place — \
         this verifier's own budget doc records that the decoration pass does not treat \
         power-glyph bodies as obstacles; here it is the ROUTER rather than the label-nudge that \
         needs to learn it. One fixture, one wire, and it expires the day that pass learns about \
         glyphs",
    ),
    // R-5, and the fixture ADR-20 named it on. ADR-20 concluded that R-5
    // was what made `shunt_feedback_amp` UNCONVERTIBLE, escalating it
    // from Tier-1 aesthetics to Tier-0 correctness. ADR-24 shows that
    // conclusion was wrong: the fixture converts with R-5 untouched,
    // because the Tier-0 failure was the router fragmenting a Steiner
    // tree, not the glyph hanging into the channel. R-5 itself is
    // unchanged, still owner-gated, and still exactly this: a Tier-1
    // aesthetic defect, recorded here as a tripwire that expires the day
    // it is fixed.
];

/// True when `(test, fixture)` is a registered expected failure.
pub fn is_registered(test: &str, fixture: &str) -> bool {
    XFAIL.iter().any(|(t, f, _)| *t == test && *f == fixture)
}

fn reason(test: &str, fixture: &str) -> &'static str {
    XFAIL
        .iter()
        .find(|(t, f, _)| *t == test && *f == fixture)
        .map_or("<unregistered>", |(_, _, r)| *r)
}

/// Collects per-fixture outcomes for one verifier, applying [`XFAIL`].
///
/// Deliberately collect-all rather than fail-fast: a hard `assert!`
/// inside a fixture loop aborts at the first offender, which hides how
/// many fixtures are actually affected and makes an exclusion look
/// broader than it needs to be.
pub struct Guard {
    test: &'static str,
    /// Genuine failures of unregistered fixtures.
    failures: Vec<String>,
    /// Registered fixtures that unexpectedly passed.
    unexpected_passes: Vec<String>,
    /// Registered fixtures that were seen at all (so a stale entry
    /// naming a fixture the test no longer enumerates is also caught).
    seen: Vec<String>,
}

impl Guard {
    pub fn new(test: &'static str) -> Self {
        Self {
            test,
            failures: Vec::new(),
            unexpected_passes: Vec::new(),
            seen: Vec::new(),
        }
    }

    /// Record one fixture's outcome. `failure` is `None` when the fixture
    /// satisfied the invariant, `Some(msg)` when it did not.
    pub fn record(&mut self, fixture: &str, failure: Option<String>) {
        if is_registered(self.test, fixture) {
            self.seen.push(fixture.to_string());
            match failure {
                None => self.unexpected_passes.push(fixture.to_string()),
                Some(msg) => eprintln!(
                    "xfail: {}[{fixture}] failed as registered ({}) — {msg}",
                    self.test,
                    reason(self.test, fixture),
                ),
            }
        } else if let Some(msg) = failure {
            self.failures.push(msg);
        }
    }

    /// Panic if any unregistered fixture failed, or any registered
    /// fixture passed / was never enumerated.
    pub fn finish(self) {
        use std::fmt::Write as _;

        let mut report = String::new();
        if !self.failures.is_empty() {
            let _ = writeln!(
                report,
                "{} failure(s):\n  {}",
                self.failures.len(),
                self.failures.join("\n  ")
            );
        }
        for fixture in &self.unexpected_passes {
            let _ = writeln!(
                report,
                "UNEXPECTED PASS: {}[{fixture}] is registered in tests/common/xfail.rs \
                 (\"{}\") but now PASSES. The deferred defect is fixed — DELETE that \
                 registry entry so the fixture is graded again.",
                self.test,
                reason(self.test, fixture),
            );
        }
        let stale: Vec<&str> = XFAIL
            .iter()
            .filter(|(t, f, _)| *t == self.test && !self.seen.iter().any(|s| s == f))
            .map(|(_, f, _)| *f)
            .collect();
        for fixture in stale {
            let _ = writeln!(
                report,
                "STALE XFAIL: {}[{fixture}] is registered in tests/common/xfail.rs but the \
                 test never enumerated that fixture. DELETE the registry entry.",
                self.test,
            );
        }
        assert!(report.is_empty(), "{report}");
    }
}
