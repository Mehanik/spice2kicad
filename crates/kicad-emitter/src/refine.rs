//! Routing-aware orientation refinement (CLAUDE.md "Layout phase 4.5").
//!
//! V5 (first wire segment leaves each pin outward) is *routing-determined*:
//! a violation is born inside the router's post-construction
//! conflict-resolution passes
//! (`spice_route::conflict::{avoid_foreign_pins, avoid_obstacles}`), which
//! are invisible to any placement-side model. So orientation selection
//! that wants to minimise V5 must use the **real router as its oracle**.
//!
//! `spice-layout` cannot depend on `spice-route` (that edge would form a
//! cycle — `spice-route` already depends on `spice-layout`). `kicad-emitter`
//! depends on *both*, so this is the one place the router can be put in the
//! loop. The phase runs AFTER `spice_layout::place_with_hint` and BEFORE the
//! final `emit_root`/`route_nets` + decoration, and may only change an
//! element's **orientation** — never its position. Decoration stays a strict
//! consumer of the placement it receives.
//!
//! The pass is greedy and deterministic (no clock / RNG, stable iteration
//! order), iterating to a fixed point under a small cap. For each at-risk,
//! non-pinned, non-symmetry element it trial-routes each V14-allowed
//! orientation (reusing `spice_layout::orient::allowed_orientations` — it
//! never widens V14) and keeps a candidate ONLY if it *strictly* reduces the
//! router's real V5 count without increasing V11 residue, symbol-body
//! overlap, or foreign-body (V12) crossings.

use kicad_symbols::{Library, Orientation, Symbol};
use spice_layout::{Placement, RefinementMeta};

use crate::schematic::{
    TextBbox, collect_net_pins, label_specs, placement_obstacles, placement_property_bboxes,
    text_bbox, trial_route,
};
use crate::v5::{PinProbe, Violation, count_outward_violations};

/// Maximum greedy sweeps over the element list before giving up on
/// further improvement. A handful is plenty for the small fixtures;
/// the cap bounds worst-case cost on a pathological large sheet.
const MAX_SWEEPS: usize = 4;

/// Cap on the number of orientation *combinations* the combinatorial
/// joint search will trial-route. A V5 violation is frequently only
/// removable by rotating an offending element AND a shared-net neighbour
/// *together* (e.g. the inverting-amp's RIN + RF + X1), which a purely
/// greedy single-element sweep cannot reach without a strictly-improving
/// intermediate step. The joint search enumerates the cartesian product
/// of the active elements' allowed orientations; this cap bounds the
/// trial-route count so a large active set degrades gracefully (the
/// search is skipped and only the greedy sweep runs).
const MAX_COMBINATIONS: usize = 512;

/// Cap on the number of *active* elements the joint search considers at
/// once (offenders + their direct shared-signal-net neighbours,
/// non-pinned). Bounds the product size together with [`MAX_COMBINATIONS`].
const MAX_ACTIVE: usize = 4;

/// Refine element orientations to minimise the router's *real* V5
/// (first-segment-outward) count, in place.
///
/// `meta` carries the same `pinned` mask and V14-`allowed` orientation
/// sets the placer used (see [`spice_layout::refinement_meta`]). Pinned
/// elements (user `align`/`place`, V7 symmetry, position-stability hint)
/// are never touched; every candidate orientation comes from the
/// element's V14-allowed set, so the phase honours V14 by construction.
///
/// Acceptance is conservative: a candidate is taken only if it strictly
/// reduces total real V5 violations AND does not increase the V11
/// foreign-pin residue, the symbol-body overlap count, or the V12
/// foreign-body wire-crossing count. The phase therefore can only
/// improve (or no-op) the higher-/equal-tier invariants while improving
/// the V5 quality metric — never trade one off against another.
pub fn refine_orientations(placement: &mut Placement, library: &Library, meta: &RefinementMeta) {
    let n = placement.elements.len();
    if n == 0 {
        return;
    }

    // Baseline measurement of the placement as received.
    let mut baseline = measure(placement, library);
    if baseline.v5 == 0 {
        return;
    }

    // Greedy single-element descent first: cheap, each accepted step
    // *strictly* reduces real V5, so it converges in at most `v5` steps.
    greedy_descent(placement, library, meta, &mut baseline);
    if baseline.v5 == 0 {
        return;
    }

    // If greedy stalled with V5 still positive, fall back to a bounded
    // *joint* search over the offending elements and their shared-net
    // neighbours. Many V5 violations are removable only by rotating an
    // offender together with a neighbour (e.g. RIN+RF+X1 on the inverting
    // amp), which the strictly-improving greedy descent cannot reach on
    // its own. The joint search early-exits the moment it finds a
    // zero-V5 combination, so its worst-case cost binds only when no full
    // fix exists.
    joint_search(placement, library, meta, &mut baseline);
}

