# Schematic Annotation Spec

Status: **draft v0.1**
Scope: in-source hints, embedded in SPICE comments, that guide
`spice2kicad` when it lays out and symbol-maps a `.kicad_sch`.

---

## 1. Goals

A SPICE netlist describes connectivity but says nothing about:

1. **Symbol choice** — which KiCad library symbol best represents a
   given SPICE primitive (e.g. `Q1` could be `Device:Q_NPN_BCE` or
   `Transistor_BJT:2N3904`).
2. **Layout** — where things should sit on the sheet so the result is
   readable instead of an auto-router blob.

Structure (clustering of related elements) is **not** an annotation
concern. The SPICE language already has the right constructs for it
— see §3.

This spec defines a small annotation language that lives inside SPICE
comments. SPICE simulators ignore it; `spice2kicad` consumes it.

Design constraints:

- **Round-trip safe.** A file that simulates today must still simulate
  after annotation.
- **Optional everywhere.** A file with zero annotations must still
  produce a valid (if ugly) schematic.
- **No geometry numbers.** The file describes structure and
  relationships; the converter owns coordinates and spacing.
- **Local first.** Most directives describe the line they sit on or
  the file they live in — no forward references needed for the common
  case.
- **Line-oriented and grep-friendly.** No nested s-exprs, no YAML.

---

## 2. Lexical form

All annotations live inside SPICE comments. Two carriers are recognized:

| Form              | Where it appears                              | Example                                |
| ----------------- | --------------------------------------------- | -------------------------------------- |
| **Block comment** | A line whose first non-space character is `*` | `*@symbol Device:R_US for=R*`          |
| **Trailing tag**  | After a `;` on any element line               | `R1 in out 1k  ;@ symbol=Device:R_US`  |

The annotation marker is the two-character sequence `@` immediately
following the comment introducer (`*@` or `;@`). Whitespace between
the marker and the directive name is optional (`;@symbol=…` and
`;@ symbol=…` are equivalent). Anything else in a comment is
free-form prose and is ignored by the converter.

A single annotation line carries **one directive**:

```
<marker> <directive> [arg]... [key=value]...
```

Directive names and bare keys are case-insensitive ASCII. Values
contain no whitespace (no quoting needed).

### 2.1 Reference identifiers

Wherever a directive accepts a component reference it accepts:

- a SPICE refdes verbatim — `R1`, `Q3`, `XU2`
- a dotted path into a subcircuit instance — `XU2.R5`

### 2.2 Trailing tag and SPICE line continuation

A trailing `;@…` tag binds to the **logical element**, not the
physical line. When a SPICE element is split across lines with `+`
continuation, the tag may sit on any of those physical lines; its
effect is the same:

```
M1 d g s b NMOS L=1u  ;@ symbol=Device:Q_NMOS
+ W=10u
```

is equivalent to placing the tag on the `+ W=10u` line.

### 2.3 Multiple directives on one element

A single annotation line carries one directive. To attach several
directives to one element, use one trailing tag per directive on
adjacent lines (or split across the SPICE element's `+` continuation
lines, per §2.2). Comma-separating directives inside a single tag is
not supported.

```
R1 in out 1k   ;@ symbol=Device:R_US
               ;@ place=right-of V1
```

---

## 3. Structure (no syntax — uses SPICE itself)

Clustering on the sheet is driven by existing SPICE constructs. There
is no `*@group` directive.

| SPICE construct                          | Schematic meaning                                                                                |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `.subckt foo … .ends` + `Xfoo …`         | Hierarchical sheet `foo`, one sheet-symbol per `Xfoo` instance on the parent. Internal nodes scoped. |
| `.include "bias.cir"` *(when the file contributes ≥1 placeable element at top level)* | Visually clustered region on the parent sheet, named after the file (`bias`). Purely visual — wires may freely cross the boundary. Internal nodes share the parent scope. |

The two are deliberately different:

