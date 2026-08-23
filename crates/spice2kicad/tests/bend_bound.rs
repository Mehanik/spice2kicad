//! **V16 bend count, against an ABSOLUTE reference** (informational).
//!
//! # Why this file exists
//!
//! ADR-23 records the finding that motivates it: *every one* of the
//! project's ~165 quality budgets was obtained by measuring the incumbent
//! placer's own output. A budget therefore records what the current
//! placer happens to achieve, and against that reference "regression" and
//! "difference" are the same measurement. `rc_phase_shift`'s B (10 as of
//! the rail-stub SIDE fix, 19 when this file was written) is not judged
//! as bad — it is **protected at whatever the incumbent happens to
//! reach**.
//!
//! This file gives B a reference that does not come from the placer: a
//! **provable lower bound on the number of bends any rectilinear ink
//! could have**, derived from the terminal geometry alone. The number it
//! prints is a *gap to close*, not a high-water mark to defend.
//!
//! It is the bend-count analogue of the wire-detour metric
//! (`placement_quality.rs::wire_detour`), which already grades emitted
//! wire *length* against the half-perimeter lower bound rather than
//! against the incumbent's length.
//!
//! # NOT a ratchet — deliberately
//!
//! The test asserts nothing about the gap. `docs/invariants.md` V16 warns
//! against admitting a metric that is not a genuinely load-bearing
//! counted quantity, and a bound that is subtly *inadmissible* — one that
//! ever exceeds the true optimum — would, as a gate, block all work while
//! being wrong. So the gap is printed and recorded to the ADR-23
//! scoreboard as an **informational** metric (the Q6-balance precedent),
//! and nothing here can fail because a fixture's bends are high.
//!
//! What this test *does* assert is the soundness of its own instrument:
//!
//! 1. Σ per-component bends == the whole-sheet B the V16 ratchet asserts
//!    on (the decomposition is exact, not approximately exact);
//! 2. every bounded component satisfies `bound <= measured` — the
//!    admissibility tripwire. If the bound ever exceeds reality, the
//!    bound is wrong and this test says so loudly, on real geometry.
//!
//! This is also why the ink graph now lives in `tests/common/ink.rs`:
//! this file and `wire_geometry.rs` measure B with the *same* code, not
//! with two implementations that agree until they don't (ADR-23 D2,
//! MEMORY "verify what a number measures").
//!
//! # The unit of accounting: the ink COMPONENT
//!
//! Components are **electrical**, computed on the raw segments: two
//! segments join iff they share an endpoint, which is KiCad's own rule
//! ("wires connect only at endpoints" — the rule
//! `spice-route/src/cleanup.rs::split_at_interior_attachments` exists to
//! serve). Reading connectivity *before* the merge into maximal runs is
//! load-bearing, not fastidious: a run-level "they touch, so they join"
//! rule collapses `two_stage_amp` into 4 components, one holding the
//! `b2`, `c2` and `e2` labels at once, because ten wire ends land on the
//! interior of a foreign net's wire. The endpoint rule gives 6, one per
//! labelled net.
//!
//! A bend has exactly two rays and — by the vertex-profile fact in
//! `common::ink::run_components` — both of its runs *end* there, so its
//! segments share that endpoint: a bend belongs to exactly one component
//! and `B_total = Σ_components B(component)` exactly. That identity is
//! asserted per fixture, not assumed.
//!
//! Two component-level hazards are handled rather than assumed away, and
//! both are conditional on the lower gates in the same way V16 itself is:
//!
//! * a maximal run whose collinear pieces belong to two components (a
//!   *collinear wire overlap*) is indivisible here, so those components
//!   are merged — sound, but coarser, and the merge count is reported.
//!   On `two_stage_amp` it fires exactly twice, at `x = 57.15` and
//!   `y = 87.63`: the registered `no_cross_net_collinear_wire_overlap`
//!   expected failure, rediscovered from geometry alone.
//! * a component corner that foreign ink passes through scores four rays
//!   globally, so the *metric* does not count it as a bend while the
//!   component's own ink plainly turns there. Such a component is
//!   dropped rather than bounded against a count that understates its
//!   corners. It fires on no fixture today.
//!
//! The component, not the net, is the unit — the same choice
//! `wire_detour` makes and for the same reason: a net may legitimately be
//! drawn as two disjoint trees bridged by a V4 name-jump label pair, and
//! charging one component with the other's span measures nothing. A
//! component's **terminals** are the points where it attaches to the rest
//! of the schematic: symbol pins (including power glyphs and `PWR_FLAG`),
//! hierarchical-sheet port pins, label anchors and no-connect markers
//! that lie on its ink.
//!
//! *Honesty label.* Because `T` is read off the artifact, the bound is
//! admissible **for this output** by construction — its hypotheses are
//! verified on the geometry it grades. But which anchors a component
//! carries is partly a router choice (where it put the name-jump label),
//! so the gap reads as "bends improvable given this decomposition", not
//! as a bound over all routers. Pin *positions* are placement output and
//! are not chosen by the router; a fully router-independent per-net
//! column would have to exclude every net whose anchors are not exactly
//! its pins, and is not built here.
//!
//! # The bound, and why it is admissible
//!
//! Fix a component. Let `S` be its ink — a finite, connected, rectilinear
//! point set — and let `T` be its terminals. The quantity bounded is the
//! *metric's own* B: vertices of `S` with exactly two rays, one
//! horizontal and one vertical.
//!
//! **The hazard.** B is NOT monotone under adding ink: attach a spur to
//! an L-corner and the vertex has three rays, so it scores as a branch
//! (J) and not as a bend. A naive "two terminals off-axis need one bend"
//! is therefore false for *arbitrary* supersets of ink. The bound is
//! stated over the class that actually applies:
//!
//! > **(H)** `S` is connected, rectilinear, and **every 1-ray vertex of
//! > `S` is a terminal in `T`** — no dangling ends.
//!
//! (H) is not an assumption about the router: it is *verified per
//! component* below. The project independently enforces it
//! (`electrical_safety.rs::no_dangling_whiskers_across_fixtures`, budget
//! 0), but a component whose leaves are not all terminals is marked **not
//! bounded** and contributes 0 rather than being trusted.
//!
//! **The extremal lemma.** Define the *NW point of `S`* as the point of
//! `S` with least y, breaking ties by least x. Nothing of `S` lies above
//! it, and nothing on its own row lies to its left, so its only possible
//! rays are `+x` and `+y`. Hence its ray count is 1 or 2; if 2 the two
//! rays are one horizontal and one vertical, i.e. **a bend**; if 1 it is
//! a leaf, hence — by (H) — a terminal. The same holds for all eight
//! extremal roles (min/max y × min/max x, and min/max x × min/max y),
//! each with its own two admissible ray directions.
//!
//! **The consequence used here.** Suppose `B(S) = 0`. Then every extremal
//! point is a leaf-terminal, so the bounding box of `S` equals the
//! bounding box of `T` (each extreme of `S` is attained by a terminal,
//! and `T ⊆ S`), and each of the eight extremal roles is filled by a
//! *terminal computable from `T` alone* — e.g. the NW role is the
//! least-x terminal on `T`'s topmost row. Each such terminal is a leaf:
//! it has exactly ONE ray, which must lie in that role's admissible pair.
//! When one terminal fills several roles, its single ray must lie in the
//! intersection of their admissible sets. **If that intersection is
//! empty, `B(S) = 0` is impossible, so `B(S) >= 1`.**
//!
//! Worked instance — the case that dominates real nets. `T = {p, q}` with
//! `p = (0,0)`, `q = (5,3)`. `p` is the only terminal on the topmost row,
//! so it fills both top roles: its ray is `+y`. `p` is also the only
//! terminal in the leftmost column, so it fills both left roles: its ray
//! is `+x`. A leaf has one ray; `{+y} ∩ {+x} = ∅`; hence at least one
//! bend. And one is achievable (the L), so for two off-axis terminals the
//! bound is not merely admissible but **exact**.
//!
//! **One free extra: anchor-free ink.** A component with no anchors at
//! all has no legal leaves, so each of its four extreme lines carries
//! *two distinct* bend witnesses (a single-point extreme line would be a
//! leaf), and a bend lies on at most two extreme lines — hence `B >= 4`.
//! A floating rectangle has exactly 4, so the rule is tight. It is worth
//! stating because the dangling-whisker gate cannot see this case: a
//! closed loop has no degree-1 endpoint to flag.
//!
//! **Where it stops, and why no tighter unconditional rule exists.** The
//! lemma refutes only `B = 0`, so the bound it yields is 0 or 1 per
//! anchored component. That ceiling is not laziness — it is close to a
//! fact about the metric. A horizontal trunk with vertical taps realises
//! `B <= 2` for *any* finite terminal set however many rows and columns
//! it spans (taps meet the trunk's interior as 3-ray Ts, which are J and
//! not B), `B <= 1` when a terminal anchors one trunk end, and `B = 0`
//! when terminals anchor both ends and every tap. So **no formula in
//! (#rows, #columns) that ever exceeds 2 can be admissible**, and in an
//! obstacle-free, direction-free world almost every terminal set is
//! routable with at most one bend. The rule is also *sufficient, not
//! necessary*: `T = {(0,0), (1,0), (0,1)}` truly needs one bend but has
//! no doubly-lonely terminal, so this bound honestly reports 0 there.
//! Do not "tighten" it without a fresh proof.
//!
//! Two consequences are stated in the report rather than hidden:
//!
//! * the printed **gap = measured − bound is an over-estimate** of the
//!   truly reducible bends;
//! * the **exact class** — components with exactly two terminals, where
//!   the obstacle-free optimum is known exactly (1 off-axis, 0 collinear)
//!   — is reported separately. That column is the honest "provably
//!   reducible" figure.
//!
//! # What is deliberately NOT in the bound
//!
//! * **Pin direction.** A pin's outward axis would tighten the bound a
//!   lot (two same-facing pins sharing a row force a 2-bend U — the floor
//!   `docs/invariants.md` V16 documents for `rc_lowpass`). But
//!   "a wire leaves along the pin axis" is V5, a **Tier-2 preference**,
//!   not a geometric necessity: nothing forbids a wire meeting a pin
//!   broadside. A direction-derived term would therefore bound a
//!   *convention-obeying* router, not any router, and would be
//!   inadmissible the moment the router disobeyed. Excluded — but note
//!   where the realistic floors live: a *separately reported*
//!   "V5-conditional" column is the sound home for them, and the same
//!   extremal machinery proves its theorems (for two same-facing pins on
//!   one row: each pin's column has a single-point extreme line, which
//!   would force a horizontal ray and contradict the pin's direction, so
//!   each column carries a bend and `B >= 2`). Such a column must never
//!   be summed into the admissible one.
//! * **Obstacles.** Symbol bodies would tighten the bound too, and are
//!   excluded for the opposite reason: they are placement output, so a
//!   bound that used them would move whenever the placer moved, which is
//!   precisely the incumbent-relative reference this file exists to
//!   escape. The target must be router- and placement-independent.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use kicad_symbols::{Library, Orientation, Rotation};
use lexpr::Value;

