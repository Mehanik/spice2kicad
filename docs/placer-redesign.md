# Why the placer needs a redesign

> Status: **motivation & constraints document**, not a design. It states
> *why* a redesign is warranted, *what the current placer does*, *what it
> cannot do and why*, and *what any redesign must preserve or must not
> repeat* — distilled from the development history (ADR-11/14/15/16/17/18,
> the `flow-orientation-wall` / `v14-placer-pin-choice-deferred` /
> `placer-redesign-retired` memory notes, and the multi-session review
> record). The actual redesign is out of scope here; ADR-17 was one
> attempt and was **retired** (see §6), so this deliberately stops short
> of prescribing an architecture.

Every code claim below is anchored to a `file:function`/`file:line`
citation, because this document exists precisely because prior redesign
reasoning was built on doc comments that the code had drifted away from
(§7 lists the live drifts). Verify against the code before trusting any
sentence here.

---

## 1. The one-paragraph case

The current placer derives every element's coordinates from **global
structure** (net classification → Y-bands → X-layers → geometry-derived
strides), then hands the result to a simulated-annealing refiner and a
real-router orientation pass. This architecture has one deep property
that governs everything else: **it is globally non-local** — changing one
element re-bases the whole page. That single property is upstream of the
three walls the project keeps hitting (flow orientation, decoration
reservation, and the recurring "overfitting" defect class), and it is why
each of those was only ever patched with a bespoke, per-case hack rather
than a general fix. A redesign is warranted not to make the fixtures
prettier — most are good now — but because **the architecture has
reached the point where each new capability costs more than the last**,
and the remaining owner-visible defects are provably unreachable without
changing that property.

---

## 2. What the current placer is (ground truth)

Entry point `spice_layout::place_with_hint` (`crates/spice-layout/src/lib.rs:543`).
The CLI runs it, then phase 4.5, then sheet placement, then emitter
decoration (`crates/spice2kicad/src/main.rs:292-330`). The pipeline, in
execution order:

| Stage | File:fn | Decides | Frame |
|---|---|---|---|
| Net classification | `net_class.rs::classify_nets:105` | `NetClass{Power,Ground,Signal}` + finer `vertical_prefs` (neg-rail → `Down`) | — |
| Y-bands | `bands.rs::assign_y_bands:46` | `Band{Top,Mid,Bot}` + `soft_y_target_frac` | — |
| X-layers | `layers.rs::assign_x_layers:43` | `u32` layer via signal-DAG longest-path; **`no_source_fallback:98` when no V/I source** | — |
| Channel rows | `channels.rs::assign_rows:71` | union-find over non-rail nets → rows | — |
| Seed | `lib.rs::place_seed:1445` | `(x,y)` from geometry-derived strides, all `Orientation::IDENTITY` | grid |
| User constraints | `lib.rs::apply_user_constraints:1639` | `align`/`place` override + **pin** (sets `user_pinned`) | grid |
| V7 symmetry | `symmetry.rs` (`detect_pairs`/`apply`) | mirror pairs about a shared axis + pin | grid |
| Idioms | `idioms.rs` (dividers, shared-node, rail-stub column, series-horizontal) | position/orientation refinements + pin | grid |
| Orientation seed | `orient::allowed_orientations` (V14 filter) + `pick_orientations` (V5) | per-element orientation | grid |
| SA | `solver/anneal.rs::refine:57` + `cost.rs` | jitter/swap/rotate movable elements against a 12-term objective + 5 hard gates | grid |
| Legalize | `legalize.rs::legalize:291` (post-SA, only if overlap) | deterministic shove of overlapping non-pinned elements | grid |
| Phase 4.5 | `kicad-emitter/src/refine.rs::refine_orientations:83` | orientation only, real-router oracle | mm/world |
| Decoration | `kicad-emitter` (`route_nets`, glyphs, labels, `translate_into_page`) | wires, power glyphs, labels, page frame | mm/world |

Key load-bearing facts a redesign must know:

- **The grid is the internal frame.** `GridPoint{i32,i32}`, 1 cell = 1.27 mm
  (`lib.rs:57,65`). mm appear only where geometry is needed (extents, cost).
  Everything lands on the 50-mil grid by construction.
