//! Deterministic, order-preserving compaction (ADR-17 stage 8).
//!
//! # Why this exists
//!
//! The SA ablation recorded in ADR-17 measured what the simulated
//! annealer actually bought: on four of ten fixtures it moved nothing at
//! all, it fixed **zero** crossings anywhere, and where it did act its
//! whole measurable contribution was **pulling the generous structural
//! seed strides tight**. That is deterministic work being done by a
//! stochastic process, and the price was global unattributability — every
//! local edit re-based the whole page through the Metropolis walk (P11
//! measured 17 of 17 poses moving when one bypass cap was added).
//!
//! This pass does that one job directly.
//!
//! # The three properties that make it the anti-SA
//!
//! * **Order-preserving.** Elements are grouped into columns (equal X) and
//!   rows (equal Y); a column may slide, but columns never swap and an
//!   element never crosses a neighbour. The left-to-right and top-to-bottom
//!   reading order the structural seed established (V6 bands and layers) is
//!   therefore an invariant of this pass, not something it re-derives.
//! * **Monotone.** Every move strictly *removes slack*: a column's new
//!   coordinate is `min(original, tightest_legal)`. A column is never
//!   pushed outward. So the pass can only shrink the layout, and running it
//!   twice is idempotent-ish (the second sweep finds less to do, never
//!   more).
//! * **Local.** A column's position depends only on the columns already
//!   settled between it and the frame edge, through a geometric clearance
//!   test. Adding an element to the far side of the sheet cannot change
//!   what happens on this side — the property P11 exists to measure and
//!   the SA structurally cannot provide.
//!
//! # Spacing is derived, not tuned
//!
//! The clearance between adjacent elements comes from
//! [`crate::world_extent_with_glyphs`] — the orientation-transformed body
//! bbox, unioned with pin reach, the value-text width estimate, and
//! ADR-14's power-glyph footprint reservation. Compaction therefore cannot
//! squeeze a symbol into the space a power glyph or a value string is going
//! to occupy in decoration. (ADR-14's reservation is deliberately
//! incomplete today; completing it is ADR-17 stage 4. This pass uses what
//! exists and widens nothing.)
//!
//! # What it never touches
//!
//! Pinned elements. A user `*@place` / `*@align`, a V7 symmetry pair, an
//! ADR-4 sidecar hint and the seed idiom pins all set `pinned`, and a
//! column or row containing a pinned member is frozen where it is —
//! everything else compacts around it. Orientation and mirror are not this
//! pass's business either: it moves origins only.

use std::collections::HashMap;

use kicad_symbols::Library;
use spice_policy::CheckedNetlist;

use crate::{GridPoint, Placement, WorldExtent, world_extent_with_glyphs};

/// Column sweeps. A second sweep pays for itself because the first one
/// changes which columns share a Y span, which can free a later column to
/// move further; a third measured no movement on any fixture.
const SWEEPS: usize = 2;

/// Round a millimetre distance UP to whole grid cells, allowing a
/// negative result. Unlike [`crate::mm_up_to_cells`] this does **not**
/// clamp to 1: a required separation smaller than one cell is a genuine
/// "these two may abut" answer, and clamping it would silently re-inflate
/// exactly the strides this pass exists to remove.
#[allow(clippy::cast_possible_truncation)]
fn ceil_cells(mm: f64) -> i32 {
    (mm / GridPoint::STEP_MM).ceil() as i32
}

