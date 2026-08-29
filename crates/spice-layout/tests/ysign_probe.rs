//! Pin-frame probe: what the placer's objective says about one fixture,
//! read in **both** pin frames (ADR-30 / ADR-31 instrument).
//!
//! `#[ignore]`d — an *instrument*, not a gate. Run with
//!
//! ```sh
//! cargo test -p spice-layout --test ysign_probe -- --ignored --nocapture
//! S2K_PROBE_FIXTURE=common_emitter S2K_PROBE_SEEDS=1,2,3 \
//!   cargo test -p spice-layout --test ysign_probe -- --ignored --nocapture
//! ```
//!
//! For each arm it prints the **seed** placement (SA off) and the
//! **final** placement (SA on), each element's pins in both frames, and
//! the full cost breakdown scored in both frames. Cross-scoring the same
//! geometry in both frames is the point: it separates "the layout moved"
//! from "the same layout is read differently", which is the one question
//! a single-frame dump cannot answer.
//!
//! What it established (ADR-31): on `named_rails` the corrected
//! (page-frame) objective scores the champion's own final layout
//! 150 353 and the challenger's 60 254 — so the corrected objective
//! genuinely *prefers* the layout that routes worse, and 96% of that
//! 90 099 gap is the single weight-200 `rail_direction` term.

use std::path::{Path, PathBuf};

use kicad_symbols::Library;
use spice_layout::{LayoutOptions, Placement, Placer, cost};
use spice_policy::CheckedNetlist;

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

fn fixture(stem: &str, lib: &Library) -> CheckedNetlist {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("crates/spice2kicad/tests/fixtures")
        .join(format!("{stem}.cir"));
    let src = std::fs::read_to_string(&path).expect("read fixture");
    let parsed = spice_parser::parse(&src, spice_diagnostics::FileId(0))
        .expect("parse")
        .netlist;
    let resolved = spice_resolve::resolve(&parsed, lib).expect("resolve");
    let (checked, _w) = spice_policy::check(resolved).expect("policy check");
    checked
}

fn place(
    checked: &CheckedNetlist,
    lib: &Library,
    placer: Placer,
    refine: bool,
    seed: Option<u64>,
) -> Placement {
    let base = LayoutOptions::default();
    let opts = LayoutOptions {
        refine,
        // Match the CLI, which passes 200 rather than the struct default.
        refine_iterations: 200,
        placer,
        seed: seed.unwrap_or(base.seed),
        ..base
    };
    spice_layout::place_with_hint(checked.clone(), lib, &opts, &spice_layout::Hint::default())
        .expect("place")
}

fn dump_placement(tag: &str, p: &Placement, checked: &CheckedNetlist, lib: &Library) {
    println!("  [{tag}]");
    let mut rows: Vec<String> = Vec::new();
    for (i, e) in p.elements.iter().enumerate() {
        let (ox, oy) = e.origin.to_mm();
        let Some(el) = checked.elements.get(i) else {
            continue;
        };
        let pins = |frame: Placer| {
            lib.lookup(&el.lib_id).map_or_else(String::new, |s| {
                e.world_pin_mm_in(s, frame)
                    .iter()
                    .map(|(n, x, y)| format!("{n}@({x:.2},{y:.2})"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
        };
        rows.push(format!(
            "    {:6} o=({ox:6.2},{oy:6.2}) rot={:3} mir={}  hist[{}] page[{}]",
            el.refdes,
            e.orientation.rotation.degrees(),
            u8::from(e.orientation.mirror_y),
            pins(Placer::default()),
            pins(Placer::YSign),
        ));
    }
    rows.sort();
    for r in rows {
        println!("{r}");
    }
}

fn dump_cost(tag: &str, p: &Placement, checked: &CheckedNetlist, lib: &Library) {
    let w = cost::CostWeights::DEFAULT;
    for frame in [Placer::default(), Placer::YSign] {
        let b = cost::breakdown_with(p, checked, lib, frame);
        println!(
            "    cost[{tag} | frame={}] total={:.2} hpwl={:.2} overlap={:.2} cross={:.1} \
             constr={:.3} rail_dir={:.2} sigflow={:.2} band_mis={:.2} soft_y={:.2} \
             layer_ord={:.2} netbbox={:.1} band_inv={:.2} rail_stub={:.2}",
            frame.name(),
            cost::total(&b, &w),
            b.hpwl,
            b.overlap,
            b.crossings,
            b.constraint_violation,
            b.rail_direction,
            b.signal_flow,
            b.band_misalignment,
            b.soft_y_residual,
            b.layer_order,
            b.net_bbox_crossings,
            b.band_inversion,
            b.rail_stub_alignment,
        );
    }
}

#[test]
#[ignore = "instrument, not a gate"]
fn ysign_probe() {
    let lib = library();
    let stem = std::env::var("S2K_PROBE_FIXTURE").unwrap_or_else(|_| "named_rails".to_string());
    let seeds: Vec<Option<u64>> = std::env::var("S2K_PROBE_SEEDS").map_or_else(
        |_| vec![None],
        |v| v.split(',').map(|s| s.trim().parse().ok()).collect(),
    );
    let checked = fixture(&stem, &lib);
    println!("=== {stem} ===");
    for placer in [Placer::default(), Placer::YSign] {
        println!("--- placer {} ---", placer.name());
        let seed_placement = place(&checked, &lib, placer, false, None);
        dump_placement("seed", &seed_placement, &checked, &lib);
        dump_cost("seed", &seed_placement, &checked, &lib);
        for s in &seeds {
            let tag = s.map_or_else(
                || "final(SA)".to_string(),
                |v| format!("final(SA,seed={v})"),
            );
            let fin = place(&checked, &lib, placer, true, *s);
            dump_placement(&tag, &fin, &checked, &lib);
            dump_cost(&tag, &fin, &checked, &lib);
        }
    }
}