- **The eeschema Y-flip (`world_y = origin_y − local_y`) is applied at each
  consumer, not stored** (`lib.rs:408`, `anneal.rs:384`, `refine.rs:719`).
  This has been a repeat source of sign bugs (§7).
- **The SA is fully deterministic** — xoshiro256\*\* seeded from a fixed
  `0xC0FFEE42` (`solver/mod.rs:51`), all ordering via `BTreeMap`/`BTreeSet`.
  Two cache-less conversions are byte-identical, enforced by
  `placement_stability.rs::conversion_is_byte_deterministic_across_fixtures`.
  (ADR-4 and `sidecar.rs:7` still say "system entropy" — stale; §7.)
- **Phase 4.5 lives in `kicad-emitter`, not `spice-layout`,** because it
  needs the real router (`trial_route`) and `spice-layout` cannot depend on
  `spice-route` without closing a dependency cycle (`refine.rs:9-12`,
  ADR-11). Any redesign that wants routing feedback earlier inherits this
  crate-graph constraint.

### The two enforcement mechanisms (this distinction is the doctrine)

`CLAUDE.md`'s constraints-vs-costs rule is real and load-bearing:

- **Hard candidate-space filter** — eliminates violating options at
  generation. The SA can never emit output that violates it. Members:
  `pinned` (`movable = !pinned`, `anneal.rs:72`), the V14 `allowed` set
  (`orient.rs`), and the SA's five never-increase gates (V11 coincidence,
  symbol overlap, V5 mirror, flow-inversion, V14 — `anneal.rs:216-265`).
- **Soft cost** — a weighted penalty the SA trades off. Correct only for
  continuous preferences; at a safe weight a soft term routinely changes
  nothing.

