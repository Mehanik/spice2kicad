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
    // --- F0 benchmark expansion (v0.2 roadmap) ------------------------
    // `rc_phase_shift` is denser than any v0.1 fixture and re-exposes
    // Tier-1 placer / decoration defects that were already deferred
    // before F0. They are recorded here, not hidden: each entry names
    // the defect and expires automatically when it is fixed.
    (
        "v13_power_glyph_value_text_clear_of_bodies_and_pintext",
        "rc_phase_shift",
        "deferred V13(6a) decoration nudge: #PWR2's GND value text clips the RB body",
    ),
    (
        "no_power_glyph_foreign_body_overlap_across_fixtures",
        "rc_phase_shift",
        "deferred V14 issue-[3]: a GND glyph body clips the foreign RB body (ADR-14 known scope limit)",
    ),
    (
        "v14_rail_pin_faces_rail",
        "rc_phase_shift",
        "deferred V14/R-5 rail-pin defect: #PWR6 (+12V) sits below its host RB's body centre",
    ),
    (
        "rendered_text_does_not_overlap_across_fixtures",
        "rc_phase_shift",
        "deferred V13 text-nudge defect: pin number \"1\" overlaps the \"b\" net label in the rendered ink",
    ),
    (
        "rails_correctly_ordered_across_fixtures",
        "rc_phase_shift",
        "deferred rail-band ordering defect: RB (+12V-only) lands below a ground-only element",
    ),
    // --- F0, second fixture: `two_stage_amp` --------------------------
    // Promoted out of `tests/f0_defects.rs` once the phase-4.5 runtime
    // defect that held it there was fixed (112 s unoptimised -> ~1.0 s).
    // It is denser than `rc_phase_shift` (17 graded elements, two
    // cascaded stages on one rail) and re-exposes the SAME deferred
    // decoration and channel-router defects, plus one it is the first
    // fixture to reach. Every entry is a tripwire that expires the day
    // its defect is fixed; none is a budget.
    //
    // NOTE FOR THE OWNER: the first entry below is a **Tier-0-classified**
    // metric and is the one judgement call in this promotion. The
    // emitted schematic is electrically CORRECT today — the ADR-22
    // partition certificate is clean, V11 pin coincidence is clean, ERC
    // reports zero errors, and `kicad-cli sch export netlist` reproduces
    // the source netlist exactly (8 distinct nets, no merge). What the
    // verifier flags is the *latent* hazard its own doc comment
    // describes: two different-net trunks sharing a collinear run, kept
    // distinct only for want of a junction dot. It is the documented
    // v0.2 channel-router wall — the same class `multivibrator` and
    // `opamp_definition_level` once carried in
    // `CROSS_NET_V02_ESCALATIONS`, both since resolved. Registered here
    // rather than given budget headroom, because Tier 0 is never traded
    // and a tripwire is the only exclusion that cannot rot.
    (
        "no_cross_net_collinear_wire_overlap",
        "two_stage_amp",
        "deferred v0.2 channel-router wall (Tier-0 LATENT short, needs owner review): the b2 and c2 \
         trunks share a collinear run at x=57.15, and the c2/e2 trunks at y=87.63 — no junction \
         merges them today (ERC clean, KiCad netlist exact), but the router cannot separate them",
    ),
    (
        "v13_labels_dont_overlap_property_text",
        "two_stage_amp",
        "deferred V13(2) decoration nudge: the `b1` net label clips Q2's Value text",
    ),
    (
        "v13_property_text_no_mutual_overlap",
        "two_stage_amp",
        "deferred V13(4) decoration nudge: RC2.Reference clips #PWR10's rail text, CE2.Value clips #PWR6's",
    ),
    (
        "v13_power_glyph_value_text_clear_of_bodies_and_pintext",
        "two_stage_amp",
        "deferred V13(6a) decoration nudge: #PWR2's and #PWR4's GND value text clip the CE1/RE1 bodies",
    ),
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
