# Structural Layered Placement (V6 redesign)

**Status:** design approved 2026-05-02. Replaces the topology-archetype
implementation of V6 (`feat(layout): topology-aware common-emitter
archetype` — commit `47a2d32`).

## Motivation

The current V6 implementation recognises one named topology
(common-emitter) by connectivity heuristics on a BJT and lays it out from a
hand-coded coordinate template. Adding a second topology (differential
pair, current mirror, opamp inverter, …) requires writing a new matcher
and a new template. This scales O(circuit-types) and rots when fixture
naming or pin order drifts.

We want a single set of general structural rules that produce a readable
placement for any circuit, without recognising it. Specifically, the five
existing test fixtures (`rc_lowpass`, `common_emitter`, `multivibrator`,
`diff_pair`, `opamp_inverting_real`) must look like an engineer drew them,
*from the same heuristic*.

## §1 — Removal

Delete:

- `crates/spice-layout/src/archetype/` (`mod.rs`, `common_emitter.rs`)
- `mod archetype;` declaration and the two call sites in
  `crates/spice-layout/src/lib.rs::place_with`

Keep:

- V5 `pick_orientations` (general)
- V7 `symmetry::detect_pairs + apply` (general)
- V8 subckt `*@symbol` mapping (resolver concern, unrelated)
- V9 SI value formatting (formatter, unrelated)
- `tests/fixtures/common_emitter.cir` — used by the new fixture-wide
  tests, not deleted

CLAUDE.md V6 invariant section is rewritten in place from
"topology-aware archetype matching" to "structural layered placement"
(see §8).

## §2 — New pipeline

`place_with` in `crates/spice-layout/src/lib.rs`:

```
1. classify_nets(checked)          → HashMap<NetId, NetClass>
2. assign_y_bands(checked, classes) → HashMap<element, BandAssignment>
3. assign_x_layers(checked, classes) → HashMap<element, x_layer>
4. seed_placement(bands, layers)   → Placement on grid
                                     (replaces today's place_seed body)
5. overlay user *@align / *@place / *@power
                                     (these pin coords; heuristics back off)
6. symmetry::detect_pairs + apply  (V7, unchanged)
7. solver::refine                  (now runs by default — see §6)
8. pick_orientations                (V5, unchanged)
```

User annotations strictly override heuristics. The four-phase placer
described in annotation-spec §5 maps to: phase 1 = structural+rails,
phase 2 = aligned, phase 3 = placed, phase 4 = refine.

## §3 — Net classification

```rust
enum NetClass { Power, Ground, Signal }
```

Rules, in order of precedence:

1. Net `"0"` → `Ground`.
2. Positive terminal of any `*@power`-tagged voltage source → `Power`.
3. `.global` net whose name (case-insensitive) matches
   `vcc|vdd|v\+|vplus` → `Power`; matches `vee|vss|v-|vminus|gnd` →
   `Ground`.
4. Any other net touched by ≥1 `*@power` source → `Power` (handles
   split rails like ±15 V).
5. A net touched only by elements that *also* connect to Power+Ground
   (e.g. supply decoupling caps) is reclassified by the majority of
   *non-decoupling* elements connected to it. Practical purpose:
   prevents a bypass cap from making `vcc` look like a Signal net.
6. Everything else → `Signal`.

Net classes feed both Y-banding (§4) and signal-DAG construction
(§5 prunes Power/Ground edges so feedback through rails doesn't create
false cycles).

`.subckt` ports inherit classification from the parent's nets after V8
mapping resolves them. Hierarchical-sheet bodies classify their internal
nets locally.

## §4 — Y-band assignment

Three horizontal bands, top to bottom: **Top** (Power rail),
**Mid** (signal layout area), **Bot** (Ground rail).
`BandAssignment { band: Top|Mid|Bot, soft_y_target: f64 }`.

Element-classification rules:

| Connections                           | Band | Soft Y target          |
| ------------------------------------- | ---- | ---------------------- |
| Power only                            | Top  | (none)                 |
| Ground only                           | Bot  | (none)                 |
| Power ↔ Ground (e.g. bypass cap)      | Mid  | (vertical span; orientation chosen by V5 `pick_orientations`)|
| Power ↔ Signal (top of bias divider)  | Mid  | 1/3 down from Top      |
| Signal ↔ Ground (emitter R, etc.)     | Mid  | 2/3 down               |
| Power-only-touching, no Ground (RC)   | Mid  | 1/3 down               |
| Ground-only-touching, no Power (RE)   | Mid  | 2/3 down               |
| Signal only (couplers, active device) | Mid  | none (free for §5)     |

