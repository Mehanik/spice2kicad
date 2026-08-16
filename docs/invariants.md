# Visual quality invariants (V1–V16, plus flow metrics)

Project-level acceptance criteria for any emitted `.kicad_sch`. These
are not part of the user-facing annotation language (`docs/annotation-spec.md`);
they are falsifiable properties a checker can measure on the output.
Every invariant has an implemented verifier in
`crates/spice2kicad/tests/` — the suite enforces them.

This file holds the **definitions and verifiers**. The *policy* that
governs how these invariants trade off against one another lives in
`CLAUDE.md`, under "Visual quality invariants":

- the **tier ordering** (Tier 0 correctness / Tier 1 readability /
  Tier 2 aesthetic, strictly ordered, with the global-improvement
  escape);
- the **ratchet-budget policy** (every per-fixture budget is a
  high-water mark that only ratchets *down*);
- the **constraints-vs-costs** distinction (hard candidate-space
  filter vs. soft SA cost term).

Read those before changing any budget literal or relaxing any
invariant here.

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
  The previously-suppressed `power_pin_not_driven` / `pin_not_driven`
  error classes are now genuinely cleared by `power:PWR_FLAG`
  emission (V10): `run_v2` (`tests/visual_quality.rs`) carries **no**
  suppression for them and asserts a fully empty error set on the
  four flat fixtures. The sole remaining allowance is one
  `power_pin_not_driven` on `opamp_inverting`'s parent ground glyph,
  which sits on a *hierarchical sheet pin* — KiCad's per-connection
  driver check (eeschema/erc/erc.cpp ~L1024-1075) will not credit a
  parent-side `PWR_FLAG` to a `power_in` glyph whose connection is
  defined through a sheet pin into the child where the real ground
  topology lives. Verified unfixable by trying the flag on the glyph
  anchor, offset+wired, on the child `0` net, and on the child
  hierarchical label; it is a genuine KiCad hierarchical artifact
  (it predates this work), allowed for `opamp_inverting` and that
  one class only.

- **V3 — `lib_symbols` are inlined verbatim.** Library entries
  emitted under `(lib_symbols)` are byte-for-byte copies of the
  corresponding `.kicad_sym` body, modulo `lib_id` name
  normalization. Rationale: portability — a consumer opening the
  emitted file must not need the same libraries installed at the
  same path. Implementation is the `Symbol::body` raw passthrough
  described in CLAUDE.md "Implementation notes". This decision is
  final for v0.1; revisiting is a v0.2 concern (tracked in CLAUDE.md
  "Project status"). Verified by a round-trip test that re-parses
  the source `.kicad_sym`, locates each used symbol in the emitted
  file's `(lib_symbols)`, and asserts byte equality of the body
  sub-tree.

  **No synthesis exception (today).** V3 is *unconditional* byte-for-byte
  verbatim for every `lib_symbols` entry — there is no synthesis or
  coordinate-transform carve-out in the emitter. ADR-13 explored a narrow
  exception (rotating emitter-owned power glyphs to fix the V14 [3]
  body-overlap residual) but it was **not pursued**: the [3] residual
  turned out to be a placer pin-choice defect, not a glyph-orientation
  one, so rotation does not address it (see the ADR-13 amendment). If a
  genuine forced-sideways glyph ever appears in a fixture, re-open ADR-13;
  any such exception would be tightly scoped to emitter-generated glyphs
  and would never touch a user-provided symbol.

- **V4 — Plain labels for in-sheet annotation; global labels for
  cross-sheet or one-pin interfaces; ≤ 1 plain label per net per
  sheet — a second only for a hierarchical-port name-jump pair.**
  Pins on the same net are connected by `(wire …)` segments emitted
  by the placer / router. Labels are *optional human-readable net
  names*, not the connectivity carrier. Three label flavours mean
  different things:

  - `(label …)` — plain net name, sheet-local. Render is a small
    text tag with no border. Use to name an in-sheet net so a reader
    can identify it.
  - `(global_label …)` — net spans every sheet by name. Render is a
    chevron-bordered tag. Use *only* for nets that genuinely cross
    sheet boundaries (a v0.2 concern) **or** for one-pin "interface"
    nets where no wire exists to anchor a plain label (ERC
    `label_dangling` fires on a wireless plain label).
  - `(hierarchical_label …)` — port on a hierarchical-sheet
    boundary. Used only by the hierarchical-sheet emitter for the
    sheet's port pins.

  Hard rules:

  1. ≤ 1 plain `(label …)` per signal net per sheet when the net has
     no hierarchical-port marker. When the net *also* touches a
     hierarchical-sheet port (`extra_pins`), a *second* plain label
     is emitted at the rightmost body pin as a name-jump pair —
     KiCad's in-sheet plain-label name-matching then binds the
     body-side wire fragment to the port-side fragment even when
     the router's Steiner tree is split by an obstacle detour.
  2. `(global_label …)` is emitted only for (a) one-pin signal nets
     (where no plain label could anchor), or (b) a future
     cross-sheet topology. For v0.1's five single-sheet fixtures the
     only legitimate global labels are the schematic's external
     ports — typically `in` and `out`.
  3. Power / Ground nets emit zero labels — `power:*` glyphs (V10)
     carry the connectivity.
  4. A label's anchor must not coincide with a foreign-net pin
     coordinate (V11) or with a port marker that already names the
     net at that coord (the `extra_pins` exclusion in
     `dangling_pin_labels`).

  Verifier: `crates/spice2kicad/tests/labels.rs` runs a per-sheet
  label-kind tally. Asserts `count(plain_label[net]) ≤ 2` (the
  name-jump pair is the only case that reaches 2) and that every
  `(global_label …)` either appears in a fixture's hand-curated
  interface allow-list or originates from a one-pin fallback path.

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

  **A left→right flow preference is NOT a refinement of V5 — it can
  contradict it.** "Series signal elements are drawn horizontal" is a
  *proxy* for readability that on real fixtures disagrees with the
  router's measured V5 (and with V12/V13/V16). Two attempts to encode it
  in the placer — a seed/SA orientation tie-break, and a hard
  `allowed`-set filter in `orient.rs` — were both measured and abandoned;
  see the ADR-15 Stage-5 post-mortem in `docs/layout-adr.md` for the
  per-fixture numbers. Do not treat "make it horizontal" as a V5
  improvement.

