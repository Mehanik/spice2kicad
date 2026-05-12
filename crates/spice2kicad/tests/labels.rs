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

fn tempdir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spice2kicad-labels-{pid}-{seq}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
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

const SHEETS: &[&str] = &[
    "rc_lowpass",
    "common_emitter",
    "multivibrator",
    "diff_pair",
    "opamp_inverting_real",
];

#[test]
fn v4_plain_label_count_per_net_within_budget() {
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let plain = count_labels(&root, "label");
        for (net, n) in &plain {
            assert!(
                *n <= 2,
                "{name}: net {net} carries {n} plain labels; V4 caps at 2 \
                 (1 for purely-internal nets, 2 for nets touching a \
                 hierarchical-sheet port)",
            );
        }
    }
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
    ];
    for (name, allowed) in allowed_per_fixture {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let globals = count_labels(&root, "global_label");
        for net in globals.keys() {
            assert!(
                allowed.contains(&net.as_str()),
                "{name}: unexpected (global_label \"{net}\") — V4 reserves \
                 global labels for cross-sheet or one-pin interface nets only",
            );
        }
    }
}