/// Greedy single-element orientation descent: repeatedly pick, for each
/// offending non-pinned element, the V14-allowed orientation that most
/// reduces real V5 without regressing V11 / overlap / V12 / V13. Each
/// accepted move strictly lowers V5, so the sweep converges quickly.
fn greedy_descent(
    placement: &mut Placement,
    library: &Library,
    meta: &RefinementMeta,
    baseline: &mut Measure,
) {
    let n = placement.elements.len();
    for _ in 0..MAX_SWEEPS {
        let mut improved_this_sweep = false;
        for i in 0..n {
            if meta.pinned.get(i).copied().unwrap_or(false) {
                continue;
            }
            let Some(allowed) = meta.allowed.get(i) else {
                continue;
            };
            // Skip elements that cannot currently contribute a V5
            // violation: only those whose own pins are flagged in the
            // baseline are worth re-orienting. This bounds the
            // trial-route count without losing any improvable element.
            let refdes = &placement.elements[i].refdes;
            if !baseline.offenders.iter().any(|v| &v.refdes == refdes) {
                continue;
            }

            let current = placement.elements[i].orientation;
            let candidates = distinct_orientations(
                allowed,
                current,
                library.lookup(&placement.elements[i].lib_id),
            );
            let mut best: Option<(Orientation, Measure)> = None;
            for cand in candidates {
                if cand == current {
                    continue;
                }
                placement.elements[i].orientation = cand;
                let m = measure(placement, library);
                placement.elements[i].orientation = current;

                // Strict V5 improvement, no regression on any
                // equal-/higher-tier guard.
                if m.v5 < baseline.v5
                    && m.v11 <= baseline.v11
                    && m.overlap <= baseline.overlap
                    && m.v12 <= baseline.v12
                    && m.v13 <= baseline.v13
                {
                    let take = match &best {
                        None => true,
                        Some((_, bm)) => m.v5 < bm.v5,
                    };
                    if take {
                        best = Some((cand, m));
                    }
                }
            }

            if let Some((orient, m)) = best {
                placement.elements[i].orientation = orient;
                *baseline = m;
                improved_this_sweep = true;
                if baseline.v5 == 0 {
                    return;
                }
            }
        }
        if !improved_this_sweep {
            break;
        }
    }
}

