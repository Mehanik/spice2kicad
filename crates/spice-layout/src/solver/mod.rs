//! Stage-3 refiner: FR seeding + SA refinement.
//!
//! Pipeline:
//!
//! 1. Stage-1 [`crate::place_seed`] gives a deterministic placement
//!    and a per-element `pinned` mask. Pinned elements (those bound
//!    by `align` / `place`) stay fixed through every later phase.
//! 2. [`force::seed`] runs Fruchterman-Reingold in continuous mm
//!    coordinates over the unpinned elements.
//! 3. [`anneal::refine`] snaps to the integer grid and runs simulated
//!    annealing, minimising the cost from [`crate::cost`].
//!
//! See `docs/layout-roadmap.md` §4 (architecture) and §5 (cost
//! function), and `docs/layout-adr.md` ADR-3 (orientation search).

use kicad_symbols::Library;
use spice_policy::CheckedNetlist;

use crate::Placement;

mod anneal;
mod force;
mod rng;

/// User-tunable knobs for the placer.
#[derive(Debug, Clone, Copy)]
pub struct LayoutOptions {
    /// Run FR seeding + SA refinement after stage-1 placement. When
    /// `false` (default), only stage-1 runs and the result is
    /// deterministic.
    pub refine: bool,
    /// PRNG seed for the SA pass. Same seed → same placement.
    pub seed: u64,
    /// FR iteration budget. ~100 is plenty for the netlist sizes in
    /// `examples/`; bump for larger circuits if seeds look bad.
    pub fr_iters: usize,
    /// SA iteration budget. Higher = better quality, longer runtime.
    /// 5000 hits the ADR-8 wall-clock target on circuits up to a few
    /// hundred elements.
    pub sa_iters: usize,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            refine: false,
            seed: 0xC0FF_EE42,
            fr_iters: 100,
            sa_iters: 5_000,
        }
    }
}

/// Apply FR seeding then SA refinement on top of a stage-1 placement.
///
/// `pinned[i] == true` keeps element `i` fixed through both passes.
/// Pinned elements come from `align` / `place` constraints; the SA
/// pass also treats them as immovable so it cannot trade a hard
/// constraint for a soft one.
pub(crate) fn refine(
    seed: Placement,
    pinned: &[bool],
    checked: &CheckedNetlist,
    library: &Library,
    opts: &LayoutOptions,
) -> Placement {
    let after_fr = force::seed(seed, pinned, checked, opts);
    anneal::refine(after_fr, pinned, checked, library, opts)
}