use common::ink::{
    self, Pt, Run, as_f64, as_str, candidate_vertices, children, find_child, is_bend, list_iter,
    maximal_runs, measure, raw_wire_segments, reject_diagonals, run_components,
};

// --- driver bits ---------------------------------------------------------

/// The fixtures graded here. Kept local (rather than shared with
/// `wire_geometry.rs`) so this file stays purely additive: a fixture
/// added to the V16 ratchet's table does not conflict with this one.
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
    "named_rails",
    "rc_phase_shift",
    "two_stage_amp",
];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load_test_library() -> Library {
    let libs_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .join("crates/kicad-symbols/tests/fixtures");
    let device =
        Library::from_file(libs_dir.join("Device.kicad_sym")).expect("parse Device.kicad_sym");
    let sim = Library::from_file(libs_dir.join("Simulation_SPICE.kicad_sym"))
        .expect("parse Simulation_SPICE.kicad_sym");
    let amp = Library::from_file(libs_dir.join("Amplifier_Operational.kicad_sym"))
        .expect("parse Amplifier_Operational.kicad_sym");
    let power =
        Library::from_file(libs_dir.join("power.kicad_sym")).expect("parse power.kicad_sym");
    device.merge(sim).merge(amp).merge(power)
}

/// `(at x y rot)` + optional `(mirror y)` of a placed `(symbol …)`.
fn placed_symbol_pose(sym: &Value) -> Option<(f64, f64, Orientation)> {
    let at = find_child(sym, "at")?;
    let mut it = list_iter(at);
    it.next();
    let x = it.next().and_then(as_f64)?;
    let y = it.next().and_then(as_f64)?;
    let rot_deg = it.next().and_then(as_f64).unwrap_or(0.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rot_u = ((rot_deg.round() as i64).rem_euclid(360)) as u16;
    let rotation = match rot_u {
        0 => Rotation::R0,
        90 => Rotation::R90,
        180 => Rotation::R180,
        270 => Rotation::R270,
        _ => return None,
    };
    let mirror_y = find_child(sym, "mirror")
        .and_then(|m| list_iter(m).nth(1).and_then(as_str))
        .is_some_and(|s| s == "y");
    Some((x, y, Orientation { rotation, mirror_y }))
}

fn symbol_lib_id(sym: &Value) -> Option<String> {
    find_child(sym, "lib_id")
        .and_then(|lid| list_iter(lid).nth(1).and_then(as_str))
        .map(str::to_owned)
}

/// Every point at which emitted ink may legitimately terminate.
///
/// Over-collection is harmless (a point that is not on the ink is simply
/// never a component's terminal); UNDER-collection is safe too, and that
/// is the property that matters: a missed terminal shows up as a leaf
/// with no terminal, which marks the component **not bounded** rather
/// than producing a wrong bound.
fn terminal_points(root: &Value, library: &Library) -> (HashSet<Pt>, usize) {
    let mut out: HashSet<Pt> = HashSet::new();
    let mut unknown_lib_ids = 0usize;

    for sym in children(root, "symbol") {
        let Some(lib_id) = symbol_lib_id(sym) else {
            continue;
        };
        let Some((ox, oy, orient)) = placed_symbol_pose(sym) else {
            continue;
        };
        let Some(lib_sym) = library.lookup(&lib_id) else {
            unknown_lib_ids += 1;
            continue;
        };
        for tp in lib_sym.pins_in(orient) {
            // eeschema y-flip, as everywhere else in the suite.
            out.insert((ink::q(ox + tp.x), ink::q(oy - tp.y)));
        }
    }

    // Hierarchical-sheet port pins: real routing endpoints that are not
    // symbols.
    for sheet in children(root, "sheet") {
        for pin in children(sheet, "pin") {
            if let Some(at) = find_child(pin, "at") {
                let mut it = list_iter(at);
                it.next();
                if let (Some(x), Some(y)) = (it.next().and_then(as_f64), it.next().and_then(as_f64))
                {
                    out.insert((ink::q(x), ink::q(y)));
                }
            }
        }
    }

    // Label anchors (V4 name-jump pairs, hierarchical/global labels) and
    // no-connect markers.
    for kind in ["label", "global_label", "hierarchical_label", "no_connect"] {
        for node in children(root, kind) {
            if let Some(at) = find_child(node, "at") {
                let mut it = list_iter(at);
                it.next();
                if let (Some(x), Some(y)) = (it.next().and_then(as_f64), it.next().and_then(as_f64))
                {
                    out.insert((ink::q(x), ink::q(y)));
                }
            }
        }
    }

    (out, unknown_lib_ids)
}

// --- the bound -----------------------------------------------------------

/// One admissible ray direction at a leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Dir {
    PosX,
    NegX,
    PosY,
    NegY,
}

