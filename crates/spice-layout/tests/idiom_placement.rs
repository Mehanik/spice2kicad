//! End-to-end POSITION assertions for the canonical-placement idioms
//! (Tier-2 V6/V7). Each test drives the *full* placer (`place_with`,
//! seed → symmetry → idiom → orientation → SA refine) on a real fixture
//! and asserts a **pin-anchored** geometric outcome — the placement a
//! human would draw — without asserting anything about orientation
//! (orientation flow is walled; these idioms are POSITION-only).
//!
//! These tests are RED until the following detectors land in
//! `spice-layout::idioms` and are wired into `place_with`:
//!
//!   1. PARALLEL two-terminal pair  → `common_emitter` RE‖CE stacked in
//!      one vertical column, adjacent.
//!   2. COLLECTOR-LOAD above BJT    → `diff_pair` RC1 above Q1's
//!      collector (same X column, pin-anchored), RC2 above Q2 likewise.
//!   3. SHARED-NODE centering       → `diff_pair` RTAIL centered under
//!      Q1/Q2's shared tail node, one band below.
//!
//! Every assertion reads live pin geometry via `world_pin_mm`, so it is
//! robust to wherever the anchor transistors finally land.

use std::path::PathBuf;
use std::sync::OnceLock;

use kicad_symbols::Library;
use spice_diagnostics::FileId;
use spice_layout::{LayoutOptions, PlacedElement, Placement, place_with};
use spice_policy::check;

fn fixture_library() -> &'static Library {
    static LIB: OnceLock<Library> = OnceLock::new();
    LIB.get_or_init(|| {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = manifest
            .parent()
            .and_then(std::path::Path::parent)
            .expect("workspace root")
            .join("crates/kicad-symbols/tests/fixtures");
        let mut lib =
            Library::from_file(dir.join("Device.kicad_sym")).expect("load Device fixture library");
        for f in [
            "power.kicad_sym",
            "Simulation_SPICE.kicad_sym",
            "Amplifier_Operational.kicad_sym",
        ] {
            lib = lib.merge(Library::from_file(dir.join(f)).unwrap_or_else(|e| {
                panic!("load fixture library {f}: {e:?}");
            }));
        }
        lib
    })
}

/// One grid step (50 mil) in millimetres.
const STEP_MM: f64 = 1.27;
/// Two grid-snapped pins are "the same column" when their X differ by
/// less than half a grid step — i.e. they are on the identical grid
/// line. (Half-grid, not zero, only to absorb float round-trip noise;
/// an origin-anchored-instead-of-pin-anchored mistake is a full 2 grid
/// steps off here and still fails.)
const SAME_COLUMN_EPS_MM: f64 = STEP_MM / 2.0;

fn place_fixture(name: &str, refine: bool) -> Placement {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = manifest.join("../spice2kicad/tests/fixtures").join(name);
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
    let parsed = spice_parser::parse(&src, FileId(0))
        .expect("parse failed")
        .netlist;
    let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
    let (checked, _warns) = check(resolved).expect("policy check failed");
    let opts = LayoutOptions {
        refine,
        ..LayoutOptions::default()
    };
    place_with(checked, fixture_library(), &opts).expect("placement")
}

fn elem<'a>(p: &'a Placement, refdes: &str) -> &'a PlacedElement {
    p.elements
        .iter()
        .find(|e| e.refdes == refdes)
        .unwrap_or_else(|| panic!("no such refdes {refdes}"))
}

/// World (x, y) mm of the pin of `refdes` that connects to SPICE `node`.
/// Pin-anchored: resolves the KiCad pin number via `pin_mapping`, then
/// looks it up in the orientation-transformed pin set.
fn pin_xy(p: &Placement, refdes: &str, node: &str) -> (f64, f64) {
    let e = elem(p, refdes);
    let ti = e.nodes.iter().position(|n| n == node).unwrap_or_else(|| {
        panic!(
            "{refdes} has no terminal on net {node}, nodes={:?}",
            e.nodes
        )
    });
    let want = &e.pin_mapping[ti];
    let sym = fixture_library()
        .lookup(&e.lib_id)
        .unwrap_or_else(|| panic!("no symbol for {}", e.lib_id));
    e.world_pin_mm(sym)
        .into_iter()
        .find(|(num, _, _)| num == want)
        .map_or_else(|| panic!("{refdes} has no pin #{want}"), |(_, x, y)| (x, y))
}

// ---------------------------------------------------------------------------
// Idiom 1 — PARALLEL two-terminal pair: common_emitter RE ‖ CE
// (both connect nets `e` and `0`) must sit vertically aligned + adjacent.
// ---------------------------------------------------------------------------

fn assert_re_ce_parallel(refine: bool) {
    let p = place_fixture("common_emitter.cir", refine);
    let re = elem(&p, "RE");
    let ce = elem(&p, "CE");

    // Vertical align: RE and CE share one X column. Assert pin-anchored
    // on their common node `e` (both have a terminal on `e`).
    let (re_ex, _) = pin_xy(&p, "RE", "e");
    let (ce_ex, _) = pin_xy(&p, "CE", "e");
    assert!(
        (re_ex - ce_ex).abs() < SAME_COLUMN_EPS_MM,
        "parallel RE‖CE must share an X column (vertical align): \
         RE.e-pin.x={re_ex:.3} CE.e-pin.x={ce_ex:.3} (refine={refine})"
    );

    // Adjacency: they are stacked close, not scattered across the sheet.
    // A stacked resistor+capacitor pair spans well under 15 grid cells
    // vertically; anything larger means the idiom did not group them.
    let dy = (f64::from(re.origin.y) - f64::from(ce.origin.y)).abs();
    assert!(
        dy <= 15.0,
        "parallel RE‖CE must be adjacent (close in Y), got |ΔY|={dy} cells (refine={refine})"
    );
}

