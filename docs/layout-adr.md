# Layout — Architectural Decision Record

Decisions made before implementation of the `spice-layout` crate.
Companion to `layout-roadmap.md` (the *what*) — this is the *how*
and the *why this and not that*.

Each decision lists the choice, the reasoning, and the
implications that downstream code must respect. Decisions are
numbered; later docs / commits can cite them as `ADR-3`, etc.

---

## ADR-1 — KiCad symbol library access

**Decision.** Parse the user's KiCad symbol libraries at runtime
(via `KICAD_SYMBOL_DIR` and the standard search paths). No
build-time bundling. No hand-written table.

**Why.** Most flexible, no vendoring drift, works with whatever
symbols the user actually has installed. The complexity is real
(parser for `.kicad_sym`, error handling for missing libs,
caching across runs) but it's a one-time investment.

**Implications.**

- A new module (or sibling crate) is needed to parse
  `.kicad_sym` files and extract pin geometry. Lives close to
  `kicad-emitter` since both speak KiCad s-expressions; consider
  factoring out `kicad-symbols` as a shared dep.
- Missing or unparseable libraries are CLI-time errors, not
  build errors. Diagnostics: a new `E0xx` code.
- Tests need a fixture library checked into `tests/fixtures/`
  to avoid depending on the developer's local KiCad install.
- Cache parsed libraries per CLI run; do not re-parse per
  symbol lookup.

---

## ADR-2 — Resolved-AST boundary

**Decision.** `spice-layout` consumes a `ResolvedNetlist`, not
the raw parser AST. Symbol lookup, `pinmap` application, and
pin-geometry attachment happen in a *resolution* pass that sits
between `spice-parser` and `spice-layout`.

**Why.** Makes `spice-layout` a pure function
`ResolvedNetlist → Placement`. Easier to test, easier to swap
algorithms, prevents symbol-lookup logic from bleeding across
crate boundaries.

**Implications.**

- A new resolution pass — likely in `spice-layout` itself or in
  a tiny `spice-resolve` crate — owns this transformation.
- `ResolvedNetlist` is a public type. Each resolved element
  carries: SPICE refdes, library-symbol id, pin geometry
  (positions relative to symbol origin in grid units), and the
  SPICE-terminal-to-KiCad-pin mapping.
- The placer never imports `spice-parser` types directly; the
  resolved type is the only dependency.

---

## ADR-3 — Orientation and mirroring in the search space

**Decision.** Per-element orientation (0/90/180/270) and mirror
state (mirror-x / mirror-y) are **part of the SA search**, not a
fixed pre-pass heuristic.

**Why.** Tight layouts and analog idioms (diff pairs needing
mirror symmetry, transistors flipped to put collector on the
power rail) are unrouteable without orientation freedom. A fixed
heuristic is a local optimum.

**Implications.**

- Each part has 8 possible orientation states (4 rotations × 2
  mirrors). The SA move set must include orientation/mirror
  flips alongside position moves.
- Cost in state-space size: 8× per part. Mitigations:
  (a) seed with a sensible orientation per element kind so SA
  rarely needs to flip; (b) make orientation moves rarer than
  position moves in the proposal distribution.
- Pin geometry must be queryable in any of the 8 orientations.
  The `kicad-symbols` module exposes a transform helper.
- Constraint lowering must be orientation-aware: `place=right-of`
  asks "which pin is *currently* the leftmost connecting pin?"
  — a function of the candidate orientation.

---

## ADR-4 — Non-determinism with sidecar position file

**Status: wired.** Implemented as `<basename>.layout.json` next to the
emitted `.kicad_sch`. The schema and reader/writer live in
`crates/spice-layout/src/sidecar.rs` (`Sidecar`, `SidecarEntry`,
`sidecar_path_for`); the placer accepts the cache as a
`spice_layout::Hint` via `place_with_hint`, reusing the same
per-element `pinned` mask that `align` / `place` use (no parallel
path). The CLI (`crates/spice2kicad/src/main.rs`) reads the sidecar
before placement and rewrites it after, on every run; `--no-layout-cache`
opts out. The acceptance test is
`crates/spice2kicad/tests/layout_cache.rs` (add-one-element stability,
round-trip, removal-drops-from-cache, opt-out). The design below was
the intended plan and matches what shipped.