/// The eight extremal roles, as (key, admissible ray directions).
///
/// A role's point is the terminal that is extreme in the primary axis and
/// then in the secondary one; its admissible rays are the two directions
/// that point back INTO the rest of the ink. See the module docs for the
/// lemma these encode.
fn role_points(terms: &[Pt]) -> Vec<(Pt, [Dir; 2])> {
    use Dir::{NegX, NegY, PosX, PosY};
    let pick = |key: fn(&Pt) -> (i64, i64)| -> Pt { *terms.iter().min_by_key(|p| key(p)).unwrap() };
    vec![
        // topmost row, leftmost / rightmost on it
        (pick(|p| (p.1, p.0)), [PosX, PosY]),
        (pick(|p| (p.1, -p.0)), [NegX, PosY]),
        // bottommost row
        (pick(|p| (-p.1, p.0)), [PosX, NegY]),
        (pick(|p| (-p.1, -p.0)), [NegX, NegY]),
        // leftmost column, topmost / bottommost on it
        (pick(|p| (p.0, p.1)), [PosX, PosY]),
        (pick(|p| (p.0, -p.1)), [PosX, NegY]),
        // rightmost column
        (pick(|p| (-p.0, p.1)), [NegX, PosY]),
        (pick(|p| (-p.0, -p.1)), [NegX, NegY]),
    ]
}

