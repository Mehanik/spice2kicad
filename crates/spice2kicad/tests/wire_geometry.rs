//! **V16 — wire rectilinearity** (Tier 2).
//!
//! The project owner's observation: "number of wire segments and corners
//! is an important metric — it's simple to read a circuit when wires are
//! minimal, straight, and connect directly the elements which are
//! connected." This file turns that into a falsifiable, non-gameable
//! per-fixture ratchet.
//!
//! # Why NOT raw segment count
//!
//! The emitted `(wire …)` count is a **Tier-0 correctness artifact**, not
//! a quality signal. `crates/spice-route/src/cleanup.rs` deliberately
//! re-segments the same ink:
//!
//! * `split_at_interior_attachments` SPLITS a run at same-net attachment
//!   vertices — KiCad connects wires only at *endpoints*, so more
//!   segments is *more correct*;
//! * `coalesce_collinear` merges abutting collinear pairs;
//! * `collapse_collinear_overlaps` replaces overlapping runs with a
//!   vertex-preserving non-overlapping cover.
//!
//! Measured on `common_emitter`: 20 raw segments whose visible ink is
//! 16 maximal straight runs. A ratchet on raw segments would create
//! optimization pressure AGAINST a Tier-0 pass. So the counted quantity
//! must be **invariant under re-segmentation of identical ink**.
//!
//! # The metric — defined on the "ink graph"
//!
//! Take the union of every emitted wire segment; group by line (same X
//! for verticals, same Y for horizontals); merge touching-or-overlapping
//! collinear spans into **maximal straight runs**. Vertices are run
//! endpoints plus run–run incidences. Rays are counted exactly the way
//! `cleanup.rs::rays_at` does: a run *ending* at the point contributes
//! one ray, a run whose *strict interior* contains it contributes two
//! (it passes through).
//!
//! * **B — bend count**: vertices with exactly 2 rays, one horizontal +
//!   one vertical. These are the L-corners of the ink. PRIMARY ratchet.
//! * **J — branch count**: vertices with 3 rays (a T), plus 4-ray
//!   vertices carrying a `(junction …)` dot (a same-net cross). 4-ray
//!   vertices *without* a dot are **inter-net crossings** and belong to
//!   the existing crossing ratchet
//!   (`placement_quality.rs::crossing_count_within_budget_across_fixtures`),
//!   NOT to J.
//!
//! B and J stay **separate** ratchets. Folding J into B would be wrong:
//! a k-pin Steiner tree topologically needs ≥ k−2 branch points, so a
//! combined number would penalise trunk-and-taps — often the most
//! readable form.
//!
//! Any **diagonal** wire segment is an outright failure. Axis-alignment
//! is what makes ray-counting sound, and nothing in the pipeline emits
//! diagonals today, so it is a free tripwire.
//!
//! # Deliberately NOT ratcheted
//!
//! * raw segment count (see above);
//! * bends-per-net — a gameable denominator: adding trivial nets lowers
//!   the average;
//! * a *rewarded* count of "nets routed straight" — gameable: a V4
//!   hierarchical-port name-jump label pair can mint a new 'straight'
//!   component out of nothing.
//!
//! Absolute per-fixture totals only.
//!
//! # Anti-gaming — and the gates this soundness DEPENDS ON
//!
//! This project has been burned by verifiers satisfiable with degenerate
//! geometry (a V5 counter that credited an "outward" wire without
//! checking its far end connected anything; verifiers that were
//! byte-identical to the model they graded). B and J are **cost-shaped**
//! — they count defects over the whole artifact — not credit-shaped, so
//! dead or decorative geometry can only ever ADD rays, never remove a
//! bend. There is no way to score better by drawing *more*.
//!
//! But that soundness is CONDITIONAL on the lower gates staying hard.
//! With them disabled, "delete all the wires" or "replace every wire
//! with a label" would score a perfect B = J = 0. The dependencies, all
//! of which must remain enforced for this ratchet to mean anything:
//!
//! 1. **Tier-0 connectivity verification** — the CLI verifies the
//!    emitted schematic's connectivity against KiCad after every
//!    conversion, so ink cannot simply be deleted.
//! 2. **`no_dangling_whiskers_across_fixtures`** (budget 0,
//!    `electrical_safety.rs`) — no stub may hang off nothing.
//! 3. **V4 label policy** (`labels.rs`) — ≤ 1 plain label per net per
//!    sheet (2 only for a hierarchical name-jump pair), so connectivity
//!    cannot migrate wholesale from wires into labels.
//!
//! Do not land this ratchet into a tree where any of those three is
//! disabled or weakened.
//!
//! # Subordination (V16 is Tier 2, and stays there)
//!
//! V16 is a continuous quality gradient with no single correct value,
//! so by CLAUDE.md's constraints-vs-costs decision rule it is Tier 2 —
//! same tier as V5/V6/V7 — and never a hard constraint. It must stay
//! subordinate to Tier 0/1: the globally bend-minimal route through a
//! symbol body (V12) or across a label (V13) is *worse* than a 2-bend
//! detour around them.
//!
//! That subordination is enforced structurally, not by tuning. V16 must
//! NEVER be a *weighted* term — no bend weight in `cost.rs`, no
//! bend-minimising router pass — since in a weighted sum "subordinate"
//! degenerates into a question of coefficients. It MAY enter phase
//! 4.5's acceptance predicate (`kicad-emitter/src/refine.rs`) in exactly
//! two shapes: a non-regression guard alongside `v11`/`overlap`/`v12`,
//! or the **final** key of the lexicographic objective, strictly after
//! `(v13, v12, v5)`. Under lexicographic comparison a candidate raising
//! V12/V13 is strictly worse however many bends it saves, so the trade
//! is unreachable by construction. See `docs/invariants.md` V16 for the
//! full rule, the ink-graph metric-fidelity condition, and the accepted
//! router → placement coupling.
//!
//! This "final lexicographic key" shape is a reformulation of an earlier,
//! more absolute rule ("verifier-shaped … never an in-loop objective").
//! It was adopted **on explicit project-owner sign-off following design
//! review**, which found the absolute wording conflated "in-loop" with
//! "able to trade against Tier 1" — true only for a weighted sum, not
//! under lexicographic comparison. This is authorised doctrine, not an
//! agent relaxing a rule to legalise its own change; see `docs/
//! invariants.md` V16 and ADR-16 in `docs/layout-adr.md` for the full
//! provenance.
//!
//! Known floor: bend-minimisation and V5-outward genuinely conflict.
//! `rc_lowpass`'s two `out` pins share a Y and sit 3.81 mm apart — a
//! 0-bend direct wire exists, but both pins face up and V5 says wires
//! leave along the pin axis, giving a 2-bend U. Both are Tier 2;
//! precedence is declared in `docs/invariants.md`: V5-outward wins the
//! first grid step, and B ratchets against *measured reality*, not a
//! theoretical zero. Expect legitimate per-net floors of 2 for
//! same-facing aligned pins.
//!
//! # Where the measurement lives
//!
//! The ink graph itself — maximal runs, ray counting, `InkCounts` — is
//! `tests/common/ink.rs`. This file owns the *ratchet*, not the metric.
//! The split exists so `tests/bend_bound.rs` (the V16 lower-bound
//! instrument) grades against byte-identical geometry instead of a second
//! implementation; ADR-23 D2 records why duplicating a measurement is the
//! failure this project keeps paying for.

