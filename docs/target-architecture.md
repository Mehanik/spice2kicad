# Target architecture research synthesis (toward best-in-class + human-level)

> Status: **research synthesis**, in progress. Distilled from four parallel
> Opus research tracks (graph-drawing literature; analog-EDA/motif/ML;
> open-source tool teardown; first-principles domain rethink) plus an
> adversarial red-team, run 2026-07-30. This file records *what the field
> knows*, *where we already lead it*, and the *target architecture* the
> evidence points to. The recommendation section is finalized after the
> red-team verdict; treat the architecture as a proposal pending an
> owner-signed spike (§7), not sanctioned doctrine.

---

## 1. The one finding all four tracks reached independently

Our three walls — (W1) **global re-basing / no locality**, (W2)
**flow-orientation proxy fighting the router**, (W3) **decoration
reservation entangled with placement** — and the sub-human *ceiling* are
**four faces of one root cause**: a **flat, global-ordinal skeleton**
(`band ∈ {Top,Mid,Bot}`, scalar `layer` via longest-path) converted to
metric coordinates and then polished by a **single global stochastic
objective** (the SA). The literature's consistent answer is the opposite
shape: **staged, deterministic, structure-first decomposition**, in which
each combinatorial choice (flow direction, idiom pose, feedback span) is
fixed *structurally and attributably* and any continuous optimizer is
demoted to a bounded local polish.

This is not one paper's opinion — it is the convergence of the
Sugiyama/ELK layered-drawing literature, the analog-EDA motif-recognition
lineage, the teardown of every working tool, and a from-scratch
first-principles critique. They disagree only on emphasis, not direction.

## 2. Where we already LEAD the published field (do not throw away)

The teardown is unambiguous: **no mainstream EDA tool or simulator does
netlist → placed-schematic at all** — the entire commercial/OSS ecosystem
is schematic → netlist. The prior art is a small research cluster, and on
the axes that matter we are ahead of it:

- **Aesthetic / ratchet layer (V1–V16).** The closest working system,
  **Weave** (arXiv 2607.03835, 2026 — deterministic SPICE→LTspice via
  Sugiyama), and the ASG constraint-optimizer, **explicitly punt** crossings,
  bends, orientation, and visual similarity — "the cost of this design is
  aesthetic." Our invariant + zero-slack-ratchet system is machinery the
  field does not have.
- **Locality.** Weave, every ELK-based viewer, ASG, MAGICAL, ALIGN all
  **globally re-base** on any edit — independently confirming our ADR-17
  finding that spacing-derived placement is intrinsically global. Our
  ADR-4 position-stability cache and ADR-19 M4 (seed now moves ~1 element)
  are **ahead of everyone**; there is nothing to borrow here, only to
  invent. The M5′ negative result (the SA's netlist-sensitivity *is* its
  bend-basin-finding) is corroborated by the AlphaChip replication war
  (tuned SA matches learned RL even with a clean reward — arXiv 2306.09633
  vs 2411.10053): **SA-as-polisher is the right stance.**
- **The real-router phase-4.5 oracle** and the lexicographic acceptance
  tuple — the field's placers optimize routing *proxies* and pay for it
  (this is literally W2); our using the real router as the oracle is the
  correct instinct, just currently confined to orientation-at-fixed-position.
- **Annotations-over-heuristics + the `*@align`/`*@place` escape.** *Every*
  symmetry/motif extractor in the literature tops out near **~88% accuracy**
  (S3DET 88.3%; Weave 88.4% connectivity) and *every* mature toolchain
  (MAGICAL, ALIGN) still ships a user-constraint override file. Our design
  choice is externally validated as correct, not a stopgap.

## 3. Where the field offers a clear upgrade we currently LACK

- **Principled signal-flow.** Our X-layers are a hand-rolled Sugiyama layer
  assignment with a `no_source_fallback` hack that (per `placer-redesign.md`
  §7) turns the flow cost *off* on every real fixture. The field's method:
  **DC-path device-direction inference** (traverse supply→ground in
  current-flow direction — drain→source, collector→emitter; TUM ESFG
  dissertation) to build a *directed* signal graph, then **greedy
  feedback-arc-set** to MARK (not reverse) feedback edges. This is a real
  answer to W2's foundation and to Wall-2 (feedback spans). Rust:
  `petgraph::greedy_feedback_arc_set`.
