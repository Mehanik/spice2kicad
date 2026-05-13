//! Stage 2 — per-net rectilinear Steiner trees.
//!
//! Stage 2a covers the small-N closed-form cases:
//!
//! * **N = 2** — single segment if pins share an axis, otherwise an
//!   L-shape via the corner `(b.x, a.y)`. The bend is not a junction
//!   (only two endpoints touch the net at this point).
//! * **N = 3** — Hwang's exact rectilinear Steiner minimum tree. The
//!   single Steiner point is the coordinate-wise median of the three
//!   pins; this is provably optimal.
//!
//! Higher-N nets fall through to a stub that lands in Task 4
//! (Hanan-grid DP) and Task 5 (FLUTE for N ≥ 10).

use crate::types::{Direction, PinRef, Segment};
use lexpr::Value as Sexpr;

const EPS: f64 = 1e-6;
const GRID_MM: f64 = 1.27;

/// Apply one grid-cell step in `dir` to the point `(x, y)`. Used by the
/// outward-stub fallback when neither L corner aligns with a pin's
/// outward direction.
fn step(dir: Direction, x: f64, y: f64) -> (f64, f64) {
    match dir {
        Direction::Up => (x, y - GRID_MM),
        Direction::Down => (x, y + GRID_MM),
        Direction::Left => (x - GRID_MM, y),
        Direction::Right => (x + GRID_MM, y),
    }
}

/// True iff the axis-parallel segment `from → to` extends in `dir`
/// from `from`. Returns false for zero-length segments.
fn segment_extends_in(from: (f64, f64), to: (f64, f64), dir: Direction) -> bool {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    match dir {
        Direction::Up => dy < -EPS && dx.abs() < EPS,
        Direction::Down => dy > EPS && dx.abs() < EPS,
        Direction::Left => dx < -EPS && dy.abs() < EPS,
        Direction::Right => dx > EPS && dy.abs() < EPS,
    }
}

/// Route a 2-pin net: single segment when collinear on either axis,
/// otherwise an L-shape via `(b.x, a.y)` (horizontal-then-vertical).
///
/// Inputs are expected to already be on the KiCad schematic grid
/// (1.27 mm) — that is a placer-side invariant, not a routing
/// responsibility. The router preserves the coordinates verbatim.
#[must_use]
pub fn route_two_pin(a: (f64, f64), b: (f64, f64)) -> Vec<Segment> {
    route_two_pin_with_outward(a, None, b, None)
}

/// Route a 2-pin net with per-endpoint outward-direction constraints.
///
/// Each endpoint may carry an [`Direction`] hint describing the
/// direction the pin's stem points away from its symbol body. The
/// routed L-corner is chosen so that the leg incident on a constrained
/// endpoint extends in that endpoint's outward direction. When neither
/// L corner satisfies a constrained endpoint, a one-grid-cell outward
/// stub is prepended (`a → a + (out_a × grid) → corner → b`) so the
/// wire leaves the pin in its outward direction before bending toward
/// the destination.
///
/// `route_two_pin(a, b)` is preserved as a thin wrapper passing
/// `None`/`None` so existing closed-form unit tests continue to pass.
#[must_use]
pub fn route_two_pin_with_outward(
    a: (f64, f64),
    out_a: Option<Direction>,
    b: (f64, f64),
    out_b: Option<Direction>,
) -> Vec<Segment> {
    let (x1, y1) = (a.0, a.1);
    let (x2, y2) = (b.0, b.1);
    if (x1 - x2).abs() < EPS && (y1 - y2).abs() < EPS {
        // Coincident pins: nothing to route.
        return Vec::new();
    }
    if (y1 - y2).abs() < EPS || (x1 - x2).abs() < EPS {
        // Collinear: a single segment. Outward constraints are
        // satisfied iff the segment already extends outward from each
        // constrained endpoint.
        let single = Segment { x1, y1, x2, y2 };
        let ok_a = out_a.is_none_or(|d| segment_extends_in(a, b, d));
        let ok_b = out_b.is_none_or(|d| segment_extends_in(b, a, d));
        if ok_a && ok_b {
            return vec![single];
        }
        // Outward direction conflicts with the collinear axis: detour
        // perpendicular via the constrained endpoint that fails. We
        // emit a stub from `a` (or `b`) outward, then return to the
        // collinear axis and run to the destination.
        return route_with_stub(a, out_a, b, out_b);
    }
    // Non-collinear: two L candidates.
    let horizontal_first = [
        Segment { x1, y1, x2, y2: y1 }, // horizontal first
        Segment { x1: x2, y1, x2, y2 },
    ];
    let vertical_first = [
        Segment { x1, y1, x2: x1, y2 }, // vertical first
        Segment { x1, y1: y2, x2, y2 },
    ];
    let h_first_corner = (x2, y1);
    let v_first_corner = (x1, y2);

    let constraints = u8::from(out_a.is_some()) + u8::from(out_b.is_some());
    let score_h = score_outward(a, h_first_corner, b, out_a, out_b);
    let score_v = score_outward(a, v_first_corner, b, out_a, out_b);

    // If there are no outward constraints, default to the legacy
    // horizontal-first L (matches `route_two_pin(a, b)` behaviour).
    if constraints == 0 {
        return horizontal_first.to_vec();
    }
    // Take a candidate that satisfies every constraint, preferring
    // horizontal-first on a tie.
    if score_h >= constraints {
        return horizontal_first.to_vec();
    }
    if score_v >= constraints {
        return vertical_first.to_vec();
    }
    // Neither L satisfies every constraint: emit an outward stub at
    // the unsatisfied pin so its leg extends outward, then route from
    // the stub endpoint to the other pin with the remaining
    // constraint. `route_with_stub` consumes whichever side it stubs
    // (preferring `out_a`) and recurses.
    route_with_stub(a, out_a, b, out_b)
}