/// Bounded combinatorial joint orientation search.
///
/// Builds the *active set* — the non-pinned elements currently producing
/// a V5 violation, plus their non-pinned neighbours sharing a signal net
/// (rotating a neighbour can swing a pin's connecting wire outward) —
/// then enumerates the cartesian product of each active element's
/// V14-allowed orientations. The combination minimising real V5 (subject
/// to no V11 / overlap / V12 / V13 regression vs `baseline`) is applied.
///
/// Deterministic: active elements are taken in ascending index order and
/// orientations in their allowed-set order, so the lexicographically
/// first minimal-V5 combination wins. Skipped (leaving the greedy sweep
/// to handle it) when the product would exceed [`MAX_COMBINATIONS`].
// Active-set construction + mixed-radix enumeration share local state
// (active / cand / counter / best) that helper-splitting would obscure.
#[allow(clippy::too_many_lines)]
fn joint_search(
    placement: &mut Placement,
    library: &Library,
    meta: &RefinementMeta,
    baseline: &mut Measure,
) {
    let n = placement.elements.len();

    // Offending element indices (non-pinned, V14-allowed known).
    let movable = |i: usize| {
        !meta.pinned.get(i).copied().unwrap_or(false)
            && meta.allowed.get(i).is_some_and(|a| !a.is_empty())
    };
    let offending: Vec<usize> = (0..n)
        .filter(|&i| {
            movable(i)
                && baseline
                    .offenders
                    .iter()
                    .any(|v| v.refdes == placement.elements[i].refdes)
        })
        .collect();
    if offending.is_empty() {
        return;
    }

    // Neighbours: any movable element sharing a non-ground signal net
    // with an offender. Build net → element-indices, then expand.
    let mut net_to_els: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, el) in placement.elements.iter().enumerate() {
        if el.is_power_source {
            continue;
        }
        for node in &el.nodes {
            if node == "0" {
                continue;
            }
            net_to_els.entry(node.as_str()).or_default().push(i);
        }
    }
    let mut active: Vec<usize> = offending.clone();
    for &o in &offending {
        for node in &placement.elements[o].nodes {
            if node == "0" {
                continue;
            }
            if let Some(els) = net_to_els.get(node.as_str()) {
                for &j in els {
                    if movable(j) && !active.contains(&j) {
                        active.push(j);
                    }
                }
            }
        }
    }
    active.sort_unstable();
    active.truncate(MAX_ACTIVE);

    // Per-active-element candidate orientations: the V14-allowed set with
    // geometrically-equivalent orientations collapsed (a symmetric 2-pin
    // resistor's eight orientations reduce to the handful that yield
    // distinct world pin layouts), so the product stays small. The
    // element's *current* orientation is forced in first so it is always
    // a candidate (the "no change" option).
    let cand: Vec<Vec<Orientation>> = active
        .iter()
        .map(|&i| {
            let symbol = library.lookup(&placement.elements[i].lib_id);
            distinct_orientations(&meta.allowed[i], placement.elements[i].orientation, symbol)
        })
        .collect();

    // Product size guard.
    let mut product: usize = 1;
    for c in &cand {
        product = product.saturating_mul(c.len().max(1));
        if product > MAX_COMBINATIONS {
            return; // Too large — leave it to the greedy sweep.
        }
    }

    let originals: Vec<Orientation> = active
        .iter()
        .map(|&i| placement.elements[i].orientation)
        .collect();

    // Enumerate the cartesian product via a mixed-radix counter.
    let radices: Vec<usize> = cand.iter().map(Vec::len).collect();
    let mut best: Option<(Vec<Orientation>, Measure)> = None;
    let mut counter = vec![0usize; active.len()];
    'enumerate: loop {
        // Apply this combination.
        for (k, &i) in active.iter().enumerate() {
            placement.elements[i].orientation = cand[k][counter[k]];
        }
        let m = measure(placement, library);
        if m.v5 < baseline.v5
            && m.v11 <= baseline.v11
            && m.overlap <= baseline.overlap
            && m.v12 <= baseline.v12
            && m.v13 <= baseline.v13
        {
            let take = match &best {
                None => true,
                Some((_, bm)) => m.v5 < bm.v5,
            };
            if take {
                let reached_zero = m.v5 == 0;
                let chosen: Vec<Orientation> = active
                    .iter()
                    .map(|&i| placement.elements[i].orientation)
                    .collect();
                best = Some((chosen, m));
                // Can't beat zero V5 — stop enumerating early.
                if reached_zero {
                    break 'enumerate;
                }
            }
        }
        // Increment mixed-radix counter; overflow ends enumeration.
        let mut k = 0;
        loop {
            if k == active.len() {
                break 'enumerate;
            }
            counter[k] += 1;
            if counter[k] < radices[k] {
                break;
            }
            counter[k] = 0;
            k += 1;
        }
    }
    // Restore originals and apply best (if any).
    for (idx, &i) in active.iter().enumerate() {
        placement.elements[i].orientation = originals[idx];
    }
    if let Some((orients, m)) = best {
        for (idx, &i) in active.iter().enumerate() {
            placement.elements[i].orientation = orients[idx];
        }
        *baseline = m;
    }
}

/// The metrics the acceptance gate compares. `offenders` carries the V5
/// violations so the sweep can skip elements that aren't offending.
/// `v13` is the combined label↔body + label↔property-text overlap count
/// (V13 parts 1 and 2), measured on the exact labels the emitter will
/// plant ([`label_specs`]).
struct Measure {
    v5: usize,
    v11: usize,
    overlap: usize,
    v12: usize,
    v13: usize,
    offenders: Vec<Violation>,
}

