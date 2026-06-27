# Visual quality invariants (V1–V15)

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

  **Synthesis exception (ADR-13, v0.2 — design, not yet wired).** V3
  remains byte-for-byte verbatim for every `lib_symbols` entry that
  originated from a user `.kicad_sym`; that portability guarantee is
  unconditional and Tier-0. The *one* permitted exception is the
  emitter's own power-glyph family (`power:GND` / `power:VCC` /
  `power:VDD` / `power:VEE` / `power:+…` / `power:PWR_FLAG`), which the
  emitter may rotate/mirror by a 90° multiple via a narrow `RawSexpr`
  coordinate-transform (ADR-13) so a glyph on a non-canonical pin faces
  outward without the forced-sideways stub. This applies only to glyphs
  the emitter *generates*, never to a user-provided symbol, and only as
  a 90°-multiple coordinate rotation of a fixed emitter-owned glyph — it
  does not introduce a typed primitive model and does not enable
  auto-drawing of unknown symbols. Justified by V14 (correct power-glyph
  orientation without body overlap). The V3 round-trip verifier is
  unaffected: it compares *user*-symbol bodies, which are never
  transformed; the synthesised orientation-suffixed glyph entries (e.g.
  `power:GND_R90`) have no user `.kicad_sym` source to round-trip
  against and are excluded by name.

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
  `wire_length_within_budget_across_fixtures`,
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
  wire-coincident, on every net that ERC requires to be driven but
  has no driving pin — i.e. any net with a `power_in`/`input` pin
  (or any Power/Ground class net, which carries a `power_in` glyph)
  and no Output/Power-output/bidirectional/tri-state/open-collector/
  open-emitter pin. The predicate is derived from KiCad pin
  electrical types (`kicad_symbols::PinElectrical::{drives,
  requires_driver}`), never from fixture/refdes names, so it covers
  rails and the diff_pair input-base nets with one rule and leaves
  passive-only R–C junctions untouched. Global Power/Ground nets are
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
  five (rc_lowpass / common_emitter / multivibrator / diff_pair /
  opamp_inverting_real) — no wire may cross a foreign body. The
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
     the `1`/`2`/`3` pin numbers (R-4). Pin-text world bboxes are
     computed from the lib-symbol definition:
     `Symbol::pin_text_local_bboxes` returns one local box per
     *visible* label (skipping `(pin_names (hide yes))`,
     `(pin_numbers (hide yes))`, and `~`/empty names KiCad draws as
     nothing), riding the pin shaft; the caller transforms each
     through the placed pose with the same orientation + eeschema
     y-flip used for body bboxes. The same `nudge_property_text`
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

- **V14 — Power glyph orientation: GND down, VCC up.** Every
  `power:GND` instance emits with the rotation that draws the
  triangle below the connection point (KiCad lib convention: rot 0).
  Every `power:+...` / `power:VCC` / `power:VDD` / `power:VEE`
  instance emits at rot 0 as well — for `VEE`, that is the KiCad lib
  convention (its pin sits at lib-angle 90 like VCC). A negative rail
  (`power:VEE`) is *attached* like ground (canonical axis Down, see
  V10), so it never triggers the forced-sideways stub. The host
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
  `power:VEE` / variants; `power:PWR_FLAG` excepted) has `rot == 0`.
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
  Verifier: `crates/spice2kicad/tests/placement_quality.rs::v15_*`
  collects every instance-section coordinate of every emitted sheet
  (excluding `lib_symbols`) and asserts the content bbox's top-left
  corner sits at the margin, no coordinate is negative, and the bbox
  fits within the A4 (297×210) drawable rectangle.
