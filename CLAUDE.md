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