This sidecar is a **position-cache artifact** for re-layout
stability — *not* a configuration or annotation carrier. It does
not describe user intent; the converter owns its contents and
rewrites it on every run. It is therefore distinct from the
YAML/TOML/JSON config sidecar that CLAUDE.md forbids: that rule
bans encoding *annotations* outside the SPICE file, whereas this
is derived geometry the tool caches for itself.

**Decision.** SA is non-deterministic (RNG seeded from system
entropy). To support incremental updates and preserve user edits,
the converter writes a **sidecar artifact** alongside the
`.kicad_sch`: `<basename>.layout.json`, containing a stable mapping
from SPICE-refdes → `(grid x, grid y, rotation, mirror)`. (JSON via
`serde` was chosen for the format; see "What we are not deciding
now".)

On re-conversion:

1. If the sidecar exists, load it as a *seed*. Existing
   refdeses get pinned to their saved positions. SA only places
   new refdeses and resolves overlaps.
2. Removed refdeses are dropped from the sidecar.
3. The sidecar is rewritten on every run.

**Why.** Position stability under netlist edits is a hard
usability requirement — users will hand-tune the schematic in
KiCad, then re-import an updated netlist, and expect untouched
parts to stay put. A sidecar is simpler than reverse-parsing
positions out of the user's edited `.kicad_sch` (which would
require diffing against our last emission and is fragile).

**Implications.**

- Sidecar schema is a public artifact — versioned, documented,
  diffable in git. Probably JSON or TOML for human-readability.
  Decide format in implementation; not load-bearing now.
- The placer's contract becomes: "given a possibly-empty
  `Hint` of pinned positions, produce a placement that respects
  the hints unless they conflict with hard constraints".
- Hand-edited positions in the sidecar are user-overridable
  pins — same constraint mechanism the resolver already needs
  for `align`/`place`. Reuse the pipeline.
- Long-term: the sidecar can also store user-overridden
  orientations, decisions about which pattern detector fired,
  etc. Don't design that now; just leave room.
- The architecture had to accommodate this from the start because
  retrofitting position stability is a refactor — which is why it
  was designed before it was wired (now shipped; see the Status note
  above).

---

## ADR-5 — Pre-flight conflict check

**Decision.** Before running optimization, the resolver runs a
**policy / consistency pass** that detects unresolvable
constraint conflicts (jointly unsatisfiable `align` + `place`
combinations, cycles in relative-placement, cross-sheet refs,
etc.) and exits with an error. Optimization is only run when the
constraint system is known to be satisfiable.

**Why.** A best-effort SA on inconsistent constraints produces
a layout that violates user intent silently — exactly what spec
principle 8 ("hard errors on typos, soft warnings on conflicts")
warns against. Detecting it deterministically up front is
strictly better.

**Implications.**

- Conflict detection is its own module with its own test
  surface. Tests are property-based: generate constraint sets,
  check that satisfiable ones pass and unsatisfiable ones get
  diagnosed.
- New diagnostic code(s) for layout-policy errors. As built these
  landed in the `E0xx` range (`E006` directional `place` cycle,
  `E007` layout-unresolved) rather than a separate `E1xx` range;
  they are documented in annotation-spec §7.
- The cost function still has soft `δ` for constraint
  violations as a defense-in-depth, but in practice that term
  should never fire — if it does, it's a bug in the policy
  check.

---

## ADR-6 — Diagnostics interface (neutral type, ariadne at CLI)

**Decision.** Library crates (`spice-parser`, `spice-layout`,
future `spice-route`) emit a neutral `Diagnostic` type. `ariadne`
is a CLI-only dependency that translates `Diagnostic` to a
rendered terminal report at the boundary.

**Sketch:**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity { Error, Warning, Note }

#[derive(Debug, Clone, Copy)]
pub struct FileId(pub u32);

#[derive(Debug, Clone, Copy)]
pub struct Span {
    pub file: FileId,
    pub start: usize,   // byte offset
    pub end: usize,
}