/// Score an L-corner choice by how many constrained endpoints have
/// their incident leg extending in their outward direction. The leg
/// incident on `a` runs `a → corner`; the leg incident on `b` runs
/// `b → corner`.
fn score_outward(
    a: (f64, f64),
    corner: (f64, f64),
    b: (f64, f64),
    out_a: Option<Direction>,
    out_b: Option<Direction>,
) -> u8 {
    let mut score = 0u8;
    if let Some(d) = out_a {
        if segment_extends_in(a, corner, d) {
            score += 1;
        }
    }
    if let Some(d) = out_b {
        if segment_extends_in(b, corner, d) {
            score += 1;
        }
    }
    score
}

/// Fallback when no L corner satisfies the outward constraint at `a`:
/// emit a one-cell stub from `a` in its outward direction, then route
/// from the stub endpoint to `b` (recursing without `a`'s constraint,
/// since the stub has already discharged it). When only `b` is
/// constrained and the L route can't honour it, the stub anchors at
/// `b` instead.
fn route_with_stub(
    a: (f64, f64),
    out_a: Option<Direction>,
    b: (f64, f64),
    out_b: Option<Direction>,
) -> Vec<Segment> {
    if let Some(d) = out_a {
        let mid = step(d, a.0, a.1);
        let mut segs = vec![Segment {
            x1: a.0,
            y1: a.1,
            x2: mid.0,
            y2: mid.1,
        }];
        // Continue from `mid` to `b`. The stub already extended outward
        // at `a`, so drop `out_a`; keep `out_b` so the destination's
        // outward constraint can still steer the rest.
        segs.extend(route_two_pin_with_outward(mid, None, b, out_b));
        return segs;
    }
    if let Some(d) = out_b {
        let mid = step(d, b.0, b.1);
        let mut segs = vec![Segment {
            x1: b.0,
            y1: b.1,
            x2: mid.0,
            y2: mid.1,
        }];
        segs.extend(route_two_pin_with_outward(a, None, mid, None));
        return segs;
    }
    // No constraints to honour: fall back to the plain L. Inline the
    // legacy 2-pin route here to avoid recursing back through
    // `route_two_pin_with_outward`.
    let (x1, y1) = (a.0, a.1);
    let (x2, y2) = (b.0, b.1);
    if (x1 - x2).abs() < EPS && (y1 - y2).abs() < EPS {
        return Vec::new();
    }
    if (y1 - y2).abs() < EPS || (x1 - x2).abs() < EPS {
        return vec![Segment { x1, y1, x2, y2 }];
    }
    vec![
        Segment { x1, y1, x2, y2: y1 },
        Segment { x1: x2, y1, x2, y2 },
    ]
}

