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
   right), constraining direction rather than axis. **It was deliberately
   NOT attempted**, because failure mode 2 blocks landing either way.
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
