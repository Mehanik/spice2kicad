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
//! never widens V14) and keeps a candidate ONLY if it *strictly* improves the
//! lexicographic objective `(V13, V12, V5, bends)` without increasing V11
//! residue, symbol-body overlap, or foreign-body (V12) crossings.
//!
//! **Ordering contract for the objective** (`docs/invariants.md` V16): V16
//! bends are the FINAL key and must stay there. They may appear in the
//! acceptance predicate only as that last key or as a non-regression guard
//! alongside `v11`/`overlap`/`v12` — never earlier in the tuple, and never
//! as a weighted term. Last place is what makes the subordination
//! structural: a candidate raising `v12` or `v13` yields a strictly greater
//! tuple however many bends it saves, so bends can never buy a wire through
//! a body or across a label. The quantity must also be the *ink-graph* bend
//! count ([`bend_count`]), never a raw segment count — `cleanup.rs` splits
//! segments for Tier-0 correctness, and a raw count would push against it.

use kicad_symbols::{Library, Orientation, Symbol};
use spice_layout::{Placement, RefinementMeta};

use crate::schematic::{
    LabelObstacles, TextBbox, collect_net_pins, label_rotation_obstacles, label_specs,
    placement_obstacles_with_refdes, placement_property_bboxes, rail_glyph_body_bboxes, text_bbox,
    trial_route,
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
    // Nothing to chase only when BOTH the Tier-1 V12 count and the
    // Tier-2 V5 count are already zero. Returning on `v5 == 0` alone
    // would skip the pass on a placement whose only defect is a wire
    // speared through a body — the higher-tier problem.
    if baseline.v5 == 0 && baseline.v12 == 0 {
        return;
    }

    // Greedy single-element descent first: cheap, each accepted step
    // *strictly* reduces real V5, so it converges in at most `v5` steps.
    greedy_descent(placement, library, meta, &mut baseline);
    if baseline.v5 == 0 && baseline.v12 == 0 {
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
            // Skip elements that cannot currently contribute a V5 or a
            // V12 violation: only those whose own pins are flagged, or
            // whose body a wire currently spears, are worth re-orienting.
            // This bounds the trial-route count without losing any
            // improvable element. V12 offenders must be included — an
            // element can be speared without having any V5 violation of
            // its own, and filtering on V5 alone made those unreachable.
            let refdes = &placement.elements[i].refdes;
            let is_v5_offender = baseline.offenders.iter().any(|v| &v.refdes == refdes);
            let is_v12_offender = baseline.v12_offenders.contains(refdes);
            if !is_v5_offender && !is_v12_offender {
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

                // Accept when the (V13, V12, V5, bends) tuple strictly
                // improves and no equal-/higher-tier guard regresses. V13
                // and V12 are Tier 1, V5 and bends (V16) are Tier 2, so
                // the Tier-1 counts lead: a candidate that removes a
                // label overlap or a wire speared through a body wins
                // even if V5/bends are unchanged, and one that adds
                // either is never taken for a V5 or bend gain. Selection
                // is lexicographic for the same reason.
                //
                // V12 belongs in the objective, not just the guard. It
                // used to appear only as the `m.v12 <= baseline.v12`
                // non-regression check, which meant the search could
                // never *seek* a V12 fix — it only ever landed one as a
                // side effect of a V5 improvement that happened to
                // straighten the same wire. That is a tier inversion:
                // a Tier-1 defect was reachable only while a Tier-2
                // gradient still pointed at it, so any change that
                // flattened V5 first (e.g. a router fix that removes V5
                // violations outright) silently stranded the V12
                // crossing. Keeping the `<=` guard as well means V12 can
                // now only fall, never rise — no sideways trade against
                // V13 within the tier.
                //
                // ORDERING CONTRACT (docs/invariants.md V16). V16 bends
                // are the FINAL key of this tuple and must stay there.
                // They may appear in this predicate in exactly two
                // shapes — the last lexicographic key, or a
                // non-regression guard alongside `v11`/`overlap`/`v12` —
                // and never earlier in the tuple, never as a weighted
                // term. Last-place lexicographic ordering is what makes
                // the subordination structural rather than a matter of
                // tuning: a candidate that raises `v12` or `v13` yields a
                // strictly greater tuple no matter how many bends it
                // saves, and is independently refused by the `<=` guards
                // below. Bends can therefore never buy a wire through a
                // body or across a label. Moving them earlier would make
                // that trade reachable — don't.
                if (m.v13, m.v12, m.v5, m.bends)
                    < (baseline.v13, baseline.v12, baseline.v5, baseline.bends)
                    && m.v11 <= baseline.v11
                    && m.overlap <= baseline.overlap
                    && m.v12 <= baseline.v12
                {
                    let take = match &best {
                        None => true,
                        Some((_, bm)) => {
                            (m.v13, m.v12, m.v5, m.bends) < (bm.v13, bm.v12, bm.v5, bm.bends)
                        }
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
                if baseline.v13 == 0 && baseline.v12 == 0 && baseline.v5 == 0 {
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
    // Truncate BEFORE sorting. `active` is built offenders-first, then
    // neighbours, so truncating in that order keeps the elements the
    // search actually exists to re-orient. Sorting first was a defect:
    // on `common_emitter` the offenders (CIN=6, Q1=8) sorted *after*
    // their six neighbours, so a `MAX_ACTIVE` of 4 cut both offenders
    // out and the joint search enumerated 16 combinations of four
    // elements that had no V5 violation to fix. Sort only afterwards,
    // to keep the enumeration order deterministic.
    active.truncate(MAX_ACTIVE);
    active.sort_unstable();

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
        // Same lexicographic (V13, V12, V5, bends) objective as
        // `greedy_descent` — Tier-1 counts lead, Tier-2 V5 next, V16
        // bends last. See the ordering contract documented on that
        // function's acceptance gate: bends must stay the FINAL key.
        if (m.v13, m.v12, m.v5, m.bends) < (baseline.v13, baseline.v12, baseline.v5, baseline.bends)
            && m.v11 <= baseline.v11
            && m.overlap <= baseline.overlap
            && m.v12 <= baseline.v12
        {
            let take = match &best {
                None => true,
                Some((_, bm)) => (m.v13, m.v12, m.v5, m.bends) < (bm.v13, bm.v12, bm.v5, bm.bends),
            };
            if take {
                // Stop only when NOTHING is left to improve on ANY key,
                // bends included. The old exit fired on
                // (V13, V12, V5) == 0 alone, which returned the
                // lexicographically FIRST zero-cost combination and hid
                // every equally-clean but straighter alternative behind
                // it — on `rc_lowpass_ports` exactly what masked R1 at
                // rot 180 (B = 2) behind rot 0 (B = 4), since rot 0 is
                // enumerated first and already scores a clean zero.
                //
                // Continuing is bounded: the enumeration is already hard
                // capped at `MAX_COMBINATIONS`, so the worst case is
                // unchanged and only the typical case does more work.
                let perfect = m.v13 == 0 && m.v12 == 0 && m.v5 == 0 && m.bends == 0;
                let chosen: Vec<Orientation> = active
                    .iter()
                    .map(|&i| placement.elements[i].orientation)
                    .collect();
                best = Some((chosen, m));
                if perfect {
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
/// violations so the sweep can skip elements that aren't offending, and
/// `v12_offenders` does the same for V12: the refdes of every element
/// whose body a wire currently spears. Without the latter the sweep
/// could only ever fix a V12 crossing on an element that *also* had a
/// V5 violation — see the tier-inversion note on the acceptance gate.
/// `v13` is the combined label↔body + label↔property-text overlap count
/// (V13 parts 1 and 2), measured on the exact labels the emitter will
/// plant ([`label_specs`]).
struct Measure {
    v5: usize,
    v11: usize,
    overlap: usize,
    v12: usize,
    v13: usize,
    /// V16 bend count of the trial route, as the **ink-graph** quantity
    /// (see [`bend_count`]) — never a raw segment or route-corner count.
    /// Always the FINAL key of the acceptance tuple; see the ordering
    /// contract on `greedy_descent`'s gate.
    bends: usize,
    offenders: Vec<Violation>,
    v12_offenders: Vec<String>,
}

/// Trial-route `placement` and measure V5, V11 residue, symbol-body
/// overlap, V12 foreign-body crossings, and V13 label overlaps.
fn measure(placement: &Placement, library: &Library) -> Measure {
    let route = trial_route(placement, library);
    let pins = pin_probes(placement, library);
    let offenders = count_outward_violations(&pins, &route.segments);
    let overlap = symbol_overlap_count(placement, library);
    let (v12, v12_offenders) = v12_crossings(placement, library, &route.segments);
    let v13 = v13_overlap_count(placement, library);
    let bends = bend_count(&route.segments);
    Measure {
        v5: offenders.len(),
        v11: route.v11_count,
        overlap,
        v12,
        v13,
        bends,
        offenders,
        v12_offenders,
    }
}

/// V16 bend count — the L-corners of the emitted **ink**.
///
/// This is deliberately the ink-graph quantity defined in
/// `docs/invariants.md` V16 and computed by its verifier
/// (`spice2kicad/tests/wire_geometry.rs`), NOT a raw segment or
/// route-corner count. Group segments by line, merge
/// touching-or-overlapping collinear spans into **maximal straight
/// runs**, then count vertices carrying exactly two rays of opposite
/// orientation.
///
/// The maximal-run merge is the load-bearing part. `spice_route`'s
/// `cleanup::split_at_interior_attachments` is a **Tier-0 correctness**
/// pass that deliberately INCREASES the segment count (KiCad joins wires
/// only at endpoints), and `coalesce_collinear` /
/// `collapse_collinear_overlaps` re-segment identical ink the other way.
/// A raw count in the acceptance gate would therefore put optimisation
/// pressure against a correctness pass — the metric must be invariant
/// under re-segmentation of identical ink, and merging runs before
/// counting is what makes it so.
fn bend_count(segments: &[crate::v5::WireSegment]) -> usize {
    // Quantise to µm so float noise cannot split a run.
    #[allow(clippy::cast_possible_truncation)]
    let q = |v: f64| (v * 1000.0).round() as i64;

    // (is_vertical, fixed coordinate) -> spans along the free axis.
    let mut lines: std::collections::BTreeMap<(bool, i64), Vec<(i64, i64)>> =
        std::collections::BTreeMap::new();
    for &((x1, y1), (x2, y2)) in segments {
        let (x1, y1, x2, y2) = (q(x1), q(y1), q(x2), q(y2));
        if x1 == x2 && y1 == y2 {
            continue; // degenerate
        }
        if x1 == x2 {
            lines
                .entry((true, x1))
                .or_default()
                .push((y1.min(y2), y1.max(y2)));
        } else if y1 == y2 {
            lines
                .entry((false, y1))
                .or_default()
                .push((x1.min(x2), x1.max(x2)));
        }
        // Diagonals are an outright V16 failure the verifier catches;
        // nothing in the pipeline emits them, so they are ignored here.
    }

    // Merge each line's spans into maximal runs.
    let mut runs: Vec<(bool, i64, i64, i64)> = Vec::new();
    for ((vertical, fixed), mut spans) in lines {
        spans.sort_unstable();
        let (mut lo, mut hi) = spans[0];
        for (a, b) in spans.into_iter().skip(1) {
            if a <= hi {
                hi = hi.max(b); // touching or overlapping — same run
            } else {
                runs.push((vertical, fixed, lo, hi));
                (lo, hi) = (a, b);
            }
        }
        runs.push((vertical, fixed, lo, hi));
    }

    // Candidate vertices: every run endpoint.
    let mut points: Vec<(i64, i64)> = Vec::new();
    for &(vertical, fixed, lo, hi) in &runs {
        for end in [lo, hi] {
            points.push(if vertical { (fixed, end) } else { (end, fixed) });
        }
    }
    points.sort_unstable();
    points.dedup();

    // A bend is a 2-ray vertex with one horizontal and one vertical ray.
    // Rays are counted as `cleanup::rays_at` does: a run ENDING here
    // contributes one, a run whose strict INTERIOR contains it two.
    points
        .into_iter()
        .filter(|&(px, py)| {
            let (mut rays, mut has_v, mut has_h) = (0usize, false, false);
            for &(vertical, fixed, lo, hi) in &runs {
                let (along, across) = if vertical { (py, px) } else { (px, py) };
                if across != fixed || along < lo || along > hi {
                    continue;
                }
                rays += if along > lo && along < hi { 2 } else { 1 };
                if vertical {
                    has_v = true;
                } else {
                    has_h = true;
                }
            }
            rays == 2 && has_v && has_h
        })
        .count()
}

/// Count V13 label overlaps for `placement`: a label's text bbox against
/// (1) any symbol body bbox, or (2) any Reference/Value property bbox.
///
/// This is a deliberate *approximation* of decoration, not a replica of
/// it, and the gap is load-bearing — see the ADR-11 post-mortem in
/// `docs/layout-adr.md`. The gate necessarily scores PRE-nudge property
/// anchors (`placement_property_bboxes`), because the real anchors are
/// chosen later by `nudge_property_text`, which needs the emitted item
/// list this function does not have. It therefore also passes an empty
/// pin-text set and `anchor_search: false`, keeping the whole model
/// consistently one step upstream of decoration.
///
/// Making the label side faithful while the property side stays upstream
/// was measured and is strictly worse: the gate then sees label/property
/// overlaps that decoration goes on to resolve (property text nudges away;
/// labels have four rotations at every pin on their net), and refuses real
/// V5 improvements to avoid those phantoms. common_emitter V5 0 -> 1 and
/// opamp_inverting_real 1 -> 2, with measured V13 already 0 in both cases.
///
/// Closing the gap for real means simulating the whole decoration text
/// pipeline per candidate orientation — route, labels, property nudge,
/// glyph-value nudge. That is feasible (the gate already trial-routes with
/// the real router) but is its own project. Until then, prefer the
/// consistently-upstream model over a half-aligned one.
fn v13_overlap_count(placement: &Placement, library: &Library) -> usize {
    let net_pins = collect_net_pins(placement, library, &[]);
    let props = placement_property_bboxes(placement);
    // Interface-label rotation obstacles, matching the emitter
    // (`emit_root`): host symbol bodies plus foreign rail-glyph bodies,
    // so the refinement gate measures the SAME rotated global-label
    // geometry the final decoration will emit (V13 item 2B).
    let negative_rails = spice_layout::net_class::negative_rail_nets(placement);
    let rail_tags = spice_layout::net_class::rail_tags(placement);
    let glyph_bodies = rail_glyph_body_bboxes(&net_pins, library, &negative_rails, &rail_tags);
    let label_obstacles = label_rotation_obstacles(placement, library, &glyph_bodies);
    // Consistently upstream of decoration — see the doc comment: no
    // pin-text set, no wires, no anchor search.
    let obs = LabelObstacles {
        properties: &props,
        bodies: &label_obstacles,
        pin_texts: &[],
        wires: &[],
    };
    let specs = label_specs(
        &net_pins,
        &[],
        &obs,
        false,
        &std::collections::BTreeMap::new(),
        &rail_tags,
    );
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
fn v12_crossings(
    placement: &Placement,
    library: &Library,
    segments: &[crate::v5::WireSegment],
) -> (usize, Vec<String>) {
    let obstacles = placement_obstacles_with_refdes(placement, library);
    let mut count = 0;
    let mut offenders: Vec<String> = Vec::new();
    for (refdes, bbox) in &obstacles {
        let mut hit = false;
        for (a, b) in segments {
            if bbox.intersects_segment(a.0, a.1, b.0, b.1) {
                count += 1;
                hit = true;
            }
        }
        if hit {
            offenders.push(refdes.clone());
        }
    }
    (count, offenders)
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

#[cfg(test)]
mod tests {
    use super::bend_count;

    /// A plain L has exactly one bend.
    #[test]
    fn single_l_corner_is_one_bend() {
        let segs = [((0.0, 0.0), (0.0, 10.0)), ((0.0, 10.0), (10.0, 10.0))];
        assert_eq!(bend_count(&segs), 1);
    }

    /// A straight run has no bends however many pieces it is cut into.
    #[test]
    fn collinear_run_has_no_bends() {
        let whole = [((0.0, 0.0), (30.0, 0.0))];
        let split = [
            ((0.0, 0.0), (10.0, 0.0)),
            ((10.0, 0.0), (20.0, 0.0)),
            ((20.0, 0.0), (30.0, 0.0)),
        ];
        assert_eq!(bend_count(&whole), 0);
        assert_eq!(bend_count(&split), 0);
    }

    /// The metric is invariant under re-segmentation of identical ink.
    ///
    /// This is the property that lets `bends` sit in phase 4.5's
    /// acceptance gate: `spice_route::cleanup::split_at_interior_attachments`
    /// is a Tier-0 correctness pass that deliberately splits runs at
    /// same-net attachment points. A raw segment/corner count would move
    /// when it runs, creating optimisation pressure against correctness.
    /// The ink-graph count does not move.
    #[test]
    fn bend_count_is_invariant_under_resegmentation() {
        // A 2-bend staple drawn as 3 maximal runs.
        let coarse = [
            ((0.0, 0.0), (0.0, 10.0)),
            ((0.0, 10.0), (20.0, 10.0)),
            ((20.0, 10.0), (20.0, 0.0)),
        ];
        // Identical ink, every run chopped at interior points.
        let fine = [
            ((0.0, 0.0), (0.0, 4.0)),
            ((0.0, 4.0), (0.0, 10.0)),
            ((0.0, 10.0), (7.0, 10.0)),
            ((7.0, 10.0), (13.0, 10.0)),
            ((13.0, 10.0), (20.0, 10.0)),
            ((20.0, 10.0), (20.0, 6.0)),
            ((20.0, 6.0), (20.0, 0.0)),
        ];
        assert_eq!(bend_count(&coarse), 2);
        assert_eq!(bend_count(&fine), 2, "re-segmentation must not change B");
    }

    /// Overlapping duplicate ink merges into one run, adding no bends.
    #[test]
    fn overlapping_collinear_spans_merge() {
        let segs = [
            ((0.0, 0.0), (10.0, 0.0)),
            ((5.0, 0.0), (15.0, 0.0)), // overlaps the first
            ((15.0, 0.0), (15.0, 5.0)),
        ];
        assert_eq!(bend_count(&segs), 1);
    }

    /// A T-junction is a branch, not a bend: 3 rays, so it is not counted.
    #[test]
    fn t_junction_is_not_a_bend() {
        let segs = [
            ((0.0, 0.0), (20.0, 0.0)),  // trunk
            ((10.0, 0.0), (10.0, 8.0)), // stub off its interior
        ];
        assert_eq!(bend_count(&segs), 0);
    }

    /// A 4-ray crossing is not a bend either.
    #[test]
    fn crossing_is_not_a_bend() {
        let segs = [((0.0, 5.0), (20.0, 5.0)), ((10.0, 0.0), (10.0, 10.0))];
        assert_eq!(bend_count(&segs), 0);
    }
}