/// Which coordinate a sweep works on.
///
/// **Only [`Axis::X`] is ever swept, and that asymmetry is the single most
/// load-bearing decision in this module.** The enum exists because the
/// clearance and alignment maths are genuinely axis-generic, and because
/// the Y variant is what the `sweeps_x_only_by_design` test pins down.
///
/// **X spacing is slack; Y spacing is meaning.** The X-layer stride is a
/// flow-depth ordering carrying a deliberately generous constant floor
/// (`X_STRIDE_FLOOR` in `lib.rs`), so the gap between two layers holds no
/// information and closing it costs nothing. The Y bands are V6's
/// *semantic* structure — Top is the positive rail, Mid is signal, Bot is
/// ground — and the gap between them is the drawing convention a reader
/// relies on to tell rails from circuitry.
///
/// Squeezing Y is order-preserving and still wrong: it collapses the
/// signal band onto the rails. Measured on this fixture set, squeezing
/// both axes rather than X alone cost `common_emitter` B 6 → 7 and
/// `opamp_inverting_real` B 6 → 7 with J 0 → 1. A Y pass restricted to
/// pure alignment snapping (move only when it strictly increases aligned
/// pins, never merely to close a gap) was also tried and was worse still,
/// at `common_emitter` B = 11. Do not re-add a Y sweep without new
/// evidence; both shapes have been measured and rejected.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    X,
    #[allow(dead_code)]
    Y,
}

/// Compact `placement`, in place.
///
/// `pinned` marks elements whose coordinates are owned by something
/// stronger than this heuristic; a line (column or row) containing one is
/// never moved. Returns the number of elements whose origin changed.
// Callers always build `prefs` with the default hasher
// (`net_class::vertical_prefs`), matching `legalize` and `glyph_geom`.
#[allow(clippy::implicit_hasher)]
pub fn compact(
    placement: &mut Placement,
    pinned: &[bool],
    checked: &CheckedNetlist,
    _library: &Library,
    prefs: &HashMap<String, crate::net_class::VertPref>,
) -> usize {
    let before: Vec<GridPoint> = placement.elements.iter().map(|e| e.origin).collect();
    for _ in 0..SWEEPS {
        compact_axis(placement, pinned, checked, prefs, Axis::X);
    }
    let moved = placement
        .elements
        .iter()
        .zip(&before)
        .filter(|(e, b)| e.origin != **b)
        .count();
    if moved > 0 {
        log::debug!("compact: pulled {moved} element(s) tight");
    }
    moved
}

/// Resolved extents for every element, in the roomy flavour: body ∪ pin
/// reach ∪ value text ∪ ADR-14 power-glyph zone.
///
/// A `;@ power` source renders nothing at all (V10 — decoration replaces
/// it with rail glyphs), so it gets a zero extent and neither blocks a
/// neighbour nor claims space of its own. Without that, compaction would
/// hold a gap open around a symbol that is never drawn.
fn extents(
    placement: &Placement,
    checked: &CheckedNetlist,
    prefs: &HashMap<String, crate::net_class::VertPref>,
) -> Vec<WorldExtent> {
    placement
        .elements
        .iter()
        .enumerate()
        .map(|(i, placed)| {
            if placed.is_power_source {
                return WorldExtent::default();
            }
            checked
                .elements
                .get(i)
                .map_or(WorldExtent::default(), |el| {
                    world_extent_with_glyphs(el, placed.orientation, placed.value.as_deref(), prefs)
                })
        })
        .collect()
}

/// A signal pin of one element, as an offset from that element's origin.
/// Rail pins are excluded — decoration terminates them in a `power:*`
/// glyph rather than a wire, so aligning two of them buys no straight ink.
struct SignalPin<'a> {
    net: &'a str,
    dx: f64,
    dy: f64,
}

