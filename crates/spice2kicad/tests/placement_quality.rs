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