**Pinning is the only mechanism that is trivially hard at *every* stage**
(verified: no code path reads a pinned element's coord as mutable). Every
other hard constraint is re-implemented separately per stage and is only
hard where re-implemented — which is exactly how ADR-14's glyph
reservation ended up "hard for oversized pairs in the SA gate, X-only at
the seed" (`anneal.rs:420-438`). A redesign that leans on pinning inherits
a mechanism that already works; one that invents new per-stage hard
constraints inherits the re-derivation tax.

### What the SA actually buys (measured, not assumed)

The ADR-17 ablation (`docs/layout-adr.md:2827-2839`) is the empirical
ground truth and every redesign proposal should start from it:

- **Inert on 4 of 10 fixtures** — seed and SA output are byte-identical
  (`multivibrator`, `diff_pair`, `port_shapes`, `opamp_definition_level`).
- **Harmful on several** — `rc_lowpass` (B 2→3, WL 11.4→17.8),
  `opamp_inverting_real` (B 6→8): the polish anti-polishes.
- **Load-bearing on 3 complex fixtures** — `common_emitter` (B 11→4),
  `named_rails` (4→2), `opamp_inverting` (5→3): here it finds a
  materially better *basin*.

ADR-17's correction is the important sentence: **"the SA is not doing
compaction; it is basin-finding."** It lowers bend *topology*, not wire
length. That is why the deterministic-compaction replacement
(`compact.rs`) was built and **killed** (§6) — compaction cannot
reproduce basin-finding, and it reclaims exactly the space decoration
needs.

---

## 3. The two root-cause engines

Nearly every visible defect is a symptom of one of two structural
properties. A redesign that fixes the symptoms without these is another
round of hacks; a redesign that fixes these makes the symptoms tractable.

### R-A — Global non-locality of spacing-derived placement (the deepest issue)

Because coordinates are derived from global structure (band/layer strides
in `lib.rs`/`bands.rs`/`layers.rs`), **changing one element re-bases the
whole page.**

- Measured: adding one bypass cap to `common_emitter` moves **17 of 17**
  pre-existing symbols; adding one series R to `rc_lowpass` moves **5 of 5**
  (`placement_stability.rs`, the P11 basin-locality probe).
- ADR-17's central hypothesis — that the SA/RNG caused this — was
  **falsified by its own control**: the bare deterministic seed
  (`--no-refine`) scores the *same* 17/17 and 5/5, and a deterministic
  order-preserving compaction sweep still scored 16/17.
  **"Determinism is not locality."**

Two consequences make this the engine, not just a quirk:

1. **A hard constraint cannot be added incrementally without a global,
   unattributable diff.** Every constraint landed to date needed a bespoke
   blast-radius container: RNG-stream preservation (`anneal.rs:196,847`),
   the `gates_active` switch (`:101`), `mirror_eligible` scoping (`:88`),
   ADR-14's deliberately-incomplete reservation, and legalization moved
   from before→after the SA (`lib.rs:659-685`). That list only grows.
2. **The zero-slack ratchets + a globally-coupled placer are jointly a
   change-prevention machine.** A net-positive change routinely trips one
   fixture's Tier-N budget because it moved everything, and the ratchets
   forbid sideways trades. This is the "local-optimum freeze" `CLAUDE.md`
   documents. It is why good changes (the R-5 rail-pin fix, the [3]
   power-glyph fix, and repeatedly this session) could only land with an
   explicit global-improvement escape.

**A redesign MUST make locality an explicit, P11-tested design property:**
adding or moving one element should perturb only its neighbourhood, and
`placement_stability.rs` should ratchet that bound down, not merely assert
byte-determinism.

### R-B — The symmetric-halo / incomplete-reservation trap

The SA's overlap footprint (`anneal.rs::footprint_half_extents:439`)
reserves decoration space with a **symmetric `.abs()` halo** about the
origin: a genuinely one-sided reach (a GND glyph *below* a pin) is folded
via `hw.max(dx.abs())` into a block on **both** sides. ADR-14's post-mortem
(`docs/layout-adr.md` "symmetric halo") proved this halo is **load-bearing
accidental spacing** — the ratchets are calibrated to it. Two hard
consequences:

1. **Reservations cannot be made honest.** The directional AABB is
   *provably a subset* of the symmetric box (`hw = max|coord|` ⇒
   `[-hw,hw] ⊇ [min,max]`), so making the footprint directional *strictly
   relaxes* the gate and the SA spends the freed space on tighter, worse
   layouts. Measured: `rc_lowpass` V13 0→1, `common_emitter` SVG-ink text
   overlap 0→1, B 4→7. The halo is not a bug to fix in place.
2. **Completion is blocked, and partial completion is a no-op.** Label
   text, `PWR_FLAG` bodies, and oversized-host self-zones remain
   unreserved (ADR-14 "known scope limits"). A *correct* partial
   reservation is measurably a no-op because *space one reservation
   reclaims is space the still-unreserved classes move into* — proven four
   ways in the ADR-14 ablation, and again by ADR-17 Stage-2's kill (4 of 7
   Tier-1 breaches were label overlaps in space compaction "believed was
   free").

**A redesign MUST treat the decoration footprint as a first-class, signed,
complete quantity from the start** — not an accidental halo the objective
is silently tuned against. This is a precondition for any compaction or
tighter-placement work, not a follow-up.

---

## 4. The walls (owner-visible, each traced to an engine)

### Wall 1 — Series-into-a-node elements drawn vertical (flow orientation)

Symptom (owner-flagged, multiple reviews): `common_emitter` **COUT**
(`rot 0`, vertical) should be horizontal like `CIN` (`rot 90`) — same role,
two orientations; `opamp_inverting_real` **RIN** (vertical) should feed the
summing node horizontally.

**Precise boundary** (this is the hard-won part — the wall is *not* "all
horizontal orientation is impossible"): `idioms::apply_series_horizontal`
(`idioms.rs:1010`, guard `:1057-1074`) succeeds for a series element whose
downstream node carries a **one-sided ground shunt** to re-column beneath
the output — this is why `rc_lowpass`/`rc_lowpass_ports` now draw R1
horizontal and read left→right. It is **blocked** when the downstream is a
bare interior node or a port with **no shunt** to anchor the reorientation
(COUT, RIN): forcing it re-basins the SA (`common_emitter` COUT forced →
B 4→7) — a direct instance of **R-A**.

Two independent attempts, both measured and abandoned:
- Seed/SA orientation tie-break → regressed **Tier-0 V11** (8 foreign-pin
  shorts on `common_emitter`).
- Hard `allowed`-set filter in `orient.rs` (ADR-15 Stage-5) → **beat
  Tier-0 V11 and survived phase 4.5**, but still tripped **Tier-1**
  (`rc_lowpass` V12 0→2, `rc_lowpass_ports` V13 0→1, `common_emitter`
  B 4→11).

The lesson, recorded in the `flow-orientation-wall` memory: *"making the
orientation choice hard does not make it good — it makes it permanent."*
On these fixtures the flow proxy **genuinely disagrees** with the router's
measured V5, and hard-constraining a bad choice surfaces as router damage
downstream. **A redesign MUST reconcile flow and routing holistically** —
decide pose (position + orientation + mirror) jointly from the flow
structure, so the geometry the flow proxy wants is the geometry that
routes cleanly. The shunt case works precisely because position and
orientation are chosen together there; the node case needs the same
treatment generalized (the redesign must be able to *rotate the pin* to
remove the same-facing bend at its source, not force an orientation onto a
fixed position).

### Wall 2 — Feedback / bridging elements cannot span

Symptom: `opamp_inverting_real` **RF** sits *between* RIN and X1 rather
than spanning above the amplifier; `opamp_definition_level` RF1/RF2
crammed beside each opamp.

Root cause: the X-layer axis (`layers.rs`) is a longest-path DAG depth. A
bridging element's two nets sit at **different depths**, and a scalar
layer index **cannot express a span**. `break_cycles` (`layers.rs:383`)
identifies feedback edges but *reverses* them silently rather than marking
them exempt; and on every current fixture the flow graph takes
`no_source_fallback` (all fixtures `;@ ignore` their source), so the DAG
layering that would feed `break_cycles` is bypassed anyway. Feedback
spanning is simply unbuilt, and cannot be expressed in the current
1-D-layer model. A redesign's structural model must be able to place an
element **across** a span, not at a point on an axis.

### Wall 3 — The overfitting pattern (the architecture's characteristic failure mode)

This is a *meta-defect*, and the strongest single argument for a redesign.
The current architecture infers circuit structure through **heuristic
guards keyed on properties that merely happen to hold for the ~10
reference fixtures**. This session alone found **seven** guards that each
silently mis-handled a whole class of circuit until a new fixture exposed
it:

1. Symmetry pinning killed the rail-stub idiom on *every symmetric*
   circuit (idioms skip pinned members).
2. `no_source_fallback` rooted every *supply-connected active* at layer 0
   (its test is "touches a rail", not "is a bias element").
3. Vertical-anchor-only rail-stub column excluded *base-fed* resistors.
4. Equality-vs-prefix net-name matching (`in`/`out` by equality,
   `vin`/`vout` by prefix) meant *numbered multi-channel ports*
   (`in1`,`in2`) matched nothing and every multi-channel circuit layered
   backwards.
5. `symmetry::detect_pairs` had no coupling predicate → *uncoupled*
   repeated channels mirrored onto the same x-span.
6. A fixture (`opamp_definition_level`) sat *outside the quality suite*, so
   overlapping-opamp-triangle states passed the whole suite.
7. A detour metric never ran (label-derived baseline hit an early-continue
   on 9/10 fixtures), so HPWL's "already covered" job was ungraded.

**Why the architecture keeps producing this:** because **R-A** makes every
change global and the ratchets forbid sideways trades, each new circuit
class is accommodated by *adding another narrowly-scoped guard* rather than
generalizing — and a guard tuned to pass the visible fixtures silently
mis-handles the invisible ones. The test suite cannot catch it because new
classes arrive with no verifier coverage. **A redesign must replace
guard-accretion with a structural model** in which circuit roles fall out
of topology (pin counts, net classes, connectivity) rather than
name-matching and per-fixture thresholds — `CLAUDE.md` principle 9
("structural placement, not pattern recognition") stated as an
architecture, not an aspiration.

### Wall 4 — Multi-channel / structural rows are narrow

`opamp_definition_level` required a whole new module (`channels.rs` +
ADR-18's four-cause fix + a geometry-derived seed stride) to lay two
independent channels as rows, and even so `bands.rs::assign_y_bands` still
returns only Top/Mid/Bot — rows exist *only* because `channels.rs`
union-finds non-rail components and pins them (and note: `row_adjusted_frac`,
the intended cost-side support, is **dead code** — §7). Validated on N=2
only. Still unhandled: 3+ channels, *mixed* coupled/uncoupled banks,
nested subcircuits. This is R-A plus the absence of real intra-Mid Y
structure.

### Not a wall (recorded so it is not re-litigated)

**Power-glyph / foreign-body overlap is CLOSED.** The long-deferred "V14
placer pin-choice" residual (`opamp_inverting_real` GND glyph clipping RF)
was resolved 2026-07-20 — it was a *layering* defect (`no_source_fallback`
rooting supply-connected X1 at layer 0), never a glyph problem; four
glyph-side fixes had failed because the defect wasn't glyph-side. Budget
now unconditional 0. The transferable lesson (memory
`v14-placer-pin-choice-deferred`): *a defect attributed to the mechanism
it appears in may live somewhere else entirely.*

---

## 5. The escape hatch cannot reach the blocked cases

A tempting objection is "the walls don't need a redesign — the user can fix
them by hand." **They cannot.** `place`/`align` are **position-only** and
**force `Orientation::IDENTITY`** (`lib.rs:1052-1055`: re-rotating would
invalidate the pin-anchored math in `solve_place`; re-enforced by the
`pinned` short-circuit in the SA and phase 4.5). For `Device:R_US`/
`Device:C`, identity **is** the unwanted vertical — so reaching for
`place`/`align` to fix COUT/RIN locks in the very orientation the user is
trying to change. There is **no orientation directive**: `;@ orientation=…`
is deferred (annotation-spec §9) *and* the deferred design as written
would put geometry numbers in user input, violating `CLAUDE.md`
principle 3, so it needs rework before it could even land — and would
still hit Wall 1.

This corrects ADR-17's own honesty-check, which claimed both flow defects
"could be fixed today with `*@place`/`*@align`." They cannot: those
directives can *move* COUT but cannot *rotate* it horizontal. A redesign
should make blocked orientations either automatic (preferred) or reachable
through an intent-level directive that stays inside principle 3.

---

## 6. What was already tried — and must not be repeated

The redesign space is heavily constrained by expensive negative results.
Do not re-run these:

- **ADR-17 (deterministic constructive placement) — RETIRED.** Its central
  promise was *attributability* (remove the SA, get locality). Falsified:
  the bare seed re-bases as badly as the SA (R-A is intrinsic to
  spacing-derived placement, not to the optimizer). Retired at Stage 2
  after the deterministic-compaction core was **killed** (7 ratchet
  breaches vs a budget of 2; 4 were Tier-1 label overlaps — the R-B trap).
  Lesson: **removing the RNG does not buy locality**, and **compaction
  cannot replace basin-finding**. A redesign that is "the SA but
  deterministic" will reproduce this.
- **Flow-orientation tie-break and hard `allowed`-filter — both abandoned**
  (Wall 1). A redesign that changes orientation *against fixed positions*
  will reproduce the Tier-1 damage. Position and orientation must be
  decided together.
- **Directional/honest footprint — measured worse** (R-B). A redesign that
  "cleans up" the symmetric halo without simultaneously completing the
  reservation and re-calibrating the ratchets will regress.
- **Glyph-side fixes for the V14 residual — four failed** before the real
  (layering) cause was found. Do not attribute a defect to the stage it is
  visible in.

### What was proven RIGHT and must be preserved

- **Pinning** as the one trivially-hard-at-every-stage mechanism (§2).
- **The real-router oracle in phase 4.5** (`refine.rs`): the grader and the
  oracle share `count_outward_violations`, so they cannot drift. Any
  placement-time proxy for routing quality has been shown to disagree with
  the real router exactly where it matters (Wall 1).
- **The lexicographic acceptance tuple `(v13, v12, v5, bends)` with
  `severed`/`v11`/`overlap`/`v12` as separate `<=` guards** (`refine.rs:149`)
  — Tier-0 connectivity is a floor no lower-tier gain can buy back.
- **The tier ordering and zero-slack ratchets** as governance — the
  freeze they cause is a symptom of R-A, not a flaw in the governance;
  fixing R-A makes the ratchets cheap to live under rather than a
  change-prevention machine.
- **The role model** (anchor / series / rail-stub / terminal, derived from
  pin counts and net classes) — ADR-15's Stage-5 validated the
  discriminator perfectly (COUT horizontal, bypass CE vertical, from pin
  roles alone); only the *enforcement machinery* failed. The roles survive
  a redesign; the guard-accretion around them should not.

---

## 7. Live doc-vs-code drifts (fix opportunistically; do not trust the stale side)

These were found while writing this document. They matter because redesign
reasoning built on the stale side will inherit the error.

1. **`channels.rs::row_adjusted_frac:160` is dead code** — no non-test
   caller; `cost.rs::soft_y_residual:1133` reads `soft_y_target_frac` raw,
   so the SA's Y cost still sees 0.50 for every channel element. Channel
   separation comes only from `pack_rows` + pinning, never the cost. The
   module doc oversells it as wired.
2. **Determinism** — ADR-4 (`layout-adr.md:115`) and `sidecar.rs:7` say the
   SA is "system entropy" seeded; it is deterministic (fixed
   `0xC0FFEE42`). ADR-17's retirement already notes this; the ADR-4 text
   and sidecar comment were never updated.
3. **`layer_order` (`cost.rs:1193`) returns 0 on `no_source_fallback`** —
   i.e. on *every* signal-source-less fixture (all BJT/opamp fixtures).
   The soft left→right flow term is effectively off there; flow is carried
   by the seed layering + the `flow_inversions` hard gate (`anneal.rs:729`).
4. **`signal_flow` (`cost.rs:917`) is top-level-dead** — nonzero only
   inside `.subckt` bodies.
5. **FR seeding is vestigial** — `fr_iters=0` (`solver/mod.rs:57`), so
   `force::seed` is a no-op; the SA always starts from the structural seed.
   The `solver/mod.rs` module doc still presents FR→SA as active.
6. **ADR-14 glyph reservation is not "hard at every stage"** — hard only
   for oversized-involving pairs in the SA gate, X-only at the seed
   (`anneal.rs:420-438`, self-documented). Only pinning and V14 truly meet
   the `CLAUDE.md` "consistency requirement".
7. **Duplicated comment paragraph** in `apply_rail_stub_columns`
   (`lib.rs:905-923`) — cosmetic.

---

## 8. Acceptance criteria for a redesign (what "done" looks like)

A redesign is worth doing only if it can claim all of:

1. **Locality is a tested property.** `placement_stability.rs` bounds how
   many pre-existing elements move when one is added/changed, and that
   bound ratchets down. *(Now tested — ADR-19 Milestone 1:
   `cache_less_placement_perturbation_within_bound`. The stale "17/17"
   counts the V15 uniform page pan and glyph renumbering as movement; the
   honest, page-pan-normalized user-symbol bound is `rc_lowpass` **0**,
   `common_emitter` **8**, ratcheting down.)* This is the R-A fix and the
   thing that makes everything else incremental.
2. **The decoration footprint is signed and complete** — body, pins,
   glyphs (one-sided), value text, labels, PWR_FLAG bodies — with the
   ratchets re-calibrated to the honest quantity rather than the accidental
   halo. (This is the R-B fix and a precondition, not a follow-up.)
3. **Pose is decided jointly** (position + orientation + mirror) from flow
   structure, so COUT/RIN and the like are horizontal-and-clean, verified
   against the *real router*, not a proxy — with no Tier-0/Tier-1
   regression.
4. **Structure comes from topology, not name-matched guards** — the
   overfitting pattern (§4 Wall 3) dissolves: a new circuit class is
   handled by the same rules, and arrives with verifier coverage.
5. **The preserved invariants hold** (§6): pinning, the real-router oracle,
   the lexicographic acceptance tuple, tier governance, and the role model.

If a proposed redesign cannot credibly claim (1) and (2), it is another
round of hacks and should be declined — that is the lesson ADR-17 paid for.

---

*Provenance: this document was assembled by the operating assistant from
the development history and two ground-truth code investigations; it
records assistant analysis, not owner-ratified doctrine. The measured
facts (ablation table, P11 numbers, ratchet deltas) are cited to their
sources; verify against the code before acting, per the drifts in §7.*
