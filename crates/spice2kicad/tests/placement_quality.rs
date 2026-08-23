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
//! * **Fixture-wide quality** — V6 used to be enforced via three
//!   common-emitter archetype tests; those have been replaced (T8)
//!   with six general checks that iterate every fixture: no
//!   symbol-symbol overlap, no symbol-label overlap, rails ordered
//!   (Power above Ground), wire-detour budget, crossing-count budget,
//!   and a focused common-emitter signal-flow regression guard.
//!
//! Tests that fail against the current placer are `#[ignore]`d with a
//! pointer to the relevant CLAUDE.md section.
//!
//! The placer lives in `crates/spice-layout/src/`.

mod common;

use std::path::{Path, PathBuf};

use common::spice_to_kicad;
use kicad_symbols::{Library, Orientation, Rotation};
use lexpr::Value;

// --- driver bits ---------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> common::TempDir {
    common::TempDir::new("pq", name)
}

fn emit(name: &str) -> common::Emitted {
    let src = fixtures_dir().join(format!("{name}.cir"));
    let tmp = tempdir(name);
    let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
    common::Emitted::new(tmp, sch)
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
fn v5_rc_lowpass_short_out_wire() {
    // Coordinate-source note: this verifier reads pin/wire coordinates
    // from the *emitted* file (post-`translate_into_page`), whereas the
    // placer reasons in pre-translation placement coordinates. The two
    // frames differ by the uniform V15 page offset, but V5 measures a
    // wire *length* — a coordinate difference — which is invariant under
    // a uniform translation, so the two agree. (Latent drift surface: any
    // future V5-style metric that compares an emitted *absolute* coord
    // against a placer coord would NOT be translation-invariant.)
    let sch = emit("rc_lowpass");
    let root = parse_sch(&sch);
    let total = total_wire_length_for_net(&root, "out");
    // Zero wire length is the *ideal* outcome: pins coincident at
    // a single point, no routing needed (the placer found a
    // perfectly pin-facing orientation). Anything > 30 mm
    // indicates the placer failed to face the pins toward each
    // other.
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

// --- V7: symmetry-aware placement (multivibrator) ------------------------

const V7_HINT: &str = "V7: placer does not detect circuit symmetry; needs \
    graph-isomorphism matcher (see CLAUDE.md \u{a7} Visual quality \
    invariants V7)";

/// Orientation of a placed `(symbol …)` instance: `(rotation_degrees,
/// mirrored)`. The KiCad emitter writes rotation as the third number
/// inside `(at x y rot)`, and (when mirrored) emits a separate
/// `(mirror x)` or `(mirror y)` token. Returns `None` if no instance
/// matches `refdes`.
fn element_orientation(root: &Value, refdes: &str) -> Option<(f64, Option<String>)> {
    for sym in children(root, "symbol") {
        let Some(at) = find_child(sym, "at") else {
            continue;
        };
        let mut found_ref = None;
        for prop in children(sym, "property") {
            let mut it = list_iter(prop);
            it.next();
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
        it.next();
        it.next(); // x
        it.next(); // y
        let rotation = it.next().and_then(as_f64).unwrap_or(0.0);
        let mirror = find_child(sym, "mirror")
            .and_then(|m| list_iter(m).nth(1).and_then(as_str).map(str::to_owned));
        return Some((rotation, mirror));
    }
    None
}

// Tolerance for "mirrored about a common axis": one KiCad grid cell
// (1.27 mm). Today's placer arranges the eight emitted elements
// left-to-right with equal stride (one cell per slot), so RB and C
// pairs sit ~8.89 mm = 7 grid cells off the Q1/Q2 axis — well above
// the threshold. A real symmetric layout reuses the Q axis for all
// four pairs and lands them within a fraction of a cell.
const V7_AXIS_TOLERANCE_MM: f64 = 1.27;

/// Asserts both elements of a pair sit at mirrored x-distances about
/// `axis_x`, within [`V7_AXIS_TOLERANCE_MM`].
fn assert_x_symmetric(root: &Value, axis_x: f64, left: &str, right: &str) {
    let lx = element_x(root, left).unwrap_or_else(|| panic!("{left} placed"));
    let rx = element_x(root, right).unwrap_or_else(|| panic!("{right} placed"));
    let dl = (lx - axis_x).abs();
    let dr = (rx - axis_x).abs();
    let delta = (dl - dr).abs();
    assert!(
        delta <= V7_AXIS_TOLERANCE_MM,
        "{V7_HINT}: pair ({left}, {right}) not mirrored about x={axis_x:.2}: \
         |{left}.x - axis| = {dl:.2}, |{right}.x - axis| = {dr:.2}, \
         delta = {delta:.2} mm > {V7_AXIS_TOLERANCE_MM:.2} mm"
    );
}

#[test]
fn v7_multivibrator_x_symmetry() {
    // Multivibrator pairs (from tests/fixtures/multivibrator.cir):
    // Q1↔Q2, RC1↔RC2, RB1↔RB2, C1↔C2 — all mirrored about the
    // vertical axis through Q1/Q2's midpoint.
    let sch = emit("multivibrator");
    let root = parse_sch(&sch);
    let q1x = element_x(&root, "Q1").expect("Q1 placed");
    let q2x = element_x(&root, "Q2").expect("Q2 placed");
    let axis_x = f64::midpoint(q1x, q2x);

    assert_x_symmetric(&root, axis_x, "RC1", "RC2");
    assert_x_symmetric(&root, axis_x, "RB1", "RB2");
    assert_x_symmetric(&root, axis_x, "C1", "C2");
}

#[test]
fn v7_multivibrator_y_alignment() {
    // Vertical symmetry axis ⇒ each mirrored pair shares its Y.
    let sch = emit("multivibrator");
    let root = parse_sch(&sch);

    let tol = V7_AXIS_TOLERANCE_MM;
    for (a, b) in [("Q1", "Q2"), ("RC1", "RC2"), ("RB1", "RB2"), ("C1", "C2")] {
        let ay = element_y(&root, a).unwrap_or_else(|| panic!("{a} placed"));
        let by = element_y(&root, b).unwrap_or_else(|| panic!("{b} placed"));
        assert!(
            (ay - by).abs() <= tol,
            "{V7_HINT}: pair ({a}, {b}) not coplanar in Y: \
             {a}.y = {ay:.2}, {b}.y = {by:.2}, delta = {:.2} mm",
            (ay - by).abs()
        );
    }
}

#[test]
fn v7_multivibrator_orientation_mirrored() {
    // Q1 and Q2 must carry mirrored orientations: same rotation, but
    // exactly one of the two has a `(mirror y)` token so the BJT
    // arrows point toward each other. Today both are emitted with
    // identity orientation (rot=0, no mirror), so this test fails.
    let sch = emit("multivibrator");
    let root = parse_sch(&sch);

    let (q1_rot, q1_mirror) = element_orientation(&root, "Q1").expect("Q1 placed");
    let (q2_rot, q2_mirror) = element_orientation(&root, "Q2").expect("Q2 placed");

    assert!(
        (q1_rot - q2_rot).abs() < 1e-6,
        "{V7_HINT}: Q1 and Q2 must share rotation for a clean Y-mirror; \
         got Q1.rot = {q1_rot}, Q2.rot = {q2_rot}"
    );
    let q1_mirrored_y = q1_mirror.as_deref() == Some("y");
    let q2_mirrored_y = q2_mirror.as_deref() == Some("y");
    assert!(
        q1_mirrored_y ^ q2_mirrored_y,
        "{V7_HINT}: exactly one of Q1, Q2 must carry `(mirror y)`; \
         got Q1.mirror = {q1_mirror:?}, Q2.mirror = {q2_mirror:?}"
    );
}

// --- framework smoke tests for the V7 helpers ----------------------------

#[test]
fn smoke_element_orientation_reads_rotation_and_mirror() {
    let src = r#"(kicad_sch
        (symbol (lib_id "Device:Q_NPN_BCE") (at 10 20 0)
            (property "Reference" "Q1" (at 10 20 0)))
        (symbol (lib_id "Device:Q_NPN_BCE") (at 30 20 0) (mirror y)
            (property "Reference" "Q2" (at 30 20 0)))
        (symbol (lib_id "Device:R_US") (at 5 5 90)
            (property "Reference" "R1" (at 5 5 90))))"#;
    let v: Value = lexpr::from_str(src).unwrap();
    assert_eq!(element_orientation(&v, "Q1"), Some((0.0, None)));
    assert_eq!(element_orientation(&v, "Q2"), Some((0.0, Some("y".into()))));
    assert_eq!(element_orientation(&v, "R1"), Some((90.0, None)));
    assert_eq!(element_orientation(&v, "Nope"), None);
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

// --- Fixture-wide quality tests (T8) ------------------------------------
//
// Six general structural / aesthetic checks that iterate every fixture
// (replacing the three V6 archetype tests). They exercise the
// post-archetype layered placer:
//
//   1. no symbol-symbol overlap (per-symbol bbox + 1 cell padding)
//   2. no symbol-label overlap (label bbox vs symbol bbox)
//   3. rails ordered (max Y of Power-only elements < min Y of Ground-only)
//   4. wire-detour budget (emitted wire / rectilinear ideal ≤ K)
//   5. crossing-count budget (true wire-segment crossings ≤ K)
//   6. common-emitter signal-flow regression guard

/// **All ten fixtures.** This list was the classic five for a long
/// time, which left the port / hierarchical-sheet / definition-level
/// paths ungraded by the geometry verifiers that live in this file —
/// notably `no_symbol_symbol_overlap_across_fixtures` and
/// `no_power_glyph_foreign_body_overlap_across_fixtures`, both
/// unconditional-0 Tier-1 invariants. Two separate intermediate states
/// that shipped a VCC glyph inside a resistor body and two massively
/// overlapping opamp triangles on `opamp_definition_level` passed the
/// whole suite, because this list could not see that fixture.
/// `electrical_safety::SHEETS`, `labels::SHEETS`, `wire_geometry::FIXTURES`
/// and `rendered_text::FIXTURES` had already been extended; this one had
/// not. There is no fixture that belongs out of it.
const FIXTURES_FOR_QUALITY: &[(&str, &str)] = &[
    ("rc_lowpass", "rc_lowpass.cir"),
    ("rc_lowpass_ports", "rc_lowpass_ports.cir"),
    ("common_emitter", "common_emitter.cir"),
    ("multivibrator", "multivibrator.cir"),
    ("diff_pair", "diff_pair.cir"),
    ("opamp_inverting", "opamp_inverting.cir"),
    ("opamp_inverting_real", "opamp_inverting_real.cir"),
    ("port_shapes", "port_shapes.cir"),
    ("opamp_definition_level", "opamp_definition_level.cir"),
    ("named_rails", "named_rails.cir"),
    ("rc_phase_shift", "rc_phase_shift.cir"),
    ("two_stage_amp", "two_stage_amp.cir"),
    ("cascode_amp", "cascode_amp.cir"),
    ("lc_ladder_lpf", "lc_ladder_lpf.cir"),
    ("sallen_key_lpf", "sallen_key_lpf.cir"),
    ("wien_bridge_osc", "wien_bridge_osc.cir"),
    ("sallen_key_driven", "sallen_key_driven.cir"),
    ("shunt_feedback_amp", "shunt_feedback_amp.cir"),
];

fn fixtures() -> Vec<(&'static str, PathBuf)> {
    FIXTURES_FOR_QUALITY
        .iter()
        .map(|(name, file)| (*name, fixtures_dir().join(file)))
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct Bbox {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Bbox {
    fn intersects(&self, other: &Self) -> bool {
        // 1 µm tolerance: bboxes that just kiss (a common outcome
        // of 1.27 mm grid placement with 2.54 mm half-extents) do
        // not count as intersection.
        let eps = 1e-3;
        self.x0 + eps < other.x1
            && other.x0 + eps < self.x1
            && self.y0 + eps < other.y1
            && other.y0 + eps < self.y1
    }
}

/// Iterate every top-level placed `(symbol …)` (i.e. not the
/// `lib_symbols` body): each must carry an `(at …)` and a `Reference`
/// property. Returns `(refdes, position)` pairs.
fn placed_symbols(root: &Value) -> Vec<(String, Pt)> {
    let mut out = Vec::new();
    for sym in children(root, "symbol") {
        let Some(at) = find_child(sym, "at") else {
            continue;
        };
        let mut found_ref: Option<String> = None;
        for prop in children(sym, "property") {
            let mut it = list_iter(prop);
            it.next();
            let key = it.next().and_then(as_str);
            let val = it.next().and_then(as_str);
            if key == Some("Reference") {
                found_ref = val.map(str::to_owned);
                break;
            }
        }
        let Some(refdes) = found_ref else {
            continue;
        };
        // Skip power-symbol glyphs (Reference == "#PWR") and PWR_FLAG
        // driver markers (Reference == "#FLG"). Both are emitted by
        // `spice_route::route` at pin coordinates, so they intentionally
        // sit on top of the connected element's pin (a same-net label
        // anchored on that pin is V11-safe, not a defect) and would
        // always trigger overlap asserts that expect only "real" placed
        // elements.
        if refdes.starts_with("#PWR") || refdes.starts_with("#FLG") {
            continue;
        }
        let mut it = list_iter(at);
        it.next();
        let Some(x) = it.next().and_then(as_f64) else {
            continue;
        };
        let Some(y) = it.next().and_then(as_f64) else {
            continue;
        };
        out.push((refdes, (x, y)));
    }
    out
}

/// Load the standard fixture libraries used by every test fixture.
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

/// Decode a placed `(symbol …)` instance's `(at x y rot)` plus
/// optional `(mirror x|y)` token into an [`Orientation`] and translation.
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

fn placed_symbol_refdes_and_lib_id(sym: &Value) -> Option<(String, String)> {
    let mut lib_id = None;
    if let Some(lid) = find_child(sym, "lib_id")
        && let Some(s) = list_iter(lid).nth(1).and_then(as_str)
    {
        lib_id = Some(s.to_string());
    }
    let mut refdes = None;
    for prop in children(sym, "property") {
        let mut it = list_iter(prop);
        it.next();
        let key = it.next().and_then(as_str);
        let val = it.next().and_then(as_str);
        if key == Some("Reference") {
            refdes = val.map(str::to_owned);
            break;
        }
    }
    Some((refdes?, lib_id?))
}

/// Resolved world extent of a placed `(symbol …)` instance: the AABB
/// of the orientation-transformed body bbox unioned with the reach of
/// every pin (pin stem endpoint). This is the *real* geometry the
/// placer must keep non-overlapping — a blind fixed square (the old
/// `SYM_HALF_MM` model) hides body/pin-stub overlap of wide parts
/// like `Device:Q_NPN_BCE`.
///
/// Value-text width is deliberately excluded here: label/value-text
/// overlap is V13's scope. The placer still pads its spacing for text
/// (a separate clearance term), but this verifier only enforces the
/// body+pin no-overlap clause (V6, Tier-1 readability).
fn resolved_world_extent(library: &Library, sym: &Value) -> Option<(String, Bbox)> {
    let (refdes, lib_id) = placed_symbol_refdes_and_lib_id(sym)?;
    if refdes.starts_with("#PWR") || lib_id.starts_with("power:") {
        return None;
    }
    let (ox, oy, orient) = placed_symbol_pose(sym)?;
    let lib_sym = library.lookup(&lib_id)?;

    let mut x0 = f64::INFINITY;
    let mut y0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    let mut grow = |wx: f64, wy: f64| {
        x0 = x0.min(wx);
        y0 = y0.min(wy);
        x1 = x1.max(wx);
        y1 = y1.max(wy);
    };

    // Body bbox, orientation-transformed into world coords
    // (rotate/mirror via apply_point, then eeschema y-flip).
    if let Some(local) = lib_sym.body_bbox() {
        for (lx, ly) in [
            (local.x0, local.y0),
            (local.x0, local.y1),
            (local.x1, local.y0),
            (local.x1, local.y1),
        ] {
            let (rx, ry) = orient.apply_point(lx, ly);
            grow(ox + rx, oy - ry);
        }
    }
    // Pin reach: each pin's endpoint extends the extent.
    for tp in lib_sym.pins_in(orient) {
        grow(ox + tp.x, oy - tp.y);
    }

    if x0.is_finite() && x1.is_finite() && y0.is_finite() && y1.is_finite() {
        Some((refdes, Bbox { x0, y0, x1, y1 }))
    } else {
        None
    }
}

/// No two placed symbols' *resolved* extents (orientation-transformed
/// body bbox ∪ pin reach) may intersect. Budget 0, ratchet (CLAUDE.md
/// V6 no-overlap clause — Tier-1 readability). Replaces the old blind
/// 2.54 mm fixed-square model, which could not see wide parts'
/// body/pin-stub overlap.
#[test]
fn no_symbol_symbol_overlap_across_fixtures() {
    let library = load_test_library();
    // Collect-then-assert (Tier 0): every fixture is measured and
    // recorded before the terminal assertion, so a failure reports the
    // whole suite rather than the first offender.
    let mut failures: Vec<String> = Vec::new();
    for (name, path) in fixtures() {
        let tmp = tempdir(name);
        let sch = common::spice_to_kicad(&path, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);
        let bboxes: Vec<(String, Bbox)> = children(&root, "symbol")
            .into_iter()
            .filter_map(|sym| resolved_world_extent(&library, sym))
            .collect();
        let mut overlaps = 0usize;
        for i in 0..bboxes.len() {
            for j in (i + 1)..bboxes.len() {
                if bboxes[i].1.intersects(&bboxes[j].1) {
                    overlaps += 1;
                    failures.push(format!(
                        "{}: symbols {} and {} overlap (resolved extents {:?} / {:?})",
                        name, bboxes[i].0, bboxes[j].0, bboxes[i].1, bboxes[j].1,
                    ));
                }
            }
        }
        common::scoreboard::record_count("t0.sym_overlap", name, overlaps);
    }
    assert!(
        failures.is_empty(),
        "symbol/symbol overlap:\n{}",
        failures.join("\n")
    );
}

/// World-frame AABB of a placed symbol's *body* only (no pin reach),
/// orientation-transformed. The value-text crowding check measures
/// against the drawn body, not pin stems: a pin is a connection point
/// that wires legitimately land on, so value text clearing the body —
/// not the pin stems — is what a reader perceives as a clean gap.
fn resolved_body_bbox(library: &Library, sym: &Value) -> Option<(String, Bbox)> {
    let (refdes, lib_id) = placed_symbol_refdes_and_lib_id(sym)?;
    if refdes.starts_with("#PWR") || lib_id.starts_with("power:") {
        return None;
    }
    let (ox, oy, orient) = placed_symbol_pose(sym)?;
    let lib_sym = library.lookup(&lib_id)?;
    let local = lib_sym.body_bbox()?;
    let mut x0 = f64::INFINITY;
    let mut y0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for (lx, ly) in [
        (local.x0, local.y0),
        (local.x0, local.y1),
        (local.x1, local.y0),
        (local.x1, local.y1),
    ] {
        let (rx, ry) = orient.apply_point(lx, ly);
        x0 = x0.min(ox + rx);
        y0 = y0.min(oy - ry);
        x1 = x1.max(ox + rx);
        y1 = y1.max(oy - ry);
    }
    Some((refdes, Bbox { x0, y0, x1, y1 }))
}

/// Rendered width (mm) of a left-justified value text at the default
/// 1.27 mm size, from Newstroke's real per-glyph advances — the same
/// metric the emitter's `text_bbox` uses, so the verifier measures the
/// box the renderer actually draws.
fn value_text_width_mm(text: &str) -> f64 {
    kicad_symbols::text_metrics::text_width(text, 1.27)
}

/// World-frame AABB of a placed element's `(property "Value" …)` text,
/// left-justified at its `(at vx vy …)` anchor (rotation 0 for every
/// fixture's value property). Height mirrors the emitter's `text_bbox`
/// (`1.4 * size`, centred on the anchor Y). Returns `None` for elements
/// with no value property (power glyphs are filtered upstream anyway).
fn value_text_bbox(sym: &Value) -> Option<Bbox> {
    for prop in children(sym, "property") {
        let mut it = list_iter(prop);
        it.next();
        if it.next().and_then(as_str) != Some("Value") {
            continue;
        }
        let val = it.next().and_then(as_str)?;
        let at = find_child(prop, "at")?;
        let mut ait = list_iter(at);
        ait.next();
        let vx = ait.next().and_then(as_f64)?;
        let vy = ait.next().and_then(as_f64)?;
        let width = value_text_width_mm(val);
        let half_h = 0.7 * 1.27; // 1.4 * size / 2
        return Some(Bbox {
            x0: vx,
            y0: vy - half_h,
            x1: vx + width,
            y1: vy + half_h,
        });
    }
    None
}

/// Read every `*@align horizontal <refdes>...` cluster from a fixture's
/// SPICE source. Returns each cluster as the list of refdes named on
/// the directive line — membership is derived generally from the spec,
/// not hard-coded per fixture.
fn horizontal_align_clusters(cir: &Path) -> Vec<Vec<String>> {
    let src = std::fs::read_to_string(cir).expect("read .cir");
    let mut out = Vec::new();
    for line in src.lines() {
        let Some(rest) = line.trim().strip_prefix("*@align") else {
            continue;
        };
        let mut toks = rest.split_whitespace();
        if toks.next() != Some("horizontal") {
            continue;
        }
        let members: Vec<String> = toks.map(str::to_owned).collect();
        if members.len() >= 2 {
            out.push(members);
        }
    }
    out
}

/// V13 (Tier-1 readability): within a horizontal `*@align` cluster,
/// consecutive members must leave a clear horizontal gap between the
/// left member's rendered value-text box and the right member's nearest
/// left feature (drawn body or its own value text), measured only
/// across features that overlap in Y (so a value text drawn clear above
/// a wide neighbour's body is not counted as crowding). The align
/// stride is a HARD spacing floor at the candidate boundary
/// (`crates/spice-layout/src/lib.rs`), so this gap is a derived
/// consequence of that floor, not a tunable cost.
///
/// Ratchet: the per-fixture minimum gap is a recorded high-water mark
/// driven UP, never lowered. The literal below is the current measured
/// minimum across the fixture's clusters; a fix that widens the gap
/// raises it, a regression that narrows it trips the assert.
#[test]
fn value_text_clear_gap_in_align_clusters() {
    // Minimum clear horizontal gap (mm) between a left member's
    // value-text box and the right member's nearest left feature,
    // across Y-overlapping features. Per fixture with a horizontal
    // align cluster. Ratchet: drive UP, never lower.
    //
    // diff_pair's clusters after the align-stride text-gap floor:
    //   RC1↔RC2 (small resistors): 2.54 mm — a clean two-cell gap, and
    //     the fixture minimum (Q1↔Q2 clears by 7.06 mm). Before this
    //     fix the placer's value-text model under-reached by 2.54 mm,
    //     so RC1's "4.7k" sat one bare grid cell (1.27 mm) from RC2's
    //     body. 2.54 mm is the new high-water mark and the ratchet
    //     floor — zero slack; raise it on improvement only, never lower.
    const MIN_GAP_MM: &[(&str, f64)] = &[("diff_pair", 2.54)];

    let library = load_test_library();
    // Collect-then-assert (ADR-23 D2): a crowded fixture must not abort
    // the loop, or every later fixture goes unmeasured.
    let mut failures: Vec<String> = Vec::new();
    for (name, path) in fixtures() {
        let clusters = horizontal_align_clusters(&path);
        let budget = MIN_GAP_MM.iter().find(|(n, _)| *n == name).map(|&(_, b)| b);
        let Some(budget) = budget.filter(|_| !clusters.is_empty()) else {
            // Nothing to grade, but the cell must exist on both sides of
            // a scoreboard comparison or it reads as "nothing to say".
            common::scoreboard::record_count("v13.align_text_gap", name, 0);
            continue;
        };

        let tmp = tempdir(name);
        let sch = common::spice_to_kicad(&path, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);

        // Index placed symbols by refdes → (body bbox, value-text bbox).
        // Body excludes pin stems. Value text defaults to the body bbox
        // when a member carries no value property.
        let mut by_refdes: std::collections::HashMap<String, (Bbox, Bbox)> =
            std::collections::HashMap::new();
        for sym in children(&root, "symbol") {
            let Some((refdes, bbox)) = resolved_body_bbox(&library, sym) else {
                continue;
            };
            let vbox = value_text_bbox(sym).unwrap_or(bbox);
            by_refdes.insert(refdes, (bbox, vbox));
        }

        // Two bboxes overlap in Y (open intervals, 1 µm tolerance).
        let y_overlap = |a: &Bbox, b: &Bbox| a.y0 + 1e-3 < b.y1 && b.y0 + 1e-3 < a.y1;

        let mut tight = 0usize;
        for cluster in clusters {
            // Members present in the schematic, ordered left-to-right by
            // body x0 so "consecutive" is geometric.
            let mut members: Vec<(String, Bbox, Bbox)> = cluster
                .iter()
                .filter_map(|r| by_refdes.get(r).map(|&(b, v)| (r.clone(), b, v)))
                .collect();
            members.sort_by(|a, b| a.1.x0.partial_cmp(&b.1.x0).unwrap());
            for w in members.windows(2) {
                let (lref, lbody, ltext) = &w[0];
                let (rref, rbody, rtext) = &w[1];
                // For each (left feature, right feature) pair sharing a
                // Y band, the horizontal clearance must meet the floor:
                // the left value text crowding the right body/text, plus
                // the symmetric cases.
                let mut min_gap = f64::INFINITY;
                for left_feat in [ltext, lbody] {
                    for right_feat in [rbody, rtext] {
                        if y_overlap(left_feat, right_feat) {
                            min_gap = min_gap.min(right_feat.x0 - left_feat.x1);
                        }
                    }
                }
                if !min_gap.is_finite() {
                    continue; // no Y-overlapping features → no crowding
                }
                if min_gap + 1e-6 < budget {
                    tight += 1;
                    failures.push(format!(
                        "{name}: align value-text gap between {lref} and {rref} is \
                         {min_gap:.3} mm, below the {budget:.3} mm ratchet floor \
                         (drive UP, never lower)",
                    ));
                }
            }
        }
        common::scoreboard::record_count("v13.align_text_gap", name, tight);
    }
    assert!(
        failures.is_empty(),
        "align-cluster value-text gaps below the ratchet floor:\n  {}",
        failures.join("\n  "),
    );
}

/// Iterate every `(global_label …)` / `(label …)`: returns `(name, pos)`.
fn all_labels(root: &Value) -> Vec<(String, Pt)> {
    let mut out = Vec::new();
    for head_name in ["global_label", "label"] {
        for node in children(root, head_name) {
            let Some(name) = list_iter(node).nth(1).and_then(as_str) else {
                continue;
            };
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
            out.push((name.to_string(), (x, y)));
        }
    }
    out
}

#[test]
fn no_symbol_label_overlap_across_fixtures() {
    // Define "label overlaps symbol" as: the label *anchor point*
    // sits inside the symbol's body bounding box. KiCad anchors
    // labels at pin endpoints, which lie outside the body, with
    // the glyph extending outward — so a label anchor inside the
    // body is genuinely a placement bug. We do not penalise glyph
    // overlap because we don't know which way each label justifies
    // (KiCad picks based on shape + rotation).
    // Collect-then-assert (ADR-23 D2): an overlapping fixture must not
    // abort the loop, or every later fixture goes unmeasured.
    let mut failures: Vec<String> = Vec::new();
    for (name, path) in fixtures() {
        let tmp = tempdir(name);
        let sch = common::spice_to_kicad(&path, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);
        let placed = placed_symbols(&root);
        // Tighter half-extent for label-vs-symbol: the smallest body
        // (a VDC source circle) is ~1.27 mm radius. A label anchor
        // closer to a symbol centre than that is genuinely on top
        // of the body drawing.
        let body_half = 1.27_f64;
        let mut hits = 0usize;
        for (lname, lpos) in all_labels(&root) {
            for (refdes, spos) in &placed {
                let dx = (lpos.0 - spos.0).abs();
                let dy = (lpos.1 - spos.1).abs();
                let eps = 1e-3_f64;
                if dx + eps < body_half && dy + eps < body_half {
                    hits += 1;
                    failures.push(format!(
                        "{name}: label {lname:?} anchor {lpos:?} sits inside symbol \
                         {refdes} body (centre {spos:?}, half {body_half})",
                    ));
                }
            }
        }
        common::scoreboard::record_count("v13.label_in_body", name, hits);
    }
    assert!(
        failures.is_empty(),
        "label anchors inside a symbol body (budget 0):\n  {}",
        failures.join("\n  "),
    );
}

/// Build a refdes → set-of-net-names map by re-reading the SPICE
/// fixture. We deliberately avoid pulling in `spice-resolve` here:
/// each line is parsed by-hand for the leading refdes and its
/// node names, mirroring the lightweight parser already used in
/// `tests/common/mod.rs::Canonical`.
fn refdes_to_nets(spice_path: &Path) -> std::collections::HashMap<String, Vec<String>> {
    let src = std::fs::read_to_string(spice_path).expect("read spice");
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for raw in src.lines() {
        let line = raw.split(';').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('*') || line.starts_with('.') {
            continue;
        }
        let mut toks = line.split_whitespace();
        let Some(refdes) = toks.next() else {
            continue;
        };
        let r0 = refdes.chars().next().unwrap_or(' ').to_ascii_uppercase();
        // Element line shape: refdes node1 node2 ... value/model.
        // Number of node terminals depends on element type; we just
        // collect *every* alphanumeric/underscore token after the
        // refdes that looks like a net (heuristic: not all digits, not
        // a known model keyword). For the rail-ordering test we only
        // need to know what nets the element touches, so over-
        // collection is fine — net classification by name (vcc, 0)
        // dominates the result.
        let n_terms = match r0 {
            'R' | 'C' | 'L' | 'V' | 'I' | 'D' => 2,
            'Q' | 'J' => 3,
            'M' => 4,
            'X' => {
                // Subckt: collect all but last token (subckt name).
                let v: Vec<&str> = toks.clone().collect();
                if v.len() < 2 {
                    continue;
                }
                v.len() - 1
            }
            _ => 0,
        };
        let nets: Vec<String> = toks.take(n_terms).map(str::to_owned).collect();
        out.insert(refdes.to_string(), nets);
    }
    out
}

#[test]
fn rails_correctly_ordered_across_fixtures() {
    // Collect-all + XFAIL registry — see `tests/common/xfail.rs` and the
    // note on `v14_rail_pin_faces_rail`.
    let mut xf = common::xfail::Guard::new("rails_correctly_ordered_across_fixtures");
    for (name, path) in fixtures() {
        let tmp = tempdir(name);
        let sch = common::spice_to_kicad(&path, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);
        let placed = placed_symbols(&root);
        let nets_per = refdes_to_nets(&path);

        let touches_power = |nets: &[String]| {
            nets.iter().any(|n| {
                let lo = n.to_ascii_lowercase();
                matches!(lo.as_str(), "vcc" | "vdd" | "v+" | "vplus")
            })
        };
        let touches_ground = |nets: &[String]| nets.iter().any(|n| n == "0");
        let touches_neg = |nets: &[String]| {
            nets.iter().any(|n| {
                let lo = n.to_ascii_lowercase();
                matches!(lo.as_str(), "vee" | "vss" | "v-" | "vminus")
            })
        };

        // Power-only = touches Power but not Ground.
        // Ground-only = touches Ground but not Power.
        // VEE / negative rail elements count as ground-side anchors
        // (they are pulled to the bottom band by `bands.rs`).
        let mut power_ys: Vec<f64> = Vec::new();
        let mut ground_ys: Vec<f64> = Vec::new();
        for (refdes, pos) in &placed {
            let Some(nets) = nets_per.get(refdes) else {
                continue;
            };
            let p = touches_power(nets);
            let g = touches_ground(nets) || touches_neg(nets);
            if p && !g {
                power_ys.push(pos.1);
            }
            if g && !p {
                ground_ys.push(pos.1);
            }
        }
        if power_ys.is_empty() || ground_ys.is_empty() {
            // Nothing to grade — but still tell the guard, so a stale
            // registry entry naming this fixture is reported, and still
            // report the cell, so the scoreboard sees a 0 rather than a
            // hole it cannot distinguish from "nothing to say".
            common::scoreboard::record_count("v14.rail_order", name, 0);
            xf.record(name, None);
            continue;
        }
        let max_power = power_ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let min_ground = ground_ys.iter().copied().fold(f64::INFINITY, f64::min);
        // KiCad Y grows downward → Power should be at smaller Y than
        // Ground. Allow one grid cell of slack.
        let misordered = max_power >= min_ground + 1.27;
        common::scoreboard::record_count("v14.rail_order", name, usize::from(misordered));
        xf.record(
            name,
            misordered.then(|| {
                format!(
                    "{name}: rails not ordered. max(Power Y) = {max_power:.2}, \
                     min(Ground Y) = {min_ground:.2} (Power should be above Ground)"
                )
            }),
        );
    }
    xf.finish();
}

/// Quantise a millimetre coordinate to an integer key. One grid step is
/// 1.27 mm, so a 1 µm quantum can never bridge two distinct grid points.
#[allow(clippy::cast_possible_truncation)]
fn key(p: Pt) -> (i64, i64) {
    ((p.0 * 1000.0).round() as i64, (p.1 * 1000.0).round() as i64)
}

/// World pin positions grouped by net name.
type PinsByNet = std::collections::HashMap<String, Vec<Pt>>;

/// World pin positions grouped by net, recovered by re-resolving the
/// SPICE source and transforming each library pin through its placed
/// pose. This is the same join `electrical_safety.rs::world_pins_for_sheet`
/// performs; it is duplicated here (rather than shared) because the two
/// test binaries need different per-pin payloads.
///
/// Returns `(pins_by_net, glyph_anchors)`. `glyph_anchors` holds the
/// quantised coordinate of every `power:*` glyph / `PWR_FLAG` instance:
/// a net reaching one of those is drawn with glyphs rather than routed
/// wire (V10) and is excluded from the detour measure — see
/// [`wire_detour`].
fn world_pins_by_net(
    spice_path: &std::path::Path,
    root: &Value,
) -> (PinsByNet, std::collections::HashSet<(i64, i64)>) {
    use spice_diagnostics::FileId;
    use std::collections::{HashMap, HashSet};

    let library = load_test_library();
    let source = std::fs::read_to_string(spice_path).expect("read spice fixture");
    let parsed = spice_parser::parse(&source, FileId(0)).expect("parse spice fixture");
    let resolved =
        spice_resolve::resolve(&parsed.netlist, &library).expect("resolve spice fixture");

    let mut by_refdes: HashMap<String, HashMap<String, String>> = HashMap::new();
    for el in &resolved.elements {
        let mut pairs = HashMap::new();
        for (i, kicad_pin) in el.pin_mapping.iter().enumerate() {
            if let Some(net) = el.nodes.get(i) {
                pairs.insert(kicad_pin.clone(), net.clone());
            }
        }
        by_refdes.insert(el.refdes.clone(), pairs);
    }

    let mut out: HashMap<String, Vec<Pt>> = HashMap::new();
    let mut glyph_anchors: HashSet<(i64, i64)> = HashSet::new();
    for sym in children(root, "symbol") {
        let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
            continue;
        };
        let Some((ox, oy, orient)) = placed_symbol_pose(sym) else {
            continue;
        };
        let Some(lib_sym) = library.lookup(&lib_id) else {
            continue;
        };
        let Some(pin_to_net) = by_refdes.get(&refdes) else {
            continue;
        };
        for tp in lib_sym.pins_in(orient) {
            if let Some(net) = pin_to_net.get(&tp.number) {
                out.entry(net.clone())
                    .or_default()
                    .push((ox + tp.x, oy - tp.y));
            }
        }
    }

    // Hierarchical-sheet port pins: the parent-side terminal of a child
    // sheet is a real routing endpoint even though it is not a symbol.
    // The emitted `(pin …)` name is the CHILD-side port name, which need
    // not equal the parent net (`X1 0 inv out … OPAMP` binds child `inp`
    // to parent `0`), so the parent net is recovered positionally: the
    // sheet's pins are emitted in `SubcktPorts.ports` order and
    // `SheetInstance.nodes` is in that same order.
    let sheet_nets: HashMap<&str, &Vec<String>> = resolved
        .sheet_instances
        .iter()
        .map(|si| (si.refdes.as_str(), &si.nodes))
        .collect();
    for sheet in children(root, "sheet") {
        let mut sheet_name: Option<String> = None;
        for prop in children(sheet, "property") {
            let mut it = list_iter(prop);
            it.next();
            if it.next().and_then(as_str) == Some("Sheetname") {
                sheet_name = it.next().and_then(as_str).map(str::to_owned);
                break;
            }
        }
        let Some(nodes) = sheet_name.as_deref().and_then(|n| sheet_nets.get(n)) else {
            continue;
        };
        for (i, (_, x, y)) in sheet_port_pins(sheet).into_iter().enumerate() {
            if let Some(net) = nodes.get(i) {
                out.entry(net.clone()).or_default().push((x, y));
            }
        }
    }

    // Every `power:*` glyph / PWR_FLAG anchor, as a coordinate. A wire
    // component touching one of these is a glyph stub, not routing —
    // `wire_detour` uses this to drop the whole net.
    for sym in children(root, "symbol") {
        if let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym)
            && (refdes.starts_with("#PWR")
                || refdes.starts_with("#FLG")
                || lib_id.starts_with("power:"))
            && let Some((ox, oy, _)) = placed_symbol_pose(sym)
        {
            glyph_anchors.insert(key((ox, oy)));
        }
    }

    (out, glyph_anchors)
}

fn uf_find(uf: &mut [usize], mut x: usize) -> usize {
    while uf[x] != x {
        uf[x] = uf[uf[x]];
        x = uf[x];
    }
    x
}

/// Half-perimeter of the bounding box of `pts` — the exact lower bound
/// on any rectilinear tree spanning them.
fn hpwl(pts: &[Pt]) -> f64 {
    let (mut x0, mut x1) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut y0, mut y1) = (f64::INFINITY, f64::NEG_INFINITY);
    for &(x, y) in pts {
        x0 = x0.min(x);
        x1 = x1.max(x);
        y0 = y0.min(y);
        y1 = y1.max(y);
    }
    (x1 - x0) + (y1 - y0)
}

/// Per-fixture wire **detour**: emitted wire length over the ideal
/// rectilinear lower bound, measured on real pin geometry.
///
/// The unit of measurement is a connected COMPONENT of emitted ink (see
/// the accumulation loop for why, not the net). For each component the
/// ideal is the half-perimeter of the bounding box of the pins attached
/// to it — the exact lower bound on any rectilinear Steiner tree
/// spanning them — so a perfect router scores 1.0 and the ratio reads
/// directly as "how far the ink wanders past the shortest possible
/// route".
///
/// Numerator and denominator are both restricted to nets that actually
/// carry **routed** wire. Two exclusions keep the measure honest, and
/// both are needed in the same direction — without them the ratio is
/// deflated toward zero and re-hides exactly the defects this metric
/// exists to catch:
///
///  * nets with no emitted wire at all contribute neither term;
///  * **glyph-carried (power / ground) nets** are dropped whole. Per V10
///    these are drawn as `power:*` glyphs, not routed: their only ink is
///    the short detached-glyph stub, while their "pin" set includes the
///    PWR_FLAG driver anchor parked far off to one side. On
///    `opamp_inverting` that pairs a 2.54 mm stub with a 66 mm bbox —
///    a meaningless 0.04 ratio that drags the fixture total to 0.22,
///    below the theoretical floor of 1.0.
///
/// Segments are attributed to nets by union-find over shared endpoints
/// and pin coincidences — the same connectivity model KiCad itself uses
/// (V11) — rather than by proximity.
fn wire_detour(spice_path: &std::path::Path, root: &Value) -> (f64, f64) {
    use std::collections::HashMap;

    let segs = wire_segments(root);
    let (pins_by_net, glyph_anchors) = world_pins_by_net(spice_path, root);
    if segs.is_empty() {
        return (0.0, 0.0);
    }

    // Union-find over quantised coordinates: wire endpoints, plus any pin
    // lying on a segment (endpoint or interior — V11 clause 2).
    let mut ids: HashMap<(i64, i64), usize> = HashMap::new();
    let id_of = |k: (i64, i64), ids: &mut HashMap<(i64, i64), usize>| -> usize {
        let n = ids.len();
        *ids.entry(k).or_insert(n)
    };
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for &(a, b) in &segs {
        let ia = id_of(key(a), &mut ids);
        let ib = id_of(key(b), &mut ids);
        edges.push((ia, ib));
    }
    let on_segment = |p: Pt, a: Pt, b: Pt| -> bool {
        let eps = 1e-6;
        let within = |v: f64, lo: f64, hi: f64| v >= lo.min(hi) - eps && v <= lo.max(hi) + eps;
        ((a.0 - b.0).abs() < eps && (p.0 - a.0).abs() < eps && within(p.1, a.1, b.1))
            || ((a.1 - b.1).abs() < eps && (p.1 - a.1).abs() < eps && within(p.0, a.0, b.0))
    };
    let mut pin_ids: Vec<(usize, &str)> = Vec::new();
    for (net, pts) in &pins_by_net {
        for &p in pts {
            let mut touched = None;
            for &(a, b) in &segs {
                if on_segment(p, a, b) {
                    touched = Some(key(a));
                    break;
                }
            }
            if let Some(anchor) = touched {
                let ip = id_of(key(p), &mut ids);
                let ia = id_of(anchor, &mut ids);
                edges.push((ip, ia));
                pin_ids.push((ip, net.as_str()));
            }
        }
    }

    let mut uf = vec![0usize; ids.len()];
    for (i, slot) in uf.iter_mut().enumerate() {
        *slot = i;
    }
    for (a, b) in edges {
        let (ra, rb) = (uf_find(&mut uf, a), uf_find(&mut uf, b));
        if ra != rb {
            uf[ra] = rb;
        }
    }

    // component -> net. V11 guarantees a component carries pins of only
    // one net (a foreign-pin coincidence would be a short), so the fold
    // below is well defined — but `pins_by_net` is a HashMap, so sort
    // first and keep the choice deterministic even if V11 ever regresses.
    // A nondeterministic ratchet is worse than a wrong one.
    pin_ids.sort_unstable();
    let mut comp_net: HashMap<usize, String> = HashMap::new();
    for (ip, net) in pin_ids {
        let r = uf_find(&mut uf, ip);
        comp_net.entry(r).or_insert_with(|| net.to_string());
    }

    // Nets whose ink reaches a power glyph / PWR_FLAG anchor: V10
    // glyph-carried, dropped whole (see the doc comment).
    let mut glyph_nets: std::collections::HashSet<String> = std::collections::HashSet::new();
    for &k in &glyph_anchors {
        if let Some(&i) = ids.get(&k)
            && let Some(net) = comp_net.get(&uf_find(&mut uf, i))
        {
            glyph_nets.insert(net.clone());
        }
    }

    // Accumulate per CONNECTED COMPONENT of ink, not per net. A net may
    // legitimately be drawn as two disjoint wire trees bridged by a V4
    // plain-label name-jump pair (`opamp_inverting`'s `inv`: RIN's pin
    // carries a label, and only RF→sheet is wired). Charging that net the
    // bounding box of ALL its pins would bill the router for a span it
    // was never asked to cross — measured 0.26 for a route that is in
    // fact optimal. The component is the unit of routing, so it is the
    // unit of measurement.
    let mut wire_by_comp: HashMap<usize, f64> = HashMap::new();
    let mut pins_by_comp: HashMap<usize, Vec<Pt>> = HashMap::new();
    for &(a, b) in &segs {
        let r = uf_find(&mut uf, ids[&key(a)]);
        if comp_net.get(&r).is_some_and(|n| !glyph_nets.contains(n)) {
            *wire_by_comp.entry(r).or_default() += manhattan(a, b);
        }
    }
    for (net, pts) in &pins_by_net {
        if glyph_nets.contains(net) {
            continue;
        }
        for &p in pts {
            if let Some(&i) = ids.get(&key(p)) {
                let r = uf_find(&mut uf, i);
                pins_by_comp.entry(r).or_default().push(p);
            }
        }
    }

    let (mut wire, mut ideal) = (0.0, 0.0);
    for (comp, len) in &wire_by_comp {
        let Some(pts) = pins_by_comp.get(comp) else {
            continue;
        };
        if pts.len() < 2 {
            continue;
        }
        wire += len;
        ideal += hpwl(pts);
    }
    (wire, ideal)
}

#[test]
fn wire_detour_within_budget_across_fixtures() {
    // Per-fixture wire DETOUR: emitted wire length over the rectilinear
    // ideal for the same ink (see `wire_detour`). 1.0 is a route that
    // could not be shorter; 1.4 means 40% of the ink is wander.
    //
    // This is deliberately a RATIO, not a length. Absolute wire length is
    // not a project objective — see the HPWL ablation in
    // `docs/layout-adr.md` ("for schematics we maximise READABILITY; area
    // is not important at all"). What is a readability defect is ink that
    // wanders past the route it needed to take, and that is scale-free.
    //
    // HISTORY — this verifier was VACUOUS from introduction until now.
    // Its baseline was derived from *labels* (`pin_pair_manhattan_sum`),
    // and nine of the ten fixtures emit no multi-pin labelled net at all,
    // so they hit a `baseline < 1e-6` early-`continue` and were never
    // graded — including all five fixtures carrying original budgets. The
    // one fixture that did reach the assertion, `opamp_inverting`, was
    // graded against a number that happened to mean something else. The
    // baseline is now pin geometry, so all ten are graded.
    //
    // The literals below are therefore NOT a ratchet lowering; they are
    // first honest measurements of a metric that had never run. They are
    // zero-slack (measured value, rounded up in the 4th decimal only) and
    // ratchet DOWN from here per CLAUDE.md § "Budgets are ratchets".
    let budgets: &[(&str, f64)] = &[
        // --- ADR-23 PROMOTION of `--placer=flow-seed` to the default
        // (owner-approved, 2026-08-18) ---------------------------------
        //
        // Every literal re-recorded at the NEW DEFAULT's measured ratio,
        // ceil-to-4dp as everywhere in this table (ADR-23 D4). Suite
        // aggregate: -37.87 points, i.e. 37.9 percentage points of
        // excess wire removed, dominated by `two_stage_amp`
        // 1.8566 -> 1.0795 (the suite's worst detour, now near-ideal).
        // Six fixtures rise, worst `sallen_key_lpf` 1.0407 -> 1.3019 and
        // `cascode_amp` 1.0843 -> 1.2196; that is the expected shape of a
        // whole-placer swap and is NOT available to an ordinary change.
        //
        // Also reclaims PRE-EXISTING slack unrelated to the swap:
        // `rc_lowpass` 1.167 -> 1.0 and `rc_lowpass_ports` 1.4001 -> 1.0.
        // Both fixtures are byte-identical across the promotion; their
        // literals were simply stale (both measure 1.0 - 1.4e-15, an
        // exactly-ideal route). Ratchet DOWN, always permitted.
        ("rc_lowpass", 1.0),
        ("common_emitter", 1.0536),
        ("multivibrator", 1.0481),
        ("diff_pair", 1.0556),
        // 1.1464 → 1.1952. RISE — a Tier-2 (V6 wire-detour) cost paid for a
        // Tier-1 (V12) gain: extending the router's power-glyph obstacle to
        // the full drawn footprint (body ∪ stem-to-pin) forces the `out`
        // feedback trunk one grid cell up (y 30.48 → 29.21) so it clears the
        // VCC chevron instead of grazing its open base. The detour is the
        // minimal +2.54 mm (two vertical segments each +1 cell), adds no
        // bends (V16 B/J unchanged), and the tier ordering permits Tier 2
        // paying for Tier 1. NOT an owner decision — landed on assistant
        // judgement under the standing instruction to proceed; flagged for
        // owner sign-off, re-examine rather than cite as precedent.
        ("opamp_inverting_real", 1.2326),
        ("rc_lowpass_ports", 1.0),
        ("opamp_inverting", 1.0834),
        ("port_shapes", 1.1143),
        // 1.0984 → 1.0732. Channel-row banding (Option B) reads both
        // channels left-to-right as congruent rows, shortening the routed
        // ink relative to its rectilinear ideal. Ratchet DOWN.
        ("opamp_definition_level", 1.0732),
        ("named_rails", 1.077),
        // 1.0438 -> 1.0481 with the promoted flow-seed default: a
        // 0.4 pp RISE, the smallest in the swap.
        ("rc_phase_shift", 1.0481),
        // 1.8566 -> 1.0795 with the promoted flow-seed default. This was
        // the suite's worst detour by a wide margin (86% longer than the
        // rectilinear ideal) and is now near-ideal — the single largest
        // Tier-2 win of the promotion. Ratchet DOWN.
        ("two_stage_amp", 1.0795),
        // --- F2 (v0.2 roadmap, second wave) NEW-GEOMETRY BASELINES.
        // Zero slack at 4 dp (the convention every literal here uses),
        // ratchet DOWN. No existing fixture's literal moved.
        ("cascode_amp", 1.2196),
        ("lc_ladder_lpf", 1.1506),
        ("sallen_key_lpf", 1.3019),
        ("wien_bridge_osc", 1.0899),
        // --- F3 (Tier-0 router fix, ADR-24): promoted out of
        // `tests/f0_defects.rs` once the Steiner-vertex-on-foreign-pin
        // defect was fixed. NEW-GEOMETRY BASELINES, zero slack, ratchet
        // DOWN only. Adding them moved no existing fixture's literal.
        ("sallen_key_driven", 1.0764),
        // 1.2410 -> 1.2076 with the promoted flow-seed default, which
        // retires the rail-stub-SIDE-fix rise that was awaiting owner
        // sign-off. Ratchet DOWN.
        ("shunt_feedback_amp", 1.2076),
    ];
    // Collect-then-assert: an in-loop `assert!` truncates the report at
    // the first offending fixture, which is the ADR-19 M4 "gate-set
    // lesson" in miniature — and it also loses every later fixture's
    // scoreboard record. Same assertions, all of them reported.
    let mut failures: Vec<String> = Vec::new();
    for (name, path) in fixtures() {
        let tmp = tempdir(name);
        let sch = common::spice_to_kicad(&path, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);
        let (total, baseline) = wire_detour(&path, &root);
        if baseline <= 1e-6 {
            // Vacuity guard. Collected rather than asserted in place: a
            // panic here aborts the whole test function, so every later
            // fixture goes unmeasured and reports nothing to the ADR-23
            // sink. Nothing is recorded for THIS fixture — `ratio` would
            // be inf/NaN, and a wrong cell is worse than an absent one,
            // which the aggregator flags as a one-sided cell.
            failures.push(format!(
                "{name}: no wired multi-pin net — the detour metric cannot grade this \
                 fixture, which is exactly the vacuity this verifier was rebuilt to remove",
            ));
            continue;
        }
        let ratio = total / baseline;
        common::scoreboard::record("detour", name, ratio);
        // The denominator is a true lower bound on any rectilinear route
        // of this ink, so a sub-1.0 reading is impossible for a correct
        // measurement and means the METRIC has broken (a net whose pins
        // are counted but whose wire is not), never that the router got
        // clever. Guarding it here is what stops this verifier drifting
        // back into vacuity unnoticed.
        if ratio < 1.0 - 1e-9 {
            failures.push(format!(
                "{name}: wire_detour = {ratio:.4} is below the theoretical floor of 1.0 — \
                 the measurement is broken, not the router (emitted wire = {total:.2} mm, \
                 rectilinear ideal = {baseline:.2} mm)",
            ));
            continue;
        }
        if std::env::var_os("S2K_QUALITY_DUMP").is_some() {
            println!("wire_detour (\"{name}\", {ratio}),");
            continue;
        }
        let &(_, budget) = budgets
            .iter()
            .find(|(n, _)| *n == name)
            .expect("budget for fixture");
        if ratio > budget {
            failures.push(format!(
                "{name}: wire_detour = {ratio:.3} > budget {budget:.3} \
                 (emitted wire = {total:.2} mm, rectilinear ideal = {baseline:.2} mm)",
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "wire-detour budget exceeded:\n{}",
        failures.join("\n")
    );
}

/// True wire-segment crossings: count pairs of wires that intersect
/// at an interior point (not at a shared endpoint).
fn count_wire_crossings(root: &Value) -> u32 {
    let segs = wire_segments(root);
    let mut count = 0_u32;
    for (i, &(a1, b1)) in segs.iter().enumerate() {
        for &(a2, b2) in segs.iter().skip(i + 1) {
            if segments_cross_interior(a1, b1, a2, b2) {
                count += 1;
            }
        }
    }
    count
}

fn segments_cross_interior(a1: Pt, b1: Pt, a2: Pt, b2: Pt) -> bool {
    let orient =
        |p: Pt, q: Pt, r: Pt| -> f64 { (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0) };
    let d1 = orient(a2, b2, a1);
    let d2 = orient(a2, b2, b1);
    let d3 = orient(a1, b1, a2);
    let d4 = orient(a1, b1, b2);
    let eps = 1e-9;
    (d1 > eps && d2 < -eps || d1 < -eps && d2 > eps)
        && (d3 > eps && d4 < -eps || d3 < -eps && d4 > eps)
}

#[test]
fn crossing_count_within_budget_across_fixtures() {
    // Per-fixture wire-segment crossing budgets. The channel router
    // routes per-net escape rows independently, so any net pair
    // whose pin-bboxes overlap will have multiple wire-segment
    // crossings — that is *router* behaviour, not a placer
    // failure. Budgets here reflect what is achievable today on
    // each fixture; tighten when a smarter router lands.
    // Ratchet high-water marks (zero slack). Phase-2 wire cleanup
    // (collapse collinear same-net overlaps into a non-overlapping,
    // vertex-preserving cover + interior-T junction dots) plus the
    // foreign power-glyph router obstacle lowered the measured crossings
    // from the prior R7 marks: common_emitter 4→3, diff_pair 1→0,
    // opamp_inverting_real 1→0. multivibrator holds at 4, rc_lowpass at
    // 0. Never raise.
    //
    // Crossing-aware V11/V12 detour selection (`conflict::CrossPass`)
    // then lowered common_emitter 3→2. Never raise.
    let budgets: &[(&str, u32)] = &[
        // --- ADR-23 PROMOTION of `--placer=flow-seed` to the default
        // (owner-approved, 2026-08-18) ---------------------------------
        //
        // Re-recorded at the NEW DEFAULT's measured counts (ADR-23 D4).
        // Crossings fall 18 points suite-wide and NO fixture rises:
        // `two_stage_amp` 10 -> 0, `rc_phase_shift` 5 -> 0,
        // `cascode_amp` 2 -> 0, `sallen_key_lpf` 2 -> 1. Making X mean
        // signal depth is what un-tangles them; twelve of eighteen
        // fixtures now measure zero.
        ("rc_lowpass", 0),
        // 2 -> 0. Reclaimed slack: the fixture measures 0 crossings on
        // master today. Ratchet DOWN, per CLAUDE.md § "Budgets are
        // ratchets, not knobs" ("when you fix something, read the new
        // count and lower the literal; don't leave slack").
        ("common_emitter", 0),
        ("multivibrator", 4),
        ("diff_pair", 0),
        ("opamp_inverting_real", 0),
        // Newly graded (the fixture list was extended to all ten).
        ("rc_lowpass_ports", 0),
        ("opamp_inverting", 0),
        ("port_shapes", 0),
        // 6 -> 0. Recorded at 6 when this fixture was first graded, with
        // the note that it would fall once the seed-stride placement
        // fault was fixed. It was, and it did. Ratchet DOWN.
        ("opamp_definition_level", 0),
        ("named_rails", 0),
        // 5 -> 0 with the promoted flow-seed default. This retires the
        // rail-stub-SIDE-fix rise (2 -> 5) that was awaiting owner
        // sign-off: the crossings it introduced were the ladder trunks
        // fighting a rail-hop column assignment, and signal-depth
        // columns remove them outright. Ratchet DOWN.
        ("rc_phase_shift", 0),
        // 10 -> 0 with the promoted flow-seed default. This was the
        // suite worst and the fixture the flow diagnosis was built on:
        // the chain `in->b1->c1->b2->c2->out` needs five columns and the
        // rail-hop layering gave it {0,1,1,1,3}. Ratchet DOWN.
        ("two_stage_amp", 0),
        // --- F2 (v0.2 roadmap, second wave) NEW-GEOMETRY BASELINES.
        // Ratchet DOWN.
        ("cascode_amp", 0),
        // Zero crossings — the ladder is the one new fixture the placer
        // draws without crossing itself, which is what makes its 16
        // bends a pure detour result rather than a tangle.
        ("lc_ladder_lpf", 0),
        ("sallen_key_lpf", 1),
        ("wien_bridge_osc", 2),
        // --- F3 (Tier-0 router fix, ADR-24): promoted out of
        // `tests/f0_defects.rs` once the Steiner-vertex-on-foreign-pin
        // defect was fixed. NEW-GEOMETRY BASELINES, zero slack, ratchet
        // DOWN only. Adding them moved no existing fixture's literal.
        ("sallen_key_driven", 3),
        ("shunt_feedback_amp", 0),
    ];
    // Collect-then-assert: see `wire_detour_within_budget_across_fixtures`.
    let mut failures: Vec<String> = Vec::new();
    for (name, path) in fixtures() {
        let tmp = tempdir(name);
        let sch = common::spice_to_kicad(&path, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);
        let crossings = count_wire_crossings(&root);
        common::scoreboard::record_count("crossings", name, crossings as usize);
        if std::env::var_os("S2K_QUALITY_DUMP").is_some() {
            println!("crossings (\"{name}\", {crossings}),");
            continue;
        }
        let &(_, budget) = budgets
            .iter()
            .find(|(n, _)| *n == name)
            .expect("budget for fixture");
        if crossings > budget {
            failures.push(format!(
                "{name}: {crossings} wire crossings > budget {budget}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "wire-crossing budget exceeded:\n{}",
        failures.join("\n")
    );
}

#[test]
fn common_emitter_signal_flows_left_to_right() {
    // Regression guard: the canonical signal chain is
    // CIN → Q1 → COUT, so left-to-right placement must respect
    // `CIN.x < Q1.x < COUT.x`. (VIN is `;@ ignore`d in the
    // fixture so it never reaches the placer; the ordering
    // anchor is the BJT's collector cap COUT and the input
    // AC-coupling cap CIN, with Q1 between them.) This is the
    // strong signal-flow assertion the original V6 archetype
    // check encoded; T8's "CIN.x < COUT.x" weakening was
    // unauthorized — restored here now that the wider seed
    // stride keeps Q1 from drifting outside the [CIN, COUT]
    // X interval.
    let sch = emit("common_emitter");
    let root = parse_sch(&sch);
    let cin_x = element_x(&root, "CIN").expect("CIN placed");
    let q1_x = element_x(&root, "Q1").expect("Q1 placed");
    let cout_x = element_x(&root, "COUT").expect("COUT placed");
    assert!(
        cin_x < q1_x && q1_x < cout_x,
        "common_emitter: signal flow not left-to-right: \
         CIN.x={cin_x:.2}, Q1.x={q1_x:.2}, COUT.x={cout_x:.2}",
    );
}

/// V14 (rail-pin facing) — every directional rail glyph must sit on the
/// *correct screen side* of the body it connects to: a positive-rail
/// glyph (`power:VCC` / `power:VDD` / `power:+…`) above its host body
/// (anchor Y ≤ host body-centre Y, screen Y growing downward), and a
/// negative-rail / ground glyph (`power:GND` / `power:VEE`) below it
/// (anchor Y ≥ host body-centre Y).
///
/// This is the R-5 defect: a 2-pin rail consumer (e.g. `RC vcc c`) whose
/// rail pin points *into* the body, dropping its VCC glyph below the
/// resistor. The host is associated to a glyph by the non-power symbol
/// whose pin coincides with the glyph anchor (glyphs carry a single pin
/// at their `(at …)` origin). Budget 0 across all fixtures, ratchet.
///
/// One placed non-power-symbol pin, in world coords, with its host body
/// centre and screen-vertical facing — used to bind each rail glyph to
/// the host pin it connects to.
struct HostPin {
    refdes: String,
    body_cy: f64,
    px: f64,
    py: f64,
    /// World-frame vertical-facing of this pin (matches
    /// `orient::ScreenFacing`): `Some(true)`=up, `Some(false)`=down,
    /// `None`=horizontal. A glyph on a horizontal pin is the
    /// detached-glyph case (no above/below expectation).
    vertical_up: Option<bool>,
}

#[test]
fn v14_rail_pin_faces_rail() {
    let library = load_test_library();
    // Collect-all rather than fail-fast: a hard `assert!` inside the
    // fixture loop aborts at the first offender, which hides how many
    // fixtures are affected and makes an XFAIL entry look broader than
    // it needs to be. `xf` also carries the unexpected-pass tripwire —
    // see `tests/common/xfail.rs`.
    let mut xf = common::xfail::Guard::new("v14_rail_pin_faces_rail");
    for (name, path) in fixtures() {
        let mut violation: Option<String> = None;
        let tmp = tempdir(name);
        let sch = common::spice_to_kicad(&path, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);

        // World pins of every NON-power placed symbol: (refdes, body-centre
        // Y, pin world x, pin world y). Used to bind each glyph to its host.
        let mut host_pins: Vec<HostPin> = Vec::new();
        for sym in children(&root, "symbol") {
            let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
                continue;
            };
            if refdes.starts_with("#PWR") || lib_id.starts_with("power:") {
                continue;
            }
            let Some((ox, oy, orient)) = placed_symbol_pose(sym) else {
                continue;
            };
            let Some(lib_sym) = library.lookup(&lib_id) else {
                continue;
            };
            let Some((_, bbox)) = resolved_body_bbox(&library, sym) else {
                continue;
            };
            let body_cy = f64::midpoint(bbox.y0, bbox.y1);
            for tp in lib_sym.pins_in(orient) {
                // The emitter passes the library-frame pin angle straight
                // through and negates world Y, so library angle 270 renders
                // screen-up and 90 screen-down (see orient::screen_facing).
                let vertical_up = match tp.angle % 360 {
                    270 => Some(true),
                    90 => Some(false),
                    _ => None,
                };
                host_pins.push(HostPin {
                    refdes: refdes.clone(),
                    body_cy,
                    px: ox + tp.x,
                    py: oy - tp.y,
                    vertical_up,
                });
            }
        }

        for sym in children(&root, "symbol") {
            let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
                continue;
            };
            if !lib_id.starts_with("power:") || lib_id == "power:PWR_FLAG" {
                continue;
            }
            // VertPref::Up for positive rails (VCC/VDD/+…); Down for
            // ground / negative rails (GND/VEE).
            let want_up =
                lib_id == "power:VCC" || lib_id == "power:VDD" || lib_id.starts_with("power:+");
            let want_down = lib_id == "power:GND" || lib_id == "power:VEE";
            if !(want_up || want_down) {
                continue; // not a directional rail glyph
            }
            let Some((ax, ay, _)) = placed_symbol_pose(sym) else {
                continue;
            };
            // Host = nearest non-power pin to the glyph anchor (the glyph's
            // single pin sits at its `(at …)` origin and is wired to a host
            // pin; allow a short stub by taking the nearest).
            let Some(host) = host_pins.iter().min_by(|a, b| {
                let da = (a.px - ax).hypot(a.py - ay);
                let db = (b.px - ax).hypot(b.py - ay);
                da.partial_cmp(&db).unwrap()
            }) else {
                continue;
            };
            // …but only if that pin is actually the one the glyph attaches
            // to. V14/R-5 is a statement about a glyph and the host body it
            // sits on: "does the rail pin face out of the body, or back
            // into it?". A glyph attached to nothing has no such
            // relationship to assert.
            //
            // An attached glyph sits either exactly on its host pin, or at
            // most SHEET_EDGE_GLYPH_OFFSET_CELLS (2 cells = 2.54 mm) away
            // down the pin's outward direction — the V14 forced-sideways
            // and sheet-edge stub fallbacks. 3 cells is therefore strictly
            // beyond every legitimate offset, so this cannot exempt a glyph
            // that really is on a host. Nothing is detached today — the
            // PWR_FLAG corner driver block that used to stand 8 cells off
            // the circuit is gone (`spice_route::pwrflag`), and it was
            // never in scope here anyway, since PWR_FLAG is skipped above.
            // The cap stays as the guard it is: a glyph that drifts off
            // its host must not silently acquire a different one.
            if (host.px - ax).hypot(host.py - ay) > MAX_HOST_ATTACH_MM {
                continue;
            }
            // A glyph attached to a *horizontally-drawn* host pin (e.g. an
            // opamp `+` input wired to ground) is the documented
            // detached-glyph-stub case: V14 governs supply-style
            // (native-vertical) pins only, so it carries no above/below
            // expectation. Mirrors `orient::satisfies_v14`'s
            // `native_vertical` gate — assert facing only for vertical
            // host pins.
            if host.vertical_up.is_none() {
                continue;
            }
            if want_up {
                if ay > host.body_cy + f64::EPSILON {
                    violation.get_or_insert_with(|| {
                        format!(
                            "{name}: positive-rail glyph {refdes} ({lib_id}) at y={ay} is BELOW \
                             its host {}'s body centre y={} — rail pin faces into the body \
                             (V14/R-5)",
                            host.refdes, host.body_cy,
                        )
                    });
                }
            } else if ay < host.body_cy - f64::EPSILON {
                violation.get_or_insert_with(|| {
                    format!(
                        "{name}: ground/negative-rail glyph {refdes} ({lib_id}) at y={ay} is \
                         ABOVE its host {}'s body centre y={} — rail pin faces into the body \
                         (V14/R-5)",
                        host.refdes, host.body_cy,
                    )
                });
            }
        }
        common::scoreboard::record_count("v14.rail_pin", name, usize::from(violation.is_some()));
        xf.record(name, violation);
    }
    xf.finish();
}

#[test]
fn v14_power_glyphs_have_canonical_orientation() {
    // V14 — every `power:GND` instance is emitted at rot 0 (triangle
    // points visually down); every `power:VCC` (and the variants
    // `+5V`/`+12V`/`+3V3`/`VDD`) is emitted at rot 0 (chevron points
    // visually up). Per-pin rotation matching the host pin's outward
    // direction is no longer allowed.
    // Collect-then-assert (ADR-23 D2): a mis-rotated glyph must not abort
    // the loop, or every later fixture goes unmeasured.
    let mut failures: Vec<String> = Vec::new();
    for (name, path) in fixtures() {
        let tmp = tempdir(name);
        let sch = common::spice_to_kicad(&path, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);
        let mut wrong = 0usize;
        for sym in children(&root, "symbol") {
            let Some(lib_id) = find_child(sym, "lib_id")
                .and_then(|n| list_iter(n).nth(1))
                .and_then(as_str)
            else {
                continue;
            };
            if !lib_id.starts_with("power:") {
                continue;
            }
            // `power:PWR_FLAG` is a driver MARKER, not a rail glyph: it
            // has no canonical screen direction (no "VCC up / GND
            // down"). It is oriented to point its body in the host
            // pin's outward direction so it clears the host body
            // (V12/V13), so it legitimately carries rot 90/180/270.
            // V14 governs only the directional rail glyphs.
            if lib_id == "power:PWR_FLAG" {
                continue;
            }
            let Some(at) = find_child(sym, "at") else {
                continue;
            };
            let mut it = list_iter(at);
            it.next();
            let _ = it.next(); // x
            let _ = it.next(); // y
            let rotation = it.next().and_then(as_f64).unwrap_or(0.0);
            // V14 is about the direction the glyph POINTS ON THE PAGE,
            // which is the library body direction composed with the
            // rotation — not the rotation alone. `power:VEE` is the one
            // rail glyph the KiCad library draws pointing UP (its arrow
            // runs to local +Y, same as the VCC chevron) while being a
            // NEGATIVE rail that belongs with GND. Emitting it at rot 0
            // therefore pointed it up, into the host body — visible in
            // the `diff_pair` render as VEE's arrowhead inside `RTAIL`.
            // Rot 180 is what makes VEE actually point down; see
            // `rails::glyph_rotation`.
            let want = if lib_id == "power:VEE" { 180.0 } else { 0.0 };
            if (rotation - want).abs() >= f64::EPSILON {
                wrong += 1;
                failures.push(format!(
                    "{name}: power glyph {lib_id} rendered at rot {rotation}, want {want}; \
                     V14 requires GND down / VCC up / VEE (negative rail) down on the page",
                ));
            }
        }
        common::scoreboard::record_count("v14.glyph_orientation", name, wrong);
    }
    assert!(
        failures.is_empty(),
        "V14 power-glyph orientation violations (budget 0):\n  {}",
        failures.join("\n  "),
    );
}

// --- V15: content lands within the page's usable area --------------------

/// Every emitted instance-section coordinate must sit at a positive page
/// margin, with the whole content bbox inside the A4 drawable region. The
/// margin must match the production constant `kicad_emitter::PAGE_MARGIN_MM`
/// — the *floor* the content bbox's top-left corner clears. V15 is
/// `min >= margin`, not `min == margin` (see `docs/invariants.md`).
const V15_MARGIN_MM: f64 = 25.4;

/// A4 drawable extent in millimetres. KiCad's A4 frame is 297×210; we
/// assert the content bbox fits within the page rectangle (a generous
/// upper bound — the point of V15 is the *floor*, but the content must
/// not run off the right/bottom edge either).
const V15_A4_W_MM: f64 = 297.0;
const V15_A4_H_MM: f64 = 210.0;

/// Recursively collect every translatable instance-section coordinate
/// `(at x y …)` / `(xy x y)` under `v`, EXCLUDING:
///   * the entire `(lib_symbols …)` subtree (definition-local geometry),
///   * any `(property … (hide yes))` node's `(at …)` (hidden sim props
///     are emitted at a fixed `(0 0 0)` and are not visible content).
///
/// Mirrors the production translator's notion of "what is content".
fn collect_instance_coords(v: &Value, out: &mut Vec<Pt>) {
    let Some(name) = head(v) else {
        // Not a list with a head symbol; nothing to collect here.
        if let Some(it) = v.list_iter() {
            for child in it {
                collect_instance_coords(child, out);
            }
        }
        return;
    };

    // Never descend into symbol-definition-local geometry.
    if name == "lib_symbols" {
        return;
    }

    // A hidden property's `(at …)` is not content — skip the whole node.
    if name == "property" && property_is_hidden(v) {
        return;
    }

    if name == "at" || name == "xy" {
        let mut it = list_iter(v);
        it.next(); // head
        if let (Some(x), Some(y)) = (it.next().and_then(as_f64), it.next().and_then(as_f64)) {
            out.push((x, y));
        }
        // `at`/`xy` carry only scalars after head; no nested coords.
        return;
    }

    for child in list_iter(v) {
        collect_instance_coords(child, out);
    }
}

/// True when a `(property …)` node carries `(effects … (hide yes))`.
fn property_is_hidden(prop: &Value) -> bool {
    let Some(effects) = find_child(prop, "effects") else {
        return false;
    };
    children(effects, "hide")
        .into_iter()
        .any(|h| list_iter(h).nth(1).and_then(as_str) == Some("yes"))
}

/// Recursively collect the `(at …)` anchor of every HIDDEN instance-section
/// `(property …)` node under `v`, EXCLUDING the `(lib_symbols …)` subtree
/// (whose `(at …)` are symbol-definition-local geometry, not page
/// coordinates). Unlike [`collect_instance_coords`], which drops hidden
/// props entirely, this returns precisely those anchors — so the verifier
/// can assert that a hidden property (e.g. a power glyph's `#PWRn`
/// Reference) is translated into the page alongside its symbol, not left
/// behind at its pre-translation (often negative) coordinate.
fn collect_hidden_instance_prop_coords(v: &Value, out: &mut Vec<Pt>) {
    let Some(name) = head(v) else {
        if let Some(it) = v.list_iter() {
            for child in it {
                collect_hidden_instance_prop_coords(child, out);
            }
        }
        return;
    };

    // Definition-local geometry never carries page coordinates.
    if name == "lib_symbols" {
        return;
    }

    if name == "property" && property_is_hidden(v) {
        if let Some(at) = find_child(v, "at") {
            let mut it = list_iter(at);
            it.next(); // head
            if let (Some(x), Some(y)) = (it.next().and_then(as_f64), it.next().and_then(as_f64)) {
                out.push((x, y));
            }
        }
        // A property has no nested instance-section coords to recurse into.
        return;
    }

    for child in list_iter(v) {
        collect_hidden_instance_prop_coords(child, out);
    }
}

/// Every V15 fixture: the five v0.1 reference fixtures, the
/// hierarchical-sheet opamp (`opamp_inverting`, which exercises sheet
/// blocks + hierarchical labels + no_connect anchors), and the
/// repo-level example.
fn v15_fixtures() -> Vec<(&'static str, PathBuf)> {
    let mut out: Vec<(&'static str, PathBuf)> = vec![
        ("rc_lowpass", fixtures_dir().join("rc_lowpass.cir")),
        ("common_emitter", fixtures_dir().join("common_emitter.cir")),
        ("multivibrator", fixtures_dir().join("multivibrator.cir")),
        ("diff_pair", fixtures_dir().join("diff_pair.cir")),
        (
            "opamp_inverting_real",
            fixtures_dir().join("opamp_inverting_real.cir"),
        ),
        (
            "opamp_inverting",
            fixtures_dir().join("opamp_inverting.cir"),
        ),
    ];
    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/rc_lowpass.cir");
    out.push(("example_rc_lowpass", example));
    out
}

// --- V6: hierarchical sheets are placeable units -------------------------
//
// A default-path `.subckt` instance becomes a KiCad `(sheet …)` block. It
// must be positioned by the structural placer (classify→bands→layers),
// landing adjacent to the symbols it shares nets with — NOT at a fixed
// off-circuit page coordinate that forces ~180 mm trunk wires.
//
// The verifier is fully general: it derives the circuit bbox from the
// emitted top-level `(symbol …)` `(at …)` coordinates and asserts every
// `(sheet …)` `(at …)` lands within that bbox expanded by a small margin.
// No fixture name or magic coordinate is hardcoded. The sheet-port
// trunk-wire budget is a recorded high-water mark (ratchet), driven down,
// never up.

/// `(at x y …)` of every top-level `(symbol …)` instance (the placed
/// circuit components). Excludes `(lib_symbols …)` definition geometry.
fn symbol_instance_origins(root: &Value) -> Vec<Pt> {
    let mut out = Vec::new();
    for sym in children(root, "symbol") {
        if let Some(at) = find_child(sym, "at") {
            let mut it = list_iter(at);
            it.next(); // head
            if let (Some(x), Some(y)) = (it.next().and_then(as_f64), it.next().and_then(as_f64)) {
                out.push((x, y));
            }
        }
    }
    out
}

/// `(refdes, (at x y))` of every top-level `(sheet …)` block. The refdes
/// is read from the `Sheetname` property the emitter stamps with the
/// SPICE instance designator.
fn sheet_origins(root: &Value) -> Vec<(String, Pt)> {
    let mut out = Vec::new();
    for sheet in children(root, "sheet") {
        let Some(at) = find_child(sheet, "at") else {
            continue;
        };
        let mut it = list_iter(at);
        it.next(); // head
        let (Some(x), Some(y)) = (it.next().and_then(as_f64), it.next().and_then(as_f64)) else {
            continue;
        };
        let mut refdes = String::from("?");
        for prop in children(sheet, "property") {
            let mut pit = list_iter(prop);
            pit.next(); // head "property"
            if pit.next().and_then(as_str) == Some("Sheetname") {
                if let Some(v) = pit.next().and_then(as_str) {
                    refdes = v.to_string();
                }
            }
        }
        out.push((refdes, (x, y)));
    }
    out
}

/// Per-fixture longest sheet-port trunk-wire budget (mm). RATCHET —
/// recorded high-water mark from the post-fix run; only ever lowered.
/// Before the structural-sheet fix `opamp_inverting`'s longest sheet
/// trunk wire was ~182 mm (sheet pinned at x=200 mm, circuit near the
/// origin). After it the sheet lands adjacent to the circuit.
const SHEET_TRUNK_WIRE_BUDGET_MM: &[(&str, f64)] = &[("opamp_inverting", 60.0)];

/// Slack (mm) around the circuit bbox within which a sheet `(at …)` must
/// land to count as "near the circuit". A sheet is a ~30 mm box; one
/// symbol-pitch of slack lets a sheet abutting the circuit still pass,
/// while a sheet flung to x≈200 mm fails by a wide margin.
const SHEET_NEAR_MARGIN_MM: f64 = 40.0;

/// Longest single `(wire …)` segment length (Manhattan) on the schematic.
/// Sheet-port trunk wires are by far the longest segments when a sheet is
/// flung across the page, so the global max is a faithful proxy.
fn longest_wire_segment(root: &Value) -> f64 {
    wire_segments(root)
        .into_iter()
        .map(|(a, b)| manhattan(a, b))
        .fold(0.0_f64, f64::max)
}

#[test]
fn hierarchical_sheet_placed_near_circuit() {
    // Fixtures that emit a default-path `(sheet …)` block.
    let cases: &[(&str, PathBuf)] = &[(
        "opamp_inverting",
        fixtures_dir().join("opamp_inverting.cir"),
    )];

    for (name, path) in cases {
        let tmp = tempdir(name);
        let sch = spice_to_kicad(path, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);

        let symbols = symbol_instance_origins(&root);
        assert!(
            !symbols.is_empty(),
            "{name}: no top-level symbols emitted; cannot derive circuit bbox",
        );
        let sheets = sheet_origins(&root);
        assert!(
            !sheets.is_empty(),
            "{name}: expected at least one (sheet …) block",
        );

        // Circuit bounding box from the placed symbol origins.
        let min_x = symbols.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
        let min_y = symbols.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
        let max_x = symbols
            .iter()
            .map(|c| c.0)
            .fold(f64::NEG_INFINITY, f64::max);
        let max_y = symbols
            .iter()
            .map(|c| c.1)
            .fold(f64::NEG_INFINITY, f64::max);

        // A sheet is a ~30 mm box; allow one symbol-pitch of slack around
        // the circuit bbox so a sheet abutting the circuit still counts as
        // "near". This is geometry-derived, not a magic coordinate: a
        // sheet flung to x=200 mm with the circuit near the origin fails
        // by a wide margin.
        for (refdes, (sx, sy)) in &sheets {
            assert!(
                *sx >= min_x - SHEET_NEAR_MARGIN_MM
                    && *sx <= max_x + SHEET_NEAR_MARGIN_MM
                    && *sy >= min_y - SHEET_NEAR_MARGIN_MM
                    && *sy <= max_y + SHEET_NEAR_MARGIN_MM,
                "{name}: sheet {refdes} at ({sx:.2}, {sy:.2}) is outside the \
                 circuit bbox [{min_x:.2}..{max_x:.2}] x [{min_y:.2}..{max_y:.2}] \
                 expanded by {SHEET_NEAR_MARGIN_MM} mm — sheet flung off the circuit",
            );
        }

        // Sheet-port trunk-wire budget (ratchet).
        if let Some(&(_, budget)) = SHEET_TRUNK_WIRE_BUDGET_MM.iter().find(|(n, _)| n == name) {
            let longest = longest_wire_segment(&root);
            assert!(
                longest <= budget + 1e-6,
                "{name}: longest wire segment {longest:.2} mm > budget {budget:.2} mm \
                 (ratchet high-water mark) — sheet trunk wire regressed",
            );
        }
    }
}

// --- V6 / V12 / V13: sheets participate in no-overlap --------------------
//
// A hierarchical `(sheet …)` block is a first-class drawable rectangle on
// the parent sheet. Two defects motivate these verifiers:
//   1. A neighbouring symbol's resolved extent (body + pin reach) must not
//      overlap the sheet body bbox — the sheet is an obstacle the placer
//      must clear (mirrors `no_symbol_symbol_overlap_across_fixtures`).
//   2. A `power:*` glyph emitted on a sheet *port pin* must not land on the
//      sheet body / port label: KiCad draws the sheet's port label at the
//      port-pin coordinate, and a glyph anchored there overprints it. The
//      documented fix is the detached-glyph-with-stub-wire offset (the
//      glyph hangs outside the sheet edge, connected by a short stub).
//
// Fully general: no fixture name or magic coordinate is hardcoded; the
// sheet body bbox and port-pin coordinates are read from the emitted file.

/// Sheet body bbox `(x0,y0,x1,y1)` from a `(sheet (at x y) (size w h) …)`.
fn sheet_body_bbox(sheet: &Value) -> Option<Bbox> {
    let at = find_child(sheet, "at")?;
    let mut ait = list_iter(at);
    ait.next();
    let x = as_f64(ait.next()?)?;
    let y = as_f64(ait.next()?)?;
    let size = find_child(sheet, "size")?;
    let mut sit = list_iter(size);
    sit.next();
    let w = as_f64(sit.next()?)?;
    let h = as_f64(sit.next()?)?;
    Some(Bbox {
        x0: x,
        y0: y,
        x1: x + w,
        y1: y + h,
    })
}

/// Every `(sheet …)` block's body bbox on the parent sheet.
fn sheet_bboxes(root: &Value) -> Vec<Bbox> {
    children(root, "sheet")
        .into_iter()
        .filter_map(sheet_body_bbox)
        .collect()
}

/// Every `(pin "name" … (at x y rot))` of a `(sheet …)` block, as
/// `(name, x, y)`. These are the sheet's port pins; KiCad renders the
/// port label at this coordinate.
fn sheet_port_pins(sheet: &Value) -> Vec<(String, f64, f64)> {
    let mut out = Vec::new();
    for pin in children(sheet, "pin") {
        let mut it = list_iter(pin);
        it.next(); // head "pin"
        let Some(name) = it.next().and_then(as_str) else {
            continue;
        };
        let Some(at) = find_child(pin, "at") else {
            continue;
        };
        let mut ait = list_iter(at);
        ait.next();
        let (Some(x), Some(y)) = (ait.next().and_then(as_f64), ait.next().and_then(as_f64)) else {
            continue;
        };
        out.push((name.to_string(), x, y));
    }
    out
}

/// Resolved world extent (body ∪ pin reach) of a placed `power:*` glyph
/// instance, plus its refdes. The glyph's body bbox is taken from its
/// inlined library symbol, orientation-transformed exactly like
/// [`resolved_world_extent`].
fn glyph_world_extent(library: &Library, sym: &Value) -> Option<(String, Bbox)> {
    let (refdes, lib_id) = placed_symbol_refdes_and_lib_id(sym)?;
    if !lib_id.starts_with("power:") {
        return None;
    }
    let (ox, oy, orient) = placed_symbol_pose(sym)?;
    let lib_sym = library.lookup(&lib_id)?;
    let mut x0 = f64::INFINITY;
    let mut y0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    let mut grow = |wx: f64, wy: f64| {
        x0 = x0.min(wx);
        y0 = y0.min(wy);
        x1 = x1.max(wx);
        y1 = y1.max(wy);
    };
    if let Some(local) = lib_sym.body_bbox() {
        for (lx, ly) in [
            (local.x0, local.y0),
            (local.x0, local.y1),
            (local.x1, local.y0),
            (local.x1, local.y1),
        ] {
            let (rx, ry) = orient.apply_point(lx, ly);
            grow(ox + rx, oy - ry);
        }
    }
    for tp in lib_sym.pins_in(orient) {
        grow(ox + tp.x, oy - tp.y);
    }
    if x0.is_finite() && x1.is_finite() && y0.is_finite() && y1.is_finite() {
        Some((refdes, Bbox { x0, y0, x1, y1 }))
    } else {
        None
    }
}

/// World-frame AABB of a placed `power:*` glyph's *body* only (no pin
/// reach), orientation-transformed. Mirrors [`resolved_body_bbox`] but
/// for glyphs (which `resolved_body_bbox` filters out): the glyph's
/// single pin sits at its `(at …)` origin coincident with the host
/// pin, so including pin reach would always touch the host — we measure
/// the drawn triangle/chevron body, which is what a reader perceives as
/// overlapping a foreign part.
fn glyph_body_bbox(library: &Library, sym: &Value) -> Option<(String, Bbox)> {
    let (refdes, lib_id) = placed_symbol_refdes_and_lib_id(sym)?;
    if !lib_id.starts_with("power:") {
        return None;
    }
    let (ox, oy, orient) = placed_symbol_pose(sym)?;
    let lib_sym = library.lookup(&lib_id)?;
    let local = lib_sym.body_bbox()?;
    let mut x0 = f64::INFINITY;
    let mut y0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for (lx, ly) in [
        (local.x0, local.y0),
        (local.x0, local.y1),
        (local.x1, local.y0),
        (local.x1, local.y1),
    ] {
        let (rx, ry) = orient.apply_point(lx, ly);
        x0 = x0.min(ox + rx);
        y0 = y0.min(oy - ry);
        x1 = x1.max(ox + rx);
        y1 = y1.max(oy - ry);
    }
    Some((refdes, Bbox { x0, y0, x1, y1 }))
}

/// Per-fixture budget for the V14 power-glyph / foreign-body overlap:
/// **0 everywhere**, no exceptions.
///
/// This function used to carry a single exception,
/// `"opamp_inverting_real" => 1`, for the long-deferred V14 "[3]"
/// residual — a `power:GND` glyph on the opamp's grounded `+` input pin
/// clipping the FEEDBACK resistor `RF`. (Before that it named `#FLG3`,
/// a `PWR_FLAG` clipping `RIN`, which had already gone stale; see ADR-17
/// § "Correction — the recorded V14 residual is stale".)
///
/// It is gone. ADR-14 predicted it was a **placement** defect, not a
/// glyph-orientation one, and that is what it turned out to be: `X1` was
/// seeded at layer 0 level with `RIN` (`layers.rs` rooted any
/// power-touching element there), which put its grounded `+` pin on top
/// of `RF`. With the layer-root refinement the glyph clears `RF` by
/// construction. No glyph rotation, no V3 exception, no decoration
/// change was needed — exactly ADR-14's call.
fn power_glyph_foreign_body_overlap_budget(_fixture: &str) -> usize {
    0
}

/// Tier-1 readability containment: a `power:*` glyph body (including the
/// `power:PWR_FLAG` driver marker) must not overlap the body bbox of any
/// NON-host real symbol. The host — the symbol the glyph attaches to —
/// is excluded: a glyph clipping its own host is the documented, accepted
/// V14 case (CLAUDE.md V14: "the glyph body may visually overlap the host
/// symbol's body … V14's contract is purely 'no surprising rotations'").
/// The host is bound by the nearest same-net (here: nearest, period —
/// glyphs sit on a single host pin) non-power pin to the glyph anchor,
/// exactly as `v14_rail_pin_faces_rail` binds glyphs to hosts.
///
/// This records the current residual as a zero-slack per-fixture ratchet
/// (issue [3], deferred V14 placer item) so it can never get worse; a
/// future placement redesign drives every budget to 0.
/// A power glyph sits on its host pin, or at most
/// `SHEET_EDGE_GLYPH_OFFSET_CELLS` (2 cells) down the pin's outward
/// direction for the V14 forced-sideways and sheet-edge stub fallbacks.
/// Three cells is strictly beyond every legitimate offset, so this
/// threshold cannot exempt a glyph that really is attached to a host.
const MAX_HOST_ATTACH_MM: f64 = 3.0 * 1.27;

#[test]
fn no_power_glyph_foreign_body_overlap_across_fixtures() {
    let library = load_test_library();
    // Collect-all + XFAIL registry — see `tests/common/xfail.rs`.
    let mut xf = common::xfail::Guard::new("no_power_glyph_foreign_body_overlap_across_fixtures");
    for (name, path) in fixtures() {
        let tmp = tempdir(name);
        let sch = common::spice_to_kicad(&path, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);

        // Body bbox of every NON-power real symbol, plus its pins (world
        // coords) for host binding.
        let real_bodies: Vec<(String, Bbox)> = children(&root, "symbol")
            .into_iter()
            .filter_map(|sym| resolved_body_bbox(&library, sym))
            .collect();
        let mut host_pins: Vec<(String, f64, f64)> = Vec::new();
        for sym in children(&root, "symbol") {
            let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
                continue;
            };
            if refdes.starts_with("#PWR") || lib_id.starts_with("power:") {
                continue;
            }
            let Some((ox, oy, orient)) = placed_symbol_pose(sym) else {
                continue;
            };
            let Some(lib_sym) = library.lookup(&lib_id) else {
                continue;
            };
            for tp in lib_sym.pins_in(orient) {
                host_pins.push((refdes.clone(), ox + tp.x, oy - tp.y));
            }
        }

        let mut overlaps = 0usize;
        let mut detail: Vec<String> = Vec::new();
        for sym in children(&root, "symbol") {
            let Some((glyph_ref, glyph_box)) = glyph_body_bbox(&library, sym) else {
                continue;
            };
            let Some((ax, ay, _)) = placed_symbol_pose(sym) else {
                continue;
            };
            // Host = nearest non-power pin to the glyph anchor (the glyph
            // wires to one host pin; allow a short stub by taking nearest).
            let host_refdes = host_pins
                .iter()
                .min_by(|a, b| {
                    let da = (a.1 - ax).hypot(a.2 - ay);
                    let db = (b.1 - ax).hypot(b.2 - ay);
                    da.partial_cmp(&db).unwrap()
                })
                .map(|(r, _, _)| r.clone());
            for (real_ref, real_box) in &real_bodies {
                if Some(real_ref) == host_refdes.as_ref() {
                    continue; // host clip is the accepted V14 case
                }
                if glyph_box.intersects(real_box) {
                    overlaps += 1;
                    detail.push(format!(
                        "{glyph_ref} {glyph_box:?} overlaps foreign body {real_ref} {real_box:?} \
                         (host={host_refdes:?})"
                    ));
                }
            }
        }

        // The budget stays a hard 0 for EVERY fixture; a fixture that
        // re-exposes the deferred issue-[3] defect is excluded by name in
        // `tests/common/xfail.rs`, which fails the test the day that
        // fixture starts passing.
        common::scoreboard::record_count("v14.glyph_body", name, overlaps);
        let budget = power_glyph_foreign_body_overlap_budget(name);
        xf.record(
            name,
            (overlaps > budget).then(|| {
                format!(
                    "{name}: {overlaps} power-glyph/foreign-body overlaps exceed ratchet budget \
                     {budget} (issue [3], deferred V14 placer item — budgets only ratchet \
                     DOWN):\n  {}",
                    detail.join("\n  "),
                )
            }),
        );
    }
    xf.finish();
}

/// No placed symbol's resolved extent (body + pin reach) and no power
/// glyph's body may overlap a `(sheet …)` body bbox. Budget 0, ratchet
/// (CLAUDE.md V6 no-overlap clause extended to sheets — Tier-1
/// readability). Sheets that emit no `(sheet …)` block are a no-op.
#[test]
fn no_symbol_sheet_overlap_across_fixtures() {
    let library = load_test_library();
    let cases: &[(&str, PathBuf)] = &[(
        "opamp_inverting",
        fixtures_dir().join("opamp_inverting.cir"),
    )];
    // Collect-then-assert (ADR-23 D2).
    let mut failures: Vec<String> = Vec::new();
    for (name, path) in cases {
        let tmp = tempdir(name);
        let sch = common::spice_to_kicad(path, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);
        let sheets = sheet_bboxes(&root);
        if sheets.is_empty() {
            failures.push(format!("{name}: expected at least one (sheet …)"));
            common::scoreboard::record_count("t0.sheet_overlap", name, 0);
            continue;
        }

        // Real placed symbols.
        let sym_boxes: Vec<(String, Bbox)> = children(&root, "symbol")
            .into_iter()
            .filter_map(|sym| resolved_world_extent(&library, sym))
            .collect();
        // Power glyphs.
        let glyph_boxes: Vec<(String, Bbox)> = children(&root, "symbol")
            .into_iter()
            .filter_map(|sym| glyph_world_extent(&library, sym))
            .collect();

        let mut overlaps = 0usize;
        for (i, sheet) in sheets.iter().enumerate() {
            for (refdes, b) in sym_boxes.iter().chain(glyph_boxes.iter()) {
                if b.intersects(sheet) {
                    overlaps += 1;
                    failures.push(format!(
                        "{name}: {refdes} extent {b:?} overlaps sheet #{i} body {sheet:?}",
                    ));
                }
            }
        }
        common::scoreboard::record_count("t0.sheet_overlap", name, overlaps);
    }
    assert!(
        failures.is_empty(),
        "symbol/sheet body overlaps (budget 0):\n  {}",
        failures.join("\n  "),
    );
}

/// A `power:*` glyph anchored on a sheet *port pin* overprints the port
/// label KiCad draws at that coordinate. The fix offsets the glyph
/// outward (detached-glyph-with-stub-wire); after it, no glyph anchor
/// coincides with a sheet port pin. Budget 0, ratchet.
#[test]
fn power_glyph_not_on_sheet_port_pin() {
    let cases: &[(&str, PathBuf)] = &[(
        "opamp_inverting",
        fixtures_dir().join("opamp_inverting.cir"),
    )];
    // Collect-then-assert (ADR-23 D2).
    let mut failures: Vec<String> = Vec::new();
    for (name, path) in cases {
        let tmp = tempdir(name);
        let sch = common::spice_to_kicad(path, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);

        // All sheet port-pin coordinates on the parent sheet.
        let mut port_pins: Vec<(String, f64, f64)> = Vec::new();
        for sheet in children(&root, "sheet") {
            port_pins.extend(sheet_port_pins(sheet));
        }
        if port_pins.is_empty() {
            failures.push(format!("{name}: no sheet port pins found"));
            common::scoreboard::record_count("v13.glyph_on_sheet_port", name, 0);
            continue;
        }

        // Power-glyph anchor coordinates.
        let mut hits = 0usize;
        for sym in children(&root, "symbol") {
            let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
                continue;
            };
            if !lib_id.starts_with("power:") {
                continue;
            }
            let Some((gx, gy, _)) = placed_symbol_pose(sym) else {
                continue;
            };
            for (pname, px, py) in &port_pins {
                let coincident = (gx - px).abs() < 1e-3 && (gy - py).abs() < 1e-3;
                if coincident {
                    hits += 1;
                    failures.push(format!(
                        "{name}: power glyph {refdes} ({lib_id}) at ({gx:.2},{gy:.2}) \
                         sits exactly on sheet port pin '{pname}' — overprints the \
                         port label (use detached-glyph-with-stub-wire offset)",
                    ));
                }
            }
        }
        common::scoreboard::record_count("v13.glyph_on_sheet_port", name, hits);
    }
    assert!(
        failures.is_empty(),
        "power glyphs on sheet port pins (budget 0):\n  {}",
        failures.join("\n  "),
    );
}

/// Every V15 violation on one emitted sheet file, as messages.
///
/// Extracted from the verifier so the fixture loop can be
/// collect-then-assert (ADR-23 D2): a spilled fixture must not abort
/// the loop, or every later fixture goes unmeasured and its scoreboard
/// cell reads as "nothing to say".
#[allow(clippy::too_many_lines)] // one cohesive check per sheet file
fn v15_violations(name: &str, file: &std::path::Path) -> Vec<String> {
    let mut violations: Vec<String> = Vec::new();
    let root = parse_sch(file);
    let mut coords = Vec::new();
    collect_instance_coords(&root, &mut coords);
    if coords.is_empty() {
        violations.push(format!(
            "{name} ({}): no instance-section coordinates collected",
            file.display()
        ));
        return violations;
    }
    let min_x = coords.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
    let min_y = coords.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
    let max_x = coords.iter().map(|c| c.0).fold(f64::NEG_INFINITY, f64::max);
    let max_y = coords.iter().map(|c| c.1).fold(f64::NEG_INFINITY, f64::max);

    // Floor: content top-left corner sits at the page margin.
    // No coordinate may be left of / above the margin (this is
    // what catches the negative-X spill the fix removes).
    if min_x < V15_MARGIN_MM - 1e-6 {
        violations.push(format!(
            "{name} ({}): min_x = {min_x:.3} < margin {V15_MARGIN_MM}; \
             content spills off the left page border",
            file.display()
        ));
    }
    if min_y < V15_MARGIN_MM - 1e-6 {
        violations.push(format!(
            "{name} ({}): min_y = {min_y:.3} < margin {V15_MARGIN_MM}; \
             content sits above the top page margin",
            file.display()
        ));
    }
    // The content sits at or beyond the margin, and inside the
    // page — NOT exactly *on* the margin.
    //
    // This assertion used to demand `min == margin` (±1 cell).
    // That was over-specified relative to V15's own definition,
    // which `docs/invariants.md` now states explicitly: the
    // invariant is `min >= margin`, and normalising the content
    // bbox onto the margin is merely the simplest way to satisfy
    // it, not the requirement. Two production behaviours
    // legitimately leave the content further inside the page:
    // the sticky page shift replayed from the layout cache
    // (position stability, ADR-4) and the symmetric property-text
    // reserve in `fold_symbol_instance`. Both only ever move
    // content *away* from the page edge, so the floor asserted
    // above is what carries the invariant; here we only bound the
    // content to the page it must live on.
    if min_x > V15_A4_W_MM + 1e-6 || min_y > V15_A4_H_MM + 1e-6 {
        violations.push(format!(
            "{name} ({}): content origin ({min_x:.3}, {min_y:.3}) is \
             not on the A4 page",
            file.display()
        ));
    }
    // Ceiling: content fits inside the A4 drawable rectangle.
    if max_x > V15_A4_W_MM + 1e-6 {
        violations.push(format!(
            "{name} ({}): max_x = {max_x:.3} exceeds A4 width {V15_A4_W_MM}",
            file.display()
        ));
    }
    if max_y > V15_A4_H_MM + 1e-6 {
        violations.push(format!(
            "{name} ({}): max_y = {max_y:.3} exceeds A4 height {V15_A4_H_MM}",
            file.display()
        ));
    }

    // Hidden instance-section property anchors (e.g. a power
    // glyph's `#PWRn` Reference) carry real page coordinates and
    // must ride the same uniform V15 translation as their symbol —
    // they must not be left at their pre-translation (negative)
    // coordinate. They do NOT vote on the content min above (a
    // hidden prop parked at (0 0 0) must not drag the bbox toward
    // the origin), but every one that *does* carry a coordinate
    // must still land on the page.
    //
    // The bound here is non-negative + in-page, not `>= margin`: a
    // co-located prop (a Reference emitted glyph-relative at
    // `y - 1.27`) can legitimately sit up to one symbol's extent
    // above/left of its glyph, just as a Reference label sits
    // outside a symbol body. The bug this catches is the anchor
    // stranded at its *pre-translation* coordinate (e.g. `#PWRn`
    // at `x = -2.54`), which goes strongly negative — `>= 0` (with
    // a one-cell tolerance for a glyph parked exactly at the
    // margin) isolates it precisely.
    let mut hidden = Vec::new();
    collect_hidden_instance_prop_coords(&root, &mut hidden);
    for (hx, hy) in &hidden {
        // `(0, 0)` is KiCad's "unplaced placeholder" anchor
        // (Sim/Footprint/Datasheet instance props); it is left
        // untranslated by design and carries no page coordinate.
        if *hx == 0.0 && *hy == 0.0 {
            continue;
        }
        if *hx < -1.27 - 1e-6 || *hy < -1.27 - 1e-6 {
            violations.push(format!(
                "{name} ({}): hidden instance property anchor \
                 ({hx:.3}, {hy:.3}) is negative — it was stranded at \
                 its pre-translation coordinate instead of riding the \
                 V15 translation with its symbol",
                file.display()
            ));
        }
        if *hx > V15_A4_W_MM + 1e-6 || *hy > V15_A4_H_MM + 1e-6 {
            violations.push(format!(
                "{name} ({}): hidden instance property anchor \
                 ({hx:.3}, {hy:.3}) lies outside the A4 page",
                file.display()
            ));
        }
    }
    violations
}

#[test]
fn v15_content_within_page_bounds() {
    let mut failures: Vec<String> = Vec::new();
    for (name, path) in v15_fixtures() {
        let tmp = tempdir(name);
        let sch = common::spice_to_kicad(&path, &tmp).expect("spice2kicad");
        // Translate the root sheet AND every child sheet emitted into
        // the directory: hierarchical fixtures write extra `.kicad_sch`
        // files whose coordinates must also land in-page.
        let dir = sch.parent().expect("sch parent");
        let mut sheet_files: Vec<PathBuf> = std::fs::read_dir(dir)
            .expect("read out dir")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "kicad_sch"))
            .collect();
        sheet_files.sort();
        if sheet_files.is_empty() {
            failures.push(format!("{name}: no .kicad_sch emitted"));
            common::scoreboard::record_count("v15.off_page", name, 0);
            continue;
        }

        let mut hits = 0usize;
        for file in &sheet_files {
            let v = v15_violations(name, file);
            hits += v.len();
            failures.extend(v);
        }
        common::scoreboard::record_count("v15.off_page", name, hits);
    }
    assert!(
        failures.is_empty(),
        "V15 page-bounds violations (budget 0):\n  {}",
        failures.join("\n  "),
    );
}

// ---------------------------------------------------------------------
// PWR_FLAG attachment (V10 / V15)
// ---------------------------------------------------------------------

/// A `PWR_FLAG` is a *driver marker*, not a component: it adds no node
/// to the netlist, so it must add no new place on the page either. Every
/// flag has to sit on geometry the circuit already draws.
///
/// Two claims, both hard floors — there is no legitimate quantity of
/// orphan markers, so there is no budget to tune:
///
/// 1. **Coincidence** — the flag's anchor lands exactly on another
///    `power:*` glyph's anchor (the rail path) or on a host symbol's /
///    sheet's pin (the sheet-local signal-net path). This is also what
///    keeps it electrically attached with no wire, which is the only
///    reason it may be moved at all.
///
/// 2. **Proximity to the circuit** — that anchor is within
///    [`MAX_HOST_ATTACH_MM`] of a *real* pin: a host symbol pin or a
///    sheet port pin. Three grid cells is strictly beyond every
///    legitimate attachment offset (the largest is the 2-cell sheet-edge
///    stub) and strictly inside any parking spot.
///
/// **Why claim 2 and not "inside the content bbox".** The defect this
/// test was written for is the *corner driver block*: between `3286946`
/// and this test, each rail's flag was parked eight grid cells outward
/// of the content bbox, paired with a `power:*` glyph synthesised for it
/// there. A bbox-containment check could not have caught it, and neither
/// could claim 1 alone — the block's flag sat exactly on its companion
/// glyph's anchor, and that glyph's own body *extended the drawn bbox to
/// include it*. What was actually wrong is that the pair as a whole hung
/// on nothing: no pin of the circuit was anywhere near. That is the
/// property measured here.
#[test]
fn pwr_flags_sit_on_existing_drawn_geometry() {
    let library = load_test_library();
    let mut failures: Vec<String> = Vec::new();
    for (name, path) in fixtures() {
        let tmp = tempdir(name);
        let sch = common::spice_to_kicad(&path, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);

        // `real_pins` — pins of things that are actually part of the
        // circuit. `anchors` additionally carries rail-glyph anchors,
        // which are derived geometry (a glyph pin exists only because a
        // host pin does), so they satisfy claim 1 but never claim 2.
        let mut real_pins: Vec<(f64, f64)> = Vec::new();
        let mut anchors: Vec<(f64, f64)> = Vec::new();
        let mut flags: Vec<(String, f64, f64)> = Vec::new();
        for sym in children(&root, "symbol") {
            let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
                continue;
            };
            let Some((ox, oy, orient)) = placed_symbol_pose(sym) else {
                continue;
            };
            if lib_id == "power:PWR_FLAG" {
                flags.push((refdes, ox, oy));
            } else if lib_id.starts_with("power:") {
                anchors.push((ox, oy));
            } else if let Some(lib_sym) = library.lookup(&lib_id) {
                for p in lib_sym.pins_in(orient) {
                    real_pins.push((ox + p.x, oy - p.y));
                }
            }
        }
        // A hierarchical sheet's port pins are the circuit's pins too —
        // a rail glyph (with its flag) legitimately hangs on one, via
        // the 2-cell sheet-edge stub.
        for sheet in children(&root, "sheet") {
            for (_, px, py) in sheet_port_pins(sheet) {
                real_pins.push((px, py));
            }
        }
        anchors.extend(real_pins.iter().copied());

        let mut orphans = 0usize;
        for (refdes, fx, fy) in &flags {
            // 1 µm: coordinates are grid-snapped and written at 2 dp, so
            // an intended coincidence is exact and only float noise is
            // absorbed.
            let coincident = anchors
                .iter()
                .any(|(ax, ay)| (ax - fx).abs() < 1e-3 && (ay - fy).abs() < 1e-3);
            let near_circuit = real_pins
                .iter()
                .any(|(px, py)| (px - fx).hypot(py - fy) <= MAX_HOST_ATTACH_MM + 1e-3);
            if !coincident {
                orphans += 1;
                failures.push(format!(
                    "{name}: {refdes} at ({fx:.2}, {fy:.2}) coincides with no drawn glyph \
                     anchor or symbol pin — it is not attached to anything a reader can see"
                ));
            }
            if !near_circuit {
                orphans += 1;
                failures.push(format!(
                    "{name}: {refdes} at ({fx:.2}, {fy:.2}) is more than {MAX_HOST_ATTACH_MM} mm \
                     from ANY circuit pin — a driver marker parked in dead space"
                ));
            }
        }
        common::scoreboard::record_count("v10.orphan_pwrflag", name, orphans);
    }
    assert!(
        failures.is_empty(),
        "orphan PWR_FLAG markers (floor 0, not a budget):\n  {}",
        failures.join("\n  "),
    );
}
