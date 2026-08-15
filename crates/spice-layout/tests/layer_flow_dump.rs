//! Layer-assignment dump: every fixture, both root policies.
//!
//! `#[ignore]`d — this is an *instrument*, not a gate. Run it with
//!
//! ```sh
//! cargo test -p spice-layout --test layer_flow_dump -- --ignored --nocapture
//! ```
//!
//! ## What it shows
//!
//! The champion's `layers::no_source_fallback` roots the BFS at
//! `input_root(i) || touches_power(i)`, so **every rail-touching stub is
//! a layer-0 root** and the X "layer" measures hops from the nearest
//! power rail rather than depth along the signal path. That functional
//! saturates at ~2 in any biased amplifier no matter how many stages it
//! has. `--placer=flow-seed` (ADR-23 challenger) roots at signal-flow
//! sources only and demotes rail stubs to followers.
//!
//! The **torn-net** column is the falsifiable summary: a Signal net whose
//! members span more than one layer has been pulled apart across columns,
//! which is precisely what a wire then has to cross the sheet to rejoin.
//! A flow-faithful skeleton should have none, except where the circuit
//! itself has genuine feedback (Sallen-Key, an LC ladder's shunt arms).
//!
//! It also enumerates the **rootless** fixtures — the ones with no
//! signal-flow root at all, which the challenger must hand back to the
//! champion's rail-rooted policy rather than collapsing to one column.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kicad_symbols::Library;
use spice_layout::Placer;
use spice_layout::layers::assign_x_layers_with;
use spice_layout::net_class::{NetClass, classify_nets};

fn library() -> Library {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("crates/kicad-symbols/tests/fixtures");
    let mut lib = Library::default();
    for f in [
        "Device.kicad_sym",
        "Simulation_SPICE.kicad_sym",
        "Amplifier_Operational.kicad_sym",
        "power.kicad_sym",
    ] {
        lib = lib.merge(Library::from_file(dir.join(f)).expect("load fixture library"));
    }
    lib
}

struct Dump {
    fallback: bool,
    layers: Vec<u32>,
    order: Vec<u32>,
    refdes: Vec<String>,
    /// Layer indices per Signal net, for the torn-net report.
    per_net: BTreeMap<String, Vec<u32>>,
}

fn dump(path: &Path, lib: &Library, placer: Placer) -> Dump {
    let src = std::fs::read_to_string(path).expect("read fixture");
    let parsed = spice_parser::parse(&src, spice_diagnostics::FileId(0))
        .expect("parse")
        .netlist;
    let resolved = spice_resolve::resolve(&parsed, lib).expect("resolve");
    let (checked, _w) = spice_policy::check(resolved).expect("policy check");
    let classes = classify_nets(&checked);
    let asg = assign_x_layers_with(&checked, &classes, placer);
    let mut per_net: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (i, el) in checked.elements.iter().enumerate() {
        for net in &el.nodes {
            if classes
                .get(net.as_str())
                .copied()
                .unwrap_or(NetClass::Signal)
                == NetClass::Signal
            {
                per_net.entry(net.clone()).or_default().push(asg.layers[i]);
            }
        }
    }
    Dump {
        fallback: asg.no_source_fallback,
        layers: asg.layers.clone(),
        order: asg.order_key.clone(),
        refdes: checked.elements.iter().map(|e| e.refdes.clone()).collect(),
        per_net,
    }
}

fn columns(d: &Dump) -> BTreeMap<u32, Vec<String>> {
    let mut m: BTreeMap<u32, Vec<(u32, String)>> = BTreeMap::new();
    for i in 0..d.refdes.len() {
        m.entry(d.layers[i])
            .or_default()
            .push((d.order[i], d.refdes[i].clone()));
    }
    m.into_iter()
        .map(|(k, mut v)| {
            v.sort();
            (k, v.into_iter().map(|(_, r)| r).collect())
        })
        .collect()
}

fn torn(d: &Dump) -> Vec<(String, u32)> {
    d.per_net
        .iter()
        .filter(|(_, ls)| ls.len() >= 2)
        .filter_map(|(net, ls)| {
            let span = ls.iter().max().unwrap() - ls.iter().min().unwrap();
            (span > 1).then(|| (net.clone(), span))
        })
        .collect()
}