/// A provable lower bound on the bends of any ink satisfying (H) whose
/// terminal set is `terms`.
///
/// Two rules, both proved in the module docs:
///
/// * **anchor-free ink** (`terms` empty) has no legal leaves at all, so
///   each of its four extreme lines carries two distinct bend witnesses;
///   a bend lies on at most two extreme lines, so `B >= 4`. This case is
///   invisible to the dangling-whisker gate (a floating rectangle has no
///   degree-1 endpoint), which is why it is worth stating.
/// * otherwise **1 when some terminal is forced into two incompatible
///   extremal roles** — which refutes `B = 0` — and 0 otherwise. The
///   lemma cannot refute `B = 1`, so 1 is this rule's ceiling.
fn bend_lower_bound(terms: &[Pt]) -> u32 {
    if terms.is_empty() {
        return 4;
    }
    if terms.len() < 2 {
        return 0;
    }
    let mut allowed: HashMap<Pt, HashSet<Dir>> = HashMap::new();
    for (p, dirs) in role_points(terms) {
        let e = allowed.entry(p).or_insert_with(|| {
            [Dir::PosX, Dir::NegX, Dir::PosY, Dir::NegY]
                .into_iter()
                .collect()
        });
        e.retain(|d| dirs.contains(d));
    }
    u32::from(allowed.values().any(HashSet::is_empty))
}