- **`.subckt` has a port list**, so the sheet-symbol has exactly those
  pins and internal nodes are hidden. Use it when the block is a
  reusable abstraction. A `.subckt` that is defined but never
  instantiated produces no schematic output.
- **`.include` has no port list**, so its visual box is permeable.
  Use it when you only want "draw these together" without refactoring
  shared nodes into ports.

An `.include` whose contents are entirely non-placeable (model
libraries, parameter packs, subckt definitions without instances) is
pulled in silently and produces no cluster. This makes the common
`.include "models/2N3904.lib"` case do the right thing.

A file that needs two clusters in one logical unit should be split
into two `.include`-d files. This is a deliberate forcing function:
files small enough to make splitting feel heavy are also small enough
that auto-layout handles them well.

### 3.1 SPICE statement classification

The converter classifies SPICE statements into three buckets:

| Bucket           | Statements                                                                                          | Treatment                                                              |
| ---------------- | --------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| **Placeable**    | `R`, `C`, `L`, `V`, `I`, `D`, `Q`, `M`, `J`, `K`, `E`, `F`, `G`, `H`, `X`, `T`                      | Rendered as a symbol; accepts every directive in §4.                   |
| **Structural**   | `.subckt` / `.ends`, `.include`, `.global`                                                          | Shapes the sheet hierarchy (§3); does not accept element-level directives. |
| **Simulation-only** | `.model`, `.param`, `.lib`, `.option`, `.tran`, `.ac`, `.dc`, `.op`, `.print`, `.probe`, `.plot`, `.ic`, `.nodeset`, `.save`, `.measure`, `.func`, `.temp`, `.end`, `.control`/`.endc` block | Passed through to any emitted netlist; does not appear on the schematic. Annotations attached to these lines emit `W103`. |

Anything not in this table is treated as Simulation-only and
preserved verbatim in netlist output.

The net `0` (and any net declared with `.global`) is automatically
rendered as a ground symbol — no annotation needed.

---

## 4. Directives

The directive set is intentionally small.

### 4.1 `symbol` — KiCad library mapping

Trailing form (most common — one element):

```
Q1 c b e 2N3904  ;@ symbol=Transistor_BJT:2N3904
```

Block form with `for=` (defaults across many elements):

```
*@symbol Device:R_US     for=R*
*@symbol Device:C        for=C*
*@symbol Device:L        for=L*
```

- `Lib:Name` is the canonical KiCad symbol identifier. The converter
  validates that the symbol's pin count is compatible with the SPICE
  element's terminals; on mismatch it errors unless `pinmap=` is also
  supplied.
- The value is required; an empty value (e.g. `;@ symbol=`) is
  malformed and should produce a diagnostic. Implementations that
  silently accept an empty value violate the spec. (Current parser
  produces `Tag::Symbol("")` without a diagnostic — known gap;
  see `tests/edge_inputs.rs::semicolon_at_equals_only`.)

**Glob syntax.** Shell-style: `*` matches any run of characters
(including empty). No other metacharacters. Matching is
case-insensitive.

**Resolution order** (highest wins):

1. trailing tag on the element line
2. last matching `for=` directive in source order (so put generic
   defaults first, exceptions later)
3. built-in default table

```
*@symbol Device:R_US  for=R*       # default for all resistors
*@symbol Device:R_PHOTO for=R10    # exception, comes later → wins for R10
```

### 4.2 `pinmap` — terminal remapping

```
;@ pinmap=<spice_index>:<kicad_pin>[, …]
```

Used together with `symbol=` when the chosen library symbol's pin
order does not match the SPICE element's. SPICE terminals are
referenced by 1-based positional index (terminal 1 = first node after
the refdes). KiCad pins may be referenced by number (`1`, `2`, …) or
by name (`A`, `K`, `+`, `-`, `D`, `G`, `S`, `B`, …) — the converter
looks up the symbol's pin table.