#[test]
#[ignore = "instrument, not a gate: run with --ignored --nocapture"]
fn layer_assignment_under_both_root_policies() {
    let lib = library();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("crates/spice2kicad/tests/fixtures");
    let mut fixtures: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("fixture dir")
        .filter_map(|e| {
            let p = e.expect("dir entry").path();
            (p.extension().is_some_and(|x| x == "cir")).then_some(p)
        })
        .collect();
    fixtures.sort();
    println!("{} fixtures\n", fixtures.len());

    let mut principled = Vec::new();
    let mut rootless = Vec::new();
    let mut active = Vec::new();
    let mut summary = Vec::new();

    for f in &fixtures {
        let name = f.file_stem().expect("stem").to_string_lossy().to_string();
        let champ = dump(f, &lib, Placer::Champion);
        let flow = dump(f, &lib, Placer::FlowSeed);
        let identical = champ.layers == flow.layers && champ.order == flow.order;
        if champ.fallback {
            if identical {
                rootless.push(name.clone());
            } else {
                active.push(name.clone());
            }
        } else {
            principled.push(name.clone());
        }

        let (ct, ft) = (torn(&champ), torn(&flow));
        summary.push((
            name.clone(),
            champ.layers.iter().copied().max().unwrap_or(0),
            flow.layers.iter().copied().max().unwrap_or(0),
            ct.len(),
            ft.len(),
        ));

        println!(
            "=== {name}  (no_source_fallback={}, {}) ===",
            champ.fallback,
            if identical { "IDENTICAL" } else { "differs" }
        );
        let (cc, fc) = (columns(&champ), columns(&flow));
        let maxl = *cc
            .keys()
            .max()
            .unwrap_or(&0)
            .max(fc.keys().max().unwrap_or(&0));
        println!("  {:<4} {:<44} flow-seed", "col", "champion");
        for l in 0..=maxl {
            let a = cc.get(&l).map(|v| v.join(" ")).unwrap_or_default();
            let b = fc.get(&l).map(|v| v.join(" ")).unwrap_or_default();
            if !a.is_empty() || !b.is_empty() {
                println!("  {l:<4} {a:<44} {b}");
            }
        }
        println!("  torn signal nets (span>1): champion {ct:?}  flow {ft:?}\n");
    }

    println!("### summary");
    println!(
        "{:<26} {:>7} {:>7} {:>7} {:>7}",
        "fixture", "cDepth", "fDepth", "cTorn", "fTorn"
    );
    for (n, cd, fd, ct, ft) in &summary {
        println!("{n:<26} {cd:>7} {fd:>7} {ct:>7} {ft:>7}");
    }
    println!("\nprincipled path (drawn source; flow-seed inert): {principled:?}");
    println!("rootless (flow-seed hands back to the champion policy): {rootless:?}");
    println!("flow-seed ACTIVE on: {active:?}");
}

/// The rootless set is the fallback's reason to exist: a circuit with no
/// signal-flow root at all must not collapse to a single column. This is
/// a real assertion (it runs in the default `cargo test` path), unlike
/// the dump above.
#[test]
fn rootless_fixtures_are_byte_identical_under_flow_seed() {
    let lib = library();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("crates/spice2kicad/tests/fixtures");
    // Enumerated by `layer_assignment_under_both_root_policies`: a pure
    // cross-coupled pair, a differential pair biased from the rails, and
    // a Wien-bridge oscillator — none has an input net at all.
    for name in ["diff_pair", "multivibrator", "wien_bridge_osc"] {
        let f = dir.join(format!("{name}.cir"));
        let champ = dump(&f, &lib, Placer::Champion);
        let flow = dump(&f, &lib, Placer::FlowSeed);
        assert_eq!(
            champ.layers, flow.layers,
            "{name}: rootless fixture must fall back to the champion layering"
        );
        assert_eq!(
            champ.order, flow.order,
            "{name}: rootless fixture must keep the champion within-bucket order"
        );
    }
}

/// The property the challenger exists to buy: on a two-stage amplifier
/// the layer must count stages, not rail hops. The champion gives the
/// chain `in→b1→c1→b2→c2→out` the layers `{0,1,1,1,3}` — Q1, the
/// coupling cap and Q2 in ONE column.
#[test]
fn two_stage_amp_layers_are_strictly_staged_under_flow_seed() {
    let lib = library();
    let f = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("crates/spice2kicad/tests/fixtures/two_stage_amp.cir");
    let flow = dump(&f, &lib, Placer::FlowSeed);
    let layer = |r: &str| -> u32 {
        let i = flow
            .refdes
            .iter()
            .position(|x| x == r)
            .unwrap_or_else(|| panic!("{r} not in netlist"));
        flow.layers[i]
    };
    for (a, b) in [("CIN", "Q1"), ("Q1", "CC"), ("CC", "Q2"), ("Q2", "COUT")] {
        assert!(
            layer(a) < layer(b),
            "flow-seed: {a} (layer {}) must precede {b} (layer {})",
            layer(a),
            layer(b)
        );
    }
    // The rail stubs sit in their stage's column, not at layer 0.
    assert_eq!(layer("RC1"), layer("Q1"), "RC1 belongs in Q1's column");
    assert_eq!(layer("RE1"), layer("Q1"), "RE1 belongs in Q1's column");
    assert_eq!(layer("RB3"), layer("CC"), "RB3 belongs in CC's column");
    assert_eq!(layer("RC2"), layer("Q2"), "RC2 belongs in Q2's column");
}
