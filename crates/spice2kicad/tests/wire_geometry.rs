//! **V16 — wire rectilinearity** (Tier 2).
//!
//! The project owner's observation: "number of wire segments and corners
//! is an important metric — it's simple to read a circuit when wires are
//! minimal, straight, and connect directly the elements which are
//! connected." This file turns that into a falsifiable, non-gameable
//! per-fixture ratchet.
//!
//! # Why NOT raw segment count
//!
//! The emitted `(wire …)` count is a **Tier-0 correctness artifact**, not
//! a quality signal. `crates/spice-route/src/cleanup.rs` deliberately
//! re-segments the same ink:
//!
//! * `split_at_interior_attachments` SPLITS a run at same-net attachment
//!   vertices — KiCad connects wires only at *endpoints*, so more
//!   segments is *more correct*;
//! * `coalesce_collinear` merges abutting collinear pairs;
//! * `collapse_collinear_overlaps` replaces overlapping runs with a
//!   vertex-preserving non-overlapping cover.
//!
//! Measured on `common_emitter`: 20 raw segments whose visible ink is
//! 16 maximal straight runs. A ratchet on raw segments would create
//! optimization pressure AGAINST a Tier-0 pass. So the counted quantity
//! must be **invariant under re-segmentation of identical ink**.
//!
//! # The metric — defined on the "ink graph"
//!
//! Take the union of every emitted wire segment; group by line (same X
//! for verticals, same Y for horizontals); merge touching-or-overlapping
//! collinear spans into **maximal straight runs**. Vertices are run
//! endpoints plus run–run incidences. Rays are counted exactly the way
//! `cleanup.rs::rays_at` does: a run *ending* at the point contributes
//! one ray, a run whose *strict interior* contains it contributes two
//! (it passes through).
//!
//! * **B — bend count**: vertices with exactly 2 rays, one horizontal +
//!   one vertical. These are the L-corners of the ink. PRIMARY ratchet.
//! * **J — branch count**: vertices with 3 rays (a T), plus 4-ray
//!   vertices carrying a `(junction …)` dot (a same-net cross). 4-ray
//!   vertices *without* a dot are **inter-net crossings** and belong to
//!   the existing crossing ratchet
//!   (`placement_quality.rs::crossing_count_within_budget_across_fixtures`),
//!   NOT to J.
//!
//! B and J stay **separate** ratchets. Folding J into B would be wrong:
//! a k-pin Steiner tree topologically needs ≥ k−2 branch points, so a
//! combined number would penalise trunk-and-taps — often the most
//! readable form.
//!
//! Any **diagonal** wire segment is an outright failure. Axis-alignment
//! is what makes ray-counting sound, and nothing in the pipeline emits
//! diagonals today, so it is a free tripwire.
//!
//! # Deliberately NOT ratcheted
//!
//! * raw segment count (see above);
//! * bends-per-net — a gameable denominator: adding trivial nets lowers
//!   the average;
//! * a *rewarded* count of "nets routed straight" — gameable: a V4
//!   hierarchical-port name-jump label pair can mint a new 'straight'
//!   component out of nothing.
//!
//! Absolute per-fixture totals only.
//!
//! # Anti-gaming — and the gates this soundness DEPENDS ON
//!
//! This project has been burned by verifiers satisfiable with degenerate
//! geometry (a V5 counter that credited an "outward" wire without
//! checking its far end connected anything; verifiers that were
//! byte-identical to the model they graded). B and J are **cost-shaped**
//! — they count defects over the whole artifact — not credit-shaped, so
//! dead or decorative geometry can only ever ADD rays, never remove a
//! bend. There is no way to score better by drawing *more*.
//!
//! But that soundness is CONDITIONAL on the lower gates staying hard.
//! With them disabled, "delete all the wires" or "replace every wire
//! with a label" would score a perfect B = J = 0. The dependencies, all
//! of which must remain enforced for this ratchet to mean anything:
//!
//! 1. **Tier-0 connectivity verification** — the CLI verifies the
//!    emitted schematic's connectivity against KiCad after every
//!    conversion, so ink cannot simply be deleted.
//! 2. **`no_dangling_whiskers_across_fixtures`** (budget 0,
//!    `electrical_safety.rs`) — no stub may hang off nothing.
//! 3. **V4 label policy** (`labels.rs`) — ≤ 1 plain label per net per
//!    sheet (2 only for a hierarchical name-jump pair), so connectivity
//!    cannot migrate wholesale from wires into labels.
//!
//! Do not land this ratchet into a tree where any of those three is
//! disabled or weakened.
//!
//! # Subordination (V16 is Tier 2, and stays there)
//!
//! V16 is a continuous quality gradient with no single correct value,
//! so by CLAUDE.md's constraints-vs-costs decision rule it is Tier 2 —
//! same tier as V5/V6/V7 — and never a hard constraint. It must stay
//! subordinate to Tier 0/1: the globally bend-minimal route through a
//! symbol body (V12) or across a label (V13) is *worse* than a 2-bend
//! detour around them. That is precisely why V16 is verifier-shaped
//! (non-regression only) and is NEVER an in-loop objective.
//!
//! Known floor: bend-minimisation and V5-outward genuinely conflict.
//! `rc_lowpass`'s two `out` pins share a Y and sit 3.81 mm apart — a
//! 0-bend direct wire exists, but both pins face up and V5 says wires
//! leave along the pin axis, giving a 2-bend U. Both are Tier 2;
//! precedence is declared in `docs/invariants.md`: V5-outward wins the
//! first grid step, and B ratchets against *measured reality*, not a
//! theoretical zero. Expect legitimate per-net floors of 2 for
//! same-facing aligned pins.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use common::spice_to_kicad;
use lexpr::Value;

