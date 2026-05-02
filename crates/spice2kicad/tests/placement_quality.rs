//! Placement-quality (layout aesthetic) invariants for emitted
//! `.kicad_sch` files.
//!
//! These are *quality* metrics — not correctness invariants like V1–V4
//! in `visual_quality.rs`. A schematic that fails one of these is
//! electrically correct but visually ugly: long trunk wires, far-apart
//! pins on a shared net, etc.
//!
//! Currently encodes:
//!
//! * **V5** — pin-facing orientation (CLAUDE.md § Visual quality
//!   invariants V5). For any two adjacent placed elements that share a
//!   net, the placer must choose orientations such that the pins on
//!   the shared net are the closest pair. The verifier sums emitted
//!   `(wire …)` segment lengths on a target net and asserts the total
//!   stays under a fixture-specific threshold.
//! * **V6** — topology-aware placement (CLAUDE.md § Visual quality
//!   invariants V6). When the netlist matches a recognised analog
//!   archetype (common-emitter amp, diff pair, …), elements are placed
//!   per a template: rails horizontal, signal flows left-to-right,
//!   bias network on the input side, bypass caps next to their device.
//!   Verifiers extract per-element `(at x y)` from the emitted
//!   `(symbol …)` instances and assert structural relations between
//!   refdeses (Q1 between RC and RE on Y; VIN < CIN < Q1 < COUT on X).
//!
//! Tests that fail against the current placer are `#[ignore]`d with a
//! pointer to the relevant CLAUDE.md section.
//!
//! The placer lives in `crates/spice-layout/src/`.

mod common;

use std::path::{Path, PathBuf};

use common::spice_to_kicad;
use lexpr::Value;

// --- driver bits ---------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> PathBuf {
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("spice2kicad-pq-{pid}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn emit(name: &str) -> PathBuf {
    let src = fixtures_dir().join(format!("{name}.cir"));
    let tmp = tempdir(name);
    spice_to_kicad(&src, &tmp).expect("spice2kicad")
}

fn parse_sch(sch: &Path) -> Value {
    let src = std::fs::read_to_string(sch).expect("read sch");
    lexpr::from_str(&src).expect("parse sch as lexpr")
}

// --- wire-length helpers -------------------------------------------------

/// Endpoint of a wire segment, in millimetres.
type Pt = (f64, f64);

/// Collect every `(wire (pts (xy a b) (xy c d)))` segment under `root`
/// as `((ax, ay), (bx, by))` in millimetres.
fn wire_segments(root: &Value) -> Vec<(Pt, Pt)> {
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
        let Some(a) = xy_coords(xys[0]) else { continue };
        let Some(b) = xy_coords(xys[1]) else { continue };
        out.push((a, b));
    }
    out
}

fn xy_coords(v: &Value) -> Option<Pt> {
    let mut it = list_iter(v);
    it.next()?; // head "xy"
    let x = as_f64(it.next()?)?;
    let y = as_f64(it.next()?)?;
    Some((x, y))
}

/// Position of every `(global_label "<net>" … (at x y …))` matching
/// `net`. KiCad-emitted nets pin one global_label at each connecting
/// terminal, so these are the canonical anchor points for the net.
fn label_positions(root: &Value, net: &str) -> Vec<Pt> {
    let mut out = Vec::new();
    for head_name in ["global_label", "label"] {
        for node in children(root, head_name) {
            let Some(name) = list_iter(node).nth(1).and_then(as_str) else {
                continue;
            };
            if name != net {
                continue;
            }
            let Some(at) = find_child(node, "at") else {
                continue;
            };
            let mut it = list_iter(at);
            it.next();
            let Some(x) = it.next().and_then(as_f64) else {
                continue;
            };
            let Some(y) = it.next().and_then(as_f64) else {
                continue;
            };
            out.push((x, y));
        }
    }
    out
}

fn manhattan(a: Pt, b: Pt) -> f64 {
    (a.0 - b.0).abs() + (a.1 - b.1).abs()
}

