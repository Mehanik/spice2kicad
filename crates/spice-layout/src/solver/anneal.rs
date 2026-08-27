//! Discrete-grid simulated annealing.
//!
//! Move set:
//!
//! * **Position jitter** — translate one element by a small random
//!   integer offset (±1–2 grid cells per axis). Most common move.
//! * **Rotate** — pick one of the four R0/R90/R180/R270 rotations.
//!   Less common (1-in-8 of all moves) because rotating a part is
//!   visually disruptive when most layouts want axis-aligned symbols.
//!
//! * **Mirror-Y** — toggle the `mirror_y` flag (`Orientation::flip`).
//!   Rare (1-in-20 of all moves): like rotate it is visually
//!   disruptive, but it lets the refiner flip a part so its
//!   shared-net pins face a neighbour without a full rotation.
//!
//! **V14 hard constraint at every move.** Both the rotate and the
//! mirror-Y move are gated against the per-element allowed-orientation
//! set (`allowed`, computed in [`crate::orient`]): a proposal whose
//! resulting orientation leaves the set is reverted immediately,
//! regardless of cost. This is the CLAUDE.md "consistency requirement":
//! a constraint enforced as a hard filter at seed time must be hard at
//! *every* SA-move stage too, or the rotate move silently undoes it.
//! Jitter / SwapY never change orientation, so they need no gate.
//!
//! Pinned elements (those fixed by `align`/`place` in stage 1) are
//! never proposed.
//!
//! Cooling schedule: exponential, `T_k = T0 * alpha^k` with `alpha`
//! chosen so the final temperature is `T0 / 1000`. Standard
//! Metropolis acceptance.
//!
//! Per-component cost is logged at `log::Level::Debug` every
//! `LOG_EVERY` iterations so weights can be tuned against
//! `examples/`.

use kicad_symbols::{Library, Orientation, Rotation};
use spice_policy::CheckedNetlist;

use super::{LayoutOptions, rng::Rng};
use crate::{
    GridPoint, PlacedElement, Placement,
    cost::{self, CostBreakdown, CostWeights},
    layers::LayerAssignment,
    net_class::NetClass,
};

/// SA proposals between two cost-breakdown log lines.
const LOG_EVERY: u32 = 1000;

