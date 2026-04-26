# Layout / auto-placer roadmap

Working notes on the auto-placement subsystem. Not a spec — the
annotation spec (`annotation-spec.md`) is the user contract; this
file is the *implementer's* contract.

The placer turns a parsed-and-annotated SPICE AST into a
coordinate-assigned schematic. It runs after constraint resolution
(spec §5) and before the KiCad emitter.

## 1. Guiding principles

- **Crossings cost much more than wirelength.** A long clean wire
  reads better than a short tangled one. The penalty function must
  reflect this; HPWL alone produces unreadable analog.
- **Annotations are constraints, not hints.** `align`/`place`/
  `power` are hard constraints in the cost (very high δ). The
  resolver may *drop* a constraint with a warning, but the placer
  never silently violates one it was given.
- **Grid-snapped, orthogonal-routable output.** Continuous
  positions are an internal detail. The emitter only ever sees
  integer grid coords.
- **Hierarchy is layout.** `.subckt` and `.include` partition the
  problem; each cluster is solved independently and composed.
  Never run a global solver across cluster boundaries.

## 2. Layout invariants

Independent of which algorithm we use, two invariants must hold
end-to-end. They constrain what the resolver consumes, how
constraints are lowered, and where legalization happens.

**Pin-anchored constraints.** `place=right-of V1` does not mean
"R1's center is to the right of V1's center" — it means "R1's
*left pin* connects to V1's *right pin* on a shared horizontal
line". Likewise `align horizontal R1 R2 R3` shares the
y-coordinate of the connecting pins, not the symbol centers.
Implications:

- The constraint resolver consumes resolved symbol pin geometry
  (post-`symbol`/`pinmap`), not just the SPICE AST. The
  `spice-layout` crate depends on the KiCad symbol library — or
  on a pin-position table extracted from it — and cannot run on
  the parsed netlist alone.
- A symbol's position is *derived* from its pin positions:
  `origin = pin.world − pin.local_offset`. Internally the placer
  may track either, but constraints lower to pin coordinates.
- Symbol orientation (0/90/180/270) is part of the placement
  state, because rotating a part moves its pins. Choosing
  orientation is part of the placer's job, not just position.

**Grid quantization.** KiCad's schematic grid is 50 mil
(1.27 mm). Symbol origins, pin coordinates, and wire endpoints
are integer multiples of the grid. KiCad library symbols already
place their pins on grid intersections, so as long as symbol
origins and orientations are grid-/90°-quantized, pin coordinates
are automatically grid-aligned.

Practical consequences:

- The placer's internal coordinate system *is* the schematic grid
  — integer cells. No separate "placer grid".
- FR/KK seeding (continuous) produces real-valued coordinates;
  snapping to the integer grid is part of SA legalization, not a
  separate post-pass.
- Cost terms involving distance use grid units. HPWL is summed
  over cells, not millimetres.

## 3. Approaches surveyed

| Family | Strength | Weakness | Role here |
| ------ | -------- | -------- | --------- |
| Force-directed (FR / KK) | Cheap, easy to seed | Continuous coords, ignores orthogonality | Seeding within a cluster |
| Simulated annealing | Discrete, handles arbitrary penalties | Slow convergence | Refinement + legalization |
| Quadratic / analytical | Fast for HPWL | Poor with readability constraints | Not used in v0.1 |
| Min-cut partitioning | Maps onto hierarchy | Needs a cut metric | Subsumed by `.subckt`/`.include` |
| Sugiyama (layered) | Great for signal-flow chains | Breaks on feedback | Inspiration for left→right ordering, not the algorithm |
| Symmetry / idiom templates | Produces "analog-looking" schematics | False positives are worse than no detection | v0.2, behind `align` |
| ML / RL / LLM | Best aesthetics in published work | Opaque, needs training data | Out of scope |

The t-SNE / UMAP analogy is real but loose: those minimize a
neighbor-distribution KL divergence in continuous space.
Schematic readability isn't distance preservation, it's
Manhattan-routability + grid alignment + honoring user constraints.
Useful for the *cost-function mindset*, not the algorithm choice.

## 4. Recommended architecture

A hybrid pipeline, recursive over the cluster tree:

1. **Partition.** Walk `.subckt`/`.include` boundaries; each cluster
   is a sub-problem with its own bounding box.
2. **Lower constraints.** Translate `align`/`place` into linear
   equality / inequality constraints on **pin** grid coords (not
   symbol centers — see §2). Resolver requires symbol pin
   geometry as input. Pin power rails to top, ground to bottom
   (spec §4.5).
3. **Seed.** Force-directed (FR or KK) within the cluster, starting
   from any pinned positions.
