//! V4 label-policy verifier.
//!
//! Per CLAUDE.md V4:
//!  * `(global_label …)` is reserved for cross-sheet nets and for
//!    one-pin "interface" nets that cannot anchor a plain label.
//!    On a single-sheet fixture without hierarchical sheets, the
//!    only global labels permitted are those one-pin interface
//!    nets — typically the schematic's `in` and `out` ports.
//!  * Internal signal nets emit one (or, when the net touches a
//!    hierarchical-sheet port marker, two) plain `(label …)` —
//!    never more than two per net per sheet.
//!  * Power / Ground nets emit zero labels (the `power:*` glyph is
//!    the connectivity carrier — V10).

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use common::spice_to_kicad;
use lexpr::Value;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> common::TempDir {
    common::TempDir::new("labels", name)
}

fn parse(path: &std::path::Path) -> Value {
    let src = std::fs::read_to_string(path).expect("read sch");
    lexpr::from_str(&src).expect("parse sch as lexpr")
}

fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    match v.list_iter() {
        Some(it) => Box::new(it),
        None => Box::new(std::iter::empty()),
    }
}

fn head(v: &Value) -> Option<&str> {
    list_iter(v).next().and_then(|h| h.as_symbol())
}

fn as_str(v: &Value) -> Option<&str> {
    v.as_str().or_else(|| v.as_symbol())
}

fn count_labels(root: &Value, kind: &str) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for item in list_iter(root) {
        if head(item) != Some(kind) {
            continue;
        }
        if let Some(name) = list_iter(item).nth(1).and_then(as_str) {
            *out.entry(name.to_owned()).or_insert(0) += 1;
        }
    }
    out
}

/// Fixtures graded for label policy. Mirrors the list in
/// `electrical_safety.rs`: the port and hierarchical-sheet fixtures were
/// emitted but graded by nothing. `opamp_definition_level` is excluded
/// there for routing defects; V4 is independent of those, so it is graded
/// here.
const SHEETS: &[&str] = &[
    "rc_lowpass",
    "common_emitter",
    "multivibrator",
    "diff_pair",
    "opamp_inverting_real",
    "opamp_inverting",
    "opamp_definition_level",
    "port_shapes",
    "rc_lowpass_ports",
    "named_rails",
    "rc_phase_shift",
    "two_stage_amp",
    "cascode_amp",
    "lc_ladder_lpf",
    "sallen_key_lpf",
    "wien_bridge_osc",
    "sallen_key_driven",
    "shunt_feedback_amp",
    "resistor_ladder_ref",
    "compensated_divider",
];

#[test]
fn v4_plain_label_count_per_net_within_budget() {
    // Collect-then-assert (ADR-23 D2): an over-budget fixture must not
    // abort the loop, or every later fixture goes unmeasured.
    let mut failures: Vec<String> = Vec::new();
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let plain = count_labels(&root, "label");
        let mut over = 0usize;
        for (net, n) in &plain {
            if *n > 2 {
                over += 1;
                failures.push(format!(
                    "{name}: net {net} carries {n} plain labels; V4 caps at 2 \
                     (1 for purely-internal nets, 2 for nets touching a \
                     hierarchical-sheet port)",
                ));
            }
        }
        common::scoreboard::record_count("v4.plain_label_excess", name, over);
    }
    assert!(
        failures.is_empty(),
        "V4 plain-label budget exceeded (cap 2 per net per sheet):\n  {}",
        failures.join("\n  "),
    );
}

#[test]
fn v4_global_labels_reserved_for_interface_one_pin_nets() {
    // None of the five fixtures has a hierarchical sheet boundary on
    // its top-level schematic; the only legitimate global labels are
    // the *external interface* nets — single-pin nets that the user
    // would drive from outside (typically `in`, `out`). Anything else
    // is a V4 violation.
    let allowed_per_fixture: &[(&str, &[&str])] = &[
        ("rc_lowpass", &["in", "out"]),
        ("common_emitter", &["in", "out"]),
        ("multivibrator", &[]),
        ("diff_pair", &["in1", "in2"]),
        ("opamp_inverting_real", &["in"]),
        ("rc_phase_shift", &["in", "out"]),
        ("two_stage_amp", &["in", "out"]),
        ("cascode_amp", &["in", "out"]),
        ("lc_ladder_lpf", &["out"]),
        ("sallen_key_lpf", &["in", "out"]),
        ("wien_bridge_osc", &["out"]),
        ("compensated_divider", &["out"]),
        ("resistor_ladder_ref", &["t2", "t3", "t4"]),
    ];
    // Collect-then-assert (ADR-23 D2): see the sibling verifier above.
    let mut failures: Vec<String> = Vec::new();
    for (name, allowed) in allowed_per_fixture {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let globals = count_labels(&root, "global_label");
        let mut unexpected = 0usize;
        for net in globals.keys() {
            if !allowed.contains(&net.as_str()) {
                unexpected += 1;
                failures.push(format!(
                    "{name}: unexpected (global_label \"{net}\") — V4 reserves \
                     global labels for cross-sheet or one-pin interface nets only",
                ));
            }
        }
        common::scoreboard::record_count("v4.global_label_misuse", name, unexpected);
    }
    assert!(
        failures.is_empty(),
        "V4 global-label policy violations:\n  {}",
        failures.join("\n  "),
    );
}