/// Sum of segment lengths (Manhattan) reachable by graph-walking from
/// any of `seeds` via shared endpoints. Restricting to the connected
/// component the labels touch keeps us from accidentally counting wire
/// segments that belong to other nets but happen to share the
/// schematic.
fn total_wire_length_for_net(root: &Value, net: &str) -> f64 {
    let segs = wire_segments(root);
    let seeds = label_positions(root, net);
    if seeds.is_empty() || segs.is_empty() {
        return 0.0;
    }

    // Endpoint-equality with millimetre coordinates: a small epsilon
    // absorbs round-trip rounding without ever bridging real grid
    // neighbours (one grid step = 1.27 mm).
    let eq = |a: Pt, b: Pt| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3;

    let mut visited = vec![false; segs.len()];
    let mut frontier: Vec<Pt> = seeds.clone();
    let mut total = 0.0_f64;

    loop {
        let mut grew = false;
        for (i, &(a, b)) in segs.iter().enumerate() {
            if visited[i] {
                continue;
            }
            if frontier.iter().any(|&p| eq(p, a) || eq(p, b)) {
                visited[i] = true;
                total += manhattan(a, b);
                frontier.push(a);
                frontier.push(b);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    total
}

// --- lexpr helpers (mirrors visual_quality.rs; kept inline for parity) ---

fn head(v: &Value) -> Option<&str> {
    let first = list_iter(v).next()?;
    as_str(first)
}

fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    if let Some(it) = v.list_iter() {
        Box::new(it)
    } else {
        Box::new(std::iter::empty())
    }
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

// --- per-fixture tests ---------------------------------------------------

// V5 — `out` net wire length on `rc_lowpass`.
//
// Threshold rationale: with both R1 and C1 at the default identity
// orientation, the placer puts R1's `out` pin (south, world y=21.59)
// and C1's `out` pin (north, world y=13.97) on opposite ends of a
// horizontal-then-vertical trunk, producing a five-segment polyline
// of total length ~52.07 mm (measured 2026-04-30 against
// `/tmp/spice2kicad-demo/rc_lowpass/rc_lowpass.kicad_sch`).
//
// A pin-facing orientation (rotate C1 180° so its `out` pin faces
// south, or rotate R1 180° so its `out` pin faces north — either
// way the two `out` pins sit at the same y) collapses the net to a
// single horizontal segment of length 8.89 mm, plus an L-bend of
// at most ~10 mm in the worst combination. Anything ≤ ~17 mm is
// achievable; anything ≥ ~30 mm indicates the placer has not chosen
// a pin-facing orientation. The threshold below sits between those
// two regimes with comfortable margin on both sides.
const V5_RC_LOWPASS_OUT_MAX_MM: f64 = 30.0;

#[test]
#[ignore = "V5: placer does not yet choose pin-facing orientations; see CLAUDE.md \u{a7} Visual quality invariants V5 \u{2014} wire from V1.out to R1.out is excessively long because both components keep default orientation"]
fn v5_rc_lowpass_short_out_wire() {
    let sch = emit("rc_lowpass");
    let root = parse_sch(&sch);
    let total = total_wire_length_for_net(&root, "out");
    assert!(
        total > 0.0,
        "V5: rc_lowpass emitted no wires for net `out` — \
         the metric is meaningless until V4 is satisfied"
    );
    assert!(
        total <= V5_RC_LOWPASS_OUT_MAX_MM,
        "V5 placement: rc_lowpass net `out` total wire length is {total:.2} mm; \
         expected \u{2264} {V5_RC_LOWPASS_OUT_MAX_MM:.2} mm. \
         Placer is not choosing pin-facing orientations for R1 and C1."
    );
}

// --- framework smoke tests (run on every `cargo test`) ------------------

#[test]
fn smoke_total_wire_length_walks_connected_segments() {
    // Two segments forming an L: (0,0)-(0,5) and (0,5)-(3,5),
    // anchored by a label at (0,0). Total Manhattan = 5 + 3 = 8.
    let src = r#"(kicad_sch
        (wire (pts (xy 0 0) (xy 0 5)))
        (wire (pts (xy 0 5) (xy 3 5)))
        (wire (pts (xy 100 100) (xy 101 100)))
        (global_label "n1" (at 0 0 0)))"#;
    let v: Value = lexpr::from_str(src).unwrap();
    let total = total_wire_length_for_net(&v, "n1");
    assert!(
        (total - 8.0).abs() < 1e-6,
        "expected 8.0, got {total} (disconnected segment must not be counted)"
    );
}

#[test]
fn smoke_total_wire_length_returns_zero_when_label_missing() {
    let src = r#"(kicad_sch
        (wire (pts (xy 0 0) (xy 0 5)))
        (global_label "other" (at 0 0 0)))"#;
    let v: Value = lexpr::from_str(src).unwrap();
    assert!(total_wire_length_for_net(&v, "n1").abs() < 1e-9);
}

#[test]
fn smoke_wire_segments_extracts_endpoints() {
    let v: Value = lexpr::from_str(
        r"(kicad_sch (wire (pts (xy 1 2) (xy 3 4))) (wire (pts (xy 5 6) (xy 7 8))))",
    )
    .unwrap();
    let segs = wire_segments(&v);
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0], ((1.0, 2.0), (3.0, 4.0)));
    assert_eq!(segs[1], ((5.0, 6.0), (7.0, 8.0)));
}