4. **Refine.** Short simulated-annealing pass on the discrete grid,
   minimizing the cost function in §4. SA naturally handles the
   "later phases never override earlier" rule via large δ on
   constraint-violation terms.
5. **Compose.** Place child cluster bounding boxes in the parent
   using the same machinery one level up.
6. **Route.** Separate orthogonal-wire pass (Lee/maze or pattern
   routing) on the final placement. Not blended into placement.

Proposed crate split:

```
crates/
  spice-layout/
    src/
      partition.rs   # cluster tree from AST
      constraints.rs # align/place → linear constraints
      cost.rs        # the penalty function (single source of truth)
                     # operates in grid units; grid snap is part of the SA pass
      solver/
        force.rs     # FR/KK seeding
        anneal.rs    # discrete refinement + legalization
      patterns/      # v0.2: idiom detectors
      lib.rs         # Placed<Element> output type
  spice-route/       # v0.2: orthogonal wire routing
```

`kicad-emitter` consumes `Placed<Element>`. It should not contain
any layout logic.

## 5. Cost function

```
cost = α·crossings
     + β·non_orthogonal_segments
     + γ·overlap
     + δ·constraint_violation        // very large
     + ε·HPWL                         // small
     + ζ·rail_direction_violation    // power up, ground down
     + η·signal_flow_violation       // input left, output right
```

All distance terms are in grid units (§2). `non_orthogonal_segments`
is a penalty during continuous FR/KK seeding only — once the SA
pass snaps to grid and angles are quantized to 0°/90°, that term
goes to zero by construction.

Component-wise breakdown must be logged from day one so weights
can be tuned empirically. HPWL is the *least* important term;
crossings and constraint violations dominate.

## 6. Analog readability strategy

Generic optimization without idiom awareness produces schematics
that connect correctly but don't *look* analog. The plan:

- **v0.1.** Ship the hybrid placer without idiom detection.
  Annotated files look good because `align` lets users assert
  idioms by hand. Unannotated files look mediocre but valid
  (spec principle: zero annotations must produce a valid
  schematic).
- **v0.2.** Add `patterns/` with one detector per idiom. Targets,
  in priority order:
  1. Differential pair (matched devices, shared tail node)
  2. Current mirror (matched devices, shorted gate/base)
  3. Resistor divider (chain on a single net to ground)
  4. Decoupling cap (cap from power rail to ground near a pin)
  5. RC filter (R then C, signal-flow ordered)
- **Detectors emit constraints, not placements.** A detector that
  finds a diff pair generates the same `align vertical` /
  symmetry constraint a user would have written. This keeps the
  constraint pipeline as the single source of truth and makes
  inferences user-overridable (an explicit annotation always wins
  over a detection).
- **Specificity over recall.** A false-positive idiom is worse
  than no idiom — it pins devices wrongly. Detectors require
  matched device *models* + topology + (where applicable) a
  naming hint. Every detection emits a `W2xx` warning so users
  can see what was inferred.
- **Global rules** (always on, even without idioms):
  - Power rails at top, ground at bottom.
  - Signal flow left→right, derived by treating inputs as DAG
    sources and outputs as sinks. Feedback edges are the only
    backward wires.
  - Decoupling caps placed adjacent to the pin they decouple,
    even when no idiom matched.

Feedback-heavy designs (oscillators, multi-loop control) break
the left→right assumption. Detect this (cycles in the
signal-flow DAG above a threshold) and fall back to pure
force-directed for that cluster.

## 7. Sequencing

- **Now:** crate skeleton, types, partition pass, constraint
  lowering. Stub solver returns trivial grid placement.
- **Next:** FR seeding + SA refinement; instrumented cost
  breakdown; tune weights against `examples/`.
- **After:** orthogonal router as a separate crate.
- **v0.2:** idiom detectors feeding the constraint pipeline.

## 8. Open questions

- **`align` semantics under mixed orientation.** When the aligned
  parts are not all in the same orientation (one horizontal
  resistor in a column of vertical ones), "the connecting pin" is
  ambiguous. Likely resolutions: (a) require uniform orientation
  within an `align` block and emit `W1xx` on mixed; (b) define a
  canonical pin per element kind (e.g. always pin 1) and document
  it. Decide before v0.1 ships, or once a real example forces the
  question.
- **Per-crossing cost weighting.** Whether `α` should depend on
  the kind of crossing (signal × signal vs signal × power vs
  power × ground) — power-rail crossings are visually worse.

## 9. Non-goals (for now)

- Multi-sheet layout beyond what `.subckt` already gives.
- Routing aesthetics beyond orthogonal + minimize crossings
  (no bus bundling, no net-class styling — see spec §9).
- Interactive / incremental layout. The placer is a batch pass.
- Learning-based placement. Re-evaluate once there's a corpus
  of human-fixed outputs to learn from.
