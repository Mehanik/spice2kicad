//! Fruchterman-Reingold force-directed seeding (continuous mm).
//!
//! Operates on element *origins* rather than per-pin positions: pins
//! follow when the symbol moves, and pin-anchored relations are the
//! SA pass's responsibility. FR's job is to move connected components
//! together and push unconnected ones apart, producing a starting
//! point that the SA does not have to dig out of a pile at the
//! origin.
//!
//! Pinned elements (`align`/`place`-fixed by stage-1) do not move;
//! they exert forces on others but receive none.
//!
//! References: Fruchterman & Reingold 1991, "Graph Drawing by
//! Force-Directed Placement". The cooling schedule is the standard
//! linear ramp from `t0` to 0 over the iteration budget.

use std::collections::HashMap;

use spice_policy::CheckedNetlist;

use super::LayoutOptions;
use crate::{GridPoint, Placement};

/// Run FR for `opts.fr_iters` iterations starting from `seed`.
pub(super) fn seed(
    mut seed: Placement,
    pinned: &[bool],
    checked: &CheckedNetlist,
    opts: &LayoutOptions,
) -> Placement {
    let n = seed.elements.len();
    if n < 2 || opts.fr_iters == 0 {
        return seed;
    }

    // Continuous positions in mm; copied from the seed and snapped
    // back to grid by the SA pass.
    let mut pos: Vec<(f64, f64)> = seed
        .elements
        .iter()
        .map(|e| {
            let (x, y) = e.origin.to_mm();
            (x, y)
        })
        .collect();

    // Edge list: pairs of element indices that share at least one
    // non-ground net. Multiplicity (two pins on the same net between
    // the same pair of elements) folds into the spring strength.
    let edges = build_edges(checked);

    // Ideal edge length and FR's `k` constant. The classic formula
    // is `k = C * sqrt(area / n)`. We do not have a fixed bounding
    // area — pick `k` so it matches the cell size: about one cell
    // diagonal per node. This keeps connected nodes ~1 cell apart
    // and unconnected ones spread by roughly `k * sqrt(n)`.
    let k = (f64::from(crate::CELL_W) + f64::from(crate::CELL_H)) * GridPoint::STEP_MM;
    let k_sq = k * k;

    // Cooling: linear ramp from `t0` to 0. `t0 = k` is a sensible
    // default — first iteration can move a node by ~1 ideal length.
    let t0 = k;
    // f64 widening only; iteration count is tiny.
    #[allow(clippy::cast_precision_loss)]
    let total_iters = opts.fr_iters as f64;

    let mut disp: Vec<(f64, f64)> = vec![(0.0, 0.0); n];

    for it in 0..opts.fr_iters {
        // f64 widening only; iteration count is tiny.
        #[allow(clippy::cast_precision_loss)]
        let it_f = it as f64;
        let temperature = t0 * (1.0 - it_f / total_iters).max(0.0);

        // Reset displacements.
        disp.fill((0.0, 0.0));

        // Repulsive forces — every pair.
        for i in 0..n {
            for j in (i + 1)..n {
                let dx = pos[i].0 - pos[j].0;
                let dy = pos[i].1 - pos[j].1;
                let dist_sq = dx.mul_add(dx, dy * dy).max(1e-6);
                let length = dist_sq.sqrt();
                let force = k_sq / dist_sq;
                let fx = (dx / length) * force;
                let fy = (dy / length) * force;
                disp[i].0 += fx;
                disp[i].1 += fy;
                disp[j].0 -= fx;
                disp[j].1 -= fy;
            }
        }

        // Attractive forces — per edge, `dist^2 / k` (FR formula).
        for &(i, j, weight) in &edges {
            let dx = pos[i].0 - pos[j].0;
            let dy = pos[i].1 - pos[j].1;
            let dist_sq = dx.mul_add(dx, dy * dy).max(1e-6);
            let length = dist_sq.sqrt();
            let force = (dist_sq / k) * weight;
            let fx = (dx / length) * force;
            let fy = (dy / length) * force;
            disp[i].0 -= fx;
            disp[i].1 -= fy;
            disp[j].0 += fx;
            disp[j].1 += fy;
        }

        // Apply displacements (clamped by temperature) for unpinned
        // elements only.
        for i in 0..n {
            if pinned[i] {
                continue;
            }
            let (dx, dy) = disp[i];
            let mag = dx.hypot(dy).max(1e-12);
            let limited = mag.min(temperature);
            pos[i].0 += dx / mag * limited;
            pos[i].1 += dy / mag * limited;
        }
    }

    // Write continuous positions back as grid-snapped origins. The SA
    // pass will jitter from here.
    for (i, p) in pos.iter().enumerate() {
        if pinned[i] {
            continue;
        }
        seed.elements[i].origin = GridPoint::new(mm_to_grid(p.0), mm_to_grid(p.1));
    }
    seed
}

/// Build `(i, j, weight)` edges. `weight` counts how many distinct
/// non-ground nets connect this pair (typically 1; can be 2 for an
/// RC where both pins of R1 also touch C1).
fn build_edges(checked: &CheckedNetlist) -> Vec<(usize, usize, f64)> {
    // Map net name → list of element indices that touch it.
    let mut by_net: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, elem) in checked.elements.iter().enumerate() {
        for node in &elem.nodes {
            if node == "0" {
                continue;
            }
            by_net.entry(node.as_str()).or_default().push(i);
        }
    }

    // Pair count: edges (i, j) with i < j, weighted by net count.
    let mut counts: HashMap<(usize, usize), f64> = HashMap::new();
    for (_, mut members) in by_net {
        members.sort_unstable();
        members.dedup();
        for a in 0..members.len() {
            for b in (a + 1)..members.len() {
                *counts.entry((members[a], members[b])).or_insert(0.0) += 1.0;
            }
        }
    }
    let mut out: Vec<(usize, usize, f64)> =
        counts.into_iter().map(|(k, w)| (k.0, k.1, w)).collect();
    // HashMap iteration order is randomised; sort so floating-point
    // summation in the FR loop is deterministic for a given seed.
    out.sort_by_key(|&(i, j, _)| (i, j));
    out
}

#[allow(clippy::cast_possible_truncation)]
fn mm_to_grid(v_mm: f64) -> i32 {
    (v_mm / GridPoint::STEP_MM).round() as i32
}
