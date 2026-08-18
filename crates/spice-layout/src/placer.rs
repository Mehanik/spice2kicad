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
//! that name registry; `--placer=<name>` on the CLI selects one.
//!
//! **[`Placer::FlowSeed`] is the default since the ADR-23 promotion**
//! (2026-08-18): it was graded PROMOTABLE against the incumbent and the
//! ratchets plus `baseline_lock` were regenerated at its geometry.
//! [`Placer::Champion`] stays registered as the **control arm** — it
//! must remain runnable, because every future challenger is graded
//! against the new default and the old default stays available for A/B.
//!
//! **A challenger is not a licence to bypass a ratchet.** An ordinary
//! change still has to satisfy every per-fixture budget. The scoreboard
//! applies to whole-placer comparisons only; see `docs/layout-adr.md`
//! ADR-23.

/// A named placement engine.
///
/// Variants are *registered alternatives*, not tuning knobs: each one is
/// a whole seed strategy that the scoreboard can grade end-to-end.
/// [`Placer::FlowSeed`] is the default since the ADR-23 promotion;
/// [`Placer::Champion`] is the retained control arm and every other
/// variant is dead on the default path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Placer {
    /// The former default, retained as the scoreboard's **control
    /// arm**: `n`-scaled Y frame, Mid sub-rows as fractions of the
    /// Top↔Bot span, `pack_rows` re-centring the row stack on its total
    /// growth, and an X layering that roots at every rail-touching
    /// element (so X measures hops from the nearest power rail).
    ///
    /// Superseded as the default by [`Placer::FlowSeed`] (ADR-23
    /// promotion, 2026-08-18). It is deliberately kept runnable: A/B
    /// against the previous architecture is the only way to attribute a
    /// future regression to the promotion rather than to the change
    /// under test.
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
    /// ADR-19 Milestone 3, ablation **B** — the *pure* signed-footprint
    /// gate. `solver::anneal::symbol_overlap_count` reserves the signed
    /// `footprint::element_footprint` (body ∪ pins ∪ rail glyph) instead
    /// of the symmetric `.abs()` halo. The property-text union is absent
    /// and `legalize` is untouched.
    ///
    /// Preserved on `wip/adr19-m3-signed-gate` (`7896f22`) and rejected
    /// under the per-fixture rule (ADR-19 § "M3 blocked"). Registered
    /// here as a graded challenger only.
    M3SignedGate,
    /// ADR-19 Milestone 3, the **full** wiring (`7896f22`'s tree) —
    /// ablation B plus the property-text union in the SA gate plus
    /// `legalize`'s roomy preference reading the signed footprint.
    M3SignedFull,
    /// ADR-19 Milestone 5′ — **SA trajectory decoupling**. The anneal
    /// draws every proposal from a private per-element RNG stream keyed
    /// on the element's refdes, swept deterministically, instead of from
    /// one global stream whose draw order is netlist-position-dependent.
    ///
    /// Attempted and reverted (ADR-19 § "M5′"): it bought no locality
    /// and destroyed the SA's bend-finding. Registered here as a graded
    /// challenger only.
    M5Streams,
    /// **Flow-faithful skeleton** — the X "layer" measures depth along
    /// the *signal path*, not hops from the nearest power rail.
    ///
    /// `layers::no_source_fallback` is the path every realistic fixture
    /// takes (a stimulus tagged `;@ ignore` leaves `sources` empty), and
    /// its root set is `input_root(i) || touches_power(i)` — so **every
    /// rail-touching stub is a layer-0 root**. That functional saturates
    /// at ~2 layers in any biased amplifier regardless of stage count:
    /// on `two_stage_amp` the chain `in→b1→c1→b2→c2→out` needs five
    /// columns and gets `{0,1,1,1,3}`, dropping Q1, the coupling cap and
    /// Q2 into one column that row-packing then stacks vertically.
    /// `common_emitter` draws well only because for a *single* stage
    /// rail-hop depth and signal depth coincide by accident.
    ///
    /// This variant changes three things, all inside the fallback, all
    /// layering-only (no spacing constant, band datum or SA weight moves):
    ///
    /// 1. **Roots are signal-flow sources only** — declared `*@port`
    ///    inputs and leaf-input nets, still filtered by ADR-18's
    ///    "boundary not interior" pass-through test. Never a rail stub.
    /// 2. **Rail stubs are followers**: after the BFS, a stub takes the
    ///    layer of the shallowest non-stub element on its signal net, so
    ///    a collector load lands in its transistor's column instead of
    ///    seeding column 0.
    /// 3. **Within-bucket ordering by neighbour barycenter** (the one
    ///    Sugiyama phase the placer skips) instead of element index.
    ///
    /// A circuit with no signal-flow root at all — `wien_bridge_osc` is
    /// a pure cycle with no input — falls back to the champion's
    /// rail-rooted policy **verbatim** and is byte-identical on both
    /// sides. That fallback is not a leftover: it is the defined
    /// behaviour for rootless circuits, and the promotion's cheapest
    /// integrity check is that `diff_pair`, `multivibrator` and
    /// `wien_bridge_osc` emit byte-identically across the swap.
    ///
    /// **The default placer since the ADR-23 promotion (2026-08-18).**
    #[default]
    FlowSeed,
}

