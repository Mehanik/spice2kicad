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

use crate::types::{RoutedNet, Segment};

const GRID_MM: f64 = 1.27;
const EPS: f64 = 1e-6;
const MAX_ITERATIONS: usize = 10;

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

/// Jog a single endpoint of `net` that touches `point` by one grid
/// cell on the axis that least disturbs its peer endpoint. Modifies
/// the affected segment's matching endpoint in place; the other end
/// of the segment becomes the new "stub" attachment point.
fn jog_endpoint_at(net: &mut RoutedNet, point: (i64, i64)) {
    // Find the first segment with an endpoint matching `point`.
    let target_idx = net
        .segments
        .iter()
        .position(|s| key(s.x1, s.y1) == point || key(s.x2, s.y2) == point);
    let Some(idx) = target_idx else {
        return;
    };
    let s = net.segments[idx];
    let at_start = key(s.x1, s.y1) == point;
    let (px, py, _qx, qy) = if at_start {
        (s.x1, s.y1, s.x2, s.y2)
    } else {
        (s.x2, s.y2, s.x1, s.y1)
    };
    // Decide jog axis: along the segment direction is bad (shortens
    // / lengthens the existing wire); pick perpendicular to the
    // segment so the segment effectively gains a small kink-stub.
    let horizontal = (py - qy).abs() < EPS;
    let (jx, jy) = if horizontal {
        // Horizontal segment — jog Y.
        (px, py + GRID_MM)
    } else {
        // Vertical or diagonal segment — jog X.
        (px + GRID_MM, py)
    };
    // Move the endpoint by one grid cell. We don't add a stub back
    // to (px, py) because that would re-create the conflict at the
    // original coordinate; the trade-off is a small wire-length
    // increase on this net in exchange for electrical separation.
    if at_start {
        net.segments[idx].x1 = jx;
        net.segments[idx].y1 = jy;
    } else {
        net.segments[idx].x2 = jx;
        net.segments[idx].y2 = jy;
    }
    let _ = qy;
    // The Segment import is still needed for tests below.
    let _ = std::marker::PhantomData::<Segment>;
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
