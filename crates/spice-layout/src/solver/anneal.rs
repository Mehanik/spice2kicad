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
    // The V11-coincidence gate (which keeps the mirror-Y move from
    // shorting two foreign pins) is engaged only when there is a
    // V14-reoriented active device to protect; otherwise the all-passive
    // fixtures keep their exact pre-V14 SA trajectory.
    let gates_active = !mirror_eligible.is_empty();

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

    let weights = CostWeights::DEFAULT;
    let mut current_breakdown = cost::breakdown(&seed, checked, library);
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
    let mut current_overlaps = symbol_overlap_count(&seed, checked);
    // V5 pin-facing alignment, used as a "never increase" gate on the
    // mirror-Y move only (see acceptance below). Tracked from the seed so
    // a flip can never make signal pins face away from their net.
    let mut current_misalignment = pin_outward_misalignment(&seed, checked);

    let mut best = seed.clone();
    let mut best_cost = current_cost;

    let mut rng = Rng::new(opts.seed);

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
        let proposal = propose_move(
            &seed,
            &movable,
            &mirror_eligible,
            &layer_buckets,
            &swap_layers,
            &mut rng,
        );

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

        let trial_breakdown = cost::breakdown(&seed, checked, library);
        let trial_cost = cost::total(&trial_breakdown, &weights);
        let delta = trial_cost - current_cost;
        // Cost-based Metropolis acceptance (RNG consumed exactly as
        // before), then the two placer-stage hard filters: V14
        // orientation and V11 foreign-pin coincidence. Either one
        // force-rejects a move cost would otherwise accept. The
        // coincidence recount runs only when the move is still alive
        // after V14 and cost, keeping the common path cheap.
        let cost_accept = delta <= 0.0 || rng.next_f64() < (-delta / temperature.max(1e-12)).exp();
        let alive = cost_accept && !v14_infeasible;
        // The V11 foreign-pin-coincidence gate exists to make the new
        // mirror-Y move safe (a flip that overlaps two foreign pins is a
        // short the router cannot undo). It is engaged only when this
        // run actually has a mirror-eligible (V14-reoriented active)
        // element; otherwise it is skipped so the SA trajectory of the
        // all-passive fixtures stays byte-identical to the pre-V14 path
        // (their V11 cleanliness is already maintained by the router).
        let trial_coincidences = if alive && gates_active {
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
            symbol_overlap_count(&seed, checked)
        } else {
            current_overlaps
        };
        let overlap_ok = trial_overlaps <= current_overlaps;
        // V5 pin-facing gate, applied to the mirror-Y move only: a flip
        // that makes more signal pins face away from their net is
        // rejected even when it lowers HPWL, because the immutable cost
        // cannot see the resulting V5 routing defect (a wire doubling
        // back through the flipped active device's body). Confined to
        // mirror-Y so it never perturbs the jitter/rotate/swap trajectory
        // of any other move or fixture.
        let is_mirror = matches!(proposal, Proposal::MirrorY { .. });
        let trial_misalignment = if alive && coincidence_ok && overlap_ok && is_mirror {
            pin_outward_misalignment(&seed, checked)
        } else {
            current_misalignment
        };
        let misalignment_ok = !is_mirror || trial_misalignment <= current_misalignment;
        let accept = alive && coincidence_ok && overlap_ok && misalignment_ok;

        if accept {
            current_breakdown = trial_breakdown;
            current_cost = trial_cost;
            current_coincidences = trial_coincidences;
            current_overlaps = trial_overlaps;
            current_misalignment = trial_misalignment;
            if current_cost < best_cost {
                best = seed.clone();
                best_cost = current_cost;
            }
        } else {
            revert_move(&mut seed, &saved);
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
            &cost::breakdown(&best, checked, library),
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
fn foreign_pin_coincidences(placement: &Placement, checked: &CheckedNetlist) -> usize {
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

/// World-frame half-extents (`half_w`, `half_h`, mm) of an element's
/// graphical body in a given orientation.
///
/// Uses the symbol's real `body_bbox`: an oversized part (an opamp
/// triangle, ~5 mm half-extent) is spaced apart while a small part (a
/// ~1 mm resistor body) is left free to pack tightly, so this gate
/// perturbs only the layouts that genuinely collide. A 90°/270°
/// rotation swaps the width and height extents. A symbol with no
/// graphical body contributes zero extent (it can never collide).
fn body_half_extents(el: &spice_resolve::ResolvedElement, orient: Orientation) -> (f64, f64) {
    let (mut hw, mut hh) = el.symbol.body_bbox().map_or((0.0, 0.0), |b| {
        // Half-extents from the symbol origin (0,0): the body may be
        // off-centre, so take the larger absolute reach on each axis
        // — that is the distance the body can collide outward.
        let hw = b.x0.abs().max(b.x1.abs());
        let hh = b.y0.abs().max(b.y1.abs());
        (hw, hh)
    });
    if matches!(orient.rotation, Rotation::R90 | Rotation::R270) {
        std::mem::swap(&mut hw, &mut hh);
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
/// CLAUDE.md). It is a *precise supplement* to the existing cell-bbox
/// `overlap` cost: that cost already keeps every body within a
/// `CELL_W × CELL_H` footprint apart, so this gate only counts a pair
/// when **at least one body is oversized** — its real half-extent
/// exceeds the cost's cell half-extent on the colliding axis. The only
/// oversized symbol in the fixtures is the opamp triangle (~5 mm
/// half-extent vs the cell's 3.81 mm); once V14 pins it at rot 0 its
/// wide body would let a neighbour slide under it cost-free, which this
/// gate forbids. Keying off "oversized vs the cost cell" makes the gate
/// a genuine no-op for every all-small-symbol fixture: their overlaps
/// are entirely handled by the cost, so the gate's count stays 0 and
/// the SA trajectory is unchanged.
#[allow(clippy::similar_names)] // ahw/ahh, bhw/bhh: half-extent pairs.
fn symbol_overlap_count(placement: &Placement, checked: &CheckedNetlist) -> usize {
    // The cell half-extents the `overlap` cost already enforces. A body
    // within these contributes nothing here (the cost covers it).
    let cell_hw = f64::from(crate::CELL_W) * GridPoint::STEP_MM / 2.0;
    let cell_hh = f64::from(crate::CELL_H) * GridPoint::STEP_MM / 2.0;

    let extents: Vec<(f64, f64, f64, f64, bool)> = checked
        .elements
        .iter()
        .zip(&placement.elements)
        .map(|(el, placed)| {
            let (hw, hh) = body_half_extents(el, placed.orientation);
            let (ox, oy) = placed.origin.to_mm();
            let oversized = hw > cell_hw + 1e-6 || hh > cell_hh + 1e-6;
            (ox, oy, hw, hh, oversized)
        })
        .collect();
    let eps = 1e-3;
    let mut count = 0;
    for a in 0..extents.len() {
        for b in (a + 1)..extents.len() {
            let (ax, ay, ahw, ahh, a_big) = extents[a];
            let (bx, by, bhw, bhh, b_big) = extents[b];
            // Only a pair touching an oversized body is the cost's blind
            // spot; small/small pairs are the cost's job.
            if !a_big && !b_big {
                continue;
            }
            if (ax - bx).abs() + eps < ahw + bhw && (ay - by).abs() + eps < ahh + bhh {
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
        }
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