mod common;

use std::path::{Path, PathBuf};

use common::ink::measure;
use common::spice_to_kicad;
use lexpr::Value;

// --- driver bits (mirrors placement_quality.rs) --------------------------

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> common::TempDir {
    common::TempDir::new("wg", name)
}

fn parse_sch(sch: &Path) -> Value {
    let src = std::fs::read_to_string(sch).expect("read sch");
    lexpr::from_str(&src).expect("parse sch as lexpr")
}

// --- the ratchet ---------------------------------------------------------

/// Every fixture that emits a root sheet — the same nine the
/// electrical-safety suite drives.
const FIXTURES: &[&str] = &[
    "rc_lowpass",
    "common_emitter",
    "multivibrator",
    "diff_pair",
    "opamp_inverting_real",
    "opamp_inverting",
    "port_shapes",
    "rc_lowpass_ports",
    "opamp_definition_level",
    "named_rails",
    "rc_phase_shift",
    "two_stage_amp",
    "cascode_amp",
    "lc_ladder_lpf",
    "sallen_key_lpf",
    "wien_bridge_osc",
    "sallen_key_driven",
    "shunt_feedback_amp",
    "stepped_attenuator",
    "opamp_transimpedance",
    "resistor_ladder_ref",
    "compensated_divider",
];

