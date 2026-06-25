# CLAUDE.md

Notes for AI assistants working in this repository. Read this before
making non-trivial changes.

## What this project is

`spice2kicad` converts SPICE netlists (ngspice / LTspice / PSpice
dialects, generic Berkeley SPICE3 base) into KiCad 6+ schematics
(`.kicad_sch`) and netlists (`.net`).

The hard part is **not** parsing SPICE. The hard part is producing
a *readable* schematic from a netlist that has no layout information.
Two questions every conversion must answer:

1. **Which KiCad library symbol** represents each SPICE element?
   (`Q1` could be `Device:Q_NPN_BCE` or `Transistor_BJT:2N3904`.)
2. **Where on the sheet** does each symbol go, so the result looks
   like a circuit diagram and not a hairball?

The user supplies hints to both via comment-embedded annotations —
see `docs/annotation-spec.md`. That spec is the source of truth for
what the parser accepts; this file describes the *thinking* behind
it.

## Project status: research / unstable

This is a **research project with no stability guarantees yet**.
Public APIs (crate boundaries, the annotation spec, sidecar formats,
diagnostic codes) all churn freely. There are no external users to
protect.

Practical consequences:

- **Don't write back-compat shims.** When a type or signature
  changes, just change all call sites in the same commit.
- **Don't write migration guides, deprecation notices, or
  `#[deprecated]` attributes.** Delete the old thing and update
  callers.
- **Don't preserve unused code "in case we need it later".**
  Delete it; git remembers.
- **Renumber / reshape diagnostic codes freely** if a better
  numbering emerges. Spec §7 is updated in lock-step.
- **Breaking changes to the annotation spec are fine right now**
  (§9 already calls out spec versioning as a v0.2 concern). The
  "additive vs breaking" rules in the "When changing the
  annotation spec" section below describe the *future* contract,
  not the present one — apply judgment.

When this project gets real users, this section gets removed and
the contracts harden. Until then: prefer the change that leaves
the codebase simpler over the one that preserves history.

### v0.2 deferred decisions (tracked)

Decisions deliberately frozen for v0.1, recorded here so they are
revisited rather than forgotten:

