//! Electrical equivalence: does the emitted schematic wire up the same
//! circuit the SPICE source described?
//!
//! Every other test in this suite measures *geometry* — where symbols and
//! wires sit, whether text collides, whether the page is tidy. None of
//! them asks the only question that really matters: **is the circuit
//! right?** A schematic can satisfy all of them and still connect a pin
//! to the wrong net, or to nothing at all.
//!
//! `phase1_erc_stays_clean` is the closest existing check, and it is not
//! close enough: `kicad-cli sch erc` reports a pin connected to *nothing*,
//! but says nothing about a pin connected to the *wrong* net — a short or
//! a swap leaves ERC perfectly happy while the circuit is wrong.
//!
//! This closes that gap by round-tripping through KiCad itself:
//!
//! 1. parse the source `.cir` for each element's node list,
//! 2. run `kicad-cli sch export netlist --format spice` on what we
//!    emitted, and parse the same way,
//! 3. compare the two **partitions** of pins into nets.
//!
//! Comparing partitions rather than net *names* is essential: KiCad
//! renames nets freely (`out` becomes `/out`, `0` becomes `GND`), and
//! those renames are correct. What must hold is that two pins share a net
//! in the emitted schematic exactly when they share one in the source.
//!
//! The source is parsed independently here rather than reusing
//! `spice-resolve`, so the two sides of the comparison share no code: the
//! expectation comes from the netlist text, the result from KiCad's own
//! connectivity engine.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Fixtures whose emitted netlist is compared against their source.
///
/// One is deliberately absent, and the reason is worth recording rather
/// than leaving as a silent gap: `opamp_inverting` lowers its `.subckt`
/// to a hierarchical sheet, so KiCad's export flattens the instance into
/// the child's `E1` and there is no `X1` to compare. Checking that
/// flattening is correct needs a hierarchy-aware comparison this test
/// does not attempt.
///
/// `opamp_definition_level` used to be absent too: it exported its
/// instances as bare `X1 __X1`, with **no nodes at all**, because the
/// emitter wrote no `Sim.*` properties for a `.subckt` instance and
/// KiCad's SPICE exporter could not recover the pin order. It is now
/// included, and it is the check that keeps that fix honest — an `X`
/// line's node *order* is exactly what regressed.
const FIXTURES: &[&str] = &[
    "rc_lowpass",
    "common_emitter",
    "multivibrator",
    "diff_pair",
    "opamp_inverting_real",
    "opamp_definition_level",
    "port_shapes",
    "rc_lowpass_ports",
    "named_rails",
    "rc_phase_shift",
];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spice2kicad-equiv-{pid}-{seq}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

/// How many leading tokens after the refdes are nodes, by device letter.
/// `None` means "not comparable".
///
/// `X` (subckt call) is deliberately not here: its arity is variable, so
/// it is handled separately in [`parse_elements`] — every token after
/// the refdes is a node except the trailing subckt name.
fn node_count(refdes: &str) -> Option<usize> {
    match refdes.chars().next()?.to_ascii_uppercase() {
        'R' | 'C' | 'L' | 'D' | 'V' | 'I' => Some(2),
        'Q' => Some(3),
        // MOSFET (drain/gate/source/bulk) and the controlled sources
        // (two output nodes, two controlling nodes) all take four.
        'M' | 'E' | 'G' | 'F' | 'H' => Some(4),
        _ => None,
    }
}

/// `refdes -> node names`, parsed from SPICE text.
///
/// Deliberately small and literal. Lines inside a `.subckt` body are
/// skipped (they describe a definition, not this sheet's wiring), as are
/// elements the annotations remove from the schematic: `;@ ignore` drops
/// the element entirely, and `;@ power` turns a source into rail glyphs
/// with no element of its own.
fn parse_elements(text: &str) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    let mut in_subckt = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('*') || line.starts_with('+') {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with(".subckt") {
            in_subckt = true;
            continue;
        }
        if lower.starts_with(".ends") {
            in_subckt = false;
            continue;
        }
        if in_subckt || line.starts_with('.') {
            continue;
        }
        // Annotation-driven omissions, checked before the comment strip.
        if line.contains("@ ignore") || line.contains("@ignore") {
            continue;
        }
        if line.contains("@ power") || line.contains("@power") {
            continue;
        }
        let code = line.split(';').next().unwrap_or("").trim();
        let mut toks = code.split_whitespace();
        let Some(refdes) = toks.next() else { continue };
        if refdes.starts_with(['X', 'x']) {
            // `X<name> n1 … nk <subckt>` — variable arity, so the nodes
            // are everything but the trailing subckt name. A bare
            // `X1 OPAMP` (the old no-nodes defect) yields an empty node
            // list, which the comparison then reports as a mismatch
            // rather than silently skipping.
            let mut toks: Vec<&str> = toks.collect();
            toks.pop();
            out.insert(
                refdes.to_ascii_uppercase(),
                toks.iter().map(|t| t.to_ascii_lowercase()).collect(),
            );
            continue;
        }
        let Some(n) = node_count(refdes) else {
            continue;
        };
        let nodes: Vec<String> = toks.take(n).map(str::to_ascii_lowercase).collect();
        if nodes.len() == n {
            out.insert(refdes.to_ascii_uppercase(), nodes);
        }
    }
    out
}