```
* SPICE MOSFET nodes are d,g,s,b but this symbol uses g,d,s,b ordering:
M1 d g s b NMOS L=…  ;@ symbol=Foo:Q_NMOS_GDS pinmap=1:2,2:1,3:3,4:4

* Diode by pin name (KiCad uses A/K, not 1/2):
D1 a k DMOD          ;@ symbol=Device:D pinmap=1:A,2:K
```

### 4.3 `place` — relative position

```
;@ place=<relation> <anchor>
```

`<relation>` is one of:

| keyword       | meaning                                         |
| ------------- | ----------------------------------------------- |
| `right-of`    | anchor's right edge → element's left edge       |
| `left-of`     | mirror of `right-of`                            |
| `above`       | anchor's top edge → element's bottom edge       |
| `below`       | mirror of `above`                               |

- `<anchor>` is a reference identifier (§2.1).
- Spacing is chosen by the layout engine; the spec does not expose
  numeric gaps.
- The geometric effect is on the **connecting pins**, not the
  symbol centers: `right-of` makes the element's leftmost
  connecting pin colinear (in y) with — and to the right of —
  the anchor's rightmost connecting pin. The converter decides
  which pin counts as "left/right/top/bottom" by inspecting the
  resolved KiCad symbol after `pinmap` is applied.

Examples:

```
R1 in  out 1k    ;@ place=right-of V1
C1 out 0   100n  ;@ place=below R1
```

### 4.4 `align` — multi-element alignment

Block-form only:

```
*@align horizontal R1 R2 R3
*@align vertical    C1 C2
```

- `horizontal` forces equal Y coordinate; X-order follows declaration
  order.
- `vertical` forces equal X coordinate; Y-order follows declaration
  order.
- All references in one `align` directive must resolve within the
  same parent sheet (i.e. you cannot align across an `.include`
  boundary or across a `.subckt` instance).
- "Equal Y" / "equal X" applies to the **connecting pins**, not
  to the symbol centers. For uniformly-oriented parts the
  distinction is invisible; for mixed orientations the behaviour
  is currently under-specified — see §9.

### 4.5 `power` — voltage source as power symbol

```
;@ power=<rail>
```

Marks a SPICE voltage source as the source of a power rail. The
source itself is not drawn; instead, every reference to the named
net renders as a KiCad power flag.

```
Vcc vcc 0 12   ;@ power=vcc
```

### 4.6 `ignore` — hide from schematic

```
;@ ignore
```

The element is parsed (so the netlist still type-checks) but emitted
as a no-connect comment in the `.kicad_sch`. Useful for zero-volt
current-measurement sources (`Vsense`), `.ic` helper sources, and
similar simulation-only elements.

---

## 5. Constraint resolution

Layout proceeds in fixed phases. Constraints from later phases never
override constraints from earlier phases.

1. **Structural** — `.subckt` boundaries (hierarchical sheets),
   `.include` boundaries (visual clusters).
2. **Aligned** — every `align` directive fixes both the shared axis
   and the order along the free axis (declaration order).
3. **Placed** — every `place` directive on an element not already
   constrained by `align`. Within this phase, source order wins on
   conflict (`W101`).
4. **Auto-fill** — anything still unconstrained is laid out by the
   default heuristic (force-directed within the parent cluster).

A constraint that references an unknown refdes is a **hard error**
(E001), not a warning, because silent typos defeat the purpose of the
spec.

A `place` directive on an element already fixed by `align` is dropped
with a `W104` warning.

---

## 6. Worked example

```spice
* Common-emitter amplifier
*@symbol Device:R_US  for=R*
*@symbol Device:C     for=C*

.include "bias.cir"     * R1, R2 — base bias divider

Vin  in  0   AC 1                          ;@ symbol=Simulation_SPICE:VSOURCE
C1   in  b   1u                            ;@ place=right-of Vin
Q1   c   b   e   2N3904                    ;@ symbol=Transistor_BJT:2N3904
                                           ;@ place=right-of C1
Rc   vcc c   4.7k                          ;@ place=above Q1
Re   e   0   1k                            ;@ place=below Q1
C2   c   out 1u                            ;@ place=right-of Q1
*@align vertical Rc Q1 Re

Vcc  vcc 0   12                            ;@ power=vcc
.end
```

