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
//! Mirror moves (`Orientation::flip`) are **deferred** per ADR-3:
//! they are part of the design search space but not implemented in
//! this first cut. TODO(stage 5): add mirror to the proposal
//! distribution alongside an idiom-aware proposal that prefers
//! mirrors near matched-device pairs.
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
pub(super) fn refine(
    mut seed: Placement,
    pinned: &[bool],
    checked: &CheckedNetlist,
    library: &Library,
    opts: &LayoutOptions,
    layers: &LayerAssignment,
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
        let proposal = propose_move(&seed, &movable, &layer_buckets, &swap_layers, &mut rng);
        let saved = apply_move(&mut seed, &proposal);

        let trial_breakdown = cost::breakdown(&seed, checked, library);
        let trial_cost = cost::total(&trial_breakdown, &weights);
        let delta = trial_cost - current_cost;
        let accept = delta <= 0.0 || rng.next_f64() < (-delta / temperature.max(1e-12)).exp();

        if accept {
            current_breakdown = trial_breakdown;
            current_cost = trial_cost;
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
    /// Swap the Y rank (origin.y) of two same-layer movable elements.
    SwapY { a: usize, b: usize },
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
/// * 0.7 jitter, 0.1 rotate, 0.2 same-layer Y-rank swap (when at
///   least one layer has two or more movable elements; otherwise
///   the swap weight collapses into jitter).
fn propose_move(
    placement: &Placement,
    movable: &[usize],
    layer_buckets: &std::collections::BTreeMap<u32, Vec<usize>>,
    swap_layers: &[u32],
    rng: &mut Rng,
) -> Proposal {
    // Resolve a proposal in {jitter, rotate, swap}. We bias to jitter:
    // the bulk of SA work is local position search.
    let bucket = rng.next_below(10);
    let want_swap = !swap_layers.is_empty() && bucket < 2; // 0.2
    let want_rotate = bucket == 2; // 0.1

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
    if want_rotate {
        Proposal::Rotate { idx }
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
    // Rotate 90° CCW; preserve mirror-y (always false in stage 3).
    let new_rotation = match el.orientation.rotation {
        Rotation::R0 => Rotation::R90,
        Rotation::R90 => Rotation::R180,
        Rotation::R180 => Rotation::R270,
        Rotation::R270 => Rotation::R0,
    };
    el.orientation = Orientation {
        rotation: new_rotation,
        mirror_y: el.orientation.mirror_y,
    };
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
            let p = propose_move(&placement, &movable, &buckets, &swap_layers, &mut rng);
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
}
