//! Legalization: guarantee no two symbol footprints overlap.
//!
//! CLAUDE.md's constraints-vs-costs doctrine says a categorical property
//! must be a hard filter on the candidate space, not a weighted cost. The
//! doctrine had a gap: it governs *moves*, and says nothing about the
//! *initial state*. A filter cannot repair an infeasible start, so when
//! the structural seed emitted overlapping bodies the annealer inherited
//! them and could only decline to make things worse — `opamp_definition_level`
//! placed two resistors inside opamp triangles, reported `2 movable /
//! 8 elements`, and never moved.
//!
//! Worse, nothing owned the guarantee. `cost::overlap` measured a uniform
//! `CELL_W × CELL_H` cell for every element, which happens to
//! over-estimate a small part and so kept small pairs apart *by accident*;
//! when that was replaced with real footprints, two resistors immediately
//! overlapped, because a soft term at a safe weight cannot hold a
//! categorical property. `no_symbol_symbol_overlap_across_fixtures`
//! asserts the property flatly, with no budget — so it is exactly the kind
//! of property that needs an owner.
//!
//! This pass is that owner. It runs after the seed and again after
//! refinement, and it makes "no two footprints overlap" a postcondition
//! rather than something the optimiser is merely discouraged from
//! violating. The soft cost keeps its job — steering toward roomy layouts
//! — and legality stops depending on it.
//!
//! The algorithm is deliberately dull, because a legalizer that surprises
//! you is worse than none: walk elements in a deterministic order, and for
//! each one that overlaps an already-placed neighbour, shove it to the
//! nearest grid position that clears every placed footprint, preferring
//! the smallest displacement. Pinned elements never move — a user
//! `*@place`/`*@align`, a symmetry pair or an idiom pin outranks
//! legality, and if such a placement is illegal the right response is to
//! report it rather than silently defy the user.

use kicad_symbols::{Library, Orientation};
use spice_policy::CheckedNetlist;

use crate::{GridPoint, Placement, WorldExtent, world_extent_with_glyphs};