`bias.cir`:

```spice
* Base-bias divider for Q1
R1   vcc b   100k
R2   b   0   22k
*@align vertical R1 R2
```

The two resistors of the bias divider are visually clustered (because
they live in their own included file) and labeled `bias` on the
parent sheet. Wires from `vcc` and node `b` cross the cluster
boundary freely — `.include` is purely visual. The net `0` renders
as a ground symbol automatically; `vcc` renders as a power flag
because of the `power=vcc` directive on `Vcc`.

---

## 7. Diagnostics

Codes prefixed `E` are errors that block conversion; codes prefixed
`W` are warnings that allow conversion to proceed with the
remediation noted. The numeric ranges (`E0xx`, `W1xx`) are reserved
for future expansion within each class.

The converter reports, in this order:

- **E001** unknown refdes in directive
- **E002** symbol pin count mismatch (with or without `pinmap`)
- **E003** unknown library symbol — the `lib_id` (from a `symbol`
  directive or the built-in default table) is not present in any
  loaded `.kicad_sym` library, *or* an element has no symbol
  mapping (e.g. `X…` subckt instance with no `;@ symbol=` tag)
- **E004** `align` references cross a sheet boundary
- **E005** invalid `pinmap` — references an unknown pin (by number
  or name), uses an out-of-range SPICE terminal index, or repeats
  a SPICE index or KiCad pin
- **E006** directional cycle in `place` graph within a single axis
  (e.g. `A right-of B`, `B right-of A`)
- **E007** internal: layout could not resolve a `place` directive
  after the policy pass (worklist stalled). Should not normally
  fire on inputs that pass the policy pass; treat as a bug.
- **W101** conflicting `place` constraints (which one was kept)
- **W102** `align` cluster has fewer than two members
- **W103** annotation on a line the parser did not recognize as an
  element (typo guard)
- **W104** `place` directive on an element already fixed by `align`
  (directive dropped)

---

## 8. Simulator compatibility

Annotations are designed to be invisible to ngspice, LTspice, and
PSpice:

- `*@…` — the leading `*` makes the entire line a SPICE comment.
- `;@…` — ngspice/LTspice/PSpice treat `;` as the start of an inline
  comment; everything from `;` to end of line is discarded before the
  element is parsed.

Two caveats:

1. Inline `;` comments are an **ngspice/LTspice/PSpice extension**,
   not part of original Berkeley SPICE3. For maximum portability use
   only the `*@` block form.
2. Inside an ngspice `.control … .endc` block, `*` is multiplication
   rather than a comment introducer. Annotations MUST NOT appear
   inside `.control` blocks.

---

## 9. Open questions / deferred

- **Spec versioning** (`*@spec version=…`). Add when v0.2 introduces
  a breaking change.
- **Net cosmetics** (`*@net style=… label=…`). Defer until users ask
  for control beyond auto-grounding `0`/`.global` nets and `power=`
  rails.
- **Absolute / corner anchoring** (`*@anchor`). Defer until chained
  `place` directives prove insufficient in real files.
- **Per-instance overrides** for `.subckt` instances (`XU1` differs
  from `XU2`) — likely a `for=` extension scoped by instance path.
- **Multi-unit symbols** (opamps with separate power-pin units, dual
  gates packaged as one part). Needs a `unit=` story.
- **Wire routing hints** (`*@route via=…`).
- **Bus / vector notation alignment** — deferred until the parser
  learns bus syntax.
- **`align` under mixed orientation** — the spec does not
  currently say which pin's coordinate is shared when aligned
  parts are rotated differently. Likely resolutions: (a) require
  uniform orientation within an `align` block, warn otherwise;
  (b) define a canonical pin per element kind. Defer until a real
  file demonstrates the need.
- **Round-trip from KiCad back to annotations** (so manual sheet
  edits survive a re-conversion) — needs a stable element-to-symbol
  identity scheme first.
