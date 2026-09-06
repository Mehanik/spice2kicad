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
//! **[`Placer::FlowSeedV4`] is the default since the second ADR-23
//! promotion** (2026-08-24): it was graded PROMOTABLE against
//! [`Placer::FlowSeed`] and the ratchets plus `baseline_lock` were
//! regenerated at its geometry. **Two** control arms stay registered and
//! runnable — [`Placer::FlowSeed`], the placer it replaced, and
//! [`Placer::Champion`], the one *that* replaced — because every future
//! challenger is graded against the new default and A/B against a
//! previous architecture is what attributes a regression to a promotion
//! rather than to the change under test.
//!
//! **A challenger is not a licence to bypass a ratchet.** An ordinary
//! change still has to satisfy every per-fixture budget. The scoreboard
//! applies to whole-placer comparisons only; see `docs/layout-adr.md`
//! ADR-23.

/// A named placement engine.
///
/// Variants are *registered alternatives*, not tuning knobs: each one is
/// a whole seed strategy that the scoreboard can grade end-to-end.
/// [`Placer::FlowSeedV4`] is the default since the second ADR-23
/// promotion; [`Placer::FlowSeed`] and [`Placer::Champion`] are the two
/// retained control arms and every other variant is dead on the default
/// path.
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
    /// **The default placer from the first ADR-23 promotion
    /// (2026-08-18) until the second (2026-08-24)**, when
    /// [`Placer::FlowSeedV4`] superseded it. Retained as a **control
    /// arm**: A/B against it is what attributes a future regression to
    /// the root-policy unification rather than to the change under test.
    FlowSeed,
    /// **Orientation-churn Stage 1** — one depth-root policy, shared by
    /// the X layering and the flow idioms.
    ///
    /// `idioms::signal_net_depth` is what tells
    /// [`crate::idioms::apply_series_horizontal`] which way a series
    /// element's signal runs, and *that* pass is the only thing that
    /// pins a series chain horizontal — the pin the SA and phase 4.5
    /// both skip. Its root policy has two tiers: declared `*@port
    /// …=input` nets, then a leaf-name backstop that requires the net be
    /// touched by **exactly one** element. Neither tier knows about a
    /// **drawn source**, which is precisely the root
    /// `layers::assign_x_layers_with` uses on its principled (non-
    /// fallback) path via `is_signal_source`.
    ///
    /// On `lc_ladder_lpf` — the one fixture with a drawn stimulus and no
    /// `*@port …=input` — `in` is touched by `RS`, `C1` and `L1`, so the
    /// leaf backstop rejects it, the depth map comes back **empty**, and
    /// `apply_series_horizontal` declines every element of the ladder.
    /// The seed's textbook drawing (`RS`, `L1`, `L2`, `L3` all horizontal
    /// on one lane, shunts hanging below) is then left unpinned for the
    /// SA to rotate apart.
    ///
    /// This variant adds a **third** tier, reached only when the first
    /// two seed nothing: root at the Signal-class nets of drawn sources,
    /// mirroring `layers::is_signal_source` verbatim (a `VoltageSrc` /
    /// `CurrentSrc` that is not `;@ power`-tagged). The function's own
    /// comment already claimed it mirrored the layering "so depth and
    /// layer agree"; only the *fallback* was ever mirrored, never the
    /// principled source-root path. Nothing else moves: the tier is
    /// last, so any fixture whose port loop or leaf backstop seeds a
    /// root is byte-unchanged.
    FlowSeedV2,
    /// **Orientation-churn Stages 1 + 2** — [`Placer::FlowSeedV2`] plus
    /// the SA's V5 never-increase gate extended from mirror-Y to *every*
    /// reorienting move.
    ///
    /// `pin_outward_misalignment` is already tracked incrementally in
    /// the anneal loop and already precedented as a move gate, but the
    /// predicate reads `!is_mirror || trial <= current`, which
    /// short-circuits to `true` for every **rotate**. So the one soft
    /// signal the SA has about pin facing is disarmed for the move that
    /// changes facing most: the annealer rotates a horizontal series
    /// element vertical whenever HPWL/compaction pays for it, and its
    /// objective has no orientation term to notice (`cost.rs` has no
    /// `pin_facing` weight — deliberately, see CLAUDE.md).
    ///
    /// Under this variant the gate is `proposal.reorients().is_some()`,
    /// so an *improving* rotation still passes and a destructive one is
    /// refused. It is a **never-increase** gate, not a new cost term —
    /// no weight is added to `cost.rs`, which would re-create the
    /// Attempt-A failure CLAUDE.md records.
    ///
    /// Known risk, recorded up front: ADR-17's corrected SA ablation
    /// found the annealer genuinely load-bearing for **bend count** on
    /// three complex fixtures. Constraining its rotate move can cost
    /// bends, and the ADR-16 protocol (V16 non-increasing per fixture)
    /// is the instrument that says where.
    FlowSeedV3,
    /// **Root-policy unification** — one signal-flow root set, read by
    /// the X layering and the flow idioms alike.
    ///
    /// [`Placer::FlowSeedV2`] closed ONE of the three divergences
    /// between `layers::assign_x_layers_with` and
    /// `idioms::signal_net_depth` — the drawn source — by bolting a
    /// third tier onto the depth map. This variant closes the *class*:
    /// both consumers call `roots::signal_flow_roots`, a single tiered
    /// policy (declared `*@port …=input` ≻ drawn source ≻ leaf-input
    /// name ≻ none) with ADR-18's "boundary, not interior" filter
    /// applied inside it — and, new here, applied to **declared ports**
    /// too, so a port sitting on an interior net can no longer root
    /// mid-chain unchallenged (`port_shapes`).
    ///
    /// The *traversals* are deliberately not unified: the layering needs
    /// element roots for a longest-path DAG, the depth map needs net
    /// roots for a shortest-hop BFS. Every defect this class produced
    /// was a disagreement about which roots exist, never about the walk.
    ///
    /// One asymmetry is designed and permanent: `no_source_fallback`'s
    /// **rail-rooted** policy stays layers-only. With no signal-flow
    /// root the depth map returns EMPTY and `apply_series_horizontal`
    /// declines — declining is correct for a rootless cycle, and
    /// fabricating a direction from rail hops is not. `diff_pair`,
    /// `multivibrator` and `wien_bridge_osc` are byte-identical for
    /// exactly that reason, and are the cheapest check that they stayed
    /// so.
    ///
    /// **The default placer since the second ADR-23 promotion
    /// (2026-08-24).** Graded PROMOTABLE against [`Placer::FlowSeed`] on
    /// a fresh whole-suite table — Tier 0 clean on both arms with no
    /// regressed cell, Tier 1 −1.00, Tier 2 −25.30 — and promoted on
    /// owner authorisation. Only the two fixtures with a drawn stimulus
    /// move (`lc_ladder_lpf`, `sallen_key_driven`); the other sixteen are
    /// byte-identical, verified with `diff` on the emitted sheets rather
    /// than inferred. The headline repair is `lc_ladder_lpf`, which the
    /// owner called "completely mad" under the old default (`RS`/`L1`/
    /// `L2`/`L3` at rotations 180/90/0/270) and which now emits the
    /// textbook ladder: all four at rot 90 on one line at y = 35.56.
    FlowSeedV4,
    /// **Rail-gated divider idiom** — [`Placer::FlowSeedV4`] plus a
    /// `detect_dividers` predicate that matches an actual voltage
    /// divider instead of any degree-2 interior net.
    ///
    /// The shipping detector accepts a tap net whose **degree is exactly
    /// 2**, on the reasoning that "no third consumer" proves the node is
    /// a divider midpoint. It proves nothing of the sort: *every*
    /// interior net of a plain series chain has degree 2. So the
    /// predicate both **over-matches** — `port_shapes`' four-resistor
    /// chain `src→R1→ni→R2→no→R3→nb→R4→0` is claimed as two "dividers"
    /// and pinned as two vertical stacks of two, before
    /// [`crate::idioms::apply_series_horizontal`] can see the chain at
    /// all — and **under-matches**: a real bias divider's tap drives a
    /// base or a gate, so its degree is 3 and the detector never fires
    /// on the one topology it was written for.
    ///
    /// Under this variant a divider is what the name says: two
    /// two-terminal resistors meeting at a **Signal-class** tap, whose
    /// two outer nets are **rails of opposite [`crate::net_class::VertPref`]**
    /// — one positive supply (`Up`), one ground or negative rail
    /// (`Down`). That is `rail → Ra → tap → Rb → rail`, the topology
    /// whose conventional drawing IS the vertical stack the idiom emits,
    /// and the polarity also fixes the stack ORDER (the supply-side
    /// resistor on top) instead of leaving it to element index. The tap
    /// degree gate is dropped entirely: a loaded tap is the *canonical*
    /// divider, not a disqualification.
    ///
    /// Nothing else moves — the geometry, the stride, the pin mechanism
    /// and the pinned-wins rule are the shipping ones.
    DividerRails,
    /// **Rail-gated divider idiom, unloaded-tap variant** — the same
    /// corrected predicate as [`Placer::DividerRails`], but with the
    /// shipping tap-degree-2 gate *retained* on top of it.
    ///
    /// This is the deliberately **conservative** reading of the defect:
    /// the rail test alone removes the over-match (a plain series chain
    /// is declined) while the retained degree gate declines the loaded
    /// bias divider the shipping predicate also declines. The predicate
    /// is a strict *narrowing* of the shipping one, so the only fixture
    /// it can move is one the shipping detector wrongly claimed.
    ///
    /// It exists to **attribute** the `divider-rails` scoreboard: that
    /// arm changes three fixtures at once (`port_shapes` loses two
    /// spurious pairs, `common_emitter` and `two_stage_amp` each gain
    /// real ones), so its aggregate cannot say which direction paid for
    /// which. This arm moves `port_shapes` only.
    DividerRailsStrict,
    /// **Facing-inverted at-risk trigger (F2)** — [`Placer::FlowSeedV4`]
    /// plus a third reason for layout phase 4.5 to *look at* an element.
    ///
    /// Phase 4.5's at-risk sweep is **offender-gated**: an element is a
    /// candidate only when it currently carries a V5 first-segment
    /// violation or a V12 wire speared through its body. On
    /// `two_stage_amp` the seed emits BOTH transistors upside down (rot
    /// 180 + mirror); the phase repaired `Q1` and never considered `Q2`,
    /// because at its post-SA position the flipped `Q2` is
    /// violation-free — both first segments leave outward. The cost is a
    /// 35 mm bypass wire and an emitter-up transistor, and no trigger
    /// could see either. With the SA disabled the phase flips *both*, so
    /// **reach, not acceptance, is what saved `Q2`**.
    ///
    /// Under this variant [`crate::dc_rank::device_facings`] supplies a
    /// third trigger: a Q / M / J device whose higher-DC-potential
    /// terminal is drawn screen-DOWN. The rank is derived from the SPICE
    /// source alone — terminal identity from element syntax, potential
    /// order from DC reachability to the rails — so it consults no
    /// device library and needs no polarity special case.
    ///
    /// **The acceptance predicate is untouched.** A candidate pose still
    /// has to strictly improve
    /// `(severed, coincident, v11, v13, v12, v5, bends)` under the same
    /// guards, so the worst case of a wrong facing answer is "trialled a
    /// pose and refused it". It is deliberately NOT a hard candidate
    /// filter (ADR-15's Stage-5 post-mortem measured that move causing
    /// Tier-1 damage) and NOT a `cost.rs` weight (CLAUDE.md
    /// constraints-vs-costs, and the V14 Attempt-A failure).
    FacingTrigger,
    /// **Terminal-net series orientation** — [`Placer::FlowSeedV4`] plus a
    /// third acceptance case in
    /// [`crate::idioms::apply_series_horizontal`].
    ///
    /// That pass is the only mechanism in the tree that draws a series
    /// element horizontally *and* **pins** it, which is the only thing the
    /// SA and phase 4.5 both respect. It declines in two places, and the
    /// first is its **shunt-bearing guard**: unless the downstream node
    /// carries a rail stub to re-column, it leaves the element to the
    /// general chooser. The guard exists for a real measurement — forcing
    /// horizontality on *every* directed series element cost
    /// `common_emitter` B 4→7 — but it also declines exactly where a
    /// coupling capacitor meets the sheet boundary, which is why `CIN` /
    /// `COUT` stand on end and their `*@port` labels attach vertically.
    ///
    /// Under this variant an element **one of whose endpoint nets is
    /// terminal** is accepted too. Terminal means, structurally: the net
    /// carries a declared `*@port`, or exactly one element touches it (a
    /// leaf). Either way the net has nothing on it to re-column — which is
    /// the hazard the shunt-bearing guard exists for — so the construction
    /// needs no downstream anchor.
    ///
    /// It is still **joint** position+orientation, per the ADR-15 Stage-5
    /// root diagnosis ("making the orientation choice hard does not make it
    /// good, it makes it permanent"): the pin on the element's *interior*
    /// side is held at its current world position while the pose changes,
    /// so the element swings out into the empty half-plane the terminal net
    /// is, instead of rotating about its own origin into whatever sits
    /// beside it. The terminal-side pin is deliberately free.
    TerminalSeries,
    /// **Terminal-net + divider-node series orientation** —
    /// [`Placer::TerminalSeries`] plus a relaxation of the *second*
    /// decline in [`crate::idioms::apply_series_horizontal`], its
    /// **both-sides guard**.
    ///
    /// A downstream node carrying rail stubs on both sides (`common_emitter`
    /// `b`, `two_stage_amp` `b1`/`b2`, `cascode_amp` `b2`) is a bias divider
    /// *through* the node, not a shunt to drop beneath an output. The
    /// shipping pass declines outright, because re-columning the divider
    /// would perturb geometry the divider idiom owns.
    ///
    /// Declining is stronger than it needs to be. Under this variant the
    /// guard becomes **orient-but-do-not-re-column**: the series element is
    /// drawn horizontal and pinned with its downstream pin landing on the
    /// divider's own column at the Y of the wire that leaves the node for
    /// the device it drives (a transistor base, a gate). The divider
    /// members are read, never written — they stay exactly where the
    /// divider idiom and [`crate::idioms::apply_rail_stub_columns`] put
    /// them. That is the conventional drawing: a horizontal coupling cap
    /// arriving at the base wire, with the divider hanging vertically
    /// through the same node.
    ///
    /// The split from [`Placer::TerminalSeries`] is for **attribution**:
    /// the terminal case and the divider case move overlapping fixtures
    /// (`two_stage_amp` gets both), so a single arm's aggregate could not
    /// say which half paid for which.
    TerminalSeriesDivider,
    /// **Page-frame pin Y in the placement cost frame (F3)** —
    /// [`Placer::FlowSeedV4`] with [`crate::PlacedElement::world_pin_mm`]
    /// and the two other library-frame→page-frame conversions inside the
    /// placer corrected to the *page* frame every other consumer uses.
    ///
    /// A KiCad library symbol's pin `y` grows **upward**; the emitted
    /// schematic's `y` grows **downward**, and the conversion is a
    /// negation about the symbol origin — `world = (ox + p.x, oy - p.y)`.
    /// That is what `kicad_emitter::schematic`'s `pin_world`,
    /// `kicad_emitter::refine`, `kicad_symbols::Symbol::pin_text_bboxes`,
    /// `crate::sheets` and `crate::solver::anneal`'s own V11 / V5 gates
    /// all compute. [`crate::PlacedElement::world_pin_mm`] instead
    /// computes `oy + p.y`, and has since the foundation commit.
    ///
    /// It is **not** a global flip, which would be an isometry and
    /// therefore invisible to a distance-based objective. It is a
    /// *per-element* mirror about that element's own origin row, so each
    /// symbol's pin set is reflected independently. For a y-symmetric
    /// two-pin part (`Device:R` upright) the reflection maps the pin
    /// *positions* onto themselves but **swaps which pin number sits at
    /// which end** — so the SA believes a bias resistor's supply pin is
    /// the one nearest ground.
    ///
    /// Affected: `cost::hpwl`, `cost::crossings`,
    /// `cost::net_bbox_crossings`, `cost::rail_direction`, and the
    /// `place` half of `cost::constraint_violation`; the V5 seed scorer
    /// in `pick_orientations`; and the pin-offset compensation in
    /// `solve_place`. Unaffected by inspection (x-only):
    /// `cost::signal_flow`, `cost::rail_stub_alignment`,
    /// `idioms::world_pin_x_of`.
    ///
    /// The SA therefore minimises an objective stated in one frame while
    /// its own accept-gates (`v11_coincident_pin_count`,
    /// `pin_outward_misalignment`) and every downstream verifier measure
    /// the other. This arm makes the two agree; the scoreboard grades
    /// whether the ~165 budgets tuned against the mismatched objective
    /// are better or worse off for it.
    YSign,
    /// **V17 signal-direction filter** — [`Placer::FlowSeedV4`] plus a
    /// second hard candidate filter in
    /// [`crate::orient::allowed_orientations`]: a symbol carrying at
    /// least one `Output` pin **and** at least one `Input` pin must be
    /// posed with its output pins to the **right** of its input pins.
    ///
    /// An amplifier symbol has an intrinsic left-to-right reading
    /// direction, and nothing in the tree enforced it. V14 cannot: it
    /// constrains the *vertical* axis, and a KiCad `(mirror y)` flips
    /// only `x`, so a mirrored opamp still has V+ up and V− down and is
    /// **V14-legal**. With the horizontal axis unconstrained the SA
    /// mirrors a directional device freely whenever it shortens a wire —
    /// on the default placer `opamp_inverting_real` and `sallen_key_lpf`
    /// both ship a `rot 0` + `mirror y` opamp, and under
    /// [`Placer::YSign`] it is `opamp_inverting_real`,
    /// `sallen_key_driven` and `wien_bridge_osc`.
    ///
    /// The rule is stated on KiCad pin **electrical types**, so it is
    /// structural rather than pattern-matched (CLAUDE.md principle 9): it
    /// covers comparators, buffers, gates and any `.subckt` mapped to a
    /// directional symbol, with no named special case. A symbol lacking
    /// either pin group is exempt — `Device:Q_NPN_BCE` carries one
    /// `input` base and two `passive` pins, and mirroring a BJT is a
    /// legitimate drawing choice.
    ///
    /// It is a **hard candidate filter**, not a `cost.rs` weight: V17 is
    /// Tier 1 and categorical, which is exactly the constraints-vs-costs
    /// decision rule's hard case, and a tunable term at a safe weight is
    /// the recorded `power_pin_outward` failure. Because the filter lives
    /// in `allowed_orientations`, every stage that can reorient an
    /// element is bound by it: [`crate::pick_orientations`], the SA's
    /// `Proposal::Rotate` **and** `Proposal::MirrorY` (both reached
    /// through `Proposal::reorients`, which is what matters here — every
    /// observed violation is a mirror), and layout phase 4.5. That is the
    /// consistency requirement discharged at all three seams.
    ///
    /// See `docs/invariants.md` V17 and ADR-33.
    SignalDirection,
    /// **The composed readability arm** — [`Placer::FlowSeedV4`] plus
    /// *all four* of the built, registered, individually-measured
    /// mechanisms that each service one of the owner's reported
    /// readability defects, and nothing else:
    ///
    /// | constituent arm | accessor | owner defect |
    /// | --- | --- | --- |
    /// | [`Placer::SignalDirection`] | [`Self::signal_direction_filter`] | opamps drawn mirrored, output facing left |
    /// | [`Placer::TerminalSeriesDivider`] | [`Self::terminal_net_series`] + [`Self::divider_node_series`] | VIN / VOUT terminals drawn on end |
    /// | [`Placer::DividerRailsStrict`] | [`Self::rail_gated_dividers`] + [`Self::divider_tap_must_be_unloaded`] | `port_shapes`' split chain — the `detect_dividers` degree-2 over-match that pins a plain series chain vertically |
    /// | [`Placer::FacingTrigger`] | [`Self::facing_inverted_trigger`] | `two_stage_amp` `Q2` drawn upside down |
    ///
    /// **Why a composition needs its own registration.** Each arm was
    /// graded alone against `flow-seed-v4`, and three of the four were
    /// *individually* PROMOTABLE or blocked on a single cell. None of
    /// that says what they do together: the placer is a globally-coupled
    /// optimiser, so four filters and constructions that are separately
    /// contained can still interact. ADR-23's instrument grades a *whole
    /// placer*, so the composition has to be one.
    ///
    /// **Why the interaction is expected to be positive, and where.**
    /// ADR-23 D12 closes on the one prediction this arm exists to test:
    /// `terminal-series-divider` cannot reach `port_shapes`' two
    /// remaining vertical terminals because `detect_dividers`' degree-2
    /// over-match pins all four members of its plain series chain
    /// *before* `apply_series_horizontal` runs at all. The rail gate is
    /// strictly upstream of that, so composing the two should unblock
    /// terminals neither arm reaches alone.
    ///
    /// **`y-sign` is deliberately NOT a constituent.** ADR-30 and ADR-31
    /// leave it unresolved — the correction is real, its Tier-2 cost was
    /// attributed to `named_rails` on a `+5` bend difference a 20-seed
    /// sweep reduced to `+0.20 ± noise`, and ADR-30's own prescription is
    /// that it must land *bundled with a re-tuning*, not alone. Folding
    /// an unresolved arm into a composition would confound the
    /// attribution of everything else in it.
    ///
    /// Every constituent is gated on its own accessor and nothing else,
    /// so this variant adds **no new mechanism** — only a name under
    /// which the four can be graded jointly. It is dead on the default
    /// path; `baseline_lock` is the empirical half.
    ///
    /// **Promoted to the default 2026-09-04** on explicit owner
    /// authorisation ("Yes, let's promote", in reply to the
    /// recommendation and the disclosed residuals). Graded per ADR-23:
    /// Tier 0 clean on both sides with conversion refusals 0/2160 —
    /// equal to the champion, after ADR-37's Tier-0 escape closed the
    /// one `sallen_key_lpf` refusal at SA seed 1 — Tier 1 -2.00,
    /// Tier 2 -35.46. Multi-seed (ADR-32, k = 9): 29 EFFECT cells, 25
    /// of them better, and every `port.label_vertical` cell at
    /// 0.000 +/- 0.000.
    ///
    /// It repairs nine owner-reported defects and breaks none: vertical
    /// `VIN`/`VOUT` terminals 9 -> 0, mirrored amplifiers 2 -> 0,
    /// `two_stage_amp`'s inverted `Q2`, and `port_shapes`' split chain.
    /// Two fixtures regress deterministically on V16 bends and both are
    /// disclosed rather than explained away: `port_shapes` +2 (the price
    /// of un-pinning its over-matched chain) and
    /// `opamp_definition_level` +4, which is **unexplained**.
    ReadableV1,
    /// **DC-series column, position only** — [`Placer::ReadableV1`] plus
    /// [`crate::dc_column`]'s constructive column idiom: elements that
    /// carry the same DC current share an X column and order in Y by
    /// [`crate::dc_rank`].
    ///
    /// The mismodelling it addresses has the V14/V17 shape — one axis
    /// promoted a generation, the orthogonal one left behind. X has meant
    /// *depth along the DC signal path* since the second promotion; Y is
    /// still [`crate::bands`]' five-value net-class-membership table,
    /// while the convention it serves is *Y proportional to DC potential
    /// along the current path*. `dc_rank` is that functional, and
    /// placement never read it — only phase 4.5's facing trigger does.
    ///
    /// It is deliberately **Y-ordering and not Y-spacing**: ADR-19's M4
    /// post-mortem measured that Y-spacing changes land in chaotic,
    /// unattributable basins, and M4 was landed-then-reverted. The bands
    /// are untouched; a matched component is re-seated onto its own
    /// barycenter as a stack, and nothing else moves.
    ///
    /// This arm **pins nothing**. `pick_orientations`, the SA and phase
    /// 4.5 keep every degree of freedom they have today, so it removes no
    /// pose from the Tier-0 repair and owes no ADR-37 escape.
    DcSeriesColumn,
    /// **DC-series column, pinned** — [`Placer::DcSeriesColumn`] with the
    /// column frozen: members are pinned, so the SA and phase 4.5 leave
    /// the stack put.
    ///
    /// The split from the unpinned arm is for **attribution**, exactly as
    /// [`Placer::TerminalSeries`] splits from
    /// [`Placer::TerminalSeriesDivider`]: an unpinned seed nudge the SA
    /// can re-basin away and a frozen construction are two different
    /// experiments, and one aggregate could not say which half paid.
    ///
    /// Pinning skips `pick_orientations`, so under CLAUDE.md's
    /// *consistency requirement* this arm must own the pose it freezes —
    /// [`crate::dc_column::apply_dc_columns`] chooses one from the same
    /// [`crate::orient::allowed_orientations`] set (V14 intersect V17)
    /// every other stage reads, and declines the whole column when that
    /// set admits nothing. It is a construction, not a filter, but it
    /// still narrows what phase 4.5's Tier-0 repair can reach — the
    /// failure mode ADR-37 records for `terminal-series-divider` on
    /// `sallen_key_lpf` — so an SA seed sweep is part of its grading
    /// rather than an optional extra.
    /// **Promoted to the default 2026-09-05** on explicit owner
    /// authorisation ("dc-series-column-pinned clearly better, please
    /// finish promotion", after reviewing the rendered side-by-side).
    /// A per-promotion sign-off, not the standing autonomy instruction.
    ///
    /// Graded per ADR-23 against `readable-v1`: Tier 0 clean both sides,
    /// Tier 1 **+0.00**, Tier 2 **-129.25**, and ADR-28 metric B
    /// (`stack.side_by_side`) **4 -> 0 suite-wide**. Multi-seed
    /// (ADR-32, k = 9): 47 EFFECT cells, 38 of them better;
    /// `stack.side_by_side / cascode_amp` 1 -> 0 is INERT, i.e. repaired
    /// on every seed rather than on a lucky draw. No added conversion
    /// refusal over 440 seeds, and phase 4.5 receives a *cleaner*
    /// placement (Tier-0-dirty baselines 12/440 -> 5/440).
    ///
    /// Three residuals, disclosed rather than argued away:
    /// `f5 / resistor_ladder_ref` genuinely regresses (k=9 t = +6.11,
    /// where the single draw reads it as unmoved);
    /// `port.label_vertical / resistor_ladder_ref` reverses sign between
    /// the instruments; and `shunt_feedback_amp`'s `[RB RF]` column is
    /// ADR-28 ambiguity 8, inherited on purpose with the shared
    /// discriminator.
    #[default]
    DcSeriesColumnPinned,
    /// **Co-net layer collapse** — `dc-series-column-pinned` plus ONE
    /// change inside [`crate::layers::assign_x_layers_with`]'s rooted-DAG
    /// path: elements incident on exactly the same set of Signal nets
    /// take the shallowest layer of the group.
    ///
    /// It repairs a clique-expansion defect in the layering. The
    /// hypergraph is reduced to a clique expansion, `break_cycles` turns
    /// each clique into a tournament, and the longest-path layering walks
    /// the Hamiltonian path it always contains — so a `k`-element net
    /// spreads its own members across up to `k - 1` layers and X stops
    /// measuring depth along the signal path. `place_seed` then multiplies
    /// every spurious gap by a full X stride.
    ///
    /// **Blast radius: two fixtures, and the other twenty are
    /// byte-identical** (verified by `diff` over all 22 emitted
    /// `.kicad_sch`, `--no-layout-cache`). Only four fixtures reach the
    /// rooted-DAG path at all — a drawn stimulus with no declared
    /// `*@port …=input` — and of those only `compensated_divider` (the
    /// R-parallel-C arm) and `opamp_transimpedance` (`RF`/`CF`/`X1` all on
    /// `{inv, out}`) present a co-net group. `resistor_ladder_ref`'s
    /// six-layer spread is the `no_source_fallback` BFS and is NOT
    /// touched.
    ///
    /// Graded per ADR-23 against `dc-series-column-pinned`, whole suite
    /// each side, single shipped seed: **Tier 0 clean, Tier 1 +0.00** (no
    /// Tier-0 or Tier-1 cell moves at all), **Tier 2 ≈ −9**. Wins:
    /// `v16.bends` 12 → 5 and 5 → 4, `f6` −5, `q3` −3, `v5` −1, detour
    /// −4.5 pts on `opamp_transimpedance`. Costs: `q5` 0 → 2 and detour
    /// +9.5 pts on `compensated_divider`, `v16.branches` 2 → 3 on
    /// `opamp_transimpedance`. Absolute emitted wire falls hard on both
    /// (114.30 → 44.45 mm and 118.11 → 62.23 mm) — the detour *ratio*
    /// rises on `compensated_divider` because its rectilinear ideal falls
    /// further than its wire does.
    ///
    /// Registered rather than landed on the default path because those
    /// three cells are RISES, and CLAUDE.md's ratchet rule forbids raising
    /// a literal for an ordinary change. Promotion is an owner decision
    /// under ADR-23 D4.
    ConetLayerCollapse,

    /// `dc-series-column-pinned` plus **the column carrying the rail
    /// stubs of its own shared nets** — see
    /// [`crate::dc_column::plan_carried_stubs`].
    ///
    /// # The defect
    ///
    /// The promoted column re-seats its members constructively from
    /// `dc_rank`, but a bypass capacitor hanging off one of its taps is
    /// placed by two OTHER authorities, neither of which knows about it:
    /// [`crate::idioms::apply_rail_stub_columns`] gives it an X (from
    /// positions the column then moves — it runs first) and
    /// [`crate::bands`] gives it a Y that is a *sheet-height fraction*.
    /// On `resistor_ladder_ref` that puts `CB2` at y = 86.36 for a `t2`
    /// tap at y = 52.07, a 30 mm vertical run, and drags the `t2` port
    /// label down with it. CLAUDE.md's "constraints are pin-anchored"
    /// invariant is implemented in ONE axis; this arm implements the
    /// other.
    ///
    /// # Why it is an arm and not a default-path fix
    ///
    /// It costs **F6 (rail-stub lateral run) +1 cell on two fixtures**,
    /// `rc_phase_shift` and `cascode_amp` (3 -> 4), and that is a Tier-2
    /// ratchet RISE, which CLAUDE.md's ratchet policy forbids an ordinary
    /// change from taking. The rise is not slack to be tuned away: a
    /// mid-column tap's stub cannot stand IN the column (the trunk
    /// continues below it), so it stands beside it, and 4 cells is the
    /// SMALLEST offset at which a `Device:C` clears a `Device:R_US` with
    /// [`crate::MIN_CLEARANCE_MM`] between them. The incumbent reaches 3
    /// only because it never seats the stub against the member at all —
    /// it leaves it a band away, which is the defect.
    ///
    /// So the trade is stated rather than taken: F6 +2 cells across two
    /// fixtures, against every carried stub landing on its tap's own
    /// line. Promotion is the owner's decision under ADR-23.
    DcColumnNodeStubs,

    /// **The composition** — [`Self::ConetLayerCollapse`] AND
    /// [`Self::DcColumnNodeStubs`] together, on `dc-series-column-pinned`.
    ///
    /// Both are owner-reported repairs that graded Tier-0 clean and
    /// Tier-1 +0.00 individually, and both were registered rather than
    /// landed because their Tier-2 literals RISE. They are composed here
    /// for two reasons.
    ///
    /// First, ADR-36's lesson: arms that are graded separately and then
    /// shipped together have not been graded. The two touch disjoint
    /// stages — one is inside `layers::assign_x_layers_with`'s rooted-DAG
    /// path, the other inside `dc_column::apply_dc_columns` — but the
    /// placer is a globally-coupled map and "disjoint code" is not
    /// "disjoint output". Only a joint run measures the pair.
    ///
    /// Second, they answer four of the owner's six reported defects on
    /// the 2026-09-05 eval render, and a single arm is what a visual
    /// review can actually judge.
    ///
    /// Composition only: this variant adds NO construction of its own.
    /// It answers `true` to exactly the accessors its two components
    /// answer, which is what makes any difference between this arm and
    /// the union of its parts an interaction rather than a new feature.
    ColumnStubsConet,
}