- **Idiom / building-block recognition → constraints.** Recognize a
  diff pair / current mirror / cascode from **topology only** (deterministic
  discriminators: *shared-gate-tied-to-a-drain = current mirror;
  shared-source-with-gate-inputs = diff pair*) and **emit symmetry/alignment
  constraints** the placer consumes — the field never emits "drawing
  recipes," it emits constraints (MAGICAL, ALIGN, Graeb sizing-rules
  TCAD 2008). This auto-generates the annotations we currently ask the user
  for, on the ~88% it can recognize, and is the mechanism for human-level
  idiom gestalt. A canonical "cell" is simply the richest end of this
  spectrum (a full relative-pose constraint bundle) — so *constraints* and
  *templates* are the same mechanism at two granularities.
- **Rails fully out of the layout graph.** Weave classifies nets first and
  **never lets ground/supply enter the layout graph** — they become flags.
  We do this for glyphs (V10/V14) but rails still perturb layering; going
  further is the single highest-leverage legibility lever.
- **lcapy's decoupled per-axis longest-path placer.** Once orientation is
  fixed, solve X and Y as **two independent 1-D longest-path (PERT)**
  problems with min-distance edges + a `stretch` flag distributing slack
  uniformly. This *formalizes our own "X spacing is slack; Y spacing is
  meaning"* finding, geometry-number-free, and is a clean deterministic
  seed for the SA polisher (LGPL — reimplement the technique).
- **Round-trip connectivity certificate** (Weave): reconstruct nets from
  emitted geometry on both sides and compare partitions — a whole-file V11,
  worth adopting as an output-side ratchet.

## 4. What the field does NOT solve (so we must invent, or accept)

- **Locality (W1)** — unsolved by everyone; our cache + M4 lead. The
  achievable frontier we already reached (seed local; SA residual inherent;
  users get 0 via the cache) is consistent with the entire field.
- **Series-element orientation from bare connectivity (W2)** — *punted by
  every tool*: lcapy demands a per-part orientation hint, SKiDL guesses and
  needs cleanup, Weave doesn't address it, SKiDL's author **tried ELK and
  rejected it** because analog lacks the I/O directionality layered layout
  assumes. Our flow-orientation wall is a genuine **open research problem**,
  not a solved thing we failed to copy.
- **Learned core placement** — premature: no "readable schematic" reward
  exists, and no board-level (netlist → human-KiCad-layout) corpus exists
  (public data is ~hundreds of analog-IC images for the *inverse* task).
  Defensible ML shape is a **GNN/LLM prior generator** (suggests flow
  direction / symmetry groups) feeding the deterministic solver, or an
  **offline authoring aid** (draft cell templates, mine the motif catalog,
  hand-verified into fixtures) — never a stochastic black box on the online
  path.

## 5. Human-level quality is a metric gap, not just an algorithm gap

V1–V16 are almost all **local negative constraints** ("don't overlap /
cross / mis-orient"). Human quality is dominated by **global positive
gestalt** that we do not measure. Zeroing every V-invariant is the *floor*
of acceptability, not the ceiling. The missing, falsifiable properties:

- **Q1 functional grouping** — cluster cohesion (`bbox_area/n` small) +
  separation (gap to nearest cluster ≥ k·pitch). V6's bands actively fight
  this by interleaving unrelated elements at the same layer rank.
- **Q2 idiom-canonical sub-drawings** — graph-match a placed sub-layout to
  a canonical template; score geometric edit distance. The single largest
  miss; V7 (symmetry) is a thin special case.
- **Q3 narrative reading order** — fraction of signal-DAG edges that go
  left→right; supply→ground top→bottom. We gesture per-layer but never
  score global monotonicity (and the flow term is off under
  `no_source_fallback`).
- **Q4 feedback-as-span** — a bridging element's body lies outside its two
  endpoints' forward-path hull, on the supply side (this is Wall 2 stated
  as a positive convention).
- **Q5 mutual alignment** — count near-miss alignments a human would snap.
  Grid snap gives grid alignment, not mutual alignment.
- **Q6 visual balance** — density variance across the page; centroid offset
  from center. V15 only checks on-page, not balanced.

These must enter the suite **verifiers-first** (measure master → set
high-water marks → let structure drive them down), obeying the V16 doctrine
(a counted quantity, never a coefficient that can trade against Tier-1).

## 6. Sources (primary, per track)

