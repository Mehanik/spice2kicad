//! The **V16 ink graph** — the one definition of B (bends) and J
//! (branches), shared by every binary that measures them.
//!
//! This module was extracted verbatim from
//! `tests/wire_geometry.rs`, which still owns the *ratchet*; it now owns
//! only the assertion, not the measurement. The extraction exists so the
//! bend **lower-bound** instrument (`tests/bend_bound.rs`) grades against
//! byte-identical geometry rather than a second implementation of the
//! same idea. ADR-23 D2 records why: "duplication of a measurement is the
//! specific failure this project keeps paying for" (MEMORY "verify what a
//! number measures") — a re-implementation drifts from the verifier it
//! claims to mirror, silently.
//!
//! # The metric
//!
//! Take the union of every emitted `(wire …)` segment; group by line
//! (same Y for horizontals, same X for verticals); merge
//! touching-or-overlapping collinear spans into **maximal straight
//! runs**. Vertices are run endpoints plus run–run incidences. Rays are
//! counted exactly the way `spice-route/src/cleanup.rs::rays_at` does: a
//! run *ending* at the point contributes one ray, a run whose *strict
//! interior* contains it contributes two (it passes through).
//!
//! * **B** — vertices with exactly 2 rays, one horizontal + one vertical.
//! * **J** — vertices with 3 rays, plus 4-ray vertices carrying a
//!   `(junction …)` dot. 4-ray vertices *without* a dot are inter-net
//!   crossings and belong to the crossing ratchet.
//!
//! The normalisation into maximal runs is what makes the count invariant
//! under `cleanup.rs`'s deliberate re-segmentation of identical ink — see
//! `docs/invariants.md` V16 and the module docs of `tests/wire_geometry.rs`
//! for the full rationale, the anti-gaming argument, and the gates this
//! soundness depends on.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use lexpr::Value;

/// Quantised coordinate: micrometres. The emitter writes millimetres on
/// a 1.27 mm grid, so µm is far finer than any legitimate distinction
/// and makes endpoint identity an exact integer comparison.
pub type Q = i64;

/// A quantised point (x, y) in micrometres.
pub type Pt = (Q, Q);

/// One emitted `(wire …)` segment as a quantised endpoint pair.
pub type RawSeg = (Pt, Pt);

#[allow(clippy::cast_possible_truncation)]
pub fn q(mm: f64) -> Q {
    (mm * 1000.0).round() as i64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    H,
    V,
}

/// A maximal straight run of ink: `axis`, the line it sits on
/// (`line` = Y for horizontal, X for vertical), and the closed span
/// `[lo, hi]` along the other coordinate.
#[derive(Debug, Clone, Copy)]
pub struct Run {
    pub axis: Axis,
    pub line: Q,
    pub lo: Q,
    pub hi: Q,
}

impl Run {
    pub fn endpoints(self) -> [Pt; 2] {
        match self.axis {
            Axis::H => [(self.lo, self.line), (self.hi, self.line)],
            Axis::V => [(self.line, self.lo), (self.line, self.hi)],
        }
    }

    /// True iff `(x, y)` lies on this run's strict interior.
    pub fn strictly_interior(self, x: Q, y: Q) -> bool {
        match self.axis {
            Axis::H => y == self.line && x > self.lo && x < self.hi,
            Axis::V => x == self.line && y > self.lo && y < self.hi,
        }
    }

    /// True iff `(x, y)` lies anywhere on this run, endpoints included.
    pub fn contains(self, x: Q, y: Q) -> bool {
        match self.axis {
            Axis::H => y == self.line && x >= self.lo && x <= self.hi,
            Axis::V => x == self.line && y >= self.lo && y <= self.hi,
        }
    }
}

/// Every `(wire (pts (xy …) (xy …)))` segment under `root`, as a
/// quantised endpoint pair. Diagonals and zero-length segments are
/// reported separately by the caller.
pub fn raw_wire_segments(root: &Value) -> Vec<RawSeg> {
    let mut out = Vec::new();
    for w in children(root, "wire") {
        let Some(pts) = find_child(w, "pts") else {
            continue;
        };
        let xys: Vec<&Value> = list_iter(pts)
            .filter(|c| c.is_list() && head(c) == Some("xy"))
            .collect();
        if xys.len() < 2 {
            continue;
        }
        let (Some(a), Some(b)) = (xy(xys[0]), xy(xys[1])) else {
            continue;
        };
        out.push((a, b));
    }
    out
}

