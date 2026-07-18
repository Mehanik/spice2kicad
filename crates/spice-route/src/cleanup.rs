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
    let empty: [std::collections::HashSet<(i64, i64)>; 0] = [];
    coalesce_collinear_with_barriers(routed, &empty);
}

/// Variant of [`coalesce_collinear`] that additionally treats every
/// coord in `barriers[i]` (per routed-net `i`) as a non-mergeable
/// shared endpoint. Used by the router pipeline to anchor own pins
/// without leaking extra `(junction …)` glyphs into the emitted
/// schematic.
pub fn coalesce_collinear_with_barriers<S: ::std::hash::BuildHasher>(
    routed: &mut [RoutedNet],
    barriers: &[std::collections::HashSet<(i64, i64), S>],
) {
    let empty: std::collections::HashSet<(i64, i64)> = std::collections::HashSet::new();
    for (i, net) in routed.iter_mut().enumerate() {
        let bar = barriers
            .get(i)
            .map_or_else(|| &empty as &dyn BarrierSet, |s| s as &dyn BarrierSet);
        coalesce_one(net, bar);
    }
}

/// Object-safe trait so the cleanup pass can take either an empty
/// barrier (legacy callers) or a populated one, regardless of the
/// caller's `HashSet` hasher.
trait BarrierSet {
    fn contains_qkey(&self, k: (i64, i64)) -> bool;
}

impl<S: ::std::hash::BuildHasher> BarrierSet for std::collections::HashSet<(i64, i64), S> {
    fn contains_qkey(&self, k: (i64, i64)) -> bool {
        self.contains(&k)
    }
}