/// Trial-route `placement` and measure V5, V11 residue, symbol-body
/// overlap, V12 foreign-body crossings, and V13 label overlaps.
fn measure(placement: &Placement, library: &Library) -> Measure {
    let route = trial_route(placement, library);
    let pins = pin_probes(placement, library);
    let offenders = count_outward_violations(&pins, &route.segments);
    let overlap = symbol_overlap_count(placement, library);
    let v12 = v12_crossing_count(placement, library, &route.segments);
    let v13 = v13_overlap_count(placement, library);
    Measure {
        v5: offenders.len(),
        v11: route.v11_count,
        overlap,
        v12,
        v13,
        offenders,
    }
}

/// Count V13 label overlaps the emitter would produce for `placement`:
/// a label's text bbox intersecting (1) any symbol body bbox, or (2) any
/// Reference/Value property-text bbox. Uses the emitter's own
/// [`label_specs`] / [`text_bbox`] / [`placement_property_bboxes`] so the
/// gate measures exactly what the verifier grades.
fn v13_overlap_count(placement: &Placement, library: &Library) -> usize {
    let net_pins = collect_net_pins(placement, library, &[]);
    let props = placement_property_bboxes(placement);
    let specs = label_specs(&net_pins, &[], &props);
    // World body bboxes (as TextBboxes) for the label↔body check.
    let bodies: Vec<TextBbox> = placement
        .elements
        .iter()
        .filter_map(|el| {
            if el.is_power_source || el.lib_id.starts_with("power:") {
                return None;
            }
            let (ox, oy) = el.origin.to_mm();
            library
                .lookup(&el.lib_id)
                .and_then(Symbol::body_bbox)
                .map(|b| {
                    let w = body_bbox_world(b, ox, oy, el.orientation);
                    TextBbox {
                        x0: w.x0,
                        y0: w.y0,
                        x1: w.x1,
                        y1: w.y1,
                    }
                })
        })
        .collect();
    let mut hits = 0;
    for spec in &specs {
        let lbbox = text_bbox(&spec.net, (spec.x, spec.y), spec.rot);
        for body in &bodies {
            if lbbox.intersects(*body) {
                hits += 1;
            }
        }
        for p in &props {
            if lbbox.intersects(*p) {
                hits += 1;
            }
        }
    }
    hits
}

/// World-frame pin probes for every placed (non-power-source) element,
/// matching `schematic::collect_net_pins`' transform: a library pin at
/// local `(x, y)` placed at origin `(ox, oy)` lands at world
/// `(ox + x, oy - y)` (eeschema y-flip), carrying the library-frame pin
/// `angle`. Power-rail sources contribute no pins (they are not drawn).
fn pin_probes(placement: &Placement, library: &Library) -> Vec<PinProbe> {
    let mut out = Vec::new();
    for el in &placement.elements {
        if el.is_power_source {
            continue;
        }
        let Some(symbol) = library.lookup(&el.lib_id) else {
            continue;
        };
        let pins = symbol.pins_in(el.orientation);
        let (ox, oy) = el.origin.to_mm();
        for kicad_pin in &el.pin_mapping {
            let Some(pin) = pins.iter().find(|p| &p.number == kicad_pin) else {
                continue;
            };
            out.push(PinProbe {
                refdes: el.refdes.clone(),
                pin_number: pin.number.clone(),
                x_mm: ox + pin.x,
                y_mm: oy - pin.y,
                angle: pin.angle,
            });
        }
    }
    out
}

/// Count pairs of placed elements whose world-frame body bboxes overlap.
/// Mirrors the no-symbol-symbol-overlap verifier's intent (body extent,
/// orientation-aware) so the gate can only ever decline an orientation
/// that introduces a body collision the SA gate would also reject.
fn symbol_overlap_count(placement: &Placement, library: &Library) -> usize {
    let boxes: Vec<Option<spice_route::Bbox>> = placement
        .elements
        .iter()
        .map(|el| {
            if el.is_power_source || el.lib_id.starts_with("power:") {
                return None;
            }
            let (ox, oy) = el.origin.to_mm();
            library
                .lookup(&el.lib_id)
                .and_then(Symbol::body_bbox)
                .map(|b| body_bbox_world(b, ox, oy, el.orientation))
        })
        .collect();
    let mut count = 0;
    for i in 0..boxes.len() {
        for j in (i + 1)..boxes.len() {
            if let (Some(a), Some(b)) = (&boxes[i], &boxes[j])
                && bboxes_overlap(a, b)
            {
                count += 1;
            }
        }
    }
    count
}