#[derive(Debug, Clone)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub code: &'static str,         // "E001", "W101", …
    pub severity: Severity,
    pub message: String,             // headline
    pub primary: Label,              // offending span
    pub secondary: Vec<Label>,       // "previously declared here", …
    pub help: Option<String>,
}
```

Lives in a small shared crate (`spice-diagnostics` or similar)
or as `spice_parser::diag` re-exported. Each library returns
`Result<Output, Vec<Diagnostic>>` for fatal cases and
`(Output, Vec<Diagnostic>)` for soft-warning paths. CLI has a
single `render::ariadne(&[Diagnostic], &SourceMap)` adapter.

**Why this codebase specifically.**

- `spice-parser` already uses `thiserror` enums with no
  `ariadne` dep — a neutral type is the natural extension, not
  a step backwards.
- The placer will produce *many* diagnostics per run; a
  `Vec<Diagnostic>` channel is needed regardless. Making it
  neutral costs nothing extra.
- Future consumers (LSP server, JSON output for CI, web
  playground) will want non-terminal renderings of the same
  data. Ariadne is a renderer, not a data model.
- Spec §7 codes (`E001`, `W101`, …) are already the stable
  diagnostic API; ariadne is just one view of them.

**Implications & risk.**

- `FileId` is introduced **now**, even though v0.1 has one
  file. Adding `.include` later requires multi-file spans;
  retrofitting `FileId` onto every `Span` after the fact is
  painful. Source files are owned by a `SourceMap` in the CLI.
- Existing `spice-parser` errors get migrated to the new type.
  Small breaking change to that crate's public API; absorb it
  before there are external users.

---

## ADR-7 — Test strategy: property tests over placements

**Decision.** Primary verification is property tests over the
public `Placement` data: no overlaps, all coordinates on grid,
all hard constraints satisfied within tolerance, crossings ≤
baseline (a regression bound, not an absolute), all pins
reachable by orthogonal wires. A handful of `examples/` get
golden *placement* snapshots (not golden `.kicad_sch`) for
regression.

> **Reconciliation (see CLAUDE.md "Constraints vs. costs" — source
> of truth).** "Hard constraints satisfied within tolerance" above
> describes the soft-cost framing (cost.rs `constraint_violation`,
> very high δ). That framing is acceptable only for *continuous*
> preferences; a categorical placement constraint must be a
> candidate-space filter (reject infeasible moves at the
> `propose_move` boundary), because a finite-weight soft term can
> still be undone by an SA move. The property test should therefore
> assert categorical constraints hold *exactly* (a filter never
> emits a violation), not merely "within tolerance".

**Why.** Golden `.kicad_sch` is brittle — every weight tweak
breaks it. Property tests track the things we actually care
about; snapshots catch unintended global regressions.

**Implications.**

- The placer's public API exposes `Placement` as inspectable
  data — a struct of `Vec<PlacedElement>`, not just an opaque
  handle that emits s-exprs.
- `kicad-emitter` consumes `Placement`. Emission is its own
  test surface (s-expr correctness), separate from placement
  correctness.
- Property tests use `proptest` or `quickcheck`. Pick one in
  implementation; not load-bearing now.
- Snapshot format is the same human-readable text the sidecar
  uses (ADR-4) — single representation reused.

---

## ADR-8 — Performance target: ngspice-tractable circuits

**Decision.** Layout must keep up with circuits that ngspice can
realistically transient-simulate on desktop hardware. Concretely
this is **a few hundred to ~1k placeable elements** in interactive
time (target: <10s end-to-end for ~500 elements on a modern
laptop). Pathological larger netlists are best-effort.

**Why.** The output is a schematic for a human to read; circuits
that don't simulate aren't worth drawing. Ties the bound to a
real-world constraint instead of an arbitrary number.

**Implications.**

- FR/KK seeding is fine at this scale (quadratic in nodes is
  ~10⁶ ops — sub-second).
- SA budget should be tunable; default cools fast enough to
  meet the target on typical inputs. Slow-cool flag for cases
  the user is willing to wait on.
- Hierarchy (`.subckt` / `.include`) keeps the per-cluster
  problem small even for big designs.
- Don't spend implementation effort on placement algorithms
  that only pay off above ~10k elements.

---

## ADR-9 — Routing crate

**Decision.** Orthogonal wire routing lives in a separate crate
`spice-route`, consuming `Placement` from `spice-layout` and
producing routed wires for `kicad-emitter` to render.

**Why.** Routing is a different cost surface and a different
algorithm class (rectilinear Steiner trees). Mixing it into
placement couples two unrelated tuning loops.

**Implications.**

- `kicad-emitter` depends on both `spice-layout` (positions) and
  `spice-route` (wires).
- `spice-route` is implemented: a rectilinear-Steiner router
  (Hwang-exact for N=3, RMST + Borah-Owens-Irwin Steinerization
  for 4 ≤ N ≤ 9, plain RMST for N ≥ 10). Defining the crate
  boundary up front kept the emitter API stable across this work.

---

## ADR-10 — Cluster boundaries as soft attractors

**Decision.** `.include` cluster boundaries are **soft
attractors**, not hard rectangles. Members feel a force pulling
them toward the cluster centroid (and a repulsive force keeping
non-members out), but wires cross the boundary freely and the
boundary box is drawn around whatever bounding region the members
happen to occupy after layout.

**Why.** Matches spec §3's "purely visual" framing of `.include`.
Hard rectangles overconstrain the placer in tight layouts and
produce ugly empty space. Soft attraction gives the visual
clustering effect without forcing geometry.

**Implications.**

- Cluster membership becomes a term in the cost function: an
  attractive force among same-cluster elements, mild repulsion
  between clusters. New `θ` weight in §5.
- Cluster bounding boxes are *computed* from final positions,
  not specified up front. Emitted as decorative rectangles
  with the cluster name as a label.
- `.subckt` is **not** a soft attractor — it's a hard
  hierarchical sheet boundary. Keep the two mechanisms
  distinct; do not unify.

---

## ADR-11 — Routing-aware orientation refinement uses the real router

**Status: wired.** Implemented as a placement-stage pass in
`crates/kicad-emitter/src/refine.rs` (`refine_orientations`), called
from the CLI orchestrator (`crates/spice2kicad/src/main.rs`) after
`spice_layout::place_with_hint` and before `kicad_emitter::emit_root`.
It is Layout phase 4.5 in CLAUDE.md.

**Context.** V5 ("the first wire segment at every pin extends outward")
is a *quality* invariant the placer is supposed to satisfy by choosing
good orientations. But a V5 violation is not visible to any
placement-side model: it is **born in the router's post-construction
conflict-resolution passes** (`spice_route::conflict::{avoid_foreign_pins,
avoid_obstacles}`), which re-route the locally-ideal stub away from a
foreign pin or a body to keep V11/V12. A pre-route orientation scorer
(the V5 seed heuristic in `pick_orientations`, the SA) cannot see that
rewrite, so it cannot reliably minimise the *real* V5. A prior
investigation confirmed that trialling allowed orientations and routing
them *for real* reaches the optimum (e.g. `opamp_inverting_real`'s
RIN=R0, RF=R0, X1 un-mirrored → V5 1, down from 3) where a placement-only
model stalls.

**Decision.** Select orientations with the real router in the loop.
Because the measurement requires routing, the pass must live where both
the placer's `Placement` and `spice_route::route` are visible.
`spice-layout` *cannot* depend on `spice-route` (that edge would close a
cycle — `spice-route` already depends on `spice-layout`); `kicad-emitter`
depends on both, so the pass lives there. It runs as a **placement**
phase (before decoration), so the decoration contract ("decoration never
moves/rotates a placed symbol") is untouched: orientation is finalised
before the final `route_nets`/glyph/label pass begins.

**Mechanism.**

1. For each at-risk, non-pinned, non-symmetry element (those producing a
   real V5 violation, plus their shared-signal-net neighbours), trial
   each orientation in the element's V14-allowed set
   (`spice_layout::orient::allowed_orientations` — never widened),
   geometrically deduped so a symmetric resistor's eight orientations
   collapse to the few distinct pin layouts.
2. For each candidate, run the real router (`trial_route`) and measure
   the router's real V5 via the **shared** `kicad_emitter::v5::
   count_outward_violations`. That same function is called by the V5
   verifier (`spice2kicad/tests/electrical_safety.rs`), so the oracle and
   the grader can never drift.
3. Accept a candidate only if it *strictly* reduces total real V5 AND
   does not increase V11 residue, symbol-body overlap, V12 foreign-body
   crossings, or V13 label overlaps. Higher-/equal-tier invariants are
   thus never traded for the V5 gain (CLAUDE.md tier rule).
4. A cheap greedy single-element descent runs first (each accepted step
   strictly lowers V5); a bounded combinatorial joint search over the
   active set (cartesian product capped) handles violations only
   removable by rotating several elements together, early-exiting on the
   first zero-V5 combination. Deterministic throughout (no clock/RNG;
   stable iteration order), so the layout cache stays reproducible.

**Why not a placer-side V5 cost or seed heuristic instead.** Tried and
insufficient: the violation does not exist until the router's conflict
passes run, so no pre-route term can score it faithfully. V5 remains a
soft seed heuristic in `pick_orientations` for the common case; this
phase is the router-in-the-loop refinement that closes the cases the
seed heuristic and SA cannot see. There is deliberately still **no V5 SA
cost term** (see CLAUDE.md "Constraints vs. costs").

---

## ADR-12 — PWR_FLAG driver markers from pin electrical types

**Status: wired.** Implemented in `crates/spice-route/src/pwrflag.rs`,
called from `route()` after Stage 1 power-symbol placement. Pin
electrical types are parsed into `kicad_symbols::PinElectrical`
(`drives()` / `requires_driver()`); the emitter derives per-net driver
state via `collect_driven_nets` / `collect_driver_required_nets`
(`schematic.rs`) and the router places the flags.

**Context.** KiCad ERC reports `power_pin_not_driven` for a `power_in`
pin and `pin_not_driven` for an `input` pin when no driving (`output` /
`power_out`) pin shares the net. Both are Tier-0 (V2) correctness
errors. The schematics we emit have unavoidable undriven nets: every
power-rail glyph exposes a `power_in` anchor, and an AC-stimulus net
whose source is `;@ ignore`d (e.g. `diff_pair`'s base inputs) reaches a
transistor input pin with no in-sheet driver.

**Decision.** Emit exactly one `power:PWR_FLAG` (a single `power_out`
pin) on every net that ERC *requires* to be driven but isn't. The
predicate is purely structural — `requires_driver && !drives`, where
`requires_driver` = the net has a `power_in`/`input` pin OR is a
Power/Ground class net (those always get a `power_in` glyph), and
`drives` = any pin is Output/Power-output/bidirectional/tri-state/
open-collector/open-emitter. **Driven off pin electrical types, never
off fixture/refdes names** (project principle 9), so one rule covers
rails and the input-only signal nets and leaves passive-only R–C
junctions untouched. The flag's anchor pin is wire-coincident with an
existing pin of the same net (V11-safe) and its body points in the host
pin's outward direction (V12/V13-safe). The `PWR_FLAG` symbol was added
verbatim to the fixture `power.kicad_sym` so the emitter inlines it
(V3).

**Hierarchical scope.** Power/Ground nets are global in KiCad
(connected by name across sheets), so their single flag is emitted on
the root sheet only; a child-sheet copy would double-drive the net
(`pin_to_pin`). Subckt-*port* nets on a child are treated as driven
(the parent owns their driver), so a child only flags its genuinely
sheet-local nets.

**Known unfixable case.** `opamp_inverting`'s parent ground glyph sits
on a hierarchical *sheet pin*; KiCad's per-connection driver check
(eeschema/erc/erc.cpp ~L1024-1075) will not credit a parent-side
`PWR_FLAG` to a `power_in` glyph whose connection is defined through a
sheet pin into the child. Verified unfixable by placing the flag on the
glyph anchor, offset+wired, on the child `0` net, and on the child
hierarchical label. It is a genuine KiCad hierarchical artifact (it
predates this work — it was previously hidden by a blanket
`power_pin_not_driven` suppression) and is allowed for that one fixture
and class only in `run_v2`.

**Why not a soft ERC suppression or a placer change.** Suppression
hides real regressions (it hid this very artifact). Pin electrical
type is the faithful, general signal; nothing else distinguishes "this
net needs a driver" from "this net is passive" without reading symbol
pin types, which the model now carries.

---

## Post-mortems / cautionary tales

Detailed narratives of past failures. CLAUDE.md keeps the one-line
*rule* each one yields; the full story lives here.

### V14 / power-glyph orientation — Attempt A and Attempt B

V14 ("power-glyph orientation: GND down, VCC up") is a **hard
constraint** (Tier 1, categorical), not a soft cost. Two earlier
attempts to enforce it failed, in opposite ways, and between them
pin down why the constraint must be a candidate-space filter applied
at *every* stage that can move an element.

**Attempt A — a soft cost term.** A `power_pin_outward` weight was
added to the SA objective (`cost.rs` / `CostWeights`). At any *safe*
weight the term did nothing: the optimiser traded it off against the
other soft terms and routinely left the glyph mis-oriented. Cranking
the weight high enough to dominate destabilised the rest of the
layout. This is the generic failure mode of encoding a *categorical*
property (one correct answer) as a *continuous* penalty: a soft term
is for preferences and tie-breakers, never for a property that must
categorically hold. There is deliberately **no `power_pin_outward`
term in the current tree** — re-adding one re-creates this failure.

**Attempt B — a seed-time filter, but only at seed time.** The
orientation candidate set was filtered at seeding
(`pick_orientations`) to those placing VCC-pins up / GND-pins down —
correct so far. But the SA cost weight was left at 0, and the SA
`rotate` move (`propose_move`'s `rotate`, p≈0.1, `rotate_once` in
`anneal.rs`) then rotated the element back *out* of the filtered set.
A hard constraint at seed-time plus a weight-0 soft cost at
refine-time means the refiner silently undoes the constraint.

**The rule both attempts yield.** A property enforced as a hard
constraint at the seeding/placement stage MUST be hard at *every*
stage that can move the element — both `pick_orientations` *and* the
SA rotate move — either by projecting every move back into the
feasible set or by restricting the move's candidate set. The correct
design for V14: filter the orientation candidate set for any element
bearing a power/ground pin to the VCC-up / GND-down survivors at both
`pick_orientations` and the SA rotate move; when the filtered set is
*empty* (a forced sideways pin), fall back to the
**detached-glyph-with-stub-wire** path — not a soft penalty.

### The V5-scorer rework that regressed V13

An attempt to fix V14 glyph-direction on `common_emitter` by
reworking the **V5 orientation scorer** rearranged the entire layout.
It was "made to pass" only by *loosening V5 / V13 budgets on other
fixtures*. Under the tier ordering this is forbidden twice over:

1. it **regressed a tier** — it broke V13 (Tier 1) to chase a layout
   change, and
2. it **loosened budgets sideways** — paying for one fixture's
   improvement by relaxing another's ratchet.

The lesson, now codified in CLAUDE.md's tier and ratchet rules:
budgets ratchet *down*, never sideways, and a change may never
regress a higher-priority tier to improve a lower one. (The narrow
exception — a change that strictly reduces *total* violations across
all fixtures, with a one-line rationale and user sign-off — is the
"global-improvement escape" in CLAUDE.md; it still never licenses a
Tier-0 regression.)

---

## What we are not deciding now

- ~~Sidecar file format (JSON vs TOML vs custom). Pick during
  implementation.~~ **Decided: JSON via `serde`** (see ADR-4 status).
- Specific RNG (rand vs fastrand vs other). Pick during
  implementation.
- Property-test framework (proptest vs quickcheck). Pick during
  implementation.
- Exact SA cooling schedule and weight values. Tune empirically
  against `examples/`.
- Whether `kicad-symbols` is its own crate or a module inside
  `kicad-emitter`. Decide when extracting.
