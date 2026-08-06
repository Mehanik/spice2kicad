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
