//! Stage 4 — wire cleanup.
//!
//! Two passes:
//!
//! * [`coalesce_collinear`] — within each routed net, merge pairs of
//!   axis-parallel segments that share an endpoint and a coordinate
//!   axis with no junction at the shared point.
//! * [`dedup_junctions`] — flatten the per-net junction lists into a
//!   single global set, with each (x, y) emitted once even if two
//!   nets recorded a junction at the same coordinate.

use crate::types::{RoutedNet, Segment};

const EPS: f64 = 1e-6;

/// Drop zero-length segments from every routed net, in place.
///
/// Earlier router stages (jog, obstacle detour, foreign-pin detour)
/// can produce degenerate segments when the original path's far
/// endpoint already coincides with the new corner. Serialising those
/// produces `(wire (pts (xy X Y) (xy X Y)))` which renders nothing
/// in eeschema but trips downstream invariants. Always strip them
/// before [`coalesce_collinear`] runs so the merge logic doesn't
/// have to tolerate them.
pub fn drop_zero_length(routed: &mut [RoutedNet]) {
    for net in routed.iter_mut() {
        net.segments
            .retain(|s| !((s.x1 - s.x2).abs() < EPS && (s.y1 - s.y2).abs() < EPS));
    }
}

/// Coalesce collinear adjacent segments per net, in place.
///
/// Two segments are merged when:
/// * they share an endpoint, and
/// * they lie on the same axis (both horizontal or both vertical) at
///   the same coordinate, and
/// * the shared point is not recorded as a junction for this net.
///
/// Iterates until no more merges fire.
pub fn coalesce_collinear(routed: &mut [RoutedNet]) {
    for net in routed.iter_mut() {
        coalesce_one(net);
    }
}

fn coalesce_one(net: &mut RoutedNet) {
    loop {
        let n = net.segments.len();
        let mut merged = false;
        'outer: for i in 0..n {
            for j in (i + 1)..n {
                if let Some(m) = try_merge(&net.segments[i], &net.segments[j], &net.junctions) {
                    // Preserve indices: replace i, remove j.
                    net.segments[i] = m;
                    net.segments.remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
        if !merged {
            return;
        }
    }
}

fn try_merge(a: &Segment, b: &Segment, junctions: &[(f64, f64)]) -> Option<Segment> {
    let a_horiz = (a.y1 - a.y2).abs() < EPS;
    let a_vert = (a.x1 - a.x2).abs() < EPS;
    let b_horiz = (b.y1 - b.y2).abs() < EPS;
    let b_vert = (b.x1 - b.x2).abs() < EPS;
    // Both horizontal at same Y.
    if a_horiz && b_horiz && (a.y1 - b.y1).abs() < EPS {
        // Find shared X.
        for &(ax, bx, other_a, other_b) in &[
            (a.x2, b.x1, a.x1, b.x2),
            (a.x2, b.x2, a.x1, b.x1),
            (a.x1, b.x1, a.x2, b.x2),
            (a.x1, b.x2, a.x2, b.x1),
        ] {
            if (ax - bx).abs() < EPS && !is_junction((ax, a.y1), junctions) {
                // shared point at (ax, a.y1).
                return Some(Segment {
                    x1: other_a,
                    y1: a.y1,
                    x2: other_b,
                    y2: a.y1,
                });
            }
        }
    }
    // Both vertical at same X.
    if a_vert && b_vert && (a.x1 - b.x1).abs() < EPS {
        for &(ay, by, other_a, other_b) in &[
            (a.y2, b.y1, a.y1, b.y2),
            (a.y2, b.y2, a.y1, b.y1),
            (a.y1, b.y1, a.y2, b.y2),
            (a.y1, b.y2, a.y2, b.y1),
        ] {
            if (ay - by).abs() < EPS && !is_junction((a.x1, ay), junctions) {
                return Some(Segment {
                    x1: a.x1,
                    y1: other_a,
                    x2: a.x1,
                    y2: other_b,
                });
            }
        }
    }
    None
}

fn is_junction(p: (f64, f64), junctions: &[(f64, f64)]) -> bool {
    junctions
        .iter()
        .any(|&(jx, jy)| (jx - p.0).abs() < EPS && (jy - p.1).abs() < EPS)
}

/// Collapse the per-net junction lists into a single deduplicated set.
/// Uses 0.001 mm-quantised keys so f64 noise doesn't desync identical
/// coordinates emitted by independent Steiner trees.
#[must_use]
pub fn dedup_junctions(routed: &[RoutedNet]) -> Vec<(f64, f64)> {
    use std::collections::HashSet;
    let mut seen: HashSet<(i64, i64)> = HashSet::new();
    let mut out: Vec<(f64, f64)> = Vec::new();
    for net in routed {
        for &(x, y) in &net.junctions {
            #[allow(clippy::cast_possible_truncation)]
            let k = ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64);
            if seen.insert(k) {
                out.push((x, y));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_three_horizontal() {
        let mut routed = vec![RoutedNet {
            segments: vec![
                Segment {
                    x1: 0.0,
                    y1: 0.0,
                    x2: 5.0,
                    y2: 0.0,
                },
                Segment {
                    x1: 5.0,
                    y1: 0.0,
                    x2: 10.0,
                    y2: 0.0,
                },
                Segment {
                    x1: 10.0,
                    y1: 0.0,
                    x2: 15.0,
                    y2: 0.0,
                },
            ],
            junctions: vec![],
        }];
        coalesce_collinear(&mut routed);
        assert_eq!(routed[0].segments.len(), 1);
        let s = routed[0].segments[0];
        assert!((s.x1 - 0.0).abs() < EPS || (s.x1 - 15.0).abs() < EPS);
        assert!((s.x2 - 0.0).abs() < EPS || (s.x2 - 15.0).abs() < EPS);
    }

    #[test]
    fn keeps_segments_separated_by_junction() {
        let mut routed = vec![RoutedNet {
            segments: vec![
                Segment {
                    x1: 0.0,
                    y1: 0.0,
                    x2: 5.0,
                    y2: 0.0,
                },
                Segment {
                    x1: 5.0,
                    y1: 0.0,
                    x2: 10.0,
                    y2: 0.0,
                },
            ],
            junctions: vec![(5.0, 0.0)],
        }];
        coalesce_collinear(&mut routed);
        assert_eq!(routed[0].segments.len(), 2);
    }

    #[test]
    fn dedups_coincident_junctions() {
        let routed = vec![
            RoutedNet {
                segments: vec![],
                junctions: vec![(5.0, 0.0)],
            },
            RoutedNet {
                segments: vec![],
                junctions: vec![(5.0, 0.0), (10.0, 0.0)],
            },
        ];
        let j = dedup_junctions(&routed);
        assert_eq!(j.len(), 2);
    }
}