/// Run SA on top of `seed`, mutating only unpinned elements.
///
/// The seed comes from FR; coords may be off-grid floats. The first
/// step here is to snap every origin to the integer grid (a no-op
/// for already-snapped pinned elements). After that the SA is purely
/// integer arithmetic on `GridPoint`.
#[allow(clippy::too_many_lines)] // SA loop + V14/V11 gates read clearer inline.
pub(super) fn refine(
    mut seed: Placement,
    pinned: &[bool],
    checked: &CheckedNetlist,
    library: &Library,
    opts: &LayoutOptions,
    layers: &LayerAssignment,
    allowed: &[Vec<Orientation>],
) -> Placement {
    // Origins are `GridPoint` (i32) by construction, so the FR-to-SA
    // boundary is implicitly grid-snapped: FR writes back through
    // `mm_to_grid`. If we ever hold continuous coords across the
    // boundary, the explicit snap goes here.

    let n = seed.elements.len();
    let movable: Vec<usize> = (0..n).filter(|i| !pinned[*i]).collect();
    if movable.is_empty() || opts.refine_iterations == 0 {
        return seed;
    }

    // Mirror-Y (the deferred ADR-3 move) is proposed only for
    // V14-rail-constrained elements — those whose allowed-orientation
    // set is a strict subset of the eight (the multi-pin active devices
    // V14 reorients, e.g. an opamp). Flipping such a part between its
    // two V14-feasible poses (R0 and R0+mirror-Y both keep V+ up / V-
    // down) lets the refiner face its signal pins toward a neighbour
    // without a rotation that would leave the feasible set. The
    // resulting flip is still accept-rejected against `allowed` below,
    // so it can never escape V14. Unconstrained parts keep the
    // pre-mirror move set; flipping them is a free aesthetic move the
    // immutable cost cannot score safely.
    let mirror_eligible: Vec<usize> = movable
        .iter()
        .copied()
        .filter(|&i| {
            allowed
                .get(i)
                .is_some_and(|a| a.len() < Orientation::ALL.len())
        })
        .collect();
    // The V11-coincidence gate below used to be scoped to runs that had
    // a mirror-eligible (V14-reoriented) element, on the premise that
    // "the all-passive fixtures' V11 cleanliness is already maintained
    // by the router". **That premise is false**, and it cost a Tier-0
    // defect: the router can jog *wires*, but it cannot move *pins*.
    // When two foreign-net pins land on the same coordinate,
    // `conflict::resolve_conflicts` correctly declines to jog (jogging
    // off a pin would disconnect it), exhausts its iteration bound, and
    // the emitted schematic shorts the two nets. `shunt_feedback_amp`
    // did exactly that: at the default 200 iterations the SA slid Q1's
    // base pin onto RE's pin-1 coordinate, merging the base and emitter
    // nets. The fixture has no rail-constrained active device, so
    // `mirror_eligible` was empty and the one gate that would have
    // rejected the move was switched off.
    //
    // The gate is therefore unconditional. It is a pure *filter* on the
    // candidate space (CLAUDE.md constraints-vs-costs: Tier-0 and
    // categorical ⇒ hard constraint, never a weighted term), and it only
    // rejects moves that *raise* the coincidence count — so on every
    // placement whose seed is already V11-clean it holds the count at
    // zero, and on every fixture that never proposes such a move the SA
    // trajectory stays byte-identical.

    // Bucket movable elements by layer so the swap-Y-rank move can pick
    // a peer cheaply. Layer index → indices of movable elements in it.
    // BTreeMap for deterministic iteration order across runs (the
    // annealer's RNG is seeded but a HashMap-iteration nondetermism
    // here breaks reproducibility — see T8 calibration notes).
    let mut layer_buckets: std::collections::BTreeMap<u32, Vec<usize>> =
        std::collections::BTreeMap::new();
    for &i in &movable {
        if let Some(&layer) = layers.layers.get(i) {
            layer_buckets.entry(layer).or_default().push(i);
        }
    }
    let swap_layers: Vec<u32> = layer_buckets
        .iter()
        .filter_map(|(k, v)| if v.len() >= 2 { Some(*k) } else { None })
        .collect();

    // Rail-pin screen-vertical prefs for the ADR-14 glyph-reach
    // reservation in `symbol_overlap_count`. A pure function of the
    // netlist — invariant across the anneal — so it is computed once
    // here and threaded down, not recomputed per proposal.
    let glyph_prefs = crate::net_class::vertical_prefs(checked);

    let weights = CostWeights::DEFAULT;
    let mut current_breakdown = cost::breakdown_with(&seed, checked, library, opts.placer);
    let mut current_cost = cost::total(&current_breakdown, &weights);
    // V11 hard constraint at the placer stage: the number of distinct
    // foreign-net pin coincidences must never *increase* across an
    // accepted move. Two pins on different nets landing on the same
    // grid coordinate is an electrical short the router cannot undo
    // (V11 is Tier-0 correctness). The SA cost has no term for this and
    // CLAUDE.md forbids adding one here; instead it is enforced as a
    // candidate-space filter — a move that raises the coincidence count
    // is rejected outright, exactly like the grid-snap and V14 gates.
    // This is what makes the new mirror-Y move safe: a flip that would
    // overlap two foreign pins is dropped before it can corrupt the
    // netlist.
    let mut current_coincidences = foreign_pin_coincidences(&seed, checked);
    // V6 symbol-collision hard constraint at the placer stage: the
    // number of strictly-overlapping symbol-body pairs must never
    // *increase* across an accepted move. The cell-bbox `overlap` cost
    // term is blind to oversized bodies (an opamp triangle is ~2× a
    // resistor cell), so once V14 pins the opamp at rot 0 a neighbour
    // can slide under its wide body cost-free. Enforced as a candidate-
    // space filter (never a cost term, per CLAUDE.md), same mechanism as
    // the V11 and V14 gates below.
    let mut current_overlaps = symbol_overlap_count(&seed, checked, &glyph_prefs, opts.placer);
    // V5 pin-facing alignment, used as a "never increase" gate on the
    // mirror-Y move only (see acceptance below). Tracked from the seed so
    // a flip can never make signal pins face away from their net.
    let mut current_misalignment = pin_outward_misalignment(&seed, checked);
    // V6 / F3 signal-flow hard gate: the number of layer-order
    // inversions must never *increase* across an accepted move. See
    // `flow_inversions`.
    let flow_pairs = flow_pairs(checked, layers);
    let mut current_inversions = flow_inversions(&seed, &flow_pairs);

    let mut best = seed.clone();
    let mut best_cost = current_cost;

    let mut rng = Rng::new(opts.seed);

    // ADR-19 M5′ (`--placer=m5-streams`, ADR-23 challenger): one private
    // RNG stream per movable element, keyed on its REFDES, swept in a
    // netlist-position-independent order. `Champion` builds neither and
    // takes the global-stream path below unchanged.
    //
    // Not recoverable from git — M5′ was measured in a working tree and
    // reverted without ever being committed (only its two docs commits
    // exist). This is a re-derivation from the ADR-19 description, and
    // the replay report says so; ADR-19's recorded numbers
    // (`common_emitter` V16 B 4→11 and friends) are what it is checked
    // against.
    let m5 = opts.placer.m5_element_streams();
    let mut m5_sweep: Vec<usize> = Vec::new();
    let mut m5_streams: std::collections::BTreeMap<usize, Rng> = std::collections::BTreeMap::new();
    if m5 {
        m5_sweep.clone_from(&movable);
        m5_sweep.sort_by(|&a, &b| {
            let (ra, rb) = (
                checked.elements.get(a).map(|e| e.refdes.as_str()),
                checked.elements.get(b).map(|e| e.refdes.as_str()),
            );
            ra.cmp(&rb).then(a.cmp(&b))
        });
        for &i in &m5_sweep {
            let key = checked
                .elements
                .get(i)
                .map_or(0, |e| refdes_stream_key(&e.refdes));
            m5_streams.insert(i, Rng::new(opts.seed ^ key));
        }
    }

    // Cooling: exponential, factor 1000 over the iteration count.
    // f64 widening only; iteration count fits comfortably.
    let total_iters = f64::from(opts.refine_iterations);
    let t0 = initial_temperature(&current_breakdown, &weights);
    let t_final = t0 / 1000.0;
    let alpha = (t_final / t0).powf(1.0 / total_iters.max(1.0));

    log::debug!(
        "spice-layout SA: {} movable / {} elements, T0={:.3}, alpha={:.5}, iters={}",
        movable.len(),
        n,
        t0,
        alpha,
        opts.refine_iterations
    );

    let mut temperature = t0;
    for it in 0..opts.refine_iterations {
        // M5' only: this iteration's swept element and its private
        // stream, lifted out of the map for the body and put back at the
        // end. `local` is `None` on every default path.
        let m5_elem = if m5 && !m5_sweep.is_empty() {
            Some(m5_sweep[(it as usize) % m5_sweep.len()])
        } else {
            None
        };
        let mut local = m5_elem.and_then(|i| m5_streams.remove(&i));

        let proposal = if let (Some(i), Some(r)) = (m5_elem, local.as_mut()) {
            propose_move_m5(&seed, i, &mirror_eligible, &layer_buckets, layers, r)
        } else {
            propose_move(
                &seed,
                &movable,
                &mirror_eligible,
                &layer_buckets,
                &swap_layers,
                &mut rng,
            )
        };

        // V14 hard gate: an orientation move (rotate / mirror-Y) whose
        // result leaves the element's allowed-orientation set is
        // infeasible. We still *apply, score and draw the Metropolis
        // value* exactly as a feasible move would, then force-reject —
        // this keeps the RNG stream byte-identical to the pre-V14
        // trajectory for every move HEAD would also have rejected, so
        // the V14 blast radius is confined to the elements the
        // constraint genuinely reorients (CLAUDE.md consistency rule:
        // hard at the seed chooser *and* every SA move).
        let v14_infeasible = proposal.reorients().is_some_and(|idx| {
            !orientation_allowed(reoriented(&seed.elements[idx], proposal), &allowed[idx])
        });

        let saved = apply_move(&mut seed, &proposal);

        let trial_breakdown = cost::breakdown_with(&seed, checked, library, opts.placer);
        let trial_cost = cost::total(&trial_breakdown, &weights);
        let delta = trial_cost - current_cost;
        // Cost-based Metropolis acceptance (RNG consumed exactly as
        // before), then the two placer-stage hard filters: V14
        // orientation and V11 foreign-pin coincidence. Either one
        // force-rejects a move cost would otherwise accept. The
        // coincidence recount runs only when the move is still alive
        // after V14 and cost, keeping the common path cheap.
        // Short-circuit preserved exactly: no Metropolis draw happens
        // when `delta <= 0.0`, so the champion stream is untouched.
        let cost_accept = delta <= 0.0 || {
            let u = match local.as_mut() {
                Some(r) => r.next_f64(),
                None => rng.next_f64(),
            };
            u < (-delta / temperature.max(1e-12)).exp()
        };
        let alive = cost_accept && !v14_infeasible;
        // The V11 foreign-pin-coincidence gate (Tier 0): two pins on
        // different nets at the same coordinate are electrically joined,
        // and no router pass can undo it. Applies to *every* move on
        // *every* fixture — see the note at `mirror_eligible` for why
        // the old mirror-only scoping was a defect, not an optimisation.
        let trial_coincidences = if alive {
            foreign_pin_coincidences(&seed, checked)
        } else {
            current_coincidences
        };
        let coincidence_ok = trial_coincidences <= current_coincidences;
        // The body-overlap gate is self-scoping (it counts only pairs
        // touching an oversized body, of which the passive fixtures have
        // none), so it is always safe to evaluate when the move is still
        // alive after cost + V14 + the coincidence filter.
        let trial_overlaps = if alive && coincidence_ok {
            symbol_overlap_count(&seed, checked, &glyph_prefs, opts.placer)
        } else {
            current_overlaps
        };
        let overlap_ok = trial_overlaps <= current_overlaps;
        // V5 pin-facing gate: a move that makes more signal pins face
        // away from their net is rejected even when it lowers HPWL,
        // because the immutable cost cannot see the resulting V5 routing
        // defect (a wire doubling back through the reoriented device's
        // body) — `cost.rs` has deliberately no orientation term.
        //
        // **Scope is a registered placer choice** (orientation-churn
        // stage 2, `--placer=flow-seed-v3`). On the default path the gate
        // binds on the mirror-Y move ONLY, so it never perturbs the
        // jitter/rotate/swap trajectory of any other move or fixture —
        // which also means `!is_mirror ||` short-circuits to `true` for
        // every **rotate**, leaving the SA free to stand a horizontal
        // series element on end whenever compaction pays for it. Under
        // stage 2 the same never-increase test binds on every reorienting
        // move (`Proposal::reorients`), so an improving rotation still
        // passes and a destructive one is refused. It stays a gate, never
        // a weight: adding a `pin_facing` term to `cost.rs` is the
        // Attempt-A failure CLAUDE.md records.
        let v5_gated = if opts.placer.sa_rotate_v5_gate() {
            proposal.reorients().is_some()
        } else {
            matches!(proposal, Proposal::MirrorY { .. })
        };
        let trial_misalignment = if alive && coincidence_ok && overlap_ok && v5_gated {
            pin_outward_misalignment(&seed, checked)
        } else {
            current_misalignment
        };
        let misalignment_ok = !v5_gated || trial_misalignment <= current_misalignment;
        // Signal-flow monotone gate. Evaluated last (it is the cheapest
        // recount — a scan of the precomputed pair list) and only while
        // the move is still alive, so it costs nothing on the moves the
        // earlier filters already killed.
        let trial_inversions = if alive && coincidence_ok && overlap_ok && misalignment_ok {
            flow_inversions(&seed, &flow_pairs)
        } else {
            current_inversions
        };
        let flow_ok = trial_inversions <= current_inversions;
        let accept = alive && coincidence_ok && overlap_ok && misalignment_ok && flow_ok;

        if accept {
            current_breakdown = trial_breakdown;
            current_cost = trial_cost;
            current_coincidences = trial_coincidences;
            current_overlaps = trial_overlaps;
            current_misalignment = trial_misalignment;
            current_inversions = trial_inversions;
            if current_cost < best_cost {
                best = seed.clone();
                best_cost = current_cost;
            }
        } else {
            revert_move(&mut seed, &saved);
        }

        if let (Some(i), Some(r)) = (m5_elem, local) {
            m5_streams.insert(i, r);
        }

        temperature *= alpha;

        if it % LOG_EVERY == 0 {
            log::debug!(
                "  it={it} T={temperature:.4} cost={current_cost:.3} \
                 hpwl={:.2} overlap={:.2} crossings={:.0} cv={:.3} \
                 rail={:.2} flow={:.2}",
                current_breakdown.hpwl,
                current_breakdown.overlap,
                current_breakdown.crossings,
                current_breakdown.constraint_violation,
                current_breakdown.rail_direction,
                current_breakdown.signal_flow,
            );
        }
    }

    log::debug!(
        "spice-layout SA done: best cost {:.3} (started {:.3})",
        best_cost,
        cost::total(
            &cost::breakdown_with(&best, checked, library, opts.placer),
            &CostWeights::DEFAULT,
        )
    );

    best
}