/// Signal-pin offsets per element, in the element's *current* orientation.
///
/// Suppressed power sources contribute nothing: the emitter draws neither
/// their symbol nor their pins (V10), so there is no ink to straighten.
fn signal_pins<'a>(
    placement: &Placement,
    checked: &'a CheckedNetlist,
    prefs: &HashMap<String, crate::net_class::VertPref>,
) -> Vec<Vec<SignalPin<'a>>> {
    placement
        .elements
        .iter()
        .enumerate()
        .map(|(i, placed)| {
            let mut out = Vec::new();
            if placed.is_power_source {
                return out;
            }
            let Some(el) = checked.elements.get(i) else {
                return out;
            };
            let pins = el.symbol.pins_in(placed.orientation);
            for (term, net) in el.nodes.iter().enumerate() {
                if net == "0" || prefs.contains_key(net) {
                    continue;
                }
                let Some(number) = el.pin_mapping.get(term) else {
                    continue;
                };
                let Some(p) = pins.iter().find(|p| &p.number == number) else {
                    continue;
                };
                out.push(SignalPin {
                    net: net.as_str(),
                    dx: p.x,
                    dy: -p.y,
                });
            }
            out
        })
        .collect()
}

/// How many shared-net signal-pin pairs would land on the same `axis`
/// coordinate if the members of this line sat at `cand`.
///
/// This is the *alignment* half of the pass, and it is why compaction is
/// not a plain 1-D squeeze. Greedy squeezing alone actively destroys
/// alignment: two elements one cell apart sit on different lines, so
/// whichever is less obstructed slides further and the pair ends up
/// *further* apart than it started — measured on `named_rails`, where
/// `RIN` and `CL` went from 1.27 mm apart to 11.4 mm and the extra bend
/// showed up in the ink. Scoring candidate coordinates by how many
/// shared-net pins they line up turns the same sweep into a snap: a
/// vertical wire is straight exactly when its two pins share an X.
fn alignment_score(
    placement: &Placement,
    pins: &[Vec<SignalPin<'_>>],
    axis: Axis,
    members: &[usize],
    cand: i32,
    settled: &[(i32, Vec<usize>)],
) -> u32 {
    const EPS: f64 = 1e-6;
    let cand_mm = f64::from(cand) * GridPoint::STEP_MM;
    let mut score = 0;
    for &i in members {
        for pi in &pins[i] {
            let mine = cand_mm
                + match axis {
                    Axis::X => pi.dx,
                    Axis::Y => pi.dy,
                };
            // Score only against lines already SETTLED. An unsettled
            // neighbour has not chosen its coordinate yet, so aligning to
            // where it currently sits is speculative — and measurably
            // worse (`common_emitter` total wire 67.3 → 77.5 mm when the
            // scan was widened to include them). A sequential sweep
            // should align to decided facts.
            for (_, settled_members) in settled {
                for &j in settled_members {
                    if j == i {
                        continue;
                    }
                    let (ox, oy) = placement.elements[j].origin.to_mm();
                    for pj in &pins[j] {
                        if pj.net != pi.net {
                            continue;
                        }
                        let theirs = match axis {
                            Axis::X => ox + pj.dx,
                            Axis::Y => oy + pj.dy,
                        };
                        if (mine - theirs).abs() < EPS {
                            score += 1;
                        }
                    }
                }
            }
        }
    }
    score
}

/// Would putting `members` at `cand` leave the V11 foreign-pin-coincidence
/// count no worse than `base`?
///
/// Trials the move in place and restores the original coordinates before
/// returning, so the caller sees no side effect.
fn v11_safe(
    placement: &mut Placement,
    checked: &CheckedNetlist,
    members: &[usize],
    axis: Axis,
    cand: i32,
    base: usize,
) -> bool {
    let saved: Vec<GridPoint> = members
        .iter()
        .map(|&i| placement.elements[i].origin)
        .collect();
    for &i in members {
        let o = placement.elements[i].origin;
        placement.elements[i].origin = match axis {
            Axis::X => GridPoint::new(cand, o.y),
            Axis::Y => GridPoint::new(o.x, cand),
        };
    }
    let ok = crate::solver::foreign_pin_coincidences(placement, checked) <= base;
    for (&i, &o) in members.iter().zip(&saved) {
        placement.elements[i].origin = o;
    }
    ok
}

/// One order-preserving compaction sweep along `axis`.
///
/// Elements sharing a coordinate on `axis` form a *line*. Lines are
/// visited in increasing coordinate order; the first stays put (it defines
/// the frame edge) and each later one moves into the window
/// `[tightest_legal, current]` — never past its predecessor, never outward.
/// Within that window it takes the coordinate that aligns the most
/// shared-net pins, and the tightest such coordinate when several tie.
fn compact_axis(
    placement: &mut Placement,
    pinned: &[bool],
    checked: &CheckedNetlist,
    prefs: &HashMap<String, crate::net_class::VertPref>,
    axis: Axis,
) {
    let ext = extents(placement, checked, prefs);
    let pins = signal_pins(placement, checked, prefs);
    let n = placement.elements.len();
    if n == 0 {
        return;
    }

    // Group element indices by their coordinate on `axis`.
    let coord = |p: GridPoint| match axis {
        Axis::X => p.x,
        Axis::Y => p.y,
    };
    let mut lines: HashMap<i32, Vec<usize>> = HashMap::new();
    for (i, e) in placement.elements.iter().enumerate() {
        lines.entry(coord(e.origin)).or_default().push(i);
    }
    let mut keys: Vec<i32> = lines.keys().copied().collect();
    keys.sort_unstable();

    // Settled lines, in visit order: (new coordinate, member indices).
    let mut settled: Vec<(i32, Vec<usize>)> = Vec::with_capacity(keys.len());
    let mut prev_new: Option<i32> = None;

    for key in keys {
        let members = lines.remove(&key).expect("key came from the map");
        let frozen = members.iter().any(|&i| pinned.get(i).copied() == Some(true));

        let new_coord = if frozen || prev_new.is_none() {
            // The first line defines the frame edge; a line holding a
            // pinned member is owned by a stronger mechanism than this.
            key
        } else {
            // Lower bound: never cross the predecessor, and clear every
            // settled line whose cross-axis span overlaps ours.
            let mut lower = prev_new.expect("checked above");
            for (settled_coord, settled_members) in &settled {
                for &j in settled_members {
                    for &i in &members {
                        if !spans_overlap(placement, &ext, axis, i, j) {
                            continue;
                        }
                        let need = gap_mm(&ext, axis, j, i);
                        lower = lower.max(settled_coord + ceil_cells(need));
                    }
                }
            }
            // Monotone: only ever remove slack. If the line already sits
            // at or inside its bound, leave it — pushing outward is
            // legalization's job, not compaction's.
            let lower = lower.min(key);
            // Choose within `[lower, key]`: most shared-net pins aligned
            // wins, tightest coordinate breaks the tie. Both keys are
            // total and deterministic, so there is no RNG and no
            // acceptance probability anywhere in this decision.
            //
            // Every candidate is additionally gated on V11 — Tier 0.
            // Coincident pins are electrically joined, so a squeeze that
            // lands one element's pin on a foreign net's pin silently
            // shorts them, and ERC does not flag it because a short is
            // electrically valid. Measured on `common_emitter`: an
            // unguarded squeeze merged `c`, `e` and the emitter bypass
            // into one net. The check is a *measurement* on the trial
            // position, not a prediction, exactly as `legalize::shove_one`
            // does it.
            let base_v11 = crate::solver::foreign_pin_coincidences(placement, checked);
            let mut best: Option<(u32, i32)> = None;
            for cand in lower..=key {
                if !v11_safe(placement, checked, &members, axis, cand, base_v11) {
                    continue;
                }
                let s = alignment_score(placement, &pins, axis, &members, cand, &settled);
                if best.is_none_or(|(bs, _)| s > bs) {
                    best = Some((s, cand));
                }
            }
            // No candidate clears V11 — including, possibly, the current
            // position if the placement arrived already shorted. Staying
            // put is always available and never makes it worse.
            best.map_or(key, |(_, c)| c)
        };

        if new_coord != key {
            for &i in &members {
                let o = placement.elements[i].origin;
                placement.elements[i].origin = match axis {
                    Axis::X => GridPoint::new(new_coord, o.y),
                    Axis::Y => GridPoint::new(o.x, new_coord),
                };
            }
        }
        prev_new = Some(new_coord);
        settled.push((new_coord, members));
    }
}

/// Clearance (mm) the *origins* of `left` and `right` need along `axis`,
/// so their resolved extents do not intersect.
///
/// `left` is the element on the smaller-coordinate side. The requirement
/// is `right.origin + right.min ≥ left.origin + left.max`, i.e. an origin
/// separation of `left.max − right.min`. No extra clearance constant is
/// added: the roomy extent already carries `MIN_CLEARANCE`-scale padding
/// through the value-text and glyph terms, and stacking another one on top
/// is what re-inflates the strides.
fn gap_mm(ext: &[WorldExtent], axis: Axis, left: usize, right: usize) -> f64 {
    match axis {
        Axis::X => ext[left].max_x - ext[right].min_x,
        Axis::Y => ext[left].max_y - ext[right].min_y,
    }
}

/// Do elements `i` and `j` overlap on the axis *perpendicular* to the
/// sweep? Only such a pair constrains the sweep — two elements on
/// different rows never collide however tightly their columns pack.
fn spans_overlap(
    placement: &Placement,
    ext: &[WorldExtent],
    axis: Axis,
    i: usize,
    j: usize,
) -> bool {
    const EPS: f64 = 1e-3;
    let (oi_x, oi_y) = placement.elements[i].origin.to_mm();
    let (oj_x, oj_y) = placement.elements[j].origin.to_mm();
    let (i_lo, i_hi, j_lo, j_hi) = match axis {
        // Sweeping X: the perpendicular axis is Y.
        Axis::X => (
            oi_y + ext[i].min_y,
            oi_y + ext[i].max_y,
            oj_y + ext[j].min_y,
            oj_y + ext[j].max_y,
        ),
        Axis::Y => (
            oi_x + ext[i].min_x,
            oi_x + ext[i].max_x,
            oj_x + ext[j].min_x,
            oj_x + ext[j].max_x,
        ),
    };
    // A degenerate (zero-width) extent belongs to a suppressed power
    // source and constrains nobody.
    if i_hi - i_lo < EPS || j_hi - j_lo < EPS {
        return false;
    }
    i_lo + EPS < j_hi && j_lo + EPS < i_hi
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pass sweeps X and never Y — see the [`Axis`] doc for the two
    /// measured failures that decided it. This test exists so removing
    /// that asymmetry is a deliberate act with a failing assertion
    /// attached, not a tidy-up someone does by accident.
    #[test]
    fn sweeps_x_only_by_design() {
        // `include_str!` resolves relative to this file at compile time,
        // so the assertion works from any working directory.
        let src = include_str!("compact.rs");
        let body = src
            .split("mod tests")
            .next()
            .expect("module body precedes its tests");
        assert!(
            body.contains("compact_axis(placement, pinned, checked, prefs, Axis::X)"),
            "the X sweep must stay"
        );
        assert!(
            !body.contains("Axis::Y)"),
            "a Y sweep was re-added; both a Y squeeze and a Y snap-only pass \
             were measured and were WORSE (see the `Axis` doc comment). If new \
             evidence says otherwise, update that doc in the same commit."
        );
    }

    #[test]
    fn ceil_cells_allows_negative_and_does_not_clamp() {
        // The clamp-to-1 in `mm_up_to_cells` would re-inflate exactly the
        // strides this pass removes, so the local helper must not have it.
        assert_eq!(ceil_cells(0.0), 0);
        assert_eq!(ceil_cells(1.27), 1);
        assert_eq!(ceil_cells(1.28), 2);
        assert_eq!(ceil_cells(-1.27), -1);
        assert_eq!(ceil_cells(-1.0), 0);
    }
}