Power rails become horizontal wires at fixed `Y_top`; ground at
`Y_bot`. The router gains a fast path: pins on a Power/Ground net route
directly up/down to the rail rather than into a per-net trunk.

Mid-band Y is *softly* refined by the solver — `soft_y_target` is a
force, not a hard constraint, so a Mid-Top biased element can drift to
Mid-Bot if signal flow demands it.

## §5 — X-layer assignment

Build a directed signal-flow graph and longest-path-layer it.

**Nodes:** elements with Mid-band Y. Top/Bot-band elements skip
layering — their X is fixed by the elements they connect to.

**Edges:** directed flow through Signal nets only.

- **Active devices** (BJT/FET/opamp): pin role gives direction
  (NPN base → collector; opamp in → out).
- **Sources** (`V*`/`I*`, with or without `*@power` — but `*@power`
  signal sources are rare; treat the positive terminal as a *signal
  source* for sources that aren't pure rails). Net flows away from the
  positive terminal.
- **Purely passive nets** (R/L/C only): no pre-determined direction;
  both endpoints are candidate edges; cycle-breaker resolves.

**Cycle breaking** (multivibrator-style feedback):

1. Run Tarjan SCC.
2. Within each SCC, pick the edge whose source has the highest
   SCC-internal in-degree (heuristic: prefer reversing the explicit
   feedback path) and reverse it. Repeat until DAG.
3. Mark reversed edges as `feedback`. Router draws them as left-going
   wires (KiCad convention).

**Layer assignment:** topological sort. `layer(v) = 1 +
max(layer(pred(v)))`; sources at layer 0. Within each layer, order
elements by the median Y of their neighbors (one Sugiyama barycentric
iteration).

**Coordinate emission:**

- `X = layer * X_STRIDE` with `X_STRIDE = 5 grid cells = 6.35 mm`.
- `Y_mid = Y_MID + (rank_within_layer − layer_center) * Y_STRIDE`,
  then nudged toward `soft_y_target` from §4.

**Fallback when no signal source exists** (oscillators, pure passive
networks): skip layering. Assign X by element index modulo a column
count. V7 symmetry pass and V5 orientation handle the rest.
Multivibrator falls into this path.

## §6 — Refinement (band-constrained solver)

`solver::refine` runs by default after seeding. The `--refine` CLI
flag is repurposed: `--refine-iterations N` controls anneal sweep count
(default 200; cap honoured for fixtures up to ~50 elements).

Cost function (`crates/spice-layout/src/cost.rs`) gains four terms:

1. **Wire-length** (existing). Manhattan over net pin-pairs.
2. **Overlap penalty** (existing). Quadratic on bbox intersection.
3. **NEW — Band misalignment.** `K_band * clamp(|y − band_range|, 0, ∞)²`
   per element. Top/Bot hard (`K_band` large); Mid soft.
4. **NEW — Soft Y target.** `K_soft * (y − soft_y_target)²` for biased
   Mid-band elements.
5. **NEW — X-layer order.** `K_order * max(0, x_pred − x_self)²` per
   signal-DAG predecessor that drifted right of its successor. Soft.
6. **NEW — Wire-crossing approximation.** Count net-bbox intersections
   between distinct nets (cheap proxy for true crossings). Quadratic.
7. **Grid snap** (existing). Final pass.

Concrete weight values are picked during implementation by calibrating
against the five fixtures, not committed up-front. Stored in a single
`const` block in `cost.rs` with one-line doc per weight.

The annealer gains one move operator: **"swap two same-layer elements'
Y rank"** — cheap, helps barycentric ordering escape its seed minimum.

## §7 — Tests replacing V6 archetype tests

Drop the three V6 tests from `crates/spice2kicad/tests/placement_quality.rs`:

- `v6_common_emitter_rails_horizontal`
- `v6_common_emitter_signal_flow_ordering`
- `v6_common_emitter_q1_central`

Replace with **fixture-wide quality checks** that iterate every `.cir`
under `tests/fixtures/`:

1. **No symbol-symbol overlap.** Bbox + 1.27 mm padding, pairwise.
2. **No symbol-label overlap.** Label text bboxes vs symbol bboxes.
3. **Power rail above signal Mid; ground rail below.** Per fixture
   that has both rails: `max(y of Power-only elements) <
   min(y of Ground-only elements)` (sign chosen for KiCad's +Y = down).
4. **Wire-length budget.** Per net, total emitted `(wire …)` length
   `≤ K × Σ pin-pair Manhattan distances`. K calibrated per fixture
   (≈ 2.0 for routed nets, 1.0 for fast-path 2-pin nets).
5. **Crossing budget.** Per fixture, count of wire-segment intersections
   (excluding shared-net junctions) below a fixture-specific cap:
   `rc_lowpass = 0`, `common_emitter ≤ 2`, `multivibrator ≤ 4`,
   `opamp_inverting_real ≤ 2`, `diff_pair ≤ 2`.
6. **Common-emitter signal-flow regression guard.** On
   `common_emitter.cir` only: `VIN`'s positive pin has the smallest
   X; the highest-X pin is on the collector net. Pure structural —
   doesn't recognise the topology, just checks signal flowed L→R for
   that fixture.

Tests use a `for cir in fixtures()` loop so adding a new `.cir`
auto-covers it. The `placement_quality.rs::V6` block is replaced; V5,
V7, V8, V9 tests are untouched.

## §8 — CLAUDE.md updates

Three sections change in `/home/eugene/Projects/spice2eeschema/CLAUDE.md`:

1. **"Visual quality invariants V6"** — rewritten in place. Title
   becomes *"V6 — Structural layered placement"*. Body summarises
   §3–§6 (net classification, Y-bands, X-layers + cycle-breaking,
   refinement cost terms). Verifier paragraph points at the six
   fixture-wide tests from §7. The "future work" sub-paragraph about
   V6 building on V5 stays; archetype-matcher language is removed.

2. **"Visual quality invariants V7"** — minor edit. Drop the reference
   to "many archetype templates have symmetry baked in" (no archetypes
   any more). V7 still composes with V6 via the §2 pipeline order.

3. **"Core design principles"** — add:

   > **9. Structural placement, not pattern recognition.** The placer
   > infers structure from net classification and signal-flow
   > direction; it does not match named topologies. Adding a new
   > circuit type should require zero placer code changes. The escape
   > hatch when heuristics fail is `*@place` / `*@align` — already in
   > v0.1.

   This is the durable lesson that prevents accidentally regrowing the
   archetype module.

The annotation spec is untouched. `docs/layout-roadmap.md` gets a
one-line update during implementation reflecting the pipeline change.

## §9 — Risks & open questions

**Risk 1 — Cycle-breaking heuristic quality.** Picking which edge to
reverse in an SCC is the soft underbelly. For multivibrator (single
4-node SCC), any reversal works because V7 dominates. For an op-amp
with feedback (single SCC: `opamp_out → Rf → opamp_in−`), reversing
the wrong edge could put the feedback resistor below the op-amp
instead of above. Mitigation: reverse the edge with the highest
SCC-internal in-degree at its source — tends to pick the explicit
feedback path. If still wrong, fall back to user `*@place`. Iterate
during implementation.

**Risk 2 — Mid-band Y-stride collisions.** When two layered elements
end up at the same X with no Y bias, both want `Y_MID`. Barycentric
ordering breaks ties by index — deterministic but arbitrary. The
annealer's swap operator handles it, but the seed's first emission
may be ugly until refine runs.

**Risk 3 — Calibration of K weights.** Six cost terms with relative
weights. Initial values picked by hand against the five fixtures;
risk that adding a sixth fixture later requires re-tuning.
Mitigation: weights live in a single `const` block in `cost.rs` with
documentation of what each one is responsible for.

**Risk 4 — Annealer runtime in default path.** Currently `--refine` is
opt-in because it's slow. Making it default means CLI users wait
longer. Mitigation: cap iterations at 200 sweeps; expose
`--refine-iterations N` for power users. Acceptable for the test
fixtures (all <50 elements); may need revisiting for larger circuits.

**Open question — when do we restore archetype matching?** This design
deliberately removes it. The CLAUDE.md principle in §8 prevents
accidental regrowth. If a real circuit later defeats general
heuristics, the answer is to *strengthen the heuristic*, not
pattern-match. The escape hatch is `*@place` / `*@align`.

## Implementation order

1. Spec frozen and committed (this doc).
2. Implementation plan written via `superpowers:writing-plans` (next).
3. Code work proceeds in stages corresponding to §§1–7.