/// Pick a starting temperature that accepts ~50% of single-move
/// uphill steps on the seed. Heuristic: a small fraction of the
/// current weighted cost, with a floor so the SA does not get stuck
/// when the seed is already excellent.
fn initial_temperature(breakdown: &CostBreakdown, weights: &CostWeights) -> f64 {
    let c = cost::total(breakdown, weights);
    (c * 0.05).max(1.0)
}

/// Concrete proposal returned by `propose_move`. The annealer applies
/// it, evaluates cost, and either keeps or reverts via the matching
/// `Saved` snapshot.
#[derive(Debug, Clone, Copy)]
enum Proposal {
    /// Jitter element `idx` by `(dx, dy)` grid cells.
    Jitter { idx: usize, dx: i32, dy: i32 },
    /// Rotate element `idx` 90° CCW.
    Rotate { idx: usize },
    /// Toggle element `idx`'s mirror-Y flag (`Orientation::flip`).
    MirrorY { idx: usize },
    /// Swap the Y rank (origin.y) of two same-layer movable elements.
    SwapY { a: usize, b: usize },
}

impl Proposal {
    /// The element this proposal *reorients*, if any. `Some(idx)` for
    /// rotate / mirror-Y (the moves subject to the V14 gate); `None`
    /// for jitter / swap-Y (which never touch orientation).
    fn reorients(self) -> Option<usize> {
        match self {
            Proposal::Rotate { idx } | Proposal::MirrorY { idx } => Some(idx),
            Proposal::Jitter { .. } | Proposal::SwapY { .. } => None,
        }
    }
}

/// True when `orient` is in the element's V14 allowed-orientation set.
fn orientation_allowed(orient: Orientation, allowed: &[Orientation]) -> bool {
    allowed.contains(&orient)
}

/// Count distinct world coordinates at which two pins on *different*
/// nets coincide — the placer-side measure of the V11 short hazard.
///
/// Ground (`"0"`) pins are excluded: ground is carried by `power:GND`
/// glyphs (V10), not wires, so a ground pin sharing a coordinate with a
/// foreign pin is not the wire-merge short V11 guards against. Pins on
/// the *same* net legitimately coincide (that is connectivity) and do
/// not count. A coordinate hosting ≥ 2 distinct foreign nets counts
/// once, so the metric is a coordinate count, not a pair count — enough
/// for the monotone "never get worse" SA filter.
pub(crate) fn foreign_pin_coincidences(placement: &Placement, checked: &CheckedNetlist) -> usize {
    use std::collections::HashMap;

    // coord (in integer micrometres, grid-exact) → set of net names.
    let mut at: HashMap<(i64, i64), std::collections::BTreeSet<&str>> = HashMap::new();
    for (el, placed) in checked.elements.iter().zip(&placement.elements) {
        let pins = el.symbol.pins_in(placed.orientation);
        let (ox, oy) = placed.origin.to_mm();
        for (term_idx, node) in el.nodes.iter().enumerate() {
            if node == "0" {
                continue; // ground travels by glyph, not wire
            }
            let Some(kpin) = el.pin_mapping.get(term_idx) else {
                continue;
            };
            let Some(p) = pins.iter().find(|p| &p.number == kpin) else {
                continue;
            };
            // Emitter convention: world Y = origin_y - pin_y.
            let wx = ox + p.x;
            let wy = oy - p.y;
            #[allow(clippy::cast_possible_truncation)]
            let key = ((wx * 1000.0).round() as i64, (wy * 1000.0).round() as i64);
            at.entry(key).or_default().insert(node.as_str());
        }
    }
    at.values().filter(|nets| nets.len() >= 2).count()
}

/// World-frame *body* half-extents (`half_w`, `half_h`, mm) of an
/// element's graphical body in a given orientation. Used only to
/// decide whether a part is "oversized vs the cost cell" — the gate's
/// activation key. Body-only (excludes pin stems) so the activation
/// set stays exactly as before: only a genuinely large body (an opamp
/// triangle, a BJT circle — see `symbol_overlap_count`) trips the
/// gate, leaving every SA trajectory without one byte-identical.
fn body_half_extents(el: &spice_resolve::ResolvedElement, orient: Orientation) -> (f64, f64) {
    let mut hw = 0.0_f64;
    let mut hh = 0.0_f64;
    if let Some(b) = el.symbol.body_bbox() {
        for (lx, ly) in [(b.x0, b.y0), (b.x0, b.y1), (b.x1, b.y0), (b.x1, b.y1)] {
            let (rx, ry) = orient.apply_point(lx, ly);
            hw = hw.max(rx.abs());
            hh = hh.max(ry.abs());
        }
    }
    (hw, hh)
}