// Idiom 1 (parallel R‖C) is DEFERRED: a position-only same-column stack
// shorts the non-ground net past the ground pin (V11, Tier 0) when a
// shared net is ground, and the clean fix is an orientation flip that the
// left→right flow-wall forbids. Re-enable when a v0.2 owns the flip. See
// `spice_layout::idioms::ParallelPair` and `apply_position_idioms`.
#[test]
#[ignore = "Idiom 1 deferred: position-only parallel stack is a V11 short under the orientation wall"]
fn common_emitter_re_ce_parallel_seed() {
    assert_re_ce_parallel(false);
}

#[test]
#[ignore = "Idiom 1 deferred: position-only parallel stack is a V11 short under the orientation wall"]
fn common_emitter_re_ce_parallel_refined() {
    assert_re_ce_parallel(true);
}

// ---------------------------------------------------------------------------
// Idiom 2 — COLLECTOR-LOAD above transistor: diff_pair RC1 above Q1's
// collector (net c1), RC2 above Q2's collector (net c2). Pin-anchored on
// the shared collector net's X column.
// ---------------------------------------------------------------------------

fn assert_collector_load_column(refine: bool, rc: &str, q: &str, collector_net: &str) {
    let p = place_fixture("diff_pair.cir", refine);
    // The resistor's non-rail pin and the transistor's collector pin are
    // the SAME net (`collector_net`); the idiom aligns their X columns.
    let (rc_x, _) = pin_xy(&p, rc, collector_net);
    let (q_x, _) = pin_xy(&p, q, collector_net);
    assert!(
        (rc_x - q_x).abs() < SAME_COLUMN_EPS_MM,
        "collector-load {rc} must share {q}'s collector ({collector_net}) X column: \
         {rc}.pin.x={rc_x:.3} {q}.collector.x={q_x:.3} (refine={refine})"
    );
}

// Idiom 2 (collector-load) is LIVE, generalised as the RAIL-STUB COLUMN
// idiom (`spice_layout::idioms::{detect_rail_stubs,
// apply_rail_stub_columns}`, wired from `lib::apply_rail_stub_columns`).
//
// It was previously deferred as "ripples the busiest ratchets + fights
// the V7 RC1/RC2 symmetry pin". Neither held once `cost::rail_direction`
// was un-inverted: the fixtures' crossing / wire-length / body-overlap
// budgets are all still green, and the symmetry pin is not the obstacle
// — `RC1`/`RC2` are pinned by the fixture's own `*@align horizontal`,
// whose constraint is a shared *row*, so correcting their column is
// orthogonal to it (and is exactly the reported defect).
//
// Note the detector is no longer BJT-specific: a stub is any two-terminal
// element with one pin on a rail, and its column is the vertically-facing
// pin of the multi-terminal device it terminates. A collector load is
// simply the case where that device is a transistor.
#[test]
fn diff_pair_rc1_over_q1_collector_seed() {
    assert_collector_load_column(false, "RC1", "Q1", "c1");
}

#[test]
fn diff_pair_rc1_over_q1_collector_refined() {
    assert_collector_load_column(true, "RC1", "Q1", "c1");
}

#[test]
fn diff_pair_rc2_over_q2_collector_seed() {
    assert_collector_load_column(false, "RC2", "Q2", "c2");
}

#[test]
fn diff_pair_rc2_over_q2_collector_refined() {
    assert_collector_load_column(true, "RC2", "Q2", "c2");
}

// ---------------------------------------------------------------------------
// Idiom 3 — SHARED-NODE centering: diff_pair RTAIL sits centered under
// the shared tail node of Q1/Q2 (both emitters on net `tail`) and one
// band below them.
// ---------------------------------------------------------------------------

fn assert_rtail_centered(refine: bool) {
    let p = place_fixture("diff_pair.cir", refine);

    // The tail node is Q1's and Q2's emitter and RTAIL's terminal. Center
    // RTAIL's tail pin at the midpoint of the two transistors' tail pins.
    let (rtail_x, _) = pin_xy(&p, "RTAIL", "tail");
    let (q1_x, _) = pin_xy(&p, "Q1", "tail");
    let (q2_x, _) = pin_xy(&p, "Q2", "tail");
    let mid = f64::midpoint(q1_x, q2_x);
    assert!(
        (rtail_x - mid).abs() <= STEP_MM, // within one grid cell of dead center
        "RTAIL must be centered under Q1/Q2's tail node: RTAIL.tail.x={rtail_x:.3} \
         midpoint={mid:.3} (Q1={q1_x:.3}, Q2={q2_x:.3}) (refine={refine})"
    );

    // One band below: RTAIL's body sits below both transistors (larger Y
    // is lower on the KiCad sheet).
    let rtail = elem(&p, "RTAIL");
    let q1 = elem(&p, "Q1");
    let q2 = elem(&p, "Q2");
    assert!(
        rtail.origin.y > q1.origin.y && rtail.origin.y > q2.origin.y,
        "RTAIL must sit below Q1/Q2: RTAIL.y={} Q1.y={} Q2.y={} (refine={refine})",
        rtail.origin.y,
        q1.origin.y,
        q2.origin.y
    );
}

#[test]
fn diff_pair_rtail_centered_below_seed() {
    assert_rtail_centered(false);
}

#[test]
fn diff_pair_rtail_centered_below_refined() {
    assert_rtail_centered(true);
}
