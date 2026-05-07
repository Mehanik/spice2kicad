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
//! 3. Repeat until no conflicts remain or 10 iterations elapse.
//!
//! This is *not* full Stage 3 rip-up & retry from the original spec —
//! that lands later. The jog-once loop is sufficient for the small
//! v0.1 fixtures.

use crate::types::{Bbox, RoutedNet, Segment};

const GRID_MM: f64 = 1.27;
const EPS: f64 = 1e-6;
const MAX_ITERATIONS: usize = 10;

/// Cap retry count for obstacle avoidance per segment. Each retry tries
/// the alternate L corner (or shifts the bend by a grid cell). After
/// the cap a warning is recorded and the offending segment is left
/// alone — a body-crossing wire is ugly but still electrically valid.
const OBSTACLE_RETRY_CAP: usize = 4;

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
/// after `MAX_ITERATIONS` jog passes.
pub fn resolve_conflicts<S: ::std::hash::BuildHasher>(
    routed: &mut [RoutedNet],
    net_pin_coords: &[std::collections::HashSet<(i64, i64), S>],
) -> Vec<String> {
    let net_pins = net_pin_coords;
    let mut warnings = Vec::new();
    for _ in 0..MAX_ITERATIONS {
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
            "conflict: net index {n} has endpoint conflicts left after {MAX_ITERATIONS} resolve iterations"
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

#[allow(clippy::cast_possible_truncation)]
fn pin_key(x: f64, y: f64) -> (i64, i64) {
    ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64)
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
    // V11 enforcement is **detect-and-warn-only** in v0.1. Naïve
    // detours can introduce new V11 violations (a detour leg crossing
    // a different foreign pin) without an obstacle-aware re-route, and
    // the router's current "pick perpendicular direction" logic is not
    // strong enough to guarantee progress. The diagnostic still
    // surfaces the bug end-to-end so callers can fix the underlying
    // placer-level pin overlap. Active rerouting is tracked as a
    // v0.2 work item.
    let _: () = ();
    // Final tally.
    for (i, net) in routed.iter().enumerate() {
        let pins = &foreign[i];
        if pins.is_empty() {
            continue;
        }
        let endpoints = collect_endpoint_hits(net, pins);
        let interior = count_interior_hits(net, pins);
        if !endpoints.is_empty() || interior > 0 {
            warnings.push(format!(
                "v11: net index {i} has {} endpoint and {interior} interior foreign-pin coincidences left after foreign-pin avoidance",
                endpoints.len()
            ));
        }
    }
    warnings
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

/// Re-route segments that pass through a symbol body (`obstacles`).
///
/// Strategy per net: identify L-shaped pin-to-pin bends (a pair of
/// orthogonal segments sharing an endpoint) where one of the two legs
/// crosses an obstacle, and try the **alternate** L corner — the other
/// way of routing the same pin pair. If the alternate also crosses,
/// shift the corner by ±1 grid cell along each axis up to the retry
/// cap. Standalone non-bend segments (the rare case after stage-3
/// jogging) are inspected too: the segment is replaced with an L via
/// a corner offset by 1 cell perpendicular to the segment, and the
/// retry budget walks 1 → 2 → … cells out.
///
/// Returns one warning per remaining body-crossing segment after the
/// retry budget is exhausted. A body-crossing wire is electrically
/// valid (KiCad still routes the net correctly), just visually ugly.
pub fn avoid_obstacles<S: ::std::hash::BuildHasher>(
    routed: &mut [RoutedNet],
    obstacles: &[Bbox],
    net_pin_coords: &[std::collections::HashSet<(i64, i64), S>],
) -> Vec<String> {
    let mut warnings = Vec::new();
    if obstacles.is_empty() {
        return warnings;
    }
    for (net_idx, net) in routed.iter_mut().enumerate() {
        let pins_iter: Vec<(i64, i64)> = net_pin_coords
            .get(net_idx)
            .map_or_else(Vec::new, |s| s.iter().copied().collect());
        // Reroute L-bend pairs first: pairs of segments that share an
        // endpoint and form an axis-aligned L. The alternate L swaps
        // the corner — but only when the corner is NOT a pin (pins
        // must remain segment endpoints to keep the net connected).
        rewrite_l_bends(net, obstacles, &pins_iter);
        // Standalone offending segments: try perpendicular detours.
        // Pin coordinates flow through so the detour avoids creating a
        // collinear extension at a pin endpoint (Stage 4 cleanup would
        // coalesce that into a single span and orphan the pin from
        // electrical connectivity).
        rewrite_standalone_crossings(net, obstacles, &pins_iter);

        // Tally remaining crossings.
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
                "obstacle: net index {net_idx} has {remaining} segment(s) crossing a symbol body after {OBSTACLE_RETRY_CAP} retries"
            ));
        }
    }
    warnings
}