/// World-frame *footprint* half-extents (`half_w`, `half_h`, mm): the
/// orientation-transformed body bbox unioned with the reach of every
/// pin stem **and the power-glyph reach of every rail pin**. Matches the
/// `no_symbol_symbol_overlap` verifier so that, once the gate is active
/// for a pair, it forbids exactly the overlaps that verifier flags —
/// including a body that merely kisses a neighbour but whose pin stem
/// then spears it.
///
/// The glyph-reach union is the SA half of the ADR-14 reservation: it
/// mirrors the same `glyph_geom::glyph_reach` delta the seed/align
/// stride reserves (`world_extent_with_glyphs`). Scope, precisely: the
/// reservation is hard **only for pairs involving an oversized body** —
/// `symbol_overlap_count` passes `prefs` only for *non-oversized*
/// consumers and skips small×small pairs entirely, so the SA can still
/// slide a small foreign body into a small host's glyph zone. Likewise
/// the phase-2 seed floor consumes only the X axis outside the align
/// path (`place_seed` reads `max_x`/`min_x`; the vertical hard floor
/// exists only in `vertical_stride_cells` on the align path). Those
/// gaps are guarded downstream by the zero-slack output ratchet
/// (`no_power_glyph_foreign_body_overlap_across_fixtures`), which
/// measures emitted geometry and trips on any drift — see ADR-14
/// "Known scope limits". The gate's half-extent model is symmetric
/// about the origin, so a glyph reach point `(dx, dy)` extends the
/// half-extent by `|dx|`/`|dy|` on BOTH sides: a reserved zone below
/// the part also blocks space above it — a strict-but-conservative
/// halo (ADR-14's Risks flagged this shape; acceptable now, revisit if
/// a glyph-dense fixture hits V15). This is extra outward spacing
/// only; it changes no orientation (V5) and no glyph pose (V14).
pub(crate) fn footprint_half_extents(
    el: &spice_resolve::ResolvedElement,
    orient: Orientation,
    prefs: Option<&std::collections::HashMap<String, crate::net_class::VertPref>>,
) -> (f64, f64) {
    let (mut hw, mut hh) = body_half_extents(el, orient);
    for p in el.symbol.pins_in(orient) {
        hw = hw.max(p.x.abs());
        hh = hh.max(p.y.abs());
    }
    if let Some(prefs) = prefs {
        for (dx, dy) in crate::glyph_geom::glyph_reach(el, orient, prefs) {
            hw = hw.max(dx.abs());
            hh = hh.max(dy.abs());
        }
    }
    (hw, hh)
}

/// Count unordered element pairs whose real body bounding boxes
/// strictly overlap in world space — the placer-side measure of the
/// symbol-symbol collision the `no_symbol_symbol_overlap` verifier
/// flags (CLAUDE.md V6).
///
/// Two bodies overlap when their centre separation is below the summed
/// half-extents on *both* axes (an axis-aligned-bbox intersection),
/// with a 1 µm tolerance so bodies that merely kiss on the grid do not
/// count — the same shape of test as the verifier's `Bbox::intersects`,
/// but against each symbol's *actual* bbox rather than a fixed square.
///
/// Enforced as a "never increase" SA filter (not a cost term, per
/// CLAUDE.md): "no two symbol footprints overlap" is categorical, and
/// `no_symbol_symbol_overlap_across_fixtures` asserts it with no budget.
///
/// **Covers every pair.** It used to skip small×small pairs on the
/// premise that the `overlap` cost "already keeps every body within a
/// `CELL_W × CELL_H` footprint apart" — true only while that cost
/// measured a uniform cell, which over-estimated small parts and so held
/// the categorical property up by accident. With the cost now measuring
/// real footprints that premise is gone: `common_emitter`'s R1×RC pair
/// overlapped by 0.76 × 1.27 mm with the soft term seeing it and trading
/// it away for HPWL. A soft term at a safe weight cannot hold a
/// categorical property; the filter must.
///
/// The `oversized` key survives only to scope the glyph-reach
/// reservation via `prefs_for` — do not re-narrow the pair scope with it.
/// Historical note on that key — its real half-extent
/// exceeds the cost's cell half-extent on the colliding axis. TWO
/// fixture symbols are oversized, not one: the opamp triangle (~5 mm
/// half-extent vs the cell's 3.81 mm) and the BJT `Device:Q_NPN_BCE`
/// (body half-extent ~4.09 mm — pinned by
/// `kicad-symbols/tests/body_bbox.rs::body_bbox_q_npn_bce_covers_circle`).
/// Once V14 pins the opamp at rot 0 its wide body would let a neighbour
/// slide under it cost-free, which this gate forbids. The BJT tripping
/// the oversized key is LOAD-BEARING for the ADR-14 glyph-zone defense:
/// `common_emitter`'s [3] fix depends on Q1 activating the gate for the
/// R2×Q1 pair, so that R2's reserved ground-glyph zone repels Q1 —
/// narrow the activation and that fix silently evaporates (the output
/// ratchet would catch it, but only after the fact). Keying off
/// "oversized vs the cost cell" makes the gate a genuine no-op for
/// every fixture whose symbols are all small: their overlaps are
/// entirely handled by the cost, so the gate's count stays 0 and the SA
/// trajectory is unchanged.
///
/// # ADR-19 M3 — why this gate still reads the halo
///
/// M3 wired [`crate::footprint`]'s signed AABB in here and **measured a
/// Tier-1 regression**; the wiring is reverted, the measurement is
/// recorded in `docs/layout-adr.md` § "M3 blocked", and the branch
/// `wip/adr19-m3-signed-gate` holds the code. In one line: making this
/// reservation honest *frees* space (the signed box is a strict subset
/// of the halo on body ∪ pins ∪ glyph), and the freed space is taken by
/// the one decoration class the placer still does **not** reserve —
/// routed net labels. `named_rails` V13 item(3) 0 → 1. The relaxation is
/// only safe once `label_geom` lands (ADR-19 M6); until then the halo's
/// over-reservation is load-bearing slack, not merely conservative.
///
/// Do not re-wire it piecemeal. The three ablations are already
/// measured — see the ADR table. They are, however, *registered* as
/// graded challengers (`--placer=m3-signed-gate` / `m3-signed-full`,
/// ADR-23); `variant` selects them and is `Champion` on every default
/// path, where this function is byte-for-byte the halo it always was.
#[allow(clippy::similar_names)] // ahw/ahh, bhw/bhh: half-extent pairs.
fn symbol_overlap_count(
    placement: &Placement,
    checked: &CheckedNetlist,
    // Rail-pin screen-vertical prefs, for the glyph-reach reservation the
    // footprint measure unions in (ADR-14 phase 3, mirroring the seed).
    // Computed once in `refine` (invariant across the anneal) and
    // threaded in, so the SA hot loop never re-runs `classify_nets`.
    prefs: &std::collections::HashMap<String, crate::net_class::VertPref>,
    // ADR-23 seam: which registered placer is running. `Champion` takes
    // the halo path below, unchanged.
    variant: crate::Placer,
) -> usize {
    if variant.m3_signed_gate() {
        return symbol_overlap_count_m3(placement, checked, prefs, variant);
    }
    // The cell half-extents the `overlap` cost already enforces. A body
    // within these contributes nothing here (the cost covers it).
    let cell_hw = f64::from(crate::CELL_W) * GridPoint::STEP_MM / 2.0;
    let cell_hh = f64::from(crate::CELL_H) * GridPoint::STEP_MM / 2.0;

    let extents: Vec<(f64, f64, f64, f64, bool)> = checked
        .elements
        .iter()
        .zip(&placement.elements)
        .map(|(el, placed)| {
            // Activation key: body-only (unchanged set — only a large
            // body trips the gate).
            let (bhw, bhh) = body_half_extents(el, placed.orientation);
            let oversized = bhw > cell_hw + 1e-6 || bhh > cell_hh + 1e-6;
            // Overlap measure: full footprint (body ∪ pin reach ∪
            // rail-pin glyph reach) so a pin stub or a reserved glyph
            // zone spearing a neighbour is caught.
            //
            // The glyph-reach reservation is scoped to *non-oversized*
            // rail-pin elements — the rail *consumers* (e.g. a 2-pin
            // resistor whose ground glyph a neighbouring large body would
            // clip, ADR-14's `common_emitter` [3]). A large body's own
            // crowded rail-pin zones (an opamp triangle carrying VCC /
            // VEE / GND pins) are deliberately NOT self-reserved here:
            // reserving them over-constrains the SA and reshuffles the
            // part into a worse layout (an opamp `RIN`/glyph value-text
            // V13 overlap — a within-Tier-1 sideways trade the ratchet
            // rule forbids). Such a body is already the gate's *neighbour*
            // that the consumer's reservation repels, so the foreign-body
            // overlap is still removed from the consumer side. (The
            // remaining opamp `#FLG4`/PWR_FLAG residual is a distinct
            // sheet-port-flavoured defect, scoped out per ADR-14.)
            let prefs_for = if oversized { None } else { Some(prefs) };
            let (fhw, fhh) = footprint_half_extents(el, placed.orientation, prefs_for);
            let (ox, oy) = placed.origin.to_mm();
            (ox, oy, fhw, fhh, oversized)
        })
        .collect();
    let eps = 1e-3;
    let mut count = 0;
    for a in 0..extents.len() {
        for b in (a + 1)..extents.len() {
            let (ax, ay, ahw, ahh, _a_big) = extents[a];
            let (bx, by, bhw, bhh, _b_big) = extents[b];
            if (ax - bx).abs() + eps < ahw + bhw && (ay - by).abs() + eps < ahh + bhh {
                count += 1;
            }
        }
    }
    count
}

