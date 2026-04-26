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
};

/// SA proposals between two cost-breakdown log lines.
const LOG_EVERY: usize = 1000;

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
) -> Placement {
    // Origins are `GridPoint` (i32) by construction, so the FR-to-SA
    // boundary is implicitly grid-snapped: FR writes back through
    // `mm_to_grid`. If we ever hold continuous coords across the
    // boundary, the explicit snap goes here.

    let n = seed.elements.len();
    let movable: Vec<usize> = (0..n).filter(|i| !pinned[*i]).collect();
    if movable.is_empty() || opts.sa_iters == 0 {
        return seed;
    }

    let weights = CostWeights::DEFAULT;
    let mut current_breakdown = cost::breakdown(&seed, checked, library);
    let mut current_cost = cost::total(&current_breakdown, &weights);

    let mut best = seed.clone();
    let mut best_cost = current_cost;

    let mut rng = Rng::new(opts.seed);

    // Cooling: exponential, factor 1000 over the iteration count.
    // f64 widening only; iteration count fits comfortably.
    #[allow(clippy::cast_precision_loss)]
    let total_iters = opts.sa_iters as f64;
    let t0 = initial_temperature(&current_breakdown, &weights);
    let t_final = t0 / 1000.0;
    let alpha = (t_final / t0).powf(1.0 / total_iters.max(1.0));

    log::debug!(
        "spice-layout SA: {} movable / {} elements, T0={:.3}, alpha={:.5}, iters={}",
        movable.len(),
        n,
        t0,
        alpha,
        opts.sa_iters
    );

    let mut temperature = t0;
    for it in 0..opts.sa_iters {
        let move_idx = movable[rng.next_below(movable.len())];
        let (saved_origin, saved_orient) = (
            seed.elements[move_idx].origin,
            seed.elements[move_idx].orientation,
        );
        propose(&mut seed.elements[move_idx], &mut rng);

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
            // Revert.
            seed.elements[move_idx].origin = saved_origin;
            seed.elements[move_idx].orientation = saved_orient;
        }

        temperature *= alpha;

        if it % LOG_EVERY == 0 {
            log::debug!(
                "  it={it} T={temperature:.4} cost={current_cost:.3} \
                 hpwl={:.2} overlap={:.2} crossings={:.0} cv={:.3}",
                current_breakdown.hpwl,
                current_breakdown.overlap,
                current_breakdown.crossings,
                current_breakdown.constraint_violation,
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

/// Apply one random move to `el`.
fn propose(el: &mut PlacedElement, rng: &mut Rng) {
    // 7-in-8: position jitter; 1-in-8: rotate.
    let r = rng.next_below(8);
    if r == 0 {
        rotate_once(el);
    } else {
        jitter(el, rng);
    }
}

fn jitter(el: &mut PlacedElement, rng: &mut Rng) {
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
        el.origin = GridPoint::new(el.origin.x + dx, el.origin.y + dy);
        return;
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