Layered/Sugiyama core: Brandes & Köpf, *Fast and Simple Horizontal
Coordinate Assignment*, GD 2001 (linear-time, deterministic) · ELK
*Layered* (arXiv 2311.00533; eclipse.dev/elk) · **Weave**, arXiv 2607.03835
(the on-topic netlist→schematic system). Orthogonal/bends: Tamassia,
*Minimum-bend embedding*, SIAM J. Comput. 1987 (bends = one min-cost-flow
optimum jointly with shape — the structural cure for the two-oracle W2);
Kandinsky/Fößmeier–Kaufmann for degree>4 parts. Constraints/locality:
Dwyer, Koren & Marriott, *IPSep-CoLa*, IEEE TVCG 2006 · Cassowary
(incremental) · SetCoLa (set-level rules → per-node constraints).
Routing: Wybrow, Marriott & Stuckey, *Orthogonal Connector Routing* /
libavoid, GD 2009 · Chu & Wong, *FLUTE*, IEEE TCAD 2008 (optimal Steiner
for ≤9-pin nets). Flow/direction: TUM ESFG dissertation (DC-path direction
+ feedback breaking) · `petgraph::greedy_feedback_arc_set`. Motifs:
Massier/Graeb, *Sizing Rules Method*, IEEE TCAD 2008 (deterministic
blueprint) · ALIGN (github.com/ALIGN-analoglayout/ALIGN-public) · S3DET,
ASP-DAC 2020 (88.3%). Aesthetics: Purchase, *Metrics for Graph Drawing
Aesthetics*, JVLC 2002 (crossings #1, bends #2 — validates our tiers).
Algorithm to borrow: lcapy `schemgraph.py`/`schemplacer.py` (decoupled
per-axis longest-path). Negative results: SKiDL (ELK rejected for analog);
ASG (constraint-optimizer plateaus like our SA); AlphaChip replication war
(SA ≈ learned RL).

---

## 7. Target architecture & staged path (post-red-team)

The four tracks first pointed at an ambitious **two-tier design** (a
neighbourhood-anchor substrate + a topology-recognized **idiom-cell** layer
carrying canonical templates). The adversarial red-team **refuted its core
claims against our own measurement record** (§8). The recommendation is
therefore the **leaner, evidence-grounded design**, with idiom cells
*deferred behind a cheap decisive spike that is expected to come back red*.

### 7.1 What to build (ranked, incremental, each a ratchet you can back out)

**Keep unchanged (proven right — do not touch):** the SA as basin-finding
polisher (M5′/ADR-17 proved it irreplaceable); the invariant + zero-slack
ratchet harness (**this is the moat — the published field has no aesthetic
layer**); pinning; the real-router phase-4.5 oracle; the lexicographic
acceptance tuple; the ADR-15 role model (validated as a discriminator, only
its enforcement failed); the ADR-4 position-stability cache (ahead of the
field on locality). R-A locality is **already at its achievable frontier**
(M1+M4; users get 0 via the cache; the SA residual is inherent to *any*
spacing-derived placement) — stop spending on it.

1. **Sugiyama-layered seed replacing `bands → layers`.** Rails classified
   fully out of the layout graph (extend V10/V14 — rails never perturb
   layering); **DC-path device-direction inference** (drain→source,
   collector→emitter; TUM ESFG) for a *directed* signal graph;
   **feedback-arc-set to MARK feedback edges (not reverse them)** — fixing
   `layers.rs:383 break_cycles`' silent reversal, the structural bug behind
   Wall 2; **lcapy-style decoupled per-axis longest-path** spacing (formalises
   our "X slack / Y meaning"). Rust: `petgraph::greedy_feedback_arc_set`.
   *Buys:* feedback drawn as spans (Wall 2 — unreachable by a scalar layer
   today), a principled flow the current `no_source_fallback` hack lacks, and
   a cleaner deterministic SA seed. Empty-baseline-per-fixture is NOT
   expected — this is a layout change under the ADR-16 protocol.
2. **Generalise phase 4.5 from orientation-only to bounded JOINT-POSE local
   repair** (position + orientation + mirror over a small neighbourhood),
   still against the **real router**, still gated by the lexicographic tuple
   + `severed`/V11/overlap/V12 guards. *This is the actual Wall-1 fix* — it
   reconciles flow and routing "holistically" (as `placer-redesign.md:229`
   demands) by letting the one trusted oracle move *pose*, not just
   orientation-at-a-fixed-position (the move ADR-15 Stage-5 proved
   insufficient). No idiom recognizer required. (= ADR-19 M6, now the
   centrepiece, not the tail.)
3. **Human-level metrics as verifiers-first ratchets** (§5 Q1–Q6): measure
   master → set high-water marks → let the seed + repair drive them down.
   Each a counted quantity, never a coefficient (V16 doctrine).
4. **Adopt the round-trip connectivity certificate** (Weave §verification) as
   an output-side V11 gate strengthening the baseline-diff protocol.

### 7.2 R-B (decoration) — reframed honestly

Cells do **not** solve R-B, and "signed-*complete* footprint at placement
time" is the R-B chicken-and-egg renamed: label and PWR_FLAG geometry is
**routing-dependent and decided emitter-side after placement**, so it cannot
be fully reserved before routing. The realistic options, none of which is
idiom cells: (a) accept the *partial* signed footprint (M2) and let the
generalised phase-4.5 repair (item 2) clean the residual label/glyph
collisions against the real router — the repair sees real decoration; or
(b) a bounded **place → route → re-reserve → repair** iteration. Pick under
measurement; do **not** wire the partial footprint into the SA gate expecting
completeness (the ADR-17 Stage-2 "relocates collisions" kill).

### 7.3 Idiom cells — DEFERRED, gated on two spikes (expected red)

Do **not** build the idiom recognizer or cell abstraction until:
- **Spike 0 (≈1 hr, no code, DO FIRST):** label where `common_emitter`'s 4
  and `opamp_inverting`'s 3 SA-found bends live — idiom *interior* or
  inter-cell *seam*? If ≥ half are seam bends (strong prior: R2/Q1/
  coupling-cap/rail relationships), "bake basins into cells" is refuted
  outright and Tier B dies for the cost of an afternoon.
- **Spike 1 (only if Spike 0 survives):** hand-build diff-pair + cascode +
  mirror templates for one telescopic-cascode OTA and attempt primary-cell
  assignment; the shared-device pose conflict (§8.2) is expected immediately.

The single most recognizable idiom gestalt — the diff-pair mirror — is
**already delivered by the V7 symmetry pass** (topological pair detection,
no templates), so Tier B's marginal gain is the *last 20%* (canonical
silhouette) at 5× the risk, and it is the 20% that hits the composition rock.