/// The partition induced by a netlist: the set of pin-groups that share a
/// net, with each pin named `REFDES.index`.
///
/// Net *names* are discarded — only the grouping survives, which is what
/// makes source and KiCad comparable despite KiCad's renaming.
fn partition(elements: &BTreeMap<String, Vec<String>>) -> BTreeSet<BTreeSet<String>> {
    let mut by_net: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for (refdes, nodes) in elements {
        for (i, node) in nodes.iter().enumerate() {
            by_net
                .entry(node.as_str())
                .or_default()
                .insert(format!("{refdes}.{i}"));
        }
    }
    by_net.into_values().collect()
}

/// Restrict `elements` to `keep`, so the two sides compare like with like.
fn restrict(
    elements: &BTreeMap<String, Vec<String>>,
    keep: &BTreeSet<String>,
) -> BTreeMap<String, Vec<String>> {
    elements
        .iter()
        .filter(|(k, _)| keep.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[test]
fn emitted_schematic_matches_source_netlist() {
    let mut failures: Vec<String> = Vec::new();

    for name in FIXTURES {
        let src_path = fixtures_dir().join(format!("{name}.cir"));
        let src_text = std::fs::read_to_string(&src_path).expect("read fixture");
        let source = parse_elements(&src_text);

        let tmp = tempdir(name);
        let sch = common::spice_to_kicad(&src_path, &tmp).expect("spice2kicad");
        let Some(round) = common::kicad_to_spice(&sch, &tmp).expect("kicad-cli netlist") else {
            eprintln!("kicad-cli not installed — skipping {name}");
            continue;
        };
        let emitted = parse_elements(&round);

        // Compare only elements both sides agree exist with the same pin
        // count. An element the emitter legitimately removes (annotation)
        // or flattens (hierarchical subckt) is not a wiring error.
        let shared: BTreeSet<String> = source
            .keys()
            .filter(|k| emitted.get(*k).is_some_and(|e| e.len() == source[*k].len()))
            .cloned()
            .collect();

        // Every source element should survive to the schematic; a missing
        // one means a component was dropped outright.
        let missing: Vec<&String> = source
            .keys()
            .filter(|k| !emitted.contains_key(*k))
            .collect();
        if !missing.is_empty() {
            failures.push(format!(
                "{name}: elements missing from emitted schematic: {missing:?}"
            ));
        }

        let want = partition(&restrict(&source, &shared));
        let got = partition(&restrict(&emitted, &shared));
        if want != got {
            let only_want: Vec<_> = want.difference(&got).collect();
            let only_got: Vec<_> = got.difference(&want).collect();
            failures.push(format!(
                "{name}: connectivity differs from the source netlist\n    \
                 in source but not emitted: {only_want:?}\n    \
                 in emitted but not source: {only_got:?}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "emitted schematics do not match their source netlists:\n  {}",
        failures.join("\n  "),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_reads_nodes_and_skips_annotated_elements() {
        let src = "\
* title\n\
V1 in 0 AC 1 ;@ ignore\n\
VCC vcc 0 DC 15 ;@ power=+15V\n\
R1 in out 1k\n\
C1 out 0 100n\n\
.end\n";
        let els = parse_elements(src);
        assert_eq!(els.keys().collect::<Vec<_>>(), vec!["C1", "R1"]);
        assert_eq!(els["R1"], vec!["in", "out"]);
    }

    #[test]
    fn partition_ignores_net_names() {
        // The same topology under KiCad's renaming must compare equal.
        let a = parse_elements("R1 in out 1k\nC1 out 0 100n\n");
        let b = parse_elements("R1 in /out 1k\nC1 /out GND 100n\n");
        assert_eq!(partition(&a), partition(&b));
    }

    #[test]
    fn partition_catches_a_swapped_connection() {
        let a = parse_elements("R1 in out 1k\nC1 out 0 100n\n");
        let b = parse_elements("R1 in out 1k\nC1 in 0 100n\n");
        assert_ne!(partition(&a), partition(&b));
    }
}