/// The **exact** obstacle-free bend optimum, for the class where it is
/// known: exactly two terminals. One bend if they share neither row nor
/// column (the L, and the lemma above proves 0 impossible); zero if they
/// are collinear (a single straight run).
fn exact_two_terminal_optimum(terms: &[Pt]) -> Option<u32> {
    if terms.len() != 2 {
        return None;
    }
    let (a, b) = (terms[0], terms[1]);
    Some(u32::from(a.0 != b.0 && a.1 != b.1))
}

// --- per-fixture analysis -------------------------------------------------

#[derive(Debug, Default)]
struct FixtureReport {
    measured: u32,
    bound: u32,
    /// Components whose leaves are all terminals — the class (H) applies
    /// to, and the only class contributing to `bound`.
    comps_bounded: usize,
    comps_total: usize,
    /// Bends sitting in bounded components (the rest are not graded).
    bends_covered: u32,
    /// Components with exactly two terminals: measured bends and the
    /// exact obstacle-free optimum.
    exact_comps: usize,
    exact_measured: u32,
    exact_optimum: u32,
    unknown_lib_ids: usize,
    /// Why components dropped out, for the coverage line.
    dangling_leaves: usize,
    /// Components dropped because foreign ink passes through one of
    /// their corners, so the metric does not score that corner as a bend
    /// (see `analyse`).
    shadowed_corners: usize,
    /// Components with ink but no anchor at all — a floating cycle. The
    /// dangling-whisker gate cannot see these.
    anchor_free: usize,
    /// Runs whose collinear pieces belong to different endpoint
    /// components, forcing a merge. On real output these are the
    /// cross-net collinear overlaps; each one coarsens the partition and
    /// so *weakens* this instrument's resolution.
    overlap_merges: usize,
}

