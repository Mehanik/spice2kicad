# Placer redesign — session work log

> A chronological record of the placer-redesign work session (2026-07-29 →
> 2026-08-04). Captures what was done, *why*, the measured outcome, and the
> commit for each step — including the negative results, which are the most
> valuable part. Starting HEAD was `e476d2a` ("docs: motivation & constraints
> for a placer redesign").

## TL;DR

- **ADR-19 (locality-first placer redesign)** was designed and partly built:
  M1 (locality verifier), M2 (signed footprint), M4 (n-independent Y datum)
  **landed**; M5′ (SA trajectory decoupling) was **measured and reverted** —
  proving the SA's netlist-sensitivity and its bend-finding are the *same*
  property.
- **A 4-track Opus research effort + adversarial red-team** produced
  `docs/target-architecture.md`: reject idiom-cells, pursue a Sugiyama-flow
  seed + generalised phase-4.5 joint-pose repair, and lean on the
  invariant/ratchet harness (which leads the published field).
- **v0.2 roadmap execution** began: Milestone A instrumentation (4 verifiers)
  and B1 (DC-path flow pass) landed; **B2/B3 reverted** with the finding that
  the flow architecture has *no lever* on the current tuned/narrow suite → the
  benchmark is the blocker.
- **F0 (expand the benchmark)** is in progress: 3 harder fixtures added; one
  (`two_stage_amp`) exposed a **real router VM-blowup / nondeterminism defect**
  under the test memory cap — currently under triage, **not yet committed**.

Every code change was gated by the project's zero-slack ratchets and the
ADR-16 baseline-diff protocol; every behaviour change that regressed a Tier-0/1
metric was reverted, not forced.

---

## Phase 1 — ADR-19: the locality-first redesign

The starting document (`docs/placer-redesign.md`) diagnosed two root engines:
**R-A** (global non-locality — adding one element re-bases the whole page) and
**R-B** (decoration reservation entangled with placement). It explicitly
declined to prescribe an architecture and warned that a prior big-bang attempt
(ADR-17) had been retired after expensive negative results.

Approach taken: **verifiers before intervention**, staged, each step reversible.

### M1 — locality-bound verifier — `1da6d3b`
- Wrote **ADR-19** (in `docs/layout-adr.md`): neighbourhood-relative coordinates
  behind a *tested* locality bound; keep the SA, don't claim determinism buys
  locality.
- Added `cache_less_placement_perturbation_within_bound` in
  `placement_stability.rs`: the R-A blast radius as a zero-slack ratchet
  (page-pan-normalised count of pre-existing user symbols moved when one element
  is added). **Corrected the doc's stale "17/17"** to the honest measured
  values: `rc_lowpass` = 0, `common_emitter` = 8. Pure verifier, no behaviour
  change.

### M2 — signed directional decoration footprint (unwired) — `cec3fd2`
- New `crates/spice-layout/src/footprint.rs`: a **signed** world-frame AABB
  (`SignedFootprint`) with body/pins, directional property text (Reference
  above / Value below), and one-sided glyph reach.
- Unit tests **proved the R-B thesis**: the directional footprint is a *subset*
  of the symmetric `.abs()` halo the SA gate uses (so wiring it in *relaxes* the
  gate → recalibration required), **and** the complete footprint *escapes* the
  halo (so the halo is genuinely incomplete). Unwired — no gate consumes it yet.

### M4 — content-derived, n-independent Y datum — `ed51164`
- The deepest R-A coupling: the old `y_bot = (n+4)·Y_RANK_STRIDE` scaled the
  whole page by element count. Replaced with **content-derived chained band
  datums** (each band's datum = previous + measured content depth + reach gap),
  Top stacks up / Bot stacks down (append-only → local), Mid sub-rows became
  absolute offsets, and `pack_rows` lost its `-total/2` re-centre.
- **Design delegated to a Fable agent** (per the user's standing instruction to
  route design forks to Fable). `MID_SUBROW_GAP = 16` was found empirically to
  be the routing-room floor (a median-10 pitch collapsed the signal band onto
  the rails, V16 bends 4→7 — "Y spacing is meaning").
- Result: locality `common_emitter` **8→7**, `rc_lowpass` 0; every ratchet held;
  `named_rails` V16 improved 2→1. Baseline regenerated under the ADR-16 protocol
  with the V16 before/after table in the commit. The cost-term re-expression
  Fable flagged as possibly-needed proved **unnecessary** (measured).

### K1 + M5′ — SA trajectory decoupling — `2e5d9e1`, `241599e`
- **K1 reseed control**: converting `common_emitter` twice with *different RNG
  seeds* (same netlist) moved 7/7 symbols → the residual blast radius looked
  like *spurious* trajectory chaos, suggesting it was containable.
- **M5′** implemented that hypothesis: private per-element RNG streams keyed on
  refdes + a deterministic sweep, to make each element's proposal sequence
  netlist-stable.
- **Measured, then REVERTED** (kill criteria K2 *and* K3): it bought **no
  locality** (still 7/7 — the acceptance cascade through the cost terms
  dominates, not the proposal RNG) **and** destroyed bend-finding
  (`common_emitter` V16 B **4→11**, +3 fixtures). **The decisive finding: the
  SA's netlist-sensitivity and its basin-finding are the same property** — you
  cannot re-key one without wrecking the other. R-A locality is at its
  achievable frontier: seed local (M4), SA residual inherent, users unaffected
  (the ADR-4 cache gives 0 movers).

---

## Phase 2 — research: what is the *right* architecture?

The user asked for deep research toward the long-term goal (best-in-class +
human-level schematic drawing). Ran **four parallel Opus research tracks**, an
**adversarial red-team**, and a **decisive spike**, synthesised into
`docs/target-architecture.md` — `3f774be`.

**The four tracks (all cited in the doc):**
1. *Graph-drawing literature* — Sugiyama/layered (ELK, Brandes–Köpf), orthogonal
   (Tamassia TSM, Kandinsky), constraint-based (IPSep-CoLa, Cassowary), routing
   (libavoid, FLUTE). Verdict: our three walls are faces of one cause (a single
   global stochastic objective); the field's answer is staged deterministic
   structure-first decomposition.
2. *Analog-EDA / motifs / ML* — Weave (2026, the on-topic netlist→schematic
   system), MAGICAL/ALIGN (motif→*constraints*, not templates), DC-path
   direction, ~88% recognition ceiling everywhere, ML premature as a core.
3. *Open-source teardown* — **no mainstream tool does netlist→placed-schematic**;
   the closest (Weave) *punts* the aesthetic tier we already have; locality and
   flow-orientation are genuinely *open* problems (everyone globally re-bases;
   every tool punts series-element orientation).
4. *First-principles rethink* — our V1–V16 are all *negative* constraints;
   human-level quality is *positive gestalt* (grouping, idiom-canonical
   sub-drawings, narrative order, feedback-as-span) that a flat 2-scalar model
   can't represent.

**The red-team overturned the ambitious two-tier idiom-cell design** using our
own data: the load-bearing bends are inter-cell *seams* not idiom interiors;
analog idioms *share devices* so canonical templates conflict at the shared
transistor; R-B decoration is routing-dependent so it can't be reserved
pre-route. Probabilities: two-tier → human-level **~15%**; the leaner design
**~65–70%** at ¼ the effort.

**Spike 0 confirmed it (RED, as predicted):** `common_emitter`'s bend-carrying
nets (`b`, `c`, `e`) are each irreducibly inter-block (bias + coupling + gain
device), so "bake basins into idiom cells" bakes in the parts that were never
the problem.

**Recommendation adopted:** Sugiyama-layered seed (rails out of graph, DC-path
flow, feedback-arc *marking*, lcapy per-axis spacing) + generalise phase-4.5 to
bounded joint-pose repair + lean on the invariant/ratchet moat. Idiom cells
rejected.

**Where we already lead the field** (do not throw away): the aesthetic/ratchet
layer, locality (our ADR-4 cache + M4), and the real-router phase-4.5 oracle.

---

## Phase 3 — v0.2 roadmap execution

Wrote `docs/v0.2-roadmap.md` (`99fa8c7`): Milestone A (instrumentation,
verifiers-first) → B (signal-flow foundation) → C (layered seed) → D
(phase-4.5 joint-pose repair) → E (structural metrics). Each behaviour change
under the ADR-16 protocol; commit each green step. Executed step-by-step with
Opus agents; each step verified and committed individually.

### Milestone A — instrumentation (all pure test-only adds)
| Step | Metric | Commit |
|---|---|---|
| A1 | **Q3 flow-monotonicity** — forward-edge left→right violations vs the placer's own layer model. Zero-slack ratchet (common_emitter 1, multivibrator 4, diff_pair 2, named_rails 1, rest 0). | `b93f557` |
| A2 | **Round-trip connectivity certificate** — reconstruct the full net partition from emitted geometry and compare to source (catches silent merges *and* splits; complements local V11). Tier-0, green on all 10. | `479129e` |
| A3 | **Q5 mutual-alignment near-miss** — shared-net pairs within 2 cells of an axis but aligned on neither (a pre-routing leading indicator of bends). Zero-slack ratchet. | `b5d87ad` |
| A4 | **Q6 balance** — CoV of component occupancy. Found too noisy on small fixtures to gate on, so kept as an **honest degeneracy tripwire + informational tracker**, not a zero-slack ratchet (per the V16 doctrine against noisy gates). | `5c02b4c` |

### Milestone B — signal-flow foundation
- **B1 — DC-path signal-flow pass (computed, unconsumed)** — `579c9c6`.
  New `crates/spice-layout/src/flow.rs`: a directed signal-flow graph from
  DC-path device direction (BJT c→e, MOSFET d→s, diode a→c, source +→−),
  passives oriented by traversal, and feedback arcs found by inline Eades greedy
  feedback-arc-set and **MARKED, never reversed** (fixing the `break_cycles`
  bug). Not called from the pipeline yet → `baseline_lock` byte-identical (zero
  behaviour change). 4 unit tests incl. inverting-opamp feedback marked-not-
  reversed.

- **B2/B3 — wire flow into layering — ATTEMPTED, REVERTED** — finding recorded
  in `619cc31`. Two variants measured, both reverted (tree clean). **The
  load-bearing discovery:** the flow architecture has *almost no lever* on the
  current suite because:
  1. **Every fixture already takes `no_source_fallback`** — all stimulus sources
     carry `;@ ignore` and are dropped before resolve, so `break_cycles`'
     feedback reversal is **dead code** on every real fixture.
  2. **Reseeding layers from the flow graph regressed V16** on the non-feedback
     fixtures (`common_emitter` B 4→6, `named_rails` 1→3) — the hand-tuned BFS is
     better ("Y spacing is meaning" again).
  3. **The only feedback case (the op-amp) is a `.subckt` sheet** that `flow.rs`
     treats as directionless, so its feedback resistor is never marked → the
     flow work is a no-op there.

  **Strategic consequence:** the 10-fixture suite is small, hand-tuned to
  near-optimal, source-ignored, and feedback-hidden — and its zero-slack
  ratchets then *actively block* any architectural change that touches those
  fixtures (the local-optimum freeze applied to the redesign itself). **The
  blocker is the benchmark, not the algorithm.**

---

## Phase 4 — F0: expand the benchmark (IN PROGRESS, uncommitted)

Owner chose **F0** as the unblocker: add harder fixtures the current placer
draws poorly, so the flow architecture has a lever and headroom to ratchet
quality *down*. An Opus agent added 3 fixtures (test + fixtures only; production
untouched, verified):

| Fixture | Topology | Purpose |
|---|---|---|
| `shunt_feedback_amp.cir` | CE BJT with collector→base feedback resistor | Visible feedback the flow graph can **mark** (no subckt) |
| `two_stage_amp.cir` | Two cascaded CE stages | Multi-stage left→right sprawl |
| `rc_phase_shift.cir` | 3-section RC ladder → CE gain stage | Long rooted chain |

All `*@port`-rooted (giving the layered algorithm a flow root without drawing a
stimulus). Registered across 16 fixture-enumerating test tables with measured
Tier-2 baselines (deliberately poor — `two_stage_amp` B=18, `rc_phase_shift`
F6=27 lateral cells — that's the reclaimable headroom). Five known-deferred
Tier-1 placer defects re-exposed by the denser layouts were handled with scoped
per-fixture skips (the gate stays a hard 0 for all 10 v0.1 fixtures).

*Precision note on that mechanism.* They are commented `continue` arms inside the
fixture loops, each self-described as "equivalent to a scoped `#[ignore]`". They
are not literal `#[ignore]`s, and the difference is load-bearing in two ways:
a `continue` reports as **passed** in the tally (an `#[ignore]` reports as
ignored, so it stays visible), and neither form carries an **unexpected-pass
tripwire** — the day the underlying placer defect is fixed, the skip silently
survives and the fixture is never re-graded. Before F0 lands, convert these to an
explicit XFAIL registry that fails when a registered fixture starts passing.

### Open issues found during verification (why F0 is not yet committed)
1. **`two_stage_amp` exposes a REAL router defect** — its conversion needs
   **>8 GB of virtual memory** and is nondeterministically OOM-killed under the
   test memory cap (4 GB default / 8 GB here). Per CLAUDE.md this is a defect to
   diagnose ("unbounded router segment growth"), not a ceiling to raise. A
   fixture that OOM-kills the process cannot live in the committed suite as-is.
   **Under triage** — likely resolutions: simplify the fixture below the blow-up
   threshold, or pull it out and file the router VM-growth defect separately with
   this fixture as the repro. `shunt_feedback_amp` and `rc_phase_shift` both
   convert cleanly and deterministically under the 4 GB cap.
2. **~~Pre-existing failure, NOT caused by F0~~ — CORRECTED: this session's own
   M4 caused it.** `flow_geometry::stub_lateral_run_within_ratchet` fails on
   **`multivibrator`** (F6 = 18 vs budget 2). The original entry here claimed it
   "fails identically on pristine HEAD — a latent defect that predates the entire
   session". **That is false.** The control arm was the session's *own* HEAD
   (`619cc31`), not pre-session master, so it could only ever confirm "not caused
   by F0" — it could not see a defect introduced earlier in the same session.
   Re-bisected against the true pre-session baseline, same command and machine:

   | commit    | what                          | F6 test |
   | --------- | ----------------------------- | ------- |
   | `e476d2a` | pre-session master            | PASS    |
   | `cec3fd2` | ADR-19 M2 (signed footprint)  | PASS    |
   | `ed51164` | ADR-19 M4 (n-indep. Y datum)  | **FAIL** |
   | `619cc31` | session HEAD                  | **FAIL** |

   So **M4 introduced it**, and ADR-19's M4 row claiming "every ratchet green" is
   wrong for the same reason. F0 is indeed innocent — but master has been RED
   since `ed51164`, and every A1–A4 ratchet recorded after it was measured on a
   regressed tree. Note that ADR-19's own staging table makes **M3 (wire
   footprint into the SA gate) a precondition for M4** ("the footprint precedes
   any spacing change, it is not a follow-up"); M3 was skipped and M4 landed
   anyway. **RESOLVED — M4 reverted** (`835e073`, merged). No targeted repair
   exists: sweeping `MID_SUBROW_GAP` gives a non-monotone response across
   fixtures (16→4/**18**/6, 14→4/2/4, 12→**7**/2/3, 10→**5**/2/4, 8→4/2/**9**)
   in which only 14 is all-green, and it passes by *one cell* of Manhattan
   tie-break margin on the very anchor-flip that caused the failure. Every
   other variant trades one fixture for another, which the within-tier rule
   forbids. `baseline_lock` is now byte-identical to `cec3fd2`, independently
   confirming the geometry is exactly pre-M4. M4's code is preserved on
   `wip/adr19-m4-pending-m3` for the post-M3 re-attempt. Full mechanism and
   measurement tables: `docs/layout-adr.md` § "M4 reverted".

   *Method note (the durable lesson).* This is the `verify-what-a-number-
   measures` failure mode again: a control arm chosen from inside the change
   window cannot falsify a claim about that window. Bisect to a commit that
   predates ALL of the work under review.

---

## Commit ledger (this session, on branch `adr19-locality-verifier`)

```
619cc31 docs(v0.2): B2/B3 reverted — flow model has no lever on the tuned suite
579c9c6 feat(layout v0.2 B1): DC-path signal-flow direction pass (computed, unconsumed)
5c02b4c test(v0.2 A4): Q6 balance as degeneracy tripwire + informational tracker
b5d87ad test(v0.2 A3): Q5 mutual-alignment near-miss verifier (verifiers-first)
479129e test(v0.2 A2): round-trip connectivity certificate (whole-file V11)
b93f557 test(v0.2 A1): Q3 global flow-monotonicity verifier (verifiers-first)
99fa8c7 docs: v0.2 structure-first placer implementation roadmap
3f774be docs: target-architecture research synthesis (4 Opus tracks + red-team)
241599e docs(adr-19): M5′ attempted, measured, REVERTED — SA re-keying is basin destruction
2e5d9e1 docs(adr-19): record K1 reseed result, pivot M5→M5′ (SA trajectory decoupling)
ed51164 feat(layout): ADR-19 M4 — content-derived, n-independent Y datum
cec3fd2 feat(layout): ADR-19 M2 — signed directional decoration footprint (unwired)
1da6d3b feat(layout): ADR-19 design + M1 cache-less locality-bound ratchet
e476d2a  (pre-session HEAD)
```
Then, restoring green master after the M4 regression above:
```
9881d4f test(v0.2 A3): reclaim Q5 slack freed by the M4 revert
835e073 Revert "feat(layout): ADR-19 M4 — content-derived, n-independent Y datum"
```
Verified independently at `9881d4f`: **68 binaries, 765 passed, 0 failed,
7 ignored** (`--no-fail-fast`, full workspace).

**F0 is no longer in the working tree** — it is preserved as a single
object-level snapshot commit on **`wip/f0-benchmark-expansion`** (`bf7ba24`;
3 fixtures + 16 test files + this log), so it survives outside `/tmp`. It is
**not landed**, and three things must happen before it can be:
1. **Re-measure every baseline.** They were taken on the `ed51164` (M4) tree,
   which no longer exists; `baseline_lock` in particular is wholly stale.
2. **Drop `two_stage_amp` and file the router VM-growth defect** with it as the
   repro — it needs >8 GB VM and is nondeterministically OOM-killed. Per
   CLAUDE.md that is a defect to diagnose, not a ceiling to raise, and the
   fixture must not be slimmed to hide it.
3. **Replace the `continue` exclusions with an XFAIL registry** that fails when
   a registered fixture starts passing (see the precision note above).

## Key durable findings (the negative results are the asset)

1. **Determinism is not locality, and netlist-stable keying is not locality
   either** — it is basin destruction (M5′). The SA's edit-sensitivity *is* its
   basin-finding.
2. **The blocker for the flow architecture is the benchmark**, not the
   algorithm — the tuned/narrow/source-ignored suite gives it no lever.
3. **We lead the published field** on the aesthetic/ratchet layer, locality, and
   the real-router oracle; idiom-cells were rejected on measured evidence.
4. **A dense real fixture (`two_stage_amp`) surfaced a latent router VM-blowup**
   the small v0.1 fixtures never triggered — itself a vindication of the
   "expand the benchmark" step.

## Recommended next steps
- Resolve `two_stage_amp`: diagnose the router VM growth (a real v0.2
  robustness item) or slim the fixture below the threshold; then commit F0.
- With a benchmark that has headroom, **retry B2/B3** (flow into layering) and/or
  add **B1′** (sheet-instance flow edges so op-amp feedback becomes visible).
- **Promote Milestone D** (generalise phase-4.5 to bounded joint-pose repair) —
  the red-team's highest-value item; targets the real `COUT`/`RIN`-drawn-vertical
  defect and needs no flow lever.