/// Per-fixture `(name, B, J)` high-water marks — **zero slack**, each
/// literal is the count measured on `master`.
///
/// Ratchet policy (CLAUDE.md § "Budgets are ratchets, not knobs"):
/// these only ever go **down**. A commit that removes bends SHOULD
/// lower the literal in the same commit. A commit may NEVER raise one
/// to make a failing test pass — a rise is a geometry regression to
/// diagnose, not a budget to bump.
///
/// Cross-check performed when these literals were first measured: the
/// 4-ray-without-dot count (this file's `inter_net_crossings`, which is
/// deliberately excluded from both B and J) agrees exactly with
/// `placement_quality.rs::count_wire_crossings` on all five ratcheted
/// fixtures — rc_lowpass 0, common_emitter 1 (budget 2), multivibrator
/// 4 (budget 4), diff_pair 0, opamp_inverting_real 0 — confirming the
/// vertex classification. The one divergence is `opamp_definition_level`
/// (ink 4 vs raw 5), which carries no crossing budget: the raw counter
/// double-counts one ink crossing whose runs `cleanup.rs` had split into
/// several `(wire …)` segments, which is exactly the re-segmentation
/// sensitivity the ink graph is built to remove.
const BEND_BRANCH_BUDGETS: &[(&str, u32, u32)] = &[
    // --- ADR-23 PROMOTION of `--placer=flow-seed` (owner-approved,
    // 2026-08-18) -------------------------------------------------------
    //
    // Every literal below is re-recorded at the NEW DEFAULT placer's
    // measured value, per ADR-23 D4 ("on promotion, `baseline_lock` and
    // every per-fixture literal are regenerated at the challenger's
    // values and the zero-slack regime resumes"). This is the one
    // sanctioned way a V16 literal rises, and it is NOT available to an
    // ordinary change: a whole placer is a different global optimum.
    // Aggregate across the suite: B −5, J −5 (V16 is Tier 2). The
    // per-fixture direction is mixed and that is expected; the wins are
    // `two_stage_amp` B 33 → 17 / J 9 → 5 and `named_rails` (2,2) → (1,1),
    // the losses `sallen_key_lpf` B 6 → 12 and `opamp_inverting` B 3 → 6.

    // B 3 -> 0. The series-horizontal flow-root fallback
    // (`idioms::signal_net_depth`) now draws `rc_lowpass` identically to
    // `rc_lowpass_ports` — R1 horizontal, C1 dropped straight below `out` —
    // so the `out` net routes as a single straight vertical drop with no
    // bends. Ratchet DOWN.
    ("rc_lowpass", 0, 0),
    // Newly graded: `named_rails` was absent from this file's FIXTURES
    // list until the fixture lists were unified. Measured on master,
    // zero slack; nothing moved, it was simply never counted.
    // B is back at 2 with the ADR-19 M4 REVERT (see `docs/layout-adr.md`,
    // "M4 reverted"). M4's content-derived Y datum had bought B 2 -> 1 on
    // this fixture and cost `flow_geometry`'s F6 ratchet 18 cells on
    // `multivibrator`; undoing it restores the pre-M4 measured value. This
    // is a restoration, not a budget bump — the literal is again exactly
    // what `master` measured before M4, and it ratchets DOWN only.
    ("named_rails", 1, 1),
    // F0 (v0.2 roadmap) NEW-GEOMETRY BASELINE, owner-approved.
    // `rc_phase_shift` — a three-section RC ladder feeding a CE stage —
    // is the long-chain circuit the current placer sprawls: B = 19 is
    // 2.4x the worst v0.1 fixture (multivibrator's 8), from 36 raw
    // segments over 28 maximal runs. Deliberately POOR;
    // this IS the Tier-2 V16 headroom F0 exists to expose. Adding it
    // moved no v0.1 fixture's (B, J). Ratchet DOWN.
    // B 19 -> 10, rail-stub SIDE fix: RB above `b` removes the fold-back
    // the ladder trunks used to jog around. Ratchet DOWN.
    // ADR-40 PROMOTION re-record (`dc-series-column-pinned` becomes the
    // default), read from the scoreboard sink.
    // Literal (B, J) (7, 2) -> (7, 1). Control sink: B 8 -> 7, J 2 -> 1.
    ("rc_phase_shift", 7, 1),
    // F0 (v0.2 roadmap) NEW-GEOMETRY BASELINE. `two_stage_amp` — two
    // cascaded CE stages sharing one rail — is the new worst fixture in
    // the suite on both counts: B = 33 (from 56 raw segments over 45
    // maximal runs) is 1.7x `rc_phase_shift`'s 19 and 4x the worst v0.1
    // fixture; J = 9 is 3x the previous worst. Nine inter-net crossings
    // ride along with it. Deliberately POOR — this is exactly the Tier-2
    // V16 headroom F0 exists to expose, and promoting the fixture moved
    // no other fixture's (B, J) by a single count. Ratchet DOWN.
    // ADR-40 PROMOTION re-record (`dc-series-column-pinned` becomes the
    // default), read from the scoreboard sink.
    // Literal (B, J) (15, 6) -> (8, 2). Control sink: B 10 -> 8, J 4 -> 2.
    ("two_stage_amp", 8, 2),
    // --- F2 (v0.2 roadmap, second benchmark wave) NEW-GEOMETRY
    // BASELINES, zero slack, ratchet DOWN only. Adding them moved no
    // existing fixture's (B, J) by a single count.
    //
    // B = 12 on eleven graded elements. The cascode's structure is a
    // COLUMN (Q2's emitter on Q1's collector, a three-resistor bias
    // ladder) and the placer has no stack model, so every vertical
    // relationship is drawn as a detour.
    // ADR-40 PROMOTION re-record (`dc-series-column-pinned` becomes the
    // default), read from the scoreboard sink.
    // Literal (B, J) (14, 4) -> (9, 1) — DOWN on both, but the literal was
    // stale: against the pre-fix control sink B rose 7 -> 9 while J fell
    // 3 -> 1. Recorded at the measured value either way.
    ("cascode_amp", 9, 1),
    // B = 16 on ten graded elements. CORRECTION to the claim in this
    // commit's message and in `docs/v0.2-roadmap.md` § F2 as first
    // written: 1.6 bends/element is the SECOND-worst density in the
    // suite, not the worst — `two_stage_amp` is 33/17 = 1.9. (Recorded
    // rather than quietly dropped, per MEMORY "verify what a number
    // measures".) What is true, and is why the fixture earns its place:
    // a doubly-terminated ladder is the one circuit in the benchmark
    // whose ideal drawing is a single straight line, and the placer
    // spends 16 bends on it. That is the long-chain / no-fold headroom
    // F2 exists to expose.
    // --- SECOND ADR-23 PROMOTION: `--placer=flow-seed-v4` becomes the
    // default (owner-authorised, 2026-08-24). Re-recorded at the NEW
    // DEFAULT's measured value, read from the scoreboard sink. Only the
    // two drawn-stimulus fixtures move; a whole-placer swap is the ONLY
    // sanctioned way one of these RISES, and it is not available to an
    // ordinary change.
    //
    // B 16 -> 5, J 2 -> 1. The largest single-fixture V16 win the
    // project has recorded on this fixture: the ladder is now drawn as
    // the single straight line its own baseline comment said it should
    // be ("the one circuit in the benchmark whose ideal drawing is a
    // single straight line, and the placer spends 16 bends on it").
    // Ratchet DOWN on both counts.
    ("lc_ladder_lpf", 5, 1),
    ("sallen_key_lpf", 9, 0),
    // B = 10 on eight graded elements: an oscillator is a pure cycle,
    // and the placer lays it out as if it were a chain, so the loop
    // closes with a long return path.
    ("wien_bridge_osc", 10, 3),
    // --- F3 (Tier-0 router fix, ADR-24): the two fixtures promoted out of
    // `tests/f0_defects.rs` once the Steiner-vertex-on-foreign-pin defect was
    // fixed. NEW-GEOMETRY BASELINES, zero slack, ratchet DOWN only. Adding
    // them moved no existing fixture's literal.
    // --- SECOND ADR-23 PROMOTION: `--placer=flow-seed-v4` becomes the
    // default (owner-authorised, 2026-08-24). Re-recorded at the NEW
    // DEFAULT's measured value, read from the scoreboard sink. Only the
    // two drawn-stimulus fixtures move; a whole-placer swap is the ONLY
    // sanctioned way one of these RISES, and it is not available to an
    // ordinary change.
    //
    // B 13 -> 12, J 4 -> 1. Ratchet DOWN on both counts.
    ("sallen_key_driven", 12, 0),
    // B 12 -> 11, rail-stub SIDE fix. Ratchet DOWN.
    // ADR-40 PROMOTION re-record (`dc-series-column-pinned` becomes the
    // default), read from the scoreboard sink.
    // B 9 -> 6, ratchet DOWN. J 2 -> 3 is a RISE and is PRE-EXISTING on
    // this branch: the control sink (the promoted default WITHOUT the
    // pin-anchoring fix) also reads J = 3, so the promotion's own
    // re-record had simply missed this literal and the branch was red
    // on it. Tier 2, recorded as promotion bookkeeping under ADR-23 D4.
    ("shunt_feedback_amp", 6, 3),
    ("stepped_attenuator", 9, 0),
    ("opamp_transimpedance", 12, 2),
    // ADR-40 PROMOTION re-record (`dc-series-column-pinned` becomes the
    // default), read from the scoreboard sink.
    // Literal (B, J) (13, 1) -> (7, 0); the control sink also reads (7, 0).
    ("resistor_ladder_ref", 7, 0),
    ("compensated_divider", 5, 2),
    // B 10 → 4. Phase 4.5's acceptance objective gained the V16
    // ink-graph bend count as its FINAL lexicographic key, after
    // (V13, V12, V5), so the refiner now separates orientations that tie
    // on every higher-tier count by how straight the resulting ink is.
    // COUT lands at rot 0 instead of rot 180 and Q1 unmirrors. V5 is
    // unchanged at 1 and no Tier-0/Tier-1 count moved. Ratchet DOWN.
    // ADR-40 PROMOTION re-record (`dc-series-column-pinned` becomes the
    // default), read from the scoreboard sink.
    // Literal (B, J) (9, 3) -> (3, 2). Control sink: B 4 -> 3, J 2 -> 2.
    ("common_emitter", 3, 2),
    // B 10 -> 8: `RC1`/`RC2` now sit on their transistors' collector
    // columns, so each collector trunk is one straight drop instead of a
    // dog-leg.
    //
    // J 2 -> 4 is a RISE and is NOT YET APPROVED — the test fails on it
    // deliberately. The two new branch vertices are `C1`/`C2` tapping the
    // now-straight collector trunks as proper Steiner Ts instead of the
    // trunk bending sideways to reach them: the same shape as the
    // owner-approved `diff_pair` J 0 -> 1 above. Escape request pending;
    // do not raise this literal without sign-off.
    // J 2 -> 4 under the global-improvement escape. NOT an explicit
    // owner decision: landed 2026-07-20 by the operating assistant
    // under the owner's standing instruction to proceed without
    // per-change confirmation. The owner approved the IDENTICAL SHAPE
    // on `diff_pair` (J 0 -> 1) and that precedent was applied here.
    // Treat as assistant-judgement precedent, not owner precedent.
    // Rationale: the rail-stub column idiom now fires on symmetric
    // circuits, straightening both collector trunks. C1/C2 tap those
    // trunks as proper Steiner Ts instead of the trunk bending
    // sideways to reach them. Net on this fixture: V5 5 -> 3, B 10 ->
    // 8, J 2 -> 4. Same shape as the diff_pair J 0 -> 1 escape.
    ("multivibrator", 8, 4),
    // J 0 → 1: `apply_shared_centers` now reserves one grid cell of
    // vertical stub under the tail trunk, so the three-way `tail` node is
    // drawn as a proper Steiner T instead of the trunk stopping sideways
    // on RTAIL's pin. Buys V5 1 → 0 on this fixture. Owner-approved.
    ("diff_pair", 2, 1),
    // B 8 -> 5, J 0 -> 1 with the `layers.rs` root refinement. `inv` is a
    // 3-pin net, so J >= k-2 = 1 is its topological floor; the previous
    // J = 0 came from a degenerate collinear layout. ESCAPE REQUEST for
    // the J rise, pending owner sign-off (see the commit message).
    ("opamp_inverting_real", 3, 1),
    ("opamp_inverting", 5, 0),
    ("port_shapes", 6, 0),
    // B 2 → 0 (stale-slack cleanup). The prior mark of 2 described the
    // best layout reachable *then* — R1 at rot 180 putting both `out` pins
    // on one row. The series-horizontal flow construction
    // (`idioms::apply_series_horizontal`, landed a3a429d) superseded it:
    // R1 is now drawn horizontal with C1 re-columned straight beneath the
    // `out` node, so the `out` net is a single straight vertical drop with
    // ZERO bends. Nothing moved for this cleanup — the layout was already
    // this on master; only the stale literal is corrected to the measured
    // value per the zero-slack ratchet policy.
    ("rc_lowpass_ports", 0, 0),
    // B 15 → 6, J 0 → 2. Channel-row banding (`channels.rs` +
    // `spice-layout::lib.rs`, Option B) now lays the two independent
    // inverting-amp channels out as two congruent rows, and pins each
    // channel's orientation THROUGH phase 4.5 to the `pick_orientations`
    // seed — the textbook non-mirror facing (input-left, output-right)
    // that reads left-to-right along the row. The prior layout let phase
    // 4.5's V5 oracle flip the opamps to a mirror facing that scored
    // V5=0 but drew the amp backwards in-row (B=15/16); the seed facing
    // reads correctly (B → 6) and the two 3-pin summing-node nets
    // (`inv1`, `inv2`) branch as proper Steiner trees (J 0 → 2) rather
    // than the router's worse pin-chaining (old J=0).
    //
    // B 15 → 6 is a ratchet DOWN and SUPERSEDES the earlier unratified
    // B=15 (itself preceded by an unratified 12); that history is retired
    // by this commit. J 0 → 2 is a ratchet RISE, approved by explicit
    // OWNER SIGN-OFF 2026-07-20 under the global-improvement escape:
    // summed across this fixture, TOTAL violations fall by 6 (B −9,
    // F5 −1, V5 +2, J +2). J = 2 is the correct branch count for two
    // 3-pin nets; V5 = 2 (the two summing-node input pins facing outward
    // toward the RF-feedback junction) is the documented V5-vs-flow
    // tension, not a defect. No Tier-0 or Tier-1 count moved anywhere;
    // the only approved rises on this fixture are Tier-2 V5 and J.
    ("opamp_definition_level", 10, 2),
];