pub fn xy(v: &Value) -> Option<Pt> {
    let mut it = list_iter(v);
    it.next()?; // head "xy"
    let x = as_f64(it.next()?)?;
    let y = as_f64(it.next()?)?;
    Some((q(x), q(y)))
}

/// Positions of every `(junction (at x y) …)` on the sheet.
pub fn junction_positions(root: &Value) -> HashSet<Pt> {
    let mut out = HashSet::new();
    for j in children(root, "junction") {
        let Some(at) = find_child(j, "at") else {
            continue;
        };
        let mut it = list_iter(at);
        it.next();
        let (Some(x), Some(y)) = (it.next().and_then(as_f64), it.next().and_then(as_f64)) else {
            continue;
        };
        out.insert((q(x), q(y)));
    }
    out
}

/// Normalise the raw segment soup into **maximal straight runs**: group
/// by (axis, line), then merge touching-or-overlapping spans. This is
/// the step that makes the metric invariant under `cleanup.rs`'s
/// re-segmentation.
pub fn maximal_runs(segments: &[RawSeg]) -> Vec<Run> {
    let mut by_line: HashMap<(Axis, Q), Vec<(Q, Q)>> = HashMap::new();
    for &((x1, y1), (x2, y2)) in segments {
        if x1 == x2 && y1 == y2 {
            continue; // zero-length: no ink
        }
        let (axis, line, lo, hi) = if y1 == y2 {
            (Axis::H, y1, x1.min(x2), x1.max(x2))
        } else {
            debug_assert_eq!(x1, x2, "diagonal must be rejected before this point");
            (Axis::V, x1, y1.min(y2), y1.max(y2))
        };
        by_line.entry((axis, line)).or_default().push((lo, hi));
    }

    let mut runs = Vec::new();
    for ((axis, line), mut spans) in by_line {
        spans.sort_unstable();
        let mut cur = spans[0];
        for &(lo, hi) in &spans[1..] {
            if lo <= cur.1 {
                // touching or overlapping -> same maximal run
                cur.1 = cur.1.max(hi);
            } else {
                runs.push(Run {
                    axis,
                    line,
                    lo: cur.0,
                    hi: cur.1,
                });
                cur = (lo, hi);
            }
        }
        runs.push(Run {
            axis,
            line,
            lo: cur.0,
            hi: cur.1,
        });
    }
    runs.sort_unstable_by_key(|r| (r.line, r.lo, r.hi));
    runs
}

/// Candidate vertices of the ink graph: every run endpoint, plus every
/// point where a horizontal and a vertical run touch.
pub fn candidate_vertices(runs: &[Run]) -> Vec<Pt> {
    let mut set: HashSet<Pt> = HashSet::new();
    for r in runs {
        for p in r.endpoints() {
            set.insert(p);
        }
    }
    for a in runs.iter().filter(|r| r.axis == Axis::H) {
        for b in runs.iter().filter(|r| r.axis == Axis::V) {
            let p = (b.line, a.line);
            if a.contains(p.0, p.1) && b.contains(p.0, p.1) {
                set.insert(p);
            }
        }
    }
    let mut v: Vec<Pt> = set.into_iter().collect();
    v.sort_unstable();
    v
}

/// Rays meeting at `(x, y)`, and which axes contribute them — counted
/// exactly as `spice-route/src/cleanup.rs::rays_at` does: an endpoint
/// contributes one ray, a strict-interior pass-through contributes two.
pub fn rays_at(runs: &[Run], x: Q, y: Q) -> (usize, Vec<Axis>) {
    let mut rays = 0usize;
    let mut axes = Vec::new();
    for r in runs {
        let at_end = r.endpoints().contains(&(x, y));
        if at_end {
            rays += 1;
            axes.push(r.axis);
        } else if r.strictly_interior(x, y) {
            rays += 2;
            axes.push(r.axis);
            axes.push(r.axis);
        }
    }
    (rays, axes)
}

