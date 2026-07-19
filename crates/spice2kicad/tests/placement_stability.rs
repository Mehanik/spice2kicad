//! **P10 / P11 — placement stability** (ADR-17 Stage 1).
//!
//! ADR-17's diagnosis is that the placer's defects are not individually
//! wrong rules but one architectural property: *a constraint cannot be
//! added without global, unattributable consequences.* Metropolis
//! acceptance over a shared RNG stream means a change anywhere re-bases
//! the whole layout, so a per-fixture ratchet — which assumes local,
//! attributable effects — turns into a change-prevention machine.
//!
//! You cannot fix what you cannot measure, and neither half of that
//! property had a verifier. These are the two.
//!
//! # P10 — determinism
//!
//! Two cache-less conversions of the same netlist produce byte-identical
//! output. This is the weaker half and it **already passes on master**:
//! the SA is seeded deterministically, so the run-to-run variance ADR-17
//! worried about does not exist. The ADR-17 design review expected this
//! test to need `#[ignore]` until the SA retires at Stage 2; measurement
//! says otherwise, so it lands live and guards the property from here on.
//!
//! Note what it does NOT say. Determinism is reproducibility of *one*
//! input, which is orthogonal to the sensitivity P11 measures: a
//! chaotic map is perfectly deterministic and still re-bases globally
//! on the smallest input change.
//!
//! # P11 — basin locality
//!
//! Adding ONE element to a netlist must move only the poses near it,
//! not re-place the sheet. This is the property ADR-15 Stage 5 needed
//! and did not have: shrinking one element's allowed orientation set
//! perturbed the SA trajectory into a different basin, moving *every*
//! element on `common_emitter` (R2 55.88 → 35.56, Q1 63.5 → 49.53, all
//! seven power glyphs) and taking B from 4 to 11. That regression was
//! only discovered by reading a `baseline_lock` diff after the fact.
//! With P11 in the suite it would have been a named, failing assertion
//! at the moment the change was made.
//!
//! **`#[ignore]`d until ADR-17 Stage 3.** It cannot pass while the SA
//! owns placement — that is precisely the finding it exists to record —
//! and the budgets below are today's measured blast radius, not a
//! target. Stage 3 un-ignores it and ratchets them down.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use lexpr::Value;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spice2kicad-stability-{pid}-{seq}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

/// Convert with the position-stability cache DISABLED.
///
/// Load-bearing: with the cache on, a re-conversion into a used
/// directory pins every element to its saved position, which would make
/// both tests here trivially pass while measuring nothing (and makes
/// phase 4.5 a silent no-op).
fn convert_no_cache(src: &Path, out_dir: &Path) -> PathBuf {
    let stem = src.file_stem().unwrap().to_string_lossy();
    let out = out_dir.join(format!("{stem}.kicad_sch"));
    let bin = env!("CARGO_BIN_EXE_spice2kicad");
    let lib_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("crates/kicad-symbols/tests/fixtures");
    let status = Command::new(bin)
        .arg(src)
        .arg("-t")
        .arg("schematic")
        .arg("--no-layout-cache")
        .arg("-o")
        .arg(&out)
        .arg("-l")
        .arg(lib_dir.join("Device.kicad_sym"))
        .arg("-l")
        .arg(lib_dir.join("Simulation_SPICE.kicad_sym"))
        .arg("-l")
        .arg(lib_dir.join("Amplifier_Operational.kicad_sym"))
        .arg("-l")
        .arg(lib_dir.join("power.kicad_sym"))
        .status()
        .expect("invoke spice2kicad");
    assert!(status.success(), "spice2kicad exited with {status}");
    out
}

// --- P10 ------------------------------------------------------------------

