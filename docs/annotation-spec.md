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

**Targeting `.subckt` instances (`X<n>`).** A `symbol` directive may
target a SPICE subcircuit instance — either as a trailing tag on the
`X<n>` line or via `for=X<n>` (or a glob like `for=XU*`). When it
does, the converter emits the named library symbol at X1's
placement *instead of* a hierarchical sheet referencing the
matching `.subckt` body. The `.subckt` definition is then treated
as a SPICE-side simulation model: it round-trips through any
emitted netlist but contributes no schematic geometry. This is the
mechanism for rendering an op-amp `.subckt` as a real
`Amplifier_Operational:*` triangle, a comparator `.subckt` as a
`Comparator:*` symbol, a logic-gate macro as the conventional gate
shape, and so on. Without a targeting `symbol` directive the
default behaviour is unchanged — each top-level `X<n>` becomes a
hierarchical sheet (CLAUDE.md V8).

```
*@symbol Amplifier_Operational:OPAMP for=X1 pinmap=1:3,2:2,3:1,4:8,5:4
.subckt OPAMP inp inn out vcc vee
E1 out 0 inp inn 1e5
.ends
X1 0 inv out vcc vee OPAMP
```

The `pinmap=` value uses the same syntax described in §4.2; for
`X<n>` instances the SPICE indices refer to the `.subckt` port list
in the order it was declared (`inp`=1, `inn`=2, `out`=3, `vcc`=4,
`vee`=5 in the example above). KiCad pin references on the
right-hand side may be numbers or names exactly as for any other
element. (Implementation status: trailing `;@ symbol=` on `X<n>`
already overrides sheet emission today; the `for=X<n>` block form
is being introduced as an additive extension. Until it lands, the
trailing-tag form is the only working override.)

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

For `.subckt` instances (§4.1, "Targeting `.subckt` instances"),
the SPICE indices refer to the *port positions* in the matching
`.subckt PORTNAME …` declaration rather than to terminals on a
SPICE primitive. The KiCad-side syntax is unchanged. A future
extension may accept port names on the left-hand side
(`pinmap=inp:3,inn:2,…`) for readability; v0.1 keeps the
positional `<spice_index>:<kicad_pin>` form for both primitives
and `X<n>` instances to avoid two parallel grammars.

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
3. **Topology-matched** *(v0.2+; not implemented in v0.1)* — the
   resolved netlist is matched against a built-in library of analog
   archetype templates (common-emitter / common-source amplifier,
   differential pair, current mirror, voltage divider, RC ladder,
   inverting / non-inverting op-amp, …). When a subgraph matches an
   archetype, its members are positioned per the archetype's
   template (rails horizontal, signal flow left-to-right, bias
   network on the input side, bypass caps beside their active
   device — see CLAUDE.md invariant V6). Members already pinned by
   phases 1–2 are *not* moved; the archetype templates fill in
   relative positions for everything else in the matched subgraph.
   Subgraphs that match no archetype fall through unchanged.
   This phase also performs **symmetry detection** as a sub-step:
   when a non-trivial refdes pairing makes the resolved netlist
   graph-isomorphic to itself (modulo node renames), members of
   each mirrored pair are co-positioned about a common axis with
   mirrored orientation. The classic case is the symmetric astable
   multivibrator (CLAUDE.md invariant V7). Symmetry detection
   composes with archetype matching — many archetypes (diff pair,
   current mirror, multivibrator) imply symmetry, and detected
   symmetry is exploited for any remaining roles the archetype
   template did not pin.
4. **Placed** — every `place` directive on an element not already
   constrained by `align` or by a matched archetype. Within this
   phase, source order wins on conflict (`W101`).
5. **Auto-fill** — anything still unconstrained is laid out by the
   default heuristic (force-directed within the parent cluster).

Orientation (rotation / mirror) is **not** part of the user-facing
annotation language in v0.1. The placer chooses orientation
automatically as a sub-step of phases 3 and 4, after positions are
fixed: for each pair of adjacent elements that share a net, it picks
the orientation pair that minimises the Manhattan distance between
the two pin positions on that net (CLAUDE.md invariant V5). The
search space is the eight rotation-and-mirror states of each symbol;
ties are broken by source order. A future `;@ orientation=…`
directive (§9 deferred) would override the auto-choice when the
heuristic picks a poor orientation.

A constraint that references an unknown refdes is a **hard error**
(E001), not a warning, because silent typos defeat the purpose of the
spec.

A `place` directive on an element already fixed by `align` is dropped
with a `W104` warning.

### 5.1 Wire emission and label policy

After phase 4 (auto-fill), a routing pass emits `(wire …)` segments
connecting pins on the same net. Wires are the default carrier of
connectivity; labels are not a substitute. The emitter never emits a
label "instead of" routing a wire it could otherwise have drawn.

Per-sheet label budget:

- **At most two labels of the same net name per sheet.** Used only
  when geometry truly cannot be reached by orthogonal wires
  (crossing-heavy nets, very long jumps). The two labels mark each
  terminal of the "label jump" — typical KiCad practice for
  un-routable connections.
- More than two coincident labels for one net is a defect, not a
  style preference. (Project invariant V4 in `CLAUDE.md`.)

