//! Stage 3 — resolve cross-net endpoint conflicts.
//!
//! When two distinct nets' Steiner trees emit segments whose endpoints
//! land on the same coordinate, KiCad treats those nets as
//! electrically merged — a silent short. The simple v0.1 fix:
//!
//! 1. Walk every endpoint coordinate across every routed net.
//! 2. If a coordinate carries endpoints from ≥ 2 distinct nets, jog
//!    one of the colliding nets' affected endpoints by exactly one
//!    grid cell (1.27 mm) along the axis that doesn't disturb its
//!    other endpoint.
//! 3. Repeat until no conflicts remain or a derived per-instance
//!    convergence bound (one pass per routed net, + 1) elapses.
//!
//! This is *not* full Stage 3 rip-up & retry from the original spec —
//! that lands later. The jog-once loop is sufficient for the small
//! v0.1 fixtures.

use crate::types::{Bbox, Direction, RoutedNet, Segment};

const GRID_MM: f64 = 1.27;
const EPS: f64 = 1e-6;

/// Bend penalty for the Lee/BFS maze router, in millimetres.
///
/// A path's Dijkstra cost is `length + bends · MAZE_BEND_PENALTY_MM`.
/// The penalty is **strictly less than one grid step** (`GRID_MM`) so
/// that total length always strictly dominates bend count: the router
/// will never accept even one extra grid cell of wire to remove a
/// bend, but among paths of *equal* length it prefers the one with
/// fewer bends. Half a grid cell gives a clean tie-break margin
/// (any length difference is a whole number of cells, ≥ `GRID_MM`,
/// while the maximum bend-count swing over a fixed-length path is
/// bounded and each bend is worth only `GRID_MM / 2`, so length wins).
/// This is the textbook rectilinear length-then-bends ordering, not a
/// tunable weight; raising it past `GRID_MM` would let the router
/// lengthen wires (a V5/V10 regression).
const MAZE_BEND_PENALTY_MM: f64 = GRID_MM / 2.0;
/// Safety backstop on the maze router's grid size, in cells.
///
/// The grid is `cols · rows`; each cell costs a `bool` in three block
/// vectors plus, in the search, a `u64` cost and an `Option<(usize,
/// u8)>` parent per (cell × 5 directions) state — roughly 5 · (8 + 16)
/// = 120 bytes/cell. At the cap below the search allocates on the order
/// of `120 · 250_000 ≈ 30 MB`, which stays inside the per-test 4 GiB
/// vsz ulimit with wide margin while bounding a single maze call's
/// memory and the O(V log V) Dijkstra to a fixed worst case. 500 × 500
/// cells is a 635 × 635 mm sheet — larger than any single KiCad A-series
/// sheet — so a problem exceeding it is not a single sheet and the maze
/// pass correctly bails rather than routing across a torn layout.
const MAZE_CELL_CAP: usize = 500 * 500;

/// Largest perpendicular detour, in grid cells, that the U-detour /
/// L-pair retry loops will attempt for a given obstacle/foreign-pin
/// set. A rectilinear detour never needs to swing wider than the
/// blocking geometry's own extent plus one clearance cell on the far
/// side: past that the segment is already clear of every box. We
/// therefore derive the cap from the union extent of the boxes the
/// segment must avoid (in cells), rather than guessing a fixed slack.
/// `boxes` is whatever set the caller is routing around (obstacles or
/// inflated foreign-pin bboxes); an empty set yields a cap of 1 (a
/// single perpendicular nudge is always enough when nothing blocks).
fn max_detour_cells(boxes: &[Bbox]) -> usize {
    let mut lo_x = f64::INFINITY;
    let mut hi_x = f64::NEG_INFINITY;
    let mut lo_y = f64::INFINITY;
    let mut hi_y = f64::NEG_INFINITY;
    for b in boxes {
        lo_x = lo_x.min(b.x0);
        hi_x = hi_x.max(b.x1);
        lo_y = lo_y.min(b.y0);
        hi_y = hi_y.max(b.y1);
    }
    if !lo_x.is_finite() || !hi_x.is_finite() {
        return 1;
    }
    let span_mm = (hi_x - lo_x).max(hi_y - lo_y).max(0.0);
    // Cells spanned by the widest box dimension, + 1 clearance cell.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cells = (span_mm / GRID_MM).ceil() as usize;
    cells.saturating_add(1).max(1)
}

/// Resolve cross-net endpoint conflicts in place.
///
/// `pin_coords` is the union of pin coordinates across all nets,
/// quantised. Endpoints landing on a pin coord are never jogged
/// (jogging away from a pin would silently disconnect that pin).
/// When the only candidates at a conflict point are pin endpoints,
/// the conflict is recorded as a warning and left alone — that case
/// is a genuine pin-on-pin overlap that needs placer-level
/// attention, not router-level.
///
/// Returns one warning per net that still has unresolved conflicts
/// after the derived per-instance convergence bound (one jog pass per
/// routed net, + 1) elapses.
pub fn resolve_conflicts<S: ::std::hash::BuildHasher>(
    routed: &mut [RoutedNet],
    net_pin_coords: &[std::collections::HashSet<(i64, i64), S>],
) -> Vec<String> {
    let net_pins = net_pin_coords;
    let mut warnings = Vec::new();
    // Convergence bound. The real exit is `conflicts.is_empty()` (or
    // `!acted`); this cap only bounds a pathological non-converging
    // case. Each pass that does work jogs at least one net off a
    // contested coord, and a conflict chain settles in at most one pass
    // per participating net, so the count of routed nets (+1 for the
    // confirming no-conflict pass) is a real dependency-depth bound —
    // the same derivation `avoid_obstacles` / `avoid_foreign_pins`
    // use, not arbitrary slack.
    let max_iterations = routed.len().saturating_add(1).max(1);
    for _ in 0..max_iterations {
        let conflicts = find_conflicts(routed);
        if conflicts.is_empty() {
            return warnings;
        }
        let mut acted = false;
        for (point, nets) in &conflicts {
            if nets.len() < 2 {
                continue;
            }
            // Pick a victim net to jog: prefer one for which `point`
            // is *not* a pin endpoint (so jogging away doesn't
            // disconnect a pin). If every candidate carries a pin
            // there, leave it alone — that's a placer-level
            // pin-on-pin conflict, not a router one.
            let victim_opt = nets
                .iter()
                .find(|&&i| !net_pins.get(i).is_some_and(|s| s.contains(point)))
                .copied();
            let Some(victim) = victim_opt else {
                continue;
            };
            jog_endpoint_at(&mut routed[victim], *point);
            acted = true;
        }
        if !acted {
            break;
        }
    }
    // Still-conflicting nets after the cap.
    let final_conflicts = find_conflicts(routed);
    let mut bad: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    for (_, nets) in &final_conflicts {
        for n in nets {
            bad.insert(*n);
        }
    }
    for n in bad {
        warnings.push(format!(
            "conflict: net index {n} has endpoint conflicts left after {max_iterations} resolve iterations"
        ));
    }
    warnings
}

/// Return one entry per coordinate that carries endpoints from ≥ 2
/// distinct routed-net indices.
fn find_conflicts(routed: &[RoutedNet]) -> Vec<((i64, i64), Vec<usize>)> {
    use std::collections::HashMap;
    let mut by_point: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, net) in routed.iter().enumerate() {
        let mut seen: std::collections::HashSet<(i64, i64)> = std::collections::HashSet::new();
        for s in &net.segments {
            for (x, y) in [(s.x1, s.y1), (s.x2, s.y2)] {
                let k = key(x, y);
                if seen.insert(k) {
                    by_point.entry(k).or_default().push(i);
                }
            }
        }
    }
    by_point.into_iter().filter(|(_, v)| v.len() >= 2).collect()
}

#[allow(clippy::cast_possible_truncation)]
fn key(x: f64, y: f64) -> (i64, i64) {
    ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64)
}

/// Quantised pin-coord → outward-direction lookup, built by the
/// router once per request.
type PinOutwardMap = std::collections::HashMap<(i64, i64), Direction>;

/// True iff `corner` is the L-corner placement that lets the leg
/// incident on `pin` (running `pin → corner`) extend in `pin`'s
/// outward direction. Returns false when `pin` is not in
/// `pin_outward` (treat as unconstrained).
fn corner_satisfies_outward(
    pin: (f64, f64),
    corner: (f64, f64),
    pin_outward: &PinOutwardMap,
) -> bool {
    let Some(&dir) = pin_outward.get(&key(pin.0, pin.1)) else {
        return false;
    };
    let dx = corner.0 - pin.0;
    let dy = corner.1 - pin.1;
    match dir {
        Direction::Up => dy < -EPS && dx.abs() < EPS,
        Direction::Down => dy > EPS && dx.abs() < EPS,
        Direction::Left => dx < -EPS && dy.abs() < EPS,
        Direction::Right => dx > EPS && dy.abs() < EPS,
    }
}

/// Jog a single endpoint of `net` that touches `point` by one grid
/// cell on the axis perpendicular to its segment, preserving wire
/// orthogonality. The original segment is replaced by an L: a one-cell
/// perpendicular stub from the new (jogged) coord back to the segment
/// axis, then the original segment continued from that axis to its
/// peer endpoint. The conflict point itself is no longer an endpoint
/// of any wire on this net, electrically separating it from the other
/// net touching the same coord.
///
/// Earlier versions of this function moved the endpoint perpendicular
/// in place, producing a single non-orthogonal segment from the moved
/// endpoint to the unmoved peer. That violated the "all wires are
/// axis-aligned" invariant (see verifier in `tests/orthogonality.rs`).
fn jog_endpoint_at(net: &mut RoutedNet, point: (i64, i64)) {
    let target_idx = net
        .segments
        .iter()
        .position(|s| key(s.x1, s.y1) == point || key(s.x2, s.y2) == point);
    let Some(idx) = target_idx else {
        return;
    };
    let s = net.segments[idx];
    let at_start = key(s.x1, s.y1) == point;
    let (px, py, qx, qy) = if at_start {
        (s.x1, s.y1, s.x2, s.y2)
    } else {
        (s.x2, s.y2, s.x1, s.y1)
    };
    // Replace the original segment with an orthogonal L:
    //
    //   horizontal segment (py == qy):  endpoint moves to (px, py±g);
    //     stub: (px, py±g) → (px+sign·g, py±g)
    //     main: (px+sign·g, py±g) → (qx, qy)?  — actually the cleanest
    //     decomposition is:
    //       stub vertical: (px, py±g)        → (px, py)
    //       continuation:  (px, py)          → (qx, qy)   [unchanged]
    //     but that leaves (px, py) as an endpoint, re-creating the
    //     conflict. Instead bend perpendicular AT the new coord and
    //     continue parallel:
    //       stub:        (px,    py±g) → (qx, py±g)
    //       continuation:(qx,    py±g) → (qx, qy)
    //     Both segments are axis-aligned and (px, py) is no longer an
    //     endpoint on this net.
    let horizontal = (py - qy).abs() < EPS;
    let (jx, jy) = if horizontal {
        (px, py + GRID_MM)
    } else {
        (px + GRID_MM, py)
    };
    let (stub, cont) = if horizontal {
        (
            Segment {
                x1: jx,
                y1: jy,
                x2: qx,
                y2: jy,
            },
            Segment {
                x1: qx,
                y1: jy,
                x2: qx,
                y2: qy,
            },
        )
    } else {
        (
            Segment {
                x1: jx,
                y1: jy,
                x2: jx,
                y2: qy,
            },
            Segment {
                x1: jx,
                y1: qy,
                x2: qx,
                y2: qy,
            },
        )
    };
    net.segments[idx] = stub;
    // Skip pushing a zero-length continuation (happens when the
    // original segment's far endpoint already coincides with the
    // jog axis).
    if !approx_zero_len(&cont) {
        net.segments.push(cont);
    }
    let _ = std::marker::PhantomData::<Segment>;
}