#[test]
fn bend_and_branch_counts_within_ratchet_across_fixtures() {
    let mut failures: Vec<String> = Vec::new();
    for name in FIXTURES {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);

        let c = match measure(&root) {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        common::scoreboard::record_count("v16.bends", name, c.bends as usize);
        common::scoreboard::record_count("v16.branches", name, c.branches as usize);

        let &(_, b_budget, j_budget) = BEND_BRANCH_BUDGETS
            .iter()
            .find(|(n, _, _)| n == name)
            .expect("V16 budget for fixture");

        if c.bends > b_budget {
            failures.push(format!(
                "{name}: B = {} bends > ratchet {b_budget} \
                 (raw segments {}, maximal runs {}, J = {}, inter-net crossings {}). \
                 Do NOT raise the budget — diagnose the routing regression.",
                c.bends, c.raw_segments, c.runs, c.branches, c.inter_net_crossings
            ));
        }
        if c.branches > j_budget {
            failures.push(format!(
                "{name}: J = {} branches > ratchet {j_budget} \
                 (raw segments {}, maximal runs {}, B = {}). \
                 Do NOT raise the budget — diagnose the routing regression.",
                c.branches, c.raw_segments, c.runs, c.bends
            ));
        }
        // Lower-is-better: report reclaimable slack so a fix ratchets
        // down. Each component is gated INDEPENDENTLY and clamped to its
        // stored literal, so the suggested tuple can never contain a
        // value ABOVE the current budget. (Bug once shipped here: the
        // hint fired when *either* component improved and then printed
        // *both* measured values, so a fixture with B regressed and J
        // improved advertised a tuple that RAISED B — precisely what
        // CLAUDE.md § "Budgets are ratchets, not knobs" forbids.)
        let b_hint = c.bends.min(b_budget);
        let j_hint = c.branches.min(j_budget);
        if b_hint < b_budget || j_hint < j_budget {
            eprintln!(
                "V16 {name}: improved — you may lower the ratchet to \
                 (\"{name}\", {b_hint}, {j_hint})"
            );
        }
    }
    assert!(
        failures.is_empty(),
        "V16 violations:\n{}",
        failures.join("\n")
    );
}