/// Route a 3-pin net via Hwang's exact RSMT. The Steiner point is
/// `(median(xs), median(ys))`; each pin connects to it through up to
/// two axis-parallel segments. Degenerate cases (Steiner point
/// coincides with a pin, or two pins share a coordinate with the
/// Steiner point) collapse naturally — no zero-length segments are
/// emitted.
#[must_use]
pub fn route_three_pin(pins: [(f64, f64); 3]) -> Vec<Segment> {
    route_three_pin_with_outward(pins, [None, None, None])
}

/// Route a 3-pin net with optional per-pin outward-direction
/// constraints. The Steiner point is still `(median(xs), median(ys))`;
/// for each pin, the leg from the Steiner point to that pin uses the
/// outward-aware 2-pin helper so the leg incident on the pin extends
/// in the pin's outward direction.
#[must_use]
pub fn route_three_pin_with_outward(
    pins: [(f64, f64); 3],
    outs: [Option<Direction>; 3],
) -> Vec<Segment> {
    let xs = [pins[0].0, pins[1].0, pins[2].0];
    let ys = [pins[0].1, pins[1].1, pins[2].1];

    let mut sx = xs;
    sx.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut sy = ys;
    sy.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let (mx, my) = (sx[1], sy[1]);

    let mut segs: Vec<Segment> = Vec::new();
    for i in 0..3 {
        let pin = (xs[i], ys[i]);
        let steiner = (mx, my);
        if (pin.0 - steiner.0).abs() < EPS && (pin.1 - steiner.1).abs() < EPS {
            continue;
        }
        // Route pin → Steiner with the pin's outward constraint. The
        // Steiner end is unconstrained (it's an internal node).
        segs.extend(route_two_pin_with_outward(pin, outs[i], steiner, None));
    }

    // Coalesce collinear horizontal segments through the Steiner X
    // band: when two pins share Y with the Steiner point and sit on
    // opposite sides of `mx`, the per-pin horizontal segments meet
    // at `mx` and can be merged into a single span. Same for the
    // vertical band. Without this the wire count and total length
    // are still correct, but adjacent segments duplicate the bend
    // at the Steiner point.
    coalesce_at(&mut segs, mx, my);
    segs
}

/// Merge two collinear segments that meet exactly at the Steiner
/// point `(mx, my)` into a single span. Idempotent; no-op when the
/// pair isn't present.
fn coalesce_at(segs: &mut Vec<Segment>, mx: f64, my: f64) {
    // Horizontal pair through (mx, my): two segments on y == my,
    // one ending at x == mx, another starting at x == mx.
    let mut left: Option<usize> = None;
    let mut right: Option<usize> = None;
    for (i, s) in segs.iter().enumerate() {
        if (s.y1 - my).abs() < EPS && (s.y2 - my).abs() < EPS {
            if (s.x2 - mx).abs() < EPS && (s.x1 - mx).abs() > EPS {
                left = Some(i);
            } else if (s.x1 - mx).abs() < EPS && (s.x2 - mx).abs() > EPS {
                right = Some(i);
            }
        }
    }
    if let (Some(l), Some(r)) = (left, right) {
        let merged = Segment {
            x1: segs[l].x1,
            y1: my,
            x2: segs[r].x2,
            y2: my,
        };
        let (a, b) = if l > r { (l, r) } else { (r, l) };
        segs.remove(a);
        segs.remove(b);
        segs.push(merged);
        return;
    }

    // Vertical pair through (mx, my).
    let mut up: Option<usize> = None;
    let mut down: Option<usize> = None;
    for (i, s) in segs.iter().enumerate() {
        if (s.x1 - mx).abs() < EPS && (s.x2 - mx).abs() < EPS {
            if (s.y2 - my).abs() < EPS && (s.y1 - my).abs() > EPS {
                up = Some(i);
            } else if (s.y1 - my).abs() < EPS && (s.y2 - my).abs() > EPS {
                down = Some(i);
            }
        }
    }
    if let (Some(u), Some(d)) = (up, down) {
        let merged = Segment {
            x1: mx,
            y1: segs[u].y1,
            x2: mx,
            y2: segs[d].y2,
        };
        let (a, b) = if u > d { (u, d) } else { (d, u) };
        segs.remove(a);
        segs.remove(b);
        segs.push(merged);
    }
}