fn analyse(root: &Value, library: &Library) -> Result<FixtureReport, String> {
    let segs = raw_wire_segments(root);
    reject_diagonals(&segs)?;
    let runs = maximal_runs(&segs);
    let (comp_of_run, overlap_merges) = run_components(&runs, &segs);
    let (terminals, unknown_lib_ids) = terminal_points(root, library);

    // The metric's own bend vertices, over the whole sheet. Membership is
    // what the V16 ratchet counts, so it is what the bound must be
    // admissible against.
    let global_bends: HashSet<Pt> = candidate_vertices(&runs)
        .into_iter()
        .filter(|(x, y)| is_bend(&runs, *x, *y))
        .collect();

    let mut rep = FixtureReport {
        unknown_lib_ids,
        overlap_merges,
        ..FixtureReport::default()
    };

    let comps: HashSet<usize> = comp_of_run.iter().copied().collect();
    rep.comps_total = comps.len();
    let mut sum_bends = 0u32;
    for comp in comps {
        let comp_runs: Vec<Run> = runs
            .iter()
            .zip(&comp_of_run)
            .filter(|(_, c)| **c == comp)
            .map(|(r, _)| *r)
            .collect();

        // Vertices are classified against THIS component's runs: the
        // hypothesis (H) is a statement about the component's own ink, and
        // a foreign wire crossing it must not disguise one of its leaves
        // as a T.
        let mut local_bends: Vec<Pt> = Vec::new();
        let mut leaves: Vec<Pt> = Vec::new();
        for (x, y) in candidate_vertices(&comp_runs) {
            if is_bend(&comp_runs, x, y) {
                local_bends.push((x, y));
            }
            if ink::rays_at(&comp_runs, x, y).0 == 1 {
                leaves.push((x, y));
            }
        }
        // The bends the *metric* attributes here: a local corner that a
        // foreign run passes through scores 4 rays globally and is not
        // counted at all. Those two numbers coincide on every fixture
        // today; where they would not, the component is dropped rather
        // than bounded against a count that understates its corners.
        let bends = u32::try_from(
            local_bends
                .iter()
                .filter(|p| global_bends.contains(p))
                .count(),
        )
        .expect("bend count fits u32");
        sum_bends += bends;

        // (H): every leaf of this component must be a terminal.
        if leaves.iter().any(|p| !terminals.contains(p)) {
            rep.dangling_leaves += 1;
            continue;
        }
        if bends as usize != local_bends.len() {
            rep.shadowed_corners += 1;
            continue;
        }
        rep.comps_bounded += 1;
        rep.bends_covered += bends;

        let mut terms: Vec<Pt> = terminals
            .iter()
            .copied()
            .filter(|p| comp_runs.iter().any(|r| r.contains(p.0, p.1)))
            .collect();
        terms.sort_unstable();
        terms.dedup();
        if terms.is_empty() {
            rep.anchor_free += 1;
        }

        let lb = bend_lower_bound(&terms);
        if std::env::var_os("S2K_BEND_BOUND_DETAIL").is_some() {
            println!(
                "    comp: runs {:>2}  bends {:>2}  anchors {:>2}  bound {lb}  bbox \
                 x[{:.2}..{:.2}] y[{:.2}..{:.2}]",
                comp_runs.len(),
                bends,
                terms.len(),
                mm(terms.iter().map(|p| p.0).min().unwrap_or(0)),
                mm(terms.iter().map(|p| p.0).max().unwrap_or(0)),
                mm(terms.iter().map(|p| p.1).min().unwrap_or(0)),
                mm(terms.iter().map(|p| p.1).max().unwrap_or(0)),
            );
        }
        assert!(
            lb <= bends,
            "ADMISSIBILITY FAILURE: component with terminals {terms:?} measures {bends} bend(s) \
             but the lower bound claims {lb}. The BOUND is wrong — a lower bound that exceeds \
             reality makes every gap in this report a lie. Do not adjust the measurement to fit.",
        );
        rep.bound += lb;

        if let Some(exact) = exact_two_terminal_optimum(&terms) {
            rep.exact_comps += 1;
            rep.exact_measured += bends;
            rep.exact_optimum += exact;
            assert!(
                exact <= bends,
                "ADMISSIBILITY FAILURE (exact class): two terminals {terms:?} measure {bends} \
                 bend(s), exact optimum claims {exact}",
            );
        }
    }

    rep.measured = sum_bends;
    Ok(rep)
}

/// Micrometres back to millimetres, for the detail dump only.
#[allow(clippy::cast_precision_loss)] // sheet coordinates are far below 2^52 µm
fn mm(v: i64) -> f64 {
    v as f64 / 1000.0
}

