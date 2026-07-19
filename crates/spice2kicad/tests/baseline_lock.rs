//! Baseline lock: snapshots every fixture's `(symbol …)` instances as
//! `(refdes, lib_id, x, y, rot, mirror)` tuples. Used as a safety net
//! for surgical layout changes: any unintended movement in any element
//! of any fixture trips the assertion. (V14 note: for `Device:R_US`
//! with the power net on terminal 0, rot 0 places the VCC pin
//! screen-up — the V14-correct orientation, as `common_emitter`'s `RC`
//! and the diff_pair / multivibrator collector resistors all show.)
//!
//! To intentionally update a single line, edit the BASELINE entry
//! below — do **not** widen the comparison or skip elements.

// Pedantic lints relaxed for this S-expression-parsing test harness:
// `car`/`cdr` and `s`/`x` are the conventional cons-cell names;
// `as_str`'s two `Some(s)` arms are intentionally distinct match
// patterns; the final `if !empty { panic! }` reads clearer than a
// formatted `assert!`.
#![allow(clippy::similar_names, clippy::match_same_arms, clippy::manual_assert)]

mod common;

use std::path::PathBuf;

use common::spice_to_kicad;
use lexpr::Value;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spice2kicad-baseline-{pid}-{seq}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn list_iter(v: &Value) -> impl Iterator<Item = &Value> {
    let mut cur = v;
    std::iter::from_fn(move || match cur {
        Value::Cons(c) => {
            let (car, cdr) = c.as_pair();
            cur = cdr;
            Some(car)
        }
        _ => None,
    })
}