/// Route the signal net by pin count, dispatching to the
/// closed-form 2-pin / 3-pin cases or the N-pin Hanan-grid /
/// rectilinear-MST routes for N ≥ 4. Returns `(segments, junctions)`.
///
/// V11 enforcement (no segment may pass through a pin owned by a
/// different net) is **not** applied at construction time — Stage
/// 3c's [`crate::conflict::avoid_foreign_pins`] handles it
/// per-segment so the rerouter can detect and roll back a detour
/// that would collinearly overlap a sibling routed net (the
/// symmetric multivibrator / diff-pair failure mode). A conflict-
/// aware constructor that subsumes both is a v0.2 channel-router
/// work item.
pub(crate) fn route_signal(
    net: &crate::NetSpec,
    foreign_pins: &std::collections::HashSet<(i64, i64)>,
) -> (Vec<Segment>, Vec<(f64, f64)>) {
    match net.pins.len() {
        0 | 1 => (Vec::new(), Vec::new()),
        2 => {
            let a = pin_xy(&net.pins[0]);
            let b = pin_xy(&net.pins[1]);
            (
                route_two_pin_with_outward_avoiding(
                    a,
                    Some(net.pins[0].outward),
                    b,
                    Some(net.pins[1].outward),
                    foreign_pins,
                ),
                Vec::new(),
            )
        }
        3 => {
            let pts = [
                pin_xy(&net.pins[0]),
                pin_xy(&net.pins[1]),
                pin_xy(&net.pins[2]),
            ];
            let outs = [
                Some(net.pins[0].outward),
                Some(net.pins[1].outward),
                Some(net.pins[2].outward),
            ];
            let segs = route_three_pin_with_outward(pts, outs);
            let junctions = steiner_junctions(&pts, &segs);
            (segs, junctions)
        }
        _ => {
            let pins: Vec<(f64, f64)> = net.pins.iter().map(pin_xy).collect();
            let outs: Vec<Option<Direction>> = net.pins.iter().map(|p| Some(p.outward)).collect();
            let segs = route_n_pin_with_outward(&pins, &outs);
            let junctions = compute_junctions(&segs, &pins);
            (segs, junctions)
        }
    }
}

/// 2-pin route variant that, before falling back to an outward stub,
/// checks whether the stub's endpoint would land on a foreign pin. If
/// so, prefer the legacy L (one outward constraint will be violated
/// but the V11 detour cascade will not have to undo our work). Used
/// only by the top-level [`route_signal`] entry; library callers use
/// [`route_two_pin_with_outward`] directly.
#[allow(clippy::cast_possible_truncation)]
fn route_two_pin_with_outward_avoiding(
    a: (f64, f64),
    out_a: Option<Direction>,
    b: (f64, f64),
    out_b: Option<Direction>,
    foreign_pins: &std::collections::HashSet<(i64, i64)>,
) -> Vec<Segment> {
    let qk = |(x, y): (f64, f64)| -> (i64, i64) {
        ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64)
    };
    let stub_lands_on_foreign = |dir: Option<Direction>, pin: (f64, f64)| -> bool {
        let Some(d) = dir else {
            return false;
        };
        foreign_pins.contains(&qk(step(d, pin.0, pin.1)))
    };
    // If we know the stub would land on a foreign pin, suppress the
    // outward constraint at that end. The standard route_two_pin
    // logic then falls back to the legacy L (or honours the
    // remaining constraint).
    let safe_out_a = if stub_lands_on_foreign(out_a, a) {
        None
    } else {
        out_a
    };
    let safe_out_b = if stub_lands_on_foreign(out_b, b) {
        None
    } else {
        out_b
    };
    route_two_pin_with_outward(a, safe_out_a, b, safe_out_b)
}

/// Route a 4+ pin net.
///
/// Dispatch:
/// * **N == 2** / **3** — defer to closed-form helpers (also exposed
///   on this entry point so callers don't special-case).
/// * **4 ≤ N ≤ 9** — rectilinear MST, then Borah-Owens-Irwin
///   Steinerization on the Hanan grid: while a Hanan-grid candidate
///   point exists whose insertion strictly shortens the tree, splice
///   it in. Iterates until no positive gain remains.
/// * **N ≥ 10** — plain rectilinear MST. Acceptable v0.1 floor; none
///   of the existing fixtures has a net this large.
///
/// All inputs and outputs sit on whatever grid the caller chose
/// (typically 1.27 mm). The router never invents fractional offsets:
/// every Steiner candidate is a Hanan grid intersection, hence on
/// the same grid as the inputs.
#[must_use]
pub fn route_n_pin(pins: &[(f64, f64)]) -> Vec<Segment> {
    let outs: Vec<Option<Direction>> = vec![None; pins.len()];
    route_n_pin_with_outward(pins, &outs)
}