// --- driver bits (mirrors placement_quality.rs) --------------------------

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spice2kicad-wg-{pid}-{seq}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn parse_sch(sch: &Path) -> Value {
    let src = std::fs::read_to_string(sch).expect("read sch");
    lexpr::from_str(&src).expect("parse sch as lexpr")
}

// --- lexpr helpers -------------------------------------------------------

fn head(v: &Value) -> Option<&str> {
    as_str(list_iter(v).next()?)
}

fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    v.list_iter().map_or_else(
        || Box::new(std::iter::empty()) as Box<dyn Iterator<Item = &Value>>,
        |it| Box::new(it),
    )
}

fn children<'a>(v: &'a Value, name: &str) -> Vec<&'a Value> {
    list_iter(v)
        .filter(|c| c.is_list() && head(c) == Some(name))
        .collect()
}

fn find_child<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    children(v, name).into_iter().next()
}

fn as_str(v: &Value) -> Option<&str> {
    v.as_symbol()
        .or_else(|| v.as_str())
        .or_else(|| v.as_keyword())
}

fn as_f64(v: &Value) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))
}

// --- the ink graph -------------------------------------------------------

/// Quantised coordinate: micrometres. The emitter writes millimetres on
/// a 1.27 mm grid, so µm is far finer than any legitimate distinction
/// and makes endpoint identity an exact integer comparison.
type Q = i64;

/// A quantised point (x, y) in micrometres.
type Pt = (Q, Q);

/// One emitted `(wire …)` segment as a quantised endpoint pair.
type RawSeg = (Pt, Pt);