fn first_atom(v: &Value) -> Option<&str> {
    list_iter(v).next().and_then(|x| match x {
        Value::Symbol(s) => Some(&**s),
        _ => None,
    })
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn as_str(v: &Value) -> Option<&str> {
    match v {
        Value::String(s) => Some(s),
        Value::Symbol(s) => Some(s),
        _ => None,
    }
}

/// Returns `(refdes, lib_id, x, y, rot, mirror)` tuples for every
/// top-level `(symbol …)` instance in the schematic.
fn extract_symbols(path: &std::path::Path) -> Vec<(String, String, f64, f64, f64, String)> {
    let src = std::fs::read_to_string(path).expect("read sch");
    let root = lexpr::from_str(&src).expect("parse sch");
    let mut out = Vec::new();
    for child in list_iter(&root) {
        if first_atom(child) != Some("symbol") {
            continue;
        }
        let mut lib_id = String::new();
        let mut x = 0.0;
        let mut y = 0.0;
        let mut rot = 0.0;
        let mut mirror = String::new();
        let mut refdes = String::new();
        for sub in list_iter(child).skip(1) {
            match first_atom(sub) {
                Some("lib_id") => {
                    if let Some(s) = list_iter(sub).nth(1).and_then(as_str) {
                        lib_id = s.to_string();
                    }
                }
                Some("at") => {
                    let parts: Vec<&Value> = list_iter(sub).skip(1).collect();
                    if let Some(v) = parts.first().and_then(|v| as_f64(v)) {
                        x = v;
                    }
                    if let Some(v) = parts.get(1).and_then(|v| as_f64(v)) {
                        y = v;
                    }
                    if let Some(v) = parts.get(2).and_then(|v| as_f64(v)) {
                        rot = v;
                    }
                }
                Some("mirror") => {
                    if let Some(s) = list_iter(sub).nth(1).and_then(|v| match v {
                        Value::Symbol(s) => Some(&**s),
                        _ => None,
                    }) {
                        mirror = s.to_string();
                    }
                }
                Some("property") => {
                    let parts: Vec<&Value> = list_iter(sub).skip(1).collect();
                    if parts.first().and_then(|v| as_str(v)) == Some("Reference") {
                        if let Some(s) = parts.get(1).and_then(|v| as_str(v)) {
                            refdes = s.to_string();
                        }
                    }
                }
                _ => {}
            }
        }
        if !refdes.is_empty() {
            out.push((refdes, lib_id, x, y, rot, mirror));
        }
    }
    out.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    out
}

/// The recorded baseline of every emitted top-level `(symbol …)`
/// instance. Updating any tuple requires deliberation: it implies a
/// layout change. Add a comment when you change one.
///
/// All coordinates here reflect the V15 page-translation pass: the
/// emitter shifts every sheet's content bounding box so its top-left
/// corner lands at `PAGE_MARGIN_MM` (25.4 mm). The translation is a
/// single uniform grid-snapped offset, so every *relative* geometry
/// (rotation, mirror, inter-element spacing) is preserved — only the
/// absolute origins move, all to non-negative coordinates inside the
/// A4 drawable area. (Regenerated when V13(4) hid the `#PWRn`
/// Reference and nudged colliding property text, and again for V13(5)
/// when the nudge pass began clearing symbol-internal pin-name/number
/// text too: those decoration changes shifted some sheets' content
/// bbox, so the V15 offset moved by a single per-fixture delta — here
/// `diff_pair` shifted uniformly by +7.62 mm in X. Symbol poses
/// relative to one another are unchanged.)
///
/// Regenerated again for R-5 (V6/V14 rail-pin facing): 2-pin rail
/// *consumers* (`RC`/`R1` on `vcc`, `RE`/`R2`/`CE` on ground in
/// `common_emitter`; `C1` on ground in `rc_lowpass`; `RTAIL` on `vee`
/// in `diff_pair`) are now orientation-filtered so their rail pin faces
/// its band — flipping their `(mirror …)` / rotation and rippling the
/// SA-refined neighbour positions. No budget changed (this is a
/// snapshot, not a budget); every V5/V6/V11/V12/V13/V14 verifier stays
/// green.
///
/// Regenerated again for the V13 power-glyph-text / PWR_FLAG fix: (a) the
/// router now anchors each `power:*` glyph's net-name Value text on the
/// pin's *outward* side, whose upward reach (VCC/VEE) extends the content
/// bbox so the V15 page-translate offset shifts some sheets by one
/// per-fixture delta; (b) each `power:PWR_FLAG` is now co-located with
/// the rail glyph it drives but rotated 0/180 to point its chevron *away*
/// from the glyph body (rot follows GND-down vs VCC/VEE-up), so the
/// `#FLG*` rows move onto their `#PWR*` coordinate and flip rotation. All
/// relative poses are otherwise preserved; every verifier stays green.
///
/// Regenerated again for the Phase-1 class-aware PWR_FLAG pruning: a
/// PWR_FLAG is no longer emitted on a *signal* net that carries a
/// `Passive` (resistor/cap) terminal, because KiCad's `DrivingPinTypes`
/// counts `PT_PASSIVE` as a valid signal-net driver — so those flags
/// were redundant. This removed the spurious signal-net flags on
/// `common_emitter` (net `b`), `multivibrator` (two base nets), and
/// `opamp_inverting_real` (one feedback node), renumbering the surviving
/// `#FLG*` rows on those three sheets (fewer flag rows = less geometry;
/// ratchet direction DOWN). No symbol moved — the removed flags were
/// co-located with an existing rail glyph/pin, so the V15 content bbox
/// and every other pose are unchanged. Rail flags (one per global rail)
/// and genuinely input-only signal nets (`diff_pair` `in1`/`in2`,
/// base-only with no passive pin) are preserved; ERC stays 0 errors.
const BASELINE: &[(&str, &str, &str, f64, f64, f64, &str)] = &[
    // (fixture, refdes, lib_id, x, y, rot, mirror), sorted by
    // (fixture, refdes) to match the verifier's own ordering.
    //
    // Regenerated wholesale rather than patched: the rail `PWR_FLAG`
    // markers moved off the circuit into a bottom-right driver block
    // (each paired with its own `power:*` glyph), so the element SET
    // changed, not merely coordinates. Absolute positions also shifted
    // when the page frame began reserving property-text room
    // symmetrically — see the V15 note in docs/invariants.md.
    //
    // Regenerated again when the collinear outward stub was restored to
    // the Steiner stage (V5). Wires changed on most fixtures, which
    // moved the content bbox and so the V15 page-translate offset;
    // phase 4.5 also re-picked a few orientations now that its
    // router-in-the-loop oracle sees the outward routes again (notably
    // `rc_lowpass_ports` R1 → rot 180). No element was repositioned by
    // hand; every verifier is green.
    //
    // This is a SNAPSHOT, not a ratchet: it catches *accidental*
    // movement. Regenerate it deliberately when geometry changes for a
    // reason you can name — never to make a quality budget pass.
    //
    // Regenerated again after: (a) the rail-stub column idiom + the
    // un-inverted `cost::rail_direction` moved most elements, and (b)
    // rail glyphs began keying off the declared `*@power=` tag rather
    // than the net's spelling (`common_emitter`'s VCC glyph is now
    // `power:+12V`). Wholesale, not patched: 107 of 107 rows changed.
    //
    // Regenerated again when `Symbol::pins_in` stopped reporting
    // horizontal pins' outward direction backwards. That angle feeds the
    // router's outward stubs AND phase 4.5's V5 oracle, so orientations
    // moved on the fixtures with horizontal pins (opamp inputs/outputs,
    // `rc_lowpass_ports` R1 back to rot 0). True V5 violations summed
    // across fixtures fell 16 → 8; V16 (B, J) per fixture is unchanged
    // on 7 of 9 — see the commit message for the two exceptions.
    (
        "common_emitter",
        "#FLG1",
        "power:PWR_FLAG",
        102.87,
        76.2,
        0.0,
        "",
    ),
    (
        "common_emitter",
        "#FLG2",
        "power:PWR_FLAG",
        102.87,
        88.9,
        180.0,
        "",
    ),
    (
        "common_emitter",
        "#PWR1",
        "power:GND",
        55.88,
        73.66,
        0.0,
        "",
    ),
    (
        "common_emitter",
        "#PWR2",
        "power:GND",
        64.77,
        73.66,
        0.0,
        "",
    ),
    (
        "common_emitter",
        "#PWR3",
        "power:GND",
        69.85,
        73.66,
        0.0,
        "",
    ),
    (
        "common_emitter",
        "#PWR4",
        "power:+12V",
        55.88,
        31.75,
        0.0,
        "",
    ),
    (
        "common_emitter",
        "#PWR5",
        "power:+12V",
        64.77,
        31.75,
        0.0,
        "",
    ),
    (
        "common_emitter",
        "#PWR6",
        "power:GND",
        102.87,
        76.2,
        0.0,
        "",
    ),
    (
        "common_emitter",
        "#PWR7",
        "power:+12V",
        102.87,
        88.9,
        0.0,
        "",
    ),
    ("common_emitter", "CE", "Device:C", 69.85, 69.85, 0.0, ""),
    ("common_emitter", "CIN", "Device:C", 35.56, 53.34, 90.0, ""),
    (
        "common_emitter",
        "COUT",
        "Device:C",
        90.17,
        52.07,
        180.0,
        "",
    ),
    (
        "common_emitter",
        "Q1",
        "Device:Q_NPN_BCE",
        63.5,
        52.07,
        0.0,
        "y",
    ),
    ("common_emitter", "R1", "Device:R_US", 55.88, 35.56, 0.0, ""),
    (
        "common_emitter",
        "R2",
        "Device:R_US",
        55.88,
        69.85,
        0.0,
        "y",
    ),
    ("common_emitter", "RC", "Device:R_US", 64.77, 35.56, 0.0, ""),
    ("common_emitter", "RE", "Device:R_US", 64.77, 69.85, 0.0, ""),
    (
        "diff_pair",
        "#FLG1",
        "power:PWR_FLAG",
        30.48,
        49.53,
        180.0,
        "",
    ),
    (
        "diff_pair",
        "#FLG2",
        "power:PWR_FLAG",
        55.88,
        49.53,
        180.0,
        "",
    ),
    (
        "diff_pair",
        "#FLG3",
        "power:PWR_FLAG",
        66.04,
        63.5,
        180.0,
        "",
    ),
    ("diff_pair", "#FLG4", "power:PWR_FLAG", 66.04, 76.2, 0.0, ""),
    ("diff_pair", "#PWR1", "power:+12V", 38.1, 31.75, 0.0, ""),
    ("diff_pair", "#PWR2", "power:+12V", 48.26, 31.75, 0.0, ""),
    ("diff_pair", "#PWR3", "power:VEE", 43.18, 63.5, 180.0, ""),
    ("diff_pair", "#PWR4", "power:+12V", 66.04, 63.5, 0.0, ""),
    ("diff_pair", "#PWR5", "power:VEE", 66.04, 76.2, 180.0, ""),
    ("diff_pair", "Q1", "Device:Q_NPN_BCE", 35.56, 49.53, 0.0, ""),
    ("diff_pair", "Q2", "Device:Q_NPN_BCE", 50.8, 49.53, 0.0, "y"),
    ("diff_pair", "RC1", "Device:R_US", 38.1, 35.56, 0.0, ""),
    ("diff_pair", "RC2", "Device:R_US", 48.26, 35.56, 0.0, "y"),
    ("diff_pair", "RTAIL", "Device:R_US", 43.18, 59.69, 0.0, ""),
    (
        "multivibrator",
        "#FLG1",
        "power:PWR_FLAG",
        95.25,
        76.2,
        0.0,
        "",
    ),
    (
        "multivibrator",
        "#FLG2",
        "power:PWR_FLAG",
        95.25,
        88.9,
        180.0,
        "",
    ),
    ("multivibrator", "#PWR1", "power:GND", 54.61, 73.66, 0.0, ""),
    ("multivibrator", "#PWR2", "power:GND", 64.77, 73.66, 0.0, ""),
    ("multivibrator", "#PWR3", "power:+5V", 36.83, 31.75, 0.0, ""),
    ("multivibrator", "#PWR4", "power:+5V", 82.55, 31.75, 0.0, ""),
    ("multivibrator", "#PWR5", "power:+5V", 35.56, 44.45, 0.0, ""),
    ("multivibrator", "#PWR6", "power:+5V", 83.82, 44.45, 0.0, ""),
    ("multivibrator", "#PWR7", "power:GND", 95.25, 76.2, 0.0, ""),
    ("multivibrator", "#PWR8", "power:+5V", 95.25, 88.9, 0.0, ""),
    ("multivibrator", "C1", "Device:C", 52.07, 52.07, 0.0, ""),
    ("multivibrator", "C2", "Device:C", 67.31, 52.07, 0.0, "y"),
    (
        "multivibrator",
        "Q1",
        "Device:Q_NPN_BCE",
        52.07,
        68.58,
        0.0,
        "",
    ),
    (
        "multivibrator",
        "Q2",
        "Device:Q_NPN_BCE",
        67.31,
        68.58,
        0.0,
        "y",
    ),
    ("multivibrator", "RB1", "Device:R_US", 35.56, 48.26, 0.0, ""),
    (
        "multivibrator",
        "RB2",
        "Device:R_US",
        83.82,
        48.26,
        0.0,
        "y",
    ),
    ("multivibrator", "RC1", "Device:R_US", 36.83, 35.56, 0.0, ""),
    (
        "multivibrator",
        "RC2",
        "Device:R_US",
        82.55,
        35.56,
        0.0,
        "y",
    ),
    (
        "opamp_definition_level",
        "#FLG1",
        "power:PWR_FLAG",
        82.55,
        66.04,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#FLG2",
        "power:PWR_FLAG",
        82.55,
        78.74,
        180.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#FLG3",
        "power:PWR_FLAG",
        82.55,
        91.44,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#PWR1",
        "power:GND",
        29.21,
        45.72,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#PWR2",
        "power:GND",
        71.12,
        55.88,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#PWR3",
        "power:VCC",
        34.29,
        40.64,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#PWR4",
        "power:VCC",
        66.04,
        50.8,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#PWR5",
        "power:VEE",
        34.29,
        55.88,
        180.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#PWR6",
        "power:VEE",
        66.04,
        66.04,
        180.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#PWR7",
        "power:GND",
        82.55,
        66.04,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#PWR8",
        "power:VCC",
        82.55,
        78.74,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#PWR9",
        "power:VEE",
        82.55,
        91.44,
        180.0,
        "",
    ),
    (
        "opamp_definition_level",
        "RF1",
        "Device:R_US",
        62.23,
        43.18,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "RF2",
        "Device:R_US",
        45.72,
        43.18,
        0.0,
        "y",
    ),
    (
        "opamp_definition_level",
        "RIN1",
        "Device:R_US",
        60.96,
        35.56,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "RIN2",
        "Device:R_US",
        46.99,
        35.56,
        0.0,
        "y",
    ),
    (
        "opamp_definition_level",
        "X1",
        "Amplifier_Operational:OPAMP",
        36.83,
        48.26,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "X2",
        "Amplifier_Operational:OPAMP",
        63.5,
        58.42,
        0.0,
        "y",
    ),
    (
        "opamp_inverting",
        "#FLG1",
        "power:PWR_FLAG",
        109.22,
        57.15,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#FLG2",
        "power:PWR_FLAG",
        109.22,
        69.85,
        180.0,
        "",
    ),
    (
        "opamp_inverting",
        "#FLG3",
        "power:PWR_FLAG",
        109.22,
        82.55,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR1",
        "power:GND",
        66.04,
        31.75,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR2",
        "power:VCC",
        66.04,
        46.99,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR3",
        "power:VEE",
        66.04,
        52.07,
        180.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR4",
        "power:GND",
        109.22,
        57.15,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR5",
        "power:VCC",
        109.22,
        69.85,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR6",
        "power:VEE",
        109.22,
        82.55,
        180.0,
        "",
    ),
    (
        "opamp_inverting",
        "RF",
        "Device:R_US",
        58.42,
        40.64,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "RIN",
        "Device:R_US",
        35.56,
        39.37,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#FLG1",
        "power:PWR_FLAG",
        59.69,
        52.07,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#FLG2",
        "power:PWR_FLAG",
        59.69,
        64.77,
        180.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#FLG3",
        "power:PWR_FLAG",
        59.69,
        77.47,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR1",
        "power:GND",
        46.99,
        41.91,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR2",
        "power:VCC",
        41.91,
        36.83,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR3",
        "power:VEE",
        41.91,
        52.07,
        180.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR4",
        "power:GND",
        59.69,
        52.07,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR5",
        "power:VCC",
        59.69,
        64.77,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR6",
        "power:VEE",
        59.69,
        77.47,
        180.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "RF",
        "Device:R_US",
        48.26,
        44.45,
        180.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "RIN",
        "Device:R_US",
        35.56,
        35.56,
        90.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "X1",
        "Amplifier_Operational:OPAMP",
        39.37,
        44.45,
        0.0,
        "y",
    ),
    (
        "port_shapes",
        "#FLG1",
        "power:PWR_FLAG",
        93.98,
        57.15,
        0.0,
        "",
    ),
    ("port_shapes", "#PWR1", "power:GND", 82.55, 54.61, 0.0, ""),
    ("port_shapes", "#PWR2", "power:GND", 93.98, 57.15, 0.0, ""),
    ("port_shapes", "R1", "Device:R_US", 35.56, 35.56, 0.0, ""),
    ("port_shapes", "R2", "Device:R_US", 35.56, 44.45, 0.0, ""),
    ("port_shapes", "R3", "Device:R_US", 82.55, 41.91, 0.0, ""),
    ("port_shapes", "R4", "Device:R_US", 82.55, 50.8, 0.0, ""),
    (
        "rc_lowpass",
        "#FLG1",
        "power:PWR_FLAG",
        64.77,
        41.91,
        0.0,
        "",
    ),
    ("rc_lowpass", "#PWR1", "power:GND", 35.56, 39.37, 0.0, ""),
    ("rc_lowpass", "#PWR2", "power:GND", 64.77, 41.91, 0.0, ""),
    ("rc_lowpass", "C1", "Device:C", 35.56, 35.56, 0.0, ""),
    ("rc_lowpass", "R1", "Device:R_US", 50.8, 35.56, 270.0, ""),
    (
        "rc_lowpass_ports",
        "#FLG1",
        "power:PWR_FLAG",
        53.34,
        41.91,
        0.0,
        "",
    ),
    (
        "rc_lowpass_ports",
        "#PWR1",
        "power:GND",
        35.56,
        39.37,
        0.0,
        "",
    ),
    (
        "rc_lowpass_ports",
        "#PWR2",
        "power:GND",
        53.34,
        41.91,
        0.0,
        "",
    ),
    ("rc_lowpass_ports", "C1", "Device:C", 35.56, 35.56, 0.0, "y"),
    (
        "rc_lowpass_ports",
        "R1",
        "Device:R_US",
        41.91,
        35.56,
        0.0,
        "",
    ),
];

#[test]
fn baseline_lock_all_fixtures() {
    let mut failures = Vec::new();
    let mut all_actual = Vec::new();

    // All nine emitted fixtures. The port and definition-level sheets
    // were absent here while the rest of the suite had already been
    // extended to grade them, so accidental movement in the newest
    // features was the least protected.
    let fixtures = [
        "common_emitter",
        "diff_pair",
        "multivibrator",
        "opamp_definition_level",
        "opamp_inverting",
        "opamp_inverting_real",
        "port_shapes",
        "rc_lowpass",
        "rc_lowpass_ports",
    ];

    for fix in fixtures {
        let dir = tempdir(fix);
        let cir = fixtures_dir().join(format!("{fix}.cir"));
        let sch = spice_to_kicad(&cir, &dir).expect("emit schematic");
        for row in extract_symbols(&sch) {
            all_actual.push((fix.to_string(), row.0, row.1, row.2, row.3, row.4, row.5));
        }
    }

    let expected: Vec<_> = BASELINE
        .iter()
        .map(|t| {
            (
                t.0.to_string(),
                t.1.to_string(),
                t.2.to_string(),
                t.3,
                t.4,
                t.5,
                t.6.to_string(),
            )
        })
        .collect();

    // Detect differences with full context.
    let mut e_iter = expected.iter();
    let mut a_iter = all_actual.iter();
    let mut e_cur = e_iter.next();
    let mut a_cur = a_iter.next();
    loop {
        match (e_cur, a_cur) {
            (None, None) => break,
            (Some(e), None) => {
                failures.push(format!("MISSING in actual: {e:?}"));
                e_cur = e_iter.next();
            }
            (None, Some(a)) => {
                failures.push(format!("EXTRA in actual: {a:?}"));
                a_cur = a_iter.next();
            }
            (Some(e), Some(a)) => {
                if e == a {
                    e_cur = e_iter.next();
                    a_cur = a_iter.next();
                } else if (&e.0, &e.1) < (&a.0, &a.1) {
                    failures.push(format!("MISSING in actual: {e:?}"));
                    e_cur = e_iter.next();
                } else if (&e.0, &e.1) > (&a.0, &a.1) {
                    failures.push(format!("EXTRA in actual: {a:?}"));
                    a_cur = a_iter.next();
                } else {
                    failures.push(format!("DIFF\n  expected: {e:?}\n  actual:   {a:?}"));
                    e_cur = e_iter.next();
                    a_cur = a_iter.next();
                }
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "baseline_lock: {} differences\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}