/// Outward-aware variant of [`route_n_pin`]. `outs[i]` is the outward
/// direction for `pins[i]`; `None` means unconstrained (Steiner points
/// added by [`steinerize`] are always unconstrained). Each tree edge
/// whose endpoint is a pin uses the outward-aware 2-pin helper so the
/// leg incident on the pin extends outward.
#[must_use]
pub fn route_n_pin_with_outward(pins: &[(f64, f64)], outs: &[Option<Direction>]) -> Vec<Segment> {
    debug_assert_eq!(pins.len(), outs.len());
    match pins.len() {
        0 | 1 => Vec::new(),
        2 => route_two_pin_with_outward(pins[0], outs[0], pins[1], outs[1]),
        3 => route_three_pin_with_outward([pins[0], pins[1], pins[2]], [outs[0], outs[1], outs[2]]),
        n if n <= 9 => {
            let mut tree = rectilinear_mst(pins);
            steinerize(&mut tree, pins);
            tree_to_segments(&tree, pins.len(), outs)
        }
        _ => {
            let tree = rectilinear_mst(pins);
            tree_to_segments(&tree, pins.len(), outs)
        }
    }
}

/// A node in the working tree: either an input pin (carrying its
/// world coordinates) or a Steiner point.
#[derive(Debug, Clone, Copy)]
struct Node {
    x: f64,
    y: f64,
}

/// Working tree edge — pair of node indices into a parallel `Vec<Node>`.
#[derive(Debug, Clone, Copy)]
struct Edge(usize, usize);

fn manhattan(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).abs() + (a.1 - b.1).abs()
}

/// Prim's MST over the rectilinear (Manhattan) distance metric.
/// Returns `(nodes, edges)` where `nodes[i]` mirrors `pins[i]`.
fn rectilinear_mst(pins: &[(f64, f64)]) -> (Vec<Node>, Vec<Edge>) {
    let n = pins.len();
    let nodes: Vec<Node> = pins.iter().map(|&(x, y)| Node { x, y }).collect();
    if n <= 1 {
        return (nodes, Vec::new());
    }
    let mut in_tree = vec![false; n];
    let mut best: Vec<f64> = vec![f64::INFINITY; n];
    let mut parent: Vec<usize> = vec![usize::MAX; n];
    in_tree[0] = true;
    for j in 1..n {
        best[j] = manhattan(pins[0], pins[j]);
        parent[j] = 0;
    }
    let mut edges = Vec::with_capacity(n - 1);
    for _ in 1..n {
        let mut pick = usize::MAX;
        let mut pick_d = f64::INFINITY;
        for j in 0..n {
            if !in_tree[j] && best[j] < pick_d {
                pick = j;
                pick_d = best[j];
            }
        }
        if pick == usize::MAX {
            break;
        }
        in_tree[pick] = true;
        edges.push(Edge(parent[pick], pick));
        for j in 0..n {
            if !in_tree[j] {
                let d = manhattan(pins[pick], pins[j]);
                if d < best[j] {
                    best[j] = d;
                    parent[j] = pick;
                }
            }
        }
    }
    (nodes, edges)
}

/// Total Manhattan length of a tree.
fn tree_length(nodes: &[Node], edges: &[Edge]) -> f64 {
    edges
        .iter()
        .map(|e| (nodes[e.0].x - nodes[e.1].x).abs() + (nodes[e.0].y - nodes[e.1].y).abs())
        .sum()
}