fn approx_zero_len(s: &Segment) -> bool {
    (s.x1 - s.x2).abs() < EPS && (s.y1 - s.y2).abs() < EPS
}

/// V11 — flag and resolve segments that touch a pin owned by a
/// different net. KiCad's connectivity engine merges geometric
/// coincidence into electrical connection without any junction
/// marker, so a wire endpoint, wire interior, or label coincident
/// with a foreign pin silently shorts the two nets.
///
/// `foreign_per_net[i]` is the pre-computed set of pin coordinates
/// owned by *some other* net (signal, power, or ground) that this
/// routed net (`routed[i]`) must avoid touching. The caller is
/// responsible for excluding `routed[i]`'s own pins from this set —
/// the function does not re-derive ownership.
///
/// For each routed net:
/// 1. For every segment whose endpoint lands on a foreign-pin
///    coord, jog the endpoint one grid cell perpendicular (reusing
///    [`jog_endpoint_at`]).
/// 2. For every segment whose **interior** crosses a foreign-pin
///    coord (axis-parallel segment whose path contains the pin),
///    insert a one-cell-tall perpendicular detour around the pin
///    (a 3-segment U).
/// 3. Repeat until convergence or the iteration cap.
///
/// Returns one warning per net that still has unresolved foreign-pin
/// coincidences after the cap.
pub fn avoid_foreign_pins<S: ::std::hash::BuildHasher>(
    routed: &mut [RoutedNet],
    foreign_per_net: &[std::collections::HashSet<(i64, i64), S>],
    own_pin_coords: &[std::collections::HashSet<(i64, i64), S>],
    obstacles: &[Bbox],
    pin_outward: &PinOutwardMap,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if routed.is_empty() || foreign_per_net.is_empty() {
        return warnings;
    }
    // Caller has already excluded each routed net's own pins from its
    // foreign set. Sort+dedup once into Vec form for the inner pass.
    let foreign: Vec<Vec<(i64, i64)>> = foreign_per_net
        .iter()
        .map(|s| {
            let mut v: Vec<(i64, i64)> = s.iter().copied().collect();
            v.sort_unstable();
            v
        })
        .collect();
    // Process nets in a deterministic priority order so the most
    // constrained net (most pins, largest pin span) routes first and
    // less-constrained nets get to react to its geometry. Ties broken
    // by net index so order is stable.
    let mut order: Vec<usize> = (0..routed.len()).collect();
    order.sort_by(|&a, &b| {
        let key_a = net_priority_key(&routed[a]);
        let key_b = net_priority_key(&routed[b]);
        key_b.cmp(&key_a).then(a.cmp(&b))
    });
    // Iterate the priority pass until convergence (no further net
    // changes). Cross-net dependencies — net A's detour blocked
    // because net B's pre-detour trunk collinearly overlaps — resolve
    // themselves once B has moved on a later pass. The real exit is the
    // `!changed` fixed-point check below; the cap only bounds the
    // pathological non-converging (oscillating) case. A settling
    // dependency chain across `n` mutually-constraining nets reaches a
    // fixed point in at most `n` passes, so the bound is the count of
    // nets that actually have a foreign pin to avoid (+1 for the
    // confirming no-change pass) — a real dependency-depth bound, the
    // same derivation `avoid_obstacles` uses, not arbitrary slack.
    let outer_cap = foreign
        .iter()
        .filter(|pins| !pins.is_empty())
        .count()
        .saturating_add(1)
        .max(1);
    for _ in 0..outer_cap {
        let pre_signatures: Vec<Vec<Segment>> = routed.iter().map(|n| n.segments.clone()).collect();
        for &i in &order {
            let pins = &foreign[i];
            if pins.is_empty() {
                continue;
            }
            let own_for_net: &std::collections::HashSet<(i64, i64), S> = match own_pin_coords.get(i)
            {
                Some(s) => s,
                None => continue,
            };
            reroute_one_net_v11(routed, i, pins, own_for_net, obstacles, pin_outward);
        }
        let changed = pre_signatures
            .iter()
            .zip(routed.iter())
            .any(|(pre, now)| pre != &now.segments);
        if !changed {
            break;
        }
    }
    // Final tally — anything left after active rerouting is reported
    // as a diagnostic. Two flavours:
    //   * `v11:` — router-level failure. The emitter (kicad-emitter)
    //     promotes this to a hard EmitError so the CLI exits nonzero
    //     rather than write a schematic it knows is electrically
    //     wrong.
    //   * `v11-placer:` — the foreign-pin coord coincides with one
    //     of the routed net's OWN pin coords, i.e. two distinct nets
    //     occupy the same world point before the router ever ran.
    //     No detour can fix that — any wire connecting the own pin
    //     necessarily lands at the shared coord. The emitter logs
    //     these as warnings only; closing them is a placer-level
    //     work item tracked by
    //     `v11_pin_overlap_is_a_placer_bug` in the verifier.
    for (i, net) in routed.iter().enumerate() {
        let pins = &foreign[i];
        if pins.is_empty() {
            continue;
        }
        let endpoints = collect_endpoint_hits(net, pins);
        let interior = count_interior_hits(net, pins);
        if !endpoints.is_empty() || interior > 0 {
            warnings.push(format!(
                "v11: net index {i} has {} endpoint and {interior} interior foreign-pin coincidences left after active rerouting",
                endpoints.len()
            ));
        }
    }
    warnings
}

/// Priority key for V11 reroute scheduling: nets that touch more
/// distinct coords (endpoints) and span a larger bbox are tackled
/// first. The values are integers (µm) so `Ord` is well-defined.
fn net_priority_key(net: &RoutedNet) -> (usize, i64) {
    use std::collections::HashSet;
    let mut coords: HashSet<(i64, i64)> = HashSet::new();
    let mut lo_x = i64::MAX;
    let mut hi_x = i64::MIN;
    let mut lo_y = i64::MAX;
    let mut hi_y = i64::MIN;
    for s in &net.segments {
        for (x, y) in [(s.x1, s.y1), (s.x2, s.y2)] {
            let k = key(x, y);
            coords.insert(k);
            lo_x = lo_x.min(k.0);
            hi_x = hi_x.max(k.0);
            lo_y = lo_y.min(k.1);
            hi_y = hi_y.max(k.1);
        }
    }
    let span = if coords.is_empty() {
        0
    } else {
        (hi_x - lo_x) + (hi_y - lo_y)
    };
    (coords.len(), span)
}

/// Reroute every offending segment of `routed[target]` so its wires
/// no longer touch any of `foreign_pins`. Strategy:
///   * **Endpoint hits** — jog the endpoint one grid cell
///     perpendicular ([`jog_endpoint_at`]), then verify the new
///     segments don't crash into a sibling net's existing trunk.
///   * **Interior hits** — replace the offending segment with a
///     three-segment U-detour at offsets `±k·GRID_MM` for
///     `k ∈ 1..=4`, sign and direction picked so all three parts
///     avoid every foreign-pin bbox AND no part collinearly overlaps
///     a sibling routed net (rolling back to the original segment if
///     no fit is found).
///
/// `foreign_pins` is the quantised pin-coord vector the caller has
/// already excluded `target`'s own pins from. `own_pins` is the
/// quantised pin-coord set used to gate jogs that would orphan one
/// of `target`'s own pins.
fn reroute_one_net_v11<S: ::std::hash::BuildHasher>(
    routed: &mut [RoutedNet],
    target: usize,
    foreign_pins: &[(i64, i64)],
    own_pins: &std::collections::HashSet<(i64, i64), S>,
    obstacles: &[Bbox],
    pin_outward: &PinOutwardMap,
) {
    #[allow(clippy::cast_precision_loss)]
    let bboxes: Vec<Bbox> = foreign_pins
        .iter()
        .map(|&(x, y)| Bbox::from_point(x as f64 / 1000.0, y as f64 / 1000.0))
        .collect();

    // Phase 1: endpoint hits. Jog each offending endpoint in place,
    // roll back if the jog creates a sibling-trunk overlap.
    let endpoints = collect_endpoint_hits(&routed[target], foreign_pins);
    for ep in endpoints {
        // Don't jog an endpoint that's actually one of `target`'s own
        // pins — that's a placer-level pin-on-pin overlap, not a
        // router bug, and jogging would disconnect the pin.
        if own_pins.contains(&ep) {
            continue;
        }
        let pre = routed[target].clone();
        let pre_seg_set: std::collections::HashSet<(i64, i64, i64, i64)> =
            pre.segments.iter().map(seg_key).collect();
        jog_endpoint_at(&mut routed[target], ep);
        let new_parts: Vec<Segment> = routed[target]
            .segments
            .iter()
            .filter(|s| !pre_seg_set.contains(&seg_key(s)))
            .copied()
            .collect();
        let new_overlap = new_parts
            .iter()
            .any(|p| part_overlaps_sibling(routed, target, p));
        let new_obstacle = new_parts.iter().any(|p| crosses_any_bbox(p, obstacles));
        if new_overlap || new_obstacle || segment_crosses_foreign(&routed[target], &bboxes) {
            routed[target] = pre;
        }
    }

    // Phase 2: interior hits. For each offending segment try
    //   (a) swap the L corner of any L-pair the offender takes part
    //       in — useful when the offender's non-pin endpoint is a
    //       Steiner / L corner whose alternate placement clears the
    //       foreign pin;
    //   (b) fall back to a 3-segment U-detour around the segment
    //       itself, walking sign × offset combinations until one is
    //       V11-clean and doesn't collinearly overlap a sibling
    //       routed net's segment.
    // Rebuild the work-list each pass because replacing segments
    // shuffles indices.
    let mut guard = 0;
    loop {
        guard += 1;
        if guard > 64 {
            break;
        }
        let Some(idx) = find_interior_offender(&routed[target], &bboxes) else {
            break;
        };
        if try_alt_l_corner(
            routed,
            target,
            idx,
            &bboxes,
            obstacles,
            own_pins,
            pin_outward,
        ) {
            continue;
        }
        if try_u_detour_l_pair(
            routed,
            target,
            idx,
            &bboxes,
            obstacles,
            own_pins,
            pin_outward,
        ) {
            continue;
        }
        if !try_detour_segment(routed, target, idx, &bboxes, obstacles) {
            // Move on so any sibling V11 cases still get a chance
            // in this outer pass. The unfixed segment trips the
            // residual-diagnostic tally.
            break;
        }
    }

    // Anchor every own pin that now appears at a segment endpoint
    // with a junction. Stage 4 cleanup honours `is_junction` and
    // refuses to coalesce across a junction-marked coord — without
    // this anchor, two collinear segments meeting at the pin would
    // be merged into a single span, leaving the pin as a mere
    // interior coincidence (which `kicad-cli` does NOT count as
    // electrical connection at netlist-export time, even though
    // KiCad's interactive ERC does).
    anchor_own_pin_endpoints(routed, target, own_pins);
}