/// Print the gap table. Split out of the test purely so the test body
/// stays under the pedantic line limit.
fn print_report(rows: &[(String, FixtureReport)]) {
    println!(
        "\nV16 bends against an ABSOLUTE reference (informational; see module docs)\n\
         \n{:<24} {:>4} {:>6} {:>5}  {:>9}  {:>21}",
        "fixture", "B", "bound", "gap", "coverage", "exact class (|T| = 2)"
    );
    println!("{}", "-".repeat(80));
    let (mut tot_b, mut tot_bound) = (0u32, 0u32);
    let (mut tem, mut opt) = (0u32, 0u32);
    let (mut tc, mut tcb, mut tbc) = (0usize, 0usize, 0u32);
    for (name, r) in rows {
        println!(
            "{:<24} {:>4} {:>6} {:>5}  {:>3}/{:<3} comps  {:>3} comps {:>3} -> {:<3}",
            name,
            r.measured,
            r.bound,
            r.measured - r.bound,
            r.comps_bounded,
            r.comps_total,
            r.exact_comps,
            r.exact_measured,
            r.exact_optimum,
        );
        tot_b += r.measured;
        tot_bound += r.bound;
        tem += r.exact_measured;
        opt += r.exact_optimum;
        tc += r.comps_total;
        tcb += r.comps_bounded;
        tbc += r.bends_covered;
    }
    println!("{}", "-".repeat(80));
    println!(
        "{:<24} {:>4} {:>6} {:>5}  {:>3}/{:<3} comps  {:>3} comps {:>3} -> {:<3}",
        "TOTAL",
        tot_b,
        tot_bound,
        tot_b - tot_bound,
        tcb,
        tc,
        "",
        tem,
        opt
    );
    println!(
        "\ncoverage: {tcb}/{tc} components satisfy (H) and are bounded; \
         {tbc}/{tot_b} measured bends sit in those components.\n\
         reading: `gap` is an UPPER bound on the reducible bends (the lemma refutes B = 0 \
         only, so `bound` <= 1 per component). The `exact class` column is the honest \
         provably-reducible figure: {tem} bends drawn where {opt} is the exact obstacle-free \
         optimum, i.e. {} bends provably wasted on two-terminal components alone.",
        tem - opt
    );
    let unknown: usize = rows.iter().map(|(_, r)| r.unknown_lib_ids).sum();
    let dangling: usize = rows.iter().map(|(_, r)| r.dangling_leaves).sum();
    let shadowed: usize = rows.iter().map(|(_, r)| r.shadowed_corners).sum();
    let floating: usize = rows.iter().map(|(_, r)| r.anchor_free).sum();
    println!(
        "instrument health: {dangling} component(s) dropped for a non-terminal leaf, \
         {shadowed} dropped for a corner shadowed by foreign ink, {floating} anchor-free \
         (floating) component(s), {unknown} symbol instance(s) with a lib_id absent from \
         the fixture libraries (each such symbol's pins are invisible as terminals, which \
         can only REMOVE coverage, never inflate the bound)."
    );
    for (name, r) in rows {
        if r.overlap_merges > 0 {
            println!(
                "  {name}: {} collinear wire overlap(s) merge two nets' ink into one \
                 component, coarsening the partition and weakening the bound here. This is \
                 the registered `no_cross_net_collinear_wire_overlap` defect, rediscovered \
                 from geometry alone.",
                r.overlap_merges
            );
        }
    }
}