// --- element position helpers (V6) ---------------------------------------

/// Position of a placed `(symbol …)` instance whose `Reference` property
/// matches `refdes`, in millimetres.
///
/// The emitter writes one top-level `(symbol (lib_id …) (at x y rot)
/// … (property "Reference" "<refdes>" …))` per placed element. We scan
/// those and return the first match.
fn element_position(root: &Value, refdes: &str) -> Option<Pt> {
    for sym in children(root, "symbol") {
        // Skip `lib_symbols` entries: those are nested inside a parent
        // `(lib_symbols …)` list and are handled by `children` only when
        // we descend into it. Top-level instance symbols always carry
        // `(at …)` directly.
        let Some(at) = find_child(sym, "at") else {
            continue;
        };
        // Find the Reference property.
        let mut found_ref = None;
        for prop in children(sym, "property") {
            let mut it = list_iter(prop);
            it.next(); // head "property"
            let key = it.next().and_then(as_str);
            let val = it.next().and_then(as_str);
            if key == Some("Reference") {
                found_ref = val.map(str::to_owned);
                break;
            }
        }
        if found_ref.as_deref() != Some(refdes) {
            continue;
        }
        let mut it = list_iter(at);
        it.next(); // head "at"
        let x = it.next().and_then(as_f64)?;
        let y = it.next().and_then(as_f64)?;
        return Some((x, y));
    }
    None
}

fn element_x(root: &Value, refdes: &str) -> Option<f64> {
    element_position(root, refdes).map(|(x, _)| x)
}

fn element_y(root: &Value, refdes: &str) -> Option<f64> {
    element_position(root, refdes).map(|(_, y)| y)
}

// --- V6: common_emitter topology-aware placement -------------------------

const V6_HINT: &str = "V6: placer treats elements generically; needs an \
    archetype matcher (see CLAUDE.md \u{a7} Visual quality invariants V6)";

#[test]
#[ignore = "V6: placer treats elements generically; needs archetype matcher \
    (see CLAUDE.md \u{a7} Visual quality invariants V6)"]
fn v6_common_emitter_rails_horizontal() {
    // The conventional CE amp has a Vcc rail above Q1 and a GND rail
    // below it, so RC (collector resistor, hangs from Vcc) and
    // RE / CE (emitter pair, drop to GND) must sit at distinctly
    // different Y values from Q1. The current placer puts everything on
    // one horizontal line, so all three Ys are equal and this test fails.
    let sch = emit("common_emitter");
    let root = parse_sch(&sch);

    let q_y = element_y(&root, "Q1").expect("Q1 placed");
    let collector_y = element_y(&root, "RC").expect("RC placed");
    let emitter_r_y = element_y(&root, "RE").expect("RE placed");
    let bypass_y = element_y(&root, "CE").expect("CE placed");

    // RC must be above (smaller y, since KiCad y grows downward) and
    // RE/CE below. Use a small tolerance to ignore mm rounding.
    let tol = 0.5;
    assert!(
        collector_y + tol < q_y,
        "{V6_HINT}: expected RC above Q1 (rc.y={collector_y:.2} < q1.y={q_y:.2})"
    );
    assert!(
        emitter_r_y > q_y + tol,
        "{V6_HINT}: expected RE below Q1 (re.y={emitter_r_y:.2} > q1.y={q_y:.2})"
    );
    assert!(
        bypass_y > q_y + tol,
        "{V6_HINT}: expected CE below Q1 (ce.y={bypass_y:.2} > q1.y={q_y:.2})"
    );
}