```
* Allowed: one label at each end of a long un-routable jump.
*   net SDA — pin on U1 (top-left), pin on U7 (bottom-right):
*   two `(label "SDA" …)` placements, one at each pin.

* Disallowed: three or more `(label "SDA" …)` on the same sheet.
*   Indicates the router gave up; route a wire or split the sheet.
```

Power rails declared via `*@power` (§4.5) render as KiCad
power-flag symbols, not labels — one flag per element terminal that
connects to the rail. They do not count against the ≤ 2 label
budget.

Hierarchical-sheet pins / labels (the cross-sheet boundary at
`.subckt` instances, §3) are exempt from the budget — the boundary
itself is what makes them necessary.

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
- **Wire routing hints** (`*@route via=…`). Note: the default
  routing policy is fixed by invariant V4 (`CLAUDE.md`) — wires for
  connectivity, ≤ 2 labels per net per sheet. A `*@route` directive
  would only be needed to override that default; defer until a real
  file demonstrates the need.
- **ERC warning policy under V2.** Invariant V2 makes ERC *errors*
  blocking and tolerates *warnings* for now. Which warnings should
  remain tolerated vs. promoted to blocking (e.g. unconnected pin,
  pin-type conflict) is unresolved. Decide once the emitter is
  producing real schematics and we can survey what actually fires.
- **Bus / vector notation alignment** — deferred until the parser
  learns bus syntax.
- **User-controlled orientation override** (`;@ orientation=…` —
  e.g. `0`, `90`, `180`, `270`, optionally with a `mirror=` flag).
  The placer auto-chooses orientation per CLAUDE.md V5; an override
  is only useful when the auto-choice picks badly. Defer until the
  auto-orientation is good enough that overrides are the
  exception. Adding it earlier would invite users to over-specify
  geometry — exactly the failure mode core principle 3 ("no
  geometry numbers in user input") guards against. Promote when a
  real file demonstrates a layout the auto-orienter cannot reach.
- **`align` under mixed orientation** — the spec does not
  currently say which pin's coordinate is shared when aligned
  parts are rotated differently. Likely resolutions: (a) require
  uniform orientation within an `align` block, warn otherwise;
  (b) define a canonical pin per element kind. Defer until a real
  file demonstrates the need.
- **Archetype library** — concrete set of built-in topology
  templates that the phase-3 *Topology-matched* pass (§5)
  recognises and positions. Candidate archetypes: common-emitter
  amp, common-source amp, common-base / common-gate, differential
  pair, current mirror, cascode, voltage divider, RC low/high-pass
  ladder, op-amp inverting and non-inverting, Schmitt trigger.
  Each template specifies the element *roles* (active device,
  bias network, supply rail, signal-coupling cap, load) and the
  relative positions those roles occupy in the conventional
  drawing. Defer the concrete template set until V6 implementation
  begins; current fixtures cover common-emitter
  (`tests/fixtures/common_emitter.cir`) and the differential-pair
  fixture is the next planned addition. A future
  `;@ ignore-archetype` (or similarly named) trailing tag would
  let the user opt a sub-network out of matching when the
  template picks a layout the user dislikes — defer until matching
  is real and a real file demonstrates the need.
- **Symmetry hints** — `;@ symmetric-with=…` (or similarly named)
  trailing tag to override or guide the auto-detector when it picks
  the wrong pairing or misses a non-obvious one. Defer until
  auto-detection proves insufficient on real circuits. The
  motivating fixture (`tests/fixtures/multivibrator.cir`) is
  detectable from topology alone.
- **Symmetry detection algorithm** — graph-isomorphism over the
  resolved netlist, with refdes-class equivalence (same SPICE
  prefix and value tier) as the equivalence relation on candidate
  pairings. Concrete algorithm and tie-breaking rules are deferred;
  the current bar for auto-detection is the multivibrator fixture
  (CLAUDE.md invariant V7).
- **Auto-detect well-known subckt patterns** (op-amp, comparator,
  logic gate, voltage reference, BJT pair, …) and suggest the
  standard KiCad symbol without an explicit `*@symbol` directive on
  the instance. The matcher would inspect the `.subckt` body
  (single VCVS, two-pole opamp model, canonical port names like
  `inp inn vcc vee out` or `+ - V+ V- OUT`, …) and offer the
  conventional `Amplifier_Operational:*` / `Comparator:*` / … as a
  default that an explicit `;@ symbol=` can still override. Defer
  until the V6 archetype matcher exists (CLAUDE.md V6, V8) — the
  same pattern-matching machinery serves both layout templating and
  symbol promotion. Until then the user opts in per instance via
  the §4.1 "Targeting `.subckt` instances" mechanism.
- **Pinmap port-name syntax for `X<n>`** — accept
  `pinmap=<port_name>:<kicad_pin>` on subckt instances so that
  `pinmap=inp:3,inn:2,vcc:8,vee:4,out:1` reads as the schematic
  intent rather than as port-position indices. Defer until the
  positional form proves error-prone in real files.
- **Round-trip from KiCad back to annotations** (so manual sheet
  edits survive a re-conversion) — needs a stable element-to-symbol
  identity scheme first.
