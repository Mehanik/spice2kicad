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

## Repository layout

```
crates/
  spice-parser/    SPICE source → typed AST (chumsky-based)
  kicad-emitter/   AST → KiCad S-expressions
  spice2kicad/     CLI binary (clap)
docs/
  annotation-spec.md   The annotation language. Authoritative.
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

4. **Use SPICE's own structure for structure.** We deliberately have
   no `*@group` directive. Clustering is expressed via `.subckt`
   (hierarchical sheet) and `.include` (visual cluster).
   Re-inventing grouping inside comments duplicates what the
   language already provides.

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
- **Layout.** Currently stubbed. The constraint resolver from
  spec §5 lives between the parser and the emitter.
- **Diagnostics.** Use `ariadne` for source-spanned error rendering.
  Every error/warning code in spec §7 should round-trip through
  `ariadne` with the offending line highlighted.
- **Bare `\r` line endings.** The lexer strips `\r` only when it
  precedes `\n` (CRLF). Bare `\r` (legacy Mac line endings) is
  treated as part of the line. This matches ngspice
  (`inpcom.c:1864`) and means files using only `\r` would parse
  as a single physical line. Convert legacy files before feeding
  them in. See `tests/edge_inputs.rs::bare_cr_line_endings` and
  `tests/edge_inputs.rs::lone_cr_in_middle_of_line`.
- **Dangling `+` continuation at unusual positions.** A `+`
  continuation line with nothing to continue (e.g. as the first
  non-title line of a file, or immediately after a `*@` block
  annotation) is parsed as a code line whose first token is `+`,
  producing an `ElementKind::Other` element with refdes `"+"`.
  Benign in practice but visible to downstream passes; emit
  error/warning diagnostics here once the parser has policy
  support for them. See
  `tests/edge_inputs.rs::continuation_at_start_of_file` and
  `tests/edge_inputs.rs::continuation_after_block_annotation_only`.
- **Numeric overflow is silent.** Values beyond `f64::MAX` parse
  to `Value::Number(f64::INFINITY)` (matching ngspice's
  `INPevaluate`). Downstream emitters should guard with
  `is_finite()` when serialising. See
  `tests/edge_inputs.rs::number_overflow_input`.
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

## Visual quality invariants

Project-level acceptance criteria for any emitted `.kicad_sch`.
These are not part of the user-facing annotation language; they
are falsifiable properties a checker can measure on the output.
Every invariant has a verifier — the test that enforces it. The
verifiers are being added in a parallel work stream; their names
below describe intent.

- **V1 — Symbols render visibly.** Every emitted `.kicad_sch` opens
  in eeschema with all components drawn at non-zero extent (no
  invisible glyphs, no missing graphics). The common failure mode
  is a `(symbol …)` instance whose `lib_id` resolves to an empty
  or stub library entry, so the body has no `(rectangle …)` /
  `(polyline …)` graphics. Verified by an SVG-export glyph-count
  test: render with `kicad-cli sch export svg`, count drawn glyphs,
  assert one per placed `Symbol`. Lives downstream of
  `crates/kicad-emitter/src/schematic.rs`.

- **V2 — Zero ERC errors.** `kicad-cli sch erc` on every emitted
  `.kicad_sch` reports zero errors. Warning policy is **TBD**:
  warnings are tolerated for now, errors are blocking. Verified
  by a fixture-driven integration test that runs `kicad-cli sch
  erc` on every example under `examples/` and asserts the report's
  `errors` count is zero. Tolerated-warning policy is tracked in
  spec §9.

- **V3 — `lib_symbols` are inlined verbatim.** Library entries
  emitted under `(lib_symbols)` are byte-for-byte copies of the
  corresponding `.kicad_sym` body, modulo `lib_id` name
  normalization. Rationale: portability — a consumer opening the
  emitted file must not need the same libraries installed at the
  same path. Implementation is the `Symbol::body` raw passthrough
  described in "Implementation notes". This decision is final for
  v0.1; revisiting is a v0.2 concern. Verified by a round-trip
  test that re-parses the source `.kicad_sym`, locates each used
  symbol in the emitted file's `(lib_symbols)`, and asserts byte
  equality of the body sub-tree.

- **V4 — Wires for connectivity, ≤ 2 labels per net.** Pins on the
  same net are connected by `(wire …)` segments emitted by the
  placer / router. `(global_label …)` and `(label …)` are reserved
  for: (a) `*@power` rails; (b) hierarchical-sheet ports
  (cross-sheet); (c) named nets the user explicitly tagged or that
  span otherwise unreachable regions. **Hard rule:** at most two
  labels carry the same net name on a single sheet — one at each
  terminal of a "label jump" (typical KiCad practice for
  un-routable connections). Three or more coincident labels for one
  net is a defect, not a style preference (cf. commit `22cb630`).
  Hierarchical-sheet pins are exempt — they're the cross-sheet
  boundary. Verified by a per-sheet label-tally test that scans
  emitted `(label …)` / `(global_label …)` nodes and asserts no
  net name occurs more than twice on the same sheet.

- **V5 — Pin-facing orientation.** For any two adjacent placed
  elements that share a net, the placer must choose orientations
  (rotation / mirror) such that the pins on the shared net are the
  closest pair — i.e. the chosen orientations minimise the
  Manhattan distance between the two pin positions on the shared
  net, subject to the grid (1.27 mm) and 90°-rotation /
  mirror-only orientation set (ADR-3). Default identity orientation
  for every element is the current behaviour and is the symptom
  this invariant exists to flag: it puts R1's `out` pin and V1's
  `out` pin on opposite sides of the layout, forcing a long
  trunk wire across the schematic
  (`/tmp/spice2kicad-demo/rc_lowpass/rc_lowpass.kicad_sch`).
  This is a *quality* metric, not a hard correctness invariant —
  a wire-routed schematic with bad orientations is still
  electrically correct, just ugly. Verified by a wire-length test:
  for each two-element internal net, the total emitted `(wire …)`
  length on that net is bounded by a small multiple of the larger
  symbol's bounding-box diagonal (a fixture-specific threshold,
  e.g. ≤ 30 mm for `rc_lowpass`'s `out` net — see
  `crates/spice2kicad/tests/placement_quality.rs`). Lives
  downstream of `crates/spice-layout/src/` (the placer chooses
  orientation; the router measures the consequence).

- **V6 — Topology-aware placement.** When the resolved netlist
  matches a recognised analog topology archetype (common-emitter
  amplifier, common-source amplifier, differential pair, current
  mirror, voltage divider, RC filter ladder, op-amp inverting /
  non-inverting, …), the placer must position the matched
  subgraph per a built-in template that mirrors how the topology
  is *traditionally drawn*. Concretely:
    - **Power rails run horizontally**: positive supply
      (`*@power`-marked sources, `Vcc`/`Vdd`) at the top, ground
      (net `0` and `.global`) at the bottom of the matched
      subgraph's bounding box.
    - **Signal flows left-to-right.** Designated input nets sit
      on the left, output nets on the right; intermediate stages
      between them.
    - **Bias networks cluster on the input side** of the active
      device they bias (base bias divider sits to the left of the
      BJT, gate bias to the left of the FET, etc.).
    - **Decoupling and bypass capacitors sit beside their
      associated active device** (emitter-bypass cap next to the
      emitter resistor, supply-decoupling cap next to the rail it
      decouples), not floating in a separate cluster.
  Like V5 this is a **quality** invariant, not a correctness one
  — a force-directed hairball is electrically valid but unreadable;
  V6 is what makes the output recognisable as the schematic an
  engineer would draw by hand. V6 *builds on* V5: V5 ensures
  pins on a shared net face each other; V6 ensures the components
  themselves are placed in the conventional positions in the
  first place.
  Verifier: a structural test on each archetype fixture. For the
  common-emitter fixture (`tests/fixtures/common_emitter.cir`,
  refdes `Q1`, `R1`/`R2` base divider, `RC` collector, `RE`
  emitter, `CE` bypass, `CIN`/`COUT` AC coupling) it asserts
  (a) at least two distinct Y bands corresponding to Vcc-rail and
  GND-rail elements (RC's top and RE's bottom are not coplanar
  with Q1's centre); (b) Q1 sits vertically between RC (above)
  and RE (below); (c) signal-flow X-ordering
  `VIN.x < CIN.x < Q1.x < COUT.x`. Scope: V6 is a v0.2+ direction
  — v0.1 may emit a correct but topology-blind layout even with
  V5 satisfied. The archetype matcher is expected to grow inside
  `crates/spice-layout/src/` (a new pass between policy
  resolution and the existing four placement phases — see
  annotation spec §5).

- **V7 — Symmetry-aware placement.** When the placer detects a
  structural symmetry in the netlist — a refdes pairing under which
  the resolved netlist is graph-isomorphic, modulo node renames —
  elements in mirrored pairs must be placed at mirrored coordinates
  about a single common axis (vertical or horizontal), with mirrored
  orientation. The classic motivating fixture is the symmetric
  astable multivibrator (`tests/fixtures/multivibrator.cir`): the
  pairing `Q1↔Q2, RC1↔RC2, RB1↔RB2, C1↔C2` makes the netlist
  isomorphic to itself, and the conventional schematic mirrors the
  whole circuit about a vertical axis through its centre, making the
  cross-coupling visible as two diagonal wires. V7 *builds on* V6:
  many archetype templates (differential pair, current mirror,
  long-tailed pair, multivibrator) have symmetry baked in, but V7
  applies more broadly — any subgraph whose graph automorphism
  group is non-trivial benefits.
  Verifier: a structural test on the multivibrator fixture that,
  with `axis_x = (Q1.x + Q2.x) / 2`, asserts (a)
  `|RC1.x - axis_x| == |RC2.x - axis_x|`,
  `|RB1.x - axis_x| == |RB2.x - axis_x|`,
  `|C1.x  - axis_x| == |C2.x  - axis_x|`
  (each within one grid cell, 1.27 mm), about the **same** axis;
  (b) each mirrored pair shares its Y coordinate (the symmetry axis
  is vertical, so `Q1.y == Q2.y`, `RC1.y == RC2.y`, …); (c) Q1 and
  Q2 carry mirrored orientations — same rotation, but exactly one
  of the two has a `(mirror y)` token in its `(symbol …)` instance
  (so the BJT arrows point toward each other). Today's placer
  arranges the eight elements left-to-right with equal stride,
  which makes *pairwise* distances equal but does **not** put the
  pairs on a common axis (RB1/RB2 and C1/C2 sit far to the right
  of the Q axis), so verifier (a) fails by roughly one cell width
  per pair. Scope: v0.2+ quality metric. The symmetry detector is
  expected to live in `crates/spice-layout/src/`, alongside (and
  composing with) the V6 archetype matcher — likely as an extra
  pass that runs after archetype matching and before phase 4
  auto-fill (annotation spec §5).

- **V8 — Standard symbol mapping for subckts.** A SPICE `.subckt`
  whose top-level instantiation `X<n>` carries a `*@symbol <lib_id>`
  directive (either as a trailing `;@ symbol=…` tag on the X line
  or as a block `*@symbol <lib_id> for=X<n>` directive) renders that
  single library symbol at the placement, with `pinmap=` mapping the
  subckt port order to the symbol's pin numbers (or names). The
  `.subckt` body is treated as a SPICE-side simulation model only —
  it is **not** emitted as a hierarchical sheet, no child
  `<subckt>.kicad_sch` file is written, and no `(sheet …)` block
  appears on the parent. The default behaviour for a `.subckt` with
  no `*@symbol` override on its instances is unchanged: each
  top-level `X<n>` becomes a hierarchical sheet (commit `4a9f062`
  feat(parser): wire pipeline end-to-end through placed-symbol
  emitter). V8 is a *refinement* of that default — the user opts in
  per X instance (or per subckt definition via `for=`).
  Motivating fixture: `tests/fixtures/opamp_inverting.cir` today
  emits `OPAMP.kicad_sch` as a child sheet with a single VCVS inside;
  `tests/fixtures/opamp_inverting_real.cir` adds
  `*@symbol Amplifier_Operational:OPAMP for=X1 pinmap=…` and expects
  a real triangle symbol on the parent instead.
  Today the resolver's `has_explicit_symbol_tag` only inspects
  trailing `Tag::Symbol(_)` tags on the element itself
  (`crates/spice-resolve/src/lib.rs`); block `*@symbol … for=X1`
  matches the resolver's per-element symbol resolution but does
  **not** suppress the `SheetInstance` routing decision that runs
  before symbol resolution. Closing this gap is the V8 work.
  Verifier: parse the resulting parent `.kicad_sch` and assert
  (a) a `(symbol …)` instance with the requested `lib_id` (e.g.
  `Amplifier_Operational:OPAMP`) at refdes `X1`; (b) NO
  `(sheet …)` block named after the subckt on the parent; (c) NO
  `<subckt>.kicad_sch` file written into the output directory; (d)
  the symbol's pin world positions are wired (or labelled per V4)
  to the same parent-sheet nets that X1's terminals reference in
  SPICE. Verifier lives at
  `crates/spice2kicad/tests/symbol_mapping.rs`.
  Interaction with V6 (topology archetypes): once V6 ships, the
  archetype matcher can recognise canonical opamp subckt patterns
  (single VCVS or two-pole with port names like
  `inp inn vcc vee out` / `+ - V+ V- OUT`) and auto-promote them
  to the standard symbol without the user writing `*@symbol`. V8
  is the explicit-override floor; V6's archetype matcher is the
  zero-annotation ceiling.

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
