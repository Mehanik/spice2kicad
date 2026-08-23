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
3. Accept a candidate whose measured `(V13, V12, V5, bends)` tuple
   strictly improves **lexicographically** — V13 and V12 (Tier 1) lead
   the ordering, V5 (Tier 2) breaks ties among them, and V16 bends
   (Tier 2) is the FINAL key, breaking ties V5 leaves open — AND V11
   residue and symbol-body overlap do not increase (V12 also carries
   its own `<=` non-regression guard, so a V13 gain can never buy a V12
   regression). There is deliberately **no V5 non-regression guard**: a
   candidate that raises V5 while lowering V13 or V12 is accepted,
   because a Tier-1 fix must never be blocked to protect a Tier-2
   metric (CLAUDE.md tier rule). An earlier version of this step
   required V5 to strictly improve while only guarding V12/V13 against
   regression; that was a **tier inversion** — a Tier-1 defect (e.g. a
   wire speared through a body) was only reachable while a Tier-2 (V5)
   gradient happened to point at it, so a router fix that flattened V5
   outright could silently strand a V12 crossing. The lexicographic
   `(V13, V12, V5)` ordering (see `refine.rs`'s `measure`/acceptance
   code) fixed that. The `bends` key was appended later, strictly last —
   see "Accepted extension: V16 bends as phase 4.5's final objective
   key" below and ADR-16.
4. A cheap greedy single-element descent runs first (each accepted step
   strictly lowers V5); a bounded combinatorial joint search over the
   active set (cartesian product capped) handles violations only
   removable by rotating several elements together, early-exiting on the
   first combination with `(V13, V12, V5, bends) == (0, 0, 0, 0)`.
   Deterministic throughout (no clock/RNG; stable iteration order), so
   the layout cache stays reproducible.

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

## ADR-13 — Narrow `RawSexpr` coordinate-transform for emitter-generated power glyphs