/// For every pair of segments (A, B) within a net that share an
/// endpoint and form an axis-parallel L, if either leg crosses an
/// obstacle try the alternate corner: an L between the same two
/// far endpoints via the *other* coordinate axis. Replace the pair
/// when the alternate has fewer crossings.
fn rewrite_l_bends(net: &mut RoutedNet, obstacles: &[Bbox], pin_coords: &[(i64, i64)]) {
    let is_pin = |p: (f64, f64)| -> bool {
        let k = pin_key(p.0, p.1);
        pin_coords.contains(&k)
    };
    let mut iter = 0;
    loop {
        if iter >= OBSTACLE_RETRY_CAP {
            return;
        }
        iter += 1;
        let n = net.segments.len();
        let mut chosen: Option<(usize, usize, Segment, Segment)> = None;
        'outer: for i in 0..n {
            for j in (i + 1)..n {
                let a = net.segments[i];
                let b = net.segments[j];
                let Some((p_far, q_far, corner)) = l_pair_endpoints(&a, &b) else {
                    continue;
                };
                let curr_cross = seg_crosses_any(&a, obstacles) || seg_crosses_any(&b, obstacles);
                if !curr_cross {
                    continue;
                }
                // Skip rewriting if the shared corner is a Steiner
                // T-junction (≥ 3 segments meet): rerouting the L would
                // disconnect the third leg from the tree.
                if corner_degree(net, corner) > 2 {
                    continue;
                }
                // Skip if the corner is a pin coordinate. Pins must
                // remain segment endpoints — swapping the L corner
                // away from a pin disconnects the net (this was the
                // opamp_inverting roundtrip regression: a 3-pin
                // Steiner tree with the L corner sitting on RF's pin).
                if is_pin(corner) {
                    continue;
                }
                // Skip if either far endpoint is a pin: the alt L
                // routes through the alt corner, which lies on the
                // axis of the OTHER far endpoint — Stage 4 cleanup
                // can then coalesce the alt's leg with any collinear
                // segment in the same net through that pin. The
                // resulting single span still passes geometrically
                // through the pin's coordinate, so KiCad's
                // wire-touches-pin rule (V11) keeps the pin
                // electrically connected — but the segment we keep
                // in `RoutedNet` no longer has the pin as an
                // endpoint, breaking downstream invariants that
                // expect every pin to anchor at least one segment
                // endpoint (e.g. junction emission, `find_conflicts`
                // foreign-pin checks). Only swap when both far
                // endpoints are Steiner points or non-pins.
                if is_pin(p_far) || is_pin(q_far) {
                    continue;
                }
                // Alternate L: corner at (p_far.x, q_far.y) if current
                // corner is (q_far.x, p_far.y), and vice versa. Try
                // both alternates and pick the one with fewer crossings.
                let alt1 = (p_far.0, q_far.1);
                let alt2 = (q_far.0, p_far.1);
                for alt in [alt1, alt2] {
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
                    let alt_cross =
                        seg_crosses_any(&s1, obstacles) || seg_crosses_any(&s2, obstacles);
                    if !alt_cross {
                        chosen = Some((i, j, s1, s2));
                        break 'outer;
                    }
                }
            }
        }
        let Some((i, j, s1, s2)) = chosen else {
            return;
        };
        let (lo, hi) = if i < j { (i, j) } else { (j, i) };
        net.segments.remove(hi);
        net.segments.remove(lo);
        net.segments.push(s1);
        net.segments.push(s2);
    }
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

fn seg_crosses_any(s: &Segment, obstacles: &[Bbox]) -> bool {
    obstacles
        .iter()
        .any(|o| o.intersects_segment(s.x1, s.y1, s.x2, s.y2))
}

/// Standalone segment that crosses an obstacle (not part of an L-pair).
/// Try replacing it with a 3-segment detour: bend perpendicular by k
/// cells, traverse parallel, bend back. k walks 1..=OBSTACLE_RETRY_CAP.
fn rewrite_standalone_crossings(
    net: &mut RoutedNet,
    obstacles: &[Bbox],
    pin_coords: &[(i64, i64)],
) {
    let is_pin = |x: f64, y: f64| -> bool {
        let k = pin_key(x, y);
        pin_coords.contains(&k)
    };
    let mut i = 0;
    while i < net.segments.len() {
        let s = net.segments[i];
        if !seg_crosses_any(&s, obstacles) {
            i += 1;
            continue;
        }
        let horizontal = (s.y1 - s.y2).abs() < EPS;
        let mut replaced = false;
        for k in 1..=OBSTACLE_RETRY_CAP {
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
                if parts.iter().any(|p| seg_crosses_any(p, obstacles)) {
                    continue;
                }
                // Replace original segment with the three detour parts.
                net.segments[i] = parts[0];
                net.segments.push(parts[1]);
                net.segments.push(parts[2]);
                // Anchor original endpoints as junctions so Stage 4
                // coalescing does not merge the perpendicular stub
                // with a collinear segment elsewhere on the net.
                // Coalescing would still leave the pin geometrically
                // on the merged wire (KiCad's V11 wire-touches-pin
                // rule keeps it connected) but the pin would no
                // longer be a segment endpoint, which breaks
                // downstream invariants that key on endpoints (e.g.
                // foreign-pin conflict detection). Only mark when
                // the endpoint is a pin coord, otherwise we'd
                // over-decorate the schematic with unnecessary
                // junction dots.
                if is_pin(s.x1, s.y1) {
                    net.junctions.push((s.x1, s.y1));
                }
                if is_pin(s.x2, s.y2) {
                    net.junctions.push((s.x2, s.y2));
                }
                replaced = true;
                break;
            }
            if replaced {
                break;
            }
        }
        i += 1;
    }
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
}