/// ADR-19 M3's rejected wiring, recovered verbatim from
/// `wip/adr19-m3-signed-gate` (`7896f22`) and reachable only through
/// `--placer=m3-signed-gate` / `m3-signed-full` (ADR-23 challengers).
///
/// Same activation key and same glyph scoping as the halo version; the
/// only difference is that the reservation is the **signed**
/// `footprint` AABB — a strict subset of the halo on the classes both
/// model — so the pair test is a real rectangle intersection instead of
/// a centre-separation test. `m3-signed-full` additionally unions the
/// drawn property text; that single `if` is the B → full ablation.
///
/// NOT for the default path: see `docs/layout-adr.md` § "M3 blocked"
/// and ADR-23's replay table.
#[allow(clippy::similar_names)] // bhw/bhh, ax0/ay0: half-extent and corner pairs.
fn symbol_overlap_count_m3(
    placement: &Placement,
    checked: &CheckedNetlist,
    prefs: &std::collections::HashMap<String, crate::net_class::VertPref>,
    variant: crate::Placer,
) -> usize {
    let cell_hw = f64::from(crate::CELL_W) * GridPoint::STEP_MM / 2.0;
    let cell_hh = f64::from(crate::CELL_H) * GridPoint::STEP_MM / 2.0;

    // World-frame `(x0, x1, y0, y1)` reservation box per element.
    let boxes: Vec<(f64, f64, f64, f64)> = checked
        .elements
        .iter()
        .zip(&placement.elements)
        .map(|(el, placed)| {
            let (bhw, bhh) = body_half_extents(el, placed.orientation);
            let oversized = bhw > cell_hw + 1e-6 || bhh > cell_hh + 1e-6;
            let mut fp = crate::footprint::body_and_pins(&el.symbol, placed.orientation);
            if !oversized {
                fp = fp.union(&crate::footprint::glyph(el, placed.orientation, prefs));
            }
            if variant.m3_property_text() && !placed.is_power_source {
                fp = fp.union(&crate::footprint::property_text(
                    &el.refdes,
                    Some(crate::footprint::drawn_value(
                        &el.refdes,
                        placed.value.as_deref(),
                    )),
                    placed.orientation,
                ));
            }
            let (ox, oy) = placed.origin.to_mm();
            (ox + fp.min_x, ox + fp.max_x, oy + fp.min_y, oy + fp.max_y)
        })
        .collect();
    let eps = 1e-3;
    let mut count = 0;
    for a in 0..boxes.len() {
        for b in (a + 1)..boxes.len() {
            let (ax0, ax1, ay0, ay1) = boxes[a];
            let (bx0, bx1, by0, by1) = boxes[b];
            if ax0 + eps < bx1 && bx0 + eps < ax1 && ay0 + eps < by1 && by0 + eps < ay1 {
                count += 1;
            }
        }
    }
    count
}