/// True when `(x, y)` is an L-corner of the ink: exactly two rays, one
/// horizontal and one vertical.
pub fn is_bend(runs: &[Run], x: Q, y: Q) -> bool {
    let (rays, axes) = rays_at(runs, x, y);
    rays == 2
        && axes.iter().filter(|a| **a == Axis::H).count() == 1
        && axes.iter().filter(|a| **a == Axis::V).count() == 1
}

/// Measured V16 quantities for one sheet.
#[derive(Debug, Default, Clone, Copy)]
pub struct InkCounts {
    /// B — L-corners of the ink.
    pub bends: u32,
    /// J — branch points (T, or same-net cross carrying a junction dot).
    pub branches: u32,
    /// 4-ray vertices with no junction dot: inter-net crossings. NOT
    /// part of B or J — cross-checked against the crossing ratchet.
    pub inter_net_crossings: u32,
    /// Diagnostics only.
    pub raw_segments: u32,
    pub runs: u32,
}

/// Reject any diagonal segment: axis-alignment is what makes
/// ray-counting sound, and nothing in the pipeline emits diagonals, so
/// this is a free tripwire.
pub fn reject_diagonals(segs: &[RawSeg]) -> Result<(), String> {
    let diagonals: Vec<_> = segs
        .iter()
        .filter(|((x1, y1), (x2, y2))| x1 != x2 && y1 != y2)
        .collect();
    if diagonals.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} diagonal wire segment(s) emitted, e.g. {:?} — V16's ink graph \
         assumes rectilinear wires",
        diagonals.len(),
        diagonals[0]
    ))
}

pub fn measure(root: &Value) -> Result<InkCounts, String> {
    let segs = raw_wire_segments(root);
    reject_diagonals(&segs)?;

    let runs = maximal_runs(&segs);
    let dots = junction_positions(root);

    let mut c = InkCounts {
        raw_segments: u32::try_from(segs.len()).unwrap_or(u32::MAX),
        runs: u32::try_from(runs.len()).unwrap_or(u32::MAX),
        ..InkCounts::default()
    };

    for (x, y) in candidate_vertices(&runs) {
        let (rays, axes) = rays_at(&runs, x, y);
        match rays {
            2 => {
                let h = axes.iter().filter(|a| **a == Axis::H).count();
                let v = axes.iter().filter(|a| **a == Axis::V).count();
                if h == 1 && v == 1 {
                    c.bends += 1;
                }
            }
            3 => c.branches += 1,
            n if n >= 4 => {
                if dots.contains(&(x, y)) {
                    c.branches += 1;
                } else {
                    c.inter_net_crossings += 1;
                }
            }
            _ => {}
        }
    }
    Ok(c)
}

// --- connected components of ink -----------------------------------------