#[test]
#[ignore = "V6: placer treats elements generically; needs archetype matcher \
    (see CLAUDE.md \u{a7} Visual quality invariants V6)"]
fn v6_common_emitter_signal_flow_ordering() {
    // Signal flows left-to-right: AC-coupling input cap, BJT, output
    // cap. Refdeses come from `tests/fixtures/common_emitter.cir`.
    // (`VIN` itself is `;@ ignore`d in the fixture and therefore not
    // emitted as a placed symbol — the input chain starts at CIN.)
    let sch = emit("common_emitter");
    let root = parse_sch(&sch);

    let cin_x = element_x(&root, "CIN").expect("CIN placed");
    let q_x = element_x(&root, "Q1").expect("Q1 placed");
    let cout_x = element_x(&root, "COUT").expect("COUT placed");

    assert!(
        cin_x < q_x,
        "{V6_HINT}: expected CIN.x ({cin_x:.2}) < Q1.x ({q_x:.2})"
    );
    assert!(
        q_x < cout_x,
        "{V6_HINT}: expected Q1.x ({q_x:.2}) < COUT.x ({cout_x:.2})"
    );
}

#[test]
#[ignore = "V6: placer treats elements generically; needs archetype matcher \
    (see CLAUDE.md \u{a7} Visual quality invariants V6)"]
fn v6_common_emitter_q1_central() {
    // Q1 must sit vertically between RC (collector resistor, above) and
    // RE (emitter resistor, below) — a strictly weaker form of the
    // rails-horizontal test, kept separate as a focused verifier of the
    // "BJT-in-the-middle" template invariant.
    let sch = emit("common_emitter");
    let root = parse_sch(&sch);

    let q_y = element_y(&root, "Q1").expect("Q1 placed");
    let collector_y = element_y(&root, "RC").expect("RC placed");
    let emitter_r_y = element_y(&root, "RE").expect("RE placed");

    assert!(
        collector_y < q_y && q_y < emitter_r_y,
        "{V6_HINT}: expected RC.y ({collector_y:.2}) < Q1.y ({q_y:.2}) < RE.y ({emitter_r_y:.2})"
    );
}

// --- framework smoke tests for the V6 helpers ----------------------------

#[test]
fn smoke_element_position_finds_by_reference_property() {
    let src = r#"(kicad_sch
        (symbol (lib_id "Device:R_US") (at 10 20 0)
            (property "Reference" "R1" (at 10 20 0))
            (property "Value" "1k" (at 10 20 0)))
        (symbol (lib_id "Device:C") (at 30 40 0)
            (property "Reference" "C1" (at 30 40 0))
            (property "Value" "1u" (at 30 40 0))))"#;
    let v: Value = lexpr::from_str(src).unwrap();
    assert_eq!(element_position(&v, "R1"), Some((10.0, 20.0)));
    assert_eq!(element_x(&v, "C1"), Some(30.0));
    assert_eq!(element_y(&v, "C1"), Some(40.0));
    assert_eq!(element_position(&v, "Q9"), None);
}

#[test]
fn smoke_label_positions_filters_by_net_name() {
    let src = r#"(kicad_sch
        (global_label "out" (at 0 0 0))
        (global_label "in" (at 5 5 0))
        (label "out" (at 9 9 0)))"#;
    let v: Value = lexpr::from_str(src).unwrap();
    let mut out = label_positions(&v, "out");
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    assert_eq!(out, vec![(0.0, 0.0), (9.0, 9.0)]);
}
