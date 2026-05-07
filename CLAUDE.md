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

- **V6 — Structural layered placement.** The placer must infer a
  readable layout from net structure alone — without matching named
  topologies — via a three-stage pipeline:
    1. **Net classification.** Every net is labelled Power (connected
       to a `*@power`-marked source or net `0`/`.global`), Ground
       (net `0` and `.global`), or Signal. Classification requires
       only the resolved netlist; no topology recognition.
    2. **Y-band assignment.** Each element is assigned a vertical band
       (Top / Mid / Bot) based on which net classes touch it: elements
       exclusively on Power nets go to Top; elements exclusively on
       Ground nets go to Bot; everything else goes to Mid. Power and
       Ground rails therefore run horizontally at the top and bottom
       of the sheet, and active circuitry lives in the middle — the
       universal analog schematic convention.
    3. **X-layer assignment.** Within each Y band, elements are
       ordered left-to-right by signal-flow depth. Depth is computed
       via Tarjan SCC collapse (to handle feedback loops) followed by
       longest-path layering on the resulting DAG. Input-side elements
       (sources, driving pins) receive the lowest layer numbers;
       output-side elements receive the highest.
    4. **Cost-function refinement.** After band/layer seeding, an SA
       pass refines positions using a penalty function that includes
       band-misalignment, soft Y-position, layer-order, and crossing-
       approximation terms. SA runs by default via
       `LayoutOptions { refine: true, .. }`.
  Like V5 this is a **quality** invariant, not a correctness one —
  a force-directed hairball is electrically valid but unreadable;
  V6 is what makes the output recognisable as the schematic an
  engineer would draw by hand. V6 *builds on* V5: V5 ensures pins
  on a shared net face each other; V6 ensures the components
  themselves are placed in conventional positions.
  Verifier: six fixture-wide tests in
  `crates/spice2kicad/tests/placement_quality.rs`:
  `no_symbol_symbol_overlap_across_fixtures`,
  `no_symbol_label_overlap_across_fixtures`,
  `rails_correctly_ordered_across_fixtures`,
  `wire_length_within_budget_across_fixtures`,
  `crossing_count_within_budget_across_fixtures`,
  `common_emitter_signal_flows_left_to_right`.
  Thresholds are calibrated per fixture. The channel-router floor
  on crossing counts remains a v0.2 improvement target.

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
  the structural layered placement V6 provides positions each element
  in the right band and layer, and V7 then enforces mirror symmetry
  within that layout for any subgraph whose graph automorphism group
  is non-trivial.
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
  expected to live in `crates/spice-layout/src/`, composing with
  V6's classify → bands → layers pipeline — likely as an extra
  pass that runs after band/layer seeding and before phase 4
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
  Interaction with V6 (structural placement): the V6 net-class and
  signal-flow pipeline places X-instances in the correct band and
  layer using only structural information; V8 controls whether that
  instance is rendered as a flat symbol or a hierarchical sheet. V8
  is the explicit-override floor; a future auto-promotion heuristic
  (e.g. recognising a canonical opamp port-name pattern) is the
  zero-annotation ceiling and belongs in a v0.2 pass.

- **V9 — SI-suffixed value formatting.** Every `(property "Value"
  "<text>")` emitted for a placeable element whose SPICE value
  parsed as a numeric `f64` (i.e. `Value::Number(_)` from
  `spice_parser::ast`) MUST be rendered with the SI prefix that
  yields the shortest reasonable representation, not as a raw
  decimal. Today's emitter writes `format!("{n}")` (see
  `format_value` in `crates/spice-layout/src/lib.rs`, commit
  `22cb630`), producing schematics where C1 = 100n shows up as
  `0.0000001` and a 100 µF cap as `0.00009999999999999999`. Both
  are unreadable and bear no relation to how SPICE source or KiCad
  conventionally express the same value.
    - **Suffix table.** Pick the suffix whose multiplier brings the
      mantissa into `[1, 1000)`:
      `1e-15→f`, `1e-12→p`, `1e-9→n`, `1e-6→u` (ASCII; renderers
      may substitute `µ` for display), `1e-3→m`, none, `1e3→k`,
      `1e6→Meg` (matches SPICE — `M` alone means milli),
      `1e9→G`, `1e12→T`. Values outside `[1e-15, 1e15)` fall back
      to `format!("{n:e}")`.
    - **Mantissa formatting.** Up to three significant digits;
      trim trailing zeros and a trailing `.`. `1.0e-6` → `1u`;
      `4.7e3` → `4.7k`; `1e-4` → `100u` (not `0.1m` — keep the
      mantissa ≥ 1 where a smaller suffix is available); `1.5e6`
      → `1.5Meg`.
    - **Unit suffix.** v0.1 emits the SI prefix only — no
      trailing `F` / `H` / `Ω`. SPICE source rarely writes them
      and the refdes (`R*`/`C*`/`L*`) already encodes the unit;
      adding them now is noise. Documented as a project policy,
      not a hard restriction; a future spec directive
      (`*@value-format=…`, see annotation-spec §9) may opt back
      in.
    - **Edge cases.**
      `0.0` → `"0"` (no suffix).
      Negative numerics carry the sign through the same formatter
      (`-0.015` → `"-15m"`).
      `NaN` / `±Inf` → emit the `format!("{n}")` text and raise a
      diagnostic (code TBD; reuse the overflow path from
      `tests/edge_inputs.rs::number_overflow_input`).
      Non-numeric values (`Value::String`, `Value::Expr` — model
      names like `QGENERIC`, `DC 15`, brace expressions like
      `{2*RBASE}`) pass through verbatim. The formatter only
      touches `Value::Number(_)`.
    - **Verifier.** For each `(symbol …)` instance whose refdes
      starts with `R`, `C`, or `L`, parse the `(property "Value"
      "<text>")` argument and assert it matches
      `^-?(0|[0-9]{1,3}(\.[0-9]{1,2})?)(f|p|n|u|m|k|Meg|G|T)?$`.
      The unit-letter (`F`/`H`/`Ω`) is intentionally excluded per
      project policy above — extending the regex is a v0.2
      decision tracked under spec §9. Verifier lives at
      `crates/spice2kicad/tests/visual_quality.rs` (or a sibling
      `value_formatting.rs` if that file gets crowded).
    - **Out of scope.** V9 governs only the on-schematic `Value`
      property text. The SPICE netlist exporter and the round-trip
      canonicalizer (`tests/common/mod.rs::normalize_value`) are
      separate concerns — the canonicalizer already collapses
      `4k7`, `4.7k`, and `4700` into the same equivalence class
      for topology comparison.
    - **Where to implement.** Replace the `Value::Number(n) =>
      format!("{n}")` arm in
      `crates/spice-layout/src/lib.rs::format_value`. That helper
      is the single chokepoint between parser-side `f64` and
      emitter-side string and already feeds every
      `(property "Value" …)` write in
      `crates/kicad-emitter/src/schematic.rs`.