/// Half-open world rectangle occupied by a placed element.
#[derive(Debug, Clone, Copy)]
struct Footprint {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Footprint {
    fn overlaps(self, other: Self) -> bool {
        // 1 µm slack so footprints that merely touch on the grid are
        // legal — the same tolerance the SA gate and the verifier use.
        const EPS: f64 = 1e-3;
        self.x0 + EPS < other.x1
            && self.x1 > other.x0 + EPS
            && self.y0 + EPS < other.y1
            && self.y1 > other.y0 + EPS
    }
}

fn footprint_at(ext: WorldExtent, origin: GridPoint) -> Footprint {
    let (ox, oy) = origin.to_mm();
    Footprint {
        x0: ox + ext.min_x,
        y0: oy + ext.min_y,
        x1: ox + ext.max_x,
        y1: oy + ext.max_y,
    }
}

/// World extent of a placed element: orientation-transformed body bbox
/// unioned with pin reach.
///
/// This is **exactly** the geometry
/// `placement_quality::no_symbol_symbol_overlap_across_fixtures` asserts
/// on, and matching it precisely is the whole point. An earlier version
/// legalized on `world_extent_with_glyphs`, which additionally reserves
/// value text and the ADR-14 power-glyph zone — a far larger box. The
/// result was a legalizer enforcing a condition nothing asked for: it
/// spread parts that were never overlapping, lengthened wires, and shoved
/// a V7 symmetry pair out of alignment on `multivibrator` to resolve a
/// "clash" that existed only in its own inflated model.
///
/// Reserving glyph space is a legitimate *seed spacing* concern and stays
/// where it is. Legality is a different, narrower question.
fn extent_of(element: &spice_resolve::ResolvedElement, orientation: Orientation) -> WorldExtent {
    let mut ext = WorldExtent {
        min_x: 0.0,
        max_x: 0.0,
        min_y: 0.0,
        max_y: 0.0,
    };
    if let Some(b) = element.symbol.body_bbox() {
        for (lx, ly) in [(b.x0, b.y0), (b.x0, b.y1), (b.x1, b.y0), (b.x1, b.y1)] {
            let (rx, ry) = orientation.apply_point(lx, ly);
            ext.min_x = ext.min_x.min(rx);
            ext.max_x = ext.max_x.max(rx);
            ext.min_y = ext.min_y.min(-ry);
            ext.max_y = ext.max_y.max(-ry);
        }
    }
    for tp in element.symbol.pins_in(orientation) {
        ext.min_x = ext.min_x.min(tp.x);
        ext.max_x = ext.max_x.max(tp.x);
        ext.min_y = ext.min_y.min(-tp.y);
        ext.max_y = ext.max_y.max(-tp.y);
    }
    ext
}

/// Candidate displacements in grid cells, nearest first.
///
/// Ordered by Manhattan distance so the shove is as small as possible,
/// and within a ring by a fixed rotation so the result is deterministic —
/// two runs must place identically (see `conversion_is_deterministic`).
fn displacement_ring(max_cells: i32) -> Vec<(i32, i32)> {
    let mut out = vec![(0, 0)];
    for r in 1..=max_cells {
        let mut ring: Vec<(i32, i32)> = Vec::new();
        for dx in -r..=r {
            for dy in -r..=r {
                if dx.abs().max(dy.abs()) == r {
                    ring.push((dx, dy));
                }
            }
        }
        // Prefer horizontal shoves: schematics read left-to-right, so
        // sliding along a row disturbs the eye less than breaking a column.
        ring.sort_by_key(|&(dx, dy)| (dx.abs() + dy.abs(), dy.abs(), dx, dy));
        out.extend(ring);
    }
    out
}

/// Number of overlapping footprint pairs in `placement`.
#[must_use]
pub fn overlap_count(placement: &Placement, checked: &CheckedNetlist, library: &Library) -> usize {
    let prints = footprints(placement, checked, library);
    let mut n = 0;
    for i in 0..prints.len() {
        for j in (i + 1)..prints.len() {
            if prints[i].overlaps(prints[j]) {
                n += 1;
            }
        }
    }
    n
}

fn footprints(
    placement: &Placement,
    checked: &CheckedNetlist,
    _library: &Library,
) -> Vec<Footprint> {
    placement
        .elements
        .iter()
        .enumerate()
        .map(|(i, placed)| {
            let ext = checked
                .elements
                .get(i)
                .map_or(WorldExtent::default(), |el| {
                    extent_of(el, placed.orientation)
                });
            footprint_at(ext, placed.origin)
        })
        .collect()
}

/// How far a single element may be shoved, in grid cells. Generous
/// enough to escape an oversized neighbour (an opamp triangle is ~4 cells
/// across), bounded so a hopeless case terminates instead of wandering.
const MAX_SHOVE_CELLS: i32 = 12;

/// Everything the per-element shove needs that does not change as
/// elements settle. Grouped so `shove_one` takes a readable argument
/// list instead of eight positional parameters.
struct ShoveCtx<'a> {
    checked: &'a CheckedNetlist,
    /// Tight extents (body ∪ pin reach) — the legality geometry.
    extents: &'a [WorldExtent],
    /// Roomy extents (plus value text and the ADR-14 glyph zone) — a
    /// preference, not a requirement. See `legalize` for why.
    roomy: &'a [WorldExtent],
    ring: &'a [(i32, i32)],
}