impl Placer {
    /// Every registered placer, the default first and the three
    /// retained control arms next.
    pub const ALL: &'static [Self] = &[
        Self::ReadableV1,
        Self::FlowSeedV4,
        Self::FlowSeed,
        Self::Champion,
        Self::M4YDatum,
        Self::M3SignedGate,
        Self::M3SignedFull,
        Self::M5Streams,
        Self::FlowSeedV2,
        Self::FlowSeedV3,
        Self::DividerRails,
        Self::DividerRailsStrict,
        Self::FacingTrigger,
        Self::TerminalSeries,
        Self::TerminalSeriesDivider,
        Self::YSign,
        Self::SignalDirection,
        Self::DcSeriesColumn,
        Self::DcSeriesColumnPinned,
        Self::ConetLayerCollapse,
        Self::DcColumnNodeStubs,
        Self::ColumnStubsConet,
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
            Self::FlowSeedV2 => "flow-seed-v2",
            Self::FlowSeedV3 => "flow-seed-v3",
            Self::FlowSeedV4 => "flow-seed-v4",
            Self::DividerRails => "divider-rails",
            Self::DividerRailsStrict => "divider-rails-strict",
            Self::FacingTrigger => "facing-trigger",
            Self::TerminalSeries => "terminal-series",
            Self::TerminalSeriesDivider => "terminal-series-divider",
            Self::YSign => "y-sign",
            Self::SignalDirection => "signal-direction",
            Self::ReadableV1 => "readable-v1",
            Self::DcSeriesColumn => "dc-series-column",
            Self::DcSeriesColumnPinned => "dc-series-column-pinned",
            Self::ConetLayerCollapse => "conet-layer-collapse",
            Self::DcColumnNodeStubs => "dc-column-node-stubs",
            Self::ColumnStubsConet => "column-stubs-conet",
        }
    }

    /// One-line description, for `--help` and the scoreboard header.
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::Champion => {
                "the original pre-promotion placer, retained as a scoreboard control arm \
                 (n-scaled Y frame; X = hops from the nearest power rail)"
            }
            Self::M4YDatum => "ADR-19 M4: content-derived, n-independent Y datum",
            Self::M3SignedGate => "ADR-19 M3 ablation B: signed footprint in the SA overlap gate",
            Self::M3SignedFull => {
                "ADR-19 M3 full wiring: signed gate + property text + signed legalize"
            }
            Self::M5Streams => "ADR-19 M5': per-refdes SA proposal streams, deterministic sweep",
            Self::FlowSeed => {
                "control arm: the first-promotion flow-faithful skeleton \
                 (signal-flow roots, stub followers, barycenter order)"
            }
            Self::FlowSeedV2 => {
                "orientation-churn stage 1: one depth-root policy \
                 (drawn sources root the flow idioms' depth map too)"
            }
            Self::FlowSeedV3 => {
                "orientation-churn stages 1+2: stage 1 plus the SA's V5 \
                 never-increase gate extended from mirror-Y to rotate"
            }
            Self::FlowSeedV4 => {
                "default: root-policy unification — one tiered signal-flow \
                 root set, read by the X layering and the flow idioms alike"
            }
            Self::DividerRails => {
                "flow-seed-v4 plus a rail-gated divider idiom: a divider \
                 spans supply -> ground, and its tap may be loaded"
            }
            Self::DividerRailsStrict => {
                "divider-rails with the tap-degree-2 gate retained: \
                 removes the over-match only, adds no new detection"
            }
            Self::FacingTrigger => {
                "flow-seed-v4 plus a third phase-4.5 at-risk trigger: a \
                 device whose higher-DC-potential terminal is drawn down"
            }
            Self::TerminalSeries => {
                "flow-seed-v4 plus the terminal-net series case: a series \
                 element on a `*@port` or leaf net is drawn horizontal"
            }
            Self::TerminalSeriesDivider => {
                "terminal-series plus the divider-node case: orient onto the \
                 divider column instead of declining, never re-column it"
            }

            Self::YSign => {
                "flow-seed-v4 with the placer cost frame's pin Y sign \
                 corrected to the page frame the emitter measures in"
            }
            Self::SignalDirection => {
                "flow-seed-v4 plus the V17 hard filter: a symbol with both \
                 input and output pins is posed output-to-the-right"
            }
            Self::ReadableV1 => {
                "flow-seed-v4 plus all four registered readability arms: \
                 signal-direction + terminal-series-divider + \
                 divider-rails-strict + facing-trigger (no y-sign)"
            }
            Self::DcSeriesColumn => {
                "readable-v1 plus the DC-series column idiom: elements on \
                 one DC current share an X column, ordered by dc_rank"
            }
            Self::DcSeriesColumnPinned => {
                "dc-series-column with the column pinned, so the SA and \
                 phase 4.5 leave the stack put (attribution arm)"
            }
            Self::ConetLayerCollapse => {
                "dc-series-column-pinned plus the co-net layer collapse: \
                 elements on the same set of Signal nets share a column"
            }
            Self::DcColumnNodeStubs => {
                "dc-series-column-pinned plus the column carrying its \
                 shared nets' rail stubs, pin-anchored in BOTH axes"
            }
            Self::ColumnStubsConet => {
                "the composition: co-net layer collapse AND the column \
                 carrying its shared nets' rail stubs"
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
        matches!(
            self,
            Self::FlowSeed
                | Self::FlowSeedV2
                | Self::FlowSeedV3
                | Self::FlowSeedV4
                | Self::DividerRails
                | Self::DividerRailsStrict
                | Self::FacingTrigger
                | Self::TerminalSeries
                | Self::TerminalSeriesDivider
                | Self::YSign
                | Self::SignalDirection
                | Self::ReadableV1
                | Self::DcSeriesColumn
                | Self::DcSeriesColumnPinned
                | Self::ConetLayerCollapse
                | Self::DcColumnNodeStubs
                | Self::ColumnStubsConet
        )
    }

    /// Orientation-churn stage 1: does `idioms::signal_net_depth` fall
    /// back to **drawn-source** roots when neither a declared `*@port
    /// …=input` nor the leaf-name backstop seeds one?
    ///
    /// This unifies the depth-root policy with
    /// `layers::assign_x_layers_with`'s `is_signal_source`, which the
    /// depth function's own comment already claimed to mirror.
    #[must_use]
    pub fn unified_depth_roots(self) -> bool {
        matches!(self, Self::FlowSeedV2 | Self::FlowSeedV3)
    }

    /// Root-policy unification: do `layers::assign_x_layers_with` and
    /// `idioms::signal_net_depth` both read
    /// `roots::signal_flow_roots` — one tiered policy — instead of two
    /// independently-drifted ones?
    ///
    /// This **supersedes** [`Self::unified_depth_roots`] rather than
    /// composing with it: v4 replaces the depth map's tier ladder
    /// wholesale instead of appending a tier to it. `flow-seed-v2`'s
    /// inline third tier stays until v4 is promoted — deleting it now
    /// would retire the control arm the comparison is graded against.
    #[must_use]
    pub fn unified_roots(self) -> bool {
        matches!(
            self,
            Self::FlowSeedV4
                | Self::DividerRails
                | Self::DividerRailsStrict
                | Self::FacingTrigger
                | Self::TerminalSeries
                | Self::TerminalSeriesDivider
                | Self::YSign
                | Self::SignalDirection
                | Self::ReadableV1
                | Self::DcSeriesColumn
                | Self::DcSeriesColumnPinned
                | Self::ConetLayerCollapse
                | Self::DcColumnNodeStubs
                | Self::ColumnStubsConet
        )
    }

    /// Rail-gated divider idiom: does [`crate::idioms::detect_dividers`]
    /// require the pair's two outer nets to be **rails of opposite
    /// vertical preference** (a supply and a ground / negative rail) and
    /// its tap to be a Signal net — instead of gating on the tap's
    /// degree being exactly 2?
    ///
    /// The degree-2 gate is the defect: it matches every interior net of
    /// a plain series chain (over-match) while rejecting every loaded
    /// bias divider (under-match). See [`Self::DividerRails`].
    #[must_use]
    pub fn rail_gated_dividers(self) -> bool {
        matches!(
            self,
            Self::DividerRails
                | Self::DividerRailsStrict
                | Self::ReadableV1
                | Self::DcSeriesColumn
                | Self::DcSeriesColumnPinned
                | Self::ConetLayerCollapse
                | Self::DcColumnNodeStubs
                | Self::ColumnStubsConet
        )
    }

    /// Rail-gated divider idiom, conservative reading: is the shipping
    /// **tap-degree-2** gate retained *on top of* the rail test, so the
    /// predicate only ever narrows and no fixture gains detection?
    ///
    /// This is the attribution arm — see [`Self::DividerRailsStrict`].
    #[must_use]
    pub fn divider_tap_must_be_unloaded(self) -> bool {
        matches!(
            self,
            Self::DividerRailsStrict
                | Self::ReadableV1
                | Self::DcSeriesColumn
                | Self::DcSeriesColumnPinned
                | Self::ConetLayerCollapse
                | Self::DcColumnNodeStubs
                | Self::ColumnStubsConet
        )
    }

    /// Orientation-churn stage 2: does the SA's V5 never-increase gate
    /// (`pin_outward_misalignment`) bind on **every** reorienting move,
    /// instead of on mirror-Y alone?
    #[must_use]
    pub fn sa_rotate_v5_gate(self) -> bool {
        matches!(self, Self::FlowSeedV3)
    }

    /// F2: does layout phase 4.5's at-risk sweep gain a **third**
    /// trigger — a Q / M / J device whose higher-DC-potential terminal
    /// is drawn screen-down (see [`crate::dc_rank`]) — beside the
    /// existing V5-offender and V12-offender gates?
    ///
    /// This is a *reach* change and nothing else: the acceptance
    /// predicate, its tuple order and its guards are all unchanged, so
    /// every pose the extra trigger reaches still has to earn its way in
    /// on `(severed, coincident, v11, v13, v12, v5, bends)`.
    #[must_use]
    pub fn facing_inverted_trigger(self) -> bool {
        matches!(
            self,
            Self::FacingTrigger
                | Self::ReadableV1
                | Self::DcSeriesColumn
                | Self::DcSeriesColumnPinned
                | Self::ConetLayerCollapse
                | Self::DcColumnNodeStubs
                | Self::ColumnStubsConet
        )
    }

    /// F1 case (a): does [`crate::idioms::apply_series_horizontal`] accept
    /// a directed series element whose upstream **or** downstream net is a
    /// *terminal* net (a declared `*@port`, or a leaf net no second
    /// element touches), instead of declining under its shunt-bearing
    /// guard?
    ///
    /// The construction stays joint — the interior-side pin is held at its
    /// world position while the pose changes — because ADR-15 Stage 5
    /// measured what an orientation-only change costs.
    #[must_use]
    pub fn terminal_net_series(self) -> bool {
        matches!(
            self,
            Self::TerminalSeries
                | Self::TerminalSeriesDivider
                | Self::ReadableV1
                | Self::DcSeriesColumn
                | Self::DcSeriesColumnPinned
                | Self::ConetLayerCollapse
                | Self::DcColumnNodeStubs
                | Self::ColumnStubsConet
        )
    }

    /// F1 case (b): does that same pass **orient without re-columning**
    /// when the downstream node carries rail stubs on both sides (a bias
    /// divider through the node), instead of declining outright?
    ///
    /// The series element's downstream pin is placed on the divider's own
    /// column at the node's outgoing-wire Y; the divider members are read,
    /// never moved.
    #[must_use]
    pub fn divider_node_series(self) -> bool {
        matches!(
            self,
            Self::TerminalSeriesDivider
                | Self::ReadableV1
                | Self::DcSeriesColumn
                | Self::DcSeriesColumnPinned
                | Self::ConetLayerCollapse
                | Self::DcColumnNodeStubs
                | Self::ColumnStubsConet
        )
    }

    /// F3: does the placer convert a library-frame pin `y` into the
    /// **page** frame (`oy - p.y`) — the frame the emitter, the router,
    /// the SA's own V11 / V5 gates and every verifier measure in —
    /// instead of the `oy + p.y` [`crate::PlacedElement::world_pin_mm`]
    /// has computed since the foundation commit?
    ///
    /// See [`Self::YSign`] for what the mismatch costs and which cost
    /// terms it reaches. Gating it on one accessor is the whole
    /// byte-identity argument for the shipping output; `baseline_lock`
    /// is the empirical half.
    #[must_use]
    pub fn page_frame_pin_y(self) -> bool {
        matches!(self, Self::YSign)
    }

    /// V17: does [`crate::orient::allowed_orientations`] narrow each
    /// element's V14 set again to the poses that draw a directional
    /// symbol's **output** pins right of its **input** pins?
    ///
    /// Gating the whole invariant on this one accessor is the entire
    /// byte-identity argument for the shipping output — the filter is
    /// unreachable unless it returns `true`, so no default-path candidate
    /// set can change. `baseline_lock` is the empirical half.
    ///
    /// See [`Self::SignalDirection`] for why V14 could not catch this and
    /// why it must be a hard filter rather than a `cost.rs` weight.
    #[must_use]
    pub fn signal_direction_filter(self) -> bool {
        matches!(
            self,
            Self::SignalDirection
                | Self::ReadableV1
                | Self::DcSeriesColumn
                | Self::DcSeriesColumnPinned
                | Self::ConetLayerCollapse
                | Self::DcColumnNodeStubs
                | Self::ColumnStubsConet
        )
    }

    /// Does the seed run [`crate::dc_column::apply_dc_columns`] — the
    /// constructive column idiom that puts a chain of DC-series elements
    /// into one X column ordered by [`crate::dc_rank`]?
    ///
    /// Gating the whole construction on this one accessor is the entire
    /// byte-identity argument for the shipping output: the pass returns
    /// immediately unless this is `true`, so no default-path placement
    /// can change. `baseline_lock` is the empirical half.
    #[must_use]
    pub fn dc_series_columns(self) -> bool {
        matches!(
            self,
            Self::DcSeriesColumn
                | Self::DcSeriesColumnPinned
                | Self::ConetLayerCollapse
                | Self::DcColumnNodeStubs
                | Self::ColumnStubsConet
        )
    }

    /// Does that construction additionally **pin** the column, freezing
    /// it against the SA and phase 4.5?
    ///
    /// The attribution half — see [`Self::DcSeriesColumnPinned`]. Pinning
    /// is also what obliges the pass to choose each member's orientation
    /// (CLAUDE.md's consistency requirement).
    #[must_use]
    pub fn dc_series_columns_pinned(self) -> bool {
        matches!(
            self,
            Self::DcSeriesColumnPinned
                | Self::ConetLayerCollapse
                | Self::DcColumnNodeStubs
                | Self::ColumnStubsConet
        )
    }

    /// Does [`crate::layers::assign_x_layers_with`]'s rooted-DAG path
    /// collapse **co-net groups** — elements incident on exactly the same
    /// set of Signal nets — onto the shallowest layer of the group?
    ///
    /// Without it the clique expansion the layering builds is turned into
    /// a tournament by `break_cycles`, and the longest-path layering walks
    /// the Hamiltonian path it always contains: a `k`-element net spreads
    /// its own members over up to `k - 1` layers, each costing a full X
    /// stride. See `layers::collapse_conet_groups` for the derivation.
    ///
    /// **Only [`Self::ConetLayerCollapse`] answers `true`**, and gating it
    /// on this one accessor is the entire byte-identity argument for the
    /// shipping output: the pass is unreachable otherwise, so no
    /// default-path layer can change. `baseline_lock` is the empirical
    /// half. It is a challenger because three Tier-2 literals RISE under
    /// it (`q5 / compensated_divider` 0 -> 2, its detour ratio 1.0714 ->
    /// 1.1667, and `v16.branches / opamp_transimpedance` 2 -> 3), and an
    /// ordinary change may not raise a ratchet.
    #[must_use]
    pub fn conet_layer_collapse(self) -> bool {
        matches!(self, Self::ConetLayerCollapse | Self::ColumnStubsConet)
    }

    /// Does the DC-series column also **carry the rail stubs of its own
    /// shared nets** — seating each beside the column at its shared pin's
    /// Y, and widening the column's pitch to clear their glyph-inclusive
    /// extents?
    ///
    /// Gating the whole construction on this one accessor is the entire
    /// byte-identity argument for the shipping output:
    /// [`crate::dc_column::plan_carried_stubs`] returns an empty plan
    /// unless this is `true`, and an empty plan leaves both the stride
    /// and the stub untouched. `baseline_lock` is the empirical half.
    ///
    /// See [`Self::DcColumnNodeStubs`] for what it repairs and what it
    /// costs.
    #[must_use]
    pub fn dc_column_node_stubs(self) -> bool {
        matches!(self, Self::DcColumnNodeStubs | Self::ColumnStubsConet)
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
    fn default_is_the_dc_series_column_placer() {
        assert_eq!(Placer::default(), Placer::DcSeriesColumnPinned);
        assert_eq!(Placer::default().name(), "dc-series-column-pinned");
    }

    /// The co-net layer collapse is a CHALLENGER, not the default.
    /// Three Tier-2 literals rise under it, and an ordinary change may
    /// not raise a ratchet — so the shipping path must not reach it.
    /// This assertion is the byte-identity argument in code; the
    /// `baseline_lock` empty diff is its empirical half.
    #[test]
    fn conet_layer_collapse_is_off_on_the_default_path() {
        assert!(!Placer::default().conet_layer_collapse());
        for p in Placer::ALL {
            // Two arms answer `true`: the isolating challenger, and
            // the composition that contains it. Until 2026-09-06 this
            // read `*p == Placer::ConetLayerCollapse` — the exact
            // one-arm form — and `column-stubs-conet` makes that false
            // BY DESIGN, since composing the pass is the whole point of
            // that variant. What must stay true is the property this
            // test exists for: nothing on the SHIPPING path reaches it.
            assert_eq!(
                p.conet_layer_collapse(),
                matches!(p, Placer::ConetLayerCollapse | Placer::ColumnStubsConet),
                "{} must not reach the co-net collapse",
                p.name()
            );
        }
        // It composes ON the shipping default, so every arm the default
        // switches on must still be switched on.
        let d = Placer::default();
        let c = Placer::ConetLayerCollapse;
        assert_eq!(c.dc_series_columns(), d.dc_series_columns());
        assert_eq!(c.dc_series_columns_pinned(), d.dc_series_columns_pinned());
        assert_eq!(c.signal_direction_filter(), d.signal_direction_filter());
        assert_eq!(c.terminal_net_series(), d.terminal_net_series());
        assert_eq!(c.unified_roots(), d.unified_roots());
        assert_eq!(c.page_frame_pin_y(), d.page_frame_pin_y());
    }

    /// Neither promotion retired its predecessor. ADR-23's promotion
    /// rule grades every future challenger against the new default, and
    /// each superseded default has to stay runnable for the A/B that
    /// attributes a regression to a promotion rather than to the change
    /// under test. `flow-seed` is the arm for the 2026-08-24 root-policy
    /// promotion; `champion` is the arm for the 2026-08-18 one.
    #[test]
    fn all_control_arms_stay_registered() {
        assert_eq!(Placer::from_name("champion"), Some(Placer::Champion));
        assert!(Placer::ALL.contains(&Placer::Champion));
        assert_eq!(Placer::from_name("flow-seed"), Some(Placer::FlowSeed));
        assert!(Placer::ALL.contains(&Placer::FlowSeed));
        // The 2026-09-04 promotion added a THIRD superseded default.
        // `flow-seed-v4` is the arm that attributes a future regression
        // to the `readable-v1` promotion rather than to the change under
        // test; retiring it would make that A/B impossible.
        assert_eq!(Placer::from_name("flow-seed-v4"), Some(Placer::FlowSeedV4));
        assert!(Placer::ALL.contains(&Placer::FlowSeedV4));
        // The 2026-09-05 promotion added a FOURTH superseded default.
        assert_eq!(Placer::from_name("readable-v1"), Some(Placer::ReadableV1));
        assert!(Placer::ALL.contains(&Placer::ReadableV1));
        assert_ne!(Placer::default(), Placer::ReadableV1);
        // Each must be genuinely *different* from the default, or the
        // A/B they exist for would be vacuous.
        assert_ne!(Placer::default(), Placer::FlowSeed);
        assert_ne!(Placer::default(), Placer::Champion);
        assert_ne!(Placer::default(), Placer::FlowSeedV4);
        assert!(!Placer::FlowSeed.unified_roots());
        assert!(!Placer::Champion.flow_seed_layering());
        // The distinguishing mechanism: the new default composes four
        // readability arms that `flow-seed-v4` has none of.
        assert!(
            !Placer::FlowSeedV4.page_frame_pin_y() && !Placer::FlowSeedV4.terminal_net_series()
        );
        assert!(Placer::default().terminal_net_series());
    }

    /// The DC-series column construction is **dead on the default
    /// path**. Both halves are gated on a `Placer` accessor and nothing
    /// else, so this assertion is the whole byte-identity argument for
    /// the shipping output — `baseline_lock` is the empirical half.
    #[test]
    fn the_dc_series_column_is_absent_from_the_control_arms() {
        // Until 2026-09-05 this asserted the construction was off by
        // DEFAULT — the byte-identity guarantee that held while it was a
        // challenger. The promotion makes that false by design, so the
        // claim with content is that the SUPERSEDED defaults lack it,
        // which is what makes them useful A/B arms.
        assert!(!Placer::ReadableV1.dc_series_columns());
        assert!(!Placer::ReadableV1.dc_series_columns_pinned());
        assert!(Placer::default().dc_series_columns());
        assert!(Placer::default().dc_series_columns_pinned());
        assert!(!Placer::FlowSeedV4.dc_series_columns());
        assert!(!Placer::Champion.dc_series_columns());
        assert!(Placer::DcSeriesColumn.dc_series_columns());
        assert!(!Placer::DcSeriesColumn.dc_series_columns_pinned());
        assert!(Placer::DcSeriesColumnPinned.dc_series_columns());
        assert!(Placer::DcSeriesColumnPinned.dc_series_columns_pinned());
        // Both compose ON the shipping default, so a comparison against
        // `readable-v1` isolates the construction under test.
        for p in [Placer::DcSeriesColumn, Placer::DcSeriesColumnPinned] {
            assert!(p.flow_seed_layering());
            assert!(p.unified_roots());
            assert!(p.signal_direction_filter());
            assert!(p.terminal_net_series());
            assert!(p.divider_node_series());
            assert!(p.rail_gated_dividers());
            assert!(p.divider_tap_must_be_unloaded());
            assert!(p.facing_inverted_trigger());
            assert!(!p.page_frame_pin_y());
        }
    }

    /// The carried-node-stub construction is **dead on the default
    /// path**. It is gated on one accessor and nothing else, so this
    /// assertion is the whole byte-identity argument for the shipping
    /// output — `baseline_lock` is the empirical half.
    ///
    /// It composes ON the promoted default, so a comparison against
    /// `dc-series-column-pinned` isolates exactly the carry.
    #[test]
    fn the_carried_node_stubs_are_off_by_default() {
        assert!(!Placer::default().dc_column_node_stubs());
        for &p in Placer::ALL {
            // Two arms answer `true`: the isolating challenger, and
            // the composition that contains it. See the sibling note in
            // `conet_layer_collapse_is_off_on_the_default_path` — the
            // invariant under test is "dead on the SHIPPING path", not
            // "exactly one variant".
            assert_eq!(
                p.dc_column_node_stubs(),
                matches!(p, Placer::DcColumnNodeStubs | Placer::ColumnStubsConet),
                "{} must not carry node stubs",
                p.name()
            );
        }
        assert!(Placer::DcColumnNodeStubs.dc_series_columns());
        assert!(Placer::DcColumnNodeStubs.dc_series_columns_pinned());
        assert!(Placer::DcColumnNodeStubs.flow_seed_layering());
        assert!(Placer::DcColumnNodeStubs.unified_roots());
        assert!(Placer::DcColumnNodeStubs.signal_direction_filter());
        assert!(Placer::DcColumnNodeStubs.terminal_net_series());
        assert!(Placer::DcColumnNodeStubs.divider_node_series());
        assert!(Placer::DcColumnNodeStubs.rail_gated_dividers());
        assert!(Placer::DcColumnNodeStubs.divider_tap_must_be_unloaded());
        assert!(Placer::DcColumnNodeStubs.facing_inverted_trigger());
        assert!(!Placer::DcColumnNodeStubs.page_frame_pin_y());
    }

    #[test]
    fn every_registered_name_round_trips() {
        for &p in Placer::ALL {
            assert_eq!(Placer::from_name(p.name()), Some(p), "{}", p.name());
        }
        assert_eq!(Placer::from_name("no-such-placer"), None);
    }

    /// The orientation-churn stages are **dead on the default path**.
    /// Both are gated on a `Placer` accessor and nothing else, so this
    /// assertion is the whole byte-identity argument for the shipping
    /// output — `baseline_lock` is the empirical half.
    #[test]
    fn the_orientation_churn_stages_are_off_by_default() {
        // v2's bolted-on third tier and v3's SA rotate gate are both
        // still challengers: the promoted default reads `roots.rs`
        // instead, and never runs either.
        assert!(!Placer::default().unified_depth_roots());
        assert!(!Placer::FlowSeed.unified_roots());
        assert!(!Placer::Champion.unified_roots());
        assert!(!Placer::default().sa_rotate_v5_gate());
        assert!(!Placer::Champion.unified_depth_roots());
        assert!(!Placer::Champion.sa_rotate_v5_gate());
        // v2 = stage 1 only; v3 = stages 1 + 2.
        assert!(Placer::FlowSeedV2.unified_depth_roots());
        assert!(!Placer::FlowSeedV2.sa_rotate_v5_gate());
        assert!(Placer::FlowSeedV3.unified_depth_roots());
        assert!(Placer::FlowSeedV3.sa_rotate_v5_gate());
        // Both build ON the first promotion's layering, so a comparison
        // against `flow-seed` isolates the stage under test.
        assert!(Placer::FlowSeedV2.flow_seed_layering());
        assert!(Placer::FlowSeedV3.flow_seed_layering());
    }

    /// v4 is the *unification*, not another stage on top of v2: it
    /// replaces the depth map's tier ladder rather than extending it, so
    /// it must NOT also report `unified_depth_roots` (that would run v2's
    /// bolted-on third tier inside the else-branch v4 never takes, and
    /// blur the two arms the scoreboard compared).
    #[test]
    fn the_root_unification_is_the_default_and_distinct_from_v2() {
        assert!(Placer::default().unified_roots());
        assert!(Placer::FlowSeedV4.unified_roots());
        assert!(!Placer::FlowSeedV4.unified_depth_roots());
        assert!(!Placer::FlowSeedV4.sa_rotate_v5_gate());
        assert!(!Placer::FlowSeedV2.unified_roots());
        assert!(!Placer::FlowSeedV3.unified_roots());
        // Built ON the first promotion's layering, so the A/B against
        // `flow-seed` isolated the root policy and nothing else.
        assert!(Placer::FlowSeedV4.flow_seed_layering());
    }

    /// The rail-gated divider predicate is **dead on the default path**
    /// and on the control arm: it is gated on one accessor and nothing
    /// else, which is the whole byte-identity argument for the shipping
    /// output (`baseline_lock` is the empirical half).
    #[test]
    fn the_rail_gated_divider_idiom_is_absent_from_the_control_arm() {
        assert!(!Placer::FlowSeedV4.rail_gated_dividers());
        assert!(!Placer::Champion.rail_gated_dividers());
        assert!(!Placer::FlowSeedV4.rail_gated_dividers());
        assert!(Placer::DividerRails.rail_gated_dividers());
        assert!(Placer::DividerRailsStrict.rail_gated_dividers());
        // The strict arm is the NARROWING one: it keeps the tap-degree
        // gate, so it can only ever decline where the shipping
        // predicate accepted.
        assert!(Placer::DividerRailsStrict.divider_tap_must_be_unloaded());
        assert!(!Placer::DividerRails.divider_tap_must_be_unloaded());
        assert!(!Placer::FlowSeedV4.divider_tap_must_be_unloaded());
        // It composes ON `flow-seed-v4`, so an A/B against that arm
        // isolates the divider predicate and nothing else.
        assert!(Placer::DividerRails.unified_roots());
        assert!(Placer::DividerRails.flow_seed_layering());
        assert!(!Placer::DividerRails.unified_depth_roots());
        assert!(!Placer::DividerRails.sa_rotate_v5_gate());
    }

    /// The facing-inverted at-risk trigger is **dead on the default
    /// path** and on both control arms: it is gated on one accessor and
    /// nothing else, which is the whole byte-identity argument for the
    /// shipping output (`baseline_lock` is the empirical half).
    #[test]
    fn the_facing_inverted_trigger_is_absent_from_the_control_arm() {
        assert!(!Placer::FlowSeedV4.facing_inverted_trigger());
        assert!(!Placer::FlowSeedV4.facing_inverted_trigger());
        assert!(!Placer::FlowSeed.facing_inverted_trigger());
        assert!(!Placer::Champion.facing_inverted_trigger());
        assert!(Placer::FacingTrigger.facing_inverted_trigger());
        // It composes ON `flow-seed-v4` and changes nothing else, so an
        // A/B against the default isolates the trigger.
        assert!(Placer::FacingTrigger.unified_roots());
        assert!(Placer::FacingTrigger.flow_seed_layering());
        assert!(!Placer::FacingTrigger.rail_gated_dividers());
        assert!(!Placer::FacingTrigger.unified_depth_roots());
        assert!(!Placer::FacingTrigger.sa_rotate_v5_gate());
    }

    /// The F1 series-orientation cases are **dead on the default path**:
    /// both are gated on a `Placer` accessor and nothing else, which is the
    /// whole byte-identity argument for the shipping output
    /// (`baseline_lock` is the empirical half).
    #[test]
    fn the_terminal_series_cases_are_absent_from_the_control_arm() {
        assert!(!Placer::FlowSeedV4.terminal_net_series());
        assert!(!Placer::FlowSeedV4.divider_node_series());
        assert!(!Placer::FlowSeed.terminal_net_series());
        assert!(!Placer::Champion.terminal_net_series());
        assert!(!Placer::DividerRails.terminal_net_series());
        // (a) alone, then (a) + (b) — the attribution split.
        assert!(Placer::TerminalSeries.terminal_net_series());
        assert!(!Placer::TerminalSeries.divider_node_series());
        assert!(Placer::TerminalSeriesDivider.terminal_net_series());
        assert!(Placer::TerminalSeriesDivider.divider_node_series());
        // Both compose ON the default, so an A/B against it isolates the
        // series-orientation cases and nothing else.
        for p in [Placer::TerminalSeries, Placer::TerminalSeriesDivider] {
            assert!(p.unified_roots(), "{}", p.name());
            assert!(p.flow_seed_layering(), "{}", p.name());
            assert!(!p.unified_depth_roots(), "{}", p.name());
            assert!(!p.sa_rotate_v5_gate(), "{}", p.name());
            assert!(!p.rail_gated_dividers(), "{}", p.name());
        }
    }

    /// The page-frame pin-Y correction is **dead on the default path**
    /// and on both control arms: it is gated on one accessor and nothing
    /// else, which is the whole byte-identity argument for the shipping
    /// output (`baseline_lock` is the empirical half).
    #[test]
    fn the_page_frame_pin_y_is_off_by_default() {
        assert!(!Placer::default().page_frame_pin_y());
        assert!(!Placer::FlowSeedV4.page_frame_pin_y());
        assert!(!Placer::FlowSeed.page_frame_pin_y());
        assert!(!Placer::Champion.page_frame_pin_y());
        assert!(!Placer::FacingTrigger.page_frame_pin_y());
        assert!(!Placer::DividerRails.page_frame_pin_y());
        assert!(Placer::YSign.page_frame_pin_y());
        // It composes ON `flow-seed-v4` and changes nothing else, so an
        // A/B against the default isolates the frame correction.
        assert!(Placer::YSign.unified_roots());
        assert!(Placer::YSign.flow_seed_layering());
        assert!(!Placer::YSign.rail_gated_dividers());
        assert!(!Placer::YSign.unified_depth_roots());
        assert!(!Placer::YSign.sa_rotate_v5_gate());
        assert!(!Placer::YSign.facing_inverted_trigger());
        assert!(!Placer::YSign.m3_signed_gate());
        assert!(!Placer::YSign.m5_element_streams());
    }

    /// The V17 signal-direction filter is **dead on the default path**
    /// and on both control arms: it is gated on one accessor and nothing
    /// else, which is the whole byte-identity argument for the shipping
    /// output (`baseline_lock` is the empirical half).
    #[test]
    fn the_signal_direction_filter_is_absent_from_the_control_arm() {
        assert!(!Placer::FlowSeedV4.signal_direction_filter());
        assert!(!Placer::FlowSeedV4.signal_direction_filter());
        assert!(!Placer::FlowSeed.signal_direction_filter());
        assert!(!Placer::Champion.signal_direction_filter());
        assert!(!Placer::YSign.signal_direction_filter());
        assert!(!Placer::FacingTrigger.signal_direction_filter());
        assert!(Placer::SignalDirection.signal_direction_filter());
        // It composes ON `flow-seed-v4` and changes nothing else, so an
        // A/B against the default isolates the V17 filter.
        assert!(Placer::SignalDirection.unified_roots());
        assert!(Placer::SignalDirection.flow_seed_layering());
        assert!(!Placer::SignalDirection.page_frame_pin_y());
        assert!(!Placer::SignalDirection.rail_gated_dividers());
        assert!(!Placer::SignalDirection.unified_depth_roots());
        assert!(!Placer::SignalDirection.sa_rotate_v5_gate());
        assert!(!Placer::SignalDirection.facing_inverted_trigger());
        assert!(!Placer::SignalDirection.terminal_net_series());
        assert!(!Placer::SignalDirection.m3_signed_gate());
        assert!(!Placer::SignalDirection.m5_element_streams());
    }

    /// The composed readability arm is exactly the OR of its four
    /// constituents — every one of their accessors true, every other
    /// registered mechanism false — and every one of them is false on
    /// the default placer.
    ///
    /// The second half is the whole byte-identity argument for the
    /// shipping output: the composition adds no new mechanism, so if
    /// each constituent is unreachable by default then so is the
    /// composition. `baseline_lock` is the empirical half.
    /// `column-stubs-conet` is a COMPOSITION and adds nothing of its
    /// own: it answers `true` to exactly the union of the accessors its
    /// two components answer, and `false` to everything else they both
    /// answer `false` to.
    ///
    /// This is what makes the arm interpretable. If the composed arm
    /// could switch on a pass neither component switches on, then any
    /// difference between it and the union of its parts would be a new
    /// feature rather than an interaction, and grading it would not tell
    /// us whether the two constructions compose cleanly.
    ///
    /// Measured on the emitted output at the time this was written: the
    /// two components move DISJOINT fixture sets (`conet-layer-collapse`
    /// moves `compensated_divider` and `opamp_transimpedance`;
    /// `dc-column-node-stubs` moves six others), and on every one of the
    /// eight the composed arm's `.kicad_sch` is byte-identical to
    /// whichever single component moves it. Zero interaction. That is an
    /// empirical fact about today's fixtures, not a theorem — this test
    /// pins the accessor half, which is the half that can be proved.
    #[test]
    fn the_composition_is_exactly_the_union_of_its_parts() {
        type Acc = (&'static str, fn(Placer) -> bool);
        let all: &[Acc] = &[
            ("conet_layer_collapse", Placer::conet_layer_collapse),
            ("dc_column_node_stubs", Placer::dc_column_node_stubs),
            ("dc_series_columns", Placer::dc_series_columns),
            ("dc_series_columns_pinned", Placer::dc_series_columns_pinned),
            ("signal_direction_filter", Placer::signal_direction_filter),
            ("terminal_net_series", Placer::terminal_net_series),
            ("divider_node_series", Placer::divider_node_series),
            ("rail_gated_dividers", Placer::rail_gated_dividers),
            (
                "divider_tap_must_be_unloaded",
                Placer::divider_tap_must_be_unloaded,
            ),
            ("facing_inverted_trigger", Placer::facing_inverted_trigger),
            ("flow_seed_layering", Placer::flow_seed_layering),
            ("unified_roots", Placer::unified_roots),
            ("unified_depth_roots", Placer::unified_depth_roots),
            ("sa_rotate_v5_gate", Placer::sa_rotate_v5_gate),
            ("page_frame_pin_y", Placer::page_frame_pin_y),
            ("m3_signed_gate", Placer::m3_signed_gate),
            ("m3_property_text", Placer::m3_property_text),
            ("m3_signed_legalize", Placer::m3_signed_legalize),
            ("m5_element_streams", Placer::m5_element_streams),
        ];
        let (a, b, c) = (
            Placer::ConetLayerCollapse,
            Placer::DcColumnNodeStubs,
            Placer::ColumnStubsConet,
        );
        for &(name, f) in all {
            assert_eq!(
                f(c),
                f(a) || f(b),
                "column-stubs-conet must answer {name} as the union of \
                 its two components ({} || {})",
                f(a),
                f(b)
            );
        }
        // And it really is BOTH, not one of them wearing a new name.
        assert!(c.conet_layer_collapse() && c.dc_column_node_stubs());
        assert!(!a.dc_column_node_stubs());
        assert!(!b.conet_layer_collapse());
    }

    #[test]
    fn readable_v1_is_exactly_its_four_constituents() {
        type Acc = (&'static str, fn(Placer) -> bool);
        // The four arms' accessors, and the arm each belongs to.
        let constituents: &[Acc] = &[
            ("signal_direction_filter", Placer::signal_direction_filter),
            ("terminal_net_series", Placer::terminal_net_series),
            ("divider_node_series", Placer::divider_node_series),
            ("rail_gated_dividers", Placer::rail_gated_dividers),
            (
                "divider_tap_must_be_unloaded",
                Placer::divider_tap_must_be_unloaded,
            ),
            ("facing_inverted_trigger", Placer::facing_inverted_trigger),
        ];
        for &(name, f) in constituents {
            assert!(f(Placer::ReadableV1), "readable-v1 must enable {name}");
            assert!(
                !f(Placer::FlowSeedV4),
                "the retained control arm must not enable {name}"
            );
            assert!(
                !f(Placer::FlowSeed) && !f(Placer::Champion),
                "control arm must not enable {name}"
            );
        }
        // It composes ON `flow-seed-v4`, so the A/B against the default
        // isolates the four arms and nothing else.
        assert!(Placer::ReadableV1.unified_roots());
        assert!(Placer::ReadableV1.flow_seed_layering());
        // …and nothing beyond them. `y-sign` in particular is left OUT:
        // ADR-30 / ADR-31 leave it unresolved and its own prescription is
        // that it must land bundled with a re-tuning, so folding it in
        // would confound the attribution of everything else here.
        let excluded: &[Acc] = &[
            ("page_frame_pin_y", Placer::page_frame_pin_y),
            ("unified_depth_roots", Placer::unified_depth_roots),
            ("sa_rotate_v5_gate", Placer::sa_rotate_v5_gate),
            ("m3_signed_gate", Placer::m3_signed_gate),
            ("m3_property_text", Placer::m3_property_text),
            ("m3_signed_legalize", Placer::m3_signed_legalize),
            ("m5_element_streams", Placer::m5_element_streams),
        ];
        for &(name, f) in excluded {
            assert!(!f(Placer::ReadableV1), "readable-v1 must NOT enable {name}");
        }
        assert_eq!(Placer::from_name("readable-v1"), Some(Placer::ReadableV1));
    }

    /// Each constituent arm stays registered and runnable on its own.
    /// The composition is graded against them for attribution — ADR-23
    /// D12's prediction is that `divider-rails` unblocks vertical
    /// terminals `terminal-series-divider` alone cannot reach — and an
    /// A/B needs both sides to exist.
    #[test]
    fn every_constituent_arm_stays_separately_registered() {
        for name in [
            "signal-direction",
            "terminal-series-divider",
            "terminal-series",
            "divider-rails-strict",
            "divider-rails",
            "facing-trigger",
        ] {
            let p = Placer::from_name(name).unwrap_or_else(|| panic!("{name} is unregistered"));
            assert_ne!(p, Placer::ReadableV1);
            assert!(Placer::ALL.contains(&p));
        }
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
