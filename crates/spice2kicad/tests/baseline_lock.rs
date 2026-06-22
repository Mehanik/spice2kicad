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
/// Five of the six fixtures are byte-identical to `master`. Only
/// `opamp_inverting_real` changed: the V14 power-pin-orientation hard
/// constraint locks its opamp `X1` to a V14-feasible pose (V+ pin up /
/// V- pin down), which the prior placer did not, so X1 and the symbols
/// the SA repacks around its wider rot-0 body move. The four passive
/// fixtures keep their exact pre-V14 SA trajectory (the V14 filter is a
/// no-op on 2-pin rail sources; the new SA gates engage only when a
/// V14-reoriented active device is present).
const BASELINE: &[(&str, &str, &str, f64, f64, f64, &str)] = &[
    // (fixture, refdes, lib_id, x, y, rot, mirror)
    ("common_emitter", "#PWR1", "power:GND", -1.27, 25.4, 0.0, ""),
    ("common_emitter", "#PWR2", "power:GND", 25.4, 36.83, 0.0, ""),
    (
        "common_emitter",
        "#PWR3",
        "power:GND",
        20.32,
        57.15,
        0.0,
        "",
    ),
    (
        "common_emitter",
        "#PWR4",
        "power:GND",
        34.29,
        49.53,
        0.0,
        "",
    ),
    ("common_emitter", "#PWR5", "power:VCC", 8.89, 25.4, 0.0, ""),
    ("common_emitter", "#PWR6", "power:VCC", 3.81, 38.1, 0.0, ""),
    (
        "common_emitter",
        "#PWR7",
        "power:VCC",
        -2.54,
        27.94,
        0.0,
        "",
    ),
    ("common_emitter", "CE", "Device:C", 30.48, 49.53, 90.0, ""),
    ("common_emitter", "CIN", "Device:C", 0.0, 45.72, 90.0, ""),
    ("common_emitter", "COUT", "Device:C", 45.72, 38.1, 0.0, ""),
    (
        "common_emitter",
        "Q1",
        "Device:Q_NPN_BCE",
        11.43,
        46.99,
        0.0,
        "y",
    ),
    (
        "common_emitter",
        "R1",
        "Device:R_US",
        3.81,
        33.02,
        180.0,
        "",
    ),
    (
        "common_emitter",
        "R2",
        "Device:R_US",
        21.59,
        36.83,
        90.0,
        "",
    ),
    ("common_emitter", "RC", "Device:R_US", -2.54, 31.75, 0.0, ""),
    ("common_emitter", "RE", "Device:R_US", 20.32, 53.34, 0.0, ""),
    (
        "common_emitter",
        "VCC",
        "Simulation_SPICE:VDC",
        3.81,
        25.4,
        270.0,
        "",
    ),
    ("diff_pair", "#PWR1", "power:GND", 17.78, 27.94, 0.0, ""),
    ("diff_pair", "#PWR2", "power:GND", -11.43, 27.94, 0.0, ""),
    ("diff_pair", "#PWR3", "power:VCC", 17.78, 17.78, 0.0, ""),
    ("diff_pair", "#PWR4", "power:VCC", 0.0, 16.51, 0.0, ""),
    ("diff_pair", "#PWR5", "power:VCC", 8.89, 16.51, 0.0, ""),
    ("diff_pair", "#PWR6", "power:GND", -11.43, 16.51, 0.0, ""),
    ("diff_pair", "#PWR7", "power:GND", -7.62, 25.4, 0.0, ""),
    ("diff_pair", "Q1", "Device:Q_NPN_BCE", 15.24, 34.29, 0.0, ""),
    (
        "diff_pair",
        "Q2",
        "Device:Q_NPN_BCE",
        24.13,
        34.29,
        0.0,
        "y",
    ),
    ("diff_pair", "RC1", "Device:R_US", 0.0, 20.32, 0.0, ""),
    ("diff_pair", "RC2", "Device:R_US", 8.89, 20.32, 0.0, "y"),
    ("diff_pair", "RTAIL", "Device:R_US", -3.81, 25.4, 270.0, ""),
    (
        "diff_pair",
        "VCC",
        "Simulation_SPICE:VDC",
        17.78,
        22.86,
        0.0,
        "",
    ),
    (
        "diff_pair",
        "VEE",
        "Simulation_SPICE:VDC",
        -11.43,
        22.86,
        0.0,
        "",
    ),
    ("multivibrator", "#PWR1", "power:GND", -8.89, 36.83, 0.0, ""),
    ("multivibrator", "#PWR2", "power:GND", 17.78, 62.23, 0.0, ""),
    ("multivibrator", "#PWR3", "power:GND", 21.59, 62.23, 0.0, ""),
    ("multivibrator", "#PWR4", "power:VCC", -8.89, 26.67, 0.0, ""),
    ("multivibrator", "#PWR5", "power:VCC", 0.0, 20.32, 0.0, ""),
    ("multivibrator", "#PWR6", "power:VCC", 39.37, 20.32, 0.0, ""),
    ("multivibrator", "#PWR7", "power:VCC", -1.27, 33.02, 0.0, ""),
    ("multivibrator", "#PWR8", "power:VCC", 40.64, 33.02, 0.0, ""),
    ("multivibrator", "C1", "Device:C", 15.24, 40.64, 0.0, ""),
    ("multivibrator", "C2", "Device:C", 24.13, 40.64, 0.0, "y"),
    (
        "multivibrator",
        "Q1",
        "Device:Q_NPN_BCE",
        15.24,
        57.15,
        0.0,
        "",
    ),
    (
        "multivibrator",
        "Q2",
        "Device:Q_NPN_BCE",
        24.13,
        57.15,
        0.0,
        "y",
    ),
    ("multivibrator", "RB1", "Device:R_US", -1.27, 36.83, 0.0, ""),
    (
        "multivibrator",
        "RB2",
        "Device:R_US",
        40.64,
        36.83,
        0.0,
        "y",
    ),
    ("multivibrator", "RC1", "Device:R_US", 0.0, 24.13, 0.0, ""),
    (
        "multivibrator",
        "RC2",
        "Device:R_US",
        39.37,
        24.13,
        0.0,
        "y",
    ),
    (
        "multivibrator",
        "VCC",
        "Simulation_SPICE:VDC",
        -8.89,
        31.75,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR1",
        "power:GND",
        2.54,
        31.75,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR2",
        "power:GND",
        11.43,
        31.75,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR3",
        "power:GND",
        200.0,
        55.08,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR4",
        "power:VCC",
        2.54,
        21.59,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR5",
        "power:VCC",
        200.0,
        70.32,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR6",
        "power:GND",
        11.43,
        20.32,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR7",
        "power:GND",
        200.0,
        75.4,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "RF",
        "Device:R_US",
        19.05,
        26.67,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "RIN",
        "Device:R_US",
        -13.97,
        26.67,
        180.0,
        "",
    ),
    (
        "opamp_inverting",
        "VCC",
        "Simulation_SPICE:VDC",
        2.54,
        26.67,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "VEE",
        "Simulation_SPICE:VDC",
        11.43,
        26.67,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR1",
        "power:GND",
        -3.81,
        36.83,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR2",
        "power:GND",
        8.89,
        25.4,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR3",
        "power:GND",
        -3.81,
        30.48,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR4",
        "power:VCC",
        -3.81,
        26.67,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR5",
        "power:VCC",
        1.27,
        25.4,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR6",
        "power:GND",
        -1.27,
        25.4,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR7",
        "power:GND",
        1.27,
        40.64,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "RF",
        "Device:R_US",
        11.43,
        30.48,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "RIN",
        "Device:R_US",
        -13.97,
        31.75,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "VCC",
        "Simulation_SPICE:VDC",
        -3.81,
        31.75,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "VEE",
        "Simulation_SPICE:VDC",
        3.81,
        25.4,
        90.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "X1",
        "Amplifier_Operational:OPAMP",
        3.81,
        33.02,
        0.0,
        "",
    ),
    ("rc_lowpass", "#PWR1", "power:GND", 6.35, 21.59, 0.0, ""),
    ("rc_lowpass", "C1", "Device:C", 2.54, 21.59, 90.0, ""),
    ("rc_lowpass", "R1", "Device:R_US", -7.62, 21.59, 90.0, ""),
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