impl Placer {
    /// Every registered placer, the default first and the retained
    /// control arm second.
    pub const ALL: &'static [Self] = &[
        Self::FlowSeed,
        Self::Champion,
        Self::M4YDatum,
        Self::M3SignedGate,
        Self::M3SignedFull,
        Self::M5Streams,
    ];

    /// The name accepted by `--placer` and printed by the scoreboard.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Champion => "champion",
            Self::M4YDatum => "m4-ydatum",
            Self::M3SignedGate => "m3-signed-gate",
            Self::M3SignedFull => "m3-signed-full",
            Self::M5Streams => "m5-streams",
            Self::FlowSeed => "flow-seed",
        }
    }

    /// One-line description, for `--help` and the scoreboard header.
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::Champion => {
                "the pre-promotion placer, retained as the scoreboard control arm \
                 (n-scaled Y frame; X = hops from the nearest power rail)"
            }
            Self::M4YDatum => "ADR-19 M4: content-derived, n-independent Y datum",
            Self::M3SignedGate => "ADR-19 M3 ablation B: signed footprint in the SA overlap gate",
            Self::M3SignedFull => {
                "ADR-19 M3 full wiring: signed gate + property text + signed legalize"
            }
            Self::M5Streams => "ADR-19 M5': per-refdes SA proposal streams, deterministic sweep",
            Self::FlowSeed => {
                "default: flow-faithful skeleton \
                 (signal-flow roots, stub followers, barycenter order)"
            }
        }
    }

    /// M3: does the SA overlap gate reserve the *signed* footprint
    /// rather than the symmetric `.abs()` halo?
    #[must_use]
    pub fn m3_signed_gate(self) -> bool {
        matches!(self, Self::M3SignedGate | Self::M3SignedFull)
    }

    /// M3: does that signed reservation also union the drawn property
    /// text? (The single edit separating ablation B from `full`.)
    #[must_use]
    pub fn m3_property_text(self) -> bool {
        matches!(self, Self::M3SignedFull)
    }

    /// M3: does `legalize`'s *roomy* shove preference read the signed
    /// footprint instead of `world_extent_with_glyphs`?
    #[must_use]
    pub fn m3_signed_legalize(self) -> bool {
        matches!(self, Self::M3SignedFull)
    }

    /// M5′: does the anneal draw proposals from private per-refdes RNG
    /// streams on a deterministic sweep, instead of one global stream?
    #[must_use]
    pub fn m5_element_streams(self) -> bool {
        matches!(self, Self::M5Streams)
    }

    /// Flow-seed: does the no-source X-layering root at signal-flow
    /// sources (and demote rail stubs to followers) instead of rooting
    /// at every rail-touching element?
    #[must_use]
    pub fn flow_seed_layering(self) -> bool {
        matches!(self, Self::FlowSeed)
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
    fn default_is_the_flow_seed_placer() {
        assert_eq!(Placer::default(), Placer::FlowSeed);
        assert_eq!(Placer::default().name(), "flow-seed");
    }

    /// The promoted default did not retire the control arm. ADR-23's
    /// promotion rule grades every future challenger against the new
    /// default, and the old default has to stay runnable for the A/B
    /// that attributes a regression to the promotion or to the change
    /// under test.
    #[test]
    fn the_champion_control_arm_stays_registered() {
        assert_eq!(Placer::from_name("champion"), Some(Placer::Champion));
        assert!(Placer::ALL.contains(&Placer::Champion));
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