/// **P10 — determinism.** Two cache-less conversions of the same source
/// are byte-identical.
///
/// Landed LIVE, not `#[ignore]`d: measured on master, all ten fixtures
/// already round-trip identically (the SA's RNG is seeded from the
/// netlist, not from entropy). See the module doc.
#[test]
fn conversion_is_byte_deterministic_across_fixtures() {
    const FIXTURES: &[&str] = &[
        "rc_lowpass",
        "rc_lowpass_ports",
        "common_emitter",
        "multivibrator",
        "diff_pair",
        "opamp_inverting",
        "opamp_inverting_real",
        "port_shapes",
        "opamp_definition_level",
        "named_rails",
    ];
    let mut failures = Vec::new();
    for name in FIXTURES {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let a = std::fs::read(convert_no_cache(&src, &tempdir(name))).expect("read a");
        let b = std::fs::read(convert_no_cache(&src, &tempdir(name))).expect("read b");
        if a != b {
            failures.push(format!(
                "{name}: two cache-less conversions differ ({} vs {} bytes)",
                a.len(),
                b.len()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "P10: conversion is not deterministic:\n{}",
        failures.join("\n")
    );
}

// --- P11 ------------------------------------------------------------------

/// `(refdes, x, y, rot, mirror)` for every placed symbol, keyed by
/// refdes. Power glyphs are included on purpose: ADR-15's Stage-5
/// basin shift moved all seven of `common_emitter`'s, and a locality
/// metric that cannot see them would have scored that change clean.
fn poses(sch: &Path) -> BTreeMap<String, (f64, f64, f64, bool)> {
    fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
        v.list_iter().map_or_else(
            || Box::new(std::iter::empty()) as Box<dyn Iterator<Item = &Value>>,
            |it| Box::new(it),
        )
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
    fn head(v: &Value) -> Option<&str> {
        list_iter(v).next().and_then(as_str)
    }
    fn children<'a>(v: &'a Value, name: &str) -> Vec<&'a Value> {
        list_iter(v)
            .filter(|c| c.is_list() && head(c) == Some(name))
            .collect()
    }

    let src = std::fs::read_to_string(sch).expect("read sch");
    let root: Value = lexpr::from_str(&src).expect("parse sch");
    let mut out = BTreeMap::new();
    for sym in children(&root, "symbol") {
        let Some(at) = children(sym, "at").first().copied() else {
            continue;
        };
        let mut it = list_iter(at);
        it.next();
        let (Some(x), Some(y)) = (it.next().and_then(as_f64), it.next().and_then(as_f64)) else {
            continue;
        };
        let rotation = it.next().and_then(as_f64).unwrap_or(0.0);
        let mirror = children(sym, "mirror")
            .first()
            .and_then(|m| list_iter(m).nth(1).and_then(as_str))
            .is_some_and(|s| s == "y");
        let mut refdes = None;
        for prop in children(sym, "property") {
            let mut pit = list_iter(prop);
            pit.next();
            if pit.next().and_then(as_str) == Some("Reference") {
                refdes = pit.next().and_then(as_str).map(str::to_owned);
                break;
            }
        }
        if let Some(r) = refdes {
            out.insert(r, (x, y, rotation, mirror));
        }
    }
    out
}

/// One P11 case: a base netlist, the same netlist with ONE element
/// added, the refdes that are expected to move (the added element's own
/// column/row neighbourhood), and the measured blast radius today.
struct LocalityCase {
    name: &'static str,
    base: &'static str,
    added: &'static str,
    /// Refdes permitted to move: the added element plus its immediate
    /// electrical neighbours. Anything else that moves is a basin shift.
    local: &'static [&'static str],
    /// Zero-slack ratchet: how many NON-`local` pre-existing symbols
    /// move today. ADR-17 Stage 3 drives this to 0.
    non_local_budget: usize,
}

/// `rc_lowpass` + one series resistor, and `common_emitter` + one
/// bypass capacitor — the two shapes ADR-17 Stage 3 must keep local.
///
/// **Measured blast radius on master** (run with `--ignored` to see it):
/// `rc_lowpass_plus_r` moves 5 of the 5 pre-existing symbols;
/// `common_emitter_plus_c` moves **17 of 17** — adding one capacitor
/// re-places the entire sheet, power glyphs included. Nothing survives
/// untouched in either case, so the "basin" is the whole page. That
/// number is ADR-17's diagnosis stated as a measurement.
///
/// The budgets are 0 rather than 5/17 on purpose: this is a *target*
/// for a test that does not run yet, not a ratchet on live behaviour.
/// Recording 5/17 as a passing budget would enshrine the defect.
const LOCALITY_CASES: &[LocalityCase] = &[
    LocalityCase {
        name: "rc_lowpass_plus_r",
        base: "rc_lowpass",
        // A second series resistor splitting `out` into `out`/`mid`.
        added: "R2 out mid 1k\nC2 mid 0 100n\n",
        local: &["R2", "C2"],
        non_local_budget: 0,
    },
    LocalityCase {
        name: "common_emitter_plus_c",
        base: "common_emitter",
        // One more bypass capacitor on the existing `b` node.
        added: "CB b 0 10n\n",
        local: &["CB"],
        non_local_budget: 0,
    },
];

/// **P11 — basin locality.** See the module doc.
///
/// `#[ignore]`d: it cannot pass while the SA owns placement. ADR-17
/// Stage 3 replaces the SA with deterministic construction and
/// un-ignores this test; the budgets below are the measured blast
/// radius on master, recorded so Stage 3 has a floor to beat rather
/// than a target to argue about.
#[test]
#[ignore = "ADR-17 Stage 1: records the SA's blast radius; un-ignore at Stage 3"]
fn adding_one_element_moves_only_its_neighbourhood() {
    let mut failures = Vec::new();
    for case in LOCALITY_CASES {
        let base_src = fixtures_dir().join(format!("{}.cir", case.base));
        let text = std::fs::read_to_string(&base_src).expect("read base fixture");
        // Splice the new element in ahead of the trailing directives, so
        // the added line is a plain element in the same deck.
        let grown = text.replace(".end", &format!("{}\n.end", case.added));
        assert_ne!(grown, text, "{}: failed to splice element", case.name);

        let dir = tempdir(case.name);
        let grown_src = dir.join(format!("{}.cir", case.name));
        std::fs::write(&grown_src, &grown).expect("write grown fixture");

        let before = poses(&convert_no_cache(&base_src, &tempdir(case.name)));
        let after = poses(&convert_no_cache(&grown_src, &tempdir(case.name)));

        let moved: Vec<&String> = before
            .iter()
            .filter(|(r, p)| after.get(*r).is_none_or(|q| q != *p))
            .map(|(r, _)| r)
            .collect();
        let non_local: Vec<&&String> = moved
            .iter()
            .filter(|r| !case.local.contains(&r.as_str()))
            .collect();

        if non_local.len() > case.non_local_budget {
            failures.push(format!(
                "{}: adding `{}` moved {} pre-existing symbol(s) outside its neighbourhood \
                 (budget {}): {non_local:?}",
                case.name,
                case.added.trim().replace('\n', "; "),
                non_local.len(),
                case.non_local_budget,
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "P11: placement is not basin-local:\n{}",
        failures.join("\n")
    );
}