/// Component id per **run** index, on KiCad's electrical connectivity.
///
/// Connectivity is computed on the *raw segments*, not on the merged
/// runs, because **KiCad connects wires only at endpoints** — the rule
/// `spice-route/src/cleanup.rs::split_at_interior_attachments` exists to
/// serve, splitting a run at every same-net attachment so the attachment
/// becomes an endpoint of both. Merging into maximal runs deliberately
/// destroys that endpoint information (it is what makes B invariant under
/// re-segmentation), so connectivity must be read before the merge.
///
/// The difference is not academic. On `two_stage_amp` a run-level
/// "they touch, so they join" rule yields **4** components — one of them
/// carrying the `b2`, `c2` and `e2` labels at once — because ten wire
/// ends land on the *interior* of a foreign net's wire, which looks like
/// a join and is not one. The endpoint rule yields **6**, exactly one per
/// labelled net.
///
/// Every **bend** vertex has exactly two rays and, by the vertex-profile
/// fact below, both of its runs *end* there — so their segments share
/// that endpoint and lie in one component. A bend therefore belongs to
/// exactly one component and `B_total = Σ_components B(component)`.
/// `bend_bound.rs` asserts that identity on real geometry rather than
/// trusting this comment.
///
/// **Vertex profiles.** At any point each axis contributes 0 rays (no
/// run), 1 ray (exactly one run *ends* — two collinear runs meeting at a
/// point would have been merged into one maximal run) or 2 rays (one run
/// passes through). So the only profiles are leaf (1 ray), bend (1+1,
/// necessarily one per axis), T (1+2) and crossing (2+2). In particular a
/// 2-ray vertex is *always* one horizontal and one vertical ray, and both
/// of its runs end there.
///
/// One wrinkle: a maximal run can be assembled from segments that share
/// no endpoint — two collinear pieces that *overlap*. Those pieces are
/// one indivisible run here, so the components they belong to are
/// **unioned**: a run always lies in exactly one component. (Merging can
/// only coarsen the partition, which weakens any derived bound and never
/// invalidates it — the extremal lemma holds for any union of runs whose
/// leaves are all anchors.) The second return value counts the runs that
/// forced such a merge, because on real output they are the *cross-net
/// collinear overlap* defect: on `two_stage_amp` there are exactly two,
/// at `x = 57.15` and `y = 87.63`, which is precisely the registered
/// `no_cross_net_collinear_wire_overlap` expected failure — the ink graph
/// rediscovers it from geometry alone.
pub fn run_components(runs: &[Run], segments: &[RawSeg]) -> (Vec<usize>, usize) {
    fn find(uf: &mut [usize], mut x: usize) -> usize {
        while uf[x] != x {
            uf[x] = uf[uf[x]];
            x = uf[x];
        }
        x
    }

    // Union-find over segment endpoints: two segments join iff they share
    // an endpoint.
    let mut ids: HashMap<Pt, usize> = HashMap::new();
    let mut uf: Vec<usize> = Vec::new();
    let mut id_of = |p: Pt, uf: &mut Vec<usize>| -> usize {
        *ids.entry(p).or_insert_with(|| {
            uf.push(uf.len());
            uf.len() - 1
        })
    };
    let mut seg_root: Vec<usize> = Vec::with_capacity(segments.len());
    for &(a, b) in segments {
        let (ia, ib) = (id_of(a, &mut uf), id_of(b, &mut uf));
        let (ra, rb) = (find(&mut uf, ia), find(&mut uf, ib));
        if ra != rb {
            uf[ra] = rb;
        }
        seg_root.push(ia);
    }

    // Which segments make up each run.
    let segs_of_run: Vec<Vec<usize>> = runs
        .iter()
        .map(|run| {
            segments
                .iter()
                .enumerate()
                .filter(|&(_, &(a, b))| {
                    a != b
                        && run.contains(a.0, a.1)
                        && run.contains(b.0, b.1)
                        && match run.axis {
                            // Collinear with the run, not merely touching it.
                            Axis::H => a.1 == run.line && b.1 == run.line,
                            Axis::V => a.0 == run.line && b.0 == run.line,
                        }
                })
                .map(|(i, _)| i)
                .collect()
        })
        .collect();

    // A run is indivisible: union whatever its pieces belong to.
    let mut overlap_merges = 0usize;
    for members in &segs_of_run {
        let mut merged_here = false;
        for w in members.windows(2) {
            let (ra, rb) = (find(&mut uf, seg_root[w[0]]), find(&mut uf, seg_root[w[1]]));
            if ra != rb {
                uf[ra] = rb;
                merged_here = true;
            }
        }
        if merged_here {
            overlap_merges += 1;
        }
    }

    let comps = segs_of_run
        .iter()
        .map(|members| {
            members
                .first()
                .map_or(usize::MAX, |&i| find(&mut uf, seg_root[i]))
        })
        .collect();
    (comps, overlap_merges)
}

// --- lexpr helpers --------------------------------------------------------

pub fn head(v: &Value) -> Option<&str> {
    as_str(list_iter(v).next()?)
}

pub fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    v.list_iter().map_or_else(
        || Box::new(std::iter::empty()) as Box<dyn Iterator<Item = &Value>>,
        |it| Box::new(it),
    )
}

pub fn children<'a>(v: &'a Value, name: &str) -> Vec<&'a Value> {
    list_iter(v)
        .filter(|c| c.is_list() && head(c) == Some(name))
        .collect()
}

pub fn find_child<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    children(v, name).into_iter().next()
}

pub fn as_str(v: &Value) -> Option<&str> {
    v.as_symbol()
        .or_else(|| v.as_str())
        .or_else(|| v.as_keyword())
}

pub fn as_f64(v: &Value) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))
}
