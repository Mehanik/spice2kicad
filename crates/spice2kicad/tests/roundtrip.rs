//! Functional round-trip tests:
//!     fixture.cir → spice2kicad → .kicad_sch → kicad-cli → .cir
//! and check that the topology of the original and the round-tripped
//! netlist match (modulo net renames, ordering, cosmetic value formatting,
//! and `*@ignore`d simulation scaffolding).
//!
//! All five tests are `#[ignore]`d while the schematic emitter is a stub —
//! flip them on as the emitter learns each fixture. Run them explicitly
//! with `cargo test -p spice2kicad --test roundtrip -- --ignored`.

mod common;

use std::path::PathBuf;

use common::{Canonical, kicad_to_spice, require_kicad_cli, spice_to_kicad};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn run_roundtrip(name: &str) {
    let src = fixtures_dir().join(format!("{name}.cir"));
    let original = std::fs::read_to_string(&src).expect("read fixture");

    let tmp = tempdir(name);
    let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");

    let Some(roundtripped) = kicad_to_spice(&sch, &tmp).expect("kicad-cli") else {
        assert!(
            !require_kicad_cli(),
            "kicad-cli not installed and REQUIRE_KICAD_CLI=1"
        );
        eprintln!("kicad-cli not on PATH — skipping round-trip comparison");
        return;
    };

    let lhs = Canonical::from_spice(&original);
    let rhs = Canonical::from_spice(&roundtripped);

    if let Err(diff) = lhs.matches(&rhs) {
        panic!(
            "round-trip mismatch for {name}:\n{diff}\n\n--- original ---\n{original}\n--- roundtripped ---\n{roundtripped}"
        );
    }
}

/// Cheap `tempdir` so we don't pull in the `tempfile` crate just for this.
fn tempdir(name: &str) -> PathBuf {
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("spice2kicad-test-{pid}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

#[test]
fn rc_lowpass() {
    run_roundtrip("rc_lowpass");
}

#[test]
fn common_emitter() {
    run_roundtrip("common_emitter");
}

#[test]
#[ignore = "opamp_inverting fixture instantiates `X1 ... OPAMP` subckt but no OPAMP symbol exists in the fixture library and emitter rejects unmapped X with E003; hierarchical-sheet emission for `.subckt` is not implemented yet"]
fn opamp_inverting() {
    run_roundtrip("opamp_inverting");
}

#[test]
fn multivibrator() {
    run_roundtrip("multivibrator");
}

#[test]
fn diff_pair() {
    run_roundtrip("diff_pair");
}

// --- Self-tests for the canonicalizer (no kicad-cli dependency) ---------
//
// These guard the comparison logic itself: it must (a) call out a real
// topology change, and (b) tolerate cosmetic/irrelevant differences. They
// run on every `cargo test`.

#[test]
fn canonical_matches_self() {
    let src = std::fs::read_to_string(fixtures_dir().join("rc_lowpass.cir")).unwrap();
    let c = Canonical::from_spice(&src);
    c.matches(&c).expect("a netlist must match itself");
}

#[test]
fn canonical_ignores_net_renaming() {
    let a = "R1 in  out 1k\nC1 out 0   100n\n";
    let b = "R1 a   b   1k\nC1 b   0   100n\n"; // same topology, different names
    let ca = Canonical::from_spice(a);
    let cb = Canonical::from_spice(b);
    ca.matches(&cb).expect("net-rename must not break match");
}

#[test]
fn canonical_ignores_value_formatting() {
    let a = "R1 in out 1k\n";
    let b = "R1 in out 1000\n";
    Canonical::from_spice(a)
        .matches(&Canonical::from_spice(b))
        .expect("1k must equal 1000");
}

#[test]
fn canonical_drops_ignored_elements() {
    // VIN is ignored — its presence in only one side should not matter.
    let a = "VIN in 0 AC 1 ;@ ignore\nR1 in out 1k\n";
    let b = "R1 in out 1k\n";
    Canonical::from_spice(a)
        .matches(&Canonical::from_spice(b))
        .expect("ignored elements must drop out of comparison");
}

#[test]
fn canonical_detects_topology_change() {
    let a = "R1 in out 1k\nC1 out 0 100n\n";
    let b = "R1 in out 1k\nC1 in  0 100n\n"; // C1 moved
    let res = Canonical::from_spice(a).matches(&Canonical::from_spice(b));
    assert!(res.is_err(), "topology change must be flagged");
}

#[test]
fn canonical_detects_missing_element() {
    let a = "R1 in out 1k\nC1 out 0 100n\n";
    let b = "R1 in out 1k\n";
    let res = Canonical::from_spice(a).matches(&Canonical::from_spice(b));
    assert!(res.is_err(), "missing element must be flagged");
}