/// Borah-Owens-Irwin style Steinerization on the Hanan grid.
///
/// Hanan grid: every (x, y) where x ∈ {pin xs} and y ∈ {pin ys}.
/// The optimal RSMT has all Steiner points on Hanan-grid
/// intersections (Hanan 1966).
///
/// Iterative refinement: at each pass, for every Hanan-grid
/// candidate not already in the tree, try inserting it as a new
/// Steiner node connected to its Manhattan-nearest existing node,
/// then re-MST and keep the change if length strictly drops. Stops
/// when no candidate improves the tree.
///
/// O(passes * H * (N + S)^2) where H = Hanan grid size ≤ N². For
/// N ≤ 9 this is comfortably under a millisecond.
fn steinerize(tree: &mut (Vec<Node>, Vec<Edge>), pins: &[(f64, f64)]) {
    let xs: Vec<f64> = unique_sorted(pins.iter().map(|p| p.0));
    let ys: Vec<f64> = unique_sorted(pins.iter().map(|p| p.1));
    let mut hanan: Vec<(f64, f64)> = Vec::with_capacity(xs.len() * ys.len());
    for &x in &xs {
        for &y in &ys {
            hanan.push((x, y));
        }
    }

    // Cap iterations defensively — improvement is monotone and each
    // step strictly decreases length, but f64 arithmetic plus the
    // Hanan-grid finite candidate set means we should converge fast.
    for _ in 0..32 {
        let baseline = tree_length(&tree.0, &tree.1);
        let mut best_gain = 0.0_f64;
        let mut best_candidate: Option<(f64, f64)> = None;
        for &(cx, cy) in &hanan {
            // Skip if this point is already a node in the tree.
            if tree
                .0
                .iter()
                .any(|n| (n.x - cx).abs() < EPS && (n.y - cy).abs() < EPS)
            {
                continue;
            }
            // Trial: append the candidate as a new node, rebuild the
            // MST over (pins + Steiner) and measure.
            let trial_pts = collect_points(&tree.0, (cx, cy));
            let trial = rectilinear_mst(&trial_pts);
            let trial_len = tree_length(&trial.0, &trial.1);
            let gain = baseline - trial_len;
            if gain > best_gain + EPS {
                best_gain = gain;
                best_candidate = Some((cx, cy));
            }
        }
        if let Some((cx, cy)) = best_candidate {
            // Commit: rebuild the tree with the new Steiner point.
            let trial_pts = collect_points(&tree.0, (cx, cy));
            let new_tree = rectilinear_mst(&trial_pts);
            *tree = new_tree;
        } else {
            break;
        }
    }

    // Pin order is preserved by construction (rectilinear_mst keeps
    // pins[0..N] as nodes[0..N]); Steiner additions are appended.
    // Drop degree-1 Steiner nodes (a Steiner point that ended up as
    // a leaf is pure overhead). The pin count caps where pruning
    // applies.
    prune_degree_one_steiner(tree, pins.len());
}

fn unique_sorted<I: Iterator<Item = f64>>(it: I) -> Vec<f64> {
    let mut v: Vec<f64> = it.collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v.dedup_by(|a, b| (*a - *b).abs() < EPS);
    v
}

fn collect_points(nodes: &[Node], extra: (f64, f64)) -> Vec<(f64, f64)> {
    let mut v: Vec<(f64, f64)> = nodes.iter().map(|n| (n.x, n.y)).collect();
    v.push(extra);
    v
}

/// Remove Steiner nodes (index ≥ `pin_count`) of degree 1. Iterates
/// because removing a leaf may reduce a neighbour to degree 1.
fn prune_degree_one_steiner(tree: &mut (Vec<Node>, Vec<Edge>), pin_count: usize) {
    loop {
        let n = tree.0.len();
        let mut deg = vec![0_usize; n];
        for e in &tree.1 {
            deg[e.0] += 1;
            deg[e.1] += 1;
        }
        let victim: Option<usize> = deg
            .iter()
            .enumerate()
            .skip(pin_count)
            .take(n - pin_count)
            .find_map(|(i, &d)| if d <= 1 { Some(i) } else { None });
        let Some(v) = victim else { break };
        // Drop edges touching `v` and the node itself.
        tree.1.retain(|e| e.0 != v && e.1 != v);
        tree.0.remove(v);
        // Re-index edges past v.
        for e in &mut tree.1 {
            if e.0 > v {
                e.0 -= 1;
            }
            if e.1 > v {
                e.1 -= 1;
            }
        }
    }
}