- **V6 — Structural layered placement.** The placer must infer a
  readable layout from net structure alone — without matching named
  topologies — via a three-stage pipeline:
    1. **Net classification.** Every net is labelled Power (connected
       to a `*@power`-marked source, or whose lowercased name matches
       a canonical supply pattern `vcc`/`vdd`/`v+`/`vplus`), Ground
       (net `0`, or a canonical ground name `gnd`/`vee`/`vss`/`v-`/
       `vminus`), or Signal. Classification requires only the
       resolved netlist; no topology recognition. Note the
       name-match is applied to *every* net (any net name appearing
       in an element's nodes), not just declared globals — so a
       signal net the user happens to name `vss` is silently
       classified Ground. The `*@power` tag and net `0` win over the
       name-match (priority order in `classify_nets`, `net_class.rs`).
       This name-based false positive is a tolerated quality risk;
       the escape hatch is to not name signal nets after rails.
       **Ground vs. negative-rail (glyph-only) distinction.** The
       `Ground` class lumps *true ground* (net `0`, name `gnd`) and
       *negative supply rails* (`vee`/`v-`/`vminus`, or any net carrying
       a `*@power=-…` negative-voltage tag) into one class — this is
       correct for *layout* (both share the bottom Y-band). But it is
       *not* correct for the **glyph** (V10): a ground triangle on a
       -12 V rail is electrically misleading. So a finer
       `negative_rail_nets(placement)` distinction (in `net_class.rs`,
       keyed off `PlacedElement::power_rail` polarity — the `*@power`
       tag wins — and the canonical negative-rail names, never net `0`)
       selects `power:VEE` instead of `power:GND` for those nets. The
       band placement is unchanged; only the drawn symbol differs.
       `vss` is treated conservatively as ground (commonly 0 V digital
       ground) unless an explicit `*@power=-…` tag promotes it.
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
       approximation terms. SA runs by default: both
       `LayoutOptions::default()` and the CLI set `refine: true`
       (pass `--no-refine` to disable).
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
  `wire_detour_within_budget_across_fixtures`,
  `crossing_count_within_budget_across_fixtures`,
  `common_emitter_signal_flows_left_to_right`.
  Thresholds are calibrated per fixture. The channel-router floor
  on crossing counts remains a v0.2 improvement target.

  **No-overlap clause (Tier-1, budget 0, ratchet).** The
  `no_symbol_symbol_overlap_across_fixtures` verifier compares each
  placed symbol's *real resolved extent* — the orientation-transformed
  `body_bbox` unioned with its pin-stub reach, in world coords (via
  `placed_symbol_pose` + `Library::lookup` + `pins_in`) — and asserts
  **no two resolved extents intersect** (budget 0, drive down never up).
  It is no longer the old blind fixed 2.54 mm half-square, which could
  not see a wide part's body/pin-stub overlap (a `Device:Q_NPN_BCE`
  spans roughly -10.8…+13 mm once pins and value text are counted, far
  past a 8.89 mm neighbour stride). The placer guarantees this by
  *deriving adjacent-element spacing from geometry*: the gap between any
  two adjacent elements is `≥ left.right_extent + right.left_extent +
  CLEARANCE`, snapped up to the grid, where each extent =
  orientation-transformed `body_bbox` ∪ pin-stub reach ∪ value-text-width
  estimate. This is a **hard constraint at the spacing/candidate boundary**
  (the align-cluster stride and the seed per-layer X positions in
  `crates/spice-layout/src/lib.rs`; both floor at the historical fixed
  stride so well-behaved small-symbol clusters keep their tuned spacing
  and only oversized parts widen), plus a matching SA "never-increase"
  hard gate (`symbol_overlap_count` in `solver/anneal.rs`, whose
  overlap measure now uses the full footprint = body ∪ pin reach). It is
  **not** a soft cost (no clearance weight in `cost.rs` — that would
  recreate the documented Attempt-A failure). Unlike V6's other metrics
  (band/layer placement, signal-flow), which are Tier-2 aesthetic
  refinements, this non-overlap clause is tiered **Tier-1 readability**:
  a symbol body or pin stub spearing a neighbour is a legibility defect
  a reader flags on sight, exactly the V12/V13 precedent (a wire through
  a body, a label over a body). Tier-0/1/2 ordering still applies — the
  no-overlap clause may never be regressed to improve a Tier-2 metric.

  **Hierarchical-sheet instances are placeable units.** A default-path
  `.subckt` instance (no `*@symbol` override) lowered to a KiCad
  `(sheet …)` block is a first-class placeable unit fed through the
  same V6 pipeline as any symbol: its ports' parent nets are its
  `nodes`, its body bbox is the sheet rectangle (~30.48 mm wide), and
  its port pins are the sheet-edge pins. It is positioned **adjacent
  to the elements it shares Signal nets with**, NOT at a hardcoded page
  coordinate, so its port trunk wires are bounded like any other net.
  (Power/Ground ports become `power:*` glyphs at the sheet pin per V10,
  so they carry no trunk wire and don't pull the sheet.) The sheet does
  **not** flow through the V5/V14 orientation or SA passes — those index
  real symbol pin geometry; the sheet has identity orientation and a
  fixed rectangle, so it is placed by `spice_layout::place_sheets`
  (`crates/spice-layout/src/sheets.rs`) after the real-element placer
  runs, from the *final* neighbour positions, then de-overlapped against
  every real symbol body and every other sheet. **The de-overlap
  footprint extends the sheet rectangle leftward by the power-glyph
  reach** (`SHEET_GLYPH_REACH_MM` = 3 grid cells): the sheet's left-edge
  port pins hang `power:*` glyphs that far outward (see V13 below), so a
  sheet jammed against a neighbour would spear it with a *glyph* even
  when the bare body clears — folding the glyph zone into the obstacle
  test pushes the sheet right until both body and glyphs clear. Sheets
  therefore participate in the symbol-vs-symbol no-overlap clause, not
  just symbol-vs-symbol. Multi-sheet files get distinct non-overlapping
  rectangles (replacing the old `idx*60` page-column stacking). Like the
  rest of V6 this is a **Tier 2** quality property.
  Verifier: `hierarchical_sheet_placed_near_circuit`
  (`crates/spice2kicad/tests/placement_quality.rs`) — for every
  emitted parent `(sheet …)` block, asserts its `(at …)` lands within
  the circuit's symbol-bbox expanded by a small geometry-derived margin
  (so a sheet flung off-page fails), AND the longest emitted
  `(wire …)` segment stays under a per-fixture sheet-port trunk-wire
  budget (`SHEET_TRUNK_WIRE_BUDGET_MM`, a recorded high-water-mark
  ratchet driven down, never up). Plus
  `no_symbol_sheet_overlap_across_fixtures` (no symbol's resolved extent
  and no `power:*` glyph body overlaps a `(sheet …)` body bbox) and
  `power_glyph_not_on_sheet_port_pin` (no glyph anchor coincides with a
  sheet port pin — it would overprint the port label). Both budget 0,
  ratchet. The verifiers derive everything from the emitted geometry —
  no fixture name or magic coordinate is hardcoded. Plus
  `crates/spice-layout/src/sheets.rs::tests`: single-sheet proximity,
  multi-sheet non-overlap, grid-snap.

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
  (so the BJT arrows point toward each other). The placer pins each
  detected mirror pair `(L, R)` at `R.x = axis_sum - L.x`, `R.y =
  L.y` about a single shared `axis_sum` (the seed bbox midpoint, or a
  user-pinned pair's midpoint when one exists), so all four pairs land
  on the **same** vertical axis — verifier (a) holds within a fraction
  of a cell rather than failing by one cell per pair. The symmetry
  detector lives in `crates/spice-layout/src/symmetry.rs`, composing
  with V6's classify → bands → layers pipeline as a pass that runs
  after band/layer seeding and before V5's orientation chooser
  (`place_with_hint` in `crates/spice-layout/src/lib.rs`).

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
  top-level `X<n>` becomes a hierarchical sheet (commit `e10e7e7`
  feat(resolve): standard symbol mapping for subckt instances
  (V8)). V8 is a *refinement* of that default — the user opts in
  per X instance (or per subckt definition via `for=`).
  Motivating fixture: `tests/fixtures/opamp_inverting.cir` today
  emits `OPAMP.kicad_sch` as a child sheet with a single VCVS inside;
  `tests/fixtures/opamp_inverting_real.cir` adds
  `*@symbol Amplifier_Operational:OPAMP for=X1 pinmap=…` and expects
  a real triangle symbol on the parent instead.
  The resolver suppresses the `SheetInstance` routing decision
  for any X instance carrying a block `*@symbol … for=X1` override:
  `has_block_symbol_override` (`crates/spice-resolve/src/lib.rs`)
  guards the `SheetInstance` push, so a block-form override is
  honoured alongside the trailing `;@ symbol=…` tag path.
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
  instance is rendered as a flat symbol or a hierarchical sheet.
  Either way the instance is V6-placed near the circuit: the
  `*@symbol`-override (flat-symbol) path places it as an ordinary
  element; the default (sheet) path positions the `(sheet …)` block
  via `spice_layout::place_sheets` — see V6's "Hierarchical-sheet
  instances are placeable units" clause. V8
  is the explicit-override floor; a future auto-promotion heuristic
  (e.g. recognising a canonical opamp port-name pattern) is the
  zero-annotation ceiling and belongs in a v0.2 pass.

- **V9 — SI-suffixed value formatting.** Every `(property "Value"
  "<text>")` emitted for a placeable element whose SPICE value
  parsed as a numeric `f64` (i.e. `Value::Number(_)` from
  `spice_parser::ast`) MUST be rendered with the SI prefix that
  yields the shortest reasonable representation, not as a raw
  decimal. The emitter applies this in `format_value`
  (`crates/spice-layout/src/lib.rs::format_value`), whose
  `Value::Number(n)` arm calls `format_si`
  (`crates/spice-layout/src/lib.rs::format_si`, commit `5163669`).
  Without it C1 = 100n would show up as `0.0000001` and a 100 µF cap
  as `0.00009999999999999999` — unreadable and unrelated to how SPICE
  source or KiCad conventionally express the same value.
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
      `crates/spice-parser/tests/numbers.rs::number_overflow_input`).
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
    - **Chokepoint.** The `Value::Number(n) => format_si(*n)` arm
      in `crates/spice-layout/src/lib.rs::format_value` is the
      single point between parser-side `f64` and emitter-side
      string, and feeds every `(property "Value" …)` write in
      `crates/kicad-emitter/src/schematic.rs`.

- **V10 — Power-as-glyphs, Steiner-tree routing.** Power and
  Ground nets emit `power:VCC` / `power:GND` library symbol
  glyphs at each connected pin (no wires). A **negative supply rail**
  (a Ground-class net flagged by `negative_rail_nets`; see V6) emits
  the distinct `power:VEE` glyph instead of `power:GND` — a ground
  triangle on a -12 V rail is electrically misleading. The VEE glyph
  is attached exactly like a GND glyph (canonical axis Down, so no
  forced-sideways stub) — only the drawn symbol differs. The
  `NetSpec::negative_rail` flag carries this through `rails::emit`;
  `power_lib_id_for_net` mirrors it so the `power:VEE` lib_symbol
  inlines verbatim (V3). Signal nets emit
  rectilinear Steiner trees: N=3 is exact via Hwang's median
  rule; 4≤N≤9 is heuristic (rectilinear MST + Borah-Owens-Irwin
  Steinerization on the Hanan grid); N≥10 is plain rectilinear
  MST. Cross-net endpoint conflicts
  resolved by 1-cell jog (cap 10 iterations). The router lives
  in `crates/spice-route/`, called from
  `crates/kicad-emitter/src/schematic.rs::route_nets`.
  Verifier: the fixture-wide crossing and wire-length budgets
  in `crates/spice2kicad/tests/placement_quality.rs`,
  calibrated against the five reference fixtures
  (rc_lowpass / common_emitter / multivibrator / diff_pair /
  opamp_inverting_real) at R7. **PWR_FLAG driver emission is now
  live** (`crates/spice-route/src/pwrflag.rs`, called from
  `route()` after Stage 1): exactly one `power:PWR_FLAG` is placed,
  wire-coincident, on a net iff (a) it has ≥1 pin, (b) ERC *requires*
  it driven — a Power/Ground net always, a Signal net only if some pin
  on it is `input`/`power_in` — and (c) it lacks a valid driver *for
  its class*. The driver rule is class-aware, mirroring KiCad's
  `DrivingPinTypes`: **any** class is driven by a true driving pin
  (`PinElectrical::drives` — Output/PowerOut/bidirectional/…), and a
  **Signal** net is *additionally* driven by any **Passive** pin
  (KiCad's `DrivingPinTypes` ∋ `PT_PASSIVE`, so a resistor/cap
  terminal is a valid signal-net driver). A **power net** — a
  name-based Power/Ground rail **or** any net carrying a component
  `power_in` pin (KiCad's `ispowerNet`, `erc.cpp:1033`, tracked via
  `NetSpec::has_power_in`, even under a signal-flavoured name) —
  ignores passives and still demands a real `power_out`. The predicate
  is derived from KiCad pin electrical types
  (`kicad_symbols::PinElectrical::{drives, requires_driver}`) plus the
  net-level `has_passive`/`has_power_in` flags, never from
  fixture/refdes names, so it covers rails and the diff_pair
  input-base nets with one rule and leaves passive-bearing signal nets
  (R–C junctions, a transistor base with a bias resistor) untouched —
  their passive terminal is itself the driver. Global Power/Ground nets are
  driven by a single root-sheet flag (child-sheet copies would
  double-drive). ERC is genuinely clean (zero `power_pin_not_driven`
  / `pin_not_driven`) on the four flat fixtures; `opamp_inverting`'s
  hierarchical-sheet-pin ground retains one documented artifact (see
  V2). The fixture `power.kicad_sym` gained a verbatim `PWR_FLAG`
  symbol so the emitter can inline it (V3).
  **Each `power:*` glyph's `#PWRn` Reference is emitted hidden**
  (`(effects … (hide yes))` in `spice-route/src/rails.rs`
  `power_symbol_sexpr`) — KiCad convention; the glyph and net-name
  Value carry all reader-visible info, so a drawn `#PWRn` is pure
  bookkeeping that only collides with neighbouring property text
  (V13(4)).
  **A `*@power` / `;@ power=` source is a power *rail*, not a drawn
  component:** the emitter suppresses its `(symbol …)` instance and
  its own pins entirely (annotation-spec §4.5). The rail's
  connectivity is carried solely by the `power:*` glyphs emitted at
  the *consuming* components' rail pins; the source itself
  contributes no symbol, no `power:*` glyph of its own, no obstacle,
  and no property text. The chokepoint is `is_power_source` on
  `PlacedElement` (set from `ElementRole::Power(_)` in
  `spice-layout::place_seed`), which gates the `(symbol …)`,
  `lib_symbols`, `collect_net_pins`, obstacle, and property-bbox
  loops in `kicad-emitter/src/schematic.rs`.
  Verifier: `tests/power_source_suppression.rs` derives the
  power-tagged source refdes *generally* from each fixture's `.cir`
  (scanning the `;@ power=` trailing tag and `*@power for=` block —
  never a hardcoded refdes/fixture list) and asserts zero drawn
  `Simulation_SPICE:V…` instances carry any of them. Ratchet floor:
  0 drawn power-source symbols, across all fixtures.

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
  **Residue is a refusal, unconditionally** (ADR-20 D4, ADR-21,
  ADR-22), and since ADR-22 the refusal is **geometric and
  mechanism-blind**. `emit_root` / `emit_child_sheet` reconstruct the
  ENTIRE net partition from the ink they are about to write —
  `kicad_emitter::connectivity::check_partition`, a union-find
  implementing exactly the four clauses above plus KiCad's by-name
  rule for `power:*` glyphs and same-named labels — and error with
  `EmitError::NetPartition` if it is not the source netlist's
  partition: two nets in one component (a short) or one net in
  several (an open). One geometric check, both directions, before a
  single byte reaches disk.
  This *replaced* two mechanism-specific checks. `V11Violation`
  recognised a merge by string-matching the router's `v11:` warning
  and `DisconnectedNet` recognised a split by union-finding wires
  alone; both are gone. Naming mechanisms was the defect — every
  other way to merge two nets needed its own string and its own
  escalation, which is why `conflict:` reached exit 0 for as long as
  nobody wrote one, and why `cross-net overlap:` could not be
  escalated at all. Naming the *consequence* catches all of them,
  including ways nobody has thought of yet.
  `EmitError::PinCoincidence` survives alongside it, not as a second
  authority but as a pre-route pre-flight: it is raised before the
  router runs, so it names the two coincident pins rather than the
  component they end up in.
  None of this depends on `--verify`, on `kicad-cli` being installed,
  or on any env var; there is no opt-out from a Tier-0 refusal.
  `--no-verify` skips only the *external* `kicad-cli` opinion, which
  remains valuable for the one thing the in-process check cannot do —
  falsify the model itself. Note the asymmetry with V12, which is
  Tier 1 and legitimately warns with a budgeted fallback.
  Second verifier (whole-file, independent):
  `crates/spice2kicad/tests/roundtrip_connectivity.rs` runs the same
  engine over terminals and geometry it derives *independently* — the
  `.cir` re-parsed and re-resolved, pin coordinates re-derived from
  the library through the emitted pose, and the geometry read back off
  the written file — which is what lets it falsify `collect_net_pins`
  and the emit tail, neither of which the production check can grade
  itself on. It carries a vacuity guard and a mutation guard.

- **V12 — Wires do not cross foreign symbol bodies.** Every emitted
  `(wire …)` segment's axis-parallel path must not strictly enter
  the body bbox of any symbol that doesn't host the wire's net.
  "Strictly" means the path penetrates the bbox interior — touching
  the edge at a pin coordinate is fine, that's the whole point of a
  pin. Today's `avoid_obstacles` pass in
  `crates/spice-route/src/conflict.rs` tries alternate-L corners and
  1..4-cell offset detours; on failure it logged an `obstacle: …`
  warning and left the segment in place (V10 called this "ugly but
  electrically valid"). V12 promotes the warning to a quality
  defect with a per-fixture crossing budget.
  Verifier: `crates/spice2kicad/tests/electrical_safety.rs::v12_*`.
  Calibration: `crates/spice2kicad/tests/electrical_safety.rs::v12_crossing_budget`
  returns `0` for every fixture, so the budget is `0` across all
  **ten** — no wire may cross a foreign body. `opamp_definition_level`
  briefly carried a non-zero "OWED, NOT ACCEPTED" budget of 4, whose
  stated precondition for retirement (fix the seed defect that put
  `RF1`/`RF2` inside a foreign opamp body) has been met; it is back to 0. The
  budget is the **high-water mark we drive down**, not a license to
  introduce new crossings — a regression trips the test.

- **V13 — Labels do not overlap symbol bodies, property text, or
  foreign-net wires.** For every emitted `(label …)` /
  `(global_label …)`:
  1. The label's text bbox does not overlap any symbol body bbox.
  2. The label's text bbox does not overlap any
     `(property "Reference" …)` or `(property "Value" …)` text bbox
     emitted on the same sheet.
  3. The label's anchor position does not lie on the interior of a
     `(wire …)` segment that belongs to a different net (V11 covers
     the foreign-pin subcase; V13 extends to wire-interior
     coincidence away from any pin).
  4. No two VISIBLE on-sheet text bboxes overlap — every host
     `(property "Reference" …)` / `(property "Value" …)` vs each
     other AND vs every `power:*` glyph's net-name `(property
     "Value" …)`, using the same `text_bbox` model. This closes the
     property-text↔property-text / property-text↔power-glyph gap
     (ISSUE-5) that parts (1)–(3), being label-anchored, did not
     cover. Two mechanisms enforce it in the DECORATION phase: the
     `#PWRn` Reference is emitted hidden (see V10/V14 note), and a
     `nudge_property_text` pass (`kicad-emitter/src/schematic.rs`,
     after routing/labels, before page translation) moves a
     colliding host Reference/Value to the first alternative anchor
     offset that clears all visible text, the symbol body, labels,
     and wire interiors — driven purely off the measured `text_bbox`
     model (no fixture constants), and moving TEXT only, never a
     symbol pose.
  5. No host `(property "Reference" …)` / `(property "Value" …)`
     overlaps a symbol's VISIBLE internal pin-name / pin-number text
     (its own symbol's or a neighbour's) — e.g. the transistor
     `QGENERIC` Value must not sit on the `B`/`C`/`E` pin names or
     the `1`/`2`/`3` pin numbers (R-4). Pin-text bboxes are computed
     from the lib-symbol definition: `Symbol::pin_text_page_bboxes`
     returns one **page-frame** box per *visible* label (skipping
     `(pin_names (hide yes))`, `(pin_numbers (hide yes))`, and
     `~`/empty names KiCad draws as nothing). It takes the placed
     pose rather than returning symbol-local boxes because KiCad's
     placement rule for outside pin text — *beside* the shaft, left
     of a vertical pin / above a horizontal one, name and number on
     opposite sides when both are drawn — is stated in drawn
     coordinates and is not rotation-covariant: a symbol-local box
     on the local −x side lands on the world +x side under a 180°
     pose. The predecessor centred the box on the shaft and so
     under-reserved the drawn side by ~0.8 mm; see ADR-26. The same
     `nudge_property_text`
     pass enforces it by adding those pin-text bboxes as one more
     obstacle class alongside bodies, labels, wires, and other
     visible text; when no candidate anchor clears every obstacle
     (a dense symbol) it keeps the least-overlap position rather
     than the colliding default. General by construction — no
     fixture/refdes constants.
  Verifiers in `crates/spice2kicad/tests/electrical_safety.rs`
  enforce all five: (1) body overlap with a per-fixture
  allow-list; (2) `v13_labels_dont_overlap_property_text`; (3)
  `v13_label_anchor_not_on_foreign_wire_interior`; (4)
  `v13_property_text_no_mutual_overlap`; and (5)
  `v13_property_text_no_pin_text_overlap` (per-fixture ratchet
  literals, all `0` today). V13 stays Tier 1.

  **Power glyphs on hierarchical-sheet port pins.** KiCad draws a
  `(sheet …)` block's port label at the port-pin coordinate. A
  `power:*` glyph anchored there overprints that label and overlaps
  the sheet body — so a glyph (and the PWR_FLAG driving it) on a
  sheet-edge pin uses the **detached-glyph-with-stub-wire** path: it
  is offset `SHEET_EDGE_GLYPH_OFFSET_CELLS` (= 2) grid cells *outward*
  from the sheet (away from the body, along the port pin's outward
  direction — Left for a left-edge port column) and bridged to the
  pin by a one-segment stub wire (same net, V11-safe). This is the
  same mechanism as the V14 forced-sideways fallback, keyed instead on
  `PinRef::on_sheet_edge` (set by the emitter for the sheet-port
  `extra_pins`); both the glyph and its PWR_FLAG share
  `rails::sheet_edge_offset`. The placer-side companion is the V6
  glyph-reach de-overlap footprint above — together they keep the
  glyph clear of *both* the sheet body and any neighbouring symbol.
  Verifiers: `power_glyph_not_on_sheet_port_pin` and
  `no_symbol_sheet_overlap_across_fixtures`
  (`crates/spice2kicad/tests/placement_quality.rs`), budget 0.

  The glyph's own *net-name text* is a separate obstacle problem, and
  the port label it must clear is drawn **inside** the sheet body,
  reading away from the edge — a left-edge pin `(at … 180)` draws its
  name rightward, into the sheet, exactly as a hierarchical label
  does (tag glyph at the anchor, string one `hier_label` lead
  further along). `sheet_port_name_bboxes` — the emitter's copy and
  the verifier's — modelled it on the *outside* until ADR-26, so the
  obstacle set was a mirror image of the real ink and
  `nudge_power_glyph_value_text` could relocate a rail name straight
  onto a port label while believing it had moved it clear.

  **Neither of these two classes — pin text, sheet-port names — is
  covered by `rendered_text.rs`'s ink calibration**, which is how both
  stayed mirror-reflected while every V13 budget read 0. Extending the
  calibration to them is owed; see ADR-26.

- **V14 — Power glyph orientation: GND down, VCC up.** Every
  `power:GND` instance emits with the rotation that draws the
  triangle below the connection point (KiCad lib convention: rot 0).
  Every `power:+...` / `power:VCC` / `power:VDD` instance emits at
  rot 0 as well (chevron drawn above the connection point).

  `power:VEE` is the exception, and emits at **rot 180**. V14 is a
  claim about the direction a glyph POINTS ON THE PAGE, which is the
  library body direction composed with the rotation — not the rotation
  alone. The KiCad library draws VEE's arrow toward local +Y, i.e.
  *upward*, exactly like the VCC chevron; but VEE is a NEGATIVE supply
  and belongs with GND. At rot 0 it therefore pointed up, into the host
  body — visible in the `diff_pair` render as VEE's arrowhead sitting
  inside `RTAIL`. Rot 180 is what actually delivers "negative rail
  points down". A negative rail is also *attached* like ground
  (canonical axis Down, see V10), so it never triggers the
  forced-sideways stub; the rotation aligns the drawn body with that
  same axis. See `rails::glyph_rotation`, which derives the angle by
  comparing each glyph's library body direction against its canonical
  attachment axis rather than hardcoding a constant. The host
  pin's outward direction does *not* alter the glyph rotation — the
  previous per-pin rotation match (commit `b4838ee`) produced GND
  glyphs at any of {0, 90, 180, 270} depending on which pin they
  attached to, which is not how schematics are conventionally drawn.
  Consequence: when the host pin's outward direction conflicts with
  the locked orientation (e.g. a GND glyph attached to a pin that
  sticks upward into the body's empty space), the glyph body may
  visually overlap the host symbol's body. The V13 verifier flags
  those cases as quality defects; closing them needs a placer-level
  pin-choice improvement (tracked separately). V14's contract is
  purely "no surprising rotations".
  Verifier: `crates/spice2kicad/tests/placement_quality.rs::v14_*`
  asserts every directional rail glyph (`power:GND` / `power:VCC` /
  variants; `power:PWR_FLAG` excepted) has `rot == 0`, and that
  `power:VEE` has `rot == 180`.
  A companion verifier
  (`electrical_safety.rs::negative_rails_render_as_vee_not_gnd`)
  asserts negative rails use `power:VEE`, true ground uses
  `power:GND`.

- **V15 — Content lands within the page's usable area.** Every
  emitted coordinate (symbol / property / wire / label / glyph /
  junction / sheet / no_connect anchor) has non-negative X/Y and
  lies inside the A4 drawable region. The placer's grid frame allows
  negative origins, so without a final pass the whole circuit spills
  off the top-left page border with ~90% of the sheet empty. The fix
  is a single final grid-snapped *uniform translation* that shifts
  the entire placed bounding box so its top-left corner sits at a
  fixed positive page margin (`PAGE_MARGIN_MM = 25.4 mm`, 20 grid
  cells). Because it is one uniform offset — no scaling, no per-
  element moves, an integer number of grid cells — every relative-
  geometry invariant (V5–V7, V10–V14) is preserved by construction
  and everything stays grid-snapped. It is applied as the single
  chokepoint `translate_into_page` in `kicad-emitter/src/schematic.rs`,
  run on the final `Sexpr` tree of every sheet (root and child)
  immediately before `to_pretty()`; operating on the emitted tree
  means it cannot miss a coordinate category generated from emitter
  constants (hierarchical labels at `-25.4`, sheet blocks, …). Two
  subtrees are excluded: the `(lib_symbols …)` block (symbol-
  definition-local geometry that must not move with the instance
  layout) and hidden `(property … (hide yes))` nodes (emitted at a
  fixed `(0 0 0)`, not visible content). This is a categorical floor,
  not a quality gradient: it needs no per-fixture ratchet budget, a
  hard `min ≥ margin` assertion suffices.

  **The invariant is `min ≥ margin`, not `min == margin`.** Normalising
  the content bbox to land exactly on the margin is merely the simplest
  way to satisfy it, not the requirement. The distinction is
  load-bearing for position stability (ADR-4): because the shift is
  recomputed from the content bbox on every run, adding one element can
  extend the bbox and re-anchor the frame, panning every existing
  element uniformly — measured as `Δ = (+5.08, −1.27) mm` applied to
  both parts of a 2-element circuit when a third was added, with grid
  coordinates bit-identical throughout. That is invisible to the placer
  and unfixable there, since any uniform pre-translation cancels in the
  normalisation. A cached page shift may therefore be *preferred* over
  re-normalising, provided the result still satisfies `min ≥ margin` and
  stays inside the usable area — that is V15-conformant, and a verifier
  demanding equality is over-specified.

  **Implementation (position-stable page frame).** `translate_into_page`
  takes an optional *preferred* shift — the one the previous run applied,
  replayed from the `.layout.json` layout cache (`sidecar::PageShiftEntry`,
  keyed per sheet) — and keeps it, per axis, whenever the result still
  satisfies `min ≥ margin` and the A4 ceiling; otherwise that axis falls
  back to bbox normalisation. The fallback is what bounds drift: a
  replayed shift is a constant carried across runs, so it cannot creep,
  and the moment content would leave the page the sheet re-anchors.
  Complementing it, the content bbox itself is made insensitive to
  decoration that flips sides: a visible `(property …)` anchor inside a
  `(symbol …)` instance votes with both its own position **and** its
  mirror about the symbol origin (`fold_symbol_instance`). Without that
  reserve, the V13 text-nudge moving an *untouched* symbol's Reference
  from its right side to its left (because a newly added neighbour
  arrived) grows the bbox 5.08 mm leftward and forces a re-anchor no
  cached shift can absorb — that was the concrete `Δ = (+5.08, −1.27) mm`
  pan above. The reserve only ever widens the bbox, i.e. only ever moves
  content further *inside* the page, so the V15 floor holds by
  construction.
  Verifier: `crates/spice2kicad/tests/placement_quality.rs::v15_*`
  collects every instance-section coordinate of every emitted sheet
  (excluding `lib_symbols`) and asserts the content bbox clears the
  margin, no coordinate is negative, and the bbox fits within the A4
  (297×210) drawable rectangle. Position stability of the frame across
  edits is verified by
  `crates/spice2kicad/tests/layout_cache.rs::page_shift_is_cached_and_does_not_drift_toward_the_page_edge`.

- **V16 — Wire rectilinearity (bends and branches).** A schematic is
  easy to read when its wires are minimal, straight, and connect
  directly the elements that are connected. V16 makes that falsifiable
  as two per-fixture counts on the emitted geometry.

  **The counted quantity is bends, NOT raw segments.** The emitted
  `(wire …)` count is a **Tier-0 correctness artifact**, not a quality
  signal: `crates/spice-route/src/cleanup.rs` deliberately re-segments
  identical ink — `split_at_interior_attachments` SPLITS runs at same-net
  attachment vertices (KiCad connects wires only at *endpoints*, so more
  segments is *more correct*), `coalesce_collinear` merges abutting
  collinear pairs, and `collapse_collinear_overlaps` replaces overlaps
  with a vertex-preserving non-overlapping cover. Measured on
  `common_emitter`: 20 raw segments whose visible ink is 16 maximal
  straight runs. A metric on raw segments would create optimization
  pressure *against* a Tier-0 pass, so the counted quantity must be
  **invariant under re-segmentation of identical ink**.

  **The ink graph.** Take the union of every emitted wire segment; group
  by line (same X for verticals, same Y for horizontals); merge
  touching-or-overlapping collinear spans into **maximal straight runs**.
  Vertices are run endpoints plus run–run incidences. Rays are counted
  exactly as `cleanup.rs::rays_at` does: a run *ending* at the point
  contributes one ray; a run whose *strict interior* contains it
  contributes two (it passes through).

  - **B — bend count.** Vertices with exactly 2 rays, one horizontal and
    one vertical: the L-corners of the ink. PRIMARY per-fixture ratchet.
  - **J — branch count.** Vertices with 3 rays (a T), plus 4-ray vertices
    that carry a `(junction …)` dot (a same-net cross). 4-ray vertices
    *without* a dot are **inter-net crossings** and belong to the
    existing crossing ratchet
    (`placement_quality.rs::crossing_count_within_budget_across_fixtures`),
    not to J.

  B and J stay **separate** ratchets. They must not be folded together: a
  k-pin Steiner tree topologically needs ≥ k−2 branch points, so a
  combined number would penalise trunk-and-taps — often the most readable
  form.

  Any **diagonal** wire segment is an outright failure, not a budgeted
  count. Axis-alignment is what makes ray-counting sound, and nothing in
  the pipeline emits diagonals today, so it is a free tripwire.

  **Deliberately not ratcheted:** raw segment count (above);
  *bends-per-net* (a gameable denominator — adding trivial nets lowers
  the average); and any *rewarded* count of "nets routed straight"
  (gameable — a V4 hierarchical-port name-jump label pair can mint a new
  'straight' component out of nothing). Absolute per-fixture totals only.

  **Anti-gaming, and the gates this depends on.** B and J are
  *cost-shaped* — they count defects over the whole artifact — not
  credit-shaped, so dead or decorative geometry can only ever ADD rays,
  never remove a bend; there is no way to score better by drawing more.
  That soundness is **conditional** on the lower gates staying hard.
  With them disabled, "delete all the wires" or "replace every wire with
  a label" would both score a perfect B = J = 0. The dependencies:
  (1) the Tier-0 kicad-cli connectivity verification the CLI runs after
  every conversion; (2)
  `electrical_safety.rs::no_dangling_whiskers_across_fixtures` (budget 0);
  (3) the V4 label policy (`labels.rs`). Do not land or trust this
  ratchet in a tree where any of the three is weakened.

  **Tier 2, and strictly subordinate.** V16 is a continuous quality
  gradient with no single correct value, so by CLAUDE.md's
  constraints-vs-costs decision rule it is Tier 2 — the same tier as
  V5/V6/V7 — and never a hard constraint. It must stay subordinate to
  Tier 0 and Tier 1: the globally bend-minimal route *through* a symbol
  body (V12) or *across* a label (V13) is worse than a 2-bend detour
  around them.

  **How that subordination is enforced — by structure, not by tuning.**
  V16 must NEVER be a *weighted* term: no bend weight in `cost.rs`, no
  bend-minimising router pass. In a weighted sum every term is tradeable
  against every other at some ratio, so "subordinate" degenerates into a
  question of coefficients — and CLAUDE.md's constraints-vs-costs rule
  already records that a soft term at a safe weight either does nothing
  or eventually outvotes something it shouldn't. Subordination by tuning
  is not subordination.

  V16 MAY, however, enter **phase 4.5's acceptance predicate**
  (`kicad-emitter/src/refine.rs`) in exactly two shapes:

  (a) a **non-regression guard**, alongside the existing
      `overlap` / `v12` guards; or
  (b) the **final key of the lexicographic objective**, strictly after
      `(v13, v12, v5)`.

  As of ADR-20 the objective carries a **Tier-0 prefix** —
  `(severed, coincident, v11, v13, v12, v5, bends)` — so "strictly
  after `(v13, v12, v5)`" now also means strictly after those three.
  Prepending keys does not weaken the argument below: it only adds
  terms that dominate `bends` even harder. `v11` moved out of the guard
  list into that prefix (it is V11, i.e. Tier 0, not a Tier-1
  preference); `overlap` and `v12` remain guards, lifted only for a
  candidate that strictly improves the Tier-0 prefix.

  Both are safe for the same structural reason, and it is a proof rather
  than a preference: under lexicographic comparison a candidate that
  raises `v12` or `v13` produces a strictly greater tuple *regardless of
  how many bends it saves*, and is independently refused by the `<=`
  guards. There is no exchange rate to get wrong, so bends can never buy
  a wire through a body or across a label. This is what distinguishes a
  lexicographic last key from a weighted term — the two coincide only
  when weights exist to trade. **Bends must not be moved earlier in the
  tuple**, which would make exactly that trade reachable.

  Conditioned on **metric fidelity**: whatever enters the gate must be
  the ink-graph quantity defined above (maximal straight runs, then
  2-ray H+V vertices), never a raw segment or route-corner count.
  `cleanup.rs::split_at_interior_attachments` is a Tier-0 correctness
  pass that deliberately *increases* segment count, so a raw count in
  the gate would create optimisation pressure against correctness.
  `refine.rs::bend_count` implements the ink-graph metric and its unit
  tests assert invariance under re-segmentation.

  **Provenance of this rule (read before "correcting" it).** The
  original text here said V16 is "verifier-shaped … never an in-loop
  objective". That formulation was found **too absolute** on design
  review — it conflates "in-loop" with "able to trade against Tier 1",
  which are the same thing in a weighted sum but *different* under
  lexicographic comparison. The review was conducted by the author of
  ADR-16 reviewing their own rule, and the amendment above was adopted
  **on explicit project-owner sign-off**, the owner choosing
  "reformulate the rule, take the tie-break" over keeping the absolute
  ban. It is a doctrine change authorised by the owner — NOT an agent
  relaxing a rule to legalise its own change. A previous run mistook it
  for the latter and reverted approved work; do not repeat that. If you
  believe this rule is wrong, raise it — do not silently revert it.

  **Accepted side effect.** Putting bends in the gate increases
  router → placement coupling: phase 4.5 already uses the real router as
  its oracle, so a `spice-route` change can now shift placement through
  the bend key as well as the V5 key. That is governed by ADR-16's
  baseline-diff protocol (a router-only change must produce an empty
  `baseline_lock` diff; any regeneration must show V16 (B, J)
  non-increasing per fixture), which exists precisely to make this
  visible.

  **Known floor: V16 and V5 genuinely conflict.** `rc_lowpass`'s two
  `out` pins share a Y and sit 3.81 mm apart. A 0-bend direct wire
  exists — but both pins face *up*, and V5 says a wire leaves a pin
  along the pin's axis, which forces a 2-bend U. Both invariants are
  Tier 2, so the tier rule does not order them; the precedence is
  declared here: **V5-outward wins the first grid step**, and B ratchets
  against *measured reality*, not a theoretical zero. Expect legitimate
  per-net floors of 2 bends for same-facing aligned pins. A future
  placer that could rotate one of the two pins would remove the conflict
  at its source; until then, do not "fix" this by weakening V5.

  **Verifier.**
  `crates/spice2kicad/tests/wire_geometry.rs::bend_and_branch_counts_within_ratchet_across_fixtures`
  builds the ink graph from the emitted root sheet of all ten fixtures
  and asserts B and J against a zero-slack `&[(&str, u32, u32)]` table.
  Current measured high-water marks (this table is synced to
  `BEND_BRANCH_BUDGETS`; it had drifted for `multivibrator` and
  `opamp_inverting_real`, whose literals moved without it):

  | fixture                  |  B |  J |
  | ------------------------ | -- | -- |
  | `rc_lowpass`             |  0 |  0 |
  | `common_emitter`         |  4 |  3 |
  | `multivibrator`          |  8 |  4 |
  | `diff_pair`              |  2 |  1 |
  | `opamp_inverting_real`   |  5 |  1 |
  | `opamp_inverting`        |  3 |  0 |
  | `port_shapes`            |  4 |  0 |
  | `rc_lowpass_ports`       |  0 |  0 |
  | `opamp_definition_level` | 15 |  0 |
  | `named_rails`            |  2 |  2 |

  Standard ratchet policy applies (CLAUDE.md § "Budgets are ratchets,
  not knobs"): these literals only ever go **down**, and the test prints
  the reclaimable value on any improvement.

  Three of these moved after the `Symbol::pins_in` pin-angle fix, which
  corrected both the router's outward stubs and the V5 measure (TOTAL V5
  across all fixtures 16 → 8, then 7):

  - `opamp_definition_level` B 10 → 12, J 2 → 0 — the fixture lost all
    three V5 violations and both branch vertices, at the cost of two
    bends. **Global-improvement escape**, owner signed off.
  - `rc_lowpass_ports` B 3 → 4 — same escape, since **WITHDRAWN**. The
    verified 2-bend layout (R1 at rot 180 puts both `out` pins on one
    row) was unreachable while rot 0 and rot 180 tied on (V13, V12, V5);
    the bend key added above now separates them, so the fixture ratcheted
    to **B = 2** — below even its pre-escape mark of 3. The escape is no
    longer claimed. (**Superseded:** the series-horizontal flow
    construction, `idioms::apply_series_horizontal`, has since re-columned
    C1 straight beneath the `out` node, so the `out` net is a single
    straight vertical drop and the true floor is now **B = 0**. See the
    table above.)
  - `diff_pair` J 0 → 1 — `idioms::apply_shared_centers` now reserves a
    grid cell of vertical stub under the tail trunk, so the three-way
    node is a proper Steiner T instead of the trunk ending sideways on
    RTAIL's pin. Buys V5 1 → 0. Owner signed off.

  Two further literals moved when V16 bends became phase 4.5's final
  lexicographic key (see the subordination rule above):

  - `common_emitter` B 10 → 4 — COUT lands at rot 0 rather than rot 180
    and Q1 unmirrors, both previously tied on (V13, V12, V5). V5 is
    unchanged at 1; no Tier-0/Tier-1 count moved. Ratchet DOWN.
  - `rc_lowpass_ports` B 4 → 2 — as above; escape withdrawn. (Later
    B 2 → 0 via the series-horizontal `out`-drop; see above.)

  Net effect of the bend key: `opamp_definition_level`'s B is the
  only rise anywhere still standing on an escape.

  It has since risen again, **12 → 15**. **This rise was NOT an explicit
  owner decision** — it was landed 2026-07-20 by the operating assistant
  under the owner's standing instruction to proceed without per-change
  confirmation; the owner never saw this specific budget. The automatic
  global-improvement escape does not reach it either (F5 −3 against B +3
  nets to zero), so it rests on assistant judgement plus the tier
  argument below. Re-examine it rather than citing it as owner
  precedent. The multi-channel placement fix restored
  left-to-right signal flow on this fixture, which was previously drawn
  backwards and X-interleaved because `layers.rs::no_source_fallback`
  matched `in`/`out` by equality but `vin`/`vout` by prefix — so
  `in1`/`in2`/`out1`/`out2`, the mandatory spelling for any
  multi-channel circuit, matched nothing. Bought alongside it, and
  strictly higher-tier than the bends: the fixture's last cross-net
  collinear wire overlap (Tier 0, a latent V11 short) 1 → 0, its V12
  wires-through-foreign-bodies 4 → 0 (the "OWED, NOT ACCEPTED" budget,
  paid off), and its wire-crossing count 6 → 0. The tier ordering is
  respected in the permitted direction: Tier 2 pays for Tier 0 and
  Tier 1, never the reverse.

  **An ABSOLUTE reference for B (informational, not a ratchet).** Every
  literal in the table above was measured on the incumbent placer's own
  output, so it records what the placer achieves, not what is achievable:
  `rc_phase_shift`'s B = 19 is not judged as bad, it is *protected* at 19.
  `crates/spice2kicad/tests/bend_bound.rs` adds the reference the table
  lacks — a **provable lower bound** on the bends any rectilinear ink
  could have, computed from terminal geometry alone (no obstacles, no pin
  directions, no router). It prints `fixture / measured B / bound / gap`
  and records `v16.bend_bound`, `v16.bend_gap` and
  `v16.bend_excess_exact` to the ADR-23 scoreboard as **informational**
  metrics.

  It is deliberately NOT a ratchet, for this section's own reason: a
  bound that were subtly *inadmissible* would, as a gate, block all work
  while being wrong. What it does assert is its own soundness — that
  Σ per-component bends equals the whole-sheet B this ratchet asserts on,
  and that `bound <= measured` on every graded component.

  The bound rests on one extremal lemma (the topmost-then-leftmost point
  of any ink can only ray east or south, so it is a bend or a leaf; and
  every leaf must be an anchor, which is verified per component rather
  than inherited from the whisker gate). It refutes `B = 0`, so it yields
  at most **1 per ink component** — and that ceiling is close to a fact
  about the metric rather than a weakness of the proof: a trunk with taps
  realises `B <= 2` for *any* terminal set, since taps meet the trunk as
  3-ray Ts, which V16 scores as J and not B. Read the gap as an *upper
  bound on reducible bends*; the tight column is the two-anchor class,
  where the obstacle-free optimum is exactly 0 or 1. First measurement,
  all twelve fixtures, 38/38 components and 86/86 bends covered:
  **B = 86, bound = 15**, and on the two-anchor class alone **14 bends
  drawn where 5 is optimal**. The V16-vs-V5 conflict recorded above is
  exactly why the unconditional bound stays low: a V5-conditional column
  is where realistic floors (like `rc_lowpass`'s documented 2) live, and
  it must never be summed into the admissible one.

  **Cross-check against the crossing ratchet.** The verifier's
  `inter_net_crossings` (4-ray vertices with no dot — excluded from both
  B and J) was compared against
  `placement_quality.rs::count_wire_crossings` when the literals were
  measured. They agree exactly on all five crossing-budgeted fixtures:
  rc_lowpass 0, common_emitter 1 (budget 2), multivibrator 4 (budget 4),
  diff_pair 0, opamp_inverting_real 0. The one divergence is
  `opamp_definition_level` (ink 4 vs raw 5), which carries no crossing
  budget; there the raw counter double-counts a single ink crossing whose
  runs `cleanup.rs` had split into several `(wire …)` segments — i.e. the
  exact re-segmentation sensitivity the ink graph exists to remove.

- **F6 — rail-stub lateral run** (flow metric, Tier 2). A rail stub —
  a two-terminal element with exactly one rail pin — does not pass a
  signal along; it *terminates* a node. The conventional drawing hangs it
  straight off that node, a vertical drop in the node's column, so its
  lateral run is ZERO. `spice-layout/src/idioms.rs`'s rail-stub column
  idiom exists to produce that.

  The idiom never puts a stub in a **horizontally-facing** pin's own
  column, which is load-bearing (see `idioms::rail_stub_anchor_x`:
  anchoring AT any pin dragged bias dividers onto horizontal base pins
  and cost V5 on three fixtures at once). What it does instead, for a
  stub whose node presents only sideways pins — a bias resistor feeding
  a transistor BASE — is take the column one geometry-derived stride
  along that pin's **outward** direction and reach the pin with a short
  horizontal run in. That is the conventional drawing, and a different
  proposal from the measured-and-rejected "anchor at the pin, offset
  zero". Measured on `multivibrator`: `RB1`/`RB2` fell from 9 grid cells
  (11.43 mm) out from the transistors they bias to 2, with no other
  fixture moving at all and no Tier-0/Tier-1 count changing.

  The sideways anchor is deliberately **declined** when the node carries
  stubs on BOTH sides. Those two groups are a divider *through* the
  node: they already share one column and the node is tapped off it, so
  there is nothing to reach from a stride away. Offering an opinion
  there only perturbs the divider — measured on `common_emitter`, whose
  `R1`/`R2` sit on `b` alongside `Q1`'s base: V16 B rose 4 → 7 and F5
  rose 1 → 2 (`CIN` flipped vertical) purely from the SA re-basining,
  with F6 unimproved. The exclusion restores exactly the behaviour that
  shape had before sideways anchors existed.

  F6 bounds that. For every rail stub, take its non-rail pin and the
  NEAREST other pin on the same net, and measure the horizontal offset
  in whole grid cells. Deliberately a *distance*, not a violation count:
  no threshold makes a lateral run categorically wrong, and a count
  would hide a stub drifting from 2 cells to 12. Note a non-zero score
  is not always a defect — `diff_pair`'s `RTAIL` terminates a node
  shared by both transistors, so the shared-centre idiom correctly seats
  it at their midpoint; and a two-stub group on one node is spread
  symmetrically about the anchor on purpose.

  Verifier:
  `crates/spice2kicad/tests/flow_geometry.rs::stub_lateral_run_within_ratchet`,
  per-fixture zero-slack maximum, ratcheting down only.

  **Remaining non-zero scores, and which are defects.** `rc_lowpass`
  and `rc_lowpass_ports` now both score **0**. The old blind spot (`C1`
  terminates `out` alongside a horizontally-facing two-terminal `R1`, so
  the sideways rail-stub anchor — which requires a multi-terminal active
  device — declined) is closed by a *different* idiom: the
  series-horizontal flow construction
  (`idioms::apply_series_horizontal`) draws `R1` horizontal and
  re-columns `C1` straight beneath `R1`'s downstream `out` pin, giving
  `C1` a zero lateral run. This now fires on the un-ported `rc_lowpass`
  as well as `rc_lowpass_ports`, because `idioms::signal_net_depth` falls
  back on the leaf-input-net NAME convention (`in`) to root the flow
  graph when no `*@port` input is declared. `diff_pair` 4 and
  `common_emitter` 4 are NOT defects (a shared-centre midpoint and a
  two-stub group spread about its anchor, both deliberate). Extending
  the sideways anchor to two-terminal neighbours is untested and would
  re-open the weak-anchor failure mode `rail_stub_anchor_x` documents.

  `named_rails` 6 was previously recorded here as "the same shape" as
  `rc_lowpass`'s blind spot. **That was wrong**, and the correction
  matters because it moves the fixture from the defect column to the
  deliberate one. Measured per stub, the fixture scores
  `CL 4, RPD 6, RPU 4` — a THREE-stub group (`RPU` up to `+5V`, `RPD`
  down to `-5V`, `CL` down to ground) all terminating the one `out`
  node, whose anchor pin (`RIN`, a `Device:R_US` at rot 180) is
  VERTICAL. So the column idiom does fire — this is not the declined
  two-terminal-horizontal case at all — and the 6 is
  `apply_rail_stub_columns` spreading the group symmetrically about the
  anchor so the three stubs do not stack. That is exactly the
  deliberate behaviour already recorded as non-defect for
  `common_emitter` 4 and `diff_pair` 4, just with three stubs rather
  than two, hence the wider spread. Driving it down would mean stacking
  stubs in one column, which the idiom exists to prevent. Treat 6 as
  the fixture's correct score, not an owed fix.