/// For every own-pin coord that currently sits at a segment endpoint
/// of `routed[target]`, ensure it is in `routed[target].junctions`.
/// Idempotent.
fn anchor_own_pin_endpoints<S: ::std::hash::BuildHasher>(
    routed: &mut [RoutedNet],
    target: usize,
    own_pins: &std::collections::HashSet<(i64, i64), S>,
) {
    let mut existing: std::collections::HashSet<(i64, i64)> = routed[target]
        .junctions
        .iter()
        .map(|&(x, y)| key(x, y))
        .collect();
    let mut new_pts: Vec<(f64, f64)> = Vec::new();
    for s in &routed[target].segments {
        for (x, y) in [(s.x1, s.y1), (s.x2, s.y2)] {
            let k = key(x, y);
            if own_pins.contains(&k) && !existing.contains(&k) {
                existing.insert(k);
                new_pts.push((x, y));
            }
        }
    }
    routed[target].junctions.extend(new_pts);
}

/// Try to replace an L-pair containing the offending segment with a
/// 3-segment U-detour anchored at the L pair's two far endpoints
/// (which are typically pins and must stay put). The detour walks
/// the intermediate corner offset `k ∈ 1..=max_detour_cells(…)` along
/// the axis perpendicular to the far-endpoint span, in both sign
/// directions, taking the first variant that is V11-clean against
/// every foreign-pin bbox AND doesn't collinearly overlap a sibling
/// routed net.
///
/// Distinct from [`try_alt_l_corner`] (which keeps two segments and
/// just relocates the corner): this function replaces the L-pair
/// with three segments, gaining freedom to route around foreign
/// pins that lie on both candidate L corners — the diff_pair case
/// where Q1.C sits directly above VCC.+ and RTAIL.1 sits directly
/// to the left of c1's RC1.2 pin.
fn try_u_detour_l_pair<S: ::std::hash::BuildHasher>(
    routed: &mut [RoutedNet],
    target: usize,
    idx: usize,
    foreign_bboxes: &[Bbox],
    obstacle_bboxes: &[Bbox],
    own_pins: &std::collections::HashSet<(i64, i64), S>,
    pin_outward: &PinOutwardMap,
) -> bool {
    let n = routed[target].segments.len();
    // Widest perpendicular swing this detour needs: bounded by the
    // extent of the geometry it must clear (foreign pins + obstacle
    // bodies). Past that the detour is already outside every box.
    let retry_cap = max_detour_cells(foreign_bboxes).max(max_detour_cells(obstacle_bboxes));
    for outward_strict in [true, false] {
        for j in 0..n {
            if j == idx {
                continue;
            }
            let a = routed[target].segments[idx];
            let b = routed[target].segments[j];
            let Some((p_far, q_far, corner)) = l_pair_endpoints(&a, &b) else {
                continue;
            };
            // The corner doubling as an own pin must stay anchored —
            // the U-detour skips that coord entirely, which would
            // orphan the pin from the new path.
            if own_pins.contains(&key(corner.0, corner.1)) {
                continue;
            }
            // T-junction at the corner means a third leg of the net
            // attaches there. Replacing the L pair would orphan that
            // leg from the rest of the tree.
            if corner_degree(&routed[target], corner) > 2 {
                continue;
            }
            // Cardinal axis of the connecting span: U detour offsets the
            // *minor* coord (the one that differs between p_far and
            // q_far in the non-original-L direction). For an L between
            // (px,py) and (qx,qy) we can try a U at either x = px + k·g
            // (running parallel to original vertical leg) or y = py + k·g
            // (running parallel to original horizontal leg). Both axes
            // are tried.
            for axis in [Axis::HorizontalFirst, Axis::VerticalFirst] {
                for k in 1..=retry_cap {
                    for sign in [1.0_f64, -1.0_f64] {
                        #[allow(clippy::cast_precision_loss)]
                        let off = sign * GRID_MM * (k as f64);
                        let (mid1, mid2) = match axis {
                            Axis::HorizontalFirst => {
                                ((p_far.0, p_far.1 + off), (q_far.0, p_far.1 + off))
                            }
                            Axis::VerticalFirst => {
                                ((p_far.0 + off, p_far.1), (p_far.0 + off, q_far.1))
                            }
                        };
                        let parts = [
                            Segment {
                                x1: p_far.0,
                                y1: p_far.1,
                                x2: mid1.0,
                                y2: mid1.1,
                            },
                            Segment {
                                x1: mid1.0,
                                y1: mid1.1,
                                x2: mid2.0,
                                y2: mid2.1,
                            },
                            Segment {
                                x1: mid2.0,
                                y1: mid2.1,
                                x2: q_far.0,
                                y2: q_far.1,
                            },
                        ];
                        if parts.iter().any(approx_zero_len) {
                            continue;
                        }
                        if parts.iter().any(|p| crosses_any_bbox(p, foreign_bboxes)) {
                            continue;
                        }
                        if parts.iter().any(|p| crosses_any_bbox(p, obstacle_bboxes)) {
                            continue;
                        }
                        if parts
                            .iter()
                            .any(|p| part_overlaps_sibling(routed, target, p))
                        {
                            continue;
                        }
                        // Outward filter (V5): the legs incident on the
                        // pin endpoints `p_far` and `q_far` are
                        // `pin → mid1` and `pin → mid2` respectively.
                        if outward_strict {
                            let p_is_pin = pin_outward.contains_key(&key(p_far.0, p_far.1));
                            let q_is_pin = pin_outward.contains_key(&key(q_far.0, q_far.1));
                            if p_is_pin && !corner_satisfies_outward(p_far, mid1, pin_outward) {
                                continue;
                            }
                            if q_is_pin && !corner_satisfies_outward(q_far, mid2, pin_outward) {
                                continue;
                            }
                        }
                        // Install: drop both original L-pair segments,
                        // append the three new parts.
                        let (lo, hi) = if idx < j { (idx, j) } else { (j, idx) };
                        routed[target].segments.remove(hi);
                        routed[target].segments.remove(lo);
                        for p in parts {
                            routed[target].segments.push(p);
                        }
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Which axis to bend along first when expanding an L-pair into a
/// 3-segment U.
#[derive(Clone, Copy)]
enum Axis {
    HorizontalFirst,
    VerticalFirst,
}

/// Try to swap the L corner of any L-pair containing
/// `routed[target].segments[idx]` to a corner that avoids every
/// foreign-pin bbox and doesn't collinearly overlap a sibling net.
/// The far endpoint of the offending segment may be a pin (which
/// we keep fixed); the corner endpoint must be either a non-pin
/// Steiner bend or, when it is one of `target`'s own pins, we
/// leave it alone. Returns true if a swap was installed.
fn try_alt_l_corner<S: ::std::hash::BuildHasher>(
    routed: &mut [RoutedNet],
    target: usize,
    idx: usize,
    foreign_bboxes: &[Bbox],
    obstacle_bboxes: &[Bbox],
    own_pins: &std::collections::HashSet<(i64, i64), S>,
    pin_outward: &PinOutwardMap,
) -> bool {
    let n = routed[target].segments.len();
    // First pass: prefer candidates whose installed L-pair's leg
    // incident on each pin endpoint extends in the pin's outward
    // direction. Second pass: relax that constraint so we still
    // resolve the V11/V12 violation even when no outward-clean
    // alternative exists.
    for outward_strict in [true, false] {
        for j in 0..n {
            if j == idx {
                continue;
            }
            let a = routed[target].segments[idx];
            let b = routed[target].segments[j];
            let Some((p_far, q_far, corner)) = l_pair_endpoints(&a, &b) else {
                continue;
            };
            // If the corner is an own pin we cannot move it without
            // orphaning that pin from the net.
            if own_pins.contains(&key(corner.0, corner.1)) {
                continue;
            }
            // A T-junction corner (≥ 3 segments meet) carries a third
            // leg that would be orphaned if we swapped the L pair only.
            if corner_degree(&routed[target], corner) > 2 {
                continue;
            }
            // Alt corners to try.
            let alt1 = (p_far.0, q_far.1);
            let alt2 = (q_far.0, p_far.1);
            for alt in [alt1, alt2] {
                // Skip the corner we already have.
                if (alt.0 - corner.0).abs() < EPS && (alt.1 - corner.1).abs() < EPS {
                    continue;
                }
                let s1 = Segment {
                    x1: p_far.0,
                    y1: p_far.1,
                    x2: alt.0,
                    y2: alt.1,
                };
                let s2 = Segment {
                    x1: alt.0,
                    y1: alt.1,
                    x2: q_far.0,
                    y2: q_far.1,
                };
                if approx_zero_len(&s1) || approx_zero_len(&s2) {
                    continue;
                }
                if [s1, s2].iter().any(|p| crosses_any_bbox(p, foreign_bboxes)) {
                    continue;
                }
                if [s1, s2]
                    .iter()
                    .any(|p| crosses_any_bbox(p, obstacle_bboxes))
                {
                    continue;
                }
                if part_overlaps_sibling(routed, target, &s1)
                    || part_overlaps_sibling(routed, target, &s2)
                {
                    continue;
                }
                // Outward-direction filter (V5): when `outward_strict` is
                // set, require that the corner placement honour every
                // pin-endpoint's outward direction. A pin endpoint here is
                // `p_far` or `q_far` — its incident leg in the new L is
                // `pin → alt`.
                if outward_strict {
                    let p_is_pin = pin_outward.contains_key(&key(p_far.0, p_far.1));
                    let q_is_pin = pin_outward.contains_key(&key(q_far.0, q_far.1));
                    if p_is_pin && !corner_satisfies_outward(p_far, alt, pin_outward) {
                        continue;
                    }
                    if q_is_pin && !corner_satisfies_outward(q_far, alt, pin_outward) {
                        continue;
                    }
                }
                // Install: replace both segments. Drop the higher index
                // first so the lower index stays valid.
                let (lo, hi) = if idx < j { (idx, j) } else { (j, idx) };
                routed[target].segments.remove(hi);
                routed[target].segments.remove(lo);
                routed[target].segments.push(s1);
                routed[target].segments.push(s2);
                return true;
            }
        }
    }
    false
}

/// True iff `seg` strictly enters the interior of any of `bboxes`.
fn crosses_any_bbox(seg: &Segment, bboxes: &[Bbox]) -> bool {
    bboxes
        .iter()
        .any(|b| b.intersects_segment(seg.x1, seg.y1, seg.x2, seg.y2))
}

/// First-segment-index whose axis-parallel interior strictly crosses
/// one of `bboxes` (the inflated foreign-pin set).
fn find_interior_offender(net: &RoutedNet, bboxes: &[Bbox]) -> Option<usize> {
    for (i, s) in net.segments.iter().enumerate() {
        for b in bboxes {
            if b.intersects_segment(s.x1, s.y1, s.x2, s.y2) {
                return Some(i);
            }
        }
    }
    None
}

/// True iff any segment of `net` strictly crosses any of `bboxes`.
fn segment_crosses_foreign(net: &RoutedNet, bboxes: &[Bbox]) -> bool {
    find_interior_offender(net, bboxes).is_some()
}

/// Try to replace `routed[target].segments[idx]` with a U-detour
/// that clears every foreign-pin bbox AND does not collinearly
/// overlap any sibling routed net's segment. Returns `true` if a
/// detour was installed, `false` if no candidate fit.
fn try_detour_segment(
    routed: &mut [RoutedNet],
    target: usize,
    idx: usize,
    foreign_bboxes: &[Bbox],
    obstacle_bboxes: &[Bbox],
) -> bool {
    let s = routed[target].segments[idx];
    let horizontal = (s.y1 - s.y2).abs() < EPS;
    let vertical = (s.x1 - s.x2).abs() < EPS;
    if !horizontal && !vertical {
        return false;
    }
    // Widest perpendicular offset worth trying: bounded by the extent
    // of the geometry the detour must clear; beyond it the detour is
    // already clear of every box.
    let retry_cap = max_detour_cells(foreign_bboxes).max(max_detour_cells(obstacle_bboxes));
    for k in 1..=retry_cap {
        for sign in [1.0_f64, -1.0_f64] {
            #[allow(clippy::cast_precision_loss)]
            let off = sign * GRID_MM * (k as f64);
            let (mid1, mid2) = if horizontal {
                ((s.x1, s.y1 + off), (s.x2, s.y2 + off))
            } else {
                ((s.x1 + off, s.y1), (s.x2 + off, s.y2))
            };
            let parts = [
                Segment {
                    x1: s.x1,
                    y1: s.y1,
                    x2: mid1.0,
                    y2: mid1.1,
                },
                Segment {
                    x1: mid1.0,
                    y1: mid1.1,
                    x2: mid2.0,
                    y2: mid2.1,
                },
                Segment {
                    x1: mid2.0,
                    y1: mid2.1,
                    x2: s.x2,
                    y2: s.y2,
                },
            ];
            if parts.iter().any(|p| crosses_any_bbox(p, foreign_bboxes)) {
                continue;
            }
            if parts.iter().any(|p| crosses_any_bbox(p, obstacle_bboxes)) {
                continue;
            }
            // Reject the detour only when one of the three NEW
            // parts collinearly overlaps a sibling routed net's
            // segment. We deliberately don't re-check the rest of
            // `routed[target].segments` — any pre-existing overlap
            // there is a separate problem the V11 pass cannot fix
            // by detouring this segment (and conservative rollback
            // would block all progress).
            if [parts[0], parts[1], parts[2]]
                .iter()
                .any(|p| part_overlaps_sibling(routed, target, p))
            {
                continue;
            }
            routed[target].segments[idx] = parts[0];
            routed[target].segments.push(parts[1]);
            routed[target].segments.push(parts[2]);
            return true;
        }
    }
    false
}

/// True iff a candidate segment `part` (intended as a new/replaced
/// part of `routed[target]`) collinearly overlaps any segment of any
/// OTHER routed net. Endpoint-only contact is fine — that's how
/// T-junctions work — but a non-empty open-interval overlap would
/// silently merge the two nets when KiCad's connectivity engine
/// canonicalises wires on load.
fn part_overlaps_sibling(routed: &[RoutedNet], target: usize, part: &Segment) -> bool {
    for (i, other) in routed.iter().enumerate() {
        if i == target {
            continue;
        }
        for s in &other.segments {
            if segments_collinearly_overlap(part, s) {
                return true;
            }
        }
    }
    false
}

/// Hash key for a segment (quantised to 1 µm) so we can compare new
/// vs old segment sets after an in-place jog. Direction-insensitive:
/// (x1,y1)→(x2,y2) and (x2,y2)→(x1,y1) hash to the same key.
#[allow(clippy::cast_possible_truncation)]
fn seg_key(s: &Segment) -> (i64, i64, i64, i64) {
    let a = (
        (s.x1 * 1000.0).round() as i64,
        (s.y1 * 1000.0).round() as i64,
    );
    let b = (
        (s.x2 * 1000.0).round() as i64,
        (s.y2 * 1000.0).round() as i64,
    );
    if a <= b {
        (a.0, a.1, b.0, b.1)
    } else {
        (b.0, b.1, a.0, a.1)
    }
}

fn segments_collinearly_overlap(a: &Segment, b: &Segment) -> bool {
    let a_horiz = (a.y1 - a.y2).abs() < EPS;
    let a_vert = (a.x1 - a.x2).abs() < EPS;
    let b_horiz = (b.y1 - b.y2).abs() < EPS;
    let b_vert = (b.x1 - b.x2).abs() < EPS;
    if a_horiz && b_horiz && (a.y1 - b.y1).abs() < EPS {
        let (alo, ahi) = if a.x1 <= a.x2 {
            (a.x1, a.x2)
        } else {
            (a.x2, a.x1)
        };
        let (blo, bhi) = if b.x1 <= b.x2 {
            (b.x1, b.x2)
        } else {
            (b.x2, b.x1)
        };
        return alo + EPS < bhi && blo + EPS < ahi;
    }
    if a_vert && b_vert && (a.x1 - b.x1).abs() < EPS {
        let (alo, ahi) = if a.y1 <= a.y2 {
            (a.y1, a.y2)
        } else {
            (a.y2, a.y1)
        };
        let (blo, bhi) = if b.y1 <= b.y2 {
            (b.y1, b.y2)
        } else {
            (b.y2, b.y1)
        };
        return alo + EPS < bhi && blo + EPS < ahi;
    }
    false
}

fn collect_endpoint_hits(net: &RoutedNet, foreign_pins: &[(i64, i64)]) -> Vec<(i64, i64)> {
    use std::collections::HashSet;
    let pin_set: HashSet<(i64, i64)> = foreign_pins.iter().copied().collect();
    let mut hits: HashSet<(i64, i64)> = HashSet::new();
    for s in &net.segments {
        for (x, y) in [(s.x1, s.y1), (s.x2, s.y2)] {
            let k = key(x, y);
            if pin_set.contains(&k) {
                hits.insert(k);
            }
        }
    }
    hits.into_iter().collect()
}

fn count_interior_hits(net: &RoutedNet, foreign_pins: &[(i64, i64)]) -> usize {
    let mut n = 0;
    for s in &net.segments {
        let horizontal = (s.y1 - s.y2).abs() < EPS;
        let vertical = (s.x1 - s.x2).abs() < EPS;
        if !horizontal && !vertical {
            continue;
        }
        for &(px, py) in foreign_pins {
            #[allow(clippy::cast_precision_loss, clippy::similar_names)]
            let (pin_x, pin_y) = (px as f64 / 1000.0, py as f64 / 1000.0);
            let inside = if horizontal {
                let lo = s.x1.min(s.x2);
                let hi = s.x1.max(s.x2);
                (pin_y - s.y1).abs() < EPS && pin_x > lo + EPS && pin_x < hi - EPS
            } else {
                let lo = s.y1.min(s.y2);
                let hi = s.y1.max(s.y2);
                (pin_x - s.x1).abs() < EPS && pin_y > lo + EPS && pin_y < hi - EPS
            };
            if inside {
                n += 1;
            }
        }
    }
    n
}

/// V12 — eliminate wires that strictly enter a foreign symbol body.
///
/// Strategy mirrors `avoid_foreign_pins`: process nets in the same
/// `net_priority_key` order so the most-constrained net moves first
/// and less-constrained nets react to its geometry. For every segment
/// of `routed[i]` that strictly crosses any `obstacles[k]` try, in
/// order:
///   1. `try_alt_l_corner` — swap the L corner of any L-pair the
///      offender is part of (cheap, no new segments).
///   2. `try_u_detour_l_pair` — replace an L pair with a 3-segment U
///      detour anchored on the L's far endpoints.
///   3. `try_detour_segment` — replace a single offending segment
///      with a 3-segment U detour around the obstacle.
///   4. Stage B fallback: BFS/Lee maze route on a coarse 1.27 mm
///      grid, returning the shortest bend-minimising path that
///      avoids every obstacle, every foreign pin, and every sibling
///      net's segment interior.
///
/// `obstacles` is the symbol-body bbox list (V12 sense).
/// `foreign_pins_per_net[i]` is the set of pin coords *foreign* to
/// `routed[i]` — passed through so detours don't re-introduce a V11
/// violation. `own_pin_coords[i]` is `routed[i]`'s own pin set —
/// anchored as junctions so Stage 4 cleanup can't coalesce away a
/// pin endpoint.
///
/// `placer_broken[i]` is set when `routed[i]`'s own pin sits strictly
/// inside a foreign symbol body — no router pass can fix that (any
/// wire connecting that pin must enter the body); the net is skipped
/// with a `v12-placer:` diagnostic so the offender's residual
/// crossing is not double-counted.
///
/// Iterates the priority pass to convergence (segment-set
/// signatures unchanged between iterations) or the derived
/// dependency-depth backstop, then emits one warning per net that
/// still has residual crossings.
#[allow(clippy::too_many_lines)]
pub fn avoid_obstacles<S: ::std::hash::BuildHasher>(
    routed: &mut [RoutedNet],
    obstacles: &[Bbox],
    own_pin_coords: &[std::collections::HashSet<(i64, i64), S>],
    foreign_pins_per_net: &[std::collections::HashSet<(i64, i64), S>],
    bounds: Option<Bbox>,
    pin_outward: &PinOutwardMap,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if obstacles.is_empty() || routed.is_empty() {
        return warnings;
    }

    // Pre-build per-net foreign-pin bbox lists once (reused on every
    // outer pass).
    let foreign_bboxes_per_net: Vec<Vec<Bbox>> = (0..routed.len())
        .map(|i| {
            foreign_pins_per_net
                .get(i)
                .map(|s| {
                    s.iter()
                        .map(|&(x, y)| {
                            #[allow(clippy::cast_precision_loss)]
                            Bbox::from_point(x as f64 / 1000.0, y as f64 / 1000.0)
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();

    // Detect placer-broken nets: any own pin strictly inside an
    // obstacle bbox. The router cannot fix this — emit a diagnostic
    // and skip enforcement for that net.
    let mut placer_broken = vec![false; routed.len()];
    for (i, _net) in routed.iter().enumerate() {
        let Some(own) = own_pin_coords.get(i) else {
            continue;
        };
        for &(x, y) in own {
            #[allow(clippy::cast_precision_loss)]
            let (fx, fy) = (x as f64 / 1000.0, y as f64 / 1000.0);
            for o in obstacles {
                // Use a degenerate segment (point) as the probe so the
                // strict-interior bbox test fires when the point sits
                // inside the body.
                if o.intersects_segment(fx, fy, fx, fy)
                    || (fx > o.x0 + 0.1 && fx < o.x1 - 0.1 && fy > o.y0 + 0.1 && fy < o.y1 - 0.1)
                {
                    placer_broken[i] = true;
                    warnings.push(format!(
                        "v12-placer: net index {i} has own pin ({fx:.3}, {fy:.3}) strictly inside a foreign symbol body; skipping V12 enforcement"
                    ));
                    break;
                }
            }
            if placer_broken[i] {
                break;
            }
        }
    }

    let mut order: Vec<usize> = (0..routed.len()).collect();
    order.sort_by(|&a, &b| {
        let key_a = net_priority_key(&routed[a]);
        let key_b = net_priority_key(&routed[b]);
        key_b.cmp(&key_a).then(a.cmp(&b))
    });

    // Convergence backstop for the outer priority pass. The loop's
    // real exit is the `!changed` fixed-point check below; this cap
    // only bounds the pathological non-converging (oscillating) case.
    // Each pass lets every net react to the others' current geometry;
    // a settling dependency chain across `n` mutually-constraining
    // nets reaches a fixed point in at most `n` passes, so the bound is
    // the count of nets that actually have a body to avoid (+1 for the
    // confirming no-change pass). This is a real dependency-depth
    // bound, not arbitrary slack.
    let outer_cap = order
        .iter()
        .filter(|&&i| !placer_broken[i])
        .count()
        .saturating_add(1)
        .max(1);
    // Maze-router blocked grid is rebuilt each outer pass (sibling
    // segments change as other nets reroute).
    for _ in 0..outer_cap {
        let pre_signatures: Vec<Vec<Segment>> = routed.iter().map(|n| n.segments.clone()).collect();
        for &i in &order {
            if placer_broken[i] {
                continue;
            }
            let own_for_net: &std::collections::HashSet<(i64, i64), S> = match own_pin_coords.get(i)
            {
                Some(s) => s,
                None => continue,
            };
            let foreign_bboxes: &[Bbox] = &foreign_bboxes_per_net[i];
            reroute_one_net_v12(
                routed,
                i,
                obstacles,
                foreign_bboxes,
                own_for_net,
                bounds,
                pin_outward,
            );
        }
        let changed = pre_signatures
            .iter()
            .zip(routed.iter())
            .any(|(pre, now)| pre != &now.segments);
        if !changed {
            break;
        }
    }

    // Final tally: emit one warning per net that still has a segment
    // crossing any obstacle.
    for (net_idx, net) in routed.iter().enumerate() {
        if placer_broken[net_idx] {
            continue;
        }
        let mut remaining = 0usize;
        for s in &net.segments {
            for o in obstacles {
                if o.intersects_segment(s.x1, s.y1, s.x2, s.y2) {
                    remaining += 1;
                    break;
                }
            }
        }
        if remaining > 0 {
            warnings.push(format!(
                "obstacle: net index {net_idx} has {remaining} segment(s) crossing a symbol body after {outer_cap} outer passes"
            ));
        }
    }
    warnings
}

/// V12 per-net pass: walk offending segments, attempt the V11 cascade
/// of corner-swap → L-pair-U → segment-U → maze. Stops when the net
/// is clean or every offender has been tried unsuccessfully.
fn reroute_one_net_v12<S: ::std::hash::BuildHasher>(
    routed: &mut [RoutedNet],
    target: usize,
    obstacles: &[Bbox],
    foreign_bboxes: &[Bbox],
    own_pins: &std::collections::HashSet<(i64, i64), S>,
    bounds: Option<Bbox>,
    pin_outward: &PinOutwardMap,
) {
    let mut guard = 0;
    loop {
        guard += 1;
        if guard > 128 {
            break;
        }
        let Some(idx) = find_obstacle_offender(&routed[target], obstacles) else {
            break;
        };
        // 1. Alt L corner.
        if try_alt_l_corner(
            routed,
            target,
            idx,
            foreign_bboxes,
            obstacles,
            own_pins,
            pin_outward,
        ) {
            continue;
        }
        // 2. U-detour around an L pair.
        if try_u_detour_l_pair(
            routed,
            target,
            idx,
            foreign_bboxes,
            obstacles,
            own_pins,
            pin_outward,
        ) {
            continue;
        }
        // 3. U-detour around the offending segment alone.
        if try_detour_segment(routed, target, idx, foreign_bboxes, obstacles) {
            continue;
        }
        // 4. Move a Steiner T-junction that sits inside an obstacle.
        //    Both endpoints of the offending segment are tried; the
        //    one that isn't an own pin and has degree ≥ 2 is a
        //    candidate junction. If one of its incident segments has
        //    its other endpoint inside the same obstacle, that
        //    endpoint is *also* considered.
        if try_move_steiner_junction(routed, target, idx, obstacles, foreign_bboxes, own_pins) {
            continue;
        }
        // 5. Stage B — maze route. Replaces the offending segment with
        // a BFS shortest-path that avoids every obstacle, foreign pin
        // and sibling segment interior.
        if try_maze_route_segment(
            routed,
            target,
            idx,
            obstacles,
            foreign_bboxes,
            bounds,
            pin_outward,
        ) {
            continue;
        }
        // Nothing helped — leave the segment and bail so the residual
        // tally surfaces it.
        break;
    }
    // Anchor any own-pin endpoint so Stage 4 cleanup can't coalesce
    // through it (mirrors `reroute_one_net_v11`).
    anchor_own_pin_endpoints(routed, target, own_pins);
}

/// First segment index of `net` strictly entering any `obstacles[k]`.
fn find_obstacle_offender(net: &RoutedNet, obstacles: &[Bbox]) -> Option<usize> {
    for (i, s) in net.segments.iter().enumerate() {
        for o in obstacles {
            if o.intersects_segment(s.x1, s.y1, s.x2, s.y2) {
                return Some(i);
            }
        }
    }
    None
}

/// Try to relocate a Steiner T-junction (≥ 3 incident segments) that
/// sits strictly inside an obstacle. The maze router cannot help once
/// a segment's endpoint is committed inside a body: any path
/// arriving at the goal emits a final segment whose interior crosses
/// the body. Moving the T-junction to a free cell rewires every
/// incident segment to a new corner that, given a good landing site,
/// clears the body in one move.
///
/// Strategy:
///   1. Locate every endpoint of `routed[target].segments[idx]` that
///      is **not** an own pin (those cannot move) and that has
///      degree ≥ 2 in `routed[target]` — a candidate junction.
///   2. For each candidate, enumerate landing cells in a small
///      rectangular halo around the junction.
///   3. For each landing, build the rewired segment set:
///      every incident segment's junction-endpoint moves to the new
///      landing; if the segment becomes non-axis-aligned it is split
///      into an L-pair (the new corner placed to keep the foreign
///      endpoint axis-stable).
///   4. Accept the first landing whose rewired segments avoid every
///      obstacle, foreign-pin bbox, and sibling-net interior.
#[allow(clippy::too_many_lines)]
fn try_move_steiner_junction<S: ::std::hash::BuildHasher>(
    routed: &mut [RoutedNet],
    target: usize,
    idx: usize,
    obstacles: &[Bbox],
    foreign_bboxes: &[Bbox],
    own_pins: &std::collections::HashSet<(i64, i64), S>,
) -> bool {
    let s = routed[target].segments[idx];
    for &endpoint in &[(s.x1, s.y1), (s.x2, s.y2)] {
        if own_pins.contains(&key(endpoint.0, endpoint.1)) {
            continue;
        }
        // Find every segment incident to `endpoint` (by either end),
        // recording for each incident the *other* endpoint (the one
        // that stays put after the move). The junction must have
        // degree ≥ 2 to be a Steiner T worth moving.
        let incidents: Vec<(usize, (f64, f64))> = routed[target]
            .segments
            .iter()
            .enumerate()
            .filter_map(|(i, seg)| {
                if (seg.x1 - endpoint.0).abs() < EPS && (seg.y1 - endpoint.1).abs() < EPS {
                    Some((i, (seg.x2, seg.y2)))
                } else if (seg.x2 - endpoint.0).abs() < EPS && (seg.y2 - endpoint.1).abs() < EPS {
                    Some((i, (seg.x1, seg.y1)))
                } else {
                    None
                }
            })
            .collect();
        if incidents.len() < 2 {
            continue;
        }
        // Only attempt this when `endpoint` is actually strictly
        // inside one of the obstacles — otherwise moving it isn't
        // motivated by V12. Capture the containing boxes so the search
        // radius can be bounded by how far the junction must travel to
        // escape them.
        let containing: Vec<&Bbox> = obstacles
            .iter()
            .filter(|o| {
                endpoint.0 > o.x0 + 0.1
                    && endpoint.0 < o.x1 - 0.1
                    && endpoint.1 > o.y0 + 0.1
                    && endpoint.1 < o.y1 - 0.1
            })
            .collect();
        if containing.is_empty() {
            continue;
        }
        // A junction strictly inside a box escapes by moving at most
        // the box's larger dimension (worst case: it sits against one
        // edge and must reach just past the opposite edge), plus one
        // clearance cell. Derive the spiral radius from the boxes the
        // junction is actually inside rather than guessing a fixed
        // slack; an unbounded layout can therefore never force an
        // unbounded search.
        let move_radius: i32 = containing
            .iter()
            .map(|o| {
                let span = (o.x1 - o.x0).max(o.y1 - o.y0).max(0.0);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let cells = (span / GRID_MM).ceil() as i32;
                cells.saturating_add(1).max(1)
            })
            .max()
            .unwrap_or(1);
        // Search landings on the 1.27 mm grid in a spiral around the
        // original junction.
        for r in 1..=move_radius {
            for dy in -r..=r {
                for dx in -r..=r {
                    // Only the spiral perimeter at radius r.
                    if dx.abs() != r && dy.abs() != r {
                        continue;
                    }
                    #[allow(clippy::cast_precision_loss)]
                    let nx = endpoint.0 + f64::from(dx) * GRID_MM;
                    #[allow(clippy::cast_precision_loss)]
                    let ny = endpoint.1 + f64::from(dy) * GRID_MM;
                    // Candidate must be outside every obstacle and
                    // every foreign-pin bbox.
                    let inside_any = obstacles.iter().chain(foreign_bboxes.iter()).any(|o| {
                        nx > o.x0 + 0.1 && nx < o.x1 - 0.1 && ny > o.y0 + 0.1 && ny < o.y1 - 0.1
                    });
                    if inside_any {
                        continue;
                    }
                    // Build the rewired segment set for this landing.
                    // For each incident (i, other), the new pair is:
                    //   - if (other.x, ny) is the natural corner (i.e.
                    //     other already shares an axis with the
                    //     landing), a single segment;
                    //   - otherwise an L-pair: (other → corner →
                    //     landing) with the corner picked to keep the
                    //     foreign endpoint on its own axis.
                    let mut new_parts: Vec<(usize, Vec<Segment>)> = Vec::new();
                    let mut ok = true;
                    for (seg_i, other) in &incidents {
                        let (ox, oy) = *other;
                        let parts: Vec<Segment> = if (ox - nx).abs() < EPS || (oy - ny).abs() < EPS
                        {
                            vec![Segment {
                                x1: ox,
                                y1: oy,
                                x2: nx,
                                y2: ny,
                            }]
                        } else {
                            // Two L-corner choices. Pick whichever
                            // produces no obstacle / foreign-pin /
                            // sibling-overlap collision.
                            let mut chosen: Option<Vec<Segment>> = None;
                            for corner in [(ox, ny), (nx, oy)] {
                                let a = Segment {
                                    x1: ox,
                                    y1: oy,
                                    x2: corner.0,
                                    y2: corner.1,
                                };
                                let b = Segment {
                                    x1: corner.0,
                                    y1: corner.1,
                                    x2: nx,
                                    y2: ny,
                                };
                                if [a, b].iter().any(approx_zero_len) {
                                    continue;
                                }
                                if [a, b].iter().any(|p| crosses_any_bbox(p, foreign_bboxes)) {
                                    continue;
                                }
                                if [a, b].iter().any(|p| crosses_any_bbox(p, obstacles)) {
                                    continue;
                                }
                                if [a, b]
                                    .iter()
                                    .any(|p| part_overlaps_sibling(routed, target, p))
                                {
                                    continue;
                                }
                                chosen = Some(vec![a, b]);
                                break;
                            }
                            if let Some(v) = chosen {
                                v
                            } else {
                                ok = false;
                                break;
                            }
                        };
                        for p in &parts {
                            if crosses_any_bbox(p, foreign_bboxes)
                                || crosses_any_bbox(p, obstacles)
                                || part_overlaps_sibling(routed, target, p)
                            {
                                ok = false;
                                break;
                            }
                        }
                        if !ok {
                            break;
                        }
                        new_parts.push((*seg_i, parts));
                    }
                    if !ok {
                        continue;
                    }
                    // Install: remove old incident segments (highest
                    // index first), then push all new parts.
                    let mut victims: Vec<usize> = new_parts.iter().map(|(i, _)| *i).collect();
                    victims.sort_unstable();
                    for v in victims.iter().rev() {
                        routed[target].segments.remove(*v);
                    }
                    for (_, parts) in new_parts {
                        for p in parts {
                            routed[target].segments.push(p);
                        }
                    }
                    return true;
                }
            }
        }
    }
    false
}

/// Stage B — Lee/BFS maze router fallback. Replaces a single offending
/// segment with the shortest bend-minimising rectilinear path between
/// its two endpoints that avoids every obstacle interior, every
/// foreign pin coord, and every sibling routed net's segment
/// interior.
///
/// The blocked grid is quantised at `GRID_MM` (1.27 mm) over a bbox
/// derived from `bounds` (caller-supplied) or, when `None`, the pin
/// union of `routed` extended by a margin derived from the
/// obstacle/foreign-pin extent (see [`compute_maze_bounds`]). A
/// cell is blocked iff it strictly lies inside an obstacle bbox, on
/// a foreign-pin coord, or on the interior of a sibling net's
/// segment (segments belonging to the same net are not blocked —
/// the path is allowed to land on existing trunks of its own net).
/// The two endpoint cells are explicitly unblocked.
///
/// Returns `true` when a path was installed. The path is converted to
/// a list of axis-aligned `Segment`s replacing the original segment
/// at index `idx`. Complexity: O(V · log V) with V ≤ `MAZE_CELL_CAP`
/// — comfortably under 1 ms per fixture in practice.
fn try_maze_route_segment(
    routed: &mut [RoutedNet],
    target: usize,
    idx: usize,
    obstacles: &[Bbox],
    foreign_bboxes: &[Bbox],
    bounds: Option<Bbox>,
    pin_outward: &PinOutwardMap,
) -> bool {
    let s = routed[target].segments[idx];
    // Don't maze-route a zero-length probe.
    if approx_zero_len(&s) {
        return false;
    }
    let Some(grid) = build_maze_grid(routed, target, obstacles, foreign_bboxes, bounds) else {
        return false;
    };
    let start = grid.world_to_cell(s.x1, s.y1);
    let goal = grid.world_to_cell(s.x2, s.y2);
    let (Some(start), Some(goal)) = (start, goal) else {
        return false;
    };
    if start == goal {
        return false;
    }
    // V5: if the start (or goal) coincides with a pin, constrain the
    // first (last) step of the maze path to that pin's outward
    // direction. Falls back to unconstrained when no outward-clean
    // path exists.
    let start_outward = pin_outward.get(&key(s.x1, s.y1)).copied();
    let goal_outward = pin_outward.get(&key(s.x2, s.y2)).copied();
    let path = match maze_shortest_path_constrained(&grid, start, goal, start_outward, goal_outward)
    {
        Some(p) => Some(p),
        None => maze_shortest_path(&grid, start, goal),
    };
    let Some(path) = path else {
        return false;
    };
    if path.len() < 2 {
        return false;
    }
    // Convert grid path to axis-aligned world segments. Coalesce
    // collinear hops on the fly so a straight run lands as a single
    // segment.
    let mut new_segs: Vec<Segment> = Vec::new();
    let mut anchor = path[0];
    let mut prev = path[0];
    let sgn = |a: usize, b: usize| -> i8 {
        match a.cmp(&b) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    };
    for &cur in path.iter().skip(1) {
        let dir_prev = (sgn(prev.0, anchor.0), sgn(prev.1, anchor.1));
        let dir_cur = (sgn(cur.0, prev.0), sgn(cur.1, prev.1));
        if dir_prev != (0, 0) && dir_prev != dir_cur {
            // Bend. Emit anchor..prev, restart anchor at prev.
            let (ax, ay) = grid.cell_to_world(anchor);
            let (bx, by) = grid.cell_to_world(prev);
            new_segs.push(Segment {
                x1: ax,
                y1: ay,
                x2: bx,
                y2: by,
            });
            anchor = prev;
        }
        prev = cur;
    }
    let (ax, ay) = grid.cell_to_world(anchor);
    let (bx, by) = grid.cell_to_world(prev);
    new_segs.push(Segment {
        x1: ax,
        y1: ay,
        x2: bx,
        y2: by,
    });
    if new_segs.iter().any(approx_zero_len) {
        // Defensive: drop zero-length parts that might survive
        // float-rounding.
        new_segs.retain(|seg| !approx_zero_len(seg));
        if new_segs.is_empty() {
            return false;
        }
    }
    // Defensive re-check: the blocked grid already excludes these
    // cases, but verify before installation.
    if new_segs.iter().any(|p| crosses_any_bbox(p, foreign_bboxes)) {
        return false;
    }
    if new_segs.iter().any(|p| crosses_any_bbox(p, obstacles)) {
        return false;
    }
    if new_segs
        .iter()
        .any(|p| part_overlaps_sibling(routed, target, p))
    {
        return false;
    }
    // Install: replace segments[idx] with the first new segment,
    // append the rest.
    routed[target].segments[idx] = new_segs[0];
    for p in new_segs.into_iter().skip(1) {
        routed[target].segments.push(p);
    }
    true
}

/// Quantised blocked-cell grid for the Lee/BFS maze router.
///
/// Two block sets are maintained:
///
/// * `blocked[i]` — cell `i` may not be visited at all.
/// * `edge_blocked_h[i]` / `edge_blocked_v[i]` — the 1.27 mm step from
///   cell `i` to its `+x` / `+y` neighbour, respectively, strictly
///   crosses an obstacle even though both endpoint cells passed the
///   strict-interior cell test. This happens when an obstacle's body
///   edge lies on a grid line: the cell on the boundary is "unblocked"
///   by the point-sample test but stepping into the body emits a wire
///   segment that strictly enters the body interior. We treat that
///   edge as impassable in [`maze_shortest_path`].
struct MazeGrid {
    cols: usize,
    rows: usize,
    x0: f64,
    y0: f64,
    blocked: Vec<bool>,
    edge_blocked_h: Vec<bool>,
    edge_blocked_v: Vec<bool>,
}

impl MazeGrid {
    fn idx(&self, c: (usize, usize)) -> usize {
        c.1 * self.cols + c.0
    }
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    fn in_bounds(&self, c: (i64, i64)) -> bool {
        c.0 >= 0 && c.1 >= 0 && (c.0 as usize) < self.cols && (c.1 as usize) < self.rows
    }
    fn world_to_cell(&self, x: f64, y: f64) -> Option<(usize, usize)> {
        let cx = ((x - self.x0) / GRID_MM).round();
        let cy = ((y - self.y0) / GRID_MM).round();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_possible_wrap
        )]
        let k = (cx as i64, cy as i64);
        if !self.in_bounds(k) {
            return None;
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        Some((k.0 as usize, k.1 as usize))
    }
    fn cell_to_world(&self, c: (usize, usize)) -> (f64, f64) {
        #[allow(clippy::cast_precision_loss)]
        let x = self.x0 + (c.0 as f64) * GRID_MM;
        #[allow(clippy::cast_precision_loss)]
        let y = self.y0 + (c.1 as f64) * GRID_MM;
        (x, y)
    }
}

/// Compute the bounding box used to size the maze grid.
///
/// The caller may supply explicit `bounds`; the unit tests do, but the
/// production emitter passes `None` (see `kicad-emitter/src/
/// schematic.rs::route_nets`, which flows through `spice-route::route`
/// into `avoid_obstacles`), so the derived path below is the *normal*
/// path on every real route. When `explicit` is `None` the bbox is the
/// union of every routed net's segment endpoints, padded by a margin
/// derived from the obstacle/foreign-pin geometry the router must
/// detour around.
///
/// Margin rationale (a real bound, not slack): the only consumer of
/// this grid is the V12 maze fallback, which replaces a body-crossing
/// segment with a detour. A rectilinear detour never needs to swing
/// wider than [`max_detour_cells`] of the geometry it avoids — past
/// that extent the path is already clear of every box. We therefore
/// size the margin as that per-instance cell count (over the union of
/// obstacles and foreign pins) plus one clearance cell, so a detour
/// terminating at a pin on the union boundary still has a free
/// neighbour to bend into. This is the same extent bound the
/// `try_detour_segment` / `try_u_detour_l_pair` loops already use, so
/// the maze grid can express every detour those loops can.
fn compute_maze_bounds(
    routed: &[RoutedNet],
    obstacles: &[Bbox],
    foreign_bboxes: &[Bbox],
    explicit: Option<Bbox>,
) -> Option<Bbox> {
    if let Some(b) = explicit {
        return Some(b);
    }
    let mut lo_x = f64::INFINITY;
    let mut lo_y = f64::INFINITY;
    let mut hi_x = f64::NEG_INFINITY;
    let mut hi_y = f64::NEG_INFINITY;
    for net in routed {
        for s in &net.segments {
            for (x, y) in [(s.x1, s.y1), (s.x2, s.y2)] {
                lo_x = lo_x.min(x);
                lo_y = lo_y.min(y);
                hi_x = hi_x.max(x);
                hi_y = hi_y.max(y);
            }
        }
    }
    if !lo_x.is_finite() || !hi_x.is_finite() {
        return None;
    }
    let margin_cells = max_detour_cells(obstacles).max(max_detour_cells(foreign_bboxes));
    #[allow(clippy::cast_precision_loss)]
    let margin_mm = (margin_cells.saturating_add(1) as f64) * GRID_MM;
    Some(Bbox {
        x0: lo_x - margin_mm,
        y0: lo_y - margin_mm,
        x1: hi_x + margin_mm,
        y1: hi_y + margin_mm,
    })
}

/// Build the maze blocked grid: a cell at (col, row) is blocked iff
/// it strictly lies inside an obstacle bbox, on a foreign-pin coord,
/// or on the interior of a sibling routed net's axis-parallel
/// segment. Sibling segments belonging to `routed[target]` are not
/// considered blockers — the maze path may share endpoints with the
/// rest of the same net's trunk.
#[allow(clippy::too_many_lines)]
fn build_maze_grid(
    routed: &[RoutedNet],
    target: usize,
    obstacles: &[Bbox],
    foreign_bboxes: &[Bbox],
    explicit_bounds: Option<Bbox>,
) -> Option<MazeGrid> {
    let bounds = compute_maze_bounds(routed, obstacles, foreign_bboxes, explicit_bounds)?;
    let width = bounds.x1 - bounds.x0;
    let height = bounds.y1 - bounds.y0;
    if !(width.is_finite() && height.is_finite()) || width <= 0.0 || height <= 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cols = ((width / GRID_MM).round() as usize).max(2) + 1;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rows = ((height / GRID_MM).round() as usize).max(2) + 1;
    if cols.saturating_mul(rows) > MAZE_CELL_CAP {
        return None;
    }
    let mut grid = MazeGrid {
        cols,
        rows,
        x0: bounds.x0,
        y0: bounds.y0,
        blocked: vec![false; cols * rows],
        edge_blocked_h: vec![false; cols * rows],
        edge_blocked_v: vec![false; cols * rows],
    };

    // Mark obstacle cells. A cell is blocked iff its center falls
    // inside any obstacle's body, including the 0.1 mm interior slop
    // used by `Bbox::intersects_segment`. We also block any cell whose
    // 4-neighbour edge would cross strictly into an obstacle interior:
    // a path step from cell A → cell B emits a 1.27 mm segment, and if
    // that segment penetrates a body bbox by ≥ 0.1 mm the verifier
    // counts it as a crossing. So we conservatively block cells that
    // are within one half-cell of any obstacle interior.
    for cy in 0..rows {
        #[allow(clippy::cast_precision_loss)]
        let wy = grid.y0 + (cy as f64) * GRID_MM;
        for cx in 0..cols {
            #[allow(clippy::cast_precision_loss)]
            let wx = grid.x0 + (cx as f64) * GRID_MM;
            for o in obstacles {
                // Block any cell strictly inside the obstacle's
                // 0.1-mm-inflated-inward bbox. This matches the
                // verifier's strict-interior test.
                if wx > o.x0 + 0.1 && wx < o.x1 - 0.1 && wy > o.y0 + 0.1 && wy < o.y1 - 0.1 {
                    let i = grid.idx((cx, cy));
                    grid.blocked[i] = true;
                    break;
                }
            }
        }
    }
    // Pre-compute per-edge obstacle crossings. Two cells passing the
    // strict-interior cell test can still be connected by a segment
    // that crosses an obstacle when the body edge sits exactly on a
    // grid line (the case for our 1.27 mm-aligned placer output).
    for cy in 0..rows {
        for cx in 0..cols {
            let i = grid.idx((cx, cy));
            #[allow(clippy::cast_precision_loss)]
            let wx = grid.x0 + (cx as f64) * GRID_MM;
            #[allow(clippy::cast_precision_loss)]
            let wy = grid.y0 + (cy as f64) * GRID_MM;
            // +x neighbour (horizontal edge).
            if cx + 1 < cols {
                for o in obstacles {
                    if o.intersects_segment(wx, wy, wx + GRID_MM, wy) {
                        grid.edge_blocked_h[i] = true;
                        break;
                    }
                }
            }
            // +y neighbour (vertical edge).
            if cy + 1 < rows {
                for o in obstacles {
                    if o.intersects_segment(wx, wy, wx, wy + GRID_MM) {
                        grid.edge_blocked_v[i] = true;
                        break;
                    }
                }
            }
        }
    }
    // Mark foreign-pin coords.
    for b in foreign_bboxes {
        let cx = ((b.x0 + b.x1) * 0.5 - grid.x0) / GRID_MM;
        let cy = ((b.y0 + b.y1) * 0.5 - grid.y0) / GRID_MM;
        let cx = cx.round();
        let cy = cy.round();
        if cx < 0.0 || cy < 0.0 {
            continue;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let col = cx as usize;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let row = cy as usize;
        if col >= cols || row >= rows {
            continue;
        }
        let i = grid.idx((col, row));
        grid.blocked[i] = true;
    }
    // Mark sibling-net segment interior cells (and endpoints — sharing
    // an endpoint with a sibling net is a V11 short by definition).
    for (i, other) in routed.iter().enumerate() {
        if i == target {
            continue;
        }
        for seg in &other.segments {
            let Some(a) = grid.world_to_cell(seg.x1, seg.y1) else {
                continue;
            };
            let Some(b) = grid.world_to_cell(seg.x2, seg.y2) else {
                continue;
            };
            let horiz = (seg.y1 - seg.y2).abs() < EPS;
            let vert = (seg.x1 - seg.x2).abs() < EPS;
            if horiz {
                let (lo, hi) = (a.0.min(b.0), a.0.max(b.0));
                for cx in lo..=hi {
                    let k = grid.idx((cx, a.1));
                    grid.blocked[k] = true;
                }
            } else if vert {
                let (lo, hi) = (a.1.min(b.1), a.1.max(b.1));
                for cy in lo..=hi {
                    let k = grid.idx((a.0, cy));
                    grid.blocked[k] = true;
                }
            }
        }
    }
    Some(grid)
}

/// BFS / Lee on the blocked grid with a bend penalty applied via
/// Dijkstra over (cell, in-direction) states. Returns the path as a
/// list of cells from `start` to `goal` inclusive, with the
/// shortest-bend-penalty score, or `None` when unreachable.
/// Same as [`maze_shortest_path`] but additionally constrains the
/// first move out of `start` (when `start_dir` is `Some`) and the last
/// move into `goal` (when `goal_dir` is `Some`) to the supplied
/// directions. Used by the V5 outward-direction enforcement at pin
/// endpoints. Returns `None` when no path satisfying the constraints
/// exists; the caller is expected to fall back to the unconstrained
/// [`maze_shortest_path`].
fn maze_shortest_path_constrained(
    grid: &MazeGrid,
    start: (usize, usize),
    goal: (usize, usize),
    start_dir: Option<Direction>,
    goal_dir: Option<Direction>,
) -> Option<Vec<(usize, usize)>> {
    let dir_to_nd = |d: Direction| -> usize {
        match d {
            Direction::Right => 0,
            Direction::Left => 1,
            // Maze-grid +y matches file-y +1 step (rows ascend).
            Direction::Down => 2,
            Direction::Up => 3,
        }
    };
    let path = maze_shortest_path(grid, start, goal)?;
    // The constrained variant: re-run the search but reject any move
    // that violates the first/last step constraint. Cheap approach:
    // run plain BFS and post-filter; this preserves the existing
    // implementation. The filter checks the second cell direction
    // against `start_dir` and the penultimate-to-last direction
    // against `goal_dir`.
    if path.len() < 2 {
        return Some(path);
    }
    #[allow(clippy::cast_possible_wrap)]
    let first_step = (
        path[1].0 as i64 - path[0].0 as i64,
        path[1].1 as i64 - path[0].1 as i64,
    );
    if let Some(d) = start_dir {
        let nd = dir_to_nd(d);
        let want = (
            i64::from([1_i32, -1, 0, 0][nd]),
            i64::from([0_i32, 0, 1, -1][nd]),
        );
        if first_step != want {
            return None;
        }
    }
    let n = path.len();
    #[allow(clippy::cast_possible_wrap)]
    let last_step = (
        path[n - 1].0 as i64 - path[n - 2].0 as i64,
        path[n - 1].1 as i64 - path[n - 2].1 as i64,
    );
    if let Some(d) = goal_dir {
        // Goal outward direction = direction the pin's stem points.
        // The maze path *arrives* at the goal, so the incoming step
        // direction is the opposite of the pin's outward.
        let nd = dir_to_nd(d);
        let want_outward = (
            i64::from([1_i32, -1, 0, 0][nd]),
            i64::from([0_i32, 0, 1, -1][nd]),
        );
        let want_incoming = (-want_outward.0, -want_outward.1);
        if last_step != want_incoming {
            return None;
        }
    }
    Some(path)
}

fn maze_shortest_path(
    grid: &MazeGrid,
    start: (usize, usize),
    goal: (usize, usize),
) -> Option<Vec<(usize, usize)>> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    // Pre-clear the start and goal cells: a maze-rerouted segment's
    // endpoints are pin / Steiner-junction positions the caller has
    // already committed to. Treating them as blocked would prevent the
    // search from leaving the start or arriving at the goal even when
    // a valid path exists everywhere else. (`maze_shortest_path` is
    // called only for endpoints the caller has determined are not
    // own pins of the target net, so opening them is safe.)
    let mut grid_blocked: Vec<bool> = grid.blocked.clone();
    let start_idx = grid.idx(start);
    let goal_idx = grid.idx(goal);
    grid_blocked[start_idx] = false;
    grid_blocked[goal_idx] = false;
    // 4-connected moves, indexed 0..4: 0=+x, 1=-x, 2=+y, 3=-y, 4=initial.
    let dx = [1_i32, -1, 0, 0];
    let dy = [0_i32, 0, 1, -1];

    // State key: (col, row, dir). We allow `dir = 4` for the start
    // state (no incoming direction yet).
    let state_dim = grid.cols * grid.rows * 5;
    // Cost in micrometres so the ordering is integer and tight.
    let mut best: Vec<u64> = vec![u64::MAX; state_dim];
    let mut parent: Vec<Option<(usize, u8)>> = vec![None; state_dim]; // (prev_state_idx, prev_dir)
    let state_idx = |c: (usize, usize), dir: usize| (c.1 * grid.cols + c.0) * 5 + dir;
    let start_state = state_idx(start, 4);
    best[start_state] = 0;
    let mut heap: BinaryHeap<Reverse<(u64, usize)>> = BinaryHeap::new();
    heap.push(Reverse((0, start_state)));

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let step_um: u64 = (GRID_MM * 1000.0).round() as u64;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bend_um: u64 = (MAZE_BEND_PENALTY_MM * 1000.0).round() as u64;

    let mut found: Option<usize> = None;
    while let Some(Reverse((cost, sidx))) = heap.pop() {
        if cost > best[sidx] {
            continue;
        }
        // Decode.
        let dir = sidx % 5;
        let cell_lin = sidx / 5;
        let cur = (cell_lin % grid.cols, cell_lin / grid.cols);
        if cur == goal {
            found = Some(sidx);
            break;
        }
        for nd in 0..4_usize {
            #[allow(clippy::cast_possible_wrap)]
            let nx = cur.0 as i64 + i64::from(dx[nd]);
            #[allow(clippy::cast_possible_wrap)]
            let ny = cur.1 as i64 + i64::from(dy[nd]);
            if !grid.in_bounds((nx, ny)) {
                continue;
            }
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            let next_cell = (nx as usize, ny as usize);
            // Skip blocked cells. Start and goal cells were pre-cleared
            // above so the search can enter them.
            if grid_blocked[grid.idx(next_cell)] {
                continue;
            }
            // Skip if the step `cur → next_cell` crosses an obstacle
            // edge that the cell-blocked test missed (body edge on a
            // grid line). The edge tables are keyed on the lower-index
            // cell of the pair.
            let cur_i = grid.idx(cur);
            let next_i = grid.idx(next_cell);
            let edge_block = match nd {
                0 => grid.edge_blocked_h[cur_i],  // +x from cur
                1 => grid.edge_blocked_h[next_i], // -x: edge stored at next
                2 => grid.edge_blocked_v[cur_i],  // +y from cur
                3 => grid.edge_blocked_v[next_i], // -y: edge stored at next
                _ => false,
            };
            if edge_block {
                continue;
            }
            let mut step = cost.saturating_add(step_um);
            if dir != 4 && dir != nd {
                step = step.saturating_add(bend_um);
            }
            let nsidx = state_idx(next_cell, nd);
            if step < best[nsidx] {
                best[nsidx] = step;
                #[allow(clippy::cast_possible_truncation)]
                {
                    parent[nsidx] = Some((sidx, dir as u8));
                }
                heap.push(Reverse((step, nsidx)));
            }
        }
    }

    let mut sidx = found?;
    let mut out_rev: Vec<(usize, usize)> = Vec::new();
    loop {
        let cell_lin = sidx / 5;
        out_rev.push((cell_lin % grid.cols, cell_lin / grid.cols));
        match parent[sidx] {
            Some((prev, _)) => sidx = prev,
            None => break,
        }
    }
    out_rev.reverse();
    Some(out_rev)
}

/// If segments `a` and `b` share an endpoint and are axis-aligned with
/// perpendicular orientations, return the two far endpoints (the ones
/// that don't coincide) plus the shared corner.
type LPair = ((f64, f64), (f64, f64), (f64, f64));

fn l_pair_endpoints(a: &Segment, b: &Segment) -> Option<LPair> {
    let a_horiz = (a.y1 - a.y2).abs() < EPS;
    let a_vert = (a.x1 - a.x2).abs() < EPS;
    let b_horiz = (b.y1 - b.y2).abs() < EPS;
    let b_vert = (b.x1 - b.x2).abs() < EPS;
    if !((a_horiz && b_vert) || (a_vert && b_horiz)) {
        return None;
    }
    for (ax, ay, ox, oy) in [(a.x1, a.y1, a.x2, a.y2), (a.x2, a.y2, a.x1, a.y1)] {
        for (bx, by, px, py) in [(b.x1, b.y1, b.x2, b.y2), (b.x2, b.y2, b.x1, b.y1)] {
            if (ax - bx).abs() < EPS && (ay - by).abs() < EPS {
                return Some(((ox, oy), (px, py), (ax, ay)));
            }
        }
    }
    None
}

/// Count how many segment endpoints in `net` land at `point`. A
/// shared corner with degree 2 is a simple L bend; degree ≥ 3 marks
/// a Steiner T-junction whose tree topology must be preserved.
fn corner_degree(net: &RoutedNet, point: (f64, f64)) -> usize {
    let mut deg = 0usize;
    for s in &net.segments {
        if (s.x1 - point.0).abs() < EPS && (s.y1 - point.1).abs() < EPS {
            deg += 1;
        }
        if (s.x2 - point.0).abs() < EPS && (s.y2 - point.1).abs() < EPS {
            deg += 1;
        }
    }
    deg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_conflict_when_nets_disjoint() {
        let mut routed = vec![
            RoutedNet {
                segments: vec![Segment {
                    x1: 0.0,
                    y1: 0.0,
                    x2: 5.08,
                    y2: 0.0,
                }],
                junctions: vec![],
            },
            RoutedNet {
                segments: vec![Segment {
                    x1: 10.16,
                    y1: 10.16,
                    x2: 15.24,
                    y2: 10.16,
                }],
                junctions: vec![],
            },
        ];
        let warnings =
            resolve_conflicts::<std::collections::hash_map::RandomState>(&mut routed, &[]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn jogs_endpoint_when_two_nets_collide() {
        let mut routed = vec![
            RoutedNet {
                segments: vec![Segment {
                    x1: 0.0,
                    y1: 0.0,
                    x2: 5.08,
                    y2: 0.0,
                }],
                junctions: vec![],
            },
            RoutedNet {
                segments: vec![Segment {
                    x1: 5.08,
                    y1: 0.0,
                    x2: 10.16,
                    y2: 0.0,
                }],
                junctions: vec![],
            },
        ];
        let _ = resolve_conflicts::<std::collections::hash_map::RandomState>(&mut routed, &[]);
        // After jogging, no coordinate should carry endpoints from
        // both nets.
        let conflicts = find_conflicts(&routed);
        assert!(conflicts.is_empty(), "still conflicting: {conflicts:?}");
    }

    // ----------------------------------------------------------------
    // Maze router (Stage B) unit tests.
    // ----------------------------------------------------------------

    /// Helper: build an empty `MazeGrid` over the explicit bounds, with
    /// no nets, obstacles, or pins. Used by maze-only tests.
    fn empty_grid(bounds: Bbox) -> MazeGrid {
        build_maze_grid(&[], usize::MAX, &[], &[], Some(bounds))
            .expect("build_maze_grid should succeed for a positive-area bbox")
    }

    fn ucell((c, r): (usize, usize)) -> (usize, usize) {
        (c, r)
    }

    #[test]
    fn maze_trivial_straight_line_in_empty_grid() {
        let g = empty_grid(Bbox {
            x0: 0.0,
            y0: 0.0,
            x1: 10.0 * GRID_MM,
            y1: 10.0 * GRID_MM,
        });
        let path = maze_shortest_path(&g, ucell((0, 0)), ucell((5, 0))).expect("path should exist");
        // Straight line: 6 cells (inclusive of both ends).
        assert_eq!(path.len(), 6);
        // All on row 0.
        assert!(path.iter().all(|(_, r)| *r == 0));
    }

    #[test]
    fn maze_returns_l_for_diagonal_endpoints_in_empty_grid() {
        let g = empty_grid(Bbox {
            x0: 0.0,
            y0: 0.0,
            x1: 10.0 * GRID_MM,
            y1: 10.0 * GRID_MM,
        });
        let path = maze_shortest_path(&g, ucell((0, 0)), ucell((3, 3))).expect("path should exist");
        // Manhattan distance 6 → path length 7 cells, with exactly one
        // bend (i.e. two collinear runs).
        assert_eq!(path.len(), 7);
        let mut bends = 0;
        for w in path.windows(3) {
            let d1 = (w[1].0.cmp(&w[0].0) as i32, w[1].1.cmp(&w[0].1) as i32);
            let d2 = (w[2].0.cmp(&w[1].0) as i32, w[2].1.cmp(&w[1].1) as i32);
            if d1 != d2 {
                bends += 1;
            }
        }
        assert_eq!(bends, 1, "expected one bend, got {bends} in path {path:?}");
    }

    #[test]
    fn maze_routes_around_an_obstacle() {
        // A 5×5 cell obstacle in the middle of a 15×15 grid forces a
        // detour. Start left of the obstacle, goal to the right.
        let bounds = Bbox {
            x0: 0.0,
            y0: 0.0,
            x1: 14.0 * GRID_MM,
            y1: 14.0 * GRID_MM,
        };
        let obstacle = Bbox {
            x0: 5.0 * GRID_MM,
            y0: 5.0 * GRID_MM,
            x1: 9.0 * GRID_MM,
            y1: 9.0 * GRID_MM,
        };
        let g = build_maze_grid(&[], usize::MAX, &[obstacle], &[], Some(bounds))
            .expect("build_maze_grid");
        // Start at (2, 7), goal at (12, 7): centerline blocked.
        let path = maze_shortest_path(&g, ucell((2, 7)), ucell((12, 7))).expect("path");
        // Verify no path point lies strictly inside the obstacle
        // (using the same 0.1 mm tolerance the verifier uses).
        for &(c, r) in &path {
            #[allow(clippy::cast_precision_loss)]
            let wx = g.x0 + (c as f64) * GRID_MM;
            #[allow(clippy::cast_precision_loss)]
            let wy = g.y0 + (r as f64) * GRID_MM;
            assert!(
                !(wx > obstacle.x0 + 0.1
                    && wx < obstacle.x1 - 0.1
                    && wy > obstacle.y0 + 0.1
                    && wy < obstacle.y1 - 0.1),
                "path cell ({c},{r}) = ({wx:.3},{wy:.3}) is inside obstacle"
            );
        }
        // Path is longer than the straight-line 11.
        assert!(path.len() > 11);
    }

    #[test]
    fn maze_returns_none_when_goal_unreachable() {
        // A horizontal wall splits the grid into halves; routing
        // across is impossible.
        let bounds = Bbox {
            x0: 0.0,
            y0: 0.0,
            x1: 10.0 * GRID_MM,
            y1: 10.0 * GRID_MM,
        };
        // Wall spans the full width as a single big obstacle.
        let wall = Bbox {
            x0: -GRID_MM,
            y0: 4.0 * GRID_MM,
            x1: 12.0 * GRID_MM,
            y1: 6.0 * GRID_MM,
        };
        let g =
            build_maze_grid(&[], usize::MAX, &[wall], &[], Some(bounds)).expect("build_maze_grid");
        // The cell-blocked test marks the wall's strict interior
        // (cells whose center is at y=5*GRID_MM); the edge-blocked
        // test additionally marks the boundary edges. Together no
        // path crosses from row<5 to row>5.
        assert!(maze_shortest_path(&g, ucell((1, 1)), ucell((1, 9))).is_none());
    }

    #[test]
    fn maze_prefers_fewer_bends_when_lengths_tie() {
        // From (0,0) to (4,4), Manhattan distance 8 (path length 9).
        // Many minimum-length paths exist with varying bend counts.
        // The bend penalty should force a 1-bend path (L-shape).
        let g = empty_grid(Bbox {
            x0: 0.0,
            y0: 0.0,
            x1: 8.0 * GRID_MM,
            y1: 8.0 * GRID_MM,
        });
        let path = maze_shortest_path(&g, ucell((0, 0)), ucell((4, 4))).expect("path");
        assert_eq!(path.len(), 9);
        let mut bends = 0;
        for w in path.windows(3) {
            let d1 = (w[1].0.cmp(&w[0].0) as i32, w[1].1.cmp(&w[0].1) as i32);
            let d2 = (w[2].0.cmp(&w[1].0) as i32, w[2].1.cmp(&w[1].1) as i32);
            if d1 != d2 {
                bends += 1;
            }
        }
        assert_eq!(
            bends, 1,
            "bend-minimising router should produce 1-bend path, got {bends}"
        );
    }

    #[test]
    fn try_move_steiner_junction_relocates_offender_into_clear_space() {
        // A single net with a Steiner T-junction inside an obstacle.
        // The junction is at (0,0), surrounded by three incident
        // segments going to (-5,0), (5,0), and (0,5). The obstacle
        // covers (-1..1)x(-1..1) — the junction sits in its interior.
        let mut routed = vec![RoutedNet {
            segments: vec![
                Segment {
                    x1: -5.0,
                    y1: 0.0,
                    x2: 0.0,
                    y2: 0.0,
                },
                Segment {
                    x1: 5.0,
                    y1: 0.0,
                    x2: 0.0,
                    y2: 0.0,
                },
                Segment {
                    x1: 0.0,
                    y1: 5.0,
                    x2: 0.0,
                    y2: 0.0,
                },
            ],
            junctions: vec![],
        }];
        let obstacles = [Bbox {
            x0: -1.0,
            y0: -1.0,
            x1: 1.0,
            y1: 1.0,
        }];
        let own_pins: std::collections::HashSet<(i64, i64)> =
            [(-5000, 0), (5000, 0), (0, 5000)].into_iter().collect();
        let foreign: [Bbox; 0] = [];
        // Pick segment 0 (incident on the junction).
        let installed =
            try_move_steiner_junction(&mut routed, 0, 0, &obstacles, &foreign, &own_pins);
        assert!(installed, "junction move should succeed in open space");
        // Every segment of the net must be outside the obstacle.
        for s in &routed[0].segments {
            assert!(
                !obstacles[0].intersects_segment(s.x1, s.y1, s.x2, s.y2),
                "segment {s:?} still crosses obstacle"
            );
        }
    }
}