- **Revisit verbatim `lib_symbols` (V3).** V3 is final for v0.1: every
  `(lib_symbols …)` entry is a byte-for-byte passthrough, so the
  emitter never *synthesises* or *tweaks* a symbol. That rule blocks
  two future features — a clean fix for the deferred placer-glyph
  body-overlap item (the V14/[3] case, where a synthesised or rotated
  glyph variant would help) and zero-annotation auto-symbol-selection
  (which may want to derive a symbol the user doesn't have installed).
  Keep V3 as the current rule; re-open the symbol-synthesis question
  in v0.2 with these two use cases as the motivation.

## Repository layout

```
crates/
  spice-parser/      SPICE source → typed AST (chumsky-based)
  spice-resolve/     AST → resolved netlist; symbol / pinmap / ignore /
                     power / subckt decisions
  spice-layout/      placement: net-class → bands → layers → SA refine →
                     symmetry → hierarchical sheets
  spice-route/       Steiner routing, power glyphs, PWR_FLAG, conflict /
                     obstacle resolution
  kicad-emitter/     placed model → KiCad S-expressions; phase-4.5
                     routing-aware orientation refinement; decoration
  kicad-symbols/     .kicad_sym parsing, pin / body geometry
  spice-policy/      pre-flight align / place conflict check
  spice-diagnostics/ neutral source-spanned diagnostic types
  spice2kicad/       CLI binary (clap)
docs/
  annotation-spec.md   The annotation language. Authoritative.
  invariants.md        Visual-quality invariant definitions (V1–V15).
examples/
  rc_lowpass.cir
```

Rust 2024 edition, MSRV 1.85. `unsafe_code = forbid`. Pedantic
clippy is on, with a few common opt-outs in workspace `Cargo.toml`.

## Core design principles

These principles drove the annotation spec and should drive the
implementation. When in doubt, prefer the simpler option.

1. **The SPICE file is the source of truth.** Anything we ask the
   user to write must live inside SPICE comments and must not change
   simulation behaviour. A file that simulates today must still
   simulate after annotation.

2. **Annotations are optional everywhere.** A zero-annotation file
   must produce a valid (if ugly) schematic. Annotations only
   improve the output; they never gate it.

3. **No geometry numbers in user input.** No mils, no millimetres,
   no pixel coordinates, no `gap=200`. Users describe *intent*
   ("R1 sits below Q1"); the converter owns *geometry*. Numbers age
   badly across edits and across symbol-library changes.
   *(v0.2 note.* The only escapes for a layout the heuristics get
   wrong are `place` / `align`. Whether a rare explicit power-user
   nudge is warranted for cases neither covers is deferred — see
   annotation-spec §9.*)*

4. **Use SPICE's own structure for structure.** We deliberately have
   no `*@group` directive. Clustering is expressed via `.subckt`
   (hierarchical sheet) and — *as a v0.2/aspirational intent, not
   current behaviour* — `.include` (visual cluster). Re-inventing
   grouping inside comments duplicates what the language already
   provides. **Status:** `.subckt` → hierarchical-sheet lowering is
   implemented; `.include` is only *preserved* as an opaque directive
   by the parser (it is not expanded, and there is no visual-cluster
   placement pass — no `.include`-file inclusion in `spice-parser` /
   `spice-resolve`, and no cluster soft-attractor term in
   `spice-layout`). The `.include`-as-visual-cluster design is spec'd
   (annotation-spec §3) and ADR'd (ADR-10, "cluster boundaries as soft
   attractors") but unbuilt; treat it as a v0.2 target.

5. **Local first.** Most directives describe the line they sit on or
   the file they live in. Forward references and cross-file
   references are allowed but should be the exception.

6. **Line-oriented and grep-friendly.** No nested s-exprs, no YAML,
   no JSON. One directive per annotation line. Every annotation is
   visible to `grep`.

7. **KISS over completeness.** Cut anything that doesn't have a real
   use case. v0.1 has six directives (`symbol`, `pinmap`, `place`,
   `align`, `power`, `ignore`); features without justification go
   to §9 of the spec ("Open questions / deferred"). Add them when
   real users complain — not before.

8. **Hard errors on typos, soft warnings on conflicts.** An unknown
   refdes in a directive is `E001` (blocks conversion). Two `place`
   directives that disagree is `W101` (one wins, conversion
   continues). Silent typos defeat the purpose of the spec; silent
   conflicts merely produce a slightly worse layout.

9. **Structural placement, not pattern recognition.** The placer
   infers structure from net classification and signal-flow
   direction; it does not match named topologies. Adding a new
   circuit type should require zero placer code changes. The
   escape hatch when heuristics fail is `*@place` / `*@align` —
   already in v0.1.

## Annotation language at a glance

Two carriers, both invisible to SPICE simulators:

```
*@<directive> ...                             ← block, on its own line
R1 in out 1k  ;@ <directive>=<value>          ← trailing tag on element
```

Six directives:

| Directive | Form              | Purpose                                          |
| --------- | ----------------- | ------------------------------------------------ |
| `symbol`  | trailing or block | KiCad library symbol mapping (with `for=` glob)  |
| `pinmap`  | trailing          | Remap SPICE terminal order to KiCad pin order    |
| `place`   | trailing          | Position relative to another element             |
| `align`   | block             | Force horizontal/vertical co-alignment of N parts |
| `power`   | trailing          | Treat a voltage source as a power rail           |
| `ignore`  | trailing          | Hide simulation-only element from the schematic  |

Layout phases (later phases never override earlier):
1. Structural (`.subckt`, `.include`)
2. Aligned (`align`)
3. Placed (`place`)
4. Auto-fill (force-directed within parent cluster)
4.5. **Routing-aware orientation refinement** — a *placement-stage*
   pass (lives in `kicad-emitter`, the one crate that can see both the
   placer and the real router; `spice-layout` cannot depend on
   `spice-route` without forming a cycle). After phases 1–4 and BEFORE
   Decoration, it trial-routes candidate orientations of at-risk,
   non-pinned, non-symmetry elements with the **real router** and keeps
   the orientation minimising the router's *measured* first-segment-
   outward (V5) violations — subject to no V11 / V12 / symbol-overlap /
   V13 regression. It changes element *orientation* only, never
   position, and **never runs during or after decoration**. This is
   placement, not decoration: it owns orientation; decoration consumes
   it. See ADR-11.
5. **Decoration** — routing (wires), power/ground glyphs, labels,
   junctions. Reads final symbol positions; never moves them.

A default-path `.subckt` instance — one lowered to a KiCad
hierarchical `(sheet …)` block — participates in placement like any
other element: it is positioned by the structural pipeline
(classify→bands→layers, V6) adjacent to the elements it shares nets
with, **not** emitted at a hardcoded page coordinate. See V6's
"Hierarchical-sheet instances are placeable units" clause.

Decoration is a strict consumer of placement output: it may add wire
stubs, detached glyphs, junctions, and labels, but must never feed a
position or orientation change back into an already-placed symbol.
This contract is unchanged. The routing-aware orientation refinement
(phase 4.5) is **placement**, not decoration: it may change orientation
because it runs *before* decoration begins. Once decoration starts —
the final `route_nets` / glyph / label pass — no symbol moves or
rotates. The V14 glyph-direction work and the routing-aware refinement
both respect this: refinement reorients during placement; decoration
only reads the result.

For full grammar, examples, and diagnostics, see
`docs/annotation-spec.md`.

## Implementation notes

- **Parser.** Built on `chumsky` 0.10. The SPICE parser must
  preserve trailing `;@…` tags and `*@…` block comments as
  first-class AST nodes — they are *not* discarded as comments.
  Pure prose comments (lines starting with `*` but not `*@`) may be
  dropped.
- **Emitter.** KiCad `.kicad_sch` is S-expression based. The emitter
  takes a placed AST (positions resolved) and renders it.
- **`lib_symbols` are verbatim passthrough.** Symbol-library entries
  inside `(lib_symbols …)` are copied byte-for-byte from the source
  `.kicad_sym` (modulo `lib_id` name normalization) — no typed
  primitive model. At parse time `lexpr::Value` is mirrored into an
  internal `RawSexpr` and stashed on `Symbol::body`; the emitter
  re-serialises that body unchanged. This guarantees portability:
  emitted files render identically without the consumer having
  matching libraries installed at the same path. Final for v0.1
  (see invariant V3).
- **Layout.** Implemented in `spice-layout` (~6.5k LOC) — net
  classification, Y-bands, X-layers, SA refinement, symmetry, and
  hierarchical-sheet placement — with the routing-aware orientation
  refinement (phase 4.5) living in `kicad-emitter` (the one crate that
  can see both the placer and the real router). The constraint resolver
  from spec §5 sits between the parser and the emitter.
- **Diagnostics.** Use `ariadne` for source-spanned error rendering.
  Every error/warning code in spec §7 should round-trip through
  `ariadne` with the offending line highlighted.
- **Bare `\r` line endings.** The lexer strips `\r` only when it
  precedes `\n` (CRLF). Bare `\r` (legacy Mac line endings) is
  treated as part of the line. This matches ngspice
  (`inpcom.c:1864`) and means files using only `\r` would parse
  as a single physical line. Convert legacy files before feeding
  them in. See
  `crates/spice-parser/tests/lex_edges.rs::bare_cr_line_endings` and
  `crates/spice-parser/tests/lex_edges.rs::lone_cr_in_middle_of_line`.
- **Dangling `+` continuation at unusual positions.** A `+`
  continuation line with nothing to continue (e.g. as the first
  non-title line of a file, or immediately after a `*@` block
  annotation) reaches `handle_code_line` as a code line whose first
  token is `+`. The parser now flags it with `W912` (spec §7) and
  drops it — it no longer produces an `ElementKind::Other` element
  with refdes `"+"` that leaks into downstream passes. See
  `crates/spice-parser/tests/lex_edges.rs::continuation_at_start_of_file`
  and
  `crates/spice-parser/tests/lex_edges.rs::continuation_after_block_annotation_only`.
- **Numeric overflow is silent.** Values beyond `f64::MAX` parse
  to `Value::Number(f64::INFINITY)` (matching ngspice's
  `INPevaluate`). Downstream emitters should guard with
  `is_finite()` when serialising. See
  `crates/spice-parser/tests/numbers.rs::number_overflow_input`.
- **Tag span semantics.** Trailing-tag (`;@…`) spans cover the
  entire byte range from the leading `;@` marker through to the
  next `;` or end-of-line. When two `;@` tags share a line (e.g.
  `R1 a b 1k ;@ symbol=Device:R ;@ place=right-of V1`), the first
  tag's span ends just before the second `;`, including any
  trailing whitespace. Diagnostic renderers using these spans
  highlight the marker bytes; if a tighter "value-only" highlight
  is desired, slice the body manually. See
  `tests/spans.rs::tag_span_simple` and
  `tests/spans.rs::tag_span_multiple_on_one_line`.

## Layout invariants

Two invariants the placer must preserve, both invisible to the
annotation spec but load-bearing for implementation:

- **Constraints are pin-anchored.** `place` and `align` describe
  relationships between *connecting pins*, not symbol centers.
  The constraint resolver therefore consumes resolved symbol pin
  geometry (after `symbol` and `pinmap`), not just the AST.
- **Everything lands on the KiCad schematic grid** (50 mil =
  1.27 mm). Symbol origins, pin coordinates, and wire endpoints
  are integer multiples of the grid. The placer can use grid
  cells as its internal coordinate system; the emitter converts
  to mm.

See `docs/layout-roadmap.md` for the consequences on placer
architecture.

### Constraints vs. costs (how invariants are enforced)

The placer has two enforcement mechanisms, and *which one* an
invariant uses is load-bearing. The codebase has historically not
written this down, so contributors guess — and guess wrong (see the
V14 failure below). Be explicit.

- **Hard constraint** — applied as a *filter/projection on the
  candidate space*: eliminate the orientations that violate it,
  snap coords onto the grid, reject infeasible moves. The
  solver/SA can never emit output that violates it. Getting it
  wrong yields *infeasibility*, not a penalty. Lives at the
  candidate-generation boundary (`pick_orientations` in `lib.rs`,
  the grid snap and `propose_move` accept/reject in
  `solver/anneal.rs`), **not** in `cost.rs`.
- **Soft cost** — a weighted penalty term in the SA objective
  (`CostBreakdown` / `CostWeights::total` in `cost.rs`). The
  optimizer *trades it off* against the other soft terms. Correct
  for *preferences* and *tie-breakers*; wrong for any property
  that must categorically hold — at a safe weight a soft term can
  (and routinely does) change nothing.

**Decision rule.** A property is a **hard constraint** when it is
Tier 0 or Tier 1 (see the tiers subsection) AND *categorical* — a
yes/no geometric fact with one correct answer ("VCC pin faces up",
"origin on grid", "no orientation puts a power pin sideways"). It is
a **soft cost** when it is a *continuous quality gradient* with no
single correct value (total wire length, crossing count,
band-misalignment, soft-Y position). Continuous gradients are
inherently Tier-2 refinements.

**Consistency requirement (the rule).** A property enforced as a hard
constraint at the seeding/placement stage MUST be hard at *every*
stage that can move the element — both `pick_orientations` and the SA
rotate move — or the refiner silently undoes it. A hard constraint at
seed-time + weight-0 soft cost at refine-time is a bug. (Detailed
Attempt-A / Attempt-B post-mortem: see `docs/layout-adr.md`
post-mortems.)

**V14 is a hard constraint (Tier 1, categorical), not a cost.** The
orientation candidate set for any element bearing a power/ground pin is
*filtered* to those placing VCC-pins up / GND-pins down; both
`pick_orientations` and the SA rotate move are restricted to the
survivors. When the filtered set is *empty* (a forced sideways pin),
the escape is the **detached-glyph-with-stub-wire** path — the
documented fallback, NOT a soft penalty. There is deliberately **no
`power_pin_outward` weight in `cost.rs`**; adding one re-creates the
Attempt-A failure (a tunable term that at safe weights does nothing).

**Per-invariant mapping** (read off the code; re-derive nothing):

| Invariant                | Enforcement                          |
| ------------------------ | ------------------------------------ |
| grid alignment           | hard (snap at SA boundary)           |
| V11 wire/pin coincidence | hard (router conflict resolution)    |
| V14 power-glyph orient.  | hard + detached-glyph stub fallback  |
| V12 obstacle avoidance   | hard with budgeted-fallback (logs)   |
| V5 pin-facing            | soft seed + routing-aware refine*    |
| V6 bands/layers          | soft seed + soft cost terms          |
| V7 symmetry              | soft (mirror move, deferred)         |

*Notes on the table.* (a) **V5 is not an SA cost term** — `cost.rs`
has no `pin_facing`/orientation term. V5 is enforced in two non-SA
stages: a *seed-time heuristic* in `pick_orientations` (the SA
`rotate` move may override it), AND the **routing-aware
orientation-refinement phase** (Layout phase 4.5, ADR-11) — a
placement-stage pass in `kicad-emitter` that uses the *real router* as
an oracle to pick the orientation minimising the router's measured
first-segment-outward count, subject to no V11/V12/overlap/V13
regression. This is correct precisely because a V5 violation is born
in the router's conflict-resolution passes, invisible to any
placement-side cost. (b) **There is no `power_pin_outward` term** in
`CostWeights`. (c) V14 is a hard candidate filter
(`orient::allowed_orientations`) at both the seed chooser and the SA
rotate move; the refinement phase only selects from that same allowed
set, so it cannot break V14.

## Visual quality invariants

Project-level acceptance criteria for any emitted `.kicad_sch`.
These are not part of the user-facing annotation language; they
are falsifiable properties a checker can measure on the output.
Every invariant has an implemented verifier in
`crates/spice2kicad/tests/` — the suite enforces them.

The full per-invariant definitions and their verifiers now live in
`docs/invariants.md`. This file keeps the *policy* that governs how
the invariants trade off against each other — the tier ordering, the
ratchet-budget rules, and the constraints-vs-costs distinction — plus
a one-line summary table below.

### Invariant tiers (priority ordering)

V1–V15 are **not** a flat list of interchangeable budgets. Past
fixes failed because nothing forbade *loosening one fixture's
budget to tighten another's*, or regressing one aesthetic
invariant to satisfy a different one. Trade-offs need a defined
direction, so each invariant lives in exactly one tier and the
tiers are strictly ordered.

- **Tier 0 — Correctness (inviolable).** A violation means the
  emitted schematic is electrically WRONG or unopenable.
  Members: **V1** (an invisible symbol is a broken file), **V2**
  (zero ERC errors), **V3** (verbatim `lib_symbols` — portability
  correctness), **V8** (its correctness clauses: right symbol, no
  phantom sheet / no stray `<subckt>.kicad_sch`), **V11** (its own
  text: "a correctness invariant, not a quality one" — geometric
  coincidence must not silently short two nets). Tier 0 is a hard
  gate, never traded for any lower-tier gain.

- **Tier 1 — Readability constraints.** Strong legibility
  properties a human notices immediately as "wrong-looking", but
  not electrical correctness. Members: **V4** (label policy),
  **V9** (SI value formatting), **V10** (routing surface: power
  glyphs / Steiner trees), **V12** (no wires through foreign
  bodies), **V13** (labels don't overlap bodies / text / foreign
  wires), **V14** (power-glyph orientation), **V15** (content lands
  within the page's usable area). Note V12's own text calls it
  "quality" — it is tiered here as Tier 1 because a wire spearing a
  body is a legibility defect a reader flags on sight, not a
  pure-aesthetic refinement.

- **Tier 2 — Aesthetic refinement.** Pure layout heuristics that
  make the result look hand-drawn. Members (each self-described as
  a "quality" metric): **V5** (pin-facing orientation), **V6**
  (structural layered placement), **V7** (symmetry-aware
  placement).

**Ordering rule (load-bearing).** A change may never regress a
higher-priority (lower-numbered) tier to improve a
lower-priority one:

  1. Never trade a Tier-0 violation for any Tier-1/2 gain.
  2. Never introduce a Tier-1 regression to improve a Tier-2
     metric (e.g. don't route a wire through a body (V12) to make
     placement prettier (V6)).
  3. Within a tier, never loosen one fixture's budget to tighten
     another's. Budgets are per-fixture high-water marks that
     ratchet *down*, never sideways — see the direction-of-change
     / monotonic-ratchet policy for the budget mechanics.

**Global-improvement escape (bar is high; default answer is "no").**
The strict order can freeze a *local optimum*: a net-better change is
blocked because it regresses one other fixture's Tier-N budget. This
is exactly why the R-5 rail-pin and the [3] power-glyph-body fixes
could not land — each was an aesthetic win overall but tripped a
single fixture's Tier-1 ratchet. Analogous to the ratchet "one
exception": a change that **strictly reduces TOTAL violations summed
across all fixtures** MAY bump a single fixture's Tier-N budget IF it
carries a one-line rationale in the commit message AND the user signs
off. Absent both, the local-optimum freeze stands and the change is
not landed. This never licenses a Tier-0 regression — Tier-0 is a hard
gate, traded for nothing.

**Cautionary example (from a real failure).** An attempt to fix V14
glyph-direction on `common_emitter` by reworking the V5 orientation
scorer regressed `V13` and loosened budgets sideways — forbidden twice
over under the tier rule. Full narrative in `docs/layout-adr.md`
post-mortems.

### Budgets are ratchets, not knobs

Every per-fixture quality budget — crossing counts, wire-length
ratios, body-overlap counts, V5/V13/V14 violation counts — is a
*recorded high-water mark*, not a tunable headroom. The literal
records the actual current count on `master`; it only ever goes
**down**.

**The rule.** A commit MAY *decrease* a stored budget, and SHOULD
whenever a fix removes violations — update the literal in the same
commit. A commit may **never increase** a stored budget to make a
failing test pass. If a change raises a fixture's count, that is a
regression to *fix*, not a budget to *bump*.

**The one exception (bar is high; default answer is "no").** A
budget may rise only when the change *adds genuinely new geometry
that did not exist before* — a new fixture, or a feature that
legitimately introduces elements — AND the rise carries a one-line
rationale in the commit message AND the user signs off. Absent all
three, treat any required increase as a defect.

**Why direction-of-change beats absolute caps.** An absolute cap of
`≤ 5` cannot distinguish "improved 5→4" from "regressed 3→4" — both
pass under it. A ratchet stores the *actual current* value, so any
increase trips the test even while still under the old cap. The
ideal is **zero slack**: each budget literal equals the measured
count, so the test fails on ANY rise.

**Where the budgets live (apply this policy there).** They are
expressed three ways today; all three are subject to this rule:
- inline match arms returning the cap per fixture, e.g.
  `tests/electrical_safety.rs::body_overlap_budget`;
- `&[(&str, _)]` const tables, e.g. the crossing budgets in
  `tests/placement_quality.rs::crossing_count_within_budget_across_fixtures`
  (`("common_emitter", 4)`) and the wire-length-ratio budgets in
  `tests/placement_quality.rs::wire_length_within_budget_across_fixtures`
  (`("common_emitter", 2.5)`);
- bare `const` literals, e.g. `V5_RC_LOWPASS_OUT_MAX_MM` in
  `tests/placement_quality.rs`.

**Practical guidance.**
- When you *fix* something, run the verifier, read the new (lower)
  count, and lower the literal to match. Don't leave slack.
- When a test fails because a count *rose*, do NOT touch the
  budget — diagnose the geometry regression instead.
- Corollary of the tiers subsection's within-tier rule: you cannot
  pay for tightening fixture A by loosening fixture B. Ratchets
  move down, never sideways.

A future v0.2 tooling idea (do not implement now) makes this
self-enforcing: a shared `assert_ratchet!(fixture, metric,
current)` helper that prints "you may lower this to N" on pass and
"regression: rose to N" on fail, replacing the hand-maintained
literals above.

Summary (full definitions + verifiers: `docs/invariants.md`):

| Invariant | One-line                                                         | Tier |
| --------- | ---------------------------------------------------------------- | ---- |
| V1        | Symbols render visibly (no empty/stub `lib_id` glyphs)           | 0    |
| V2        | Zero ERC errors (`kicad-cli sch erc`)                            | 0    |
| V3        | `lib_symbols` inlined byte-verbatim (portability)               | 0    |
| V4        | Label policy: ≤ 1 plain label/net/sheet (2 only for name-jump)  | 1    |
| V5        | Pin-facing orientation (shared-net pins are the closest pair)   | 2    |
| V6        | Structural layered placement (classify → bands → layers → SA)   | 2    |
| V7        | Symmetry-aware placement (mirror pairs about a common axis)     | 2    |
| V8        | Standard symbol mapping for `.subckt` instances (`*@symbol`)    | 0    |
| V9        | SI-suffixed value formatting (`4.7k`, not `4700`)               | 1    |
| V10       | Power-as-glyphs, Steiner-tree routing, PWR_FLAG drivers         | 1    |
| V11       | Wire/label–pin coincidence is electrical (no silent shorts)     | 0    |
| V12       | Wires do not cross foreign symbol bodies                        | 1    |
| V13       | Labels/text don't overlap bodies, text, or foreign-net wires    | 1    |
| V14       | Power-glyph orientation: GND down, VCC up (rot 0)               | 1    |
| V15       | Content lands within the page's usable area (A4)                | 1    |

Full definitions + verifiers: `docs/invariants.md`.

## When changing the annotation spec

The spec is the user-facing contract. Treat changes as you would
changes to a public API:

- Additive changes (new directive, new optional key) are safe.
- Behavioural changes to existing directives are breaking.
- Removing a directive is breaking.

The spec deliberately does **not** carry a version field yet (see
spec §9). Add `*@spec version=…` and a version-handshake the day
v0.2 introduces a breaking change — not before.

When tempted to add a new directive, first check spec §9 to see if
it's already been considered and deferred. If it has, the spec
already records the reason it isn't in v0.1; respect that or update
§9 with new evidence.

## What not to do

- Don't introduce a YAML / TOML / JSON sidecar file. The whole
  point is that annotations live alongside the netlist.
- Don't add geometry numbers (mils, mm, coordinates) to the spec.
- Don't add a `*@group` directive. Use `.subckt` or `.include`.
- Don't add features speculatively. v0.1 deliberately omits things
  that would be nice to have (net cosmetics, multi-unit symbols,
  routing hints) — they are listed in spec §9 with reasons.
- Don't bypass `unsafe_code = forbid` or weaken the workspace lints
  without explicit discussion.

## Reference: KiCad source

The KiCad source tree is checked out at `../kicad-source/` (sibling
to this repo). Consult it when you need ground truth on `.kicad_sch`
S-expression schema, symbol library file format, or how the official
tools render specific constructs. Prefer reading the KiCad source
over guessing format details.

## Useful commands

```sh
just check         # fmt + clippy + test
just test
just hooks         # install git pre-commit hooks
cargo install --path crates/spice2kicad
```

## Committing during multi-agent / parallel work

**Commit each green milestone before launching another agent (or
workflow) that may touch git.** Subagents and workflow steps run shell
commands freely, including `git stash` / `git checkout` / `git reset`
to "clean up" or inspect prior work. When valuable changes are sitting
*uncommitted* in the working tree, a later agent's git operation can
revert or park them, leaving an inconsistent tree (e.g. test files
referencing modules that got stashed away). Uncommitted work is the
only thing at risk — once a milestone is committed (on a branch; see
the default-branch rule), git ops can stash and reset around it
without losing it, and recovery is a merge rather than a manual
reconstruction. So: land step N (tests green, committed) *before*
dispatching the agent for step N+1.

A regression in the router (or placer) can produce unbounded segment
growth and OOM-kill the host before any single test fails. To keep
that contained, **always run tests and one-off conversions under a
virtual-memory cap** — never `cargo test --workspace` bare.

`just test` already does the right thing: `ulimit -v
${RUST_TEST_MAX_VSZ_KB:-4194304}` and `--test-threads
${RUST_TEST_THREADS:-2}`. A runaway test process then hits its own
4 GiB cap and fails (with a Rust allocation error or SIGABRT) instead
of taking the whole machine down.

When invoking `cargo` directly, wrap the same way:

```sh
bash -c 'ulimit -v 4194304 && cargo test -p <crate> -- --test-threads=2'
bash -c 'ulimit -v 4194304 && cargo run -q -p spice2kicad -- …'
```

Tighten the cap (e.g. `RUST_TEST_MAX_VSZ_KB=1048576`, 1 GiB) when
fuzzing the router or running large fixtures: a quicker abort gives
faster feedback than a slow death-march. Loosen only when you have
positively diagnosed a test that legitimately needs more (large
roundtrips against full KiCad libraries occasionally do).

If a test does hit the cap, that is a defect — diagnose root cause
(a counted iteration limit, a stale segment-set growth invariant, an
unbounded recursion) instead of just raising the ceiling.