/// Count *signal* pins that must route *across their own host body* to
/// reach their net — the placer-side measure of the V5 / V12 routing
/// defect a harmful mirror-Y flip introduces.
///
/// A pin sits on one side of its host symbol's body; the clean way out
/// is *away* from the body (the side the pin is on). If the rest of the
/// pin's net lies on the **opposite** side of the body, the router has
/// to carry the wire back across the body — abandoning the outward-clean
/// first segment (V5) and spearing the body (V12). This is exactly what
/// flipping the inverting-amp opamp does: its output pin lands on the
/// left edge while its only neighbour (the feedback resistor) is on the
/// right, so the output wire must cross the opamp triangle.
///
/// For each multi-element signal pin we compare two directions in the
/// horizontal and vertical axes independently: the body-clear direction
/// (body centroid → pin) and the direction to the net centroid. When
/// they oppose on the pin's *dominant* body-exit axis, the pin is
/// counted. Used only as a "never increase" gate on the mirror-Y move
/// (not a cost term): a flip that raises this count is rejected even
/// when it lowers HPWL, since the immutable cost cannot see the
/// across-body route. Ground (`"0"`) pins are excluded (carried by
/// glyphs, not wires).
#[allow(clippy::similar_names, clippy::cast_precision_loss)] // bcx/bcy centroid; pin counts are tiny.
fn pin_outward_misalignment(placement: &Placement, checked: &CheckedNetlist) -> usize {
    use std::collections::HashMap;

    let mut net_pts: HashMap<&str, Vec<(f64, f64)>> = HashMap::new();
    for (el, placed) in checked.elements.iter().zip(&placement.elements) {
        let pins = el.symbol.pins_in(placed.orientation);
        let (ox, oy) = placed.origin.to_mm();
        for (term_idx, node) in el.nodes.iter().enumerate() {
            if node == "0" {
                continue;
            }
            let Some(kpin) = el.pin_mapping.get(term_idx) else {
                continue;
            };
            let Some(p) = pins.iter().find(|p| &p.number == kpin) else {
                continue;
            };
            net_pts
                .entry(node.as_str())
                .or_default()
                .push((ox + p.x, oy - p.y));
        }
    }

    let mut misaligned = 0;
    for (el, placed) in checked.elements.iter().zip(&placement.elements) {
        let pins = el.symbol.pins_in(placed.orientation);
        let (ox, oy) = placed.origin.to_mm();
        // Body centroid in world coords. Skip bodyless symbols.
        let Some(bbox) = el.symbol.body_bbox() else {
            continue;
        };
        let (bcx, bcy) = placed.orientation.apply_point(
            f64::midpoint(bbox.x0, bbox.x1),
            f64::midpoint(bbox.y0, bbox.y1),
        );
        let (bcx, bcy) = (ox + bcx, oy - bcy);
        for (term_idx, node) in el.nodes.iter().enumerate() {
            if node == "0" {
                continue;
            }
            let Some(kpin) = el.pin_mapping.get(term_idx) else {
                continue;
            };
            let Some(p) = pins.iter().find(|p| &p.number == kpin) else {
                continue;
            };
            let Some(pts) = net_pts.get(node.as_str()) else {
                continue;
            };
            if pts.len() < 2 {
                continue; // single-pin net: no facing preference
            }
            let (px, py) = (ox + p.x, oy - p.y);
            let cx = (pts.iter().map(|q| q.0).sum::<f64>() - px) / (pts.len() as f64 - 1.0);
            let cy = (pts.iter().map(|q| q.1).sum::<f64>() - py) / (pts.len() as f64 - 1.0);
            // Body-clear direction (centroid → pin) and direction to net.
            let (clear_x, clear_y) = (px - bcx, py - bcy);
            let (net_x, net_y) = (cx - px, cy - py);
            // Pick the pin's dominant body-exit axis (the axis on which it
            // sits furthest from the body centre — the side it exits).
            // The pin is across-body when the net lies on the opposite
            // side of that axis.
            let across = if clear_x.abs() >= clear_y.abs() {
                clear_x * net_x < -1e-9
            } else {
                clear_y * net_y < -1e-9
            };
            if across {
                misaligned += 1;
            }
        }
    }
    misaligned
}

/// The ordered element pairs the layer assignment says must run
/// left→right: `(u, v)` sharing at least one Signal-class net with
/// `layer(u) < layer(v)`. Pure function of the netlist and the layer
/// assignment, so it is computed once before the anneal and the
/// per-proposal gate is a cheap X comparison over this list.
///
/// **Rail stubs are excluded** (ADR-15's role model): a two-terminal
/// element with exactly one rail pin does not pass a signal along, it
/// *terminates* a node, and convention draws it as a vertical drop in
/// that node's column — which is what `idioms::apply_rail_stub_columns`
/// places it as. Its X is therefore owned by the column, not by the
/// flow order, and holding it to a left→right ordering would fight the
/// idiom for no readability gain (a collector load sits directly ABOVE
/// its transistor, not to the right of it).
fn flow_pairs(checked: &CheckedNetlist, layers: &LayerAssignment) -> Vec<(usize, usize)> {
    use std::collections::{BTreeMap, BTreeSet};

    let classes = crate::net_class::classify_nets(checked);
    if layers.layers.len() != checked.elements.len() {
        return Vec::new();
    }
    let stubs: BTreeSet<usize> = crate::idioms::detect_rail_stubs(checked)
        .into_iter()
        .map(|s| s.element)
        .collect();
    let mut net_to_elements: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, el) in checked.elements.iter().enumerate() {
        if stubs.contains(&i) {
            continue;
        }
        for net in &el.nodes {
            if classes
                .get(net.as_str())
                .copied()
                .unwrap_or(NetClass::Signal)
                == NetClass::Signal
            {
                net_to_elements.entry(net.as_str()).or_default().push(i);
            }
        }
    }
    let mut pairs: BTreeSet<(usize, usize)> = BTreeSet::new();
    for members in net_to_elements.values() {
        for &u in members {
            for &v in members {
                if u != v && layers.layers[u] < layers.layers[v] {
                    pairs.insert((u, v));
                }
            }
        }
    }
    pairs.into_iter().collect()
}

/// Count the flow pairs whose emitted X order is REVERSED — the placer-
/// side measure of the F3 "signal flows left→right" property.
///
/// This is a **hard monotone gate** at the mover, never a cost term.
/// `cost.rs` already carries a soft `layer_order` weight, and ADR-15
/// found it simply outvoted: on `rc_lowpass_ports` it measured ~10² while
/// HPWL and the crossing terms pulled the other way, so left→right flow
/// did not survive refinement. Per CLAUDE.md's constraints-vs-costs rule
/// a categorical yes/no property belongs at the candidate boundary, so a
/// move that raises this count is force-rejected exactly like the V11
/// coincidence and V6 overlap gates — the soft term stays as the
/// tie-breaking gradient that pulls the count *down*.
fn flow_inversions(placement: &Placement, pairs: &[(usize, usize)]) -> usize {
    pairs
        .iter()
        .filter(|&&(u, v)| {
            let (xu, _) = placement.elements[u].origin.to_mm();
            let (xv, _) = placement.elements[v].origin.to_mm();
            xu > xv
        })
        .count()
}

/// The orientation `el` *would* take under a reorienting proposal,
/// without mutating anything. Non-reorienting proposals return the
/// current orientation unchanged (the caller only consults this for
/// rotate / mirror-Y).
fn reoriented(el: &PlacedElement, proposal: Proposal) -> Orientation {
    match proposal {
        Proposal::Rotate { .. } => rotated_ccw(el.orientation),
        Proposal::MirrorY { .. } => el.orientation.flip(),
        Proposal::Jitter { .. } | Proposal::SwapY { .. } => el.orientation,
    }
}

/// The orientation 90° CCW from `o`, preserving the mirror-Y flag.
/// Shared by [`reoriented`] (pre-apply check) and [`rotate_once`]
/// (in-place apply) so the two never disagree.
fn rotated_ccw(o: Orientation) -> Orientation {
    Orientation {
        rotation: match o.rotation {
            Rotation::R0 => Rotation::R90,
            Rotation::R90 => Rotation::R180,
            Rotation::R180 => Rotation::R270,
            Rotation::R270 => Rotation::R0,
        },
        mirror_y: o.mirror_y,
    }
}

/// Snapshot of just enough state to revert a proposal that was rejected.
#[derive(Debug, Clone, Copy)]
enum Saved {
    Pose {
        idx: usize,
        origin: GridPoint,
        orientation: Orientation,
    },
    SwapY {
        a: usize,
        a_y: i32,
        b: usize,
        b_y: i32,
    },
}