/// Count routed wire segments whose interior penetrates a foreign
/// element's body bbox (V12). `placement_obstacles` already excludes
/// power glyphs / suppressed rail sources and adds the router's clearance
/// margin, so an interior intersection here is a genuine V12 crossing.
fn v12_crossing_count(
    placement: &Placement,
    library: &Library,
    segments: &[crate::v5::WireSegment],
) -> usize {
    let obstacles = placement_obstacles(placement, library);
    let mut count = 0;
    for bbox in &obstacles {
        for (a, b) in segments {
            if bbox.intersects_segment(a.0, a.1, b.0, b.1) {
                count += 1;
            }
        }
    }
    count
}

/// Transform a symbol-local body bbox into a world-frame `Bbox`, using
/// the same convention as pin coordinates: rotate/mirror via
/// [`Orientation::apply_point`], then apply the eeschema y-flip
/// (`world_y = origin_y - local_y`), and take the AABB of the four
/// transformed corners.
fn body_bbox_world(
    local: kicad_symbols::LocalBbox,
    ox: f64,
    oy: f64,
    orient: Orientation,
) -> spice_route::Bbox {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (lx, ly) in [
        (local.x0, local.y0),
        (local.x0, local.y1),
        (local.x1, local.y0),
        (local.x1, local.y1),
    ] {
        let (rx, ry) = orient.apply_point(lx, ly);
        let wx = ox + rx;
        let wy = oy - ry;
        min_x = min_x.min(wx);
        max_x = max_x.max(wx);
        min_y = min_y.min(wy);
        max_y = max_y.max(wy);
    }
    spice_route::Bbox {
        x0: min_x,
        y0: min_y,
        x1: max_x,
        y1: max_y,
    }
}

/// Strict (interior) overlap of two world-frame bboxes. A shared edge
/// (touching, e.g. abutting bodies that merely kiss) is not an overlap.
fn bboxes_overlap(a: &spice_route::Bbox, b: &spice_route::Bbox) -> bool {
    let eps = 1e-6;
    a.x0 < b.x1 - eps && b.x0 < a.x1 - eps && a.y0 < b.y1 - eps && b.y0 < a.y1 - eps
}

/// Collapse a V14-allowed orientation set to one representative per
/// *distinct pin geometry*. Two orientations are equivalent when they
/// place every pin (by number) at the same local offset and outward
/// angle — e.g. a symmetric 2-pin resistor's `(mirror y)` variant is
/// identical to its un-mirrored one, so eight orientations reduce to the
/// few that actually move pins. This shrinks the joint-search product
/// without losing any reachable layout.
///
/// `current` is forced to be the first representative so the "no change"
/// option is always trialled (and wins ties via the lexicographic-first
/// rule in the caller). When no symbol is available, the allowed set is
/// returned unchanged (no geometry to dedupe on).
#[allow(clippy::cast_possible_truncation)]
fn distinct_orientations(
    allowed: &[Orientation],
    current: Orientation,
    symbol: Option<&Symbol>,
) -> Vec<Orientation> {
    let Some(symbol) = symbol else {
        return allowed.to_vec();
    };
    // Geometry key: quantised (number, x, y, angle) per pin, sorted.
    let key_of = |o: Orientation| -> Vec<(String, i64, i64, u16)> {
        let mut v: Vec<(String, i64, i64, u16)> = symbol
            .pins_in(o)
            .into_iter()
            .map(|p| {
                (
                    p.number,
                    (p.x * 1000.0).round() as i64,
                    (p.y * 1000.0).round() as i64,
                    p.angle,
                )
            })
            .collect();
        v.sort();
        v
    };
    let mut seen: Vec<Vec<(String, i64, i64, u16)>> = Vec::new();
    let mut out: Vec<Orientation> = Vec::new();
    // Force `current` first if it is in the allowed set.
    let mut ordered: Vec<Orientation> = Vec::with_capacity(allowed.len());
    if allowed.contains(&current) {
        ordered.push(current);
    }
    for &o in allowed {
        if o != current {
            ordered.push(o);
        }
    }
    for o in ordered {
        let k = key_of(o);
        if !seen.contains(&k) {
            seen.push(k);
            out.push(o);
        }
    }
    out
}