#[test]
fn bend_lower_bound_gap_across_fixtures() {
    let library = load_test_library();
    let mut rows: Vec<(String, FixtureReport)> = Vec::new();
    // Collect-then-assert on the self-consistency check below: a panic
    // inside this loop aborts the whole test function, so every later
    // fixture goes unmeasured and reports nothing to the ADR-23
    // measurement sink — where a truncated metric reads as a metric that
    // had nothing to say ("a blind cell is not conservatively blind").
    let mut unsound: Vec<String> = Vec::new();

    for name in FIXTURES {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = common::TempDir::new("bb", name);
        let sch = common::spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root: Value = lexpr::from_str(&std::fs::read_to_string(&sch).expect("read sch"))
            .expect("parse sch as lexpr");

        let rep = analyse(&root, &library).unwrap_or_else(|e| panic!("{name}: {e}"));

        // Instrument self-consistency: the per-component decomposition
        // must reproduce, exactly, the number the V16 ratchet asserts on.
        let whole = measure(&root).unwrap_or_else(|e| panic!("{name}: {e}"));
        if rep.measured != whole.bends {
            // Nothing is recorded for this fixture: the check that just
            // failed is precisely the one that licenses the numbers
            // below, so a cell here would be a WRONG cell, which is worse
            // than an absent one (the aggregator flags absent cells).
            unsound.push(format!(
                "{name}: per-component bends ({}) != whole-sheet B ({}) — the component \
                 decomposition of the ink graph is not exact, so every per-component \
                 attribution below is unsound",
                rep.measured, whole.bends
            ));
            continue;
        }

        common::scoreboard::record_count("v16.bend_bound", name, rep.bound as usize);
        common::scoreboard::record_count("v16.bend_gap", name, (rep.measured - rep.bound) as usize);
        common::scoreboard::record_count(
            "v16.bend_excess_exact",
            name,
            (rep.exact_measured - rep.exact_optimum) as usize,
        );

        rows.push(((*name).to_string(), rep));
    }

    // The report. Informational by design — nothing below can fail a
    // build; see the module docs for why this is not a ratchet.
    print_report(&rows);

    assert!(
        unsound.is_empty(),
        "bend-bound instrument self-consistency:\n  {}",
        unsound.join("\n  "),
    );
}

// --- unit tests for the bound itself --------------------------------------
//
// The bound is a mathematical claim, so it is tested as one: against
// hand-computed terminal sets whose optimum is known by inspection. A
// bound validated only on emitted geometry would be validated against
// the very thing it is supposed to judge.

#[test]
fn bound_is_one_for_two_off_axis_terminals() {
    // The L: p and q share neither row nor column. Exactly one bend is
    // both necessary (the lemma) and sufficient (draw the L).
    assert_eq!(bend_lower_bound(&[(0, 0), (5_000, 3_000)]), 1);
    assert_eq!(
        exact_two_terminal_optimum(&[(0, 0), (5_000, 3_000)]),
        Some(1)
    );
}

#[test]
fn bound_is_zero_for_collinear_terminals() {
    // A straight run connects them with no corner, so a bound of 1 would
    // be inadmissible.
    assert_eq!(bend_lower_bound(&[(0, 0), (0, 9_000)]), 0);
    assert_eq!(bend_lower_bound(&[(0, 0), (4_000, 0), (9_000, 0)]), 0);
    assert_eq!(exact_two_terminal_optimum(&[(0, 0), (0, 9_000)]), Some(0));
}

#[test]
fn bound_is_zero_for_a_realisable_bend_free_steiner_tree() {
    // a --- b horizontally, with c dropping onto the middle of that run:
    // a proper 3-ray T, zero bends. The bound MUST be 0 here — this is
    // the shape the metric deliberately scores as J, not B, and a bound
    // of 1 would be inadmissible.
    let terms = [(0, 5_000), (10_000, 5_000), (5_000, 0)];
    assert_eq!(bend_lower_bound(&terms), 0);
}

#[test]
fn bound_fires_for_three_terminals_in_general_position() {
    // No two share a row or a column, and the same terminal is both the
    // topmost and the leftmost — so its single leaf ray would have to
    // point two ways at once.
    let terms = [(0, 0), (5_000, 3_000), (9_000, 7_000)];
    assert_eq!(bend_lower_bound(&terms), 1);
}

#[test]
fn bound_is_zero_for_a_single_terminal() {
    // Degenerate input must not produce a bound out of thin air: with one
    // terminal every extremal role collapses onto that point, whose
    // admissible sets intersect emptily — which would be a spurious 1.
    assert_eq!(bend_lower_bound(&[(3_000, 4_000)]), 0);
}

#[test]
fn anchor_free_ink_needs_four_bends() {
    // A component with no anchor has no legal leaf, so every extreme line
    // carries two distinct bend witnesses; a bend lies on at most two
    // extreme lines, hence B >= 4. Tight: a rectangle has exactly 4.
    assert_eq!(bend_lower_bound(&[]), 4);
}