/// Pick the next move. Distribution (per-call):
///
/// * 0.2 same-layer Y-rank swap (bucket < 2, when at least one layer
///   has two or more movable elements; otherwise the swap weight
///   collapses into jitter),
/// * 0.1 *orientation* move (bucket == 2), split by a secondary draw
///   into rotate (3/4) and mirror-Y (1/4) — so mirror-Y is ~0.025
///   overall, rarer than rotate because a flip is the most visually
///   disruptive single move,
/// * 0.7 jitter (remaining buckets). The bulk of SA work is local
///   position search.
///
/// The bucketing of the *primary* draw is byte-identical to the
/// pre-mirror distribution: jitter and swap consume the same RNG
/// values they always did, so adding mirror-Y only perturbs the
/// already-rare orientation slot. Rotate and mirror-Y are gated
/// against the V14 allowed-orientation set by the caller; an out-of-set
/// result is dropped before being applied.
///
/// `mirror_eligible` is the subset of `movable` whose V14
/// allowed-orientation set is *restricted* (`< 8` orientations) — i.e.
/// rail-bearing elements where a flip is a V14/symmetry-meaningful
/// move. Mirror-Y is proposed *only* for those: flipping a signal-only
/// part is a free aesthetic move the immutable cost function cannot
/// score safely, and on the reference fixtures it trades a tiny HPWL
/// gain for V11/V12 defects (a foreign-net short or a wire spearing a
/// body). Confining mirror-Y to rail-constrained elements keeps it in
/// the search space (ADR-3) without that Tier-0/1 hazard. When no
/// element is mirror-eligible the mirror slot degrades to a rotate.
fn propose_move(
    placement: &Placement,
    movable: &[usize],
    mirror_eligible: &[usize],
    layer_buckets: &std::collections::BTreeMap<u32, Vec<usize>>,
    swap_layers: &[u32],
    rng: &mut Rng,
) -> Proposal {
    let bucket = rng.next_below(10);
    let want_swap = !swap_layers.is_empty() && bucket < 2; // 0.2
    let want_orient = bucket == 2; // 0.1

    if want_swap {
        let layer = swap_layers[rng.next_below(swap_layers.len())];
        let elems = &layer_buckets[&layer];
        let i = rng.next_below(elems.len());
        let mut j = rng.next_below(elems.len());
        while j == i {
            j = rng.next_below(elems.len());
        }
        let (a, b) = (elems[i], elems[j]);
        // Skip degenerate swaps (both already at the same Y) — fall
        // through to a jitter so the iteration is not wasted.
        if placement.elements[a].origin.y != placement.elements[b].origin.y {
            return Proposal::SwapY { a, b };
        }
    }

    let idx = movable[rng.next_below(movable.len())];
    if want_orient {
        // Secondary draw: mirror-Y 1/4 of the time (on a separately
        // chosen mirror-eligible element), else rotate the primary
        // `idx`. The eligibility check is evaluated *first* and short-
        // circuits before any RNG is drawn, so when no element is
        // mirror-eligible (every fixture without a V14-reoriented active
        // device) the RNG stream stays byte-identical to the pre-mirror
        // trajectory — the orientation slot is a plain rotate exactly as
        // before. Mirror-Y is confined to V14-restricted elements (see
        // the caller), so a flip stays inside the V14-feasible poses.
        if !mirror_eligible.is_empty() && rng.next_below(4) == 0 {
            let m = mirror_eligible[rng.next_below(mirror_eligible.len())];
            Proposal::MirrorY { idx: m }
        } else {
            Proposal::Rotate { idx }
        }
    } else {
        let (dx, dy) = jitter_delta(rng);
        Proposal::Jitter { idx, dx, dy }
    }
}

