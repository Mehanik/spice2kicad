//! The placer-selection seam (ADR-23).
//!
//! The project has two questions to answer about a layout change, and
//! they need two different instruments:
//!
//! 1. *"Did this change break what we shipped?"* — answered by the
//!    per-fixture, zero-slack ratchets and `baseline_lock`.
//! 2. *"Is placer B better than placer A?"* — answered by the
//!    champion/challenger scoreboard, which permits sideways trades
//!    because two different placers land in two different global optima
//!    and neither dominates the other across ~165 correlated scalars.
//!
//! Question 2 needs the ability to run a *named* placer end-to-end and
//! measure the emitted geometry with the same verifiers. This module is
//! that name registry; `--placer=<name>` on the CLI selects one, and
//! [`Placer::Champion`] — today's placer, bit-for-bit — is the default,
//! so the flagless path is unchanged.
//!
//! **A challenger is not a licence to bypass a ratchet.** An ordinary
//! change still has to satisfy every per-fixture budget. The scoreboard
//! applies to whole-placer comparisons only; see `docs/layout-adr.md`
//! ADR-23.

/// A named placement engine.
///
/// Variants are *registered alternatives*, not tuning knobs: each one is
/// a whole seed strategy that the scoreboard can grade end-to-end.
/// [`Placer::Champion`] is the incumbent and the default; every other
/// variant is dead on the default path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Placer {
    /// The incumbent placer: `n`-scaled Y frame, Mid sub-rows as
    /// fractions of the Top↔Bot span, `pack_rows` re-centring the row
    /// stack on its total growth.
    #[default]
    Champion,
    /// ADR-19 Milestone 4 — the content-derived, `n`-independent Y
    /// datum. Band datums chain downward by *measured* content depth
    /// plus reach-derived clearance instead of by the element count;
    /// Top stacks upward and Bot downward (append-only band growth);
    /// `pack_rows` anchors row 0 instead of re-centring.
    ///
    /// Landed as `ed51164` and reverted (ADR-19 § "M4 reverted"). It is
    /// registered here as the scoreboard's first real challenger — the
    /// instrument's own acceptance test — not as a candidate for the
    /// default path.
    M4YDatum,
}

impl Placer {
    /// Every registered placer, champion first.
    pub const ALL: &'static [Self] = &[Self::Champion, Self::M4YDatum];

    /// The name accepted by `--placer` and printed by the scoreboard.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Champion => "champion",
            Self::M4YDatum => "m4-ydatum",
        }
    }

    /// One-line description, for `--help` and the scoreboard header.
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::Champion => "incumbent placer (default; n-scaled Y frame)",
            Self::M4YDatum => "ADR-19 M4: content-derived, n-independent Y datum",
        }
    }

    /// Look a placer up by the name `--placer` accepts.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|p| p.name() == name)
    }

    /// Comma-separated list of every registered name, for diagnostics.
    #[must_use]
    pub fn known_names() -> String {
        Self::ALL
            .iter()
            .map(|p| p.name())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::Placer;

    #[test]
    fn default_is_the_champion() {
        assert_eq!(Placer::default(), Placer::Champion);
        assert_eq!(Placer::default().name(), "champion");
    }

    #[test]
    fn every_registered_name_round_trips() {
        for &p in Placer::ALL {
            assert_eq!(Placer::from_name(p.name()), Some(p), "{}", p.name());
        }
        assert_eq!(Placer::from_name("no-such-placer"), None);
    }

    #[test]
    fn names_are_unique() {
        let mut names: Vec<&str> = Placer::ALL.iter().map(|p| p.name()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate placer name in Placer::ALL");
    }
}