/// Find the nearest legal position for element `i`, or `None` if no
/// candidate within `MAX_SHOVE_CELLS` clears the already-settled
/// footprints without regressing V11.
///
/// Mutates `placement` only transiently: each candidate is trialled in
/// place so V11 can be *measured* rather than predicted, and the original
/// origin is always restored before returning.
fn shove_one(
    placement: &mut Placement,
    i: usize,
    ctx: &ShoveCtx<'_>,
    settled: &[(usize, Footprint)],
) -> Option<(GridPoint, Footprint)> {
    let ext = ctx.extents[i];
    let origin = placement.elements[i].origin;
    // Baseline V11 count, so a shove can be required not to worsen it.
    let base_coincidences = crate::solver::foreign_pin_coincidences(placement, ctx.checked);
    let mut fallback = None;

    for &(dx, dy) in ctx.ring {
        let cand = GridPoint {
            x: origin.x + dx,
            y: origin.y + dy,
        };
        let f = footprint_at(ext, cand);
        if settled.iter().any(|&(_, other)| f.overlaps(other)) {
            continue;
        }
        // A shove must never land a pin on a foreign net's pin. That is
        // V11, Tier 0: coincident pins are electrically joined, so the
        // "fix" would short two nets — strictly worse than the overlap it
        // resolves. Measured, not assumed: without this check the
        // legalizer merged common_emitter's collector and emitter nets
        // into one, which ERC does not even flag because a short is
        // electrically valid.
        placement.elements[i].origin = cand;
        let ok =
            crate::solver::foreign_pin_coincidences(placement, ctx.checked) <= base_coincidences;
        placement.elements[i].origin = origin;
        if !ok {
            continue;
        }
        // Legal. Prefer it if it also leaves room for text/glyphs.
        let roomy_f = footprint_at(ctx.roomy[i], cand);
        let roomy_clear = settled.iter().all(|&(j, _)| {
            !roomy_f.overlaps(footprint_at(ctx.roomy[j], placement.elements[j].origin))
        });
        if roomy_clear {
            return Some((cand, f));
        }
        if fallback.is_none() {
            fallback = Some((cand, f));
        }
    }
    fallback
}