/// FNV-1a over a refdes — a netlist-*position*-independent stream key.
///
/// The point of M5′ is that adding `R9` to a file must not renumber the
/// draws `R1` sees, so the key is the name, never the index.
fn refdes_stream_key(refdes: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in refdes.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// ADR-19 M5′ — one proposal for a *given* element, drawn from that
/// element's private stream.
///
/// Same move mix and same secondary draws as [`propose_move`]; the only
/// difference is that the element is chosen by the deterministic sweep
/// in `refine`, not by a draw from the global stream. Reachable only
/// through `--placer=m5-streams`.
fn propose_move_m5(
    placement: &Placement,
    idx: usize,
    mirror_eligible: &[usize],
    layer_buckets: &std::collections::BTreeMap<u32, Vec<usize>>,
    layers: &LayerAssignment,
    rng: &mut Rng,
) -> Proposal {
    let bucket = rng.next_below(10);
    let want_orient = bucket == 2; // 0.1, as in `propose_move`

    if bucket < 2 {
        // Swap-Y with a peer in this element's own layer bucket.
        if let Some(elems) = layers.layers.get(idx).and_then(|l| layer_buckets.get(l))
            && elems.len() >= 2
        {
            let mut j = rng.next_below(elems.len());
            while elems[j] == idx {
                j = rng.next_below(elems.len());
            }
            let b = elems[j];
            if placement.elements[idx].origin.y != placement.elements[b].origin.y {
                return Proposal::SwapY { a: idx, b };
            }
        }
    }

    if want_orient {
        if mirror_eligible.contains(&idx) && rng.next_below(4) == 0 {
            Proposal::MirrorY { idx }
        } else {
            Proposal::Rotate { idx }
        }
    } else {
        let (dx, dy) = jitter_delta(rng);
        Proposal::Jitter { idx, dx, dy }
    }
}

fn apply_move(seed: &mut Placement, p: &Proposal) -> Saved {
    match *p {
        Proposal::Jitter { idx, dx, dy } => {
            let el = &mut seed.elements[idx];
            let saved = Saved::Pose {
                idx,
                origin: el.origin,
                orientation: el.orientation,
            };
            el.origin = GridPoint::new(el.origin.x + dx, el.origin.y + dy);
            saved
        }
        Proposal::Rotate { idx } => {
            let el = &mut seed.elements[idx];
            let saved = Saved::Pose {
                idx,
                origin: el.origin,
                orientation: el.orientation,
            };
            rotate_once(el);
            saved
        }
        Proposal::MirrorY { idx } => {
            let el = &mut seed.elements[idx];
            let saved = Saved::Pose {
                idx,
                origin: el.origin,
                orientation: el.orientation,
            };
            el.orientation = el.orientation.flip();
            saved
        }
        Proposal::SwapY { a, b } => {
            let a_y = seed.elements[a].origin.y;
            let b_y = seed.elements[b].origin.y;
            seed.elements[a].origin = GridPoint::new(seed.elements[a].origin.x, b_y);
            seed.elements[b].origin = GridPoint::new(seed.elements[b].origin.x, a_y);
            Saved::SwapY { a, a_y, b, b_y }
        }
    }
}

fn revert_move(seed: &mut Placement, saved: &Saved) {
    match *saved {
        Saved::Pose {
            idx,
            origin,
            orientation,
        } => {
            seed.elements[idx].origin = origin;
            seed.elements[idx].orientation = orientation;
        }
        Saved::SwapY { a, a_y, b, b_y } => {
            seed.elements[a].origin = GridPoint::new(seed.elements[a].origin.x, a_y);
            seed.elements[b].origin = GridPoint::new(seed.elements[b].origin.x, b_y);
        }
    }
}

fn jitter_delta(rng: &mut Rng) -> (i32, i32) {
    // Offset uniform in {-2, -1, 0, 1, 2} per axis, excluding (0, 0).
    loop {
        // i32 widening from u8.
        #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
        let dx = (rng.next_below(5) as i32) - 2;
        #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
        let dy = (rng.next_below(5) as i32) - 2;
        if dx == 0 && dy == 0 {
            continue;
        }
        return (dx, dy);
    }
}

fn rotate_once(el: &mut PlacedElement) {
    // Rotate 90° CCW; preserve mirror-y.
    el.orientation = rotated_ccw(el.orientation);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PlacedElement;
    use kicad_symbols::Orientation;

    fn placed(refdes: &str, x: i32, y: i32) -> PlacedElement {
        PlacedElement {
            refdes: refdes.to_string(),
            lib_id: "Device:R".to_string(),
            origin: GridPoint::new(x, y),
            orientation: Orientation::IDENTITY,
            nodes: Vec::new(),
            pin_mapping: Vec::new(),
            value: None,
            is_power_source: false,
            power_rail: None,
        }
    }

    /// `flow_inversions` counts exactly the ordered pairs whose X order
    /// is reversed — the quantity the SA gate holds monotone.
    #[test]
    fn flow_inversions_counts_reversed_pairs_only() {
        let placement = Placement {
            elements: vec![placed("R1", 0, 0), placed("R2", 10, 0), placed("R3", 5, 0)],
        };
        // R1 → R2 is in order; R2 → R3 is reversed; R1 → R3 is in order.
        assert_eq!(flow_inversions(&placement, &[(0, 1)]), 0);
        assert_eq!(flow_inversions(&placement, &[(1, 2)]), 1);
        assert_eq!(flow_inversions(&placement, &[(0, 1), (1, 2), (0, 2)]), 1);
    }

    /// A rail stub terminates a node in that node's column, so it must
    /// not appear in the flow-ordering pair set (ADR-15's role model);
    /// a series element on the signal path must.
    #[test]
    fn flow_pairs_exclude_rail_stubs() {
        use crate::layers::assign_x_layers;
        use crate::net_class::classify_nets;
        use kicad_symbols::Library;
        use spice_diagnostics::FileId;
        use spice_policy::check;

        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixture_dir = manifest
            .parent()
            .and_then(std::path::Path::parent)
            .expect("workspace root")
            .join("crates/kicad-symbols/tests/fixtures");
        let library = Library::from_file(fixture_dir.join("Device.kicad_sym"))
            .expect("load Device fixture library")
            .merge(
                Library::from_file(fixture_dir.join("Simulation_SPICE.kicad_sym"))
                    .expect("load Simulation_SPICE fixture library"),
            );
        // R1 is a series element (`in` → `mid`, both Signal); C1 is a
        // rail stub (`mid` → ground); R2 is a second series element.
        let src = "test\nV1 in 0 AC 1\nR1 in mid 1k\nR2 mid out 1k\nC1 mid 0 1u\n.end\n";
        let parsed = spice_parser::parse(src, FileId(0))
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, &library).expect("resolve failed");
        let (checked, _warns) = check(resolved).expect("policy check failed");
        let classes = classify_nets(&checked);
        let layers = assign_x_layers(&checked, &classes);
        let idx = |refdes: &str| {
            checked
                .elements
                .iter()
                .position(|e| e.refdes == refdes)
                .expect("element present")
        };
        let pairs = flow_pairs(&checked, &layers);
        let c1 = idx("C1");
        assert!(
            pairs.iter().all(|&(u, v)| u != c1 && v != c1),
            "rail stub C1 must not constrain the flow order: {pairs:?}"
        );
        assert!(
            pairs.contains(&(idx("R1"), idx("R2"))),
            "series pair R1 → R2 must constrain the flow order: {pairs:?}"
        );
    }

    /// Two same-layer elements should be eligible for a Y-rank swap;
    /// after applying the proposal their Y coordinates are exchanged.
    #[test]
    fn swap_y_rank_move_swaps_origins() {
        let mut placement = Placement {
            elements: vec![placed("R1", 0, 5), placed("R2", 10, 12)],
        };
        // Both elements are movable and on the same layer.
        let movable = vec![0, 1];
        let mut buckets: std::collections::BTreeMap<u32, Vec<usize>> =
            std::collections::BTreeMap::new();
        buckets.insert(0, vec![0, 1]);
        let swap_layers = vec![0_u32];
        let mut rng = Rng::new(0xDEAD_BEEF);

        // Loop until propose_move returns a SwapY (0.2 probability per
        // call — capped iteration count keeps the test bounded).
        let mut maybe_swap: Option<Proposal> = None;
        for _ in 0..1000 {
            let p = propose_move(&placement, &movable, &[], &buckets, &swap_layers, &mut rng);
            if matches!(p, Proposal::SwapY { .. }) {
                maybe_swap = Some(p);
                break;
            }
        }
        let proposal = maybe_swap.expect("propose_move never produced SwapY in 1000 tries");

        let first_y = placement.elements[0].origin.y;
        let second_y = placement.elements[1].origin.y;
        let saved = apply_move(&mut placement, &proposal);
        assert_eq!(placement.elements[0].origin.y, second_y);
        assert_eq!(placement.elements[1].origin.y, first_y);

        // X stays put — only Y rank swaps.
        assert_eq!(placement.elements[0].origin.x, 0);
        assert_eq!(placement.elements[1].origin.x, 10);

        // Revert restores the original Y rank.
        revert_move(&mut placement, &saved);
        assert_eq!(placement.elements[0].origin.y, first_y);
        assert_eq!(placement.elements[1].origin.y, second_y);
    }

    /// Mirror-Y is proposed only for a mirror-eligible element, and it
    /// targets that element (never a non-eligible one). With element 1
    /// the sole eligible index, every MirrorY proposal must carry
    /// `idx == 1`.
    #[test]
    fn mirror_y_targets_only_eligible_elements() {
        let placement = Placement {
            elements: vec![placed("R1", 0, 5), placed("R2", 10, 12)],
        };
        let movable = vec![0, 1];
        let mirror_eligible = vec![1_usize];
        let buckets: std::collections::BTreeMap<u32, Vec<usize>> =
            std::collections::BTreeMap::new();
        let swap_layers: Vec<u32> = vec![];
        let mut rng = Rng::new(0x1234_5678);

        let mut saw_mirror = false;
        for _ in 0..5000 {
            let p = propose_move(
                &placement,
                &movable,
                &mirror_eligible,
                &buckets,
                &swap_layers,
                &mut rng,
            );
            if let Proposal::MirrorY { idx } = p {
                assert_eq!(idx, 1, "mirror-Y must target the eligible element");
                saw_mirror = true;
            }
        }
        assert!(saw_mirror, "mirror-Y never proposed in 5000 tries");
    }

    /// A non-reorienting move (jitter / swap) leaves orientation
    /// unchanged; rotate / mirror compute their target orientation via
    /// the shared `reoriented` helper that the V14 gate consults.
    #[test]
    fn reoriented_matches_apply() {
        let el = placed("R1", 0, 0);
        // Rotate: R0 → R90.
        let rot = Proposal::Rotate { idx: 0 };
        assert_eq!(reoriented(&el, rot).rotation, Rotation::R90);
        assert!(!reoriented(&el, rot).mirror_y);
        // Mirror: toggles mirror_y.
        let mir = Proposal::MirrorY { idx: 0 };
        assert!(reoriented(&el, mir).mirror_y);
        assert_eq!(reoriented(&el, mir).rotation, Rotation::R0);
        // Jitter: unchanged.
        let jit = Proposal::Jitter {
            idx: 0,
            dx: 1,
            dy: 0,
        };
        assert_eq!(reoriented(&el, jit), Orientation::IDENTITY);
    }
}