fn coalesce_one(net: &mut RoutedNet, barriers: &dyn BarrierSet) {
    loop {
        let n = net.segments.len();
        let mut merged = false;
        'outer: for i in 0..n {
            for j in (i + 1)..n {
                if let Some(m) =
                    try_merge(&net.segments[i], &net.segments[j], &net.junctions, barriers)
                {
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

fn try_merge(
    a: &Segment,
    b: &Segment,
    junctions: &[(f64, f64)],
    barriers: &dyn BarrierSet,
) -> Option<Segment> {
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
            if (ax - bx).abs() < EPS
                && !is_junction((ax, a.y1), junctions)
                && !is_barrier((ax, a.y1), barriers)
            {
                // shared point at (ax, a.y1). Reject when the merged
                // span would have a barrier coord in its interior —
                // that's the V5 outward-stub case where the trunk
                // passes through a pin between the two stubs.
                let merged = Segment {
                    x1: other_a,
                    y1: a.y1,
                    x2: other_b,
                    y2: a.y1,
                };
                if !barrier_in_interior(&merged, barriers) {
                    return Some(merged);
                }
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
            if (ay - by).abs() < EPS
                && !is_junction((a.x1, ay), junctions)
                && !is_barrier((a.x1, ay), barriers)
            {
                let merged = Segment {
                    x1: a.x1,
                    y1: other_a,
                    x2: a.x1,
                    y2: other_b,
                };
                if !barrier_in_interior(&merged, barriers) {
                    return Some(merged);
                }
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

#[allow(clippy::cast_possible_truncation)]
fn is_barrier(p: (f64, f64), barriers: &dyn BarrierSet) -> bool {
    let k = ((p.0 * 1000.0).round() as i64, (p.1 * 1000.0).round() as i64);
    barriers.contains_qkey(k)
}

/// True iff any barrier coordinate lies strictly inside `seg`'s
/// axis-parallel span (exclusive of both endpoints). Walks the 1.27 mm
/// grid between the endpoints; barrier coords align to the grid by
/// construction. Used by the cleanup pass to refuse a merge that
/// would route the merged trunk through a pin.
#[allow(clippy::cast_possible_truncation, clippy::similar_names)]
fn barrier_in_interior(seg: &Segment, barriers: &dyn BarrierSet) -> bool {
    const GRID_UM: i64 = 1270;
    let qx1 = (seg.x1 * 1000.0).round() as i64;
    let qy1 = (seg.y1 * 1000.0).round() as i64;
    let qx2 = (seg.x2 * 1000.0).round() as i64;
    let qy2 = (seg.y2 * 1000.0).round() as i64;
    if qx1 == qx2 {
        let (lo, hi) = (qy1.min(qy2), qy1.max(qy2));
        let mut y = lo + GRID_UM;
        while y < hi {
            if barriers.contains_qkey((qx1, y)) {
                return true;
            }
            y += GRID_UM;
        }
    } else if qy1 == qy2 {
        let (lo, hi) = (qx1.min(qx2), qx1.max(qx2));
        let mut x = lo + GRID_UM;
        while x < hi {
            if barriers.contains_qkey((x, qy1)) {
                return true;
            }
            x += GRID_UM;
        }
    }
    false
}

/// Quantise a single coordinate to a 1 µm integer key, matching the
/// scheme used throughout the router (`(v * 1000.0).round() as i64`).
#[allow(clippy::cast_possible_truncation)]
fn qk1(v: f64) -> i64 {
    (v * 1000.0).round() as i64
}

/// Given the member intervals on one line, return a set of
/// **non-overlapping** sub-intervals that (a) cover exactly the union of
/// the members and (b) have a breakpoint at *every* member endpoint, so
/// no original vertex is lost. Overlapping / nested / duplicate members
/// collapse into shared sub-intervals; a gap between two disjoint
/// clusters stays a gap.
///
/// Why split rather than merge into one span: `kicad-cli`'s netlist
/// export connects wires only at shared segment **endpoints**, never at
/// a bare point on a wire's interior (see
/// `conflict::anchor_own_pin_endpoints`). A branch / pin that attached
/// at a member endpoint must therefore remain a segment endpoint after
/// cleanup, or the export silently drops the connection. Splitting the
/// union at every member endpoint preserves all those vertices while
/// still eliminating the redundant collinear overlap.
fn split_union_at_endpoints(ivals: &mut [(f64, f64)]) -> Vec<(f64, f64)> {
    ivals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<(f64, f64)> = Vec::new();
    let mut i = 0;
    while i < ivals.len() {
        // Grow a maximal cluster of members overlapping (positively) or
        // touching the running union.
        let mut hi = ivals[i].1;
        let mut breaks: Vec<f64> = vec![ivals[i].0, ivals[i].1];
        let mut j = i + 1;
        while j < ivals.len() && ivals[j].0 <= hi + EPS {
            breaks.push(ivals[j].0);
            breaks.push(ivals[j].1);
            if ivals[j].1 > hi {
                hi = ivals[j].1;
            }
            j += 1;
        }
        // Distinct sorted breakpoints spanning the cluster.
        breaks.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        breaks.dedup_by(|a, b| (*a - *b).abs() < EPS);
        for w in breaks.windows(2) {
            if (w[1] - w[0]).abs() > EPS {
                out.push((w[0], w[1]));
            }
        }
        i = j;
    }
    out
}

/// Collapse collinear same-net overlaps into a non-overlapping cover.
///
/// Two axis-parallel segments on the same line (same X for verticals,
/// same Y for horizontals) whose spans overlap by a positive length —
/// including the fully-nested and exact-duplicate cases — are
/// redundant: the shorter is entirely covered by the longer. This pass
/// replaces every overlapping cluster with the *split* of its union at
/// each original member endpoint (see [`split_union_at_endpoints`]), so
/// the redundant overlap is gone but every attachment vertex survives.
///
/// **V11 safety.** The output covers exactly the union of the members,
/// pointwise a subset of the longest member's span; no coordinate
/// outside the members' extents is introduced, so no new foreign-pin
/// coincidence can arise — the pre-cleanup V11 pass that vetted the
/// members' spans still holds.
///
/// **Connectivity safety.** Every original segment endpoint remains a
/// segment endpoint (it is a breakpoint of the split), so `kicad-cli`'s
/// endpoint-only connection rule sees the same vertices it did before.
///
/// Junction lists are left untouched; [`add_connection_junctions`] and
/// [`dedup_junctions`] run afterwards.
pub fn collapse_collinear_overlaps(routed: &mut [RoutedNet]) {
    // BTreeMap, not HashMap: this map is *iterated* to build the output
    // segment list, so hash order would make the emitted wire order vary
    // between runs of the same binary on the same input.
    use std::collections::BTreeMap as HashMap;
    for net in routed.iter_mut() {
        // fixed-coord key -> (representative fixed coord, spans).
        let mut vert: HashMap<i64, (f64, Vec<(f64, f64)>)> = HashMap::new();
        let mut horiz: HashMap<i64, (f64, Vec<(f64, f64)>)> = HashMap::new();
        let mut others: Vec<Segment> = Vec::new();
        for s in &net.segments {
            let is_vert = (s.x1 - s.x2).abs() < EPS;
            let is_horiz = (s.y1 - s.y2).abs() < EPS;
            if is_vert && !is_horiz {
                let e = vert.entry(qk1(s.x1)).or_insert((s.x1, Vec::new()));
                e.1.push((s.y1.min(s.y2), s.y1.max(s.y2)));
            } else if is_horiz && !is_vert {
                let e = horiz.entry(qk1(s.y1)).or_insert((s.y1, Vec::new()));
                e.1.push((s.x1.min(s.x2), s.x1.max(s.x2)));
            } else {
                // Zero-length (both axes) or diagonal — never emitted by
                // the axis-aligned router, but preserve verbatim if seen.
                others.push(*s);
            }
        }
        let mut out = others;
        for (_, (x, mut spans)) in vert {
            for (lo, hi) in split_union_at_endpoints(&mut spans) {
                out.push(Segment {
                    x1: x,
                    y1: lo,
                    x2: x,
                    y2: hi,
                });
            }
        }
        for (_, (y, mut spans)) in horiz {
            for (lo, hi) in split_union_at_endpoints(&mut spans) {
                out.push(Segment {
                    x1: lo,
                    y1: y,
                    x2: hi,
                    y2: y,
                });
            }
        }
        net.segments = out;
    }
}

/// Add a junction dot at every point where three or more same-net wire
/// *rays* meet — KiCad's own junction rule. A ray is one direction a
/// wire leaves the point: a segment with an **endpoint** at `p`
/// contributes one ray; a segment whose **strict interior** contains
/// `p` contributes two (it passes through). Two rays (a straight
/// pass-through, an L-corner, or two collinear segments meeting
/// end-to-end) need no dot; three (a T, whether the trunk is split at
/// `p` or passes through it) or four (a cross) do.
///
/// This covers both the split-T case produced by
/// [`collapse_collinear_overlaps`] (three endpoints coincide) and a
/// mid-span T where a branch endpoint lands on an unbroken trunk
/// interior. Each [`RoutedNet`] is a single net by construction, so
/// every incidence examined here is same-net.
///
/// Idempotent against pre-existing junctions (e.g. own-pin anchors from
/// `conflict::anchor_own_pin_endpoints`): a point already recorded is
/// not duplicated. [`dedup_junctions`] flattens the rest.
pub fn add_connection_junctions(routed: &mut [RoutedNet]) {
    use std::collections::HashSet;
    for net in routed.iter_mut() {
        // Candidate points: every distinct segment endpoint (a junction
        // can only occur where at least one wire ends or a branch
        // attaches — both are endpoints of some segment).
        let mut candidates: Vec<(f64, f64)> = Vec::new();
        {
            let mut seen: HashSet<(i64, i64)> = HashSet::new();
            for s in &net.segments {
                for (x, y) in [(s.x1, s.y1), (s.x2, s.y2)] {
                    if seen.insert((qk1(x), qk1(y))) {
                        candidates.push((x, y));
                    }
                }
            }
        }
        let mut existing: HashSet<(i64, i64)> = net
            .junctions
            .iter()
            .map(|&(x, y)| (qk1(x), qk1(y)))
            .collect();
        let mut add: Vec<(f64, f64)> = Vec::new();
        for &(px, py) in &candidates {
            let mut rays = 0usize;
            for s in &net.segments {
                let at_a = (s.x1 - px).abs() < EPS && (s.y1 - py).abs() < EPS;
                let at_b = (s.x2 - px).abs() < EPS && (s.y2 - py).abs() < EPS;
                if at_a || at_b {
                    rays += 1;
                } else if point_strictly_interior(s, px, py) {
                    rays += 2;
                }
            }
            if rays >= 3 {
                let k = (qk1(px), qk1(py));
                if existing.insert(k) {
                    add.push((px, py));
                }
            }
        }
        net.junctions.extend(add);
    }
}

/// True iff `(px, py)` lies on the strict interior (exclusive of both
/// endpoints) of the axis-parallel segment `s`.
fn point_strictly_interior(s: &Segment, px: f64, py: f64) -> bool {
    let is_vert = (s.x1 - s.x2).abs() < EPS;
    let is_horiz = (s.y1 - s.y2).abs() < EPS;
    if is_vert && !is_horiz {
        (px - s.x1).abs() < EPS && py > s.y1.min(s.y2) + EPS && py < s.y1.max(s.y2) - EPS
    } else if is_horiz && !is_vert {
        (py - s.y1).abs() < EPS && px > s.x1.min(s.x2) + EPS && px < s.x1.max(s.x2) - EPS
    } else {
        false
    }
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