/// Shove overlapping elements apart until no two footprints overlap.
///
/// Returns the number of elements moved. Pinned elements are never
/// touched: a user directive or a symmetry pair outranks legality, and
/// quietly overriding one would defeat the point of the annotation.
// Callers always build `prefs` with the default hasher (`net_class::
// vertical_prefs`), so generalising over `BuildHasher` would add a type
// parameter no caller can vary. Same call shape as `glyph_geom`.
#[allow(clippy::implicit_hasher)]
pub fn legalize(
    placement: &mut Placement,
    pinned: &[bool],
    checked: &CheckedNetlist,
    _library: &Library,
    prefs: &std::collections::HashMap<String, crate::net_class::VertPref>,
) -> usize {
    let extents: Vec<WorldExtent> = placement
        .elements
        .iter()
        .enumerate()
        .map(|(i, placed)| {
            checked
                .elements
                .get(i)
                .map_or(WorldExtent::default(), |el| {
                    extent_of(el, placed.orientation)
                })
        })
        .collect();

    // Place in a deterministic order: pinned elements first, so they
    // claim their space and everything else legalizes around them, then
    // the rest by index.
    let mut order: Vec<usize> = (0..placement.elements.len()).collect();
    order.sort_by_key(|&i| (!pinned.get(i).copied().unwrap_or(false), i));

    log::debug!(
        "legalize: {} elements, {} pinned",
        placement.elements.len(),
        pinned.iter().filter(|p| **p).count()
    );
    // Snapshot for the Tier-0 safety net below.
    let before_origins: Vec<GridPoint> = placement.elements.iter().map(|e| e.origin).collect();
    let before_coincidences = crate::solver::foreign_pin_coincidences(placement, checked);
    // The *roomy* extent additionally reserves value text and the ADR-14
    // power-glyph zone. Legality never requires it — the overlap assert
    // measures body ∪ pin reach — but decoration still has to fit text
    // somewhere, and a position that is merely legal can leave it nowhere
    // to go: packing RF2 tight on `opamp_definition_level` left both the
    // `out2` label and VCC's net name with no clear spot, trading two
    // Tier-1 text invariants for the overlap fix. So candidates that also
    // clear the roomy extent are preferred, and the tight one is the
    // fallback.
    let roomy: Vec<WorldExtent> = placement
        .elements
        .iter()
        .enumerate()
        .map(|(i, placed)| {
            checked
                .elements
                .get(i)
                .map_or(WorldExtent::default(), |el| {
                    world_extent_with_glyphs(el, placed.orientation, placed.value.as_deref(), prefs)
                })
        })
        .collect();
    let ring = displacement_ring(MAX_SHOVE_CELLS);
    let ctx = ShoveCtx {
        checked,
        extents: &extents,
        roomy: &roomy,
        ring: &ring,
    };
    let mut settled: Vec<(usize, Footprint)> = Vec::new();
    let mut moved = 0usize;

    for &i in &order {
        // A `;@ power` source draws nothing — decoration replaces it with
        // rail glyphs (V10) — so it occupies no space and must not shove
        // anyone. Without this the legalizer moved three elements on
        // `common_emitter` to clear a symbol that is never rendered.
        if placement.elements[i].is_power_source {
            continue;
        }
        let origin = placement.elements[i].origin;
        let here = footprint_at(extents[i], origin);

        if !settled.iter().any(|&(_, other)| here.overlaps(other))
            || pinned.get(i).copied().unwrap_or(false)
        {
            // Legal where it is, or immovable. A pinned element that
            // clashes stays put and is reported by the postcondition —
            // the user's instruction wins over our tidiness.
            settled.push((i, here));
            continue;
        }

        if let Some((cand, f)) = shove_one(placement, i, &ctx, &settled) {
            if cand != origin {
                placement.elements[i].origin = cand;
                moved += 1;
            }
            settled.push((i, f));
        } else {
            // No legal spot within reach. Leave it and let the
            // postcondition report an overlap rather than teleport
            // the element somewhere absurd.
            log::debug!(
                "legalize: no legal position within {MAX_SHOVE_CELLS} cells for element {i} ({:.2},{:.2})..({:.2},{:.2})",
                here.x0,
                here.y0,
                here.x1,
                here.y1
            );
            settled.push((i, here));
        }
    }
    // Tier-0 safety net. Coincident pins are electrically joined, so a
    // shove that creates one silently shorts two nets — categorically
    // worse than the overlap it fixes, and CLAUDE.md's tier rule trades
    // Tier 0 for nothing. The per-move guard checks each shove against
    // the state at that moment, which is not enough: moves are
    // sequential, so the set can drift as later elements land. Re-check
    // the finished result and abandon the whole pass if it regressed —
    // an unlegalized placement is a quality defect, a shorted one is a
    // wrong circuit.
    if crate::solver::foreign_pin_coincidences(placement, checked) > before_coincidences {
        for (el, origin) in placement.elements.iter_mut().zip(&before_origins) {
            el.origin = *origin;
        }
        log::debug!("legalize: abandoned — would have introduced a V11 pin coincidence");
        return 0;
    }
    if moved > 0 {
        log::debug!("legalize: moved {moved} element(s)");
    }
    moved
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(x0: f64, y0: f64, x1: f64, y1: f64) -> Footprint {
        Footprint { x0, y0, x1, y1 }
    }

    #[test]
    fn touching_footprints_are_legal() {
        // Abutting on the grid is how tight layouts are supposed to look.
        assert!(!fp(0.0, 0.0, 2.0, 2.0).overlaps(fp(2.0, 0.0, 4.0, 2.0)));
    }

    #[test]
    fn straddling_footprints_overlap() {
        assert!(fp(0.0, 0.0, 2.0, 2.0).overlaps(fp(1.0, 1.0, 3.0, 3.0)));
    }

    #[test]
    fn displacement_ring_starts_at_no_move_and_grows() {
        let ring = displacement_ring(2);
        assert_eq!(ring[0], (0, 0));
        // Manhattan distance is non-decreasing, so the first legal spot
        // found is also the smallest disturbance.
        let dists: Vec<i32> = ring.iter().map(|&(x, y)| x.abs() + y.abs()).collect();
        assert!(dists.windows(2).all(|w| w[0] <= w[1]), "{dists:?}");
    }

    #[test]
    fn displacement_ring_is_deterministic() {
        assert_eq!(displacement_ring(3), displacement_ring(3));
    }
}