#[allow(clippy::cast_possible_truncation)]
fn q(mm: f64) -> Q {
    (mm * 1000.0).round() as i64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Axis {
    H,
    V,
}

/// A maximal straight run of ink: `axis`, the line it sits on
/// (`line` = Y for horizontal, X for vertical), and the closed span
/// `[lo, hi]` along the other coordinate.
#[derive(Debug, Clone, Copy)]
struct Run {
    axis: Axis,
    line: Q,
    lo: Q,
    hi: Q,
}

impl Run {
    fn endpoints(self) -> [Pt; 2] {
        match self.axis {
            Axis::H => [(self.lo, self.line), (self.hi, self.line)],
            Axis::V => [(self.line, self.lo), (self.line, self.hi)],
        }
    }

    /// True iff `(x, y)` lies on this run's strict interior.
    fn strictly_interior(self, x: Q, y: Q) -> bool {
        match self.axis {
            Axis::H => y == self.line && x > self.lo && x < self.hi,
            Axis::V => x == self.line && y > self.lo && y < self.hi,
        }
    }

    /// True iff `(x, y)` lies anywhere on this run, endpoints included.
    fn contains(self, x: Q, y: Q) -> bool {
        match self.axis {
            Axis::H => y == self.line && x >= self.lo && x <= self.hi,
            Axis::V => x == self.line && y >= self.lo && y <= self.hi,
        }
    }
}

/// Every `(wire (pts (xy …) (xy …)))` segment under `root`, as a
/// quantised endpoint pair. Diagonals and zero-length segments are
/// reported separately by the caller.
fn raw_wire_segments(root: &Value) -> Vec<RawSeg> {
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

fn xy(v: &Value) -> Option<Pt> {
    let mut it = list_iter(v);
    it.next()?; // head "xy"
    let x = as_f64(it.next()?)?;
    let y = as_f64(it.next()?)?;
    Some((q(x), q(y)))
}

/// Positions of every `(junction (at x y) …)` on the sheet.
fn junction_positions(root: &Value) -> HashSet<Pt> {
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
fn maximal_runs(segments: &[RawSeg]) -> Vec<Run> {
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
fn candidate_vertices(runs: &[Run]) -> Vec<Pt> {
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
fn rays_at(runs: &[Run], x: Q, y: Q) -> (usize, Vec<Axis>) {
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

/// Measured V16 quantities for one sheet.
#[derive(Debug, Default, Clone, Copy)]
struct InkCounts {
    /// B — L-corners of the ink.
    bends: u32,
    /// J — branch points (T, or same-net cross carrying a junction dot).
    branches: u32,
    /// 4-ray vertices with no junction dot: inter-net crossings. NOT
    /// part of B or J — cross-checked against the crossing ratchet.
    inter_net_crossings: u32,
    /// Diagnostics only.
    raw_segments: u32,
    runs: u32,
}

fn measure(root: &Value) -> Result<InkCounts, String> {
    let segs = raw_wire_segments(root);

    // Tripwire: axis-alignment is what makes ray-counting sound.
    let diagonals: Vec<_> = segs
        .iter()
        .filter(|((x1, y1), (x2, y2))| x1 != x2 && y1 != y2)
        .collect();
    if !diagonals.is_empty() {
        return Err(format!(
            "{} diagonal wire segment(s) emitted, e.g. {:?} — V16's ink graph \
             assumes rectilinear wires",
            diagonals.len(),
            diagonals[0]
        ));
    }

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

// --- the ratchet ---------------------------------------------------------

/// Every fixture that emits a root sheet — the same nine the
/// electrical-safety suite drives.
const FIXTURES: &[&str] = &[
    "rc_lowpass",
    "common_emitter",
    "multivibrator",
    "diff_pair",
    "opamp_inverting_real",
    "opamp_inverting",
    "port_shapes",
    "rc_lowpass_ports",
    "opamp_definition_level",
];

/// Per-fixture `(name, B, J)` high-water marks — **zero slack**, each
/// literal is the count measured on `master`.
///
/// Ratchet policy (CLAUDE.md § "Budgets are ratchets, not knobs"):
/// these only ever go **down**. A commit that removes bends SHOULD
/// lower the literal in the same commit. A commit may NEVER raise one
/// to make a failing test pass — a rise is a geometry regression to
/// diagnose, not a budget to bump.
///
/// Cross-check performed when these literals were first measured: the
/// 4-ray-without-dot count (this file's `inter_net_crossings`, which is
/// deliberately excluded from both B and J) agrees exactly with
/// `placement_quality.rs::count_wire_crossings` on all five ratcheted
/// fixtures — rc_lowpass 0, common_emitter 1 (budget 2), multivibrator
/// 4 (budget 4), diff_pair 0, opamp_inverting_real 0 — confirming the
/// vertex classification. The one divergence is `opamp_definition_level`
/// (ink 4 vs raw 5), which carries no crossing budget: the raw counter
/// double-counts one ink crossing whose runs `cleanup.rs` had split into
/// several `(wire …)` segments, which is exactly the re-segmentation
/// sensitivity the ink graph is built to remove.
const BEND_BRANCH_BUDGETS: &[(&str, u32, u32)] = &[
    ("rc_lowpass", 3, 0),
    ("common_emitter", 10, 3),
    ("multivibrator", 10, 2),
    ("diff_pair", 2, 0),
    ("opamp_inverting_real", 8, 0),
    ("opamp_inverting", 3, 0),
    ("port_shapes", 4, 0),
    ("rc_lowpass_ports", 3, 0),
    ("opamp_definition_level", 10, 2),
];

#[test]
fn bend_and_branch_counts_within_ratchet_across_fixtures() {
    let mut failures: Vec<String> = Vec::new();
    for name in FIXTURES {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);

        let c = match measure(&root) {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };

        let &(_, b_budget, j_budget) = BEND_BRANCH_BUDGETS
            .iter()
            .find(|(n, _, _)| n == name)
            .expect("V16 budget for fixture");

        if c.bends > b_budget {
            failures.push(format!(
                "{name}: B = {} bends > ratchet {b_budget} \
                 (raw segments {}, maximal runs {}, J = {}, inter-net crossings {}). \
                 Do NOT raise the budget — diagnose the routing regression.",
                c.bends, c.raw_segments, c.runs, c.branches, c.inter_net_crossings
            ));
        }
        if c.branches > j_budget {
            failures.push(format!(
                "{name}: J = {} branches > ratchet {j_budget} \
                 (raw segments {}, maximal runs {}, B = {}). \
                 Do NOT raise the budget — diagnose the routing regression.",
                c.branches, c.raw_segments, c.runs, c.bends
            ));
        }
        // Lower-is-better: report reclaimable slack so a fix ratchets down.
        if c.bends < b_budget || c.branches < j_budget {
            eprintln!(
                "V16 {name}: improved — you may lower the ratchet to \
                 (\"{name}\", {}, {})",
                c.bends, c.branches
            );
        }
    }
    assert!(
        failures.is_empty(),
        "V16 violations:\n{}",
        failures.join("\n")
    );
}
