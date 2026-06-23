//! V10 power-source suppression: a voltage source carrying a `power`
//! directive (`;@ power=<rail>` trailing tag or a `*@power for=<ref>`
//! block) is a power RAIL, not a drawn component. The annotation spec
//! §4.5 states "The source itself is not drawn; instead, every
//! reference to the named net renders as a KiCad power flag."
//!
//! This verifier asserts the emitter honours that: no
//! `(symbol (lib_id "Simulation_SPICE:V…") …)` instance may carry the
//! refdes of a power-tagged source. The set of power-tagged refdes is
//! DERIVED from each fixture's `.cir` by scanning the `power`
//! annotation — never a hardcoded refdes/fixture list — so the check
//! generalises to any future fixture.

mod common;

use std::path::PathBuf;

use common::spice_to_kicad;
use lexpr::Value;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> PathBuf {
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("spice2kicad-pwrsup-{pid}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

/// Derive the set of power-tagged source refdes from a fixture `.cir`,
/// generally, by scanning the `power` annotation in both carriers:
///   * trailing tag: `VCC vcc 0 DC 12  ;@ power=+12V`  → refdes `VCC`
///   * block form:   `*@power for=VCC`                  → refdes `VCC`
///
/// No fixture/refdes names are hardcoded.
fn power_source_refdes(src: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for raw in src.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        // Block carrier: `*@power for=<refdes>`.
        if lower.starts_with("*@power") {
            if let Some(idx) = lower.find("for=") {
                let rest = line[idx + 4..].trim();
                let refdes = rest.split_whitespace().next().unwrap_or("");
                if !refdes.is_empty() {
                    out.insert(refdes.to_string());
                }
            }
            continue;
        }
        // Trailing carrier: the element line bears `;@ power=` (or
        // `;@power=`). The refdes is the line's first token.
        if let Some((body, tags)) = line.split_once(";@") {
            // Re-stitch in case of multiple `;@` tags on one line.
            let all_tags = format!(";@{tags}");
            if all_tags.to_ascii_lowercase().contains("power=") {
                let refdes = body.split_whitespace().next().unwrap_or("");
                if !refdes.is_empty() {
                    out.insert(refdes.to_string());
                }
            }
        }
    }
    out
}

fn list_iter(v: &Value) -> impl Iterator<Item = &Value> {
    let mut cur = v;
    std::iter::from_fn(move || match cur {
        Value::Cons(c) => {
            let (head, tail) = c.as_pair();
            cur = tail;
            Some(head)
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

fn as_str(v: &Value) -> Option<&str> {
    match v {
        Value::String(s) | Value::Symbol(s) => Some(s),
        _ => None,
    }
}

/// Returns `(refdes, lib_id)` for every top-level `(symbol …)`.
fn extract_symbols(path: &std::path::Path) -> Vec<(String, String)> {
    let src = std::fs::read_to_string(path).expect("read sch");
    let root = lexpr::from_str(&src).expect("parse sch");
    let mut out = Vec::new();
    for child in list_iter(&root) {
        if first_atom(child) != Some("symbol") {
            continue;
        }
        let mut lib_id = String::new();
        let mut refdes = String::new();
        for sub in list_iter(child).skip(1) {
            match first_atom(sub) {
                Some("lib_id") => {
                    if let Some(s) = list_iter(sub).nth(1).and_then(as_str) {
                        lib_id = s.to_string();
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
            out.push((refdes, lib_id));
        }
    }
    out
}

const FIXTURES: &[&str] = &[
    "common_emitter",
    "diff_pair",
    "multivibrator",
    "opamp_inverting",
    "opamp_inverting_real",
    "rc_lowpass",
];

#[test]
fn power_sources_emit_no_drawn_symbol() {
    let mut failures = Vec::new();
    for fix in FIXTURES {
        let cir = fixtures_dir().join(format!("{fix}.cir"));
        let src = std::fs::read_to_string(&cir).expect("read fixture");
        let power_refs = power_source_refdes(&src);
        if power_refs.is_empty() {
            continue;
        }
        let dir = tempdir(fix);
        let sch = spice_to_kicad(&cir, &dir).expect("emit schematic");
        for (refdes, lib_id) in extract_symbols(&sch) {
            if power_refs.contains(&refdes) {
                failures.push(format!(
                    "{fix}: power-tagged source {refdes} drawn as \
                     symbol (lib_id {lib_id}); must be suppressed \
                     (rail glyphs carry connectivity)"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "V10 power-source suppression violated:\n{}",
        failures.join("\n")
    );
}
