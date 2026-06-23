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
/// A4 drawable area. (Regenerated once when V13(4) hid the `#PWRn`
/// Reference and nudged colliding property text: those decoration
/// changes shrank/shifted each sheet's content bbox, so the V15
/// offset moved by a single per-fixture delta — symbol poses relative
/// to one another are unchanged.)
const BASELINE: &[(&str, &str, &str, f64, f64, f64, &str)] = &[
    // (fixture, refdes, lib_id, x, y, rot, mirror)
    (
        "common_emitter",
        "#PWR1",
        "power:GND",
        58.42,
        35.56,
        0.0,
        "",
    ),
    (
        "common_emitter",
        "#PWR2",
        "power:GND",
        53.34,
        55.88,
        0.0,
        "",
    ),
    ("common_emitter", "#PWR3", "power:GND", 63.5, 52.07, 0.0, ""),
    (
        "common_emitter",
        "#PWR4",
        "power:VCC",
        36.83,
        36.83,
        0.0,
        "",
    ),
    (
        "common_emitter",
        "#PWR5",
        "power:VCC",
        30.48,
        35.56,
        0.0,
        "",
    ),
    ("common_emitter", "CE", "Device:C", 63.5, 48.26, 0.0, ""),
    ("common_emitter", "CIN", "Device:C", 33.02, 44.45, 90.0, ""),
    ("common_emitter", "COUT", "Device:C", 78.74, 36.83, 0.0, ""),
    (
        "common_emitter",
        "Q1",
        "Device:Q_NPN_BCE",
        44.45,
        45.72,
        0.0,
        "y",
    ),
    (
        "common_emitter",
        "R1",
        "Device:R_US",
        36.83,
        31.75,
        180.0,
        "",
    ),
    (
        "common_emitter",
        "R2",
        "Device:R_US",
        54.61,
        35.56,
        90.0,
        "",
    ),
    (
        "common_emitter",
        "RC",
        "Device:R_US",
        30.48,
        30.48,
        180.0,
        "",
    ),
    ("common_emitter", "RE", "Device:R_US", 53.34, 52.07, 0.0, ""),
    ("diff_pair", "#PWR1", "power:VCC", 30.48, 26.67, 0.0, ""),
    ("diff_pair", "#PWR2", "power:VCC", 39.37, 26.67, 0.0, ""),
    ("diff_pair", "#PWR3", "power:GND", 25.4, 39.37, 0.0, ""),
    ("diff_pair", "Q1", "Device:Q_NPN_BCE", 45.72, 44.45, 0.0, ""),
    (
        "diff_pair",
        "Q2",
        "Device:Q_NPN_BCE",
        58.42,
        44.45,
        0.0,
        "y",
    ),
    ("diff_pair", "RC1", "Device:R_US", 30.48, 30.48, 0.0, ""),
    ("diff_pair", "RC2", "Device:R_US", 39.37, 30.48, 0.0, "y"),
    ("diff_pair", "RTAIL", "Device:R_US", 25.4, 35.56, 0.0, ""),
    ("multivibrator", "#PWR1", "power:GND", 44.45, 67.31, 0.0, ""),
    ("multivibrator", "#PWR2", "power:GND", 52.07, 67.31, 0.0, ""),
    ("multivibrator", "#PWR3", "power:VCC", 26.67, 25.4, 0.0, ""),
    ("multivibrator", "#PWR4", "power:VCC", 69.85, 25.4, 0.0, ""),
    ("multivibrator", "#PWR5", "power:VCC", 25.4, 38.1, 0.0, ""),
    ("multivibrator", "#PWR6", "power:VCC", 71.12, 38.1, 0.0, ""),
    ("multivibrator", "C1", "Device:C", 41.91, 45.72, 0.0, ""),
    ("multivibrator", "C2", "Device:C", 54.61, 45.72, 0.0, "y"),
    (
        "multivibrator",
        "Q1",
        "Device:Q_NPN_BCE",
        41.91,
        62.23,
        0.0,
        "",
    ),
    (
        "multivibrator",
        "Q2",
        "Device:Q_NPN_BCE",
        54.61,
        62.23,
        0.0,
        "y",
    ),
    ("multivibrator", "RB1", "Device:R_US", 25.4, 41.91, 0.0, ""),
    (
        "multivibrator",
        "RB2",
        "Device:R_US",
        71.12,
        41.91,
        0.0,
        "y",
    ),
    ("multivibrator", "RC1", "Device:R_US", 26.67, 29.21, 0.0, ""),
    (
        "multivibrator",
        "RC2",
        "Device:R_US",
        69.85,
        29.21,
        0.0,
        "y",
    ),
    (
        "opamp_inverting",
        "#PWR1",
        "power:GND",
        59.69,
        31.75,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR2",
        "power:VCC",
        59.69,
        46.99,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR3",
        "power:GND",
        59.69,
        52.07,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "RF",
        "Device:R_US",
        58.42,
        43.18,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "RIN",
        "Device:R_US",
        25.4,
        43.18,
        180.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR1",
        "power:GND",
        25.4,
        31.75,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR2",
        "power:VCC",
        30.48,
        26.67,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR3",
        "power:GND",
        30.48,
        41.91,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "RF",
        "Device:R_US",
        44.45,
        30.48,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "RIN",
        "Device:R_US",
        30.48,
        43.18,
        270.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "X1",
        "Amplifier_Operational:OPAMP",
        33.02,
        34.29,
        0.0,
        "",
    ),
    ("rc_lowpass", "#PWR1", "power:GND", 43.18, 27.94, 0.0, ""),
    ("rc_lowpass", "C1", "Device:C", 39.37, 27.94, 90.0, ""),
    ("rc_lowpass", "R1", "Device:R_US", 29.21, 27.94, 90.0, ""),
];

#[test]
fn baseline_lock_all_fixtures() {
    let mut failures = Vec::new();
    let mut all_actual = Vec::new();

    let fixtures = [
        "common_emitter",
        "diff_pair",
        "multivibrator",
        "opamp_inverting",
        "opamp_inverting_real",
        "rc_lowpass",
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