- **V10 — Power-as-glyphs, Steiner-tree routing.** Power and
  Ground nets emit `power:VCC` / `power:GND` library symbol
  glyphs at each connected pin (no wires). Signal nets emit
  rectilinear Steiner minimum trees (exact for N≤9 pins via
  Hwang's median rule + Borah-Owens-Irwin Steinerization;
  rectilinear MST for N≥10). Cross-net endpoint conflicts
  resolved by 1-cell jog (cap 10 iterations). The router lives
  in `crates/spice-route/`, called from
  `crates/kicad-emitter/src/schematic.rs::route_nets`.
  Verifier: the fixture-wide crossing and wire-length budgets
  in `crates/spice2kicad/tests/placement_quality.rs`,
  calibrated against the five reference fixtures
  (rc_lowpass / common_emitter / multivibrator / diff_pair /
  opamp_inverting_real) at R7. Open items: PWR_FLAG-style
  driver emission for `power_pin_not_driven` ERC suppression
  (currently filtered in `tests/visual_quality.rs::run_v2`).

- **V11 — Wire/label–pin coincidence is electrical.** KiCad's
  connectivity engine treats geometric coincidence as electrical
  connection, with no `(junction …)` marker required. Concretely:
    1. A wire endpoint coincident with a pin → that pin joins the
       wire's net.
    2. A wire's *interior* passing through a pin (axis-aligned
       segment whose path contains the pin coordinate) → same: the
       pin joins the wire's net. Mid-wire pins are connected, not
       ignored.
    3. A `(label …)` / `(global_label …)` coincident with a pin →
       that pin joins the label's net.
    4. A wire endpoint coincident with another wire's interior
       (T-junction) → connected; KiCad draws an automatic junction
       dot and merges the nets.
  The corollary the router must enforce: **for every signal-net
  segment, neither its endpoints nor its interior may land on a
  pin owned by a different net, and a `(global_label …)` for a
  net may only sit on a pin of that same net.** Violating any of
  these silently shorts two nets — there is no ERC error, just a
  wrong netlist on export.
  This invariant binds **all** geometry the router emits: Stage 2
  RSMT segments, Stage 3 jogs, Stage 3b obstacle detours,
  Stage 4 cleanup output, and the `dangling_pin_labels` pass in
  `kicad-emitter/src/schematic.rs`.
  Verifier: a per-fixture test that loads the emitted
  `.kicad_sch`, builds a `(coord → net_name)` map from the
  resolved netlist, and asserts that every emitted `(wire …)`
  endpoint, every interior pin coincidence, and every
  `(global_label …)` position belongs to the same net as
  whichever pin (if any) sits at that coordinate. Lives at
  `crates/spice2kicad/tests/electrical_safety.rs` (new file).
  Implementation hooks: `find_conflicts` in
  `crates/spice-route/src/conflict.rs` flags only
  endpoint-on-endpoint coincidence between routed nets — extend
  it (and add an interior-pin-on-segment pass) so the same
  jog/L-swap machinery resolves foreign-pin coincidences. Stage 4
  cleanup must drop zero-length segments before serialisation
  (a previously observed defect produced
  `(wire (pts (xy 7.62 49.53) (xy 7.62 49.53)))` on
  `common_emitter`).
  This is a **correctness** invariant, not a quality one — a
  V11-violating schematic is electrically wrong, not just ugly.
  Recall the contrast with V5/V6/V7 (quality) and V10 (routing
  surface): V10 says *what* the router emits; V11 says *what it
  is forbidden to emit*.

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

## Memory limits when running tests / conversion jobs

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