## 8. Adversarial red-team verdict (the decisive input)

The red-team grounded every point in our measured record:

1. **Core claim refuted.** The SA's load-bearing bends are inter-cell seams,
   not idiom interiors; "shrink what the SA searches over" relocates the
   global search to the seams, where the net graph is *denser* and M5′
   already proved the acceptance cascade re-bases and re-basins. "Pre-verified
   to route clean" holds only at cell count = 1; inter-cell wires cross cell
   bboxes → V12/V13 breaches (the exact Tier-1 kills of the prior attempts).
2. **Idiom composition is disqualifying.** Analog idioms **share devices by
   construction** (unlike disjoint VLSI standard cells); a shared device gets
   one pose, so two conflicting canonical templates cannot both hold — and a
   recognizer *mis-assignment* applies a confident wrong template, the common
   case on exactly the multi-stage circuits cells exist to serve. Worked
   example: telescopic-cascode OTA, devices shared across diff-pair /
   cascode / mirror.
3. **Framing wrong:** R-A and R-B are independent engines; R-A is at frontier
   (cells inherit it coarser), R-B is untouched (routing-dependent decoration
   can't be reserved pre-route). The proposal's foundation stone S0 = the
   already-**blocked** ADR-19 M3.
4. **Better architecture the tracks under-weighted:** generalise the one
   trusted mechanism (phase 4.5, real router drives geometry) to bounded
   joint-pose repair — router-as-objective applied to *pose*, orthogonal to
   and far cheaper than idiom cells.
5. **Probabilities (red-team's honest priors):** full two-tier reaches
   human-level **~15%** (plateaus like ASG/Weave *after* large investment);
   Sugiyama-seed + kept SA/ratchets + generalised phase-4.5 joint-pose repair
   captures most of the human-perceived gain at **~65–70%**, ¼ the effort,
   incremental and reversible.

**Bottom line:** the simpler design is the right call. Build the
Sugiyama-layered seed (with feedback *marking*) and the generalised
phase-4.5 joint-pose repair; treat the invariant/ratchet harness as the
differentiator it is; run Spike 0 before entertaining idiom cells, and
expect it to come back red.