/// Convert tree edges to L-shaped axis-parallel segment pairs.
/// Each MST edge becomes one or two segments depending on whether
/// the endpoints share a coordinate.
fn tree_to_segments(
    tree: &(Vec<Node>, Vec<Edge>),
    pin_count: usize,
    outs: &[Option<Direction>],
) -> Vec<Segment> {
    let mut segs: Vec<Segment> = Vec::new();
    let out_of = |idx: usize| -> Option<Direction> {
        if idx < pin_count {
            outs.get(idx).copied().flatten()
        } else {
            None
        }
    };
    for e in &tree.1 {
        let a = tree.0[e.0];
        let b = tree.0[e.1];
        if (a.x - b.x).abs() < EPS && (a.y - b.y).abs() < EPS {
            continue;
        }
        segs.extend(route_two_pin_with_outward(
            (a.x, a.y),
            out_of(e.0),
            (b.x, b.y),
            out_of(e.1),
        ));
    }
    segs
}

/// Find junction points: any coordinate where ≥ 3 segment endpoints
/// meet, restricted to actual pin or Steiner points (an L-bend with
/// no third branch is not a junction).
fn compute_junctions(segs: &[Segment], pins: &[(f64, f64)]) -> Vec<(f64, f64)> {
    use std::collections::HashMap;
    // Quantise to a grid so we can hash f64 endpoints reliably.
    // Inputs sit on the 1.27 mm KiCad grid (max ~hundreds of mm),
    // so `x * 1000` is comfortably within i64 range.
    #[allow(clippy::cast_possible_truncation)]
    let key = |x: f64, y: f64| -> (i64, i64) {
        ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64)
    };
    let mut counts: HashMap<(i64, i64), (f64, f64, usize)> = HashMap::new();
    for s in segs {
        for (x, y) in [(s.x1, s.y1), (s.x2, s.y2)] {
            let entry = counts.entry(key(x, y)).or_insert((x, y, 0));
            entry.2 += 1;
        }
    }
    let mut out = Vec::new();
    for (_, (x, y, c)) in counts {
        if c >= 3 {
            // Only emit at points that aren't dangling — every junction
            // by definition has ≥ 3 endpoints meeting, so it is interior.
            // Skip if this is a non-pin, non-corner point with degree 2
            // (handled by the >= 3 filter already). Pins themselves can
            // also be junction points if 3 segments meet there (T-pin).
            let _ = pins;
            out.push((x, y));
        }
    }
    out
}

fn pin_xy(p: &PinRef) -> (f64, f64) {
    (p.x_mm, p.y_mm)
}

/// Compute junction points for a 3-pin Steiner tree. A junction is
/// emitted at the Steiner point when at least three segment endpoints
/// meet there — this excludes the degenerate cases where the
/// Steiner point coincides with a pin (yielding a plain L-shape).
fn steiner_junctions(pts: &[(f64, f64); 3], segs: &[Segment]) -> Vec<(f64, f64)> {
    let xs = [pts[0].0, pts[1].0, pts[2].0];
    let ys = [pts[0].1, pts[1].1, pts[2].1];
    let mut sx = xs;
    sx.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut sy = ys;
    sy.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let (mx, my) = (sx[1], sy[1]);

    // Steiner point coincides with a pin iff (mx, my) equals one of
    // the pins exactly. In that case the routing collapses to two
    // segments and no branch junction is needed.
    let coincident = (0..3).any(|i| (xs[i] - mx).abs() < EPS && (ys[i] - my).abs() < EPS);
    if coincident {
        return Vec::new();
    }

    // Count segment endpoints meeting at (mx, my). A junction is
    // needed only when three or more meet (T-junction / cross).
    let mut hits = 0usize;
    for s in segs {
        if (s.x1 - mx).abs() < EPS && (s.y1 - my).abs() < EPS {
            hits += 1;
        }
        if (s.x2 - mx).abs() < EPS && (s.y2 - my).abs() < EPS {
            hits += 1;
        }
    }
    if hits >= 3 {
        vec![(mx, my)]
    } else {
        Vec::new()
    }
}

/// Render a `Segment` as a `(wire (pts (xy …) (xy …)))` S-expr.
pub(crate) fn segment_to_sexpr(s: &Segment) -> Sexpr {
    let txt = format!(
        "(wire (pts (xy {:.2} {:.2}) (xy {:.2} {:.2})))",
        s.x1, s.y1, s.x2, s.y2
    );
    lexpr::from_str(&txt).expect("wire sexpr parses")
}

/// Render a junction point as a `(junction (at …))` S-expr.
pub(crate) fn junction_sexpr(p: (f64, f64)) -> Sexpr {
    let txt = format!("(junction (at {:.2} {:.2}))", p.0, p.1);
    lexpr::from_str(&txt).expect("junction sexpr parses")
}