**Status: design (v0.2), NOT pursued — premise corrected (see
amendment).** This ADR reopens invariant V3's "verbatim everything"
rule for one tightly-scoped synthesis path and specifies its shape,
integration point, and the V3 amendment it requires. It was the design
for v0.2 Item 5 (CLAUDE.md "v0.2 deferred decisions → Revisit verbatim
`lib_symbols` (V3)"), but the implementation attempt found its
motivating premise does not match the real defect — see the amendment
immediately below before reading the rest.

**Amendment (2026-06-28) — premise corrected; mechanism not landed.** A
v0.2 implementation attempt (Item 5) built and unit-tested this
mechanism in full (`transform_glyph_body`, glyph-variant threading
route→emitter, suffixed `power:GND_R180` inlining), then **discarded
it**, because the forced-sideways premise in point (1) below does **not**
match the actual V14 [3] residual. Measured on `common_emitter`
(byte-identical output on master): the residual is `#PWR1`, a
**correctly-oriented** `power:GND` glyph on R2's *down*-facing pin
(canonical GND-down, rot 0) whose triangle clips a corner of **Q1's
foreign body** — not a glyph bending into its *own* host. It is a placer
**pin-choice / neighbour-placement** defect (where Q1 sits relative to
R2's grounded pin), not a glyph-orientation defect. Rotating `#PWR1`
would dodge Q1 only by pointing the ground triangle *upward* (an
upside-down GND symbol — a V14-intent regression), which this ADR's
transform explicitly does not sanction (it rotates only so the
business-end faces the host pin's *outward* direction, which `#PWR1`
already does at rot 0). Consequences: (a) **the V14 [3] residual stays
deferred to the placer redesign** (MEMORY "V14 placer pin-choice
deferred") — it is not an emitter-glyph-orientation problem; (b) this
ADR's narrow transform only helps a *genuine* forced-sideways case
(filtered orientation set empty), and **no current fixture produces
one**, so the mechanism was unexercised end-to-end and not landed (the
project's "no unused code" rule). Re-open this ADR — the
`transform_glyph_body` design below is sound and cheap to re-create — if
and when a real forced-sideways glyph appears in a fixture.

**Context / problem.** V3 is a Tier-0 portability guarantee: every
`(lib_symbols …)` entry is a byte-for-byte passthrough of the source
`.kicad_sym` body, so the emitter never *synthesises* or *tweaks* a
symbol. The mechanism is `RawSexpr::from_lexpr`
(`crates/kicad-symbols/src/lib.rs:283-302`) mirroring the parsed
`lexpr::Value` into `Symbol::body`
(`crates/kicad-symbols/src/lib.rs:326`), which the emitter
re-serialises unchanged via `lib_symbol_inline`
(`crates/kicad-emitter/src/schematic.rs:816-824`) and
`impl From<RawSexpr> for Sexpr`
(`crates/kicad-emitter/src/schematic.rs:826-834`). The only edit
`lib_symbol_inline` makes is rewriting slot-1 (the bare name) to the
full `lib_id`; all graphics, pins, and properties forward untouched.

That rule blocks two v0.2 features. This ADR addresses one and
deliberately leaves the other unenabled:

1. **The V14 [3] power-glyph body-overlap case (in scope).** V14 locks
   each power glyph to its conventional rotation (rot 0: GND triangle
   down, VCC/VEE chevron up) regardless of the host pin's outward
   direction — `symbol_pose` hardcodes the angle to `0`
   (`crates/spice-route/src/rails.rs:209-214`), and the verifier
   (`tests/placement_quality.rs::v14_*`) asserts `rot == 0` on every
   directional rail glyph. When a GND glyph attaches to an *upward*-
   facing pin, the rot-0 triangle extends back *into* the host symbol
   body. Today's only escape is the **forced-sideways offset + stub
   wire** fallback: `glyph_offset` shifts the anchor one grid cell
   along the pin's outward direction and `stub_wire` bridges the host
   pin to it (`crates/spice-route/src/rails.rs:232-241`, `248-257`).
   This clears the *body* but produces a glyph hanging off a stub in
   an unnatural place — the residual defect tracked as the deferred
   V14 placer item (CLAUDE.md "v0.2 deferred decisions"; MEMORY
   "V14 placer pin-choice deferred"). A *rotated/mirrored* glyph whose
   business-end (triangle tip / chevron) faces the correct outward
   direction would sit flush on the pin with no stub and no body
   overlap — but synthesising that variant is exactly what V3 forbids.

2. **Zero-annotation auto-symbol-drawing (out of scope, NOT enabled).**
   Deriving a body for a symbol the user does not have installed would
   also need synthesis. This ADR's narrow option deliberately does
   **not** enable it (see Consequences).

**Decision.** Reopen V3 to permit a single, narrow synthesis path: a
**`RawSexpr` coordinate-transform** applied to **emitter-generated
power glyphs only** (`power:GND` / `power:VCC` / `power:VDD` /
`power:VEE` / `power:+…` variants / `power:PWR_FLAG`). Every
**user-supplied** `lib_symbols` entry stays byte-for-byte verbatim, so
V3's portability guarantee is fully preserved for user symbols. The
rejected alternative — a full typed graphical-primitive model — is too
invasive and risks V3 across *all* symbols (see Rejected alternatives).

**Scope (precise).** The transform applies **only** to the lib-symbol
bodies of the emitter's own power-glyph family — symbols the emitter
itself owns and inlines from the fixture `power.kicad_sym` (already a
verbatim passthrough today). It never touches a `Symbol` that
originated from a user's `.kicad_sym`. The discriminator is the same
one the rest of the power path already uses: the symbol is an
emitter-synthesised power glyph iff it is produced by the
`spice-route::rails` / `pwrflag` glyph path (`is_power_source` /
`power_lib_id_for_net` lineage), not a placed user element. A
user-installed symbol that happens to be named `power:GND` is **not**
in scope — the transform keys off provenance (emitter-generated), not
lib_id string.

**The transform.** Given a glyph's raw `RawSexpr` graphical tree and a
target orientation (one of rotation ∈ {90, 180, 270} plus an optional
mirror), rewrite the *coordinate-bearing leaves* of the tree:

- **pin positions** — the `(at x y angle)` inside each `(pin …)`, with
  the pin angle rotated by the same amount;
- **graphic-primitive coordinates** — the `(xy x y)` points inside
  `(polyline …)`, the `(center …)` / `(start …)` / `(end …)` of
  `(circle …)` / `(arc …)`, the `(start …)`/`(end …)` of
  `(rectangle …)`, and any `(at …)` on a drawn `(text …)`.

applying the 2-D rotation (and optional axis mirror) about the
symbol's local origin, snapped to the grid (the glyph's primitives are
already grid-aligned, so 90°-multiple rotation keeps them so). The
result is a synthesised glyph whose triangle tip / chevron points in
the chosen outward direction. The transform is a pure
`RawSexpr → RawSexpr` function over the captured body; it does **not**
introduce a typed primitive model — it walks and rewrites the same
opaque s-expr tree the verbatim path already carries. Non-coordinate
leaves (stroke, fill, type, names) pass through untouched, exactly as
in the verbatim path.

Because it is a coordinate rotation of a *known emitter-owned* glyph
by a 90° multiple, it cannot produce malformed graphics: the input is
one of a handful of fixed fixture symbols, not arbitrary user input.
This is the property that makes the narrow transform safe where a
general synthesis pass would not be.

**Integration — where it hooks.** The transform **replaces the
forced-sideways offset + stub fallback** in the `rails.rs` glyph path
for the non-canonical-pin case. Concretely:

- `symbol_pose` (`crates/spice-route/src/rails.rs:209-214`) currently
  returns `(x, y, 0)` and lets `glyph_offset` push the anchor sideways
  when the pin faces the wrong way. Under this ADR, the
  *forced-sideways* branch of `glyph_offset`
  (`crates/spice-route/src/rails.rs:236-238` —
  `is_forced_sideways`) is replaced: instead of offsetting + emitting a
  stub, the emitter selects the glyph orientation whose transformed
  business-end faces the host pin's outward direction, synthesises that
  rotated `RawSexpr` body, and anchors the glyph *directly on the pin*
  (no offset, no `stub_wire`).
- The **sheet-edge** branch of `glyph_offset`
  (`crates/spice-route/src/rails.rs:233-234`) is **unchanged**: a
  sheet-port glyph is offset outward to clear the *sheet body and port
  label*, a different concern that rotation does not solve. Sheet-edge
  keeps offset + stub.
- `value_text_anchor` (`crates/spice-route/src/rails.rs:280-286`)
  already keys off the host pin's outward direction, so it continues to
  place the net-name on the outward side with no change.
- The PWR_FLAG co-location (`flag_rotation`,
  `crates/spice-route/src/pwrflag.rs:157-159` — note the function is
  named `flag_rotation`, *not* `pwrflag_rotation`) already rotates the
  flag to point away from the glyph body; it composes with a rotated
  glyph by reading the glyph's (now-transformed) body direction rather
  than the hardcoded canon.

**Call boundary.** The transform is a `kicad-symbols` helper
(`fn transform_glyph_body(body: &RawSexpr, rot: u16, mirror: bool) ->
RawSexpr`), called from `spice-route::rails` at glyph-emit time — the
one site that both knows the host pin's outward direction and owns the
glyph. The emitter's verbatim `lib_symbol_inline` path
(`schematic.rs:816-824`) is **not** modified: a synthesised glyph
either (a) is inlined as a *distinct* `(lib_symbols …)` entry whose
name encodes the orientation (e.g. `power:GND_R90`), keeping each
`lib_symbols` body internally consistent with the instances that
reference it, or (b) keeps the canonical body in `lib_symbols` and
applies the transform only to the instance — **(a) is preferred**, so
the inlined definition still matches its instances and the V3 verifier
(which compares `lib_symbols` bodies, not instances) sees a
self-consistent file. The orientation-suffixed name lives only in the
emitted file's `lib_symbols`; it is never a user-facing lib_id.

**V3 amendment.** V3 stays Tier-0 verbatim for **all user symbols**.
The single permitted synthesis exception is emitter-generated
power-glyph rotation, justified by V14. Add the following clause to
`docs/invariants.md` V3 (drafted here, applied there marked as the
ADR-13 amendment):

> **Synthesis exception (ADR-13, v0.2).** V3 remains byte-for-byte
> verbatim for every `lib_symbols` entry that originated from a user
> `.kicad_sym`; that portability guarantee is unconditional and
> Tier-0. The *one* permitted exception is the emitter's own
> power-glyph family (`power:GND` / `power:VCC` / `power:VDD` /
> `power:VEE` / `power:+…` / `power:PWR_FLAG`), which the emitter
> may rotate/mirror by a 90° multiple via a narrow `RawSexpr`
> coordinate-transform (ADR-13) so a glyph on a non-canonical pin
> faces outward without the forced-sideways stub. This applies only
> to glyphs the emitter *generates*, never to a user-provided symbol,
> and only as a 90°-multiple coordinate rotation of a fixed
> emitter-owned glyph — it does not introduce a typed primitive model
> and does not enable auto-drawing of unknown symbols. Justified by
> V14 (correct power-glyph orientation without body overlap).

This changes no invariant **semantics** beyond stating the exception:
user-symbol passthrough is exactly as before.

**How V3's existing verifier stays green.** The V3 round-trip test
re-parses each source `.kicad_sym`, locates each *used user symbol* in
the emitted `(lib_symbols)`, and asserts byte equality of the body
sub-tree. That path is **untouched**: no user symbol is ever
transformed, so every user-symbol body still round-trips byte-for-byte.
The synthesised glyphs are emitter-owned power symbols, which the V3
verifier does not (and should not) compare against a user
`.kicad_sym` — they have no user source to round-trip against. If the
verifier currently asserts byte-equality against the fixture
`power.kicad_sym`, it is narrowed to the *canonical* (rot-0) glyph
entries only; the orientation-suffixed synthesised entries are
excluded by name, exactly as the amendment scopes them.

**Rejected alternative — full typed graphical-primitive model.**
Replace the opaque `RawSexpr` passthrough with a typed model of KiCad
graphical primitives (rectangles, polylines, arcs, …) so any symbol
can be synthesised or transformed generically. Rejected:

- It risks V3 across **all** symbols: once the emitter round-trips
  through a typed model instead of byte-copying, any modelling gap
  (an unmodelled primitive, a property-ordering difference, a
  floating-point reformat) silently breaks the byte-verbatim
  portability guarantee for *user* symbols — the exact Tier-0 property
  V3 exists to protect.
- It is far more invasive than the problem warrants. The only thing
  that needs synthesis today is a 90° rotation of a handful of fixed
  emitter-owned glyphs. A general typed model is a large surface for a
  narrow need.
- The narrow `RawSexpr` transform gets the V14 [3] win while leaving
  the verbatim path — and thus V3 for user symbols — completely
  unchanged. The typed model would have to re-establish that guarantee
  by hand.

The symbol-synthesis question for the *general* case (auto-drawing
symbols the user lacks) stays open for a later v0.2 decision; this ADR
deliberately does not answer it.

**Consequences / follow-on.**

- **Item 5 implementation.** This ADR is the design for v0.2 Item 5.
  Implementation: add `transform_glyph_body` to `kicad-symbols`; in
  `spice-route::rails`, replace the `is_forced_sideways` offset+stub
  branch with orientation-selection + body synthesis; emit
  orientation-suffixed `lib_symbols` entries; update the V14 verifier
  to accept a flush rot-rotated glyph (the rotation now lives in the
  synthesised *body*, so the instance angle and the V14 "rot == 0"
  assertion need re-expressing in terms of *effective business-end
  direction*, not raw instance angle); narrow the V3 verifier per the
  amendment. The deferred V14 placer item then closes via decoration,
  not a placer redesign — note this in MEMORY when landed.
- **Sheet-edge glyphs keep offset + stub.** Rotation does not clear a
  sheet body / port label; that path is out of scope and unchanged.
- **Auto-symbol-drawing remains deferred.** This narrow option
  deliberately does NOT enable synthesising bodies for unknown user
  symbols — it only rotates fixed emitter-owned glyphs. Re-open the
  general symbol-synthesis question separately in v0.2 with
  zero-annotation auto-symbol-selection as the motivation (CLAUDE.md
  "v0.2 deferred decisions").
- **V14 stays a hard candidate filter at placement.** The transform is
  a *decoration-phase* glyph synthesis, not a placement-phase
  orientation change of a host symbol — it rotates the *glyph*, never
  the element bearing the pin. The constraints-vs-costs contract
  (CLAUDE.md) is untouched: `allowed_orientations` still filters host
  element orientations; this ADR only changes how the *attached glyph*
  is drawn once the host is placed.

---

## ADR-14 — Reserve power-glyph footprint as placement geometry

**Status: accepted; Option A implemented (see the amendment immediately
below for the as-built scope).** Originally written design-first, to
make the decision defensible before any code and to stop a *fifth*
dead-end (four prior attempts all correctly stopped at the tier floor —
see "Why not …" below).

**Amendment (2026-07-01) — implemented; known scope limits.** Option A
landed (phases 1–4, `glyph_geom.rs` / `world_extent_with_glyphs` /
`footprint_half_extents`) and closed `common_emitter` [3] (1→0). The
as-built reservation is, however, **narrower than the "hard at every
stage" ideal above**, and the narrowing is deliberate (widening it
regressed the opamp fixture — see the scoping comment in
`solver/anneal.rs::symbol_overlap_count`). Record of the blind spots:

- **SA gate: oversized-involving pairs only.** The gate skips
  small×small pairs entirely (they are the cell-bbox cost's job) and
  reserves glyph zones only on *non-oversized* consumers. This covers
  the fixture that motivated the ADR (`common_emitter`'s R2×Q1 — Q1's
  BJT body is oversized and trips the activation) but leaves a small
  foreign body free to drift into a small host's glyph zone during SA.
- **Seed floor: X-only outside `align`.** The layer stride consumes
  only `max_x`/`min_x` of `world_extent_with_glyphs`; the vertical hard
  floor exists only on the align path (`vertical_stride_cells`). A
  vertically-reaching glyph zone gets no seed-time Y protection.
- **Oversized-host self-zones.** A grounded-emitter BJT (`Q1 c b 0`)
  is *itself* oversized, so its own GND-glyph zone gets no SA-gate
  defense (the gate scopes prefs to non-oversized elements) and no
  seed-X protection (the pin is vertical). Only the output ratchet
  guards it.
- **PWR_FLAG bodies are never reserved.** The co-located `PWR_FLAG`
  marker points *anti*-outward of the rail glyph
  (`pwrflag.rs::flag_rotation`), on the opposite side of the pin from
  the reserved reach — the same shape as the scoped-out
  `opamp_inverting_real` `#FLG4` residual.
- **Sideways-transformed rail pins — resolved.** `glyph_reach` maps the
  transformed pin angle with the same table the decoration side uses
  (`angle_to_direction` / `rails::outward_delta`), keeping the
  reservation drift-free with the drawn value text. This used to yield
  the true outward direction only for *vertical* pins — a rail pin
  rotated horizontal degenerated, landing inside the body bbox and
  reserving nothing extra — because `Symbol::pins_in` reported the raw
  `.kicad_sym` angle (inward-facing) instead of the world-outward one.
  The `pins_in` fix (`world_outward = (180 - inward) mod 360`) makes
  the angle genuinely outward for every orientation, so a horizontal
  rail pin now reserves real space too; pinned by
  `spice-layout/tests/glyph_geom.rs::reach_pins_decoration_geometry_across_orientations`.

All these gaps share one guard: the zero-slack output ratchet
`no_power_glyph_foreign_body_overlap_across_fixtures`, which measures
*emitted* geometry and trips on any drift. A possible remedy — widen
the gate activation to "either body is oversized OR either element's
glyph reach exceeds the cell half-extent" — is explicitly **deferred
until a fixture demonstrates the need**: today every measured count is
already at its floor, so widening buys nothing and risks reshuffling
layouts (the within-Tier-1 sideways trade the ratchet rule forbids).

**Completion (2026-07-20) — property text reserved; label text still not.**
Landed here rather than as a stage of ADR-17, per that ADR's retirement
(salvaged part 2: "ADR-14's decoration reservation completed as an
ADR-14 completion, not a stage of this ADR"). ADR-17 Stage 2's
post-mortem found four of its seven Tier-1 breaches were label overlaps,
because this reservation covered the glyph body and value text only —
nothing reserved label or Reference/Value text.

The *property-text* half is now modelled. `world_extent`'s text term was
a width-only `grow(w, 0.0)` ray on +X; it is now a real box that also
reserves the band the Reference (local y −2.54) and Value (local y
+2.54) text occupy above and below the origin. The band is symmetric
because the placer has no orientation-faithful field-direction model —
the emitter's is `field_render_rotation` — and symmetric is the
conservative reading. The WIDTH is still the Value estimate only; a
longer Reference is not modelled.

**It is measurably a no-op on every fixture, and that is the finding.**
The property text's total vertical reach is `VALUE_TEXT_OFFSET_MM +
PROP_TEXT_HALF_H_MM` = **3.44 mm**, which fits inside the align path's
existing **3.81 mm** (3-cell) spacing floor. Probed by exaggerating the
half-height and watching `baseline_lock`: total reach 3.74 mm → diff
EMPTY; 4.04 mm → 12 differences. The floor absorbs the faithful
reservation entirely, with ~0.4 mm to spare.

Consequence, and it generalises beyond this commit: **a decoration
reservation cannot be validated in isolation.** On today's floors a
correct one changes nothing, so no fixture-level test can distinguish it
from its absence — hence the two `property_text_reservation_tests` unit
tests in `spice-layout/src/lib.rs`, which assert the *model* directly.
Without them the term would be silently deletable. This is the precise
sense in which the reservation is a precondition for any compaction
attempt rather than a successor to one: it buys no observable quality
until something removes the slack, and it must be measured together with
whatever does.

**Completion (2026-07-20, part 2) — glyph value text reserved on
horizontally-facing rail pins.** The first live blocker this reservation
gap produced: a denser (correct) multi-channel layout put X2's
left-facing GND glyph text on X1's body (V13(6a) = 3 on
`opamp_definition_level`).

*Mechanism, confirmed against ink.* A rail glyph's Value property is
emitted with no `justify`, so KiCad **centres** it on its anchor
(`TextKind::CenteredProperty` — the same model the V13 verifier grades
against). `kicad-cli sch export svg` confirms it: a `GND` label anchored
at x = 25.40 renders ink x ∈ [23.71, 27.09] — dead centred, 3.39 mm
wide. `glyph_reach` reserved only out to the anchor, so on a
horizontally-facing pin ~1.7 mm of label lay outside the reserved zone.

*What landed.* `glyph_reach` now reserves the value text's **full
rendered box** — both axes — on horizontally-facing rail pins, in BOTH
consumers (`world_extent_with_glyphs` and `footprint_half_extents`), so
ADR-14's single-source rule holds. The box comes from
`kicad_symbols::text_geom` (the ONE text model, held to real SVG ink by
`rendered_text.rs`) and the rendered string from a new shared
`glyph_geom::rail_value_text`, which `rails::glyph_sexpr_at` now also
calls — the placer cannot reserve a box sized for a different string
than the emitter draws.

*Blast radius:* one fixture. `opamp_inverting_real` shifts its
right-hand cluster +2.54 mm in X (one grid cell — the reserved
half-width, snapped); 11 of 109 `baseline_lock` rows. The other eight
fixtures are byte-identical. Pure spacing — no rotation, no mirror, no
reordering. **Every ratchet is unchanged, including V16 (B, J) on every
fixture**; nothing improved, so no literal was lowered.

*What was measured and REJECTED — vertically-facing pins.* Reserving
the text box on vertical pins too (the canonical GND-down / VCC-up case,
where the string's width is *perpendicular* to the pin axis) was
implemented and measured in four ablations. It regresses Tier 1 on
`opamp_definition_level`: label `out2` lands on RF2's body (V13(1)
0→1) and a foreign `INV1` wire crosses the VEE glyph (V13(6b) 0→1),
and V16 J rises 0→2. Applying it in the SA gate is worse still —
`common_emitter` B 4→7 and F6's rail-stub lateral run 4→5 cells —
because that gate's halo is symmetric, so a perpendicular reach blocks
space on *both* sides. The ablation table:

| seed | SA gate | result |
| --- | --- | --- |
| anchor | anchor | master (green) |
| text box | anchor | V13(1) 0→1, V13(6b) 0→1, V16 J 0→2; 21 baseline rows |
| anchor | text box | `common_emitter` B 4→7, F6 4→5 cells; 17 rows |
| text box | text box | union of both; 38 rows |
| **horizontal-only box** | **horizontal-only box** | **GREEN; 11 rows (landed)** |

The reason is the general one: **space reclaimed by one reservation is
space the still-unreserved decoration classes move into.** Label text
and wires remain unreserved, so a faithful vertical reservation does not
remove the collision, it relocates it. That is the same result ADR-17
Stage 2 hit, and it is further evidence for "a complete decoration
reservation is a precondition, not a successor". The vertical half stays
out until label text is reserved too, and should return *with* it.

**Label text remains unreserved** — the larger half. Not attempted: the
information lives in `kicad-emitter`'s `label_specs`, which depends on
routed geometry, so `spice-layout` cannot call it without inverting the
crate dependency. It needs either a shared label-metrics module beside
`glyph_geom`, or a coarse per-signal-pin reservation analogous to
`glyph_reach` (which today skips signal pins outright, so signal pins
reserve zero decoration space). Unlike property text, a `global_label`
is wide enough to exceed the 3.81 mm floor, so this half is expected to
move layouts and trip ratchets — it should come back as an escape
request with numbers, never assumed free.

**Attempt (2026-07-20) — label text IS computable pre-routing; NOT
LANDED, per-consumer measurement below.** Built and measured on branch
`archive/adr14-label-reservation-alone` (`spice-layout/src/label_geom.rs`). Do not
re-derive this; read the table.

*The architectural question is answered: yes, and no new crate is
needed.* Every input to a label's identity is netlist data, not routed
geometry — which nets get one (V4: signal nets only, from
`net_class`), what kind (declared `*@port` → directional
`global_label`; single body terminal → interface `global_label`;
otherwise plain `label`), the text (the net name), and the width
(`kicad_symbols::text_metrics` real Newstroke advances through
`text_geom`, the ink-calibrated model both crates already share). The
policy therefore belongs *placement-side*, as `spice-layout::label_geom`
— the exact shape of `glyph_geom`, with `kicad-emitter` reading it, so
the dependency keeps pointing the safe way. A shared types crate and
caller-supplied estimator injection were both rejected: the dependency
already points the right way, and injection would make the placer's
behaviour a function of caller wiring, which is what ADR-14's
single-source rule exists to forbid. The ONE input that is not known
pre-routing is *which pin* of a multi-terminal net carries the label
(the emitter takes the geometrically leftmost — a function of the very
positions being decided); it is bounded by reserving every candidate
pin.

*Per-consumer measurement.* Four consumers take the reservation. They
are not equally able to hold it:

| consumer | fixtures moved | result |
| --- | --- | --- |
| seed layer-stride (`place_seed`) | 1 (`opamp_definition_level`) | **GREEN, every ratchet unchanged** |
| align stride (`apply_user_constraints`) | 1 (`diff_pair`) | F6 rail-stub lateral run 4 → 6 cells; align value-text gap 2.54 → 1.777 mm |
| `legalize` | 1 (`opamp_definition_level`) | V5 0 → 5; V16 B 12 → 13 |
| SA gate (`footprint_half_extents`) | 6 | F6 + V16 B/J regressions |
| all four together | 9 | union of the above |

*Two mechanisms, each diagnosed — neither is a tuning problem.*

1. **The align stride cannot know the orientation.** It runs inside
   `place_seed` → `apply_user_constraints`, *before* `pick_orientations`,
   so it computes every member's extent in `Orientation::IDENTITY`. A
   power-glyph reach is near-vertical and survived that; a label box is
   strongly one-sided along the pin axis, so identity puts it on the
   wrong side of a mirrored member. On `diff_pair` this widens the
   Q1↔Q2 stride for `in2`'s label, which in the emitted schematic
   points the *other* way (Q2 is mirrored). The over-reservation is a
   modelling artefact, not a faithful footprint.
2. **The SA gate's footprint model is symmetric.** `footprint_half_extents`
   returns half-extents about the origin, so a one-sided reach `(dx, dy)`
   blocks `|dx|` on BOTH sides — already flagged as "a strict-but-
   conservative halo" for glyphs. A ~6 mm one-sided global label becomes
   a ~12 mm halo. Narrowing the scope to global-label nets only does
   **not** help (5 fixtures moved, 4 ratchets regressed), because the
   halo, not the count, is the defect. **The SA gate needs a signed /
   asymmetric footprint before it can hold any label reservation.** That
   is the concrete prerequisite for finishing this.

*Why nothing landed.* The green sub-variant (seed layer-stride only)
buys **no measured quality** — the same result the property-text and
horizontal-glyph-text completions produced, and for the same stated
reason: a reservation buys nothing until something removes the slack.
Landing it alone would also break this ADR's single-source rule, since
the SA would stay blind to a reach the seed enforces. Weighed together,
a partial reservation is a layout move with no upside and a rule
deviation, so it stays on the branch pending owner sign-off.

*It did not unblock the then-pending multi-channel work* (branch
`fix/multichannel-layout`, since landed by other means and deleted
2026-08-08). Cherry-picked on top, `opamp_definition_level` V13(6a) goes
3 → 2 — identical to the naive rebase — and V5 / V16 / F5 fail as
before. The blocker is not label reservation. *(For the record: that
work did land, and went further — `opamp_definition_level` now measures
F5 = 0 and V16 B = 6, against the 1 and 15 this experiment was weighed
against.)*

**POST-MORTEM (2026-07-20) — the symmetric halo is LOAD-BEARING. Do not
"fix" it. The decoration-reservation program is CLOSED.**

The prerequisite the previous amendment named — "the SA gate needs a
signed / asymmetric footprint before it can hold any label reservation"
— was built and measured. **It is not a prerequisite; it is the thing
that breaks.** This is the fourth failed attempt at completing the
reservation, and the first one that explains the other three. Read this
before touching `footprint_half_extents`, `world_extent_with_glyphs`, or
`label_geom`: on its face "make the footprint model honest" looks
obviously correct, and it is obviously wrong.

*The finding.* `anneal.rs::footprint_half_extents` collapses every reach
with `.abs()` (`hw = hw.max(p.x.abs())`, likewise for glyph reaches), so
a **one-sided** reach becomes a **two-sided halo**: a `power:GND` glyph
extends *down* only, a label box is strongly one-sided along the pin
axis, and both get mirrored onto the opposite side for free. Every prior
amendment called that halo "strict but conservative" — an accident to be
cleaned up. It is not an accident to be cleaned up. It is the spacing
the layouts are calibrated to.

*The decisive argument — it is a proof, not a measurement.* The
directional AABB is **provably a subset** of the symmetric box.
Symmetric `hw = max|coord|`, so `[-hw, hw] ⊇ [min_x, max_x]` for any
reach set, on both axes, always. Making the footprint directional
therefore **strictly relaxes** the gate — it can only ever remove
reserved space, never add it. It does not remove *phantom* space that
was doing nothing; it removes space the SA immediately spends on
tighter, worse layouts. Every V13 / V16 / F6 / rendered-text budget on
`master` is calibrated against the halo, so "honesty" is a
across-the-board loosening dressed as a bug fix.

*Measured — directional alone* (tag `archive/adr14-directional-plus-label`,
`d176b9e`), vs `master`:

| fixture | metric | master | directional |
| --- | --- | --- | --- |
| `rc_lowpass` | V13 label↔body (budget 0) | 0 | **1** |
| `rc_lowpass` | V13 item (3) interface-label↔foreign-body | 0 | **1** |
| `common_emitter` | rendered-text overlap (real `kicad-cli sch export svg` INK) | 0 | **1** |
| `common_emitter` | V16 B | 4 | **7** |
| `common_emitter` | F6 rail-stub lateral run | 4 cells | **5 cells** |
| `rc_lowpass_plus_r` | P11 glyph poses | 2 | **3** |

16 `baseline_lock` diffs across 3 fixtures. **Nothing improved anywhere
— not one metric on one fixture.**

*Measured — directional PLUS the complete label reservation*
(the label change cherry-picked on top, giving `8b060cf` — now
`archive/adr14-directional-plus-label`): an
**identical failure set**. The reservation recovers nothing the
directional model gave away. With `legalize` in scope it is strictly
worse (`opamp_definition_level` V5 0→5, V16 B 12→13).

*The corollary, which is the general result.* **An exact directional
model plus a complete label reservation together reserve strictly LESS
space than the accident does.** The reservation program was premised on
the reverse — that faithfulness would reserve *more* and the extra would
buy quality. It reserves less, and the deficit is exactly what the four
attempts kept re-discovering as "space reclaimed by one reservation is
space the still-unreserved decoration classes move into". There is no
remaining unreserved class large enough to close the gap, because the
gap is not an unreserved class: it is the mirrored copy of a reach that
does not exist.

*Two corrections to the prior record.* Both were stated as fact in
earlier amendments and are wrong:

1. **`world_extent_with_glyphs` was never symmetrized.** `WorldExtent`
   is `{min_x, max_x, min_y, max_y}` — signed since it was written — and
   `glyph_reach` already returns signed offsets, folded in with
   `min`/`max`. **Only the SA gate collapsed them.** The gate also built
   its extents in the *opposite Y frame* from the seed (`world_extent`
   applies the eeschema y-flip `(rx, -ry)`; `footprint_half_extents`
   reads `pins_in(orient)` unflipped) — a frame mismatch that `.abs()`
   was hiding. Symmetrizing was never the seed's behaviour, so "make the
   two consumers agree" is not a route back in.
2. **The align stride's `Orientation::IDENTITY` extent is a real latent
   defect — but on MIRROR, not rotation.** The stride
   (`lib.rs::apply_user_constraints`) computes signed extents at
   `Orientation::IDENTITY`, justified by the comment "align-pinned
   members keep identity orientation (`pick_orientations` skips pinned
   elements)". That holds for *rotation* and not for *mirror*:
   `diff_pair`'s `Q2` and `multivibrator`'s `C2` are emitted `mirror y`
   (baseline rows), so a one-sided box is reserved on the wrong side of
   every mirrored align member **today**. It is latent only because the
   halo covers for it. *A claim that `rc_lowpass`'s R1 (rot 270)
   demonstrates this does NOT check out* — `rc_lowpass` has no `*@align`
   directive at all, so R1 never reaches the stride; the only align
   groups in the suite are `diff_pair` Q1/Q2 + RC1/RC2 and
   `multivibrator` Q1/Q2. Worth its own fix if the stride ever carries a
   directional box; harmless while it carries a halo.

*The one suggested future direction — UNTRIED, NOT ENDORSED, needs
owner sign-off.* Keep the reserved **area** roughly constant while
moving it to the correct side: directional extents **plus an explicit
compensating outward margin** sized to preserve today's total reach —
rather than shrinking to honesty and hoping the missing reservations
refill it. This inverts the failed premise (it treats the halo's
magnitude as the calibrated quantity and its *placement* as the bug)
and it is the only variant not yet measured. It is also a
re-parameterisation of every layout in the suite, so it must arrive as
an escape request with the full ratchet table, never as a cleanup.

*Recoverable work.* Three variants exist, preserved as tags (the wip
branches were retired 2026-08-08; an earlier version of this paragraph
had the SHA→branch mapping **backwards**, so read the SHAs, not the old
names):

| tag | SHA | what it is |
| --- | --- | --- |
| `archive/adr14-directional-plus-label` | `8b060cf` | directional gate **+** complete label reservation — and its parent `d176b9e` is the **directional gate alone**. Both experiments are successive commits on this one tag. |
| `archive/adr14-label-reservation-alone` | `0214412` | the **label reservation alone**, on an unmodified symmetric halo, from an earlier base. The per-consumer table above is attributed to this tree. |

`crates/spice-layout/src/label_geom.rs` (318 lines) is byte-identical on
both and exists **nowhere on master** — yet master's own source
forward-references it by name (`footprint.rs:274`, `anneal.rs:586`,
both "once `label_geom` lands"). That file is the single reason these
tags exist. Do not re-derive; re-measure only against a variant this
post-mortem does not already cover.

### Problem

Exactly one class of converted-schematic defect remains: a power
glyph's **body** overlapping a **foreign** symbol body. The tracked
residual ("[3]") is on `common_emitter`: `#PWR1`, a *correctly
oriented* `power:GND` glyph (canonical, rot 0, triangle down) anchored
on R2's down-facing grounded pin, whose triangle clips a corner of
**Q1's** body. Q1 is not the glyph's host — R2 is — so this is not the
accepted "glyph clips its own host" V14 case; it is a foreign-body
overlap. It is contained by the zero-slack ratchet
`no_power_glyph_foreign_body_overlap_across_fixtures`
(`crates/spice2kicad/tests/placement_quality.rs:1943`), budget
`common_emitter` = 1, `opamp_inverting_real` = 1, 0 elsewhere
(`power_glyph_foreign_body_overlap_budget`,
`placement_quality.rs:1921-1927`). The goal is to drive every budget to
0 without regressing any higher/equal tier.

Crucially this is **not** a glyph-*orientation* defect. The glyph is
already in its conventional rotation; rotating it (ADR-13) would only
dodge Q1 by pointing the ground triangle *upward* (an upside-down GND —
a V14-intent regression), which is exactly why the ADR-13 amendment
re-classified [3] as a placer defect, not an emitter-glyph one. The
glyph sits in the *right* place facing the *right* way; the problem is
that **Q1 was placed too close to where R2's ground glyph would land**.

### Root cause — placement is blind to glyph footprint

Power glyphs, PWR_FLAG markers, and net labels are emitted in
**Decoration (Layout phase 5)** by `spice-route` / `kicad-emitter`,
which "reads final symbol positions; never moves them" (CLAUDE.md
"Layout phases", roadmap §4.1). The glyph's geometry does not exist
until decoration: it is first realized in `spice_route::rails`, where
`symbol_pose` (`crates/spice-route/src/rails.rs:209-215`) computes the
glyph anchor from the host pin and `power_symbol_sexpr`
(`rails.rs:288`) emits a `power:*` lib symbol whose body extends roughly
±1 grid cell about that anchor (the bbox the verifier recomputes in
`glyph_body_bbox`, `placement_quality.rs:1882-1906`).

The placement passes that decide where Q1 and R2 sit run *earlier* and
have no model of that footprint:

- The seed/spacing model `world_extent`
  (`crates/spice-layout/src/lib.rs:369-406`) unions only the
  orientation-transformed **body bbox**, the **pin-stem reach**, and a
  **value-text width** estimate. There is no term for the power glyph
  that decoration will later hang off a rail pin — `WorldExtent`
  (`lib.rs:356-362`) has no glyph field, and neither the per-layer seed
  stride (`place_seed`, `lib.rs:781`, `831-838`) nor the align-cluster
  stride (`lib.rs:1015-1046`) reserves any glyph clearance.
- The SA "never-increase" overlap gate `symbol_overlap_count`
  (`crates/spice-layout/src/solver/anneal.rs:428`) measures
  `footprint_half_extents` (`anneal.rs:394`) = body ∪ pin reach — again
  **no glyph term**.
- `cost.rs` has body `overlap` (`cost.rs:312`) and rail-direction
  (`cost.rs:622`) terms, but nothing that knows a glyph will occupy a
  cell beyond a rail pin.

So the placer packs Q1 next to R2 using only their bare bodies, and the
ground glyph's space is *realized later*, in decoration, when nothing
can move. The only place the pipeline reserves glyph space at all is
`spice-layout/src/sheets.rs` — `SHEET_GLYPH_REACH_MM`
(`sheets.rs:52`), which extends a *hierarchical sheet's* de-overlap
rectangle leftward by the port-glyph reach. That mechanism exists for
**sheet port pins only**; it has no analogue for an ordinary element's
rail pin. The four prior dead-ends all tried to bolt glyph clearance
onto a pipeline that does not represent it.

### The phase-ordering tension (and the precedent that resolves it)

Decoration is a strict one-way consumer: it may not move a placed
symbol (roadmap §4.1; CLAUDE.md decoration contract). Yet the glyph's
footprint *must* influence placement, because by the time the glyph
exists the symbols are frozen. The naive resolutions — let decoration
nudge a symbol (breaks the contract / Tier-0 risk), or have
`spice-layout` import `spice-route` to ask where glyphs go (closes a
dependency **cycle**: `spice-route` already depends on `spice-layout`)
— are both blocked.

**Precedent: phase 4.5.** ADR-11's routing-aware orientation refinement
faces the identical cycle constraint and resolves it by living in
`kicad-emitter` — the one crate that sees *both* the placer's
`Placement` and the real router (`crates/kicad-emitter/src/refine.rs`,
called from the orchestrator after `place_with_hint` and before
`emit_root`, `crates/spice2kicad/src/main.rs:287-298`). It is
**placement, not decoration**: it runs before the final
route/glyph/label pass and changes orientation only. This ADR uses the
same boundary-crossing shape — glyph footprint is a *placement* concern
that is computed where both sides are visible, not a decoration nudge.

The key insight separating this from ADR-13: the [3] fix is **not** an
orientation change of the glyph or the host. It is reserving *space* so
foreign bodies never land in a glyph zone in the first place. That is a
spacing/seeding concern, which is exactly where `world_extent` and the
SA overlap gate already live.

### Design options

A power glyph's reach is a *deterministic function of the host pin*:
canonical axis (Up for VCC, Down for GND/VEE — `canonical_axis`,
`rails.rs:134-144`) and a ~1-cell body extent, plus a possible
forced-sideways/sheet-edge offset (`glyph_offset`, `rails.rs:232-241`).
Critically, this reach is computable from **placement-side data alone**
— the resolved netlist's net classes (already in `spice-layout` via
`net_class.rs`) and the element's own pins — *without* importing
`spice-route`. The glyph footprint is small, fixed, and rule-derived,
not router output. That is what makes options A/B feasible without a
cycle.

**Option A — glyph reach as part of effective placement geometry
(recommended).** Teach the placer that a rail pin carries a reserved
glyph zone, and fold that zone into the same `WorldExtent` /
`footprint_half_extents` machinery that already reserves body + pin +
value-text space. Concretely:

- A new pure helper in `spice-layout` (e.g.
  `net_class::glyph_reach(element, orientation, classes) -> Option<
  WorldExtent-delta>`) returns, for each rail pin, the cell(s) the
  glyph will occupy *outward* of that pin (Up/Down per
  `canonical_axis`, ±1 cell body). It encodes the **same geometry**
  `rails::canonical_axis` + the glyph bbox use, but lives placement-side
  and depends on nothing in `spice-route`. (To prevent the two
  definitions drifting, the glyph-reach constant is a shared
  `kicad-symbols` or `spice-layout` const that `spice-route::rails`
  *also* reads — single source of truth, dependency points the safe
  way.)
- `world_extent` (`lib.rs:369`) unions this delta, so the seed stride
  (`place_seed`) and align stride keep foreign bodies a glyph-zone
  clear of any rail pin **as a hard spacing floor at the
  candidate boundary** — the same mechanism, and the same tier, as the
  existing body/pin no-overlap clause (V6 no-overlap, Tier-1).
- `footprint_half_extents` (`anneal.rs:394`) unions the same delta, so
  the SA "never-increase" gate cannot slide a foreign body into a glyph
  zone either (the consistency-requirement rule: hard at *every* stage
  that can move the element).

Because the reservation is added to the *element bearing the rail pin*
(R2 reserves the cell below its ground pin), the placer naturally keeps
Q1 out of that cell — the foreign body is repelled, not the glyph
moved. Nothing in decoration changes; the glyph still emits exactly as
today, but now lands in space the placer guaranteed was clear.

**Label/text footprint (V13), jointly.** Approach 2 (below) failed by
buying glyph clearance and paying in a label-on-body (V13). To avoid
re-creating that, the *same* reservation pass must also account for the
glyph's net-name **value text**, whose anchor is already a deterministic
function of the host pin's outward direction (`value_text_anchor`,
`rails.rs:280-286`). Option A folds the value-text reach into the same
`WorldExtent` delta (it already has a value-text term to extend), so the
placer reserves the *whole* decoration footprint — glyph body + glyph
value text — as one zone. Reserving more space cannot push a label onto
a body: the V13 nudge pass (`nudge_property_text`,
`kicad-emitter/src/schematic.rs`) runs in decoration over a *less*
crowded layout, strictly easier, not harder.

**Option B — glyph-reach repulsion as a soft cost term.** Add a
`cost.rs` term penalizing a foreign body inside a rail pin's glyph
zone. **Rejected.** This is the Attempt-A failure shape verbatim: a
foreign-body-in-glyph-zone is a *categorical* geometric fact (the cell
is occupied or it isn't), and CLAUDE.md "Constraints vs. costs" is
explicit that a categorical Tier-1 property must be a hard
candidate-space filter, never a soft term — at a safe weight it does
nothing; cranked, it destabilizes the layout. Documented for
completeness so a future contributor does not re-propose it.

**Option C — sheet-style post-hoc de-overlap for element glyphs.**
Generalize `sheets.rs`'s `SHEET_GLYPH_REACH_MM` de-overlap (which
nudges a *sheet rectangle* off neighbours after placement) to ordinary
elements: after `place_with_hint`, in a `kicad-emitter` phase-4.5-style
pass, detect glyph-zone/foreign-body overlaps and shift the *foreign*
element away. **Rejected as primary**, viable as fallback. It re-opens
the boundary question (it *moves* a placed element after the placer ran)
and, more importantly, a post-hoc shove fights a finished, constrained
layout: shoving Q1 can break its own V5/V6 spacing or collide it
elsewhere, exactly the "post-hoc gate fighting a finished layout"
failure mode the four dead-ends share. Option A reserves the space
*during* placement so the SA optimizes around it from the start, which
is strictly better. C is noted only as the escape if A proves
infeasible on some fixture.

### Why this avoids each of the four prior dead-ends

1. **SA "never-increase" overlap gate including glyph reach → regressed
   V5.** That attempt added glyph reach to the *overlap gate only*,
   asymmetrically, so the gate rejected the orientations the router
   needed for outward first segments (V5). Option A adds glyph reach to
   **both** the seed/spacing floor *and* the SA gate (the
   consistency-requirement rule), and — decisively — reserves it as
   *extra spacing on the rail-pin element*, not as an orientation
   restriction. It never narrows the orientation candidate set, so the
   V5 seed heuristic and the phase-4.5 router-in-the-loop refinement
   keep exactly the freedom they have today. V5 is untouched.
2. **Seed-time glyph-reach clearance in `place_seed` → regressed V13
   (label onto a body).** That attempt reserved glyph space but ignored
   the glyph's *value text*, so making room for the triangle shoved a
   net-label onto a neighbour. Option A reserves glyph body **and**
   value-text reach jointly (the V13-joint clause above), and the V13
   nudge pass then operates on a *less* crowded layout. The specific
   failure — clearance bought at V13's expense — cannot recur because
   the label footprint is part of the same reservation.
3. **Detached-glyph-with-stub fallback → non-outward first segment (V5)
   / body-cross (V12).** That attempt *moved the glyph* onto a stub,
   creating a wire that either ran inward (V5) or speared a body (V12).
   Option A **does not move the glyph at all** — it stays exactly where
   `symbol_pose` puts it today, on the pin, rot 0. There is no new stub
   wire, so no new V5/V12 surface. (The existing forced-sideways/
   sheet-edge stubs are unchanged and out of scope.)
4. **Emitter glyph-rotation (ADR-13) → GND-up (V14-intent regression).**
   [3] is a correctly-oriented glyph clipping a *foreign* body;
   rotation would point GND up. Option A keeps the glyph at rot 0 and
   instead moves the *foreign element away during placement*. V14 is
   untouched — the glyph orientation never changes.

### Recommendation

Adopt **Option A**: make each rail pin's power-glyph footprint (body +
value text) part of the bearing element's **effective placement
geometry**, reserved as a hard spacing floor in `world_extent` /
`place_seed` / align-stride and as a hard term in the SA
`footprint_half_extents` gate — the identical mechanism and tier (V6
no-overlap, Tier-1) that already reserves body/pin/value-text space.
The glyph-reach geometry is computed placement-side from net class +
host pin (no `spice-route` import, no cycle), with the reach constant
shared single-source so `rails.rs` and the placer cannot drift. This
drives the foreign body out of the glyph zone *during* optimization
rather than fighting a finished layout post-hoc, and it changes **no**
glyph orientation, position, or stub — so V5, V12, V13, and V14 are all
left as they are while the V12/V14-flavoured foreign-body overlap [3] is
removed at its source.

If, on a given fixture, reserving the zone proves jointly infeasible
with `place`/`align` pins (the reservation cannot be honored without
violating a user constraint), the correct outcome is **no regression**:
the placer keeps the user constraint (higher precedence) and the
fixture's ratchet stays at its current value — never bumped. That
fixture then remains a documented deferral, not a budget increase.

### Phased implementation plan

Each phase is independently testable and ratchet-safe (no phase may
*raise* a budget; phases land only when their verifier is green).

1. **Phase 1 — shared glyph-reach geometry (no behaviour change).**
   Extract the glyph body-reach constant + canonical-axis mapping into a
   single shared location (`kicad-symbols` or a `spice-layout` module)
   and have `spice-route::rails` read it instead of its local constant.
   Pure refactor; existing tests must stay byte-identical. Establishes
   the single source of truth before either consumer uses it.
2. **Phase 2 — placement-side `glyph_reach` helper + `WorldExtent`
   fold-in.** Add the pure `spice-layout` helper returning a rail pin's
   reserved glyph+value-text delta; union it into `world_extent`. Gate
   it behind the seed/align stride only at first. Verify: the [3]
   ratchet drops on `common_emitter` and/or `opamp_inverting_real`
   toward 0; **no other budget rises** (run the full
   `placement_quality` + `electrical_safety` suite under the vsize cap).
   Lower the [3] literal to the new measured count in the same commit.
3. **Phase 3 — SA gate symmetry.** Union the same delta into
   `footprint_half_extents` (`anneal.rs`) so the SA rotate/move cannot
   re-introduce the overlap the seed avoided (consistency-requirement
   rule). Verify the ratchet holds at the Phase-2 value under SA
   refinement (it must not regress when `refine: true`).
4. **Phase 4 — drive ratchet to 0 and ratchet down.** Confirm every
   `power_glyph_foreign_body_overlap_budget` entry now measures 0;
   update the literals to 0; the deferred MEMORY note ("V14 placer
   pin-choice deferred") is closed and updated to point at this ADR.
   Only land if **all** of V5/V12/V13/V14 and every other ratchet are
   non-regressed.

### Tier accounting

- **Touches (must not regress):** V5 (Tier-2, pin-facing — preserved:
  no orientation set is narrowed), V6 no-overlap clause (Tier-1 — this
  is the mechanism extended), V12 (Tier-1 — no new wires), V13 (Tier-1
  — value-text reserved jointly), V14 (Tier-1 — no rotation change),
  symbol-symbol/overlap budgets (Tier-1).
- **Improves:** the [3] foreign-body-overlap ratchet (Tier-1
  readability), driven 1→0 on the two non-zero fixtures and held 0→0
  elsewhere.
- **Tier order:** the change is a Tier-1 readability fix implemented as
  a hard spacing constraint; it regresses no Tier-0 (it adds no glyph
  rotation/synthesis — V3 untouched; it changes no wiring — V11/V2
  untouched) and trades nothing from a higher tier to gain a lower one.
- **Ratchets:** every literal moves **down** (or holds); none is bumped.
  Per the within-tier rule, no fixture's budget is loosened to tighten
  another's — reserving glyph space on one element cannot, by
  construction, cost another fixture violations (the reservation is
  local geometry, not a global trade).

### Risks / open questions

- **Spacing inflation.** Reserving a glyph cell on every rail pin
  widens layouts; on a glyph-dense fixture this could nudge content
  past the A4 usable area (V15). Mitigation: reserve **only** the
  outward glyph cell (not a full bbox halo) and verify V15 in Phase 2.
  If V15 trips, the reach is over-estimated — tighten it, don't relax
  V15.
- **`place`/`align` interaction.** A user pin constraint may conflict
  with the reserved zone. Resolution (above): user constraint wins, no
  regression, fixture stays deferred rather than its budget bumped.
- **Drift between `rails.rs` and the placer helper.** Mitigated by the
  Phase-1 shared constant, but the *axis/offset logic* is duplicated;
  a future refactor could share more. Acceptable for now; flag if a
  third consumer appears.
- **`opamp_inverting_real`'s residual may have a different cause.** Its
  budget is also 1; confirm in Phase 2 that it is the same
  foreign-body-in-glyph-zone shape and not a sheet-port artifact (which
  `sheets.rs` already handles). If different, scope it out of this ADR
  rather than forcing one mechanism to cover two defects.

---

## Post-mortems / cautionary tales

Detailed narratives of past failures. CLAUDE.md keeps the one-line
*rule* each one yields; the full story lives here.

### Symbol-body overlap on `opamp_definition_level` is a *seed* defect

`opamp_definition_level` places RF1 overlapping X2's body and RF2
overlapping X1's (~2.0 x 1.3 mm each). The consequences cascade: a
resistor pin ends up strictly inside a foreign symbol body, at which
point the router logs

    v12-placer: net index 4 has own pin (...) strictly inside a foreign
    symbol body; skipping V12 enforcement

and gives up on V12 for that net — yielding 4 wires crossing foreign
bodies and 6 V5 violations. All of it traces to the one placement
overlap, which is why the fixture cannot yet join the graded set.

**The annealer cannot fix it.** With `RUST_LOG=debug` the SA reports
`2 movable / 8 elements` and `best cost X (started X)` — it never moves.
The overlap is baked into the structural seed (classify -> bands ->
layers), and RF/X are not in the movable set.

**Attempt (reverted): let the SA escape an overlapping seed.**
`symbol_overlap_count` is used only as a ratchet — `trial <= current` —
so the annealer can avoid *adding* an overlap but is never obliged to
*remove* one, and `cost::overlap` measures uniform CELL boxes rather than
real bodies (a recorded TODO), so an oversized opamp exerts no extra
repulsion. The attempt measured overlaps whenever the move was
geometrically legal (not only when the metropolis test already passed),
force-accepted any strict reduction, and tracked `best` by
`(overlaps, cost)`.

**CORRECTION (added later).** The ERC errors below were almost certainly
NOT caused by the SA change. A latent router defect was live at the time:
a branch ending on the mid-span of an unbroken trunk emits an
electrically split net, because KiCad connects wires only at segment
endpoints (`SCH_LINE::GetConnectionPoints`; fixed in `24a138c`). Any
placement perturbation that landed a Steiner vertex inside a body
triggered it, and three separate perturbations during that session all
produced "a dropped connection". So a wrong *semantics model* did not
merely hide defects — it manufactured phantom Tier-0 regressions that
vetoed legitimate placer work. That is the strongest argument for
grading against KiCad's own output rather than our model of it. The
entry's other conclusion — that the overlap is a seed defect the SA
cannot reach — still stands and was confirmed independently.

Result: it changed `opamp_inverting_real`'s layout — rotations and
mirrors included — and produced **ERC errors** (`pin_not_connected`,
`pin_not_driven`). Tier 0 is traded for nothing, so it was reverted
despite a genuine side gain (text struck by wires 15 -> 12). It also did
not fix the target: the overlaps survived, because the elements involved
are not movable in the first place.

**The rule.** Do not attack seed-quality defects from inside the SA. The
annealer only refines what the seed hands it, and widening its acceptance
rules to compensate reaches Tier-0 correctness before it reaches the
defect. The fix belongs in the structural seed: bands/layers must size
their slots from real symbol body extents rather than uniform cells, so
an oversized body never lands on a neighbour to begin with. That is the
same "placement under-models decoration's deterministic consequences"
shape as the other two walls, and the same staged oracle work is the way
in.

### Legalization belongs after refinement, not before the annealer

The placer had no owner for "this placement is legal". CLAUDE.md makes
categorical properties hard *filters*, but a filter governs *moves* and
cannot repair an infeasible *start*, so an overlapping seed simply
propagated: `opamp_definition_level` placed two resistors inside opamp
triangles, the annealer reported `2 movable / 8 elements`, and nothing
downstream could fix it.

A shove-to-nearest-legal pass was added. Placing it **after the seed**
looked obvious and was wrong. Measured, with the pass disabled versus
enabled: `electrical_safety` 23/24 → 17/24 while `placement_quality`
went 23/24 → 24/24. It was buying one Tier-2 crossing at the cost of a
Tier-0 short plus four Tier-1 invariants — a trade the ordering rule
forbids outright.

The mechanism is worth recording because no guard could have caught it.
The pass moved `RC` and `CE`, neither anywhere near the eventual fault.
That perturbed the annealer onto a different trajectory, which left
`RE`'s ground pin under net `e`'s trunk, and the router speared it —
`wire (63.500,52.070)→(63.500,59.690)` on net `E` through pin `RE.2` on
`GND`. The violation is *wire-vs-pin in emitted geometry*; every
placement-side guard measures *pin-vs-pin on the resolved placement*, and
`spice-layout` cannot consult the router without a crate cycle. A
placement pass cannot police what the router will later do with its
output.

Its founding premise was also false: the SA's gate admits moves that
*reduce* overlap, so the annealer resolves seed overlaps unaided —
`no_symbol_symbol_overlap_across_fixtures` passes on every fixture with
the seed pass removed.

**The rule.** Legalization runs *after* refinement, gated on the
placement actually being illegal: a no-op where the annealer already
succeeded, and the postcondition's owner where it genuinely fails. More
generally — a corrective pass inserted *upstream* of an optimiser does
not merely add its own effect, it redirects everything downstream, and
the cost of that redirection is unbounded by anything the pass can see.

### Phase-4.5 gate alignment — why the gate stays upstream of decoration

The phase-4.5 refinement gate (ADR-11) scores each candidate orientation
by trial-routing it and counting V5, V11, V12, symbol overlap and V13. Its
V13 term models the labels the emitter *would* plant, via the emitter's own
`label_specs`. After a run of text-geometry fixes made the emitter's label
and field models renderer-faithful, the gate's model was left behind, and
aligning the two looked like obvious cleanup. It was measured, and it is
strictly worse.

**What was tried.** Three increments, each verified against the suite and
against `kicad-cli sch export svg` ink:

1. *Flavour-correct label bboxes* in the gate (`plain_label_bbox` /
   `global_label_bbox` instead of one generic centred box).
2. *Same obstacle classes the verifiers grade* — foreign rail-glyph
   bodies, symbol pin text, label-vs-label.
3. *Same `label_specs` inputs decoration passes* — real pin-text set,
   `anchor_search: true`.

**What happened.** With all three, measured V13 stayed at 0 across every
fixture while V5 regressed: `common_emitter` 0 → 1 and
`opamp_inverting_real` 1 → 2. A pure Tier-2 loss bought nothing.

**Why.** Alignment was only ever partial, and could not be otherwise. The
gate scores **pre-nudge** property anchors, because the real ones are
chosen later by `nudge_property_text`, which consumes the emitted item
list the gate does not build. Making the *label* side faithful while the
*property* side stayed upstream produced a model that is wrong in a new,
more pessimistic way: it sees label/property collisions that decoration
then resolves — property text nudges away across a 24-candidate grid, and
a plain label may take any of four rotations at any pin on its net — so the
gate refused genuine V5 improvements to dodge overlaps that never reach
the page.

**The rule.** A partially-aligned model is worse than a consistently
misaligned one. If the gate cannot model every decoration pass, keep every
part of it one step upstream rather than mixing horizons. Closing the gap
for real means simulating the whole decoration text pipeline per candidate
— route, labels, property nudge, glyph-value nudge. That is feasible (the
gate already trial-routes with the real router) but is its own project.

**Also tried and rejected: reordering decoration.** If labels were placed
*after* the property nudge they would see final property geometry, removing
the gate's excuse. Measured: it regresses V13. Labels are pin-anchored —
they may only sit at a pin of their own net — whereas property text roams a
candidate grid around its own symbol. Labels are therefore the *tighter*
constraint and must be placed first, with the freer pass adapting. The
existing order is correct, and the intuition that "properties are more
constrained" is backwards.

**What did land.** The gate's accept/select rule now honours the documented
tier order: it minimises `(V13, V5)` lexicographically instead of
minimising V5 subject to `V13 <= baseline`. Under the old rule the refiner
could *keep* an existing Tier-1 label overlap while chasing a Tier-2 V5
gain; it now prefers removing the overlap and can never accept a V5 gain
that introduces one. With the current upstream V13 model this is a no-op on
every fixture — it removes a latent tier-order violation rather than
changing today's output.

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

### The collinear outward stub: suppressed too broadly, restored too broadly

`1c75781` fixed a Tier-0 net-severing bug in `cleanup::try_merge` (a
degenerate zero-length merge deleting a Steiner branch). With the merge
fixed, the collinear outward stub in `route_two_pin_with_outward` — which
that merge had been silently deleting — started surviving as a dangling
whisker, so the commit stopped generating it. Its stated reason was that
in the collinear case the stub is always re-covered by the continuation
and therefore "buys no V5 compliance".

**That reason is true of only one of two sub-cases**, and suppressing
both cost 7 V5 violations across four fixtures (`common_emitter` 1→3,
`multivibrator` 4→5, `diff_pair` 0→2, `opamp_definition_level` 3→5).

- Failing direction ALONG the shared axis: the continuation must travel
  that same line, so it does retrace the stub. Whisker. Suppress.
- Failing direction PERPENDICULAR to it: the stub lifts the run onto a
  parallel axis one cell over and the continuation may cross at *that*
  axis, rejoining the shared line only at the far pin. Not re-covered,
  and the first segment genuinely leaves the pin outward. This is the
  ordinary jog-around route.

Which sub-case applies is **not** predictable from the geometry alone —
it flips with run orientation, because the continuation defaults to a
horizontal-first L. So the current code builds the stub route and checks
it: a route that revisits the pin after leaving it is rejected.

**Two walls found while restoring it, both Tier 0, both worth not
re-running into.**

1. *Always emitting the 3-leg jog* (steering the continuation onto the
   offset axis instead of rejecting the retrace) reaches the same V5
   counts but pushes trunks off the pin row on both axes. On the
   symmetric fixtures two nets then land on the same offset channel —
   `common_emitter` C/E, `multivibrator` B1/B2,
   `opamp_definition_level` OUT1/OUT2 — a cross-net collinear overlap,
   i.e. a latent V11 short that the single-track jog reports as
   "unresolved by single-track jog (channel router — v0.2)".
2. *The stub makes a leg three segments where the plain route is one*,
   and the Stage-3 jog does not always carry a 3-segment leg across
   intact: it can move the trunk and leave the far riser behind.
   `examples/rc_lowpass.cir` net `in` came apart exactly so. This is a
   latent defect in the jog, not in the stub.

Both are handled the same way, in `spice_route::route`: Stages 3–3d and
cleanup now run inside a bounded retry, and a net that ends up severed
or in an unresolvable overlapping pair has its **collinear stub only**
suppressed and is re-routed. Suppressing the pin's outward *hints*
entirely is the wrong granularity — it also surrenders the L-corner V5
the net was getting for free (measured: `multivibrator` 4 → 6). The
retry degrades monotonically toward "no stubs anywhere", which is the
pre-restoration geometry, so it can never ship worse than that.

**Also recorded: a model/render gap in property-text placement.** The
restored routes let phase 4.5 re-pick `rc_lowpass_ports` R1 to rot 180,
where `nudge_property_text` chose an anchor its own model scored as
perfectly clear but which `kicad-cli` rendered kissing a pin number by
0.06 mm. The nudge's pin-text obstacles now carry a 0.5 mm clearance
(0.25 mm was still too small — measured, not guessed). Only the SVG-ink
test can see this class of defect; the modelled V13 checks cannot.

### A hinged residual is not a collision check — the rail-stub column collapse

`*@align horizontal R1 C1` or `C1 … ;@ place=right-of R1` on
`rc_lowpass_ports` emitted **both symbols at one origin** (`35.56 35.56
rot 0`), shorting the two nets; the CLI's connectivity verifier failed
the conversion *after* writing the file. Tier 0.

**The obvious diagnosis was wrong, and cost the first pass of this
investigation.** It looked like a pin-extent problem: `place` is
pin-anchored, and in identity orientation both `Device:R_US` and
`Device:C` put *both* pins at x = 0, so `right-of` appears to have zero
horizontal extent to separate against. Measured, `solve_place` is
**correct** — it opens the full `CELL_W` (7.62 mm) on exactly this pair.
Confirmed by stubbing out one call: with `apply_rail_stub_columns`
disabled the same input emits R1 at 35.56 and C1 at 43.18.

**Actual cause.** `C1` is a ground-side rail stub on net `out`, so
`idioms::apply_rail_stub_columns` snapped its X to that net's anchor
column — which is `R1`'s own column. The idiom's revert guard scored
only `cost::constraint_residual`, and `place_residual`'s `RightOf` X
term is a **one-sided hinge** (`(ax - tx).max(0)`, ε = 0): collapsing the
target onto the anchor scores an unchanged zero, i.e. "not strictly
worse", so the guard waved it through. A `place`d element is
`user_pinned`, so the post-refinement legalizer would not repair it
either.

**The rule.** *A hinged/one-sided residual can never serve as a
collision guard.* It is satisfied at zero separation by construction —
that is what makes it a good SA objective and a useless safety check.
Any pass that relocates an element onto a column derived from *other*
elements needs a categorical, measured no-overlap condition alongside
the residual. `apply_rail_stub_columns` now also reverts wholesale when
`legalize::overlap_count` *rises*, which covers the whole class (any
stub snapping onto an already-occupied column), not merely the
annotated case.

**Why a fix and not a diagnostic.** A pre-flight hard error in
`spice-policy` was considered and rejected: the layout the user asked
for is legitimately expressible and now renders correctly, so blocking
it would trade silent corruption for a spurious refusal. Per CLAUDE.md
principle "hard errors on typos, soft warnings on conflicts" — this is
neither; it was a converter defect wearing a user-input costume. No new
diagnostic code was added. Verifier:
`spice2kicad/tests/place_no_coincidence.rs`.

### `place=above` / `below` were inverted — the second screen-Y-sign bug

Spec §4.3 defines `above` as "anchor's top edge → element's bottom
edge": the annotated element sits **above** the anchor, i.e. at the
*smaller* y, because KiCad screen Y grows downward. Both `solve_place`
(`spice-layout/src/lib.rs`) and `cost::place_residual` picked the
anchor's **max-y** pin as its "top", so `;@ place=above R1` emitted the
element *below* R1 and vice versa — consistently, on both the seed and
the SA-objective side, which is why nothing caught it.

Nothing caught it for two further reasons worth recording: no fixture
uses `above`/`below` (the only `place` uses in the tree are
`right-of`), and `spice-layout/tests/properties.rs::check_relation`
was itself written against the buggy direction, so the property test
*confirmed* the inversion instead of falsifying it. A verifier derived
from the implementation rather than from the spec is not a verifier.

This is the same class as the `cost::rail_direction` inversion
(`pin_extents_y` returns `(y_min, y_max)`; screen Y grows downward) —
third occurrence of the sign confusion in this file's history. Fixed on
the code side (the spec is the contract). Direction is now pinned
against the spec's own wording by
`spice-layout/tests/place_direction.rs` and, end-to-end, by
`spice2kicad/tests/place_no_coincidence.rs::above_and_below_match_the_spec_direction_end_to_end`.

---

## ADR-15 — Readability-first placement: constructive role/anchor placement, demoting the SA to a polisher

**Status: partially implemented** (Stages 0, 1, 2 and 4 landed; Stage 3
not landed; Stage 5 not started). Written up after the fact — the
decision drove a session's worth of implementation while living only in
an agent's report. Where the as-built code differs from the design, the
**code** is recorded as the truth and the divergence is called out
explicitly below; do not read the "Decision" section as a description
of current behaviour without the "Corrections" section next to it.

**Context — the diagnosis.** The placer was believed to behave like an
area/wirelength-minimising PCB placer. Measurement disproved it:

- `hpwl` carries weight **1.0** (`cost.rs:127`, self-documented as "a
  tiny regulariser") and contributed **156.21 at weight 1.0 — ~0.05% of
  the SA objective**. It has never driven a layout.
- The dominant term was `cost::rail_direction` (weight **200.0**,
  `cost.rs:128`), at ~81–97% of the objective — and it carried an
  **inverted Y convention**. `pin_extents_y` returns `(y_min, y_max)`;
  the caller bound it as `(y_top, y_bot)`; screen Y grows *downward*.
  The net effect was to pull positive rails to the screen bottom and
  grounds to the top — the objective was actively optimising for an
  upside-down schematic. Fixed in `00ea294`; the hinge now reads
  `VertPref::Up => (y - y_min).max(0.0)` / `VertPref::Down =>
  (y_max - y).max(0.0)` (`cost.rs:717`+).

That single sign error surviving unnoticed is the symptom; the
architectural defects behind it are:

1. **An undifferentiated bag of quadratics.** `CostBreakdown` has 12
   terms, **eight** of them squared-displacement mm² terms with weights
   spanning **20 → 1000** (`layer_order` 20, `signal_flow` 25,
   `band_misalignment` 50, `soft_y_residual` 50, `rail_stub_alignment`
   50, `band_inversion` 100, `rail_direction` 200,
   `constraint_violation` 1000), plus `overlap` (200, an *area*), and
   three non-quadratics (`hpwl` 1, `net_bbox_crossings` 4, `crossings`
   100). One sign error inside that sum dominates silently and nothing
   in the suite can see it.
2. **Categorical properties encoded as gradients.** Readability
   conventions ("a bypass cap hangs below its device", "signal flows
   left to right") are yes/no geometric facts. Encoding them as
   weighted mm² penalties violates the project's own
   constraints-vs-costs rule (CLAUDE.md) — at a safe weight, a soft
   term routinely changes nothing.
3. **The SA barely matters.** ~200 iterations of ±2-cell jitter on a
   layout ~90% determined by the constructive seed. Treating it as the
   thing that "produces" the placement misallocates every fix.

**Decision — the role model.** Place constructively from *structural*
roles, and demote the SA to a polisher that may not undo them. Roles are
derived from **pin counts, pin angles and net classes only** — never
refdes, element kind, or a named topology (CLAUDE.md principle 9,
"structural placement, not pattern recognition"):

- **Anchor** — an element with **≥3 pins**. Its placed pins define the
  columns (pin X) and rows (pin Y) that other roles hang off.
- **Rail stub** — a 2-terminal element with exactly one rail-class pin;
  it terminates a node. Its column is the anchor-side X for the signal
  net; its side is the rail's `VertPref`. **Implemented** as
  `idioms.rs` idiom 4 (`detect_rail_stubs` / `apply_rail_stub_columns`)
  — see the corrections below for how the as-built column and side
  differ from this sentence.
- **Series element** — 2-terminal, both pins signal-class. It lies on
  the signal path, is drawn with a horizontal pin axis, and is placed in
  the flow lane between its neighbours.
- **Terminal nets** — `*@port` nets, plus the leaf-net name conventions
  in `layers.rs::no_source_fallback` (`in`/`input`/`vin*` →
  left, `out`/`output`/`vout*` → right), pin lanes to the far left
  (inputs) and far right (outputs).

**Key consequence — conventions are constructive assignment plus the
`pinned` mask, not SA cost terms.** Pinning is the only mechanism in the
tree that is *trivially hard at every stage*: the SA never proposes a
pinned element (`anneal.rs:72` filters `movable` by `!pinned`, and
`propose_move` only ever indexes `movable`), and phase 4.5 skips it in
both sub-searches (`refine.rs:129` in `greedy_descent`, `refine.rs:261`
in `joint_search`). There is no gate to keep in sync — which is exactly
the failure mode the constraints-vs-costs rule exists to prevent, and
exactly what bit the V14 Attempt-A post-mortem.

**Corrections to the naive conventions** (derived during review; each
one was a wrong rule someone would otherwise have coded):

- **"Capacitors are horizontal" is WRONG.** The correct generalisation
  is *series-on-signal-path elements are horizontal*. A bypass capacitor
  (e.g. `CE`) is a **rail stub** and stays vertical. The role model
  supplies the discriminator structurally, so no element-kind test is
  needed.
- **"GND-connected elements go down" is a per-terminal stub rule, not a
  per-element rule.** A transistor that touches ground *through* an
  emitter resistor must not sink to the bottom band. Only an element
  whose own pin is on the rail is a stub.
- **Two stubs on the same node cannot both be exactly on-column.** The
  convention is nearest-on-column, with siblings spread symmetrically
  about the anchor at the derived stride.

**Staging.**

| Stage | Content | Status |
| ----- | ------- | ------ |
| 0 | rail-direction Y-convention fix + rail-stub idiom | **LANDED** (`00ea294`) |
| 1 | verifiers | **LANDED** — `flow_geometry.rs` F3/F4, `wire_geometry.rs` V16 |
| 2 | stub column assignment | **LANDED** — `idioms.rs` idiom 4 (column only; does *not* pin — see below) |
| 3 | `align` shared-axis-only semantics | **NOT LANDED** — needs an annotation-spec change and owner sign-off |
| 4 | flow hardening, positions only | **LANDED** (`b8f5df1`) — monotone flow-order gate in `anneal.rs` |
| 5 | flow orientation via `allowed`-set filtering in `orient.rs` | **IMPLEMENTED, MEASURED, ABANDONED** — reverted; see the Stage-5 post-mortem below |
| 6 | consolidation | not started |

### Measured corrections — where the design's claims do not match the code

Recorded deliberately, so the next reader re-derives nothing:

- **`cost::layer_order` contributes exactly nothing on every current
  fixture.** `cost.rs:1150-1157` returns `0.0` whenever
  `layer_asg.no_source_fallback` is set — and that flag is set on every
  fixture we have, because they all `;@ ignore` their source, leaving
  the layer assignment with no root. The design's premise that the soft
  term was merely "outvoted at weight 20" is therefore **wrong**: at
  weight 20 it is multiplied by a constant zero. This strengthens the
  case for the Stage-4 hard gate rather than a weight bump — there was
  never a weight that could have worked.
- **F3 (flow inversions) already measured 0 on every fixture.** The
  `FLOW_RATCHET` table (`flow_geometry.rs:523`) is `(0, 0)` for all ten
  fixtures. The Stage-4 gate **protects** that property; it did not fix
  anything. An earlier F3 draft counted rail stubs as flow pairs and
  consequently scored the *conventional* drawing as defective — rail
  stubs must be excluded from the pair set. Both the gate
  (`anneal.rs:687`) and idiom 4 exclude them via
  `idioms::detect_rail_stubs`; the F3 verifier reimplements the
  predicate independently from the netlist (`flow_geometry.rs:229`)
  rather than calling into the crate under test, on purpose.
- **Idiom 4 does not pin, and does not set the stub's side.** The
  "constructive assignment + `pinned` mask" consequence above is the
  *design*; as built, `apply_rail_stub_columns` takes `pinned: &[bool]`
  **immutably**, skips any group containing a pinned member
  (`idioms.rs:766`), and mutates **X only**
  (`origin = GridPoint::new(x + dx_cells, origin.y)`, `idioms.rs:816`).
  The rail's `VertPref` is used only as a *grouping key*; which side of
  the device a stub actually lands on is still decided by the soft
  `cost::rail_direction` term. So Stage 2 delivered the column, not the
  hard guarantee — the mask half of the decision is unbuilt.
- **The stub column is not literally "the anchor pin's X".**
  `rail_stub_anchor_x` (`idioms.rs:652`) uses the **mean world X of the
  signal net's vertically-facing pins on ≥3-terminal elements**, falling
  back to all non-stub vertically-facing pins, and excludes pins with
  `angle % 180 == 0` (a stub cannot hang off a left/right-facing pin).
  No anchor → the seed column is kept.
- **Terminal-net lane pinning was implemented, measured, and
  REJECTED.** It was the remaining Stage-4 item. Results: V16 `B` on
  `rc_lowpass_ports` rose **2 → 3**, with **no** measurable F3/F4
  improvement (both were already 0), and it made the owner-visible
  defect *worse* — R1 flipped rot 180 → 270 and slid to the right of
  C1. Recorded as tried-and-rejected with the numbers so nobody
  re-attempts it blind. Note the `rc_lowpass_ports` B budget is at
  **2** today, past its own pre-escape mark of 3; the rejected change
  would have spent that hard-won ratchet for nothing.
- **Stage 5 was implemented, measured, and ABANDONED (reverted).** See
  the dedicated post-mortem immediately below. `orient.rs` at HEAD is
  again purely V14 (`allowed_orientations` at `:110`, filtering
  `Orientation::ALL` by `satisfies_v14`) with no flow-orientation logic
  in the tree.

### Stage-5 post-mortem — flow orientation via `allowed`-set filtering

**Status: tried, measured, reverted. Do not re-attempt as-is.**

**What was implemented** (all inside `orient.rs::allowed_orientations`,
no other crate touched):

- `is_series_signal_element` — the structural discriminator: 2-terminal,
  role is not `Power`, and NEITHER node appears in `vertical_prefs` (so
  ground, supply and negative-supply rails are all excluded). Purely
  pin-role-derived; no element-kind and no refdes matching, per CLAUDE.md
  principle 9.
- `horizontal_axis_subset` — filters the candidate set down to the
  orientations where every mapped pin is `ScreenFacing::Horizontal`,
  falling back to `base` when the filtered set is empty.
- Series elements fall into the existing
  `nodes.len() <= 2 && !has_rail_pin` early-return branch
  (`orient.rs:140`), so intersecting with V14 is **vacuously safe**: a
  series element has no rail pin by construction, therefore V14
  constrains nothing and nothing is lost to the intersection.

**The mechanism worked.** Both owner-reported defects were fixed:

| Fixture | Element | Before | After |
| ------- | ------- | ------ | ----- |
| `common_emitter` | COUT | `(at 90.17 52.07 0)` (vertical) | `(at 78.74 48.26 90)` (horizontal) |
| `rc_lowpass_ports` | R1 | `(at 41.91 35.56 180)` | rot 90 (horizontal) |

The structural discriminator behaved exactly as designed: COUT went
horizontal while CE (a bypass capacitor, i.e. a rail stub) correctly
stayed vertical — decided from pin roles alone.

**Tier 0 HELD.** V11 and V2/ERC were clean. This matters: the prior
"flow-orientation wall" record predicted a Tier-0 V11 regression *plus*
the phase-4.5 V5 oracle undoing the change. **Neither recurred.** The
`allowed`-set filter genuinely is structurally different from a seed/SA
tie-break, and it did survive phase 4.5 — exactly as this ADR argued it
would. That half of the wall is disproven.

**It failed on Tier 1 instead:**

| Invariant | Fixture | Before → After | Budget |
| --------- | ------- | -------------- | ------ |
| V12 (foreign-body wire crossings) | `rc_lowpass` | 0 → 2 | 0 |
| V13 (label↔body overlap) | `rc_lowpass_ports` | 0 → 1 | 0 |
| V16 bends (B) | `common_emitter` | 4 → 11 | ratchet |
| V16 branches (J) | `opamp_inverting_real` | 0 → 1 | ratchet |
| V5 (Tier 2, for completeness) | `rc_lowpass` C1.1, `opamp_inverting_real` X1.1, `rc_lowpass_ports` C1.1 | 0 → 1 each | ratchet |

The V13 hit is the global label `"out"` overlapping C1's body; it trips
both the V13 verifier and the item-3 interface-label verifier, both at
budget 0.

**Root diagnosis — the durable insight:**

> **Making the orientation choice hard does not make it *good* — it makes
> it *permanent*.**

On these fixtures the flow proxy genuinely DISAGREES with the router's
measured V5. Previously phase 4.5's oracle silently reverted the bad
choice, so the damage was invisible. Removing the oracle's ability to
revert did not improve the choice; it merely let the disagreement surface
downstream as router damage.

**Two distinct failure modes — they need different fixes, keep them
separate:**

1. **Local (`rc_lowpass_ports`) — axis is only half the constraint.**
   R1 did go horizontal, but the MIRROR flipped the flow: the emitted
   `in` global label landed at x=45.72 and `out` at x=38.1, i.e. input
   on the right, output on the left — backwards from the requested
   convention — and the `out` label then collided with C1's body.
   Constraining the *axis* leaves the *direction* (mirror state)
   unconstrained, and a horizontal 2-pin element has two mirror states
   that V5 rates identically. The obvious next increment is a
   **port-net-facing filter** (input pin faces left, output pin faces
   right), constraining direction rather than axis. It was deliberately
   not attempted *here*, because failure mode 2 blocks landing either way.

   **CORRECTION (2026-08-08): its multi-pin form WAS later attempted, and
   the outcome differs from what this post-mortem predicts.** Tag
   `archive/opamp-output-facing-experiment` (`34c907a`) holds a hard
   candidate-space filter keeping only orientations that face every
   KiCad `output` pin screen-right, for elements with ≥3 terminals (2-pin
   passives excluded precisely to avoid re-running this Stage-5 case).
   It works mechanically — the opamp triangle points right, V16 B 8→5,
   the GND glyph clears `RF` — and still fails, but for a **placement**
   reason, not failure mode 2: `X1` had been seeded *left of* `RIN`, so
   the mirror was the router's locally-optimal answer to a bad position.
   Orientation was the symptom; position was the cause. That defect is
   fixed on master by the placement-side root refinement the experiment's
   own post-mortem prescribed (`layers.rs` `no_source_fallback` roots
   restricted to genuine rail stubs), and the textbook facing is now held
   by channel-row seed pinning. Note also that the experiment's V5 cost
   (`X1.1`/`X1.2` losing their outward first segment) is exactly the
   class a genuinely constrained maze search would address — see ADR-21
   on `maze_shortest_path_constrained` never having constrained anything.
2. **Global (`common_emitter`) — SA basin shift.** Shrinking one
   element's allowed set perturbed the entire SA trajectory into a
   different basin: *every* element moved (R2 55.88 → 35.56, Q1 63.5 →
   49.53, all seven power glyphs), and B jumped 4 → 11. This is not a
   local orientation cost and cannot be fixed by a better orientation
   rule. It is the same placer redesign the V14 residual is waiting on
   (ADR-14 "Known scope limits"), and it blocks landing Stage 5
   regardless of whether mode 1 is solved.

**Methodological note (this caused a false clean read).**
`--no-fail-fast` is essential when measuring this class of change: the
first verification run stopped at `baseline_lock` and looked clean,
hiding all three Tier-1 regressions behind it.

**Revised statement of the "flow-orientation wall".** The wall is NOT
what was previously believed — it is not that the phase-4.5 oracle always
undoes the change, and not that a Tier-0 V11 regression is unavoidable.
Both were beaten by the `allowed`-set filter. The wall is that **the flow
proxy and measured routing quality genuinely disagree**, and
hard-constraining the proxy converts a silently-reverted bad choice into
a permanent one — plus the SA-basin sensitivity of mode 2. Landing flow
orientation requires the placer redesign that reconciles flow and routing
holistically, not a better filter.

---

## ADR-16 — The baseline-diff protocol: two instruments for a coupled loop

**Status:** accepted.

**Context — the systemic hole.** Layout phase 4.5
(`crates/kicad-emitter/src/refine.rs`, ADR-11) uses the **real router**
as its orientation oracle: it trial-routes candidate orientations and
keeps the one minimising the router's *measured* V5 violations. That is
the right design — a V5 violation is born inside the router's
conflict-resolution passes and is invisible to any placement-side cost —
but it makes placement a **function of router behaviour**. Any change
inside `crates/spice-route/` can therefore shift placement *globally*.
This is not hypothetical: a wire-straightening pass once rotated a
transistor two crates away from the edit, passed every gate in the
suite, and had to be reverted on sight.

`crates/spice2kicad/tests/baseline_lock.rs` already snapshots every
element's `(refdes, lib_id, x, y, rot, mirror)`, so the *movement* is
detectable. The hole is downstream of that: **regenerating the baseline
is a legitimate mechanical act**, and no gate distinguishes "the
baseline moved and the layout got better" from "the baseline moved and
the layout got worse". A contributor who regenerates in good faith
launders a regression through a green suite.

**Decision — a two-instrument protocol.** `baseline_lock` detects
*motion*; the quality ratchets judge *direction*. Neither alone is
sufficient, so both are required, in this order:

1. **A change confined to `crates/spice-route/` MUST produce an EMPTY
   `baseline_lock` diff.** Router changes are supposed to alter *ink*,
   not *placement*. If the baseline moved, the change leaked through
   phase 4.5's oracle. That is not automatically wrong — a better router
   can legitimately teach the refiner a better orientation — but it
   **reclassifies the change**: it is no longer a routing change, it is a
   layout change, and it invokes rule 2.

2. **Any change that regenerates the baseline MUST show V16 (B, J)
   non-increasing per fixture**, alongside the existing V5 / V12 / V13 /
   crossing / detour ratchets. Report the before/after table in the
   commit message. This is what converts "the layout moved" into "the
   layout moved and here is the evidence it improved". V16 is the
   instrument this protocol was missing: bends and branches are the
   quantity a wire-straightening pass claims to improve, so a pass that
   moves placement while *raising* B is exactly the failure mode above,
   now visible.

Standard ratchet policy governs rule 2 — the literals go down, never up,
and the CLAUDE.md global-improvement escape (strictly-fewer TOTAL
violations across all fixtures, one-line rationale, user sign-off) is the
only path to a single fixture's rise.

**Rejected alternative — freeze phase 4.5's oracle.** The obvious
decoupling is to give `refine` a *private pinned copy* of the router, so
`spice-route` edits cannot perturb placement at all. Rejected: it trades
one failure class for a worse one. The refiner would then optimise
orientations against **a router that no longer exists**, and every
improvement to the real router would silently widen the gap between the
orientation chosen and the wires actually drawn — the "wrong oracle"
class ADR-11 was written to avoid. A stale oracle produces confidently
wrong answers with no diff to inspect; a live oracle produces a
`baseline_lock` diff, which is a *signal*. **The guard belongs at the
gate, not in the oracle.** Coupling is the price of measuring reality;
the protocol above is how we pay it.

**Consequences.** Contributors touching `spice-route` should expect to
run `baseline_lock` first and treat a non-empty diff as a scope change
rather than a nuisance. Reviewers should refuse a baseline regeneration
that arrives without the V16 table.

**Accepted extension: V16 bends as phase 4.5's final objective key.**

`rc_lowpass_ports` currently costs B = 4. Its `out` net must leave C1.1
upward and enter R1.2 from below at a different X, and the rectilinear
minimum for that shape is provably 4 bends. Rotating R1 to 180 puts both
`out` pins on one row and yields **B = 2** — below even the pre-pin-angle
mark of 3 — while staying V5-clean with no V11 / V12 / V13 / overlap
change. This was verified end-to-end by pinning the orientation through
the layout cache and re-measuring the emitted ink, not predicted.

It was unreachable because rot 0 and rot 180 **tie** on (V13, V12, V5):
`greedy_descent` skips the fixture (neither element is a V5 or V12
offender at the point it runs), and `joint_search` stopped at the first
zero-cost combination it enumerated, which is rot 0. Only a bend-aware
key can separate them.

**Status: adopted on project-owner sign-off following design review.**
The review (by ADR-16's own author, re-examining their own rule) found
the original "never an in-loop objective" wording too absolute: it
conflates "in-loop" with "able to trade against Tier 1", which coincide
only in a weighted sum, not under lexicographic comparison. Presented
with the choice, the owner selected "reformulate the rule, take the
tie-break". Recorded explicitly because a subsequent run mistook this
amendment for an agent rewriting doctrine to legalise its own change and
reverted the approved work — it is not; the authorisation is the owner's.

**Decision: V16 bends are now the final lexicographic key of phase 4.5's
acceptance objective**, strictly after `(v13, v12, v5)`, with the
existing `v11` / `overlap` / `v12` hard guards unchanged. The revised
V16 rule and the proof that last-place lexicographic ordering makes the
subordination structural (rather than a matter of coefficients) live in
`docs/invariants.md` V16 — including the two permitted shapes
(non-regression guard, or final objective key) and the requirement that
the quantity be the **ink-graph** bend count, never a raw segment or
corner count.

Two mechanical consequences were decided with it:

- `joint_search`'s early exit no longer fires on
  `(V13, V12, V5) == 0`; it now requires bends to be zero too. The old
  exit returned the lexicographically first zero-cost combination and
  hid every equally-clean but straighter alternative behind it. The
  enumeration is already hard-capped by `MAX_COMBINATIONS`, so the
  worst case is unchanged — only the typical case does more trial
  routing.
- Router → placement coupling increases, since the bend key joins the V5
  key in reading real router output. This protocol is what governs it:
  a router-only change must still produce an empty `baseline_lock` diff,
  and any regeneration must show V16 (B, J) non-increasing per fixture.

Measured effect when it landed: `rc_lowpass_ports` B 4 → 2 and
`common_emitter` B 10 → 4, with V5, V11, V12, V13, overlap and crossing
counts unmoved on every fixture.

---

## ADR-17 — Deterministic constructive placement with router-verified local repair

**Status: RETIRED — owner decision, after Stage 2 was KILLED. Four
parts salvaged; see "RETIRED" immediately below.** The ADR is kept, not
deleted: its diagnosis of the coupling between the ratchet regime and a
global optimizer, and the kill record that falsified its own central
claim, are the durable value. Nothing below the RETIRED section is a
plan any more — read it as a record.

*(Historical status, for context: proposed / owner-approved, staged,
with per-stage kill criteria and per-stage escape-list sign-off.)*

**Supersedes** ADR-15's "SA as polisher" *end-state* — a supersession
that lapses with this retirement: ADR-15's as-built disposition stands.

---

### RETIRED — why, and what was salvaged

#### Why retired

**1. The SA was never the blast-radius culprit.** The control this ADR
never ran is the **bare deterministic seed** — `--no-refine`, so no SA
and no compaction at all. Measured on master (`56b1ab5`):

| P11 case | SA (master) | seed + compaction (Stage 2) | **bare seed** |
| -------- | ----------- | --------------------------- | ------------- |
| `rc_lowpass` + 1 R | 5 / 5 | 5 / 5 | **5 / 5** |
| `common_emitter` + 1 C | 17 / 17 | 16 / 17 | **17 / 17** |

Removing the optimizer entirely changes nothing. Global re-basing is
**intrinsic to any spacing-derived placement**: classify→bands→layers
derives its strides from global structure, so one insertion re-spaces
that element's column and every coordinate derived after it. The RNG was
never the mechanism.

> **Determinism is not locality.**

This ADR's headline — "the redesign's primary product is that a change's
diff becomes attributable to the change" — therefore does not follow
from its proposed mechanism. Stage 2 measured the first half of that
(deterministic compaction: no better); the seed arm closes it.

**2. The SA is not doing compaction; it is basin-finding.** Conclusion
(b) above ("where it does act, its entire measurable value is
COMPACTION") **contradicts the table printed directly above it**, which
shows the SA improving the V16 bend count `B` on three fixtures:
`common_emitter` 11 → 4, `named_rails` 4 → 2, `opamp_inverting` 5 → 3.
Bends are not wire length. The corrected picture:

- **inert on 4 of 10** fixtures (`diff_pair`, `multivibrator`,
  `opamp_definition_level`, `port_shapes` — byte-identical output);
- **harmful on `rc_lowpass`** (B 3 vs the seed's 2, WL 17.8 vs 11.4),
  on the `OPAMP` child sheet (X 2 vs 1), and on
  `opamp_inverting_real` (B 8 vs 6 — a case conclusion (c) missed);
- **load-bearing on the 3 complex fixtures** above, where it finds a
  materially better basin, not merely a tighter one.

An optimizer that is inert on 40% of inputs still stands as a criticism.
"Its entire measurable value is compaction" does not, and Stage 2 was
designed against that mis-statement.

**3. Attributability is already solved where it matters.** Re-converting
an *edited netlist* through the ADR-4 layout cache moves **0 of 8** user
symbols on `common_emitter`+CB and **0 of 2** on `rc_lowpass`+R2/C2,
with both conversions passing the CLI's post-emit connectivity check.
(Four of `common_emitter`'s nine power glyphs report as "moved" only
because glyph refdes are assigned in emission order and one insertion
renumbers every later glyph — **all nine geometries are identical**:
identity shifted, geometry did not. `rc_lowpass` genuinely relocates two
glyph poses, but that is *decoration* re-anchoring around a new
neighbour, downstream of placement.) So:

- **users editing netlists already have attributable diffs** — the cache
  delivers exactly the property this ADR promised;
- **developers changing placer code can never have per-fixture locality
  from any spacing-derived algorithm** — that workflow is governed by
  ADR-16's two-instrument protocol, not by an algorithm change.

This is now pinned by the reformulated **P11 — cache-path stability**
(`placement_stability.rs`), which replaces the deleted basin-locality
P11. See salvage item 3.

#### What is salvaged

| # | Item | Status |
| - | ---- | ------ |
| 1 | **Phase-4.5 Tier-0 connectivity guard.** Phase 4.5 can accept an orientation that severs a net (found during Stage 2, latent on master). Landing as a **hard guard**, not a term in the acceptance objective. | landing separately, on its own merits |
| 2 | **Complete ADR-14's decoration reservation** — labels, Reference/Value text, PWR_FLAG bodies — as an **ADR-14 completion, NOT an "ADR-17 Stage 4"**. It is the identified cause of **4 of Stage 2's 7 breaches** and remains the standing plan for the `opamp_inverting_real` glyph residual. | in progress, re-parented to ADR-14 |
| 3 | **The Stage-1 verifiers** (F5/P4, P5, P10, P11), with **P11 reformulated** as cache-path stability. The old basin-locality P11 is deleted: budget-0 against a measured 5/17 was a target no architecture can reach, and leaving it `#[ignore]`d was worse than deleting it. F5/P4, P5 and P10 are unchanged. | LANDED |
| 4 | **Hand `*@place` / `*@align` annotations** for the two flow defects (`common_emitter`'s vertical `COUT`; `rc_lowpass_ports`' co-located `in`/`out`). The **zero-annotation** flow aspiration returns to v0.2, with its walls documented (MEMORY "flow-orientation wall"; ADR-15 Stage-5 post-mortem). | queued |

#### Durable findings that outlive the retirement

- **X spacing is slack; Y spacing is meaning.** The X-layer stride is a
  flow-depth ordering with a generous constant floor, so closing it costs
  nothing. The Y bands are V6's *semantic* structure (Top rail / Mid
  signal / Bot ground); squeezing them is order-preserving and **still
  wrong** — it collapses the signal band onto the rails and the router
  pays in bends. Four variants measured, not tuned (`common_emitter` B):
  X+Y squeeze **7**, X-squeeze + Y-snap **11**, least-disturbance
  tie-break **13**, X-only **6**.
- **Stage 4 is a precondition for any compaction attempt, not its
  successor.** Compaction reclaims exactly the space decoration was
  going to put labels and property text in, so it is structurally unable
  to be safe until the decoration reservation is complete. Any future
  attempt at spacing changes must complete salvage item 2 first.
- **Determinism is not locality** (above). Any future attempt must make
  locality an explicit design property with an acceptance test, not a
  hoped-for consequence of removing the RNG.

---

It also **reverses ADR-15's decision to leave the pose-assignment
mechanism with the annealer.** ADR-15 named the right mechanism —
"constructive assignment + the `pinned` mask, not cost terms" — and then
did not build it: its Stage 3 is recorded NOT LANDED, and its own
corrections section records that idiom 4 "does not pin, and does not set
the stub's side … the mask half of the decision is unbuilt". Every pose
decision therefore still resolves inside the SA. ADR-17 moves that
decision out of the optimizer and into a deterministic construction.
(Note for the record: this is a reversal of ADR-15's *as-built
disposition*, which its corrections section states plainly; ADR-15 never
used the phrase "discrete assignment solver" and no such component was
ever specified or deferred by name.)

---

### Diagnosis — the two blocked defect classes are one defect

Two things have been stuck for several sessions:

1. **Flow orientation.** `common_emitter`'s `COUT` is drawn vertical
   where it should be horizontal; `rc_lowpass_ports` emits its `in` and
   `out` global labels at **identical x = 41.91**, input below and
   output above, so the sheet shows no left→right flow at all. (Both
   verified against a fresh cache-less conversion at `6c28b72`.)
2. **The V14 glyph residual** on `opamp_inverting_real` — see the
   correction below for what it actually is.

They are not two problems. They are one architectural property:

> **A constraint cannot be added to the SA without global,
> unattributable consequences.**

The codebase already documents itself fighting this, in five places
that were each written as a local workaround:

- `anneal.rs`'s **RNG-stream-preservation machinery** (`:196`, `:847`,
  both commented "the RNG stream stays byte-identical to the pre-…")
  — deliberate care to keep the random stream identical so unrelated
  fixtures don't move.
- `anneal.rs`'s **`gates_active` switch** (`:101`), which switches the
  V11 gate off entirely when `mirror_eligible` is empty, so the gate
  does not perturb all-passive fixtures it has nothing to say about.
- **`mirror_eligible` scoping** (`anneal.rs:88`), narrowing which
  elements the mirror move may touch.
- **ADR-14's glyph reservation, shipped deliberately incomplete** — the
  reservation is hard only for oversized-involving pairs in the SA gate
  and X-only at the seed stride, because widening it "risks reshuffling
  layouts".
- **Legalization moved from before the SA to after it** (`lib.rs`, the
  long comment above `legalize_if_needed`), for exactly this reason and
  stated in exactly these terms: legalizing the *seed* "perturb[ed] the
  SA's starting point, sending it down a different trajectory", which on
  `common_emitter` cost a **Tier-0 V11 short** plus three Tier-1
  invariants "in exchange for one Tier-2 crossing". The fix was not to
  make legalization better — it was to move it somewhere the SA could
  not amplify it. That is the clearest statement in the tree of the
  problem this ADR names, and it was written as a local workaround.

Every hard constraint that has landed needed a bespoke blast-radius
hack. The two that could not be contained are exactly the two that are
stuck. That is not a coincidence; it is the pattern.

**The measurement.** P11 (`placement_stability.rs`, ADR-17 Stage 1)
states the property as a number: adding **one** bypass capacitor to
`common_emitter` moves **17 of 17** pre-existing symbols, power glyphs
included. Adding one series resistor to `rc_lowpass` moves 5 of 5. The
"basin" is the whole page.

> **AMENDMENT (retirement).** The number is right; the attribution is
> wrong. The bare deterministic seed — no SA, no compaction — scores the
> *same* 17/17 and 5/5. The blast radius is a property of
> spacing-derived placement, not of Metropolis acceptance. See the
> RETIRED section.

### Governance consequence

Zero-slack per-fixture ratchets (CLAUDE.md § "Budgets are ratchets, not
knobs") assume changes have **local, attributable effects**. Metropolis
acceptance over a shared RNG stream **guarantees they do not**. The
ratchet regime and a chaotic optimizer are, jointly, a
change-prevention machine — which is precisely the "local-optimum
freeze" CLAUDE.md already documents under the global-improvement
escape.

Neither half is wrong on its own. The ratchets are correct policy; the
SA is a reasonable optimizer. The combination is what blocks work.

**The redesign's primary product is therefore not a prettier layout. It
is that a change's diff becomes attributable to the change.** Every
other benefit is downstream of that.

### Correction — the recorded V14 residual is stale

The record (CLAUDE.md, ADR-14, and the budget comment at
`crates/spice2kicad/tests/placement_quality.rs`
`power_glyph_foreign_body_overlap_budget`) says the `opamp_inverting_real`
residual is *"a `power:PWR_FLAG` driver marker (`#FLG3` at
(30.48, 44.45), the VEE rail flag) clipping `RIN`"*.

**That no longer exists in emitted output.** Measured at `6c28b72` on a
fresh cache-less conversion: `#FLG3` is at **(59.69, 77.47)** and
overlaps nothing. The live residual is:

> `#PWR1` (`power:GND`, at `(46.99, 41.91)` rot 0, bbox
> `45.72 .. 48.26 × 41.91 .. 44.45`) overlapping the body of **`RF`**
> (`47.244 .. 49.276 × 41.910 .. 46.990`), with host `X1`.

A **GND glyph on the oversized opamp's grounded `+` input pin, clipping
the feedback resistor.** Still one overlap, still on the same fixture,
still Tier 1 — but a different pair, a different glyph *kind*
(`power:GND`, not `PWR_FLAG`), and a different victim. The stale comment
has been refreshed in the same commit as this ADR's Stage 1.

The class description in ADR-14 ("anchored on the oversized opamp
triangle, sheet-port-flavoured") survives the correction; the specific
coordinates and refdes did not.

### The SA ablation

All ten fixtures converted twice, once normally and once with the
stage-3 force-directed + simulated-annealing refinement disabled, and
diffed. The flag is **`--no-refine`** (`crates/spice2kicad/src/main.rs`,
`refine: !cli.no_refine`) — note it disables the FR seed + SA, *not*
phase 4.5, which lives in `kicad-emitter` and still runs in both arms.

> **CORRECTION (Stage 2) — the sentence above is WRONG, and so is every
> "off" number in the table below.** `main.rs` gated the phase-4.5 call on
> `if opts.refine`, so `--no-refine` ablated *both* passes. With phase 4.5
> restored to both arms the seed scores far better than recorded
> (`common_emitter` B 11 → 4, WL 121.9 → 59.7 mm), and conclusion (b) does
> not survive. Do not reuse this table to size any future stage; see
> "Stage 2 — KILLED" below for the corrected measurement.

Reproduce with:

```sh
L="--lib crates/kicad-symbols/tests/fixtures/Device.kicad_sym \
   --lib crates/kicad-symbols/tests/fixtures/Simulation_SPICE.kicad_sym \
   --lib crates/kicad-symbols/tests/fixtures/Amplifier_Operational.kicad_sym \
   --lib crates/kicad-symbols/tests/fixtures/power.kicad_sym"
for f in crates/spice2kicad/tests/fixtures/*.cir; do
  n=$(basename "$f" .cir)
  bash -c "ulimit -v 4194304 && cargo run -q -p spice2kicad -- $f            -o /tmp/sa_on/$n.kicad_sch  $L"
  bash -c "ulimit -v 4194304 && cargo run -q -p spice2kicad -- $f --no-refine -o /tmp/sa_off/$n.kicad_sch $L"
done
```

(Fresh output directories are load-bearing: re-converting into a used
directory pins every element via the layout cache.)

Poses moved = non-power symbols whose `(x, y, rot, mirror)` differs
between the two arms. `B` is the V16 ink-graph bend count
(`wire_geometry.rs`); `X` is the inter-net crossing count; `WL` is total
wire length in mm over maximal runs.

| Fixture | poses moved | B on/off | X on/off | WL mm on/off |
| ------- | ----------- | -------- | -------- | ------------ |
| `common_emitter` | 8/8 | 4 / 11 | 0 / 0 | 95.3 / 121.9 |
| `diff_pair` | 0/5 | 2 / 2 | 0 / 0 | 24.1 / 24.1 |
| `multivibrator` | 0/8 | 10 / 10 | 4 / 4 | 184.2 / 184.2 |
| `named_rails` | 4/4 | 2 / 4 | 0 / 0 | 22.9 / 35.6 |
| `opamp_definition_level` | 0/6 | 12 / 12 | 6 / 6 | 152.4 / 152.4 |
| `opamp_inverting` | 2/2 | 3 / 5 | 0 / 0 | 33.0 / 86.4 |
| `opamp_inverting_real` | 3/3 | 8 / 6 | 0 / 0 | 54.6 / 88.9 |
| `port_shapes` | 0/4 | 4 / 4 | 0 / 0 | 64.8 / 64.8 |
| `rc_lowpass` | 2/2 | 3 / 2 | 0 / 0 | 17.8 / 11.4 |
| `rc_lowpass_ports` | 2/2 | 2 / 2 | 0 / 0 | 8.9 / 11.4 |

(The SA-on `B` column reproduces `BEND_BRANCH_BUDGETS` in
`wire_geometry.rs` exactly on all nine ratcheted fixtures, which
validates the measurement.)

The table covers root sheets only. `opamp_inverting`'s hierarchical
child sheet `OPAMP.kicad_sch` behaves the same way and is worth
recording separately: 1/1 pose moved, B 5/5, WL 104.1 / 146.0 — and
X **2 with the SA on versus 1 with it off**. On that sheet the SA
*introduces* a crossing while shortening wire, a second instance of
conclusion (c) below.

**Three conclusions:**

**(a) On FOUR of ten fixtures the SA moves nothing at all** —
`diff_pair`, `multivibrator`, `opamp_definition_level`, `port_shapes`.
Every ink metric is bit-identical with it switched off.

**(b) Where it does act, its entire measurable value is COMPACTION.**
It fixes **zero** crossings on all ten fixtures — `X` is identical in
both arms everywhere, including the two fixtures that have crossings to
fix. What it does is shorten wire: 121.9 → 95.3 mm, 86.4 → 33.0,
88.9 → 54.6. That is worth having, and it is *deterministic work being
done by a stochastic process*.

**(c) On `rc_lowpass` the SA's output is WORSE than the raw seed** —
B 3 vs 2 and WL 17.8 vs 11.4 mm — and on the `OPAMP` child sheet it
adds a crossing (X 1 → 2). The polish can anti-polish. A
~200-iteration Metropolis walk with no acceptance gate on the output
metrics has no obligation to end better than it started, and on one
fixture in ten it doesn't.

An optimizer that is inert on 40% of inputs, fixes none of the defect
class it is credited with, actively regresses 10%, and costs global
attributability on 100% is not earning its coupling.

### The design

The pipeline becomes:

1. **classify** — net classes, as today.
2. **flow graph** — BFS depth from the input-terminal nets. The feedback
   edges that `break_cycles` (`layers.rs:258`) identifies are carried
   through **marked exempt**, rather than silently *reversed* as they are
   today: a feedback resistor must be excused from the flow ordering, not
   asserted to run backwards. When a netlist declares no input net, fall
   back to the existing layering (`layers.rs::no_source_fallback`).
3. **role assignment** — ADR-15's **anchor / series / rail-stub /
   terminal** roles. ADR-15 Stage 5 **VALIDATED** this: the structural
   discriminator worked perfectly, putting `COUT` horizontal and the
   bypass `CE` vertical **from pin roles alone**, with no element-kind
   or refdes test. The role model is not in question; only what consumes
   it is.
4. **columns, with COMPLETE decoration reservation** — every column
   reserves the space its decoration will occupy: power-glyph body *and*
   value text, labels, stubs. ADR-14 reserved a subset; this reserves
   the rest.
5. **net lines** — the row/column tracks nets will run on.
6. **joint pose assignment** — **position, orientation AND mirror
   emitted together from ONE datum: the element's flow position.**
7. **user `align` / `place` + V7 symmetry + sidecar** — applied over the
   construction, as today.
8. **deterministic order-preserving compaction** — the SA's one real
   job, done deterministically and without reordering.
9. **legalize** — as today.
10. **generalized router-verified repair** — phase 4.5, promoted (see
    below).
11. **decoration** — unchanged, still a strict consumer.

**The key rationale, and the reason this is not just another optimizer.**
Orientation and position are coupled — that is what ADR-15 Stage 5
proved when filtering one element's orientation set moved every element
on the sheet. But they are coupled *through the optimizer*. The naive
reading of Stage 5's finding is "we need a joint optimizer over position
and orientation", which is a **bigger SA** and makes every problem above
worse. The correct reading is the opposite:

> Emit position, orientation and mirror from **one deterministic
> construction** over the same structural facts, so **there is no
> optimizer for the coupling to flow through.**

Separability was never the question. The question is whether the
coupling is resolved by *search* (where it propagates globally and
unattributably) or by *construction* (where it is resolved once, locally,
by a rule you can read).

**Why sequential phases are still safe here.** The obvious objection is
that steps 7–10 are exactly the "later phase undoes earlier phase"
failure ADR-15 Stage 5 hit. They are not, because every phase after step
6 is **LOCAL and MONOTONE**: compaction preserves order and only removes
slack; legalization only separates overlapping pairs; repair touches
only measured offenders and only within a bounded neighbourhood. None of
them re-bases. That is the property being bought, and Stage 3's kill
criterion is what tests whether it was actually bought.

**Phase 4.5 survives and is PROMOTED.** ADR-11's core insight is
untouched and confirmed by everything above: a V5 violation is born in
the router's conflict-resolution passes and is invisible to any
placement-side cost, so the real router must remain the oracle. What
changes is scope. Its current contract — *"orientation only, never
position"* — relaxes to:

> **bounded local pose repair, offenders only, before decoration.**

Concretely: orientation candidates as today, plus slot shifts of at most
±2 cells, applied only to elements the router reports as offenders,
still gated by no V11 / V12 / V13 / overlap / V16 regression, still
strictly before decoration begins. Decoration's contract (a strict
consumer that never feeds position back) is unchanged.

**CLAUDE.md's layout-phase list will need amending when Stage 5 lands**
— phase 4.5's "changes element *orientation* only, never position"
sentence becomes the bounded-local-pose-repair wording above. That
amendment is **not made now**; it is made by the Stage 5 commit, if
Stage 5 lands.

### Honesty check

This must be recorded, because the redesign is large and the honest
case for it is narrower than its size suggests.

**Both Item-1 defects could be fixed TODAY, at zero risk, with hand
`*@place` / `*@align` annotations.** That is the annotation spec's
designed escape hatch (CLAUDE.md principle 9: "The escape hatch when
heuristics fail is `*@place` / `*@align` — already in v0.1"). A user who
hits either defect has a working, supported answer right now.

The redesign is therefore justified by exactly two things, and nothing
else:

- the **zero-annotation quality bar**. Note this is an aspiration, not
  a stated contract: CLAUDE.md principle 2 sets the floor at "a valid
  (if ugly) schematic", which today's output already clears. The case
  for ADR-17 is that "ugly but valid" is the right floor for v0.1 and
  the wrong ceiling for the project — an unannotated netlist should
  read as a circuit diagram. Anyone weighing this ADR's cost should
  weigh it against *that*, not against a rule it violates; and
- **unblocking all future Tier-2 readability work**, which is currently
  gated behind the attributability problem rather than behind any
  individual rule.

**Item 2 (the glyph residual) does NOT justify the redesign.** It rides
along because Stage 4's complete decoration reservation is likely to
clear it for free. It has a narrow decoration-side plan B, and under the
Stage-4 kill criterion it is never allowed to hold Item 1 hostage.

### What ADR-15 got wrong

Recorded by ADR-15's own author, re-reading it against the measurements
above. Three of its judgements were wrong:

1. **"Demote the SA to a polisher" was wrong AS AN END-STATE.** A
   polisher that re-basins globally under any constraint change is not a
   polisher, it is a **coupling amplifier** — it converts every local
   edit into a global diff. And the ablation shows what the polishing
   actually is: compaction. Deterministic work misassigned to a
   stochastic process. (ADR-15's *diagnosis* of the cost function was
   correct and stands; only the prescription was wrong.)

2. **Stage 5 was MIS-DESIGNED, not unlucky.** ADR-15's Stage-5
   post-mortem reads the failure as "the flow proxy and measured routing
   quality genuinely disagree". That is true but secondary. The design
   error is upstream: **filtering the orientation candidate set treats
   orientation as separable from position.** Finding 2 of that same
   post-mortem — the global SA basin shift, where every element moved —
   **disproves the separability the mechanism assumed.** The post-mortem
   recorded the disproof and did not draw the conclusion.

3. **The headline consequence was only HALF-DELIVERED.** ADR-15 states
   the mechanism as "constructive assignment + the `pinned` mask, not
   cost terms". As built, `idioms.rs::apply_rail_stub_columns` takes
   `pinned` **immutably**, mutates **X only**, and leaves the stub's
   **side** to the soft `cost::rail_direction` term — because pinning
   the side would have perturbed the SA elsewhere. The `pinned` half,
   the half that carries the hardness guarantee, was never built. Note
   the reason: *it was blocked by the blast-radius problem this ADR
   exists to fix.*

**What ADR-15 got RIGHT, and ADR-17 keeps unchanged:**

- **the role model** (anchor / series / rail-stub / terminal, derived
  from pin counts, pin angles and net classes only) — validated by
  Stage 5's discriminator and carried forward intact;
- **verifiers before intervention** — measure first, then change; this
  ADR's Stage 1 is that rule applied to itself;
- **pinning as the only trivially-consistent hard mechanism** — the
  observation that a hard constraint must be hard at *every* stage that
  can move an element, and that a mask is the only cheap way to achieve
  that.

### Staging

| Stage | Content | Status |
| ----- | ------- | ------ |
| 0 | This ADR; owner sign-off | **LANDED** (now RETIRED) |
| 1 | Verifiers (F5/P4, P5, P10, P11); no behaviour change | **LANDED** — kept; P11 reformulated (salvage 3) |
| 2 | Deterministic order-preserving compaction; SA retirement | **KILLED** — see the Stage-2 outcome below |
| 3 | Joint flow-pose construction (position + orientation + mirror) | **ABANDONED** — flow handled by annotations (salvage 4) |
| 4 | Complete decoration reservation | **RE-PARENTED to ADR-14** (salvage 2) |
| 5 | Generalized router-verified local repair (phase 4.5 promoted) | **ABANDONED** — only the Tier-0 connectivity guard survives (salvage 1) |
| 6 | Consolidation; delete the superseded machinery | **ABANDONED** — the SA stays |

### Kill criteria

Stated as **measurements, not judgement calls**. Each is evaluated at
the end of its stage, against the fixture suite.

- **Stage 2 kill.** If deterministic compaction cannot reach `≤` the
  current `(WL, B)` **per fixture** using **at most TWO** escape
  requests, or if **any** Tier-0 or Tier-1 count rises — the SA stays,
  and the redesign **halts**. Record the outcome as: *"the wall's true
  name is the SA, and we chose to keep it."*

- **Stage 3 kill.** If `Σ(V12 + V13)` summed across all fixtures is
  `> 0` after repair, the **positional hypothesis is FALSIFIED** —
  revert Stage 3, keep the Stage-1 verifiers at their newly measured
  floors, and **close the flow question permanently**. Do **not** iterate
  past **two** falsifying rounds; a third round is tuning, not
  hypothesis-testing.

- **Stage 4 kill.** If completing the decoration reservation cannot
  clear the glyph overlap, fall back to the decoration-side plan B for
  **Item 2 only**. Item 2 must not hold Item 1 hostage, and Stage 4
  failing does not stop Stages 5–6.

- **Stage 5 kill.** If the generalized repair exceeds `MAX_COMBINATIONS`
  or oscillates (a pose repaired and re-repaired across rounds), keep
  **orientation-only** repair and ship Stage 5 reduced to today's
  phase 4.5 scope.

### Owner decisions

- **Building ADR-17 is APPROVED.**
- **The Stage-3 doctrine amendment — a within-Tier-2 precedence order of
  F5 flow > V5 pin-facing > V16 bends — is DEFERRED to Stage 3 by the
  owner. It is NOT approved.** It is recorded here as an **open decision
  that Stage 3 must obtain sign-off for before relying on it.** Stage 3
  may not assume it; if Stage 3 needs it, Stage 3 asks.
- **Escape lists for Stages 2, 3 and 5 are brought for sign-off in
  advance, per stage.** A stage may not discover its escapes while
  landing.

### Stage 2 — KILLED, and the ablation that motivated it was invalid

**Outcome, in the words the kill criterion prescribes: *the wall's true
name is the SA, and we chose to keep it.***

The attempt is preserved as tag `archive/adr17-stage2-killed`
(commit `45804dc`). Master is unchanged and green with the SA intact.

#### The invalidating discovery — `--no-refine` ablated TWO passes

This section's ablation states that `--no-refine` disables the FR seed +
SA, "*not* phase 4.5, which lives in `kicad-emitter` and still runs in
both arms". **That was false when it was written.** `main.rs` guarded the
call:

```rust
if opts.refine {
    kicad_emitter::refine_orientations(&mut placement, &library, &refine_meta);
}
```

So every "SA off" number in the table above is *seed without phase 4.5*.
The two passes were conflated, and the conclusions drawn from the
difference were attributed entirely to the SA.

Re-measured with phase 4.5 restored to both arms, the seed alone scores
**far better** than recorded — `common_emitter` B 11 → 4 and total wire
121.9 → 59.7 mm. Conclusion (b), "where it does act, its entire
measurable value is COMPACTION", does not survive: most of what the
ablation attributed to the SA was phase 4.5's orientation refinement
being switched off alongside it.

**This does not rescue the seed**, because the seed's own output is
*Tier-0 broken* — see below. But it does mean Stage 2 was designed
against a mis-measurement, and any future stage must re-derive its
targets from a corrected ablation.

> **AMENDMENT (ADR-17 retirement) — the "corrected" numbers in the two
> paragraphs above are themselves a measurement artifact.** The seed
> figures `common_emitter` B 11 → 4 and WL 121.9 → 59.7 mm were measured
> on an **electrically broken file**: unguarded phase 4.5 rotates `COUT`
> to 180, the router's conflict cascade drops the severed branch, and the
> wire total is low precisely *because a wire is missing*. That is the
> same defect diagnosed one section below. The honest seed number, with
> the connectivity guard in place, is **B 11 / WL 127.0 mm** — i.e.
> within noise of the original "invalid" ablation figure (B 11 /
> WL 121.9), and it is what the Stage-2 measurement table below already
> records for the "seed only" arm. So the ablation's *conclusion (b)*
> still falls (see the RETIRED section: it is contradicted by its own
> bend column, not by phase 4.5), but the phase-4.5 conflation did **not**
> materially change the seed's score. The original numbers are left
> above, unrewritten, because the sequence of two artifacts is the
> lesson.

#### A second Tier-0 defect found on the way: phase 4.5 can sever a net

Phase 4.5's acceptance predicate (`refine.rs`, `Measure`) scores
`(v13, v12, v5, bends)` and guards `v11` / `overlap` / `v12`. It has
**no connectivity term**, so it will accept an orientation that
disconnects the circuit.

Measured on `common_emitter`: the seed layout puts `COUT` where rotating
it to 180 improves every metric phase 4.5 can see, while boxing its `c`
pin between a foreign pin (V11 blocks one L-route) and `Q1`'s body (V12
blocks the other). The router's conflict cascade exhausts its detours and
drops the branch; the CLI's post-emit connectivity check then refuses the
file:

```
ERROR: net "c" is not fully connected in the emitted schematic
  net in the source but split in the schematic: {"COUT.0", "Q1.0", "RC.1"}
```

The branch adds a `severed` count to `TrialRoute` / `Measure` (union-find
over emitted endpoints, KiCad's endpoint-only join rule, signal nets
only) as a pure non-regression guard. It closed the break. **This defect
is independent of ADR-17 and outlives its kill** — it is latent on master
today, masked only because the SA happens to produce layouts where the
tempting orientation is not available. It should be fixed on its own
merits, in its own commit, with its own baseline check.

#### What was built

`crates/spice-layout/src/compact.rs` — order-preserving column
compaction, run where the SA ran (after seeding/idioms, before
`legalize`), plus `--sa-refine` replacing `--no-refine` so the annealer
stays reachable. Properties as specified: no RNG, columns never reorder,
every move is `min(original, tightest_legal)` so it only removes slack,
spacing derived from `world_extent_with_glyphs` (ADR-14's reservation
used as-is, not widened), pinned lines frozen. Shared-net pin-alignment
snapping is folded into the same choice: among candidates in
`[tightest, current]`, take the one aligning the most shared-net pins,
tightest breaking the tie.

Every candidate is additionally gated on a **measured** V11 count.
Without it, compaction merged `common_emitter`'s `c`, `e` and emitter
bypass into one net — a silent short, exactly the failure
`legalize::shove_one` already guards against.

#### Four design variants were measured, not tuned

| Variant | `common_emitter` B | `opamp_inverting_real` B/J |
| ------- | ------------------ | -------------------------- |
| squeeze X and Y | 7 | 7 / 1 |
| squeeze X, snap-only Y | 11 | 6 / 0 |
| least-disturbance tie-break | 13 | 6 / 0 |
| **squeeze X only** (kept) | **6** | **6 / 0** |

The one durable design finding: **X spacing is slack; Y spacing is
meaning.** The X-layer stride is a flow-depth ordering with a generous
constant floor, so closing it costs nothing. The Y bands are V6's
*semantic* structure (Top rail / Mid signal / Bot ground), and squeezing
them is order-preserving and still wrong — it collapses the signal band
onto the rails and the router pays in bends. Recorded so a future stage
does not re-derive it.

#### The measurement

All arms include phase 4.5 and the connectivity guard, so they are
comparable. `B / J / X / WL(mm)`, root sheets plus the `OPAMP` child.

| fixture | SA on (master) | seed only | seed + compaction |
| ------- | -------------- | --------- | ----------------- |
| `rc_lowpass` | 3 / 0 / 0 / 17.8 | 2 / 0 / 0 / 11.4 | 2 / 0 / 0 / 11.4 |
| `common_emitter` | 4 / 3 / 0 / 95.2 | 11 / 1 / 0 / 127.0 | 6 / 2 / 0 / 67.3 |
| `multivibrator` | 10 / 2 / 4 / 184.2 | 10 / 2 / 4 / 184.2 | 10 / 2 / 4 / 184.2 |
| `diff_pair` | 2 / 1 / 0 / 24.1 | 2 / 1 / 0 / 24.1 | 2 / 1 / 0 / 24.1 |
| `opamp_inverting_real` | 8 / 0 / 0 / 54.6 | 6 / 0 / 0 / 88.9 | 6 / 0 / 0 / 78.7 |
| `opamp_inverting` | 3 / 0 / 0 / 33.0 | 5 / 1 / 0 / 86.4 | 2 / 1 / 1 / 47.0 |
| `port_shapes` | 4 / 0 / 0 / 64.8 | 4 / 0 / 0 / 64.8 | 4 / 0 / 0 / 64.8 |
| `rc_lowpass_ports` | 2 / 0 / 0 / 8.9 | 2 / 0 / 0 / 11.4 | 2 / 0 / 0 / 11.4 |
| `opamp_definition_level` | 12 / 0 / 6 / 152.4 | 12 / 0 / 6 / 152.4 | 12 / 0 / 6 / 152.4 |
| `named_rails` | 2 / 2 / 0 / 22.9 | 2 / 1 / 0 / 26.7 | 2 / 1 / 0 / 26.7 |
| `OPAMP` (child) | 5 / 0 / 2 / 104.1 | 5 / 0 / 1 / 146.1 | 5 / 0 / 1 / 146.1 |

Compaction is doing real, visible work — `common_emitter` B 11 → 6 and
wire 127.0 → 67.3 mm against the bare seed, and it beats the SA outright
on `rc_lowpass`, `opamp_inverting_real` and `opamp_inverting`. It is not
a failure of *effect*.

#### Why it was killed

The criterion is "≤ current on every ratchet, at most TWO escapes, and no
Tier-0/Tier-1 rise anywhere". Measured breaches:

- **V16** (2): `common_emitter` B 4 → 6; `opamp_inverting` J 0 → 1.
  (Four fixtures *improved* and would have ratcheted down: `rc_lowpass`
  3 → 2, `common_emitter` J 3 → 2, `opamp_inverting_real` 8 → 6,
  `opamp_inverting` B 3 → 2.)
- **`electrical_safety`** (5 of 25): `v13_labels_dont_overlap_symbol_body`,
  `v13_labels_clear_pin_text`, `v13_labels_no_mutual_overlap`,
  `item3_interface_global_labels_clear_foreign_bodies`,
  `v5_first_segment_extends_outward`. All Tier-1 except V5, all budget 0.
- **`flow_geometry`** (2 of 3): `series_pose_and_terminal_order_within_ratchet`
  (F5) and `series_discriminator_separates_stub_from_series_on_common_emitter`.
- `baseline_lock`: 50 differences (legitimate for a layout change, and
  ADR-16's regeneration was never reached).

Seven ratchet breaches against a budget of two, four of them Tier-1
label-overlap invariants at budget 0. `placement_quality`,
`visual_quality` and `placement_stability` were not run to completion —
the criterion had already been met several times over and continuing
would have been tuning, which the stage brief forbids.

The Tier-1 V13 cluster is the informative one: compaction pulls symbols
together, and the space it reclaims is exactly the space **decoration**
was going to put labels and property text in. ADR-14's reservation covers
power-glyph body and value text and is documented as incomplete; nothing
reserves label or Reference/Value text. So compaction is structurally
unable to be safe *until Stage 4's complete decoration reservation
exists*. **The staging order is wrong: Stage 4 is a precondition for
Stage 2, not a successor.**

#### P11 — the headline claim did not survive contact

The redesign's stated primary product is attributability. Measured on the
branch:

| P11 case | master (SA) | seed + compaction |
| -------- | ----------- | ----------------- |
| `rc_lowpass_plus_r` | 5 / 5 | 5 / 5 |
| `common_emitter_plus_c` | 17 / 17 | **16** / 17 |

**Essentially unchanged.** This falsifies the module's own locality
claim, and the reason is structural rather than incidental: a
left-to-right sequential compaction sweep settles each column against the
ones already settled, so inserting an element anywhere changes one
column's occupancy and that change propagates through *every* column to
its right. Order-preserving 1-D compaction is monotone and deterministic
but it is **not local** in P11's sense.

That matters well beyond Stage 2. ADR-17 argues the blast radius is the
SA's fault and that determinism cures it. On this evidence determinism is
**not sufficient** — a deterministic sequential sweep re-bases the page
just as thoroughly. Any future attempt must make locality an explicit
design property with P11 as its acceptance test, not a hoped-for
consequence of removing the RNG.

#### What it would take

1. **Stage 4 first.** Complete decoration reservation — labels, property
   text, Reference/Value — folded into the extent compaction measures.
   Four of the seven breaches are decoration collisions in space
   compaction legitimately believed was free.
2. **Fix phase 4.5's connectivity gap independently** (above). Any
   non-SA layout is one bad orientation away from a severed net today.
   **DONE** — `Measure::severed`, a hard non-regression guard beside
   `v11`/`overlap`/`v12`, never a key of the lexicographic objective
   (connectivity is Tier 0: a categorical floor, not a gradient). The
   demonstrated `COUT` rot-180 candidate is pinned by a regression test
   that measures it directly and asserts the guard alone rejects it.
   `baseline_lock` diff EMPTY, every ratchet unchanged, as predicted.

3. **Re-derive Stage 2's targets from a corrected ablation.** The
   numbers in this ADR's ablation table cannot be used; they measure two
   passes and attribute the result to one.
4. **Treat locality as a first-class requirement**, designed for and
   measured by P11, not assumed from determinism.

Until then the SA stays. It is inert on four fixtures, fixes no
crossings, and costs global attributability — every criticism in this ADR
stands — but it is currently the only thing producing a layout that
clears all seven ratchet families at once, and on `common_emitter` it is
load-bearing for Tier-0 correctness.

### Stage 1 — what landed, and what it measured

Four verifiers, all at today's measured (defective) counts with zero
slack, no placer behaviour changed:

- **F5 / P4** (`flow_geometry.rs`) — series-signal elements horizontal,
  upstream pin at lower X. Series = 2-terminal, non-`Power` role,
  NEITHER node rail-class; recomputed in the test from the netlist, per
  the F3 precedent, so it can falsify the crate rather than restate it.
  Guarded by
  `series_discriminator_separates_stub_from_series_on_common_emitter`,
  which asserts the bypass cap `CE` is classified **non**-series and
  stays **vertical** — without it, F5 degenerates into a demand that
  every two-terminal part be drawn sideways (ADR-15's "capacitors are
  horizontal is WRONG" trap).

  **Measured total: 16, not the 2 the design review expected.** The
  defect is systemic: `rc_lowpass` 1, `rc_lowpass_ports` 1,
  `common_emitter` 1, `multivibrator` 2, `diff_pair` 0,
  `opamp_inverting` 2, `opamp_inverting_real` 1, `port_shapes` 3,
  `opamp_definition_level` 4, `named_rails` 1. Only `diff_pair` — which
  contains no series element at all — is clean by construction.
  `rc_lowpass` is the instructive case: R1 *is* horizontal and still
  fails, on **direction** (upstream `in` pin at x = 54.61, right of the
  downstream `out` pin at x = 46.99) — the "axis is only half the
  constraint" mode ADR-15 Stage 5 identified but never measured.

- **P5** (`flow_geometry.rs`) — every declared input terminal strictly
  left of every declared output terminal. Measured 1, on
  `rc_lowpass_ports` (both at x = 41.91). F4 pins each terminal to the
  correct end of *its own* net; P5 is the sheet-wide statement F4 cannot
  make.

- **P10** (`placement_stability.rs`) — two cache-less conversions
  byte-identical. **Landed LIVE, not `#[ignore]`d.** The ADR-17 design
  review expected this to be un-ignorable only after the SA retires at
  Stage 2; measurement says the
  SA is seeded deterministically and all ten fixtures already round-trip
  byte-identically today. Determinism is orthogonal to the sensitivity
  P11 measures — a chaotic map is perfectly deterministic and still
  re-bases globally on the smallest input change.

- **P11** (`placement_stability.rs`) — as landed, *basin locality*:
  adding ONE element moves poses only in the affected neighbourhood.
  `#[ignore]`d, budgets at 0 against a measured 5/17.

  > **AMENDMENT (retirement) — this P11 is DELETED and REPLACED.** The
  > seed-arm control shows budget-0 locality is unreachable by any
  > spacing-derived placement, so the test was a target no architecture
  > could hit; keeping it `#[ignore]`d in the suite was worse than
  > deleting it. Its replacement, **P11 — cache-path stability**, asserts
  > the achievable property users actually experience: edit a netlist,
  > re-convert into the same output directory so the ADR-4 sidecar is
  > read, and **zero pre-existing user symbols change pose** (measured
  > 0/2 and 0/8), the connectivity check still passes, and no V12 / V13 /
  > body-overlap count grows on the extended sheet. Power glyphs are
  > matched by pose + `lib_id` rather than refdes; the emission-order
  > refdes renumbering is a real, small, separately-fixable defect that
  > the new test pins. **Landed LIVE.** F5/P4, P5 and P10 are unchanged.

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

---

## HPWL ablation — measured, kept, renamed in spirit

**Status: measured; term KEPT at weight 1.0. Do not delete without
re-running this ablation.**

**The challenge.** `cost.rs`'s `hpwl: 1.0` is absolute half-perimeter
wire length. For schematics we maximise READABILITY and area is not an
objective; "too short nets are bad for readability". HPWL is also the
ONLY term in the SA objective that rewards *crowding* — the way to lower
it is to move elements closer together. Its stated job ("wires shouldn't
sprawl") is covered by two later, better measures: **V16 bends**
(`wire_geometry.rs`, corners not distance) and the **wire-detour**
ratchet (`placement_quality.rs`, normalised by a rectilinear ideal, so
it measures router *detour* rather than element spacing).

**Correction to the second citation (2026-07-20).** At the time this
ablation was written the detour ratchet was `wire_length_within_budget_
across_fixtures`, whose baseline was derived from *labels* — and nine of
the ten fixtures emit no multi-pin labelled net, so they hit a
`baseline < 1e-6` early-`continue` and were never graded. The metric
cited here as covering HPWL's job **was not running on any fixture the
ablation table reports**. The ablation's CONCLUSION is unaffected: it
rests on measured V16 / V5 / F5 / V13 movements, not on this ratchet.
But the claim "HPWL's job is already covered" was, as of that writing,
only half-supported. The verifier has since been rebuilt on pin geometry
(`wire_detour_within_budget_across_fixtures`) and now grades all ten;
the citation is sound going forward. If the hpwl term is re-litigated,
re-run the ablation against the working detour metric rather than
inheriting this table's coverage claim.

**The experiment.** `hpwl` set to 0.0, nothing else changed, all ten
fixtures re-converted and the whole `spice2kicad` suite re-run
(`--no-fail-fast`). Every other fixture/metric not listed below was
byte-identical or unchanged.

| Metric | Fixture | 1.0 → 0.0 | Verdict |
| ------ | ------- | --------- | ------- |
| V16 B | `common_emitter` | 4 → **2** | better |
| V16 J | `common_emitter` | 3 → **4** | worse |
| V16 B | `opamp_inverting` | 3 → **4** | worse |
| V16 B | `opamp_inverting_real` | 8 → **6** | better |
| V16 B | `rc_lowpass_ports` | 2 → **3** | worse |
| V5 outward | `opamp_inverting` | 1 → **2** (`RF.1`, `RF.2`) | worse |
| F5 series pose | `opamp_inverting` | 2 → **1** | better |
| F5 series pose | `opamp_inverting_real` | 1 → **0** | better |
| CIN-horizontal guard | `common_emitter` | pass → **FAIL** (CIN goes vertical) | worse |
| P11 cache growth | `rc_lowpass_plus_r` | V13 label↔body 0 → **1** | worse, **Tier 1** |

**Conclusion — outcome 2 of the three anticipated: the term is
load-bearing under a misleading name.** It is not a wire-length
objective and should not be read as one. What it empirically supplies is
a weak **cohesion prior**: it keeps a net's pins inside one region of the
sheet, which is what stops `common_emitter`'s `CIN` flipping vertical and
what keeps the P11 grown-sheet layout from re-anchoring a label onto a
body. Deleting it trades three fixtures' V16/V5 and one Tier-1 V13
(P11) for two fixtures' F5 series pose — a mixed result, and the Tier-1
loss alone disqualifies it under the tier ordering.

Two consequences recorded so this is not re-litigated from the name:

1. The doc comment on `CostBreakdown::hpwl` now says what the term
   actually does and points here. It is not evidence that "shorter is
   better" is a project goal.
2. Because it is a crowding term, it is the wrong instrument for any
   FUTURE readability work. If a spacing/cohesion property is wanted
   explicitly, build it as a term that measures cohesion (net-pin
   dispersion) rather than length, and retire this one against that —
   with this same ablation table as the acceptance bar.

---

## ADR-18 — Multi-channel layout: numbered ports, uncoupled repeats, and a geometry-derived seed stride

**Status:** ACCEPTED, landed. The V16 bend rise it carries was **NOT an
explicit project-owner decision** — it was landed by the operating
assistant under the owner's standing instruction to proceed without
per-change confirmation, and the automatic global-improvement escape
does not reach it either (see below). Re-examine that budget rather
than citing it as owner precedent.

### The symptom

`opamp_definition_level` — two independent inverting-opamp channels —
was drawn **backwards and X-interleaved**: `RIN1` at x=60.96 fed `X1` at
x=36.83 (the opamp to the LEFT of its own input resistor), and the two
channels' elements were shuffled into each other's x-span. It also
carried the fixture's two worst outstanding debts: an "OWED, NOT
ACCEPTED" V12 budget of 4 (wires through foreign bodies) and the last
surviving `CROSS_NET_V02_ESCALATIONS` entry (a latent V11 short).

### Four root causes, none of them the router

1. **`layers.rs::no_source_fallback` matched port names asymmetrically.**
   `in`/`input`/`out`/`output` were matched by **equality**, but
   `vin`/`vout` by **prefix**. A multi-channel circuit *must* number its
   ports (`in1`, `in2`, `out1`, `out2`), so under equality matching they
   matched nothing at all: **every** multi-channel circuit had no input
   anchor and was layered backwards. Fixed by stripping trailing channel
   digits and matching exactly against a closed set in both directions
   (`boundary_net_role`). Prefix matching was not the right
   generalisation either — `in_amp`, `input_stage`, `inverting` are
   ordinary interior nets.
   The fix additionally requires an input-owning element to be a
   **pass-through** (≤ 2 Signal nets); without that, `diff_pair`'s
   transistors (whose bases are `in1`/`in2` but which also carry
   collector and tail nodes) get rooted at layer 0 and collapse onto
   their own collector loads.

2. **The `refined_roots` → `coarse_roots` well-formedness fallback was a
   side door.** The `signal_degree <= 1` refinement exists precisely to
   stop a rail-supplied interior element (an opamp) being a layer-0
   root; the fallback handed back the unrefined set, re-admitting exactly
   what the refinement rejected, whenever no input anchor existed. It now
   relaxes the degree threshold *minimally* (the power-touching elements
   of minimum signal degree), which spans whenever `coarse_roots` did.

3. **`symmetry::detect_pairs` had no coupling predicate.** A σ-involution
   proves two halves are structurally interchangeable; it does **not**
   prove they belong on opposite sides of a shared axis. `diff_pair` and
   `multivibrator` are *coupled* (a shared node sits on the axis).
   N repeated but *uncoupled* channels share only the supply rails, and
   mirroring channel 2 onto `axis_sum - L.x` maps it onto the SAME
   x-span channel 1 occupies. Now gated by union-find over **non-rail**
   nets, dropping uncoupled pairs individually. `diff_pair` and
   `multivibrator` stay byte-identical (`coupled_halves_still_pair`).

4. **The seed was infeasible, and no downstream owner could repair it.**
   `place_seed`'s within-bucket Y rank stride was a hardcoded 5 cells
   (6.35 mm) *regardless of geometry*, so two `OPAMP` triangles — a
   10.16 mm body with a 15.24 mm pin span — were seeded 6.35 mm apart.
   This is the general lesson worth keeping: **a hard constraint cannot
   repair an infeasible start.** The SA overlap gate is a
   never-*increase* filter, so it locks the overlap in rather than
   fixing it; `legalize` shoves greedily in index order, which on two
   mutually overlapping triangles merely relocates the clash (measured:
   it shoved X1 into X2). The stride is now geometry-derived
   (`bucket_y_strides`: body ∪ own rail-glyph reach, floored at the old
   value), scoped to a bucket stacking **two or more** oversized bodies.
   Widening single-oversized buckets too was implemented and measured,
   and regressed `opamp_inverting_real` (V5 0→1, V16 B 5→7) for no
   gain — the sideways trade the ratchet rule forbids.

### Measured, and the owner-approved escape

`opamp_definition_level` now reads left-to-right per channel: RIN1
35.56 → X1 53.34 → RF1 59.69; RIN2 52.07 → X2 62.23. Per ADR-16's
baseline-diff protocol, **18 of 120 baseline rows move, all on this
fixture**; the other nine fixtures are byte-identical, so the
anti-overfit bar is met by construction.

| metric (fixture: `opamp_definition_level`) | before | after | tier |
| ------------------------------------------ | ------ | ----- | ---- |
| cross-net collinear overlap (latent V11)    | 1      | **0** | 0    |
| V12 wires through foreign bodies            | 4      | **0** | 1    |
| wire crossings                              | 6      | **0** | 2    |
| F5 flow-pose violations                     | 4      | **1** | 2    |
| V16 B (bends)                               | 12     | **15**| 2    |
| V16 J (branches)                            | 0      | 0     | 2    |

The B rise 12 → 15 is a **ratchet rise on assistant judgement, not on
explicit owner sign-off** — the owner had given a standing instruction
to proceed without per-change confirmation, but never saw this budget.
The automatic global-improvement escape does *not* reach it either, on
the owner's original framing (F5 −3 against B +3 is net zero across
fixtures). What
the measurement above adds is that the rise is paid for by strictly
**higher-tier** gains — a Tier-0 latent short and a Tier-1 V12 debt both
go to zero. That is the *permitted* direction under the tier ordering
(Tier 2 pays for Tier 0/1), never the forbidden one.

Two long-standing debts were retired by this and their comments'
stated preconditions honoured rather than deleted:
`v12_crossing_budget`'s "MUST ratchet down to 0 when the seed defect is
fixed", and the last `CROSS_NET_V02_ESCALATIONS` entry.

### Coverage note (why this was landable at all)

This work landed *after* the fixture lists were unified so that
`placement_quality`'s `no_symbol_symbol_overlap_across_fixtures` and
`no_power_glyph_foreign_body_overlap_across_fixtures` — both
unconditional-0 Tier-1 invariants — actually graded
`opamp_definition_level`. They previously did not, and two earlier
intermediate states of this same work (a VCC glyph inside `RF1`'s body;
two massively overlapping opamp triangles) had passed the entire suite.
Do not measure a placement change against a suite that cannot see the
fixture it moves.

## ADR-19 — Locality-first placement: neighbourhood-relative coordinates behind a tested locality bound

**Status: DESIGN. Milestone 1 (the locality-bound verifier) LANDED with
this ADR; Milestones 2–6 are a staged plan, each gated on owner sign-off
per ADR-17's "escape lists brought for sign-off in advance, per stage"
rule.** This is the design `docs/placer-redesign.md` deliberately stops
short of. It does **not** license skipping that document's §8 bar: it is
here *because* it targets acceptance criteria (1) locality-as-a-tested-
property and (2) the signed-complete decoration footprint head-on, in
that order, rather than assuming them.

### Why this is not ADR-17 again

ADR-17 was retired because its central promise — *determinism buys
locality* — was falsified: the bare deterministic seed re-bases 17/17
just like the SA (§ ADR-17 RETIRED). ADR-19 does **not** remove the SA
and does **not** claim determinism as the fix. It keeps every mechanism
placer-redesign.md §6 proved right (pinning, the real-router oracle, the
lexicographic tuple + guards, tier governance, the role model) and
changes exactly the property ADR-17 mis-attributed: **the coordinate
model, from page-global to neighbourhood-relative.** "Determinism is not
locality" is the premise, not a hoped-for consequence.

### The mechanism, located (R-A)

The blast radius is produced by three global coordinate-derivations, now
pinned to lines by the ADR-19 code survey:

1. **Y is scaled by the element count.** `y_bot = (n+4)·Y_RANK_STRIDE`
   (`lib.rs:1527`); Bot/Mid coordinates are measured *downward from
   `y_bot`* (`lib.rs:1593-1599`), and `pack_rows` re-centres the whole
   stack with `y + shift[r] − total/2` (`lib.rs:1431`). Adding *any*
   element changes `n` and re-bases every Bot/Mid absolute Y.
2. **X is a left-to-right prefix-sum.** `layer_x[l] = layer_x[l−1] +
   stride` with `stride` keyed on the per-layer *max* width
   (`lib.rs:1501-1515`). Widening or inserting any layer shifts every
   layer to its right.
3. **Ordinals are global ranks** — longest-path layers
   (`layers.rs:505-525`), min-index row numbering (`channels.rs:130`).

The `align`/`place` path (`lib.rs:1674-1841`) is the *one* part that is
already neighbourhood-relative: members take an anchor's fixed coord plus
a locally-accumulated cursor. **It is the model ADR-19 generalizes to the
whole placement.**

### The design

- **Order is meaning; absolute stride is slack.** Keep
  classify→bands→layers as the *ordinal* skeleton (which band, which
  flow-column, which row). Replace the *metric* conversion so a
  coordinate is an offset from a **local anchor** (the element's flow /
  band neighbour), never from a page-global prefix-sum or an `n`-scaled
  datum. Band identity maps to a **fixed datum** (Top / Bot at constant
  y, Mid between) independent of `n`; columns are spaced by the *local
  pair clearance* to the immediate left neighbour only. Distant bands and
  columns then keep their coordinates when a far element is added — the
  P11b bound (Milestone 1) is what proves it, and it ratchets down.
- **Signed, complete decoration footprint (R-B).** Replace the symmetric
  `.abs()` halo (`anneal.rs:439-456`) with a first-class signed
  `Footprint { body, pins, glyph (one-sided), value_text, labels,
  pwr_flag }`. `glyph_geom::glyph_reach` already computes the *signed*
  reach; stop folding it through `hw.max(dx.abs())`. Because the honest
  AABB is a strict subset of the halo, making it honest **relaxes** the
  gate — so the ratchets must be re-calibrated to the honest quantity **in
  the same commit**, and the footprint must be *complete* first (label /
  Reference-Value / PWR_FLAG classes, which today are unreserved). This is
  the Stage-2-kill lesson made structural: **the footprint precedes any
  spacing change, it is not a follow-up.**
- **Joint pose (R-C), role model preserved.** Emit position + orientation
  + mirror from one flow-role datum. The role discriminator
  (anchor / series / rail-stub / terminal, from pin counts + net classes,
  never refdes/name — Wall 3) is carried forward intact; ADR-15 Stage 5
  validated it. Phase 4.5 is *promoted* from orientation-only to **bounded
  local pose repair** (offenders only, ±2 cells, real-router oracle), with
  the existing `(v13, v12, v5, bends)` tuple and `severed`/`v11`/`overlap`
  /`v12` guards unchanged.
- **The SA stays, as a *local* polisher.** It is basin-finding, not
  compaction, and load-bearing on `common_emitter` / `named_rails` /
  `opamp_inverting` (§ ADR-17 ablation). Its moves become
  neighbourhood-scoped; the two cost terms that read the *moving page
  bbox* — `rail_direction` (`cost.rs:753`) and `soft_y_residual`
  (`cost.rs:1146`) — are re-expressed against the **fixed datum** so the
  optimizer stops coupling every element through the page frame.

### Staging (verifiers first; each stage has a kill criterion)

| M | Content | Gate / kill |
|---|---------|-------------|
| **1** | **P11b — cache-less locality bound** (this commit). Page-pan-normalized count of pre-existing *user* symbols that move when one element is added; ratchet `rc_lowpass=0`, `common_emitter=8`. | LANDED. Pure verifier, no behaviour change. |
| 2 | Signed **complete** footprint as a computed quantity + unit tests; **not yet wired** to any gate. | Pure add; no ratchet may move. |
| 3 | Wire the footprint into the SA overlap gate, `legalize`, and phase-4.5 V13; **re-calibrate ratchets in the same commit**. | **ATTEMPTED, MEASURED, BLOCKED — kill criterion fired.** Only the two zero-movement parts landed (`legalize::extent_of` de-duplication + the `property_text` fidelity fix). Wiring either consumer regresses Tier-1 V13 and the F6/detour ratchets. See "M3 blocked" below. |
| 4 | **Fixed-datum Y** (decouple `y_bot` from `n`; re-express the page-frame cost terms). Baseline regenerated under ADR-16 (V16 (B,J) non-increasing per fixture). | **LANDED `ed51164`, then REVERTED — pending M3.** See "M4 reverted" below. |
| 5 | **Relative X columns** (local neighbour clearance replaces the prefix-sum). It was DROPPED on a measurement taken *after M4* (a `--no-refine` seed moving 1 mover); M4 is reverted, so that measurement no longer describes the tree and the drop is **withdrawn**, not re-affirmed. | P11b must not rise; every ratchet holds. |
| **5′** | **SA trajectory decoupling** — private per-element RNG streams keyed on refdes + deterministic sweep, to make each element's proposal sequence netlist-stable. | **ATTEMPTED, MEASURED, REVERTED.** Killed by K2 **and** K3 at once: the private-stream sweep bought **no** locality (`common_emitter` movers stayed 7/7 — the acceptance cascade through the cost terms dominates, not the proposal RNG), **and** its one-time re-basin destroyed the SA's bend-finding — `common_emitter` V16 B **4→11**, `opamp_inverting_real` 5→10, `opamp_inverting` 3→5, `named_rails` 1→2. The finding sharpens the wall: **the SA's netlist-sensitivity and its basin-finding are the SAME property** — the specific random search trajectory both lands the good bend basins *and* is what shifts under a netlist edit. K1's "spurious, containable" reading was **falsified**: re-keying to netlist-stable streams keeps the move set but lands strictly worse basins. Determinism is not locality (ADR-17); *and* now: netlist-stable keying is not locality either — it is basin destruction. |

| 6 | Joint-pose construction + promoted phase 4.5 (bounded local repair). | Wall-1 flow cases (`COUT`/`RIN`) horizontal-and-clean vs the **real router**, no Tier-0/1 regression. |

**R-A locality — frontier, as of the M4 revert.** M5′ proved the cache-less
blast radius of the SA **inherent**: it cannot be re-keyed away without
destroying the basin-finding the V16 ratchets depend on. This is the same wall
ADR-17 hit from the compaction side and ADR-15 Stage-5 from the orientation side.
M4's seed-locality claim (a `--no-refine` seed moving 1 element on
`common_emitter`+CB) is **withdrawn with M4**; the M1 ratchet is back at its
pre-M4 value (`common_emitter`=8, `rc_lowpass`=0). The two findings that survive
the revert are M5′'s (determinism/netlist-stable keying is not locality) and
the M4 post-mortem below (the Y datum cannot be re-derived before M3).

### M3 blocked — the honest footprint is a *subset*, and the freed space is label space

**Status: the wiring is NOT landed. Two zero-movement parts of M3 are.**
The kill criterion in the M3 row fired: every way of consuming
`footprint.rs` measured so far raises a Tier-1 V13 budget, and F6 / wire
detour with it.

**What landed** (empty `baseline_lock` diff, whole workspace green):

1. `legalize::extent_of` now *is* `footprint::body_and_pins`. The tight
   legality geometry had been open-coded a third time; it is one
   definition now. Byte-identical output — this is a de-duplication.
2. `footprint::property_text` was **wrong** as M2 shipped it. It modelled
   host Reference/Value as `TextKind::CenteredProperty` at rotation 0.
   The emitter writes those fields with `(justify left)` and KiCad draws
   them at `field_render_rotation(orientation)`. So the M2 box was
   half-a-string too wide behind the anchor, half-a-string too short
   ahead of it, and pointed the wrong way for every mirrored or rotated
   symbol. `field_render_rotation` moved from `kicad-emitter` down into
   `kicad_symbols::text_geom` — the crate all three consumers share —
   and the placer-side box now matches `placement_property_bboxes`
   exactly. Nothing consumes it yet, so this moves nothing; it is the
   precondition for M6 consuming a box that is actually the drawn one.
3. Phase 4.5's V13 model **needed no change and got none**. It already
   scores real body bboxes and real property bboxes; it is the *more*
   faithful model of the two. M3's honest action there was to align the
   placer to it, which is (2).

**What was measured and reverted.** Three trees, one machine, whole
workspace, `--no-fail-fast`, fresh output dirs, cache off:

| variant | SA overlap gate | `legalize` roomy | result |
|---|---|---|---|
| **C (landed)** | halo (unchanged) | `world_extent_with_glyphs` | **green; `baseline_lock` empty** |
| **B** | signed `body ∪ pins ∪ one-sided glyph` | unchanged | 23 `baseline_lock` rows; **V13(1), V13(7), V13 item(3) each 0 → 1 on `named_rails`**; F6 `common_emitter` 4 → 5; detour `common_emitter` 1.014 → 1.025; Q3 `common_emitter` 1 → 3; Q5 `named_rails` 2 → 4 (while `common_emitter` 3 → 2 — a sideways trade) |
| **A** | as B | signed `element_footprint` | B's eight failures **plus two**: V16 **B** `common_emitter` 4 → 7, and `rendered_text_does_not_overlap_across_fixtures` (real `kicad-cli` SVG ink) 0 → 1 on `common_emitter`. The roomy change is not neutral; it is strictly worse than leaving it alone. |
| **full M3** | as B **plus directional property text** | as A | 52 `baseline_lock` rows; V16 **B** `common_emitter` 4→5, `opamp_inverting_real` 5→9, `opamp_inverting` 3→5; crossings `opamp_inverting` 0→1; detour `common_emitter` 1.014→1.048; F6 4→6; Q3 1→2; Q5 `common_emitter` 3→6, `opamp_inverting_real` 0→2; P11 cache-path stability breaks (4 glyph poses); the `severed`-guard's demonstrated `COUT` rot-180 case stops reproducing |

**The mechanism, and it is not a tuning problem.** M2's own test proves
the signed footprint is a **subset** of the halo on the classes both
model. So making the gate honest can only *free* space. The freed space
is not empty — it is where `spice-route` will later plant a **net
label**, the one decoration class the placer still does not reserve at
all. Three independent V13 verifiers caught the same `named_rails` label
landing on a body / on pin text / on a foreign body. The halo's
over-reservation was **doing the label class's job by accident**; remove
it and the accident stops.

This is ADR-17's Stage-2 kill, measured a second time from the opposite
direction. Stage 2 relocated collisions by *compacting*; M3-B relocates
them by *un-reserving*. Same cause: an incomplete reservation cannot be
consumed, in either direction.

**Corollary that changes the staging.** ADR-19's design text says "the
footprint must be *complete* first (label / Reference-Value / PWR_FLAG
classes, which today are unreserved)". M2 delivered the
Reference-Value class and deferred labels/`PWR_FLAG` to M6. The
measurement says that deferral is not available: **the label class is
not an M6 refinement of M3, it is a precondition of M3.** M3 cannot be
consumed until `label_geom` lands, so ADR-19's dependency edge is
M2 → *label reservation* → M3 → M4, not M2 → M3 → … → M6.

That is awkward, because `archive/adr14-label-reservation-alone` already computes exactly
this geometry and its own commit message records that wiring it "nets out
worse". So the two halves of the reservation are *each* individually
unlandable.

**CORRECTION — the "land them together" experiment is NOT untried; it was
run, and it failed.** An earlier draft of this section proposed combining
them as the next experiment. The ADR-14 reservation post-mortem *in this
same file* already records it: "*Measured — directional PLUS the complete
label reservation … an **identical failure set**. The reservation recovers
nothing the directional model gave away. With `legalize` in scope it is
strictly worse (`opamp_definition_level` V5 0→5, V16 B 12→13).*" Its stated
general result subsumes M3 entirely:

> An exact directional model plus a complete label reservation together
> reserve strictly LESS space than the accident does. … There is no
> remaining unreserved class large enough to close the gap, because the gap
> is not an unreserved class: it is the mirrored copy of a reach that does
> not exist.

So M3 is **not** blocked pending an untried experiment. M3 is the **third
independent reproduction** of an established negative — genuinely
corroborating (it fired on `named_rails`, where the earlier runs fired on
`rc_lowpass` / `common_emitter`, and it arrived from the SA-gate side rather
than the seed side), but not a new frontier. The post-mortem's own
instruction governs: *"Do not re-derive; re-measure only against a variant
this post-mortem does not already cover."*

**Consequence for ADR-19.** The dependency edge M2 → label reservation →
M3 → M4 is correct as a *statement of what M3 needs*, but it is not a route
forward, because the label reservation has been measured not to supply it.
M3 is dead as specified, and M4 sits behind M3. The only variant the ADR-14
post-mortem records as untried is **directional extents plus an explicit
compensating outward margin** — sized to preserve today's total reach,
inverting the failed premise by treating the halo's *magnitude* as the
calibrated quantity and its *placement* as the bug. That variant is flagged
there as UNTRIED, NOT ENDORSED, a re-parameterisation of every layout in the
suite, and requiring an owner escape request with the full ratchet table.
Milestone D (bounded joint-pose repair) is the remaining ADR-19 work that
does **not** sit behind this chain.

*Record-keeping note — RESOLVED 2026-08-08.* ADR-14's "Recoverable work"
paragraph used to attribute `8b060cf` to `archive/adr14-label-reservation-alone` and
`d176b9e` to `archive/adr14-directional-plus-label`. That was backwards, and worse
than a simple name swap: `0214412` is a **materially different variant**
(label reservation alone, on an unmodified halo, from an earlier base),
not another name for `8b060cf`. Both branches are now retired in favour
of tags named for their *contents* —
`archive/adr14-directional-plus-label` (`8b060cf`, whose parent
`d176b9e` is the directional gate alone) and
`archive/adr14-label-reservation-alone` (`0214412`) — so the ambiguity
cannot recur. ADR-14's paragraph carries the corrected table.

**Additional findings.**

- The full-M3 direction (property text in the SA's *hard* gate) is
  independently wrong on doctrine, and the numbers agree. Property text
  is repairable downstream — `nudge_property_text` moves it — so it is
  not the categorical yes/no fact CLAUDE.md's decision rule requires of
  a hard constraint. Making it one over-constrains the SA and costs V16
  bends on three fixtures. If property text is ever reserved, it belongs
  in a *preference* (the `legalize` roomy extent), not in the gate.
- The `legalize` roomy extent is the **second-worst** place to put the
  signed footprint. Its doc comment already records that an earlier
  version legalized on the *roomier* `world_extent_with_glyphs` and had
  to be pulled back; M3 pushed the same lever the other way and it
  regresses too — V16 **B** `common_emitter` 4 → 7 and a real-SVG-ink
  text overlap on top of everything B already breaks. The tight/roomy
  split is calibrated; both ends of it are load-bearing.
- Pre-existing, unrelated to M3: `balance_quality`'s informational
  `Q6_REFERENCE` for `common_emitter` is stale on `master` — measured
  1.2247 against a literal of 1.0000, on a pristine `c968cbd` tree. It
  is informational-only (the hard gate is the degeneracy ceiling), but
  the M4 revert appears to have freed it and nothing reclaimed it.
- **Where the code lives — ON MASTER, since ADR-23.** The wiring is no
  longer only on a branch: it is live and compiled in
  `solver/anneal.rs::symbol_overlap_count_m3` and
  `legalize.rs::roomy_extents`, dead on the default path and reachable as
  `--placer=m3-signed-gate` / `--placer=m3-signed-full`. Run it, don't
  re-apply it. Tag `archive/adr19-m3-signed-gate` (`7896f22`) is retained
  only as **provenance** — ADR-23 D6 claims those challengers are that
  SHA verbatim, and that claim is auditable only while it resolves.
  (Ablation **A** — signed gate + signed `legalize`, no property text —
  is not registered; it was never a buildable tree on the branch either,
  and adding an enum variant is now the cheaper route to it.) The
  mechanism above is the thing to change.

### M4 reverted — the Y datum cannot lead M3

`ed51164` (M4, content-derived n-independent Y datum) was **reverted**. It
landed with a false green: its commit message claims "all ratchet suites green"
having enumerated `placement_quality`, `electrical_safety`,
`placement_stability`, `wire_geometry` and `baseline_lock` — **`flow_geometry`
was never run**, and the later working sessions compared against the session's
own HEAD rather than pre-session `master`, so the regression stayed invisible.

True bisect of `flow_geometry.rs::stub_lateral_run_within_ratchet`
(`multivibrator` F6 budget 2), one command, one machine:

| commit | state |
|---|---|
| `e476d2a` (pre-session master) | PASS |
| `cec3fd2` (M2, signed footprint) | PASS |
| `ed51164` (M4) | **FAIL — 18 cells** |
| `619cc31` (HEAD) | FAIL — 18 cells |

**Mechanism.** M4 makes each Mid sub-row datum chain as
`next = prev + depth[prev] + max(MID_SUBROW_GAP, reach_clearance)`, so the
16-cell floor is applied *on top of* the reserved bucket depth. On
`multivibrator` (seed, `--no-refine`, cache off) the MidUp→MidCtr datum pitch
goes 13 → 31 cells and MidLo follows: `RC1 19→7, RB1 29→17, C1 32→38,
Q1 45→59`. The bias resistors `RB1`/`RB2` (MidUp) end up ~39 cells above the
transistor bases they feed (MidLo). F6 anchors a rail stub on the *Manhattan-
nearest* other pin of its own net; past that stretch, `RB1`'s nearest pin on
net `b1` flips from `Q1`'s base (2 cells laterally) to the cross-coupling
capacitor `C2` in the **other channel's column** — 18 cells laterally. The
number F6 reports is real: net `b1` now runs 39 cells vertically and 18
laterally instead of hanging off the base.

**Why no targeted repair landed.** Two were measured, both trade one fixture
for another (forbidden sideways under the within-tier rule), with a chaotic
response that has no monotone structure:

| variant | `common_emitter` F6 | `multivibrator` F6 | `named_rails` F6 |
|---|---|---|---|
| ratchet (budget) | 4 | 2 | 6 |
| `MID_SUBROW_GAP` 16 (as landed) | 4 | **18** | 6 |
| `MID_SUBROW_GAP` 14 | 4 | 2 | 4 |
| `MID_SUBROW_GAP` 12 | **7** | 2 | 3 |
| `MID_SUBROW_GAP` 10 | **5** | 2 | 4 |
| `MID_SUBROW_GAP` 8 | 4 | 2 | **9** |
| floor as datum *pitch*, `max(GAP, depth+reach)` | **7** | 2 | **9** |

`MID_SUBROW_GAP = 14` is the only all-green row, and it passes by **one cell**
of Manhattan tie-break margin on the very flip described above — a knife-edge,
not a fix. The principled reformulation (treat the floor as a minimum datum
*pitch* rather than an additive clearance, which is arguably what a
depth-reserving chain should do) regresses two fixtures.

**The finding.** ADR-19 states of M3 that "the footprint precedes any spacing
change, it is not a follow-up", and stages the safe/precondition work ahead of
the dangerous Y change. M4 shipped with M3 skipped. The table above is what
that costs: with the SA gate still reading the *halo* rather than the honest
signed footprint, every Y-spacing value lands in a different, unattributable
basin, and there is no local argument for choosing one. **M4 is re-attemptable
only after M3**, and its re-attempt must run `flow_geometry` (F3/F4/F5/P5/F6)
in the gate set.

**Where the code lives.** The revert removes M4's implementation from the
mainline but does not discard it: branch **`archive/adr19-m4-pending-m3`** (at
`619cc31`) holds the tree as M4 landed, so the post-M3 re-attempt starts from
the code rather than re-deriving it. Read this section first — the datum-chain
mechanism above is the thing to change, not to re-apply.

**Gate-set lesson (the primary failure, distinct from the control arm).**
`ed51164`'s commit message *enumerates* the suites it ran —
`placement_quality`, `electrical_safety`, `placement_stability`,
`wire_geometry`, `baseline_lock` — and `flow_geometry` is simply absent. The
regression was therefore visible-by-omission at commit time, before any
control-arm reasoning entered. A hand-picked suite subset is not a gate. Run
the whole workspace with `--no-fail-fast` (cargo's default is fail-fast, which
truncates the red list at the first failing binary and reads as a shorter
failure than it is).

**Ratchet arithmetic across the revert.** Two literals return to their pre-M4
(`cec3fd2`) values — `wire_geometry` `named_rails` V16 B 1→2, and
`placement_stability` P11b `common_emitter` mover 7→8. Both were lowered *by
`ed51164` itself*; restoring them alongside the revert that created them is not
a budget bump, and neither exceeds any mark recorded at or before M4. Owner
signed off. In the other direction the revert *freed* slack that the suite
hides: A3's Q5 verifier only `eprintln!`s a reclaimable value rather than
failing on it, and `common_emitter` was reclaimed 4→3 (`9881d4f`). Any future
change that moves Y must re-run A1/A3 with `--nocapture` for the same reason.

**M5′ premise corrections (Fable, verified):** the SA is load-bearing for
bends on **two** fixtures (`common_emitter` B 11→4, `opamp_inverting` B 5→3),
not three — the "3" came from the invalid ablation table the record forbids
reusing; `named_rails` is neutral. The Tier-0 dependency on `common_emitter`
predates the `Measure::severed` guard and no longer exists. Default
`refine_iterations` is 200. M5′ does **not** change spacing, so the
footprint-precondition (which binds compaction) does not gate it; it may
precede M3.

Y (M4) sits behind the footprint (M2–M3) and the datum work deliberately:
**"X spacing is slack; Y spacing is meaning"** — squeezing Y collapses the
signal band onto the rails and the router pays in bends (§ ADR-17 Stage 2,
four measured variants). The safe/precondition work leads; the dangerous
Y change trails, exactly as the Stage-2 kill demanded.

### Must-not-repeat (from placer-redesign.md §6)

Determinism-as-locality (ADR-17); a flow-orientation hard filter against
*fixed* positions (ADR-15 Stage 5 / Wall 1); an "honest" footprint without
same-commit recalibration (R-B); glyph-side fixes for what is a *layering*
defect (V14 residual). ADR-19 avoids each by construction: locality is
measured not assumed (M1); pose is joint not orientation-only (M6); the
footprint is completed-then-recalibrated (M2–M3); roles come from topology
not names.

### Honesty check

Milestone 1 is the only stage landing now, and it moves no symbol. Its
whole value is that it converts R-A from an untested quirk into a governed,
ratcheting number — which is placer-redesign.md acceptance criterion (1)
and the precondition ADR-17 paid to learn ("make locality an explicit
design property with an acceptance test, not a hoped-for consequence").
The measured baseline **corrects** the doc's stale "17/17": on this tree,
page-pan-normalized user-symbol movement is `rc_lowpass` **0** and
`common_emitter` **8**. Milestones 2–6 are a plan; each returns for
sign-off before it lands.


## ADR-20 — Tier 0 leads phase 4.5's objective; the `shunt_feedback_amp` Tier-0 short

**Status:** landed (the acceptance-predicate + oracle changes); the
remaining `shunt_feedback_amp` defect is diagnosed and **owner-gated**
(it is R-5, see "Root cause" below).

### The report

`shunt_feedback_amp` — a textbook shunt-feedback CE amplifier — converted
at default settings into a schematic that **shorted the transistor's base
net to its emitter net**, and the CLI's own post-emit connectivity check
rejected it. `--refine-iterations` 0/1/20/40/60/80/100/400 converted
cleanly; 150 and 200 (the default) failed. Not a gradient — one specific
SA end-state.

### Mechanism, precisely

1. At 200 iterations the SA lands `Q1` at `(46.99, 45.72)`, and the
   placement reaching layout phase 4.5 **already has two severed signal
   nets** (`severed = 2`, measured by phase 4.5's real-router oracle).
   Nothing after the placer can move an element, so this is a *placer*
   defect, not a router one.
2. Phase 4.5's acceptance predicate scored `(v13, v12, v5, bends)` and
   held `severed` as a **floor** — a `<=` guard it could never *seek*.
   With the incoming baseline already at 2, the phase had no reason to
   repair the severance, and the repair it stumbled into (because it
   improved the Tier-1/2 tuple) was rotating `Q1` until its **base pin sat
   exactly on `RE`'s pin 1**.
3. Two pins of different nets on one coordinate is a short **no router
   pass can undo**: `spice-route` moves wires, not pins, and
   `conflict::resolve_conflicts` deliberately declines to jog a wire
   endpoint that sits on a pin (that would disconnect the pin). It burned
   its whole derived iteration bound, emitted `conflict: … endpoint
   conflicts left after 6 resolve iterations` as a **warning**, and the
   emitter wrote coincident geometry.
4. The only thing that turned this into a non-zero exit was the CLI's
   *optional* post-emit `kicad-cli` connectivity check — skipped by
   `--no-verify` and skipped entirely on a machine without KiCad. On such
   a machine the converter shipped a wrong circuit silently.

### What the earlier reading got wrong

The defect lock originally read this as "the router's conflict-resolution
cascade cannot legalise the SA's placement". True, but it names the
*victim*. The cascade behaved correctly at every step; what was missing
was that **three separate stages had no term for the hazard**:

* the SA's V11 foreign-pin-coincidence gate was scoped off
  (`gates_active = !mirror_eligible.is_empty()`) on the premise that
  all-passive fixtures' "V11 cleanliness is already maintained by the
  router" — false for pin-on-pin, which the router cannot touch;
* phase 4.5's `v11` term counts the router's `v11:` *wire* warnings, of
  which a pin-on-pin overlap produces none, and its `overlap` term uses
  strict body interiors, so two bodies whose pin tips abut read clean;
* phase 4.5's oracle routed against a **smaller obstacle set** than the
  real emit path (it omitted rail-glyph bodies), and had no model at all
  of rail-glyph *anchor pins*, which are live.

### Decisions

**D1. The two Tier-0 counts lead the lexicographic objective.** It is now
`(severed, coincident, v11, v13, v12, v5, bends)`. As leading keys they
subsume their old `<=` guards *and* become seekable. The previous comment
argued `severed` must stay a guard because "a reduction in `severed` could
outrank a Tier-1 V13 regression" — that inverts CLAUDE.md's ordering rule,
which is asymmetric: rule 1 forbids trading a *Tier-0 violation* away for
Tier-1/2 gain, and therefore mandates paying Tier 1 to recover Tier 0. The
Tier-1 guards (`overlap`, `v12`) are lifted only for a candidate that
strictly improves the Tier-0 prefix.

`v11` sits in the Tier-0 prefix, not among the lifted guards. Exempting it
for a `severed` repair is trading Tier 0 for Tier 0 — measured: the first
version of this change answered the two severed nets with a pose carrying
one unresolved `v11:` residue.

**Blast radius: nil by construction and by measurement.** When both Tier-0
counts are 0 on each side the comparison falls straight through to the old
tuple. All eleven graded fixtures measure `severed = coincident = v11 = 0`
at both baseline and final, and all eleven emit **byte-identical**
`.kicad_sch` before and after.

**D2. `coincident` measures what KiCad joins, including glyph pins.**
`schematic::tier0_short_count` counts (a) pin-on-pin across nets, host
pins *and* rail-glyph anchor pins, and (b) a wire touching a rail-glyph
anchor beyond its own stub. (b) needs no net attribution: rail nets are
drawn as glyphs, not trunks (V10), so an anchor's legitimate wire budget is
at most one incident end and never a crossing. Endpoints count, not just
interiors — the short that survived the interior-only version was the `c`
trunk *turning a corner* on `RC`'s `+12V` anchor.

`spice_route::rails::glyph_anchor` is exported for this: the anchor offset
rule is decoration geometry and must have one definition.

**D3. Phase 4.5's oracle routes against the real obstacle set.** It now
appends `rail_glyph_body_bboxes`, as `emit_root` does. ADR-16 rejected
*freezing* the oracle precisely to avoid optimising against a router that
does not exist; an oracle that sees fewer obstacles than the real one is
the same failure in a quieter form. Verified faithful afterwards: for the
final `shunt_feedback_amp` placement the trial route's 19 segments are
**identical** to the emitted wires modulo the page shift.

**D4. Two Tier-0 conditions are now refusals, not log lines.**

* *Pin-on-pin* (`EmitError::PinCoincidence`) — checked in `route_nets`
  before a wire is routed, unconditional, no external tool needed, and
  deliberately **not** behind `SPICE2KICAD_V11_STRICT` (that env-gate
  covered `v11:` wire residue). *Superseded by ADR-21*: the env-gate is
  gone and `v11:` residue is refused just as unconditionally — the
  "repairable" premise was wrong, and the gate meant `--no-verify`
  shipped the short at exit 0.
* *Severed net* (`EmitError::DisconnectedNet`) — `report_disconnected_nets`
  used to print `ERROR: net "c" is not fully connected` and then return
  `Ok`. Measured: a `shunt_feedback_amp` pose printed exactly that and
  **exited 0**. A converter that has already told you the circuit is wrong
  must not also tell the shell it succeeded.

**D5. The SA's V11 gate is unconditional.** The `mirror_eligible` scoping
was an optimisation resting on a false premise. Byte-identical output on
all eleven fixtures: a filter that only rejects coincidence-*increasing*
moves is inert wherever no such move is proposed.

### Root cause of the residual — R-5, and why it stops here

With D1–D3 in place the base/emitter short is gone (`severed` 2 → 0,
`coincident` 0), but `shunt_feedback_amp` still does not convert: one
`v11:` residue remains, the `c` trunk terminating on `RC`'s own `vcc` pin.
**All eight V14-allowed poses of `Q1` were enumerated against the real
router at this position; every one scores non-zero on at least one Tier-0
count.** Orientation is not a sufficient lever here — the lever is
position, and phase 4.5 owns orientation only.

The reason the position is unroutable is the deferred **R-5 rail-pin**
defect: `RC` and `RB` are placed with their rail pin facing *into* the
circuit (`RC` is rot 180, so its `vcc` pin is its lower pin), and the
`+12V` glyph therefore hangs **downward into the routing channel** the
collector and emitter trunks need.
`placement_quality::v14_rail_pin_faces_rail` measures exactly this and
flags the fixture (`#PWR3` below `RB`'s body centre).

R-5 is owner-gated: CLAUDE.md records that the R-5 fix "could not land
because it tripped a single fixture's Tier-1 ratchet", and the
global-improvement escape requires owner sign-off. **This ADR contributes
new evidence for that decision: on `shunt_feedback_amp` R-5 escalates from
a Tier-1 aesthetic defect to a Tier-0 correctness failure.** It is no
longer only about how the page reads.

Until that lands, `shunt_feedback_amp` stays a defect lock in
`crates/spice2kicad/tests/f0_defects.rs` — with a corrected diagnosis and
the same unexpected-pass tripwire — and the converter **refuses** rather
than shipping the wrong circuit.

### Measurement artifact worth recording

An early probe drove `Q1` through all eight poses via the ADR-4 layout
sidecar and reported `rot 0 + mirror-Y` as **clean** (exit 0). It is not:
that pose emits `v11: … 1 interior foreign-pin coincidence` and a severed
`c` net. The probe read the CLI's exit code, which at the time was 0 for a
severed net (see D4). MEMORY "verify what a number measures" again: the
control arm was the thing under test.

## ADR-21 — A Tier-0 refusal cannot be optional: the `v11:` wire-on-foreign-pin hole

**Status:** landed. Scope is deliberately narrow — one warning promoted to
an unconditional error, plus an audit of its siblings. No placer, router
or geometry change; `baseline_lock` diff is EMPTY and every graded
fixture's emitted `.kicad_sch` is byte-identical.

### The report

ADR-20 stated the principle — *the converter must refuse rather than emit
an electrically wrong schematic* — and implemented two refusals
(`PinCoincidence`, `DisconnectedNet`). One case of the same class was
still shipping at exit 0:

```
$ spice2kicad shunt_feedback_amp.cir -o out.kicad_sch --no-layout-cache --no-verify …
spice2kicad route: v11: net index 1 has 1 endpoint and 0 interior foreign-pin
                   coincidences left after active rerouting
spice2kicad route: obstacle: net index 1 has 2 segment(s) crossing a symbol body
                   after 6 outer passes
   -> exit=0, and out.kicad_sch shorts the collector net to `vcc`.
```

Without `--no-verify` the *optional* post-emit `kicad-cli` check caught it
and exited 1. That is precisely the situation ADR-20 D4 called
unacceptable, still true for a third case.

### Why it was a class gap, not an oversight

The three ways a routed sheet can be the wrong circuit are: pins merged,
nets merged, net severed.

| condition                         | ADR-20 status        |
| --------------------------------- | -------------------- |
| pin-on-pin across nets            | `PinCoincidence` ✔   |
| net severed (under-connected)     | `DisconnectedNet` ✔  |
| **nets merged by a wire endpoint on a foreign pin** | **warning** ✘ |

The third is exactly what `conflict::avoid_foreign_pins` reports as
`v11:`. In `kicad-emitter`'s `route_nets` it was escalated to
`EmitError::V11Violation` **only when `SPICE2KICAD_V11_STRICT` was set**.

### D1. The env-gate's stated justification was already stale

The gate's comment read: *"the env-gate keeps the existing single fixture
with a known placer-level pin overlap (`opamp_inverting_real`) emittable
for the V12/V13 verifier suite"*. Measured on the ADR-20 tree
(`a693648`), converting all twelve routable fixtures with `--no-verify
--no-layout-cache`:

* `opamp_inverting_real` emits **no** `v11:` warning — and no warning of
  any kind. The V14 power-pin-orientation fix removed its overlap long
  ago; `electrical_safety.rs::v11_pin_overlap_is_a_placer_bug` and
  `v11_violation_budget` already assert **zero on every fixture**.
* Exactly one fixture trips `v11:` — `shunt_feedback_amp`, an F0 *defect
  lock*, not a graded fixture.

So the gate protected nothing. What it actually did was let `--no-verify`,
and every machine without `kicad-cli`, ship a shorted schematic at exit 0.
The gate is deleted; there is no opt-out from a Tier-0 refusal.

### D2. "Repairable in principle" is not a reason to warn

The old `EmitError` doc justified the softer treatment by contrasting
`v11:` *wire* residue ("which the router can often detour") with
`PinCoincidence` ("no routing pass can move a pin"). That contrast does
not survive contact with where the warning is raised: the `v11:` tally in
`conflict::avoid_foreign_pins` is taken **at the cascade's fixed point** —
the loop above it exits on `!changed`, i.e. after every detour the router
can find has already been tried — and nothing downstream of `route_nets`
moves a wire. A `v11:` line is not a hint that repair is pending; it is
the router reporting that repair is *finished and failed*.

The correct axis is not repairability but *what the reader gets*: a wire
endpoint on a foreign net's pin is joined by KiCad on load, so the
emitted file is a different circuit. That is Tier 0 by CLAUDE.md's own
words — V11 "is a correctness invariant, not a quality one".

### D3. Refuse before writing, not after

The check sits in `route_nets`, which runs inside `emit_root` /
`emit_child_sheet` and therefore *before* any bytes reach disk. The
refused conversion leaves **no** `.kicad_sch` behind, so no later step can
pick up a file the converter has already judged wrong. The regression test
asserts this second property explicitly.

### Blast radius: measured nil

All eleven graded fixtures convert at exit 0 both before and after, and
their emitted `.kicad_sch` files are **byte-identical** (`diff -rq` over
two fresh output directories, `--no-layout-cache --no-verify`). This is
nil *by construction* as well: the new code path is unreachable unless the
router emits a `v11:` line, and no graded fixture emits one — a fact two
existing verifiers already ratchet at zero. No budget moved in either
direction; there was no slack to remove.

`shunt_feedback_amp` stays a defect lock (its residual is the owner-gated
R-5 rail-pin defect — ADR-20 § "Root cause"). Its lock now asserts the
*stronger* behaviour: the failure mode moved from "the optional
`kicad-cli` check catches it" to "the converter refuses unconditionally",
so the lock matches the router's own unwrapped `v11: net index` line
instead of the connectivity report's wrapped prose.

### Sibling holes — the audit

Every diagnostic `route_nets` prints, classified by tier and by whether it
can still reach exit 0.

| diagnostic | what it means geometrically | tier | disposition |
| --- | --- | --- | --- |
| `v11:` | wire endpoint/interior on a foreign net's pin | 0 | **now a hard error** |
| `conflict:` | wire endpoints of ≥ 2 distinct nets on one coord | 0 | **open** — see below |
| `cross-net overlap:` | segments of 2 nets collinearly overlap | 0 | **open, and not directly escalatable** — see below |
| `obstacle:` | V12 body crossing after the outer cap | 1 | warning is correct (budgeted fallback, per CLAUDE.md) |
| `v12-placer:` | own pin strictly inside a foreign body; V12 skipped | 1 | warning is correct |
| `rails:` | `power:*` lib_id missing → `(global_label)` fallback | 1 | warning is correct — the label still carries the net by name, so connectivity is preserved |
| `pwrflag:` | `power:PWR_FLAG` missing → net left undriven | 0 (V2) | warning, but it is a *library-configuration* failure and the emitted circuit is still the right circuit; ERC reports it. Left alone. |

**`conflict:` is a genuine open Tier-0 hole.** `find_conflicts` returns
every coordinate carrying wire endpoints from ≥ 2 distinct routed nets, and
KiCad joins wires that share an endpoint — so a surviving conflict is a
net merge. It is reported as a warning and reaches exit 0. It is *not*
observed on any of the thirteen fixtures, and since ADR-20 its main
generator is gone: `resolve_conflicts` declines to jog only when *every*
candidate at the point is a pin endpoint, i.e. pin-on-pin, which
`PinCoincidence` now rejects before routing. The residual generator is the
iteration cap being exhausted by an oscillating chain. It was left alone
here rather than escalated blind, because escalating a *string* is the
weaker instrument (see below) and there is no fixture to verify it
against.

**`cross-net overlap:` must NOT be escalated as a string.** Two nets'
segments overlapping collinearly is a merge (at least one endpoint of one
segment lies on the other). But the warning is emitted from
`deconflict_cross_net_overlaps`, which runs **before** `run_cleanup`, and
`spice-route`'s own comment records that this signal is "a pre-cleanup,
conservative signal, and `coalesce`/`collapse` routinely resolve pairs it
reported" — rolling back on it was measured to make `multivibrator` worse
(V5 4 → 6). Promoting it to an error would refuse conversions that are in
fact clean. The router already has the faithful post-cleanup predicate,
`first_cross_net_overlap`, but only consults it to decide whether to retry
with a suppressed stub, never to report.

Measured, on the one fixture that trips it. `two_stage_amp` (an F0 runtime
defect lock, committed but unregistered) emits **two** of these lines on
the real emit path — not just inside phase 4.5's trial routes:

```
route: cross-net overlap: nets 3/1 unresolved by single-track jog (channel router — v0.2)
route: cross-net overlap: nets 3/5 unresolved by single-track jog (channel router — v0.2)
```

and nonetheless converts at **exit 0 with `kicad-cli` verification on and
clean** (kicad-cli 9.0.2; the check compares the exported netlist against
the source and would report "net in the schematic but not the source" for
a merge). So on the only available evidence the warning is a false
positive for the property it looks like it is reporting. Escalating it
would have converted a correct conversion into a failure — the precise
outcome this work was told not to cause. It stays a warning, and the
finding is that the *signal*, not the tier, is what needs fixing.

**The structural finding.** `kicad-emitter` has an in-process geometric
check for **severance** (`disconnected_nets`, union-find over emitted
wires, now `DisconnectedNet`) and **no** in-process geometric check for
**merge**. Every merge refusal today is a string match on a router
warning, which means each new way to merge two nets needs its own string
and its own escalation. The class fix is the mirror of
`disconnected_nets`: union-find the emitted wires, absorb each pin, and
refuse when two distinct source nets land in one component. That would
subsume `v11:`, `conflict:` and `cross-net overlap:` at once, be
faithful to final post-cleanup geometry (which the last of those is not),
and be exactly the `kicad-cli` connectivity check the converter currently
outsources — without needing KiCad installed. It is deliberately **not**
built here: it is a new verifier with its own false-positive surface
(rail glyphs and name-jump labels carry connectivity without wires), and
this ADR's mandate was to close a hole without turning a good conversion
into a failure. It is the recommended next step.

*Built in **ADR-22**.* Both `v11:` and `DisconnectedNet` are gone,
replaced by the partition check; the feared false-positive surface was
handled by modelling KiCad's by-name rule rather than by skipping the
nets that need it, and `two_stage_amp` still converts at exit 0.

### Verification

* Twelve routable fixtures converted `--no-layout-cache --no-verify`
  before and after: eleven at exit 0 with byte-identical output,
  `shunt_feedback_amp` 0 → 1.
* New regression test
  `f0_defects.rs::v11_residue_is_refused_without_kicad_cli` asserts the
  **exit code** (`Some(1)`) under `--no-verify`, and that no
  `.kicad_sch` was written. It asserts no stderr substring: both the old
  and the new path print a `v11:` line, so stderr cannot distinguish them
  — and stderr substrings have already misled one author on this file
  (ADR-20's lock matched prose the CLI never printed unwrapped).

## ADR-22 — Refuse on the consequence, not the mechanism: the geometric net-partition certificate

**Status:** landed. No placer, router or geometry change: the eleven
graded fixtures plus the `OPAMP` child sheet emit **byte-identical**
`.kicad_sch` before and after, and `baseline_lock`'s diff is EMPTY.

**Design fork resolved by consultation, not by the implementer.** See
§ "The fork the implementer was not allowed to resolve alone" below.

### The report

ADR-21 closed one Tier-0 hole and, in closing it, wrote down the
structural finding that this ADR acts on:

> `kicad-emitter` has an in-process geometric check for **severance**
> (`disconnected_nets`) and **no** in-process geometric check for
> **merge**. Every merge refusal today is a string match on a router
> warning, which means each new way to merge two nets needs its own
> string and its own escalation.

That is not a missing feature. It is a *category error in where the gate
sits*. There are only three ways a routed sheet can be the wrong circuit
— pins merged, nets merged, net severed — but there are indefinitely
many *mechanisms* that produce them, and the emitter was recognising
mechanisms:

| mechanism | recognised by | could it reach exit 0? |
| --- | --- | --- |
| pin-on-pin across nets | `pin_coincidences`, geometric, pre-route | no |
| wire left on a foreign pin | **string match** on the router's `v11:` | no (since ADR-21) |
| wire endpoints of ≥ 2 nets on one coord | `conflict:` — **nothing** | **yes** |
| post-cleanup collinear overlap of 2 nets | `cross-net overlap:` — nothing, and *unescalatable* | **yes** |
| pin off its own net's wire graph | `disconnected_nets`, geometric | no |
| anything else | — | yes |

The bottom row is the point. A mechanism-shaped gate is only ever as
complete as the enumeration behind it, and the enumeration is a list
someone has to keep extending. Worse, two of the rows show the two ways
a *string* gate fails independently of completeness:

* **`conflict:` had no escalation** — not because anyone decided it was
  acceptable, but because writing one is a separate act that nobody had
  performed. ADR-21 audited it, confirmed it was Tier-0-shaped, and
  declined to escalate blind for want of a fixture to verify against.
  A hole nobody disagrees about stayed open for want of a string.
* **`cross-net overlap:` cannot be escalated at all.** It is emitted
  *before* `run_cleanup`, and `two_stage_amp` prints two of them while
  converting **correctly** (exit 0, `kicad-cli` clean). Escalating that
  string would turn a good conversion into a failure. A string tells you
  what a pass *observed at some intermediate moment*, which is not the
  same question as what the file *does*.

And the correct answer was being **outsourced**: the CLI's post-emit
`kicad-cli` connectivity check *is* the partition comparison, but it is
disabled by `--no-verify` and absent on any machine without KiCad.

### The decision

**D1. Refuse on the consequence.** `emit_root` and `emit_child_sheet`
now reconstruct the ENTIRE net partition from the ink they are about to
serialise and compare it against the source netlist's partition. A
mismatch — two source nets in one geometric component (a short), or one
source net in several (an open) — is `EmitError::NetPartition`,
unconditional, no external tool, no env var, no flag.

The reconstruction (`kicad_emitter::connectivity::check_partition`)
implements exactly KiCad's two join rules and nothing else:

1. **Geometric.** A wire joins its own endpoints; a pin, power-glyph
   anchor or label anchor lying on a wire — endpoint or strict interior —
   joins that wire; items sharing a coordinate are one connection point.
2. **By name.** Power symbols connect by their `Value`; labels connect by
   their text. `power:PWR_FLAG` is excluded (its Value is the literal
   string `PWR_FLAG`, not a net name); it still participates
   geometrically through the coordinate it shares with the rail pin it
   drives.

`(junction …)` items contribute no edges: a junction is only ever emitted
where three or more wire *endpoints* already meet, which rule 1 has
joined already.

**Why this is the durable fix and not merely a wider net.** It is blind
to mechanism by construction. A new router pass, a new decoration, a new
glyph flavour — none of them needs a new escalation, because none of them
can produce a wrong circuit without producing a wrong *partition*. The
`conflict:` hole is closed without anyone writing a `conflict:` handler,
which is the test of whether the fix is structural.

**D2. It runs before any bytes reach disk.** Same placement in the
pipeline as its predecessor: after routing, glyphs, labels and the
decoration text-nudges — so it measures the FINAL ink, never an
intermediate — and before `translate_into_page`. A refused conversion
leaves no `.kicad_sch` behind, matching ADR-21 D3. The regression test
asserts that property explicitly.

This placement is load-bearing and is exactly what makes the check
succeed where `cross-net overlap:` failed. That warning is a
*pre-cleanup* sample; `coalesce`/`collapse` routinely resolve the pairs
it reports. Measured: `two_stage_amp` emits two of those lines and
converts at exit 0 with this check active — so the check agrees with
`kicad-cli` and disagrees with the string, which is the correct
disagreement.

**D3. Two mechanism checks are deleted, one is kept.**

* `EmitError::V11Violation` — **deleted**, with the `v11:` string
  escalation in `route_nets`. `shunt_feedback_amp` is still refused, and
  now with a far better diagnosis: instead of "1 unresolved foreign-pin
  coincidence(s)" it reports `MERGE: source nets ["c", "vcc"] share one
  geometric component`, naming the six terminals involved — which is
  precisely ADR-20's hand-derived root cause, now printed by the tool.
* `EmitError::DisconnectedNet` and `disconnected_nets` — **deleted**. The
  partition check subsumes it *and strictly strengthens it*. The old one
  was documented as "deliberately conservative": it skipped Power/Ground
  nets (they carry no wires by design) and skipped any net with two
  same-name labels (labels join islands by name, which a coordinate-only
  walk cannot see). Both skips existed because it modelled only rule 1.
  Modelling rule 2 removes the need for either, so a *broken rail* — a
  ground pin whose glyph never got emitted — is now caught, where before
  it was skipped by policy. A unit test pins exactly that.
* `EmitError::PinCoincidence` — **kept**, and demoted in the docs from a
  correctness authority to a pre-flight. It is fully subsumed (pin-on-pin
  is a merge like any other) but it is raised *before* the router runs,
  so it names the two coincident pins and their nets rather than the
  component they end up in, and it does not spend a routing pass on a
  placement already known to be wrong.

**D4. `--no-verify` now means what it says.** Its help text claimed the
`kicad-cli` check "is the only thing standing between a modelling bug and
a silently wrong circuit". That was true when written and is now false:
the in-process check has no opt-out. The text is rewritten to say what
`kicad-cli` still uniquely buys — an *independent* opinion, KiCad's own
connectivity engine rather than our model of it, which is the only thing
that can catch the model itself being wrong.

### The fork the implementer was not allowed to resolve alone

`roundtrip_connectivity.rs` (the A2 verifier) already implemented this
algorithm test-side. The obvious move — share one implementation — has a
known failure mode in this repo: **a test that is a byte-copy of the
thing it grades cannot falsify it.** The V13 label-overlap suite was a
byte-copy of the emitter's own text-geometry model, and only real
`kicad-cli` SVG ink could falsify it (MEMORY "verify text geometry
against SVG"). The alternative — two independent implementations — buys
falsification and pays in drift, and drift in a Tier-0 check is itself a
hazard.

The fork was referred to an outside architect rather than settled by the
implementer. The recommendation, adopted:

> Decompose the check by *epistemics*, not by module. It has three
> separable parts and they do not have the same falsifiability:
> **(1) pin→net attribution and pin world coordinates**, **(2) the
> connectivity-interpretation model**, **(3) late-pipeline fidelity**
> (page translation, serialisation, bytes on disk). Share the part where
> duplication buys nothing; keep independent the parts where independence
> is the whole point; delegate the part that is unfalsifiable in-repo to
> the external oracle.

The decisive observation was on axis 1: `collect_net_pins` is the same
map that feeds the **router**. The production check therefore grades the
router's output against the router's own input — shared fate. If that
attribution or the pose maths behind it is wrong, the router draws to the
wrong pin *and the check blesses it*. The production check is
structurally blind there, permanently, and no amount of care fixes it.

On axis 2 the recommendation cut the other way and is worth recording
because it is counter-intuitive: **two in-repo implementations give no
independence on the interpretation model.** Both authors encode the same
beliefs about KiCad's semantics; if a belief is wrong, both agree and
both are wrong — the V13 failure exactly. The only falsifier of the model
is KiCad. So a second hand-written union-find would buy coverage of
typos in the easiest part of the code and nothing else.

As built:

* **Production** owns the reconstruction, graded against
  `collect_net_pins`, hard error in both sheet emitters.
* **A2 keeps its independent inputs and shares only the engine.** It
  re-parses the `.cir` through `spice-parser`/`spice-resolve`, re-derives
  every terminal's world coordinate from the library through the
  *emitted* pose, and reads the geometry back off the written **file**
  with a different parser (`lexpr`). That covers axes 1 and 3 — the two
  the production check cannot cover itself. A comment in the file states
  why, because the 25-minute suite will one day tempt someone to delete
  the derivation "since production checks it now".
* **Axis 2's falsifier already exists and is already mandatory here**: the
  CLI runs `kicad-cli`'s netlist comparison on every graded conversion in
  the suite (`kicad-cli` 9.0.2 on this machine), so the model is
  differentially graded against KiCad on all eleven fixtures every run.
  No new differential test was added; one would have duplicated it.
* **Vacuity and mutation guards.** A2 asserts it derived ≥ 2 terminals
  per fixture (a silently-empty derivation would turn the file green
  while measuring nothing), and `the_reconstruction_is_sensitive_on_real_fixtures`
  injects, into each real fixture's real reconstruction, one defect per
  edge class the model implements: a wire between two foreign nets' pins,
  two same-named anchors on foreign nets, and erasure of all geometry.
  Each must be caught. An unfired guard is worth nothing.

### Blast radius: measured nil

* Twelve routable fixtures converted `--no-layout-cache --no-verify` on
  `b7b59ca` and on this tree: eleven at exit 0 with **byte-identical**
  output (`diff -rq` over two fresh directories — 12 files including the
  `OPAMP` child sheet), `shunt_feedback_amp` refused on both.
* `two_stage_amp` — the fixture that killed naive string escalation —
  still converts at **exit 0** with the check active (measured, 2 min
  wall on this tree), while still printing its two `cross-net overlap:`
  lines. This is the specific breakage this work was told not to cause,
  and it did not occur.
* Nil *by construction* as well: the new code reads `items` and returns;
  nothing in the emit path can observe it except by not running.
* No budget moved in either direction. There was no slack to remove: this
  is a categorical gate, budget 0 everywhere, like V11 itself.

`shunt_feedback_amp` stays a defect lock — its residual is the
owner-gated R-5 rail-pin defect (ADR-20 § "Root cause"), untouched here.
Its lock is updated because the failure *mode* changed: it now asserts
the partition finding (`MERGE: source nets ["c", "vcc"]`, one unwrapped
line, deterministic order from a `BTreeSet`) instead of the router's
`v11:` warning.

### What is still warning-tier — the re-audit

ADR-21's sibling-holes table, re-derived against this change:

| diagnostic | tier | disposition now |
| --- | --- | --- |
| `v11:` | 0 | warning again — and correctly so. Its *consequence* is refused by the partition check; the line survives as the human-readable explanation of why. |
| `conflict:` | 0 | **closed, without a handler.** Wire endpoints of two nets on one coordinate put both nets' wires in one component. |
| `cross-net overlap:` | 0 | **closed, without a handler, and without refusing `two_stage_amp`.** The check measures post-cleanup ink, which is the state the warning could not see. |
| `obstacle:` / `v12-placer:` | 1 | warning is correct (V12, budgeted fallback by design) |
| `rails:` | 1 | warning is correct — the `(global_label)` fallback still carries the net by name, and the partition check now *proves* that on every conversion rather than asserting it in a comment |
| `pwrflag:` | 0 (V2) | warning. A missing `power:PWR_FLAG` library entry leaves a net undriven, but the emitted circuit is still the *right* circuit — the partition is unaffected — and ERC reports it. Unchanged. |

**No warning-tier Tier-0 path survives** in `route_nets`: every row above
that can make the file a different circuit is now caught by what it does
to the partition. The one remaining Tier-0-tagged warning (`pwrflag:`) is
a library-configuration failure that does not change connectivity.

### The finding worth carrying forward

The generalisable lesson is not "add a partition check". It is: **a gate
that names a mechanism inherits the completeness of an enumeration; a
gate that names a consequence does not.** Three of this project's Tier-0
holes (ADR-20's severed net, ADR-21's `v11:`, ADR-22's `conflict:`) were
each found by someone noticing a specific mechanism had no gate. That
search does not terminate. Ask instead what property must hold of the
*output*, and measure it there.

The corollary, from the fork: when you move a verifier into production,
ask which of its inputs the production copy will *share with the thing it
grades*. Those are the axes on which it has just gone blind, and they are
what the test must keep deriving independently.

## ADR-23 — Two instruments: the ratchets detect drift, the scoreboard selects an architecture

**Status:** landed, and **exercised**. The seam, the sink, the
aggregator and the promotion rule went in additively — with no
`--placer` flag and no scoreboard invocation, emitted output was
byte-identical on all thirteen fixtures and `baseline_lock`'s diff was
EMPTY. On **2026-08-18** the rule was used for the first time:
`--placer=flow-seed` was graded PROMOTABLE on a fresh table and
**promoted to the default** on owner approval, regenerating
`baseline_lock` and every per-fixture literal at its geometry.
`champion` remains registered as the control arm. See § "The promotion"
below for the fresh table, the two Tier-1 regressions it carries, and
the D2 gap it exposed.

### The report

`git diff 0ccf3f0 HEAD -- crates/spice2kicad/tests/baseline_lock.rs`
removes **0 rows**. Twenty-seven commits, sixteen days, five attempted
behaviour changes (ADR-19 M3, M4, M5′, roadmap B2, B3) — net emitted
geometry change: **zero**. Every landed behaviour change was
byte-identical by construction; every one that moved a symbol was
reverted.

That is not timidity. The suite records ~165 zero-slack per-fixture
scalars plus a ~120-row exact-coordinate `baseline_lock`, and **every one
of those literals was obtained by measuring the incumbent placer's own
output** on eleven hand-tuned circuits. Against that reference,
"regression" and "difference" are the same measurement. Pareto
non-regression across ~165 correlated scalars of a globally-coupled,
chaotic map is achievable essentially only by a no-op — which is exactly
the observed history.

The ratchets are not the bug. They are the project's genuine moat for
the *shipping* path, and this ADR does not weaken them. The bug is that
one instrument was being asked two different questions:

| question | instrument | shape |
| --- | --- | --- |
| Did this change break what we shipped? | per-fixture zero-slack ratchets + `baseline_lock` | conjunctive, per fixture, no slack |
| Is placer B better than placer A? | **the scoreboard** (this ADR) | aggregate, tier-ordered, sideways trades allowed *within* a tier |

The second question *must* permit sideways trades: two different placers
produce two different global optima and neither dominates on ~165
correlated scalars. Refusing sideways trades there is not rigour, it is a
guarantee that no architecture can ever be selected.

### D1. The seam: `--placer=<name>`, champion by default

`spice_layout::Placer` (`crates/spice-layout/src/placer.rs`) is a name
registry; `LayoutOptions::placer` carries the selection; `place_seed` and
`pack_rows` take it, and `refinement_meta` takes it too — it *re-runs the
seed* to reproduce phase 4.5's pin mask, so a seam that missed it would
silently desynchronise the refiner from the placer.

`Placer::Champion` is `Default`, and the CLI's `--placer` defaults to
`champion`, so the flagless path is the incumbent bit-for-bit.
**Verified by byte-diff, not by inference:** the pristine `2d3c81b`
binary and the seamed binary were run over all thirteen fixtures
(`--no-layout-cache --no-verify`, fresh output directories each) and
`diff -rq` reports no difference — including the two F0 defect locks,
which refuse identically. `baseline_lock` is unchanged.

An unregistered name is a hard CLI error listing the registry, never a
silent fall-back to the champion. A non-default placer prints a stderr
banner saying it is not the shipping placer.

### D2. The measurement sink, and why the scoreboard is not a measuring binary

Every verifier's metric function is a private `fn` in its own
integration-test **binary**, and Rust integration tests cannot import one
another. A separate measuring binary could therefore only (a) move ~2 kLOC
of measurement code into `tests/common/`, or (b) re-implement it.

(b) is duplication, and duplication of a measurement is the specific
failure this project keeps paying for (MEMORY "verify what a number
measures"): the scoreboard would silently drift from the verifier it
claims to mirror. (a) also **doubles the runtime** — conversion is the
dominant cost and is completely unmemoized, so a measuring binary
re-converts all eleven fixtures for every metric it computes.

So the measurement stays where the assertion is. `common::scoreboard::record`
is a no-op unless `S2K_SCOREBOARD_DIR` is set; each verifier reports the
number it *already computed*, on the line before the assertion that
grades it. There is exactly one definition of every metric and it is the
one the ratchet asserts on. The scoreboard binary
(`crates/spice2kicad/tests/scoreboard.rs`) is a pure aggregator over
those records, `#[ignore]`d so it never runs in the default `cargo test`
path.

Collecting a placer's row is therefore *the suite itself*, run with the
sink on and `S2K_PLACER=<name>`:

```sh
just scoreboard-run champion
just scoreboard-run m4-ydatum
just scoreboard champion m4-ydatum
```

A challenger run is **expected to be red** — every zero-slack ratchet is
calibrated on the champion's output. `--no-fail-fast` is what keeps the
measurements complete anyway. Four verifiers that asserted *inside* their
fixture loop were converted to collect-then-assert (`crossings`,
`detour`, `v12`, symbol-overlap, `v11_pin_overlap`): same assertions, all
of them reported. That is independently the ADR-19 M4 "gate-set lesson" —
a truncated failure list reads as a shorter failure than it is.

### D3. Coverage, and the tier-weighted aggregate

Metrics recorded (34): Tier 0 — conversion refusal, the ADR-22 net-partition
certificate, placer-level pin-on-pin, V11 wire/label-on-foreign-pin,
symbol/symbol overlap. Tier 1 — V12, ten model-side V13 families, the real
`kicad-cli` SVG-ink V13, V14 rail-pin (R-5) and V14 glyph-over-body
(issue [3]). Tier 2 — V5, V16 (B and J), crossings, wire detour, Q3, Q5,
F3/F4/F5/P5/F6, P11b locality. Informational — Q6 CoV (the project's own
record says it is not a ratchet, so it is printed and excluded).

**The aggregate is lexicographic `(T1, T2)`, not a weighted blend**, because
that *is* CLAUDE.md's ordering rule ("never introduce a Tier-1 regression to
improve a Tier-2 metric") lifted from per-fixture to aggregate. A single
scalar `S = 1000·T1 + T2` is printed for readability together with the
proof obligation that makes it faithful — `|T2| < 1000` — checked and
printed rather than assumed.

Within a tier, one violation is one point. That is the only non-arbitrary
unit for a quantity whose ideal is zero. Two metrics are not counts and
their units are a *choice*, which the report must be read against:

* **wire detour** is a ratio; one point = one percentage point of excess
  wire;
* **F6** is a *distance* in grid cells, deliberately (a count would hide a
  stub drifting from 2 cells to 12), so one point = one cell.

Tier 0 is **not** aggregated. It stays per-fixture hard for champion and
challenger alike — inviolable, and cheap to satisfy, since every fixture
measures 0 today.

### D4. The promotion rule

A challenger is **promotable** when, against the champion:

1. **no Tier-0 regression** — every Tier-0 metric is 0 on every fixture
   (per-fixture hard, never traded, never aggregated); and
2. the aggregate **`(T1, T2)` strictly improves lexicographically** —
   `T1 < 0`, or `T1 == 0` and `T2 < 0`; and
3. the comparison is **complete** — no metric cell present on one side
   only, and no registered metric missing its champion measurement (a
   verifier that aborted before recording anything would otherwise read
   as "no change").

On promotion, `baseline_lock` and **every** per-fixture literal are
regenerated at the challenger's values, and the zero-slack regime resumes
immediately against the new champion. There is no intermediate state in
which both placers are graded.

**The scoreboard does not grant the exception; it supplies the evidence.**
The report prints the verdict and exits green either way. Promotion
remains an owner decision on the printed table, exactly as CLAUDE.md's
global-improvement escape requires.

**Scope, stated so it cannot be borrowed.** This applies to
**whole-placer comparisons only** — a registered `--placer` variant
graded end-to-end. It is *not* available to an ordinary change. A commit
that edits `cost.rs`, adds a router pass, or tweaks a constant is still
governed by the per-fixture zero-slack ratchets and may not cite an
aggregate improvement to raise a budget. The distinction is not
sentiment: a whole placer is a different global optimum, where sideways
trades are structural; an ordinary change is a perturbation of the *same*
optimum, where a sideways trade is just a regression with an excuse.

### D5. Validating the instrument against history — the ADR-19 M4 replay

An instrument that has never been run against a known answer is not an
instrument. ADR-19 M4 (the content-derived, `n`-independent Y datum) is
registered as `--placer=m4-ydatum` — the code from `ed51164`, preserved
on `archive/adr19-m4-pending-m3`, re-applied as a *challenger*, dead on the
default path.

Measured, whole suite each side, one machine, `--no-fail-fast`:

| metric | tier | fixture | champion | m4-ydatum | Δ points |
| --- | --- | --- | ---: | ---: | ---: |
| `v13.4_text_mutual` | 1 | rc_phase_shift | 0 | 1 | **+1.00** |
| `v13.6a_glyphtext` | 1 | rc_phase_shift | 1 | 0 | −1.00 |
| `v13.ink_overlap` (real SVG ink) | 1 | rc_phase_shift | 1 | 0 | −1.00 |
| **Tier 1 total** | | | | | **−1.00** |
| `v5` | 2 | rc_phase_shift | 5 | 3 | −2.00 |
| `v16.bends` | 2 | named_rails | 2 | 1 | −1.00 |
| `v16.bends` | 2 | rc_phase_shift | 19 | 17 | −2.00 |
| `v16.branches` | 2 | rc_phase_shift | 3 | 4 | +1.00 |
| `crossings` | 2 | rc_phase_shift | 2 | 1 | −1.00 |
| `detour` | 2 | common_emitter | 1.0135 | 1.0091 | −0.44 |
| `detour` | 2 | diff_pair | 1.0556 | 1.0208 | −3.47 |
| `detour` | 2 | multivibrator | 1.0481 | 1.0240 | −2.40 |
| `detour` | 2 | named_rails | 1.1250 | 1.0345 | −9.05 |
| `detour` | 2 | rc_phase_shift | 1.2313 | 1.1800 | −5.13 |
| `q3` | 2 | rc_phase_shift | 4 | 3 | −1.00 |
| `q5` | 2 | common_emitter | 3 | 4 | +1.00 |
| `q5` | 2 | rc_phase_shift | 3 | 4 | +1.00 |
| `f6` | 2 | multivibrator | 2 | **18** | **+16.00** |
| `f6` | 2 | named_rails | 6 | 5 | −1.00 |
| `f6` | 2 | rc_phase_shift | 23 | 27 | +4.00 |
| `p11b.movers` | 2 | common_emitter+CB | 8 | 7 | −1.00 |
| **Tier 2 total** | | | | | **−6.50** |

Every other cell — 34 metrics × 11 fixtures — is identical on both sides.
Tier 0 is clean on both. Coverage is 11/11 on both sides for every
per-fixture metric.

**Verdict: `m4-ydatum` is PROMOTABLE** (`T1 = −1.00`, `T2 = −6.50`,
`S = −1006.50`), and this is a real result, not a tuned one — the
weighting is fixed by the tier order before the measurement, and the two
non-count units are stated above.

**What the replay actually establishes, stated carefully.**

1. **The instrument reproduces the known answer.** ADR-19's M4 post-mortem
   records the fatal defect as `flow_geometry::stub_lateral_run_within_ratchet`,
   `multivibrator` F6 budget 2, measured 18. The scoreboard reports exactly
   `f6 / multivibrator: 2 → 18`, from an independent run. It also reproduces
   M4's own commit-message claims — `named_rails` V16 B 2→1 and P11b
   `common_emitter` 8→7 — which that commit lowered its literals for. The
   instrument is measuring what it says it measures.
2. **The ratchets were not "wrong" about M4.** M4 *does* regress a Tier-2
   ratchet, badly, on one fixture. Under the per-fixture rule that is a
   revert, and the revert was correct *for a change presented as an
   ordinary commit*. What the scoreboard adds is the information the
   per-fixture rule structurally cannot carry: that the same change also
   removes a Tier-1 defect and improves wire detour on five of eleven
   fixtures, V5 and Q3 and crossings on the hardest fixture, and locality
   (the thing ADR-19 exists to fix) on the fixture ADR-19 measures. Both
   readings are true. They answer different questions.
3. **The T1 verdict is thin and the T2 verdict is unit-sensitive. Say so.**
   T1 = −1.00 rests on a single fixture (`rc_phase_shift`) and on treating
   `v13.6a_glyphtext` and `v13.ink_overlap` as two defects; they are
   plausibly two *views* of one crowded region, in which case the honest
   T1 is 0 and T2 decides. T2 = −6.50 is the residue of two large opposing
   terms (detour −20.50, F6 +19.00), so it flips under a different F6 or
   detour unit. **A promotable verdict this close is a reason to look at
   the table, not to skip it.** The scalar's job is to focus attention,
   not to replace judgement — which is why the promotion rule ends at the
   owner and not at a green test.
4. **The general finding for the roadmap.** The negative results recorded
   against M3/M4/M5′/B2/B3 were obtained under the per-fixture rule. At
   least one of them (M4) does *not* survive re-measurement under the
   architecture-selection rule. The other four have not been replayed and
   this ADR claims nothing about them; replaying them is now cheap
   (register a `--placer` variant, run two suites) and is the recommended
   next use of the instrument.

**M4 is NOT landed by this ADR.** It is registered as a graded challenger
and remains off the default path. Promoting it means accepting the
`multivibrator` F6 regression as the cost of the rest, regenerating
`baseline_lock` and every literal at its values, and — per ADR-19's own
post-mortem — doing so with `flow_geometry` in the gate set. That is an
owner decision, and the table above is what it should be taken on.

### Known limits of the instrument

* **It grades what the verifiers measure.** A property no verifier
  measures is invisible to the aggregate exactly as it is invisible to
  the ratchets. Coverage is printed per metric so the blind spots are
  visible; V1, V2 (ERC), V15 and the netlist-equivalence suite are
  currently pass/fail only and contribute nothing.
* **A cell absent from *both* sides is silent.** The report flags
  one-sided cells and fully-uninstrumented metrics, but two runs that
  aborted at the same place would agree vacuously.
* ~~**`f0_defects`, `layout_cache`, `symbol_mapping` and `spec_version`
  keep their own conversion drivers** and are not placer-aware, so a
  challenger run leaves them on the champion.~~ **CLOSED for the two
  that grade geometry** — see "D6" below. `symbol_mapping` (V8 symbol
  *selection*: a resolver/emitter decision with no placement content,
  and mostly `#[ignore]`d pending the resolver override) and
  `spec_version` (CLI parse-time version handshake, driven with
  `-t netlist`, which never reaches the placer) remain deliberately
  placer-blind: forwarding the flag to them would add a code path
  without adding a measurement.
* **Aggregation hides which fixture paid.** Always read the table; the
  scalar exists to focus attention, not to replace it.

### D6. Replaying the other four reverts

D5 replayed M4 and closed with "replaying the other four is now cheap and
is the recommended next use of the instrument". This is that replay.
Same protocol: register a `--placer` variant, dead on the default path,
collect a whole-suite row with the sink on, aggregate. Nothing here is
promoted; no baseline was regenerated and no budget literal moved.

**First, the instrument re-validated.** The M4 row was re-collected from
scratch on this tree and reproduces D5 exactly — `T1 = −1.00`,
`T2 = −6.50`, `S = −1006.50`, `f6 / multivibrator 2 → 18`. Two
independent runs, same answer.

#### Recovery status

| replay | recovered from | status |
| --- | --- | --- |
| M3 ablation **B** (pure signed gate) | `archive/adr19-m3-signed-gate` `7896f22`, verbatim | registered `m3-signed-gate` |
| M3 **full** (B + property text + signed `legalize` roomy) | same commit, whole tree | registered `m3-signed-full` |
| M5′ (per-refdes SA streams) | **not recoverable** | registered `m5-streams` as a *re-derivation* |
| B2 (feedback-arc marking) | **not recoverable** | **structurally inert — not registered** |
| B3 (DC-path direction into layering) | **not recoverable** | **skipped** |

M5′, B2 and B3 were each measured in a working tree and reverted without
ever being committed: the reflog holds only their docs commits
(`241599e` for M5′, `619cc31` for B2/B3, docs-only, 45 lines of
`v0.2-roadmap.md`). Only B1's `flow.rs` was ever committed, and that is
the *unconsumed* model, not B2/B3's consumers.

#### M3 ablation B — `m3-signed-gate`

| metric | tier | fixture | champion | challenger | Δ |
| --- | --- | --- | ---: | ---: | ---: |
| `v13.1_label_body` | 1 | named_rails | 0 | 1 | **+1.00** |
| `v13.7_label_pintext` | 1 | named_rails | 0 | 1 | **+1.00** |
| `v13.ink_overlap` | 1 | common_emitter | 0 | 1 | **+1.00** |
| **Tier 1 total** | | | | | **+3.00** |
| `v5` | 2 | named_rails | 2 | 1 | −1.00 |
| `v5` | 2 | rc_phase_shift | 5 | 6 | +1.00 |
| `v16.bends` | 2 | common_emitter | 4 | 7 | +3.00 |
| `v16.bends` | 2 | named_rails | 2 | 1 | −1.00 |
| `v16.bends` | 2 | rc_phase_shift | 19 | 14 | −5.00 |
| `v16.branches` | 2 | rc_phase_shift | 3 | 5 | +2.00 |
| `crossings` | 2 | rc_phase_shift | 2 | 5 | +3.00 |
| `detour` | 2 | common_emitter | 1.0135 | 1.0253 | +1.18 |
| `detour` | 2 | named_rails | 1.1250 | 1.1111 | −1.39 |
| `detour` | 2 | rc_phase_shift | 1.2313 | 1.1450 | −8.63 |
| `q3` | 2 | common_emitter | 1 | 3 | +2.00 |
| `q3` | 2 | named_rails | 1 | 0 | −1.00 |
| `q5` | 2 | common_emitter | 3 | 2 | −1.00 |
| `q5` | 2 | named_rails | 2 | 4 | +2.00 |
| `f5` | 2 | rc_phase_shift | 1 | 0 | −1.00 |
| `f6` | 2 | common_emitter | 4 | 5 | +1.00 |
| `f6` | 2 | named_rails | 6 | 3 | −3.00 |
| `f6` | 2 | rc_phase_shift | 23 | 14 | −9.00 |
| `p11b.movers` | 2 | common_emitter+C | 8 | 7 | −1.00 |
| **Tier 2 total** | | | | | **−17.84** |

**Verdict: NOT PROMOTABLE** (`T1 = +3.00`; `S = +2982.16`). Twice over:
a clear Tier-1 regression, *and* the comparison is **incomplete** — the
challenger has no `v13.1_label_body / rc_phase_shift` cell, because that
verifier still asserts inside its fixture loop and stopped at
`named_rails`. D2 converted four such verifiers; this is a fifth that was
missed. The missing cell cannot rescue the verdict (it can only add to
T1), but it is a live instrument defect worth closing.

**This is the one unambiguous result of the whole replay.** M3-B is
worse, on the tier that leads, and the original revert was right.

#### M3 full wiring — `m3-signed-full`

| metric | tier | fixture | champion | challenger | Δ |
| --- | --- | --- | ---: | ---: | ---: |
| `v13.ink_overlap` | 1 | rc_phase_shift | 1 | 0 | −1.00 |
| **Tier 1 total** | | | | | **−1.00** |
| `v5` | 2 | common_emitter | 1 | 0 | −1.00 |
| `v5` | 2 | named_rails | 2 | 1 | −1.00 |
| `v5` | 2 | rc_phase_shift | 5 | 4 | −1.00 |
| `v16.bends` | 2 | common_emitter | 4 | 5 | +1.00 |
| `v16.bends` | 2 | named_rails | 2 | 1 | −1.00 |
| `v16.bends` | 2 | opamp_inverting | 3 | 5 | +2.00 |
| `v16.bends` | 2 | opamp_inverting_real | 5 | 9 | +4.00 |
| `v16.bends` | 2 | rc_phase_shift | 19 | 17 | −2.00 |
| `v16.branches` | 2 | named_rails | 2 | 1 | −1.00 |
| `v16.branches` | 2 | opamp_inverting_real | 1 | 0 | −1.00 |
| `crossings` | 2 | opamp_inverting | 0 | 1 | +1.00 |
| `crossings` | 2 | rc_phase_shift | 2 | 1 | −1.00 |
| `detour` | 2 | common_emitter | 1.0135 | 1.0476 | +3.41 |
| `detour` | 2 | named_rails | 1.1250 | 1.1000 | −2.50 |
| `detour` | 2 | opamp_inverting | 1.1111 | 1.1000 | −1.11 |
| `detour` | 2 | opamp_inverting_real | 1.1951 | 1.3243 | **+12.92** |
| `detour` | 2 | rc_phase_shift | 1.2313 | 1.2576 | +2.62 |
| `q3` | 2 | common_emitter | 1 | 2 | +1.00 |
| `q5` | 2 | common_emitter | 3 | 6 | +3.00 |
| `q5` | 2 | named_rails | 2 | 0 | −2.00 |
| `q5` | 2 | opamp_inverting_real | 0 | 2 | +2.00 |
| `q5` | 2 | rc_phase_shift | 3 | 4 | +1.00 |
| `f6` | 2 | common_emitter | 4 | 6 | +2.00 |
| `f6` | 2 | named_rails | 6 | 4 | −2.00 |
| `f6` | 2 | rc_phase_shift | 23 | 21 | −2.00 |
| `p11b.movers` | 2 | common_emitter+C | 8 | 6 | −2.00 |
| **Tier 2 total** | | | | | **+15.34** |

**Verdict as the rule gives it: PROMOTABLE** (`T1 = −1.00`,
`T2 = +15.34`, `S = −984.66`). **Do not act on it.** Under strict
lexicographic ordering a single Tier-1 point outranks any Tier-2 sum, so
this passes while being *visibly worse* on Tier 2 — `opamp_inverting_real`
detour 1.195 → 1.324 and bends 5 → 9 are exactly the kind of defect the
project reverts on sight. This is the rule behaving as specified, not a
bug, and D4's "the scoreboard supplies the evidence, the owner decides"
is what stands between it and a bad promotion.

Note also that ADR-19's ablation table has **inverted**: it recorded
`full` as strictly worse than `B` ("the roomy swap is strictly worse, not
neutral"). Aggregated, `full` is the better of the two by a tier. Both
readings are measurements of different things — that table counted *red
verifiers*, this one counts *points against the champion*.

#### M5′ re-derivation — `m5-streams`

Not the original code. The re-derivation is: one `Rng` per movable
element seeded `opts.seed ^ fnv1a(refdes)`, a deterministic sweep in
refdes order, proposals *and* the Metropolis draw taken from the swept
element's own stream, same move mix as `propose_move`.

It **reproduces ADR-19's recorded signature qualitatively**: bends rise
on exactly the four fixtures M5′ named, with `opamp_inverting` 3 → 5
matching exactly, `opamp_inverting_real` 5 → 7 (recorded 5 → 10),
`common_emitter` 4 → 8 (recorded 4 → 11), `named_rails` 2 → 3 (recorded
1 → 2, against a champion that has since drifted). Same phenomenon,
different magnitudes — treat the aggregate as *an* M5′, not *the* M5′.

| metric | tier | fixture | champion | challenger | Δ |
| --- | --- | --- | ---: | ---: | ---: |
| `v13.4_text_mutual` | 1 | rc_phase_shift | 0 | 1 | +1.00 |
| `v13.6a_glyphtext` | 1 | rc_phase_shift | 1 | 0 | −1.00 |
| `v13.ink_overlap` | 1 | rc_phase_shift | 1 | 0 | −1.00 |
| **Tier 1 total** | | | | | **−1.00** |
| `v5` | 2 | named_rails | 2 | 1 | −1.00 |
| `v5` | 2 | opamp_inverting | 1 | 0 | −1.00 |
| `v5` | 2 | rc_phase_shift | 5 | 3 | −2.00 |
| `v16.bends` | 2 | common_emitter | 4 | 8 | +4.00 |
| `v16.bends` | 2 | named_rails | 2 | 3 | +1.00 |
| `v16.bends` | 2 | opamp_inverting | 3 | 5 | +2.00 |
| `v16.bends` | 2 | opamp_inverting_real | 5 | 7 | +2.00 |
| `v16.bends` | 2 | rc_phase_shift | 19 | 14 | −5.00 |
| `v16.branches` | 2 | common_emitter | 3 | 5 | +2.00 |
| `v16.branches` | 2 | opamp_inverting | 0 | 1 | +1.00 |
| `crossings` | 2 | opamp_inverting_real | 0 | 1 | +1.00 |
| `crossings` | 2 | rc_phase_shift | 2 | 3 | +1.00 |
| `detour` | 2 | common_emitter | 1.0135 | 1.1277 | **+11.41** |
| `detour` | 2 | named_rails | 1.1250 | 1.1500 | +2.50 |
| `detour` | 2 | opamp_inverting | 1.1111 | 1.0833 | −2.78 |
| `detour` | 2 | opamp_inverting_real | 1.1951 | 1.1250 | −7.01 |
| `detour` | 2 | rc_phase_shift | 1.2313 | 1.2882 | +5.69 |
| `q3` | 2 | common_emitter | 1 | 3 | +2.00 |
| `q3` | 2 | named_rails | 1 | 0 | −1.00 |
| `q3` | 2 | rc_phase_shift | 4 | 3 | −1.00 |
| `q5` | 2 | common_emitter | 3 | 0 | −3.00 |
| `q5` | 2 | named_rails | 2 | 3 | +1.00 |
| `q5` | 2 | opamp_inverting | 1 | 0 | −1.00 |
| `q5` | 2 | opamp_inverting_real | 0 | 1 | +1.00 |
| `f5` | 2 | common_emitter | 1 | 0 | −1.00 |
| `f6` | 2 | common_emitter | 4 | 11 | **+7.00** |
| `f6` | 2 | named_rails | 6 | 8 | +2.00 |
| `f6` | 2 | rc_phase_shift | 23 | 24 | +1.00 |
| **Tier 2 total** | | | | | **+21.81** |

**Verdict as the rule gives it: PROMOTABLE** (`T1 = −1.00`,
`T2 = +21.81`, `S = −978.19`). **Do not act on it either**, and for a
sharper reason than M3-full: ADR-19 killed M5′ on the finding that
re-keying the SA destroys its basin-finding, and this table *confirms
that finding* — bends up on four of five affected fixtures, F6 up 10
points, `common_emitter` detour +11.41 pp. The aggregate says
"promotable" only because a lone Tier-1 point outranks all of it.

Its one genuinely interesting cell: **`p11b.movers` does not move**. M5′
was proposed to buy locality and bought none — reproduced exactly.

#### B2 — structurally inert, and provably so

Not registered, because the measurement is unnecessary: B2's target code
is **unreachable on every fixture**.

`assign_x_layers` returns at `if sources.is_empty() { return
no_source_fallback(…) }`, which sits *strictly before* the
`break_cycles(adj)` call B2 would rewrite. `is_signal_source` counts a
`VoltageSrc`/`CurrentSrc` **not** tagged `;@ power`. Across all thirteen
fixtures every V/I source is either `;@ power=…` (excluded by role) or
`;@ ignore` (dropped before resolve), and `port_shapes.cir` has no source
at all. So `sources` is empty everywhere, `break_cycles` never runs, and
replacing its edge *reversal* with feedback-arc *marking* cannot change
one byte of emitted output.

**This is a finding, not a tie: B2 has no lever on this benchmark.** It
also confirms `1f7ed02`'s adjacent finding (the source/non-source branch
was a no-op) from the other side: not only is the directional branch
dead, the entire directed-graph path downstream of it is dead too.

#### B3 — skipped, with the reason

B3 is the *real* lever (it replaces `no_source_fallback`, the path that
takes 100% of the traffic), so it is not inert. It is skipped because it
cannot be *recovered*: no B3 code was ever committed, so replaying it
means re-implementing a layering reseed on top of a restored 728-line
`flow.rs` (`git show 74ba098^:crates/spice-layout/src/flow.rs`) — a new
implementation graded as if it were the old one, which is the wrong
experiment. Its recorded outcome also has a structural blocker that the
scoreboard cannot dissolve: the canonical feedback case is an op-amp
`.subckt` instance that `flow.rs` treats as directionless, so the marking
never fires there. Retry it after **B1′** (sheet-instance flow edges) and
**F0** (fixtures with un-ignored flow), per the roadmap.

#### What the five replays establish

**All the Tier-1 headroom on this benchmark is one defect on one
fixture.** The champion has exactly **one** real-`kicad-cli`-SVG-ink text
overlap in the entire suite: `v13.ink_overlap / rc_phase_shift = 1`.
Three of the four graded challengers — M4, M3-full, M5′ — remove it, and
that removal is the *whole* of each one's `T1 = −1.00`. They are three
unrelated perturbations (a Y datum, an overlap-gate box, an RNG keying)
with nothing in common except that they move `rc_phase_shift`.

So `T1 < 0` currently means **"did you perturb `rc_phase_shift`?"**, not
"is this a better architecture". Three placers cleared the promotion bar
on the same accidental cell while two of them were plainly worse
everywhere else. The tier-lexicographic rule is right in principle, but
with a champion this close to Tier-1-clean, one point of T1 is noise with
a veto.

**Were the reverts correct? Yes — four of five, and arguably all five.**

* **M3-B: correct revert.** Worse on the leading tier, unambiguously.
* **M3-full: correct revert.** Reads promotable, but only on that one
  `rc_phase_shift` cell, while costing `opamp_inverting_real` 4 bends and
  13 points of detour.
* **M5′: correct revert**, and the scoreboard *confirms* ADR-19's
  original diagnosis rather than overturning it.
* **B2: correct — there was nothing to revert.** It could not have
  changed anything.
* **B3: correct on the evidence available**, and still blocked on a
  structural precondition (B1′/F0) that no instrument change addresses.
* **M4 remains the one real candidate** — the only challenger that
  improves *both* tiers (`T2 = −6.50`), and even it is the residue of
  detour −20.50 against F6 +19.00.

**The instrument's own lesson.** ADR-23 was built because the ratchets
could detect drift but not select an architecture. That is still true and
still worth having. But this replay shows the scoreboard has the mirror
weakness: with only one Tier-1 defect left to remove on eleven hand-tuned
fixtures, its leading key is nearly degenerate, and "promotable" is
cheaper to earn than it looks. **The fix is not a different weighting** —
retuning the aggregate to fit these five results is precisely the
overfitting this ADR exists to avoid. The fix is **F0**: fixtures the
current placer does *poorly* on, so Tier 1 has real headroom and the
leading key has something to measure. The roadmap already says this; the
replay is independent evidence for it.

Two smaller follow-ups this surfaced, both real:

1. `v13.1_label_body` still asserts inside its fixture loop and truncated
   the M3-B row. D2 converted four such verifiers; convert this one too.
2. `q6.cov` printed `Δ = +0.00` for a value that moved 1.2247 → 1.4142.
   It is informational and excluded from the aggregate by design, so the
   zero is correct as a *contribution* — but printing it in the Δ column
   reads as "unchanged". Print the informational rows without a Δ.
### D7. Closing the `f0_defects` hole, and what it forced in the gate

The limit above named its own most valuable extension, and it has been
taken. `f0_defects` and `layout_cache` now forward
`common::placer_args()`, so `S2K_PLACER=<name>` reaches them.

The prize is `shunt_feedback_amp`. ADR-20 calls its Tier-0 net-merge
refusal the strongest acceptance test the project has for a replacement
placer, and it was invisible to every challenger's row. It now reports two
Tier-0 metrics — `t0.convert_fail` and `t0.partition` — recorded *before*
the lock's assertions, so a challenger that FIXES the refusal still reports
its `0` even as the lock goes red with UNEXPECTED PASS. The measurement
survives the outcome that makes the test fail.

**This forced one correction to the promotion rule, and it is a
correction, not a relaxation.** D4 clause 1 was written as "every Tier-0
metric is 0 on every fixture", which D3 could call "cheap to satisfy,
since every fixture measures 0 today" only because this fixture was
uninstrumented. It is now non-zero **on the champion**. Left absolute, the
gate would veto every challenger — including one that leaves the refusal
exactly as it found it — turning the project's strongest acceptance test
into a gate no placer can pass. The gate therefore keys off
`t0_worse`: the cells where the challenger is strictly worse than the
champion. Against an all-zero champion the two forms coincide exactly, so
the M4 replay's verdict in D5 is unchanged. The report prints all three
lists (champion absolute, challenger absolute, regressed) so the absolute
state stays visible and is not quietly traded away.

`t0.cross_net_overlap` (cross-net collinear wire overlaps — the latent
V11 short) is registered as a Tier-0 metric in the same pass; it was
being measured by `electrical_safety` and reported to nobody.

### D8. The other half of the finding: metrics with an ABSOLUTE reference

This ADR opens by observing that **every one of the ~165 literals was
obtained by measuring the incumbent placer's own output**, so against
that reference "regression" and "difference" are the same measurement.
The scoreboard answers half of what that implies — it lets placer B be
compared to placer A. It does *not* answer the other half: *how good is
either of them?* Both instruments are still anchored to the incumbent.
`rc_phase_shift`'s B = 19 is not judged as bad; it is **protected** at
19.

The fix for that half is a metric with a reference that does not come
from the placer. One already existed and was not recognised as a
different *kind* of number: `wire_detour` grades emitted wire length
against the half-perimeter lower bound, so 1.0 means "could not be
shorter" and the ratio reads directly as headroom. **That shape
generalises**, and `crates/spice2kicad/tests/bend_bound.rs` applies it to
V16: a *provable* lower bound on the bends any rectilinear ink could
have, computed from terminal geometry alone — no obstacles, no pin
directions, no router. It reports `measured / bound / gap` per fixture
and registers `v16.bend_bound`, `v16.bend_gap` and
`v16.bend_excess_exact` here as **`Tier::Info`**, zero aggregate weight,
on the Q6 precedent.

Informational is not timidity, it is the same discipline D4 applies to
promotion: a bound that were subtly *inadmissible* — ever above the true
optimum — would, as a gate, block all work while being wrong. It asserts
only its own soundness (Σ per-component bends == whole-sheet B; and
`bound <= measured` on every graded component), never the placer's
quality. Full lemma, proof and limits: that file's module docs and
`docs/invariants.md` V16.

**First measurement, and what it says about the roadmap.** Twelve
fixtures, 38/38 components and 86/86 bends covered: **B = 86 against a
bound of 15**. Read carefully, because the naive reading is wrong. The
lemma refutes `B = 0` only, so the bound is at most 1 per component —
and that ceiling is close to a fact about the metric rather than a weakness of
the proof: a trunk with taps realises `B <= 2` for *any* terminal set,
because taps meet the trunk as 3-ray Ts, which V16 scores as J and not
B. In an obstacle-free, direction-free world almost nothing forces a
bend. So `86 − 15` is an **upper** bound on reducible bends, not an
estimate of them, and most of that gap is congestion and V5 adherence
rather than slack.

The one number in the report that is *tight* is the two-anchor class,
where the obstacle-free optimum is known exactly (1 off-axis, 0
collinear): **14 bends drawn where 5 suffice, i.e. 9 provably wasted on
two-terminal ink alone**. That is the honest headroom figure this
benchmark work was trying to expose, and it is small — which is itself
the finding. The instrument that would raise it is the *V5-conditional*
column (deliberately not built, and never to be summed into the
admissible one): the realistic floors — like `rc_lowpass`'s documented
2-bend U — are conditional on the project's own conventions, not on
geometry.

Two smaller results fell out of building it, both worth keeping:

1. **Component connectivity must be read before the ink graph merges
   runs.** Segments join iff they share an *endpoint* — KiCad's rule,
   and the one `cleanup.rs::split_at_interior_attachments` exists to
   serve. A run-level "they touch, so they join" rule collapses
   `two_stage_amp` into 4 components, one carrying the `b2`, `c2` *and*
   `e2` labels, because ten wire ends land on the interior of a foreign
   net's wire.
2. **The instrument independently rediscovered a registered defect.**
   From geometry alone it flags exactly two collinear wire overlaps on
   `two_stage_amp`, at `x = 57.15` and `y = 87.63` — the same two the
   `no_cross_net_collinear_wire_overlap` xfail entry names. An
   instrument that reproduces a known answer it was not told about is an
   instrument (D5's own test, applied here).

### D9. `flow-seed` — the flow-faithful skeleton, graded and NOT promotable

**Status:** registered as `--placer=flow-seed`, dead on the default path.
Measured against the champion, whole suite each side, `--no-fail-fast`.
**Verdict: NOT promotable** — one Tier-0 cell regresses. The rest of the
table is the largest aggregate improvement the instrument has recorded.

> **SUPERSEDED IN PART BY ADR-25.** The blocking Tier-0 cell was
> *diagnosed wrong here*. It is not global re-basing and not the
> geometry-derived stride: phase 4.5's `overlap` guard was measuring body
> bboxes while the invariant it protects is stated over body ∪ pin reach,
> so the phase rotated `C1` R90 → R0 and shipped two pin-reach overlaps
> its own guard reported as zero. The hole is on the **champion** path.
> With ADR-25's one-function fix the cell is 0 on all 18 fixtures and the
> verdict is **PROMOTABLE** at Tier 1 −4.00 / Tier 2 −163.35 (the
> aggregate shrinks because two thirds of the previously reported
> `sallen_key_lpf` gain *was* the overlap). Read the "What blocks it"
> paragraph below as the record of a mis-attribution, not as guidance.

**The diagnosis it acts on.** `layers::no_source_fallback` is the path 16
of 18 fixtures take: every fixture but `lc_ladder_lpf` and
`sallen_key_driven` tags its stimulus `;@ ignore`, so `sources` is empty.
Its root set is `input_root(i) || touches_power(i)` — **every
rail-touching stub is a layer-0 root** — so the X "layer" measures hops
from the nearest power rail, not depth along the signal path, and that
functional saturates at ~2 in any biased amplifier regardless of stage
count. On `two_stage_amp` the chain `in→b1→c1→b2→c2→out` needs five
columns and gets `{0,1,1,1,3}`, dropping Q1, the coupling cap and Q2 into
one column that row-packing then stacks vertically. `common_emitter`
draws well only because for a *single* stage rail-hop depth and signal
depth coincide by accident.

`flow-seed` roots at signal-flow sources only (declared `*@port` inputs
and leaf-input nets, still behind ADR-18's `signal_degree <= 2`
"boundary not interior" filter), demotes rail stubs — Power **or**
Ground, signal degree ≤ 1 — to *followers* assigned after the BFS, and
orders each bucket by neighbour barycenter. It touches no spacing
constant, band datum or SA weight. A circuit with no signal-flow root at
all (`diff_pair`, `multivibrator`, `wien_bridge_osc`) falls through to
the champion's rail-rooted policy verbatim.

`crates/spice-layout/tests/layer_flow_dump.rs` is the instrument for the
layering itself: an `#[ignore]`d dump of every fixture under both root
policies, plus a **torn-signal-net** count (a Signal net whose members
span more than one column is precisely what a wire then crosses the
sheet to rejoin). On the thirteen fixtures the challenger is active on,
torn nets fall 8 → 3.

**The result, by tier.**

| tier | Δ points (challenger − champion) |
| --- | ---: |
| Tier 0 | `t0.sym_overlap` / `sallen_key_lpf` **0 → 2** (regression); `t0.cross_net_overlap` / `two_stage_amp` **2 → 0** |
| Tier 1 | **−5.00** |
| Tier 2 | **−180.33** |

`two_stage_amp`, the fixture the diagnosis was built on, against the
hand-`*@place`d control arm:

| metric | champion | flow-seed | hand-placed |
| --- | ---: | ---: | ---: |
| wire crossings | 10 | **0** | 0 |
| V16 bends (B) | 33 | **17** | 14 |
| V16 branches (J) | 9 | **5** | — |
| wire detour | 1.8565 | **1.0794** | — |
| Q3 flow-monotonicity | 8 | **4** | — |
| F6 worst rail-stub lateral run | 19 | **6** | — |
| V5 | 5 | **2** | — |
| Tier-0 collinear overlaps | 2 | **0** | — |

**Eight registered XFAIL entries discharge**, reported by the tripwires
themselves as `UNEXPECTED PASS`. All four on `two_stage_amp` — the
`no_cross_net_collinear_wire_overlap` Tier-0 entry (both runs, at
`x = 57.15` and `y = 87.63`) and the three V13 decoration entries
(`v13.2` 1→0, `v13.4` 2→0, `v13.6a` 3→0) — plus three on
`rc_phase_shift` (`no_power_glyph_foreign_body_overlap`, the real-ink
`rendered_text_does_not_overlap`, `v13.6a`) and one on `cascode_amp`
(`v13_labels_clear_pin_text`).

Every one of them was registered as a **decoration** or **channel-router**
defect. They are a *layering* defect. That is the finding worth keeping
even though the challenger is not promotable: **a defect attributed to a
downstream stage can be a symptom of the skeleton**, and a deferral
written against the wrong stage will never expire on its own.

`rc_phase_shift` moves nearly as far (bends 19 → 6, F6 23 → 7, detour
1.2313 → 1.0278, crossings 2 → 0), and `shunt_feedback_amp` and
`cascode_amp` improve on most Tier-2 axes.

**What blocks it, and why it is not tuned away here.** `sallen_key_lpf`
gains two symbol/symbol overlaps: `C1` clips both `X1`'s opamp triangle
and `RA`. It is Tier 0, per-fixture hard, and never traded — the
promotion rule stops there.

Two ablations were run before reporting, because attribution matters
more than the verdict:

1. **It is not the barycenter ordering.** With the ordering ablated to
   the netlist order (the champion's key), the two overlaps are
   *byte-identical*, same coordinates. The third ingredient is not the
   cause.
2. **It is not a seed infeasibility that `--no-refine` could show.**
   Both placers refuse `sallen_key_lpf` under `--no-refine` on the same
   ADR-22 net-partition certificate, so the seed-only arm carries no
   information here.

What remains is the mechanism ADR-17's retirement already named:
`sallen_key_lpf`'s tail column moves one X stride left (its layer count
drops 4 → 3 when `C2` follows `R2` into its column), the whole
downstream chain re-bases, and phase 4.5 then picks `C1` at rot0 instead
of rot90 — an orientation whose 7.6 mm vertical extent no longer clears
`X1` above and `RA` below. **Global re-basing is intrinsic to any
spacing-derived placement** (ADR-17 RETIRED), and the repair is in the
geometry-derived stride / clearance layer — ADR-18 root cause 4, "a hard
constraint cannot repair an infeasible start" — which this challenger
deliberately does not touch. Fixing it *inside* the root policy would be
tuning the aggregate against the fixture that blocks it, which is the
overfitting this ADR exists to prevent.

**Recorded, not landed.** No budget literal moved, no `baseline_lock`
row was regenerated, and the default path is byte-identical (verified:
full suite green; all 18 fixtures convert under both placers; the three
rootless fixtures, the two principled-path fixtures and `rc_lowpass` are
byte-identical between the two). Promotion — or a decision to pay the
`sallen_key_lpf` Tier-0 cell down in the spacing layer first — is an
owner call on the table above.

> **SUPERSEDED BY "The promotion" below (2026-08-18).** `flow-seed` was
> promoted to the default. Read D9's numbers as history: they were taken
> before ADR-25 and before the `fix/rail-stub-side` commit, and both
> moved them. The fresh table is in the promotion section.

### The promotion — `flow-seed` becomes the default (2026-08-18)

**Status:** landed, on **owner approval of the printed table**. This is
the first exercise of D4, and the first time `baseline_lock` and the
per-fixture literals have been regenerated wholesale for an
architecture change rather than a decoration one.

#### Re-graded first, because D9's numbers were stale

Two changes landed between D9's grading and this promotion, and both
moved the table, so the verdict was re-measured on the promotion tree
rather than inherited:

* **ADR-25** removed the Tier-0 cell (`t0.sym_overlap` /
  `sallen_key_lpf`) that had blocked it, and in doing so gave back two
  thirds of that fixture's reported gain — the gain *was* the overlap.
* **`fix/rail-stub-side`** (`71e2483..90f8683`) corrected a re-column
  helper that had been forcing every downstream shunt below its node.
  `flow-seed`'s previously-low bend counts on `rc_phase_shift` and
  `shunt_feedback_amp` were partly an artefact of that wrong layout.

Fresh run, whole suite each side, `--no-fail-fast`, one machine, on the
promotion tree:

| | ADR-23 D9 (stale) | ADR-25 re-grade (stale) | fresh, as graded | **fresh, final** |
| --- | ---: | ---: | ---: | ---: |
| Tier 0 regressions | 1 (`t0.sym_overlap`) | none | none | **none** |
| Tier 1 total Δ | −5.00 | −4.00 | −1.00 | **−4.00** |
| Tier 2 total Δ | −180.33 | −163.35 | −113.87 | **−113.87** |
| verdict | NOT promotable | PROMOTABLE | PROMOTABLE | **PROMOTABLE** |

The Tier-2 improvement is roughly **half** what D9 advertised. That is
the instrument working: two intervening fixes each removed part of the
challenger's advantage, and re-grading is what exposed it. Promoting on
D9's table would have over-claimed by 2×.

The two "fresh" columns differ only by the five metric ids registered in
this commit (see "two blind cells" below): the graded table read Tier 1
−1.00, the final one −4.00, because `junction.cross_net` on
`two_stage_amp` (4 → 0) and `v13.9_foreign_over_glyph` on `named_rails`
(0 → 1) were being measured and thrown away. The verdict is unchanged
either way, and it is the *stricter* of the two that was used to decide.

Every cell that moved, fresh:

| metric | tier | fixture | champion | flow-seed | Δ |
| --- | --- | --- | ---: | ---: | ---: |
| `t0.cross_net_overlap` | 0 | two_stage_amp | 2 | **0** | −2 |
| `v13.2_label_prop` | 1 | two_stage_amp | 1 | **0** | −1 |
| `v13.6a_glyphtext` | 1 | two_stage_amp | 1 | **0** | −1 |
| `v14.glyph_body` | 1 | sallen_key_lpf | 0 | 1 | **+1** |
| `v13.9_foreign_over_glyph` | 1 | named_rails | 0 | 1 | **+1** |
| `junction.cross_net` | 1 | two_stage_amp | 4 | **0** | −4 |
| `v5` | 2 | cascode_amp | 4 | **1** | −3 |
| `v5` | 2 | two_stage_amp | 5 | **2** | −3 |
| `v5` | 2 | sallen_key_lpf | 5 | **3** | −2 |
| `v5` | 2 | common_emitter / opamp_inverting / named_rails / shunt_feedback_amp | 1/1/2/1 | **0/0/1/0** | −4 |
| `v5` | 2 | opamp_inverting_real | 0 | 1 | +1 |
| `v16.bends` | 2 | two_stage_amp | 33 | **17** | −16 |
| `v16.bends` | 2 | sallen_key_lpf | 6 | 12 | +6 |
| `v16.bends` | 2 | opamp_inverting | 3 | 6 | +3 |
| `v16.bends` | 2 | cascode_amp / opamp_inverting_real / shunt_feedback_amp | 12/5/11 | 13/6/12 | +3 |
| `v16.bends` | 2 | named_rails | 2 | **1** | −1 |
| `v16.branches` | 2 | two_stage_amp | 9 | **5** | −4 |
| `v16.branches` | 2 | sallen_key_lpf | 3 | **1** | −2 |
| `v16.branches` | 2 | rc_phase_shift / named_rails | 3/2 | **2/1** | −2 |
| `v16.branches` | 2 | cascode_amp / common_emitter / shunt_feedback_amp | 3/3/2 | 4/4/3 | +3 |
| `crossings` | 2 | two_stage_amp | 10 | **0** | −10 |
| `crossings` | 2 | rc_phase_shift | 5 | **0** | −5 |
| `crossings` | 2 | cascode_amp | 2 | **0** | −2 |
| `crossings` | 2 | sallen_key_lpf | 2 | **1** | −1 |
| `detour` | 2 | two_stage_amp | 1.8565 | **1.0794** | −77.71 pp |
| `detour` | 2 | sallen_key_lpf | 1.0407 | 1.3019 | +26.12 pp |
| `detour` | 2 | cascode_amp | 1.0842 | 1.2195 | +13.53 pp |
| `detour` | 2 | named_rails / opamp_inverting / shunt_feedback_amp | | | −10.91 pp |
| `detour` | 2 | common_emitter / opamp_inverting_real / port_shapes / rc_phase_shift | | | +11.10 pp |
| `q3` | 2 | two_stage_amp | 8 | **4** | −4 |
| `q3` | 2 | cascode_amp / named_rails / shunt_feedback_amp | 3/1/3 | **2/0/2** | −3 |
| `q3` | 2 | rc_phase_shift / common_emitter | 2/1 | 4/2 | +3 |
| `q5` | 2 | two_stage_amp / rc_phase_shift | 7/3 | **5/2** | −3 |
| `q5` | 2 | opamp_inverting_real | 0 | 3 | +3 |
| `q5` | 2 | cascode_amp / common_emitter / sallen_key_lpf | 3/3/2 | 4/4/3 | +3 |
| `f3` | 2 | rc_phase_shift / sallen_key_lpf / two_stage_amp | 1/1/1 | **0/0/0** | −3 |
| `f5` | 2 | cascode_amp / opamp_inverting / rc_phase_shift | 2/2/1 | **1/1/0** | −3 |
| `f5` | 2 | sallen_key_lpf | 0 | 2 | +2 |
| `f6` | 2 | rc_phase_shift | 24 | **8** | −16 |
| `f6` | 2 | two_stage_amp | 19 | **6** | −13 |
| `f6` | 2 | cascode_amp / named_rails | 7/6 | **5/4** | −4 |
| `f6` | 2 | common_emitter | 4 | 5 | +1 |

**F3 is now zero on all eighteen fixtures.** That is the most direct
confirmation available that the promotion did what it claims: F3 counts
elements drawn upstream of something they are fed by, and every
remaining one disappeared when X stopped meaning "hops from a rail".

#### The two Tier-1 regressions, called out

The owner approved a **promotion**, not a specific Tier-1 loss. Both are
registered in `tests/common/xfail.rs` as tripwires that expire the day
they are fixed — not given budget headroom, because a budget hides a
count inside a number that only ratchets:

1. **`v14.glyph_body` / `sallen_key_lpf`, 0 → 1.** `#PWR4`'s GND glyph
   (host `X1`) clips `C1`'s body. This one **was** on the scoreboard: it
   is the single `+1.00` Tier-1 cell, weighed against `−2.00`. Same
   deferred issue-[3] class `wien_bridge_osc` already carries; ADR-14's
   known scope limit (the SA reserves the glyph footprint hard only for
   oversized-involving pairs, and `X1`'s opamp triangle is the oversized
   body).
2. **`no_foreign_label_or_wire_over_power_glyph_body` / `named_rails`,
   0 → 1.** The `in` global label overlaps the `n5` (−5 V) rail glyph
   body. Decoration-fixable exactly as that verifier's own budget doc
   says — the label-nudge pass does not treat power-glyph bodies as
   obstacles.

#### The finding that outlives the promotion: two blind cells

Regression 2 above **was not in the table that graded the promotion**,
because its verifier reported nothing to the measurement sink. Nor did
`junction_parity`'s three metrics, nor P11's cache-path check. D2's
contract — "each verifier reports the number it *already computed*, on
the line before the assertion that grades it" — was never enforced, so
"comparison complete" (D4 rule 3) meant *complete over the registered
metrics*, which is a weaker statement than it reads as.

Five metric ids were added in this commit — `v13.9_foreign_over_glyph`
(T1), `junction.missing` / `junction.spurious` / `junction.cross_net`
(T1), `p11.cache_out_of_step` (T2) — and they moved the reported Tier-1
aggregate from **−1.00 to −4.00**: `two_stage_amp`'s four cross-net
collinear contact points fall **4 → 0** (an improvement the promotion
could not previously claim), against the `named_rails` regression it
could not previously see. Both directions were invisible, which is the
point — a blind cell is not conservatively blind.

**Rule going forward:** a fixture-enumerating verifier without a
`scoreboard::record*` call on the line before its assertion is a cell no
scoreboard can see move. Adding one is the whole contract.

#### One verifier's measurement was corrected (not relaxed)

`cache_path_keeps_pre_existing_symbols_in_place` (P11) failed under the
new default, reporting "adding `CB b 0 10n` moved 8 pre-existing user
symbols through the layout cache". Measured, it is **one uniform
translation**: every one of the eight moves by exactly `(+8.89, 0)`, and
nothing else changes. The new bypass cap lands 8.89 mm left of the
previous leftmost symbol, so the **V15 page-fit pass** — which shifts
each sheet's content bbox to the page margin — translates the sheet.
That is true of every placer and `baseline_lock`'s own history already
records it as a non-event ("the V15 offset moved by a single per-fixture
delta … Symbol poses relative to one another are unchanged").

The verifier now groups pre-existing user symbols by their `(dx, dy)`
delta and requires **exactly one group**, with rotation/mirror/lib_id
unchanged. This is not a new idea in that file: **P11b already does
it** — `residual_movers` factors out "the single uniform page
translation V15 may apply", taking the modal integer-grid delta, for
exactly this reason. P11 was never updated to match its own sibling. This is MEMORY "verify what a number measures" again: the old
comparison was a statement about the *page origin* presented as one
about placement locality.

It is deliberately **not** a budget and not a relaxation of the thing
being graded:

* two distinct deltas fail; one symbol out of step by a single grid cell
  fails; any rotation fails;
* `p11_delta_grouping_catches_one_symbol_out_of_step` is the control arm
  — three synthetic sheets proving the grouping still catches a tear;
* and the **champion control arm** measures a single delta of `(0, 0)`
  on both cases, i.e. it still satisfies the strictly stronger old
  property. Verified by running the suite with `S2K_PLACER=champion`.

#### What was regenerated, and the five fixtures that were not

`baseline_lock`: **157 of 282 rows** moved, across ten fixtures. Eight
are byte-identical, five of them load-bearingly — `diff_pair`,
`multivibrator`, `wien_bridge_osc` (rootless, so they take the old
rail-rooted policy verbatim) and `lc_ladder_lpf`, `sallen_key_driven`
(real drawn sources, never in the fallback). Verified with `cmp` on the
emitted sheets under `--placer champion` vs the new default, not
inferred. That is the cheapest available check that the fallback
survived the swap.

Every per-fixture literal was re-recorded at the new default's measured
value, read from the **scoreboard sink**, not from test output — five of
these budgets only fail on excess and print their "you may lower this to
N" hint through `eprintln!`, which cargo swallows on a passing test. A
previous agent reading test output reported "V16 unchanged" when B had
improved 19 → 10.

Two literals were also reclaimed that have nothing to do with the swap:
`detour` on `rc_lowpass` (1.167 → 1.0) and `rc_lowpass_ports`
(1.4001 → 1.0). Both fixtures are byte-identical across the promotion;
their literals were simply stale. Ratchet DOWN, always permitted.

#### Three XFAIL entries discharged

All three on `two_stage_amp`, all announced by their own tripwires as
`UNEXPECTED PASS`, and **all three had been filed against the wrong
stage**:

* `no_cross_net_collinear_wire_overlap` — the **Tier-0** entry, filed as
  a "deferred v0.2 channel-router wall". It was not a router defect: the
  `b2`/`c2` trunks shared a column because rail-hop layering collapsed
  `in→b1→c1→b2→c2→out` into three columns.
* `v13_labels_dont_overlap_property_text` and
  `v13_power_glyph_value_text_clear_of_bodies_and_pintext` — both filed
  as *decoration nudge* defects. Also layering.

That is D9's finding landing for real: **a defect attributed to a
downstream stage can be a symptom of the skeleton, and a deferral
written against the wrong stage never expires on its own.** D9 predicted
eight discharges; the other five had already expired to ADR-24 and
ADR-26 before this promotion ran.

#### `champion` stays registered

`Placer::Champion` is not deleted and must not be. It is the scoreboard's
**control arm**: every future challenger is graded against the new
default, and A/B against the previous architecture is the only way to
attribute a future regression to the promotion rather than to the change
under test. `placer::tests::the_champion_control_arm_stays_registered`
pins that. Two `kicad-emitter` unit tests (the ADR-17 `COUT` rot-180
severance probe and ADR-25's pin-reach control arm) are likewise pinned
to `Placer::Champion` explicitly — both state in their own doc comments
that they measure a champion placement, and both cases evaporate on the
new default's geometry.

`spice_layout::layers::assign_x_layers` — the variant-free entry point —
also stays pinned to the champion policy, deliberately. It is a fixed
*reference* layering for `cost::layer_order` (inert either way: the
variant only alters the no-source fallback, which `layer_order`
short-circuits on) and for the Q3 flow-monotonicity verifier, whose
budgets are stated against it. Re-pointing it would silently redefine a
graded metric, so Q3 grades the new default against the *old* skeleton's
idea of flow — the conservative choice, and the only one that keeps
those literals comparable across the swap.



## ADR-24 — A Steiner vertex is not an endpoint: the router's own Tier-0 severance

**Status:** landed. Scope is confined to `crates/spice-route/`. **Every
one of `baseline_lock`'s 247 pre-existing rows is byte-identical** — no
element of any previously-graded fixture moved — and eleven of the
thirteen previously-emitted `.kicad_sch` files are byte-identical too.
Two Tier-0 defect locks are discharged and their fixtures promoted.

### The report

`sallen_key_driven` — the Sallen-Key low-pass of `sallen_key_lpf` with
its stimulus **drawn** rather than `;@ ignore`d — converted into an
electrically wrong schematic. A `--refine-iterations` sweep:

| setting | result |
| --- | --- |
| `--no-refine` (bare seed) | **SPLIT**: `np` reconstructs as 2 islands |
| 0 … 150, 201, 400 | clean |
| 199, 200 (the default) | **MERGE**: `np` + `out` in one component |

The `--no-refine` row is what made this different from ADR-20's
`shunt_feedback_amp`, which is clean at `--no-refine`. `--no-refine`
ablates both the SA and phase 4.5, so what remains is the bare
deterministic seed — and it was **already Tier-0 broken**. That looked
like evidence about the *placer*. It was not.

### Mechanism — one cause, two faces

Both faces are the same property: **the conflict/detour passes are
per-segment, and a Steiner vertex is not a segment property.**

**Face 1, the seed SPLIT.** Net `np` has three pins. Its exact Hwang
tree puts the single Steiner point at the coordinate-wise *median* of
them — provably optimal, and here that median is `(64.77, 44.45)`, which
is **`RA`'s `inv` pin**. `resolve_conflicts` correctly wants the tree off
a foreign net's endpoint, and calls `jog_endpoint_at`, which rewrites
**one** incident segment per pass into an L. Its destination depends on
that segment's axis — a horizontal leg goes to `y + g`, a vertical leg to
`x + g`. Applied three times to one vertex it therefore sent the three
legs to *different* coordinates:

```
steiner            resolve_conflicts (3 passes)
(34.29,44.45)─┐    (34.29,45.72)──────(64.77,45.72)   ← orphan
              │
(64.77,58.42)─┼─   (66.04,44.45)──(66.04,58.42)
              │    (66.04,44.45)──(66.04,22.86)       ← trunk, without the orphan
(64.77,22.86)─┘
```

`cleanup::trim_whiskers` then deleted the orphan — **including the
segment sitting on `R2`'s own `np` pin**, because it only tests the
endpoint it is trimming, not the far end of the chain. The router
finished with a net whose third pin had no wire at all and printed
**no warning**: its retry loop detects `any_broken`, but its only lever
is outward-stub suppression, which has no bearing on this, and after
`max_attempts` it keeps the geometry and breaks. ADR-22's partition
certificate caught it — which is the only reason this was a refusal and
not a silently shipped short.

**Face 2, the default MERGE.** At 200 iterations the placement is
different and `np`'s tree is clean, but `avoid_obstacles` detours the
branch to `X1`'s `inp` pin around a body and parks a corner on
`(59.69, 44.45)` — where net `out`'s trunk already turns. A shared wire
endpoint is a merge. Nothing saw it: `avoid_foreign_pins` keys on foreign
*pins*, `avoid_obstacles` on symbol *bodies*, `deconflict_cross_net_overlaps`
on *collinear* overlap, and `resolve_conflicts` — the one pass that does
key on shared endpoints — ran **once, at the top of the attempt, over the
pristine Steiner trees**, before any detour existed.

### D1. A vertex on a foreign pin is moved at construction time

`steiner::move_vertices_off_foreign_pins` relocates any point where
**three or more** of a net's own segments meet, when that point is a
foreign pin. It runs inside `route_signal_inner`, on the tree, before any
conflict pass sees it. Relocation is topology-preserving by construction:
the vertex keeps its degree and its legs; each leg is rebuilt as an L
whose two lines (`y = vy`, `x = vx`) do not contain the vacated
coordinate, so no downstream pass is *required* to re-stitch anything.

The module header records a standing decision that V11 enforcement is
deliberately not done at construction, so that a detour which would
overlap a sibling net can be rolled back. That reasoning is sound for a
degree-1 endpoint and fails for a vertex, because the rollback machinery
is per-segment and a vertex is shared.

**Degree ≥ 3 is not a tuning threshold; it is the codebase's own line.**
`conflict::corner_degree`'s doc says it: *"A shared corner with degree 2
is a simple L bend; degree ≥ 3 marks a Steiner T-junction whose tree
topology must be preserved."* Both L-pair repairs (`try_alt_l_corner`,
`try_u_detour_l_pair`) already **decline** at `corner_degree > 2`, with
the comment *"replacing the L pair would orphan that leg from the rest of
the tree"* — i.e. the existing V11 machinery is complete for degree ≤ 2
and explicitly abstains at degree ≥ 3, leaving those to the per-segment
jog that fragments them. This fills exactly that gap and nothing else.

Measured, not assumed: extending it to degree ≥ 2 also works and fixes
the same two fixtures, but it re-routes `opamp_inverting`'s `inv` net,
whose deliberate V4 **name-jump label pair** exists precisely so a
split-by-detour tree still connects by name. That cost `opamp_inverting`
crossings 0 → 1, V16 J 0 → 1 and wire-detour 1.111 → 1.122 — three Tier-2
ratchets on a fixture that was not broken. Rejected.

**The quadrant is chosen, not fixed.** The displacement is diagonal (so
no leg travels back along the line it came from), and which of the four
diagonals is picked by *least added Manhattan length*. Always taking
`(+g, +g)` costs two cells per leg on a vertex whose legs all run
up-and-left, where the up-left quadrant costs zero.

### D2. Conflict resolution re-runs after the detour passes

`resolve_conflicts` now also runs at the end of each iteration of the
V11/V12 convergence loop, so a cross-net endpoint the detours *create* is
seen and jogged, and the next iteration re-judges whatever it moved.

Inert by construction on clean geometry: `find_conflicts` returns empty
and the pass returns immediately. Measured: with D2 alone, all thirteen
fixtures emit byte-identical output and `sallen_key_driven` converts at
the default iteration count.

### D3. What this corrects in ADR-20

ADR-20 diagnosed `shunt_feedback_amp`'s residual as the owner-gated R-5
rail-pin defect, on the strength of phase 4.5's oracle reporting the
incoming placement as `severed = 2` — "nothing after the placer can move
an element, so this is a *placer* defect".

The premise is right and the inference was wrong, for a reason ADR-16
already named in a different context: **phase 4.5's oracle is the real
router.** `severed = 2` was measuring the router fragmenting its own
trees, not the placer producing an unroutable placement. With D1 in place
the same placement, unchanged to the millimetre, measures `severed = 0`
and the fixture converts. `baseline_lock` proves the placement did not
move: 247 of 247 rows byte-identical.

R-5 is untouched and still owner-gated. It is real — `shunt_feedback_amp`
still trips `v14_rail_pin_faces_rail`, now as a registered XFAIL — but it
is a Tier-1 aesthetic defect, not what made the fixture unconvertible.

The generalisable form: **a metric taken through a stage is a joint
measurement of the input and that stage.** ADR-20 attributed all of it to
the input. This is the same failure MEMORY "verify what a number
measures" records, reached from a new direction, and the control arm that
would have caught it is exactly the one this ADR ran — re-measure the
metric after changing only the *stage*.

### D4. The `*@port` masking finding, which is a second defect

The `sallen_key_driven` lock recorded, as its strangest control arm, that
adding `*@port in=input` — "a purely cosmetic label directive that
changes no topology" — made the fault disappear, and read that as
evidence of ADR-17's "global, unattributable consequences".

It is simpler and worse than that: **`*@port` is not cosmetic.** It is
read by the placer in two places, and the fixture's own comment, the
lock's text and the annotation spec's framing all describe it as a
labelling directive:

* `layers::no_source_fallback` extends its input/output root sets with
  every element on a declared port net ("reinforce the same left/right
  bias by POSITION only");
* `idioms::signal_net_depth` seeds its BFS **only** from
  `*@port …=input` nets, falling back to a *leaf-net name* heuristic that
  requires the net to touch exactly one element.

That second one is why the twin fixtures diverge, and it is the defect.
In `sallen_key_lpf` the source is `;@ ignore`d, so `in` touches one
element, the name fallback fires, and every series element gets a flow
direction. In `sallen_key_driven` the source is drawn, so `in` touches
two elements, the fallback does **not** fire, and — with no `*@port`
either — `signal_net_depth` returns an **empty map**: every series
element is directionless. Adding `*@port in=input` restores the roots,
which changes placement, which reshuffles the router into a
non-pathological configuration. It never fixed anything; it moved the
dice.

So the masking is not evidence of unattributable coupling. It is a
**latent-input defect**: `signal_net_depth`'s fallback is keyed on
`net_members == 1`, which is a *proxy* for "boundary of the signal
chain" that fails on exactly the case the fallback exists for — a
circuit whose input net is boundary-ish but has a drawn source on it. The
same is true of `layers::no_source_fallback`'s leaf test. Both are
untouched here (they are placement quality, not Tier 0, and changing them
moves every fixture), and both are recorded as the open item.

**Two things follow for the spec.** `*@port` is currently documented as a
terminal *declaration* whose effect is a directional `(global_label …)`;
it is in fact also a placement input, and design principle 3 ("users
describe intent, the converter owns geometry") is satisfied but
under-documented. And a directive that changes placement must never be
described in a fixture comment as "purely cosmetic" — that framing is
what made a straightforward missing-root bug look like evidence for
retiring the placer architecture.

### Blast radius

* `baseline_lock`: **0 rows removed, 35 added**, all on the two promoted
  fixtures. Verified by set comparison of the regeneration dump against
  the previous table, not by eye. ADR-16 requires a `spice-route`-only
  change to leave placement alone; it did.
* Emitted `.kicad_sch`, all pre-existing fixtures: byte-identical except
  `two_stage_amp`, which loses **one** wire segment (56 → 55) with
  identical total wire length, identical junction count and identical
  symbol poses — a collinear pair merged. Its V16 `(B, J)` is unchanged
  at `(33, 9)`.
* `opamp_inverting`: byte-identical (see D1 for the variant that was
  rejected because it was not).
* No budget rose anywhere. The only new literals are the two promoted
  fixtures' own, recorded at their measured values with zero slack.

### The two fixtures, promoted

`tests/f0_defects.rs` now holds **no locks**. Both tripwires fired and
were followed:

| | `sallen_key_driven` | `shunt_feedback_amp` |
| --- | --- | --- |
| V16 (B, J) | 13, 4 | 12, 2 |
| crossings | 3 | 0 |
| wire detour | 1.0764 | 1.2034 |
| V5 | 3 | 5 |
| Q5 near-miss | 5 | 2 |
| Q3 flow inversions | 3 | 2 |
| F5 series pose | 3 | 1 |
| stub lateral run | 7 | 9 |
| Tier-0 (partition, V11, coincidence, ERC) | 0 | 0 |

Three XFAIL entries were added, each naming a Tier-1 defect an existing
fixture already carries an entry for (two V13 decoration nudges, and R-5
on `shunt_feedback_amp`). None is new.

The file keeps one test, re-pointed rather than deleted. ADR-21's
unconditional-refusal regression used to ride on `shunt_feedback_amp`
being broken; with no broken fixture left it would have been lost
*because the converter improved*. It now **installs** its fault instead
of finding one: the ADR-4 layout sidecar stacks `rc_lowpass`'s `R1` and
`C1` so `C1`'s ground pin lands on `R1`'s input pin, and the CLI must
exit 1 under `--no-verify` and write nothing. A control arm asserts the
same invocation converts cleanly without the sabotaged sidecar, so the
test cannot rot into "the CLI fails at everything".

One incidental finding from building it, worth knowing: **`legalize`
separates overlapping bodies even for elements pinned by the ADR-4 cache,
but nothing separates coincident pins.** Pinning both parts to one origin
does not reproduce the fault; stacking them six cells apart does.

### Open items this leaves

1. **`signal_net_depth` and `no_source_fallback` root detection** (D4).
   Both use "the net touches exactly one element" as a proxy for "this is
   the boundary of the signal chain", and both fail on a drawn source.
   The fix — root at the source element when one exists, as
   `assign_x_layers` already does — moves placement on every rooted
   fixture and is a separate change with its own baseline regeneration.
2. **The router's give-up path is silent.** When the retry loop exhausts
   `max_attempts` with a net still severed, it keeps the geometry and
   emits no warning at all. ADR-22's certificate turns that into a
   refusal, so it is not a correctness hole — but it is a diagnosis hole,
   and it is why this defect presented as "the converter refuses and says
   nothing about why".
3. **`trim_whiskers` can delete a chain that reaches an own pin.** It
   tests only the endpoint being trimmed, so a severed branch is removed
   from the far end inward until the segment on the net's own pin goes
   too. It did not *cause* this defect (the branch was already severed
   when it ran) but it erased the evidence, and a whisker chain incident
   on an own pin should be kept, not trimmed.

## ADR-25 — Phase 4.5's guards must measure what the invariant measures: the pin-reach hole

**Status:** landed. Scope is one function in
`crates/kicad-emitter/src/refine.rs`. The default path is
**byte-identical** — all 18 fixtures (19 emitted sheets) diff clean
against the pre-fix binary, `baseline_lock` is untouched, no budget
literal moved, full workspace suite green.

### The report

ADR-23 D9 graded `--placer=flow-seed` as the largest aggregate
improvement the scoreboard has recorded and **not promotable**, blocked
by exactly one Tier-0 cell: `t0.sym_overlap` on `sallen_key_lpf`,
**0 → 2**. `C1` clipped both `X1`'s opamp triangle and `RA`.

D9 attributed the residual to global re-basing — the tail column moving
one X stride left — and located the repair in "the geometry-derived
stride / clearance layer (ADR-18 root cause 4)". **That attribution is
wrong, and the correction matters more than the cell**: the defect is on
the champion path, in a stage the challenger does not touch.

### Mechanism, named to the stage and the decision

A symbol overlap reaching the emitted file is odd on its face, because
`spice_layout::legalize` exists to separate overlapping bodies. It ran,
and it succeeded:

1. `spice_layout::place` ends with `legalize_if_needed`, which measures
   `legalize::overlap_count` — footprints from
   `footprint::body_and_pins`, i.e. **body bbox ∪ pin reach**. On
   `sallen_key_lpf` under `flow-seed` it measures **0**: it does not even
   log, let alone shove. The placement leaving the placer is legal.
2. **Layout phase 4.5 then changes orientation, which changes body
   extent, and nothing downstream re-checks.** Measured with a pose probe
   either side of `refine_orientations`: `C1` enters at **R90** (the
   champion's pose, a 4.06 mm tall extent) and leaves at **R0**, whose two
   3.81 mm pin stems stretch it to **7.62 mm** — no longer clearing `X1`
   above or `RA` below. The emitted sheet carries two extent overlaps.
3. Phase 4.5 *does* carry an `overlap` non-regression guard for exactly
   this. It reported `overlap=0` at baseline **and** at final. It was
   measuring **body bboxes only**, while the postcondition it is the last
   defence for — `legalize`'s, and
   `placement_quality::no_symbol_symbol_overlap_across_fixtures`'s — is
   stated over **body ∪ pin reach**. Both overlaps are pure pin-stem
   geometry, so the guard could not see what it was guarding.

**ADR-20's guard exemption is not involved.** The debug line reads
`baseline severed=0 coincident=0 v11=0`, so `tier0(m) < tier0(baseline)`
is unreachable and the `overlap`/`v12` guards were fully armed. The
challenger did not buy an overlap to repair Tier 0; the guard simply
returned the wrong number.

This is MEMORY "verify what a number measures" in its purest form: the
function's own doc comment claimed it "mirrors the
no-symbol-symbol-overlap verifier's intent (body extent,
orientation-aware)" while measuring a strict *subset* of that verifier's
geometry. The claim and the code disagreed, and only the claim was ever
read.

### D1. The guard measures the resolved extent

`symbol_overlap_count` now takes body bbox ∪ pin reach — the one
definition `footprint::body_and_pins` and `resolved_world_extent` both
already use, restated in the emitter's page frame because
`kicad-emitter` cannot depend on `spice-layout` (the same crate-cycle
constraint that puts phase 4.5 in `kicad-emitter` at all). `Probe::overlap`
is the same function, so `pruned`'s soundness proof — arm 1, "fails the
surviving guard directly" — is unchanged.

Widening a guard can only ever *decline* a candidate. Phase 4.5 selects
from the V14-allowed orientation set and never proposes a pose the seed
chooser could not, so a larger extent cannot invent a layout; it can only
refuse one. Confirmed by measurement: the champion's emitted output is
byte-identical on all 18 fixtures, and `flow-seed`'s changes on exactly
one — `sallen_key_lpf`, the blocked cell.

### The champion is latently affected — this is not a challenger patch

The hole lives in `kicad-emitter`, on the default path, and is
placer-independent. The offending pose is in `C1`'s allowed set on the
**champion's own** `sallen_key_lpf` placement: rotate the settled `C1`
from R90 to R0 and the resolved-extent count goes 0 → 2 while the retired
body-only model still reads 0.

What keeps the champion from shipping it today is not the guard but a
coincidence: on the champion's geometry that same pose also measures
`v11 = 1` and `v12 = 3`, so the objective tuple rejects it on its own.
`flow-seed` found a placement where the tuple *favoured* it. That is the
same mask ADR-20's `severed` case describes ("master's SA is what makes
it disagree here — that coincidence is precisely the mask this defect was
hiding behind, and any future placer change can remove it"), and the same
remedy: `severed_guard_tests::a_pin_reach_only_overlap_is_invisible_to_a_body_only_extent`
pins it on the champion fixture with the champion placer, isolates the
guard from the tuple, and carries the **retired body-only model as a
control arm** — deliberately a copy, not a parameter of the live
function, because a control that shares code with the thing under test
proves nothing.

### The general rule this instance is an instance of

CLAUDE.md's "consistency requirement" says a property enforced as a hard
constraint at one stage must be hard at *every* stage that can move the
element. This adds the measurement half of it:

> **A guard that protects a postcondition must be evaluated over the
> same geometry the postcondition is stated in.** A guard measuring a
> strict subset of it is not conservative — it is unsound, and it is
> silent about the difference.

Phase 4.5 is where this bites hardest, because it is the only stage that
changes body extent after the placement stage's last legality check.
Its `v13` model was already aligned this way (ADR-19 M3 notes phase 4.5's
V13 model "is the *more* faithful model, so M3 aligned this one to it, not
the reverse"). `overlap` was the outlier.

### Consequences for ADR-23 D9

The blocking cell is closed: `t0.sym_overlap` is **0 on all 18 fixtures
under both placers**. Re-graded whole-suite (champion records unchanged —
its output is byte-identical, so its measurements are):

| | before this ADR | after |
| --- | --- | --- |
| Tier 0 | `t0.sym_overlap`/`sallen_key_lpf` 0 → 2 (**regression**) | **challenger clean; no cell worse than champion** |
| Tier 1 total Δ | −5.00 | **−4.00** |
| Tier 2 total Δ | −180.33 | **−163.35** |
| verdict | NOT promotable | **PROMOTABLE** |

The aggregate moved *toward zero* because the closed cell was being paid
for: `flow-seed`'s `sallen_key_lpf` keeps `C1` at R90 now, and that costs
V16 B 6 → 12, detour 1.0407 → 1.3019, and one cell each of
`v13.4_text_mutual`, `v13.ink_overlap` and `v14.glyph_body` on that
fixture. **Two thirds of the previously reported improvement on that one
fixture was the overlap.** Every other fixture is unchanged from D9's
table, including `two_stage_amp` (crossings 10 → 0, B 33 → 17, detour
1.8565 → 1.0794) and `rc_phase_shift` (B 19 → 6, F6 23 → 7).

Promotion remains an owner decision on the printed table (ADR-23 D4);
this ADR removes the Tier-0 veto, it does not exercise the escape.

### What was rejected

* **Touching the seed stride / clearance layer** (D9's own proposed
  repair, ADR-18 root cause 4). Not attempted: the diagnosis above shows
  spacing is not the mechanism — the incoming placement is legal at the
  spacing it has — and ADR-19's M3/M4 negatives wall spacing changes
  behind an explicit argument this change would not have.
* **Special-casing `sallen_key_lpf`, or tuning the root policy until the
  aggregate looked right.** That is the overfitting ADR-23 exists to
  prevent, and it would have left the champion-path hole open.
* **Adding a post-phase-4.5 legalize pass.** It would repair the symptom
  by *moving* an element after placement has finished — precisely the
  decoration-contract violation CLAUDE.md forbids ("once decoration
  starts, no symbol moves or rotates", and phase 4.5 itself "changes
  element orientation only, never position"). The guard is the right
  owner: refusing the pose costs nothing, since the pre-refinement pose
  is always available and always legal.

## ADR-26 — The two text models nothing calibrates were both drawn on the wrong side

**Status:** landed. Scope is one function in `kicad-symbols`, two in
`kicad-emitter`, and the two verifier-side copies. The default
(champion) path **changes** — this is not a no-op — and every cell that
moved improved. `baseline_lock`'s diff is EMPTY (no symbol moves or
rotates), no budget literal moved, no verifier was weakened, skipped or
`#[ignore]`d, and **seven** xfail registry entries expired and were
deleted.

### The report

ADR-25 left `--placer=flow-seed` promotable but carrying one
uncomfortable cell: `v13.ink_overlap` — the real `kicad-cli` SVG-ink
measurement, the only instrument that can falsify the model-side V13
family — went net **+2** (`common_emitter`, `opamp_inverting`,
`sallen_key_lpf` each 0 → 1; `rc_phase_shift` 1 → 0), while every
*model-side* V13 cell improved. Model and ground truth disagreed in
direction. That is the shape of a model defect, not a placement one, and
it was.

Two hypotheses were on the table: (1) the tighter packing outruns the
text-nudge candidate set, (2) a model-fidelity gap the nudge passes
cannot see through. **Hypothesis 2 held on two of the three, hypothesis 1
on the third**, and the two model defects turned out to sit on the
champion path as well.

| fixture | colliding pair (measured from ink) | model-side verifier saw it? |
| --- | --- | --- |
| `common_emitter` | `RE`'s Reference over `CE`'s pin-number "1" | **no** — V13(5) read 0 |
| `opamp_inverting` | the sheet's `vcc` port name under `#PWR2`'s "VCC" | **no** — V13(6a) read 0 |
| `sallen_key_lpf` | `C1`'s Reference under `#PWR4`'s "VEE" | yes — V13(4) = 1 |

### D1. Pin text rides *beside* the shaft, and the side is a page-frame fact

`Symbol::pin_text_local_bboxes` centred each visible pin label on the pin
*shaft*. KiCad draws outside pin text alongside it. Measured from
rendered ink at size 1.27: the band runs 0.42..1.69 mm off-axis for a
name or a lone number, and 0.31..1.58 mm for a number sharing a shaft
with a name (KiCad splits them onto opposite sides). The old box spanned
±0.89 mm about the axis — under-reserving the drawn side by ~0.8 mm and
reserving 0.89 mm of empty space on the other.

The margin that produced the defect: on `common_emitter` the emitter
scored `RE`'s Reference as clearing `CE`'s pin-number box by **0.002 mm**
(58.299 vs 58.301, after the pass's own 0.5 mm pin-text clearance), and
KiCad rendered the two overlapping by 0.17 mm.

The repair could not be a wider local box. KiCad's rule is stated in
*drawn* coordinates — "place it to the left of the pin", "above means
negative Y" (`eeschema/pin_layout_cache.cpp`) — and a rule of that shape
is **not rotation-covariant**: a box on the symbol-local −x side lands on
the world +x side under a 180° pose. A symbol-local model can therefore
only be correct by being symmetric, which is exactly the model that was
wrong. `Symbol::pin_text_page_bboxes(origin, orient)` replaces it, taking
the placed pose and returning page-frame boxes.
`pin_number_side_is_page_frame_not_symbol_frame` pins the argument on
`Device:C` at rot 0 and rot 180.

Inside names (`pin_names (offset > 0)`) were also mis-modelled — centred
on the body-root-plus-offset point rather than *starting* there and
reading inward. Corrected in the same function; verified against the
`OPAMP` symbol's `V+` / `V-` / `+` / `-` ink.

### D2. A sheet's port names are drawn INSIDE the sheet

`sheet_port_name_bboxes` — one copy in the emitter, one in the verifier,
both carrying a comment asserting it — modelled a hierarchical sheet's
port label as reading *outward*, into the empty strip beside the sheet.
KiCad draws it inside the sheet body, reading away from the edge, exactly
as a hierarchical label does: tag glyph at the anchor, string one
`hier_label` lead further along. On `opamp_inverting` the `vcc` port name
renders at x 50.0..52.7 for a pin anchored at x 48.26; the model claimed
45.1..48.3 — a mirror image, not an approximation.

So the obstacle set the power-glyph value-text nudge consults was empty
exactly where the port labels are and occupied exactly where they are
not. It relocated `#PWR2`'s "VCC" onto `vcc` while believing it had moved
it *off* an obstacle.

### D3. The candidate sweep, appended not reordered

`sallen_key_lpf` is the case the model *did* see: V13(4) reported it, and
`nudge_power_glyph_value_text` still shipped it, because all eight of its
cardinal candidates collided and it took the least-bad colliding anchor.
Sixteen offsets (diagonals, a third ring) are **appended** to that list —
never interleaved — so a glyph that already finds a clear cardinal keeps
the identical anchor and a layout that was already clean is byte-identical.
Only a glyph with no clear candidate at all can move, and today such a
glyph ships a rendered overlap. This is the same discipline
`property_offset_candidates` already documents for the host-text pass.

### What this cost the challenger's Tier-1 case, and why that is the honest number

Re-graded whole-suite, both sides collected after the fix:

| | ADR-25's table | after ADR-26 |
| --- | --- | --- |
| `v13.ink_overlap` | champion 2 (xfail'd), challenger 4; net **+2** | **0 on all 18 fixtures, both placers** |
| Tier 1 total Δ | −4.00 | **−1.00** |
| Tier 2 total Δ | −163.35 | −163.35 (unchanged) |
| Tier 0 | challenger clean | challenger clean; **champion carries `t0.cross_net_overlap`/`two_stage_amp` = 2**, which the challenger clears |
| verdict | PROMOTABLE | **PROMOTABLE** |

Tier 1 fell from −4.00 to −1.00 because **most of `flow-seed`'s Tier-1
advantage was these two model defects**, and the champion has now been
repaired of them too. The surviving −1.00 is `v13.2_label_prop` on
`two_stage_amp`; `v13.6a` and `v14.glyph_body` are each +1/−1 sideways
within their tier. Tier 2 is untouched at −163.35 — no wire was
re-routed, because both nudge passes rewrite a `(property … (at …))` and
nothing else.

Read the promotion case on Tier 2 and on the Tier-0 cell, then, not on
Tier 1. That is a *better* case than ADR-25's, not a weaker one: the
Tier-1 term it lost was never a placement property.

### The general finding: what nothing calibrates, drifts

`rendered_text.rs` exists because "a model cannot falsify itself". It
calibrates six text classes — plain / global / hierarchical label,
property Reference, property Value, power-glyph Value — against real ink,
asserting the model is a tight superset.

**Pin text and sheet-port names are the only two modelled text classes it
does not cover. Both were wrong, and both were wrong by a mirror
reflection, not by a tolerance.** The V13 budgets that consume them all
read 0 throughout, because emitter and verifier shared the same wrong
box; only the ink test could see it, and only indirectly, as an overlap
between two *other* strings.

> A text class that no ink calibration covers is not "modelled
> approximately". It is unfalsified, and this project has now found a
> mirror-image error in every such class it has looked at.

Extending `text_bbox_model_covers_rendered_ink` to those two classes is
the durable guard and is **not** done here — it needs the symbol library
in that test binary and its own measured epsilons. Recorded as owed.

### What was rejected

* **Widening the pin-text box symmetrically** to cover both possible
  sides. It is a strict superset and cannot be wrong, but it makes the
  *verifier* over-reserve by ~1.7 mm on the side KiCad provably never
  draws on — false positives on the champion, and a model that is
  conservative rather than faithful. Faithfulness was available: the side
  rule was measurable, and it was measured.
* **Special-casing any of the three fixtures**, or tuning the nudge until
  `v13.ink_overlap` read 0. The three defects have three different
  mechanisms and each repair is general; two of them fired on fixtures
  nobody was looking at (`rc_phase_shift`, `sallen_key_driven`,
  `cascode_amp`, `lc_ladder_lpf`, `wien_bridge_osc`, `two_stage_amp`).
* **Registering an xfail for the new ink overlaps.** That is deferral
  against the wrong stage: the overlaps were a model defect on the
  shipping path, not a challenger's cost.

## ADR-27 — A connected pin is an exit angle: junction-dot parity with KiCad's own rule

**Status:** landed for the *dot* half; the V16 `J` doctrine question is
**analysed and left open for the owner**, deliberately not decided here.
Scope of the code change is two functions in
`crates/spice-route/src/cleanup.rs` and their one call site. Placement is
untouched: `baseline_lock`'s diff is EMPTY, no wire is re-routed, and no
budget literal moved in any direction. One new verifier
(`crates/spice2kicad/tests/junction_parity.rs`); nothing weakened,
skipped or `#[ignore]`d, and no xfail added.

### The report

The project owner, reviewing rendered output:

> I also don't like the way the convertion avoid placing junction by
> connecting the componen to line going through directly. It much more
> clear to use T junction with the dot, even the current approach pass
> ERC and minimize the length and number of junctions.

Read as a style note this is arguable. It is not a style note. **We were
off-spec against KiCad's own junction rule**, and the file KiCad would
write differs from the file we wrote.

### KiCad's rule, from the source

`eeschema/junction_helpers.cpp::AnalyzePoint( items, p )` decides the
question, and `SCH_SCREEN::IsExplicitJunctionNeeded` /
`SCH_SCREEN::GetConnectionPoints` act on the answer. In order:

1. Collect every connectable item overlapping `p`. A `SCH_JUNCTION_T`
   hitting `p` sets `hasExplicitJunctionDot`.
2. **Merge collinear wires before counting** (`SCH_LINE::MergeOverlap`),
   so two abutting collinear segments become one line whose *interior*
   contains `p`. Skipped when a dot is already there.
3. Accumulate **exit angles** on the WIRES layer:
   * a wire `IsConnected(p)` — `p` is one of its endpoints — sets
     `breakLines` and contributes **one** angle;
   * a wire that merely hit-tests `p` is deferred to `midPointLines` and
     contributes **two** (forward + reverse) iff `breakLines` ended true;
   * `SCH_SYMBOL_T` / `SCH_SHEET_T` connected at `p` — i.e. **a pin lands
     there** — sets `breakLines` and contributes **one** angle, drawn
     from a separate counter (`uniqueAngle++`) so a pin at 90° can never
     alias a wire at 90°;
   * a label connected at `p` sets `breakLines` and contributes nothing.
4. `isJunction = exitAngles[WIRES].size() >= 3`.

`SCH_SCREEN::IsJunction`'s header states the same rule as five criteria,
and two of them are about pins:

> - One wire midpoint **and a symbol pin**.
> - Two or more wire endpoints **and a symbol pin**.

### Where we diverged

`cleanup.rs::rays_at` iterated `segments` only. A pin contributed
nothing. So a trunk running *through* a pin scored 2 rays where KiCad
scores 3 (pass-through 2 + pin 1), and every such node shipped undotted.
The symptom is exactly what the owner saw: a component tapped off a line
that runs straight past it, with no dot to mark the tap — and open the
sheet in eeschema, nudge any wire, and KiCad inserts the dots the file
omitted.

The fix passes the per-net own-pin set (which `trim_whiskers` and the
coalesce barrier already receive) into `add_connection_junctions`, and
counts `+1` exit angle per own-net pin at the point. `prune_stale_junctions`
takes the same set and applies the *same* predicate — it runs immediately
before the add pass, so a narrower rule there would prune a dot the next
pass is about to re-add.

**Why a raw per-segment count equals KiCad's merge-then-count.** KiCad
merges collinear wires and then scores a merged interior as two angles;
we score two collinear segments meeting end-to-end as 1 + 1. Every
configuration agrees: a merged interior is 2, an unmerged abutting pair
is 2, an L-corner is 2 either way (perpendicular lines never merge), and
a 4-ray cross is 4 either way. There is no point at which merging changes
the count, so the cheaper formulation is not an approximation.

### The verifier, and why it is not a re-implementation of the fix

`tests/junction_parity.rs` reconstructs KiCad's predicate over the
**emitted file** — ink read back off disk, pin coordinates re-derived
from the library through the emitted pose, in the shape
`roundtrip_connectivity.rs` established — and asserts the emitted
`(junction …)` set matches **exactly**, in both directions. It is not the
production rule's twin: production counts rays over the router's own
`Segment` list with the router's own pin set, while the verifier merges
collinear lines the way `AnalyzePoint` does, over geometry that has been
through page translation and serialisation, with pins derived
independently of `collect_net_pins`. Its mutation guard injects three
defects per fixture (erase every dot, dot in empty space, inject a T) and
requires each to be caught.

The predicate is evaluated on the file **as written, dots included**,
which is the self-consistent question: step 2 above makes the merge
itself conditional on the dot being present. A same-net perpendicular
crossing is exactly that case — its four split arms merge back into two
crossing lines *unless* the dot is there, so the dot is what makes the
point a junction, and that is precisely what eeschema writes when a user
dots a crossing by hand. Measured: zero spurious dots anywhere, so the
existing geometry was already self-consistent under this reading.

### Dots added, per fixture

Nineteen, on eight of the eighteen fixtures. Every one is a node where a
trunk passed through a pin; every one is a dot KiCad would have inserted.

| fixture                  | before | after | added |
| ------------------------ | -----: | ----: | ----: |
| `rc_phase_shift`         |      3 |     7 |    +4 |
| `two_stage_amp`          |      5 |     9 |    +4 |
| `cascode_amp`            |      3 |     6 |    +3 |
| `shunt_feedback_amp`     |      2 |     5 |    +3 |
| `lc_ladder_lpf`          |      2 |     4 |    +2 |
| `common_emitter`         |      3 |     4 |    +1 |
| `sallen_key_lpf`         |      3 |     4 |    +1 |
| `wien_bridge_osc`        |      3 |     4 |    +1 |
| `diff_pair`              |      1 |     1 |     0 |
| `multivibrator`          |      4 |     4 |     0 |
| `named_rails`            |      2 |     2 |     0 |
| `opamp_definition_level` |      2 |     2 |     0 |
| `opamp_inverting_real`   |      1 |     1 |     0 |
| `sallen_key_driven`      |      4 |     4 |     0 |
| `opamp_inverting`        |      0 |     0 |     0 |
| `port_shapes`            |      0 |     0 |     0 |
| `rc_lowpass`             |      0 |     0 |     0 |
| `rc_lowpass_ports`       |      0 |     0 |     0 |

V16 `(B, J)` is **unchanged on every fixture**. That is not luck: a dot
added at a trunk-through-pin point sits on a **2-ray** ink vertex whose
two rays are collinear, which V16 counts as neither a bend (that needs
one H + one V) nor a branch (that needs 3 rays, or 4 with a dot). No
ratchet moved anywhere in the suite.

### The one carve-out: KiCad's rule is net-blind, and we are not

Four points remain where `AnalyzePoint` fires and we deliberately emit no
dot, all on `two_stage_amp`: `(52.07, 87.63)`, `(57.15, 48.26)`,
`(57.15, 57.15)`, `(57.15, 87.63)`. They are the fixture's registered
`no_cross_net_collinear_wire_overlap` defect — the `b2`/`c2` trunks
sharing the collinear run at `x = 57.15` and `c2`/`e2` the one at
`y = 87.63` — rediscovered from geometry by a second instrument. **None of
them involves a pin**; each is one net's trunk passing through a point
where another net's trunk ends.

Dotting them is the wrong output, not the right one: KiCad breaks
segments at a junction, so the dot is what would convert the documented
*latent* short into a real one. The verifier therefore identifies them
structurally — points whose contributing wires span more than one ink
component under KiCad's endpoint-sharing rule — and holds them under a
zero-slack per-fixture ratchet (`CROSS_NET_CONTACT_POINTS`), which can
shrink but never grow. In a schematic without cross-net overlap the
carve-out is empty by construction: one net is one ink component, so
every junction point has exactly one. Duplicating the cross-net gate's
*assertion* here is the failure ADR-23 D2 warns about; recording the
count so it cannot grow silently is not.

### The V16 `J` doctrine question — measured, NOT decided

The architect's observation: V16 counts `J` = branch vertices, lower is
better, and that prices the *readable* form of a three-way node (trunk
ends at the node, stub taps it — a 3-ray T, `J+1`) **above** the implicit
one (trunk runs through the pin — 2 rays, `J`-free). The project has
conceded the point once already, in
`idioms.rs::apply_shared_centers`, which pays `+1 J` on `diff_pair` for a
proper T because "a T is the readable form of a three-way node".

The proposal: **redefine `J` to count only branch vertices NOT coincident
with a pin of the net.** Mid-air Steiner branching stays expensive
(genuinely confusing ink); pin-anchored Ts become free.

Measured on all eighteen fixtures, decomposing each fixture's `J` by
distance from the nearest pin **on the same ink component** (one net is
one ink component, and V11 forbids a foreign pin on our ink, so
"on this component" is "of this net"):

| fixture                  |  B |  J | J at pin | J 1 cell from own pin | J mid-air |
| ------------------------ | -: | -: | -------: | --------------------: | --------: |
| `rc_lowpass`             |  0 |  0 |        0 |                     0 |         0 |
| `common_emitter`         |  4 |  3 |        0 |                     0 |         3 |
| `multivibrator`          |  8 |  4 |        0 |                     0 |         4 |
| `diff_pair`              |  2 |  1 |        0 |                     1 |         0 |
| `opamp_inverting_real`   |  5 |  1 |        0 |                     0 |         1 |
| `opamp_inverting`        |  3 |  0 |        0 |                     0 |         0 |
| `port_shapes`            |  4 |  0 |        0 |                     0 |         0 |
| `rc_lowpass_ports`       |  0 |  0 |        0 |                     0 |         0 |
| `opamp_definition_level` |  6 |  2 |        0 |                     2 |         0 |
| `named_rails`            |  2 |  2 |        0 |                     2 |         0 |
| `rc_phase_shift`         | 19 |  3 |        0 |                     0 |         3 |
| `two_stage_amp`          | 33 |  9 |        0 |                     1 |         8 |
| `cascode_amp`            | 12 |  3 |        0 |                     3 |         0 |
| `lc_ladder_lpf`          | 16 |  2 |        0 |                     0 |         2 |
| `sallen_key_lpf`         |  6 |  3 |        0 |                     1 |         2 |
| `wien_bridge_osc`        | 10 |  3 |        0 |                     1 |         2 |
| `sallen_key_driven`      | 13 |  4 |        0 |                     0 |         4 |
| `shunt_feedback_amp`     | 12 |  2 |        0 |                     1 |         1 |
| **total**                |    | **42** |    **0** |                **12** |    **30** |

(The three right-hand columns are recorded by
`junction_parity.rs::report_pin_anchored_branch_share`, informational and
never asserted. The one-cell column is restricted to pins on the branch's
own ink component: without that restriction `two_stage_amp` scores 2, one
of which is a coincidentally-adjacent foreign pin — measure what you mean
to measure.)

**Three findings, and the first two change the shape of the proposal.**

1. **`J at pin` is 0 on every fixture.** Adopting the redefinition *as
   literally worded* — "not coincident with a pin" — would move no `J`
   literal anywhere. It is a **no-op on the current corpus**. There is
   nothing to sign off in terms of budget movement, and no fixture would
   get cheaper.

2. **The reason is V5, and it is structural.** Every pin-anchored branch
   in the suite sits *one grid cell off* the pin, never on it, because
   V5's outward rule says a wire leaves a pin along the pin's axis before
   it turns. That is not an accident of the current router: it is the
   documented precedent. `diff_pair`'s owner-approved `J 0 → 1` is
   recorded as `apply_shared_centers` "reserving a grid cell of vertical
   stub under the tail trunk, so the three-way node is a proper Steiner T
   instead of the trunk stopping sideways on RTAIL's pin". The readable T
   the project already paid for is exactly the case the proposed wording
   does not reach.

3. **Reworded to "within one outward stub of a pin of the net", the rule
   bites — on 12 of 42 branch vertices, across 8 of 18 fixtures.**
   `J` would fall `diff_pair` 1→0, `opamp_definition_level` 2→0,
   `named_rails` 2→0, `cascode_amp` 3→0, `two_stage_amp` 9→8,
   `sallen_key_lpf` 3→2, `wien_bridge_osc` 3→2, `shunt_feedback_amp` 2→1.
   The remaining 30 are genuinely mid-air Steiner branching — the ink the
   redefinition means to keep charging for — so the metric survives the
   change with two thirds of its mass intact. This is a real, measurable
   proposal, unlike the literal one.

**Recommendation** (the decision is the owner's; nothing is adopted in
code, and `docs/invariants.md` V16's definition is unchanged):

* **Adopt the intent; reword the predicate.** "Not coincident with a pin"
  should read "not anchored on the net's own terminal geometry" —
  operationally, within one grid cell of a pin **on the branch's own ink**.
  Coincidence alone is the wrong test because V5 owns the first grid step
  out of every pin, and the project has already ratified the one-cell
  shape once.
* **Land it as a definition change with an explicit re-measurement, not
  as eight ratchet improvements.** Those eight literals would drop
  because the metric changed, not because the drawing did; recording them
  as ordinary ratchet wins would corrupt the one instrument that tells
  drift from progress. If it is taken, the `BEND_BRANCH_BUDGETS` table
  should be re-measured in one commit that says so.
* **The trigger to take it is a change it unblocks.** The pressure the
  architect diagnosed is real but currently latent: no router or placer
  change is presently blocked by a `J` ratchet on a node it wants to draw
  as a proper T. Waiting for the first one costs nothing now that the
  measurement exists, and it buys the amendment a concrete before/after
  instead of an argument from anticipation. In the meantime the 30
  mid-air branches are the fixtures' real `J` mass and the ratchet keeps
  working on them.

### What this change explicitly did NOT do

**Reshape routes.** With dots present, a trunk passing through a pin is
legitimate KiCad idiom and reads correctly. Converting those nodes into
stub-and-T shapes is a router change under ADR-16's full baseline-diff
protocol, for uncertain gain, and it is the change the `J` question above
would have to be settled *before* attempting. Ship the dots, re-ask.

### What was rejected

* **Making the parity verifier net-aware in order to assert on the
  cross-net points.** KiCad's predicate is net-blind by construction — it
  derives nets *from* geometry — so a net-aware "parity" check would not
  be parity with anything. The structural component test says the same
  thing without inventing an authority.
* **Registering an xfail for the four cross-net points.** They are not a
  new defect and not a challenger's cost: they are one already-registered
  defect seen through a second instrument, and a ratchet that reads 4
  today and must read 0 the day the channel router lands is a stronger
  statement than an expiring exclusion.
* **Emitting a dot at every own pin on a wire.** That is the rule KiCad
  does *not* implement, and it would dot every ordinary two-terminal
  connection. The count matters: a pin where a wire merely *ends* is two
  exit angles, not three.

## ADR-28 — Measure what the eye reads first: chain-axis uniformity and shared-current-path stacking

**Status:** landed, **informational**. Two metrics, five registered
metric ids, one new verifier
(`crates/spice2kicad/tests/readability_metrics.rs`). Purely additive:
`baseline_lock`'s diff is EMPTY, no budget literal moved, no existing
verifier changed. Neither metric is a ratchet, and neither carries
aggregate weight in the ADR-23 promotion rule.

### The gap

Three times the project's instruments have scored as an improvement
something the owner, on sight, called damage:

1. the SA scored destroying a textbook LC-ladder drawing as a **2.7×
   cost improvement** — it rotated series elements off-axis to buy
   wirelength, and its objective has no orientation term at all;
2. phase 4.5 scored mangling two inductors as a **strict V5 win**, and
   was correct by its own metric, at frozen positions;
3. the ADR-23 aggregate scored `flow-seed` the largest improvement the
   instrument had recorded, and the owner then said the **champion is
   better on 4 of 18 fixtures**.

Every one of those is a true statement about the metric that made it.
The common cause is not a weighting: **no registered metric measures
axis consistency, orientation uniformity, or device stacking**, which is
what a reader picks up before reading a single refdes. ADR-23's own
"Known limits" says it plainly — a property no verifier measures is
invisible to the aggregate exactly as it is invisible to the ratchets —
and the promotion's "two blind cells" post-mortem says what an unmeasured
cell costs in practice. So the fix is a *measurement*, not a coefficient.

Both metrics read the emitted `.kicad_sch` and re-derive their structure
from the netlist. Neither imports a classification from `spice-layout`,
following the `flow_geometry.rs` precedent: a metric that borrows the
placer's own model can only restate it, never falsify it.

### Metric A — series-chain axis uniformity (`chain.axis`, `chain.reversal`)

A **series chain** is a maximal path of two-terminal signal elements
linked by nets of **signal degree 2**: each interior net touches exactly
two drawn, non-rail-stub elements, so the signal leaving one member has
nowhere to go but into the next. Chain candidates are the drawn,
two-pin, *series-signal* elements — two terminals, not a power source,
NEITHER node rail-class — which is `flow_geometry.rs`'s F5
discriminator, re-derived locally. Every candidate has at most two nets,
so every vertex has chain-degree ≤ 2 and each component is a path or a
cycle by construction.

For each chain the metric picks the axis (horizontal or vertical) that
reads the drawing **most charitably** and reports two counts:

* **`chain.axis`** — members whose drawn pin axis differs from that
  axis;
* **`chain.reversal`** — members that ARE on that axis but travel
  *against* the chain's majority direction, where a member's travel is
  the sign of `exit_pin − entry_pin` and entry/exit are the nets the
  chain arrives on and leaves by (endpoints use their free net for the
  missing side).

`chain.members` is registered as the denominator, so a zero that means
"clean" is distinguishable from a zero that means "no chain here".

**The specimen.** `lc_ladder_lpf`'s `RS → L1 → L2 → L3` is one chain.
The shipping placer emits it at rotations 180 / 90 / 0 / 270 — one
chain, four orientations — and scores `(axis 2, reversal 1)`. The
deterministic seed (`--no-refine`) emits all four at 90 and scores
`(0, 0)`. That is the ranking the metric exists to reproduce, and it
does.

### Metric B — shared-current-path stacking (`stack.side_by_side`)

Devices in series on a DC current path — a cascode's two transistors, a
collector load above its transistor, a rail-to-rail bias divider — are
conventionally **stacked in Y**. The current runs down the page from
supply to ground, and the stack is how a reader sees that it is one
current.

A **DC-series pair** is two drawn elements `u`, `v` such that

1. each conducts DC between two *distinct* rail nets — there is a path
   supply → … → `u` → … → ground in the DC graph that does not re-use
   `u`'s own edge; and
2. they share a non-rail net `N` whose **DC degree is exactly 2**, so
   all of `u`'s current flows into `v`.

`stack.side_by_side` counts the pairs drawn wider than tall
(`|dx| > |dy|` between element centres, centre = mean of emitted body
pins). `stack.pairs` is the denominator.

The DC graph is deliberately narrower than the netlist graph:

* a **capacitor** carries no DC and contributes no terminal at either
  end — which is why an RC low-pass's `R1`/`C1` is never asked to stack;
* a **BJT base / FET gate** is not a conductor (SPICE order `c b e` /
  `d g s`), so the current path is collector–emitter / drain–source.
  Counting a base would raise the DC degree of every bias node and
  dissolve exactly the rail-to-rail divider this metric exists to see;
* an **op-amp symbol or hierarchical `(sheet …)` instance** conducts DC
  at its pins without having a single current path *through* it: it gets
  no DC edge, but its terminals DO count toward a net's DC degree. That
  asymmetry is load-bearing — without it `opamp_inverting`'s virtual
  ground reads as degree 2 and `RIN`/`RF` are reported as a series pair
  they are not.

**Clause 2 is what keeps the metric from demanding nonsense**, and the
suite's own fixtures prove it in both directions. `cascode_amp`'s
`Q1`/`Q2` share `c1`, whose only DC conductors are those two: one
current, one column. `diff_pair`'s `Q1`/`Q2` share `tail` with `RTAIL`,
so that net has DC degree 3, the current *splits*, and the conventional
drawing is side by side — and the metric does not count it.
`stacking_discriminator_separates_the_cascode_from_the_diff_pair` is the
assertion that keeps that honest; it is metric B's analogue of F5's
`series_discriminator_separates_stub_from_series_on_common_emitter`.

**The specimen.** `cascode_amp` presents five DC-series pairs —
`RC/Q2`, `Q2/Q1`, `Q1/RE`, `RB1/RB2`, `RB2/RB3`, which is exactly the
structure the fixture's own header claims ("Q2's emitter sits on Q1's
collector, and the three-resistor bias chain RB1/RB2/RB3 is a vertical
ladder from the rail to ground"). Under `--placer=champion` all five are
stacked, score **0**. Under the shipping `flow-seed`, `Q1`/`Q2` are
10.16 mm apart in X and 1.27 mm in Y, score **1** — which is the owner's
stated reason for preferring the champion on that fixture.

### The measured table

Whole fixture set, both placers, `readability_metrics.rs` with
`S2K_READABILITY_DUMP=1`. `A` = `chain.axis`, `R` = `chain.reversal`,
`M` = `chain.members`, `S` = `stack.side_by_side`, `P` = `stack.pairs`.

| fixture | A | R | M | S | P | note |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| rc_lowpass | 0 | 0 | 0 | 0 | 0 | |
| rc_lowpass_ports | 0 | 0 | 0 | 0 | 0 | |
| common_emitter | 0 | 0 | 0 | 0 | 3 | RC/Q1, Q1/RE, RB1/RB2 all stacked |
| multivibrator | 0 | 0 | 0 | 0 | 2 | |
| diff_pair | 0 | 0 | 0 | 0 | 2 | Q1/Q2 correctly NOT a pair |
| opamp_inverting | 0 | 0 | 0 | 0 | 0 | |
| opamp_inverting_real | 0 | 0 | 0 | 0 | 0 | |
| port_shapes | 0 | 0 | 3 | 0 | 0 | uniform chain, drawn vertical (F5's business) |
| opamp_definition_level | 0 | 0 | 0 | 0 | 0 | |
| named_rails | 0 | 0 | 0 | 0 | 0 | |
| rc_phase_shift | 0 | 0 | 4 | 0 | 2 | `R1→R2→R3→CIN` all horizontal, one direction |
| two_stage_amp | 0 | 0 | 0 | 0 | 6 | |
| **cascode_amp** | 0 | 0 | 0 | **1** | 5 | `Q1/Q2` side by side — champion scores **0** |
| **lc_ladder_lpf** | **2** | **1** | 4 | 0 | 0 | `RS→L1→L2→L3` at four rotations |
| sallen_key_lpf | 0 | 0 | 0 | 0 | 0 | |
| wien_bridge_osc | 0 | 1 | 2 | 0 | 0 | `RS`/`CS` both horizontal, opposed |
| sallen_key_driven | 0 | 0 | 0 | 0 | 0 | |
| shunt_feedback_amp | 0 | 0 | 0 | 1 | 2 | `RB`/`RF` — see the ambiguities below |

**Champion vs the shipping default, across all 18 fixtures × 5 metrics:
exactly ONE cell differs**, `stack.side_by_side / cascode_amp`, champion
0 → flow-seed 1. `lc_ladder_lpf` is byte-identical between the two
placers (it has a real drawn source and never enters the rail-rooted
fallback, as the promotion section records), so its `(2, 1)` is a
property of **phase 4.5**, not of the layering swap — which is why its
control arm is `--no-refine`, not `--placer=champion`.

That thinness is itself a finding. The owner says the champion is better
on four of eighteen fixtures; these two metrics see **one** of them. They
close a named gap, they do not close the gap.

### Informational at birth, and what would promote each

Both are `Tier::Info` in `tests/scoreboard.rs`: printed per fixture, zero
weight in the `(T1, T2)` aggregate, no per-fixture budget literal. That
follows the project's own precedents — Q6's balance CoV and ADR-23 D8's
V16 bend bound — and the reason is D8's: *a bound that were subtly wrong
would, as a gate, block all work while being wrong.* The ambiguities
below are unresolved, and a metric that can be wrong about a **correct**
drawing must not be able to reject one.

The only assertions in the new verifier are therefore

* **synthetic control arms** — hand-placed pin geometry with a known
  answer, so the metrics cannot silently degenerate to "always 0"
  (`chain_metric_counts_a_known_synthetic_ladder`,
  `stacking_metric_counts_a_known_synthetic_divider`); and
* **specimen rankings in `≤` form** — they fire only if the arm the
  owner prefers becomes strictly WORSE than the arm they reject, i.e.
  only if the metric's own validation inverts. They can never block a
  change that improves the shipping placer.

**What would justify promoting metric A to a ratchet.** (a) The
corner/fold ambiguity below is closed — either a fixture with a
legitimately folded chain exists and the metric is shown to grade it
correctly, or the metric is taught to allow one fold; and (b) the
placer can actually reach 0 on `lc_ladder_lpf` with refinement ON, so
the ratchet records an achieved state rather than an aspiration. At that
point it becomes a per-fixture zero-slack literal exactly like F5, and
Tier 2 (a continuous-ish aesthetic gradient, by CLAUDE.md's
constraints-vs-costs decision rule).

**What would justify promoting metric B.** (a) The feedback-resistor
ambiguity below is closed, so `shunt_feedback_amp`'s cell is known to be
a defect rather than a definitional artefact; and (b) at least one more
fixture exercises a stack, so the ratchet is not a single-fixture
statement. Metric B is more nearly categorical than A — "these two
devices carry one current, so draw them in one column" is a yes/no
geometric fact — so it is the better ratchet candidate of the two, and
plausibly Tier 1 rather than Tier 2 when it gets there.

Neither may become a **weighted** term in `cost.rs`. That is the V16
doctrine applied to a new metric: subordination by coefficient is not
subordination, and a tunable term at a safe weight does nothing (the
Attempt-A failure). If either graduates, it graduates as a per-fixture
count with a tier, or as a lexicographic key — never as a weight.

### The ambiguities, recorded rather than silently resolved

Where a choice was genuinely arguable it is listed here with the
alternative, per the project's rule that a deferral written against the
wrong reason never expires.

1. **A two-element chain.** Included. A chain of two has no "majority",
   so the metric arbitrarily names one member — but the *count* is
   symmetric (1 either way), so only the message is arbitrary, not the
   number. Excluding them would have made `wien_bridge_osc`'s `RS`/`CS`
   invisible, and two series elements drawn in opposite directions is a
   defect the eye catches immediately. *Alternative:* require ≥ 3
   members, and report 2-chains separately.

2. **Mirror as reversal.** Not counted as such. The metric never reads
   the `(mirror …)` field; it reads where the entry and exit **pins**
   landed. A mirror that reverses the drawn pin order IS counted (it
   changes the travel vector), and a mirror that does not is not
   (it changes nothing a reader sees). Grading the field rather than the
   geometry would flag mirrors that are invisible on the sheet.

3. **A chain that legitimately turns a corner.** Counted as a violation
   — this is the definition's weakest point, and the main reason metric
   A is informational. A long chain folded to fit an A4 page is good
   practice, and the metric would score the fold. No fixture folds a
   chain today (`lc_ladder_lpf` is the longest at four members and does
   not need to), so the case is unmeasured rather than mis-measured.
   *Alternative:* allow one axis change per chain free, or allow a
   change at a member whose entry and exit both stay on the page's long
   axis. Not chosen, because a free fold also excuses the
   `lc_ladder_lpf` defect the metric exists to see.

4. **Charitable axis selection.** For each chain the metric evaluates
   BOTH axes and keeps the reading that minimises `(off_axis,
   reversals)` lexicographically, breaking a tie toward **horizontal**
   (the project's own F3/F5 convention is that signal flows left to
   right). So the metric never invents a violation by insisting on an
   axis the drawing did not choose. *Alternative:* fix the axis to
   horizontal always — rejected, because it would double-count the
   `port_shapes` chain, which is uniform and merely vertical, and
   verticality is already F5's business. **A and F5 are complementary,
   not redundant:** F5 asks "is this series element horizontal and
   pointing downstream?", A asks "do the members of one chain agree with
   *each other*?". `port_shapes` scores A = 0 and F5 = 3; that is
   correct in both.

5. **Axis and reversal are disjoint.** An off-axis member is counted
   once, under `chain.axis`, and is not also counted as reversed. One
   element cannot be blamed twice for one pose, and the split is what
   lets a reader tell "the ladder zig-zags" (an axis defect) from "one
   inductor is drawn backwards" (a direction defect) — different repairs
   in the placer. *Alternative:* one combined "non-conforming members"
   count. Rejected: it would hide which repair is needed.

6. **Element centres, not shared pins, decide stacked-vs-side-by-side.**
   "Laid out side by side" is a statement about where the bodies sit,
   which is what a reader sees; measuring the shared-net *pins* instead
   would score a correctly stacked pair as side-by-side whenever the
   router took a jog. *Alternative:* the shared pin pair. Not chosen.

7. **The exactly-diagonal pair.** `|dx| == |dy|` is NOT counted. It is
   genuinely ambiguous, and an informational metric should not invent a
   defect out of a tie.

8. **A feedback resistor on a DC path.** Counted.
   `shunt_feedback_amp`'s `RB`/`RF` share `b` at DC degree 2 and both sit
   on a supply-to-ground path, so they satisfy the definition — but `RF`
   is a collector-to-base feedback resistor, and the textbook drawing
   runs it *along the feedback direction*, not stacked. This cell is
   **1 on both placers** and is the clearest known candidate for a false
   positive. *Alternative:* exclude pairs where one member is a feedback
   arc in the flow graph. Not implemented, because it drags flow
   direction into a metric that is otherwise pure DC topology, and
   because F0-class fixtures with real feedback are exactly what would
   let the question be settled with measurement rather than argument.

9. **Distinct rail nets, not rail polarity.** Clause 1 asks for two
   *distinct* rail nets rather than "one supply and one ground", so it
   works unchanged on `named_rails`' `p5`/`n5` and on any future
   split-supply fixture without a polarity table. The cost is that two
   different *names* for one physical rail would read as distinct; no
   fixture does that today (ground is always `0`).

10. **A cycle has no endpoint.** A chain component that is a cycle (no
    degree-1 member) is walked from its lexicographically smallest
    member, so the result is deterministic. No fixture presents one; the
    choice is recorded so a future oscillator ring does not surprise
    anyone.

### One aggregator change, and why it is not a behaviour change

`tests/scoreboard.rs` now prints `(info)` in the Δ column for a
`Tier::Info` row instead of `+0.00`. ADR-23 D6 recorded that exact
defect as a live follow-up — `q6.cov` printed `Δ = +0.00` for a value
that moved 1.2247 → 1.4142, which reads as "unchanged" — and it bites
harder now that five of the registered ids are informational. The
contribution really is zero, so nothing about the aggregate, the
verdict, or the promotion rule changes; only the column that was lying
about it.
