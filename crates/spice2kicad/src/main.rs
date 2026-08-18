use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, ValueEnum};
use kicad_symbols::Library;
use spice_diagnostics::{Diagnostic, Severity};
use spice_layout::LayoutOptions;

mod render;
mod verify;

use render::SourceMap;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Target {
    /// KiCad flat netlist (.net)
    Netlist,
    /// KiCad schematic (.kicad_sch)
    Schematic,
}

#[derive(Parser, Debug)]
#[command(
    name = "spice2kicad",
    version,
    about = "Convert SPICE netlists to KiCad"
)]
struct Cli {
    /// Input SPICE file
    input: PathBuf,

    /// Output file (defaults to stdout)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Output target
    #[arg(short, long, value_enum, default_value_t = Target::Schematic)]
    target: Target,

    /// KiCad symbol library file (`.kicad_sym`). May be passed multiple
    /// times; later libraries override earlier ones on `lib_id` collision.
    /// Required for the schematic target.
    #[arg(short = 'l', long = "lib")]
    libs: Vec<PathBuf>,

    /// Skip the stage-3 force-directed + simulated-annealing
    /// refinement after the deterministic seed placer (default is to
    /// run it). Schematic target only.
    #[arg(long)]
    no_refine: bool,

    /// Iteration cap for the SA refiner (default 200).
    #[arg(long)]
    refine_iterations: Option<u32>,

    /// Which registered placement engine to run (ADR-23).
    ///
    /// `flow-seed` (the default since the ADR-23 promotion) is the
    /// shipping placer — omitting the flag changes nothing. `champion`
    /// is the retained **control arm**, the placer that shipped before
    /// the promotion, kept runnable for A/B. Every other name is a
    /// *challenger*: an alternative registered so the champion/
    /// challenger scoreboard can grade it end-to-end against the same
    /// verifiers. A non-default placer is not a supported output mode
    /// and is not a licence to bypass a ratchet; see
    /// `docs/layout-adr.md` ADR-23.
    #[arg(long, default_value = "flow-seed", value_parser = parse_placer)]
    placer: spice_layout::Placer,

    /// Disable the position-stability layout cache (ADR-4). By default
    /// the converter reads `<basename>.layout.json` next to the output
    /// `.kicad_sch` (if present) to keep untouched parts in place across
    /// re-conversions, and rewrites it on every run. Pass this flag to
    /// ignore and not write that cache — every run then re-anneals from
    /// scratch. Schematic target with an `--output` path only.
    #[arg(long)]
    no_layout_cache: bool,

    /// Skip the post-emit `kicad-cli` connectivity check.
    ///
    /// By default the converter re-reads the schematic it just wrote
    /// through `kicad-cli` and confirms every net connects the same pins
    /// the SPICE source did. This flag skips *that* step, and only that
    /// step.
    ///
    /// It does NOT disable the converter's own net-partition check: the
    /// emitter reconstructs the whole net partition from the geometry it
    /// is about to write and refuses, always, if two nets merged or one
    /// came apart (ADR-22). That check needs no external tool, runs
    /// before any bytes reach disk, and cannot be turned off — a Tier-0
    /// refusal has no opt-out.
    ///
    /// What `kicad-cli` still adds is an *independent* opinion: it is
    /// KiCad's own connectivity engine rather than our model of it, so it
    /// is the only thing that can catch the model itself being wrong.
    /// Turn it off when `kicad-cli` is unavailable or the cost matters.
    #[arg(long)]
    no_verify: bool,
}

/// `--placer` value parser: resolve a name against the registry, and
/// list the registered names on a miss rather than failing silently.
fn parse_placer(name: &str) -> Result<spice_layout::Placer, String> {
    spice_layout::Placer::from_name(name).ok_or_else(|| {
        format!(
            "unknown placer `{name}`; registered placers: {}",
            spice_layout::Placer::known_names()
        )
    })
}

fn load_library(paths: &[PathBuf]) -> Result<Library> {
    if paths.is_empty() {
        return Err(anyhow!(
            "the schematic target requires at least one --lib <FILE.kicad_sym>"
        ));
    }
    let mut lib = Library::default();
    for p in paths {
        let part = Library::from_file(p).with_context(|| format!("loading {}", p.display()))?;
        lib = lib.merge(part);
    }
    Ok(lib)
}

/// Render diagnostics to stderr and exit non-zero if any are errors.
/// Returns true when execution should continue (no fatal diags).
fn surface_diags(diags: &[Diagnostic], sources: &SourceMap) -> bool {
    if diags.is_empty() {
        return true;
    }
    let stderr = io::stderr();
    let mut handle = stderr.lock();
    let _ = render::render_all(diags, sources, &mut handle);
    let _ = handle.flush();
    !diags.iter().any(|d| d.severity == Severity::Error)
}

fn run(cli: &Cli) -> Result<()> {
    let source = fs::read_to_string(&cli.input)
        .with_context(|| format!("reading {}", cli.input.display()))?;

    let mut sources = SourceMap::new();
    let file_id = sources.add(cli.input.clone(), source.clone());

    let netlist = match spice_parser::parse(&source, file_id) {
        Ok(outcome) => {
            if !surface_diags(&outcome.diagnostics, &sources) {
                std::process::exit(1);
            }
            outcome.netlist
        }
        Err(diags) => {
            surface_diags(&diags, &sources);
            std::process::exit(1);
        }
    };

    // Annotation-spec version handshake (spec §4.7). Absent `*@spec`
    // → assume current, no diagnostic. An unsupported declared version
    // is a hard error (E911) before any resolve/layout work.
    let version_diags = spice_parser::check_spec_version(&netlist);
    if !surface_diags(&version_diags, &sources) {
        std::process::exit(1);
    }

    match cli.target {
        Target::Netlist => {
            let rendered = kicad_emitter::emit_netlist(&netlist)?;
            write_or_stdout(cli.output.as_deref(), &rendered)?;
        }
        Target::Schematic => {
            emit_schematic_target(cli, &netlist, &sources)?;
        }
    }
    Ok(())
}

fn write_or_stdout(out: Option<&std::path::Path>, body: &str) -> Result<()> {
    match out {
        Some(path) => {
            fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
        }
        None => print!("{body}"),
    }
    Ok(())
}

// Flat orchestration: parse → resolve → policy → layout → emit, with
// the same dance repeated once for each child subckt body. Splitting
// it into helpers obscures the shared library / options / sources
// arguments more than it clarifies; allow the long body.
#[allow(clippy::too_many_lines)]
fn emit_schematic_target(
    cli: &Cli,
    netlist: &spice_parser::Netlist,
    sources: &SourceMap,
) -> Result<()> {
    let library = load_library(&cli.libs)?;

    let resolved = match spice_resolve::resolve(netlist, &library) {
        Ok(r) => r,
        Err(diags) => {
            surface_diags(&diags, sources);
            std::process::exit(1);
        }
    };

    // Pull out the sheet structure before policy/layout since the
    // top-level placer only consumes top-level elements + their
    // align/place. Subckt bodies are placed independently.
    let top_subckts = resolved.subckts.clone();
    let top_sheet_instances = resolved.sheet_instances.clone();
    // Top-level `*@port` terminals: cloned for the emitter (directional
    // global_label) and moved into the placer input (left/right X-layer
    // bias). Empty when the file declares no `*@port`.
    let top_ports = resolved.ports.clone();
    let top_resolved = spice_resolve::ResolvedNetlist {
        elements: resolved.elements,
        align: resolved.align,
        place: resolved.place,
        ports: resolved.ports,
        subckts: top_subckts.clone(),
        // Carry the sheet instances through to placement. The top-level
        // element placer ignores them for positioning (sheets are placed
        // separately by `place_sheets`), but the idiom detector reads
        // their port nets so a tap wired into a `.subckt` instance port
        // is counted as a real consumer — otherwise a two-resistor
        // network feeding an opamp sheet (the `opamp_inverting` `inv`
        // net) is misdetected as a bare resistor divider.
        sheet_instances: top_sheet_instances.clone(),
    };

    let (checked, warnings) = match spice_policy::check(top_resolved) {
        Ok(ok) => ok,
        Err(diags) => {
            surface_diags(&diags, sources);
            std::process::exit(1);
        }
    };
    if !surface_diags(&warnings, sources) {
        std::process::exit(1);
    }

    let opts = LayoutOptions {
        refine: !cli.no_refine,
        refine_iterations: cli.refine_iterations.unwrap_or(200),
        placer: cli.placer,
        ..LayoutOptions::default()
    };
    if cli.placer != spice_layout::Placer::default() {
        eprintln!(
            "spice2kicad: placer `{}` — {} (NOT the shipping placer; ADR-23 non-default)",
            cli.placer.name(),
            cli.placer.description()
        );
    }

    // Position-stability sidecar (ADR-4): when the cache is enabled and
    // an output path is known, load `<basename>.layout.json` (if present)
    // as a placement hint so untouched elements keep their saved
    // position across re-conversions. This is a tool-owned position
    // CACHE, not a user-annotation carrier (see ADR-4 / sidecar.rs).
    let sidecar_path = (!cli.no_layout_cache)
        .then_some(cli.output.as_deref())
        .flatten()
        .map(spice_layout::sidecar::sidecar_path_for);
    // A cache hit must prove it came from THIS netlist. The sidecar is
    // keyed by output path, so without this check converting a second
    // netlist to a path a first one used inherits its positions — the
    // hint drags shared refdes to coordinates chosen for a different
    // circuit, which measurably left a net disconnected
    // (`opamp_definition_level` net `out1`, after `opamp_inverting` wrote
    // the same path). Identity is the source path, so *editing* a netlist
    // still hits — which is the whole point of the cache. A mismatch is
    // an ordinary cache miss, never an error.
    let expected_circuit = spice_layout::sidecar::source_id(&cli.input);
    let cached_sidecar = sidecar_path
        .as_deref()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|text| spice_layout::sidecar::Sidecar::from_json(&text))
        .filter(|s| s.circuit == expected_circuit);
    let hint = cached_sidecar
        .as_ref()
        .map(spice_layout::sidecar::Sidecar::to_hint)
        .unwrap_or_default();
    // Announce a cache HIT at default verbosity. A hit pins every cached
    // element, which makes the placer's SA refinement and the phase-4.5
    // routing-aware orientation pass silent no-ops — so a second
    // conversion into a directory a previous run wrote is NOT a fresh
    // conversion, however much it looks like one. That silence has
    // already sent one debugging session down a wrong path (every
    // element reported `pinned=true`, attributed to a phase-4.5 defect).
    // A behaviour-changing no-op should never be invisible; a single
    // line on a hit is cheap and never fires on a fresh output dir.
    if !hint.pins.is_empty() {
        eprintln!(
            "spice2kicad: layout cache hit ({} element(s) pinned from {}) \
             — placement refinement is a no-op for them; \
             use --no-layout-cache or a fresh output directory to re-place from scratch",
            hint.pins.len(),
            sidecar_path
                .as_deref()
                .map_or_else(|| "cache".into(), |p| p.display().to_string()),
        );
    }
    // V15 page shifts applied last run, replayed so the page frame stays
    // put when the netlist is edited. Caching positions alone is not
    // enough: the emitter's final page translation is recomputed from the
    // content bounding box, so a newly added element can re-anchor the
    // frame and pan every *existing* symbol uniformly. The emitter keeps
    // a replayed shift only while it remains V15-conformant, and
    // re-normalises otherwise. See `sidecar::PageShiftEntry`.
    let cached_shift = |sheet: &str| -> Option<kicad_emitter::PageShift> {
        cached_sidecar
            .as_ref()?
            .page_shifts
            .get(sheet)
            .map(|e| kicad_emitter::PageShift {
                cells_x: e.cells_x,
                cells_y: e.cells_y,
            })
    };

    // Keep a copy of the checked netlist for structural sheet placement
    // (`place_sheets` needs net classification); `place_with_hint`
    // consumes the original by value.
    let checked_for_sheets = checked.clone();
    // `refinement_meta` recomputes the placer's pinned/allowed state for
    // the routing-aware orientation-refinement phase below; it needs the
    // same `CheckedNetlist`, so compute it before `place_with_hint`
    // consumes `checked` by value.
    let refine_meta = match spice_layout::refinement_meta(&checked, &hint, opts.placer) {
        Ok(m) => m,
        Err(diags) => {
            surface_diags(&diags, sources);
            std::process::exit(1);
        }
    };
    let mut placement = match spice_layout::place_with_hint(checked, &library, &opts, &hint) {
        Ok(p) => p,
        Err(diags) => {
            surface_diags(&diags, sources);
            std::process::exit(1);
        }
    };

    // Layout phase 4.5 — routing-aware orientation refinement (CLAUDE.md
    // "Layout phases"). Runs AFTER placement and BEFORE the emitter's
    // route_nets/decoration: trial-routes V14-allowed candidate
    // orientations with the *real* router and selects the one minimising
    // the router's measured first-segment-outward (V5) violations,
    // without regressing V11/V12/symbol-overlap. It changes orientation
    // only (never position); decoration remains a strict consumer. Skip
    // when refinement is disabled (`--no-refine`), keeping the un-refined
    // SA path unchanged.
    if opts.refine {
        kicad_emitter::refine_orientations(&mut placement, &library, &refine_meta);
    }

    // V6: position each hierarchical-sheet instance adjacent to the
    // circuitry it shares signal nets with, rather than at a fixed
    // off-circuit page coordinate. Returns refdes → world origin (mm).
    let sheet_origins: std::collections::HashMap<String, (f64, f64)> = spice_layout::place_sheets(
        &checked_for_sheets,
        &placement,
        &library,
        &top_sheet_instances,
    )
    .into_iter()
    .collect();

    // Snapshot the freshly-computed placement for the sidecar rewrite.
    // Removed refdeses simply do not appear in the new snapshot, so they
    // drop out of the cache (ADR-4 step 2). The file is written after
    // emission, once the applied V15 page shifts are known.
    let mut snapshot =
        spice_layout::sidecar::Sidecar::from_placement(&placement).with_source(&cli.input);

    // Place each subckt body on its own child sheet. Only emit children
    // for subckts that actually have an instance in this file.
    let mut child_placements: Vec<(String, spice_layout::Placement, Vec<String>)> = Vec::new();
    let used: std::collections::BTreeSet<&str> = top_sheet_instances
        .iter()
        .map(|s| s.subckt_name.as_str())
        .collect();
    for sc in &top_subckts {
        if !used.contains(sc.name.as_str()) {
            continue;
        }
        let body_resolved = spice_resolve::ResolvedNetlist {
            elements: sc.elements.clone(),
            ..spice_resolve::ResolvedNetlist::default()
        };
        let (body_checked, body_warns) = match spice_policy::check(body_resolved) {
            Ok(ok) => ok,
            Err(diags) => {
                surface_diags(&diags, sources);
                std::process::exit(1);
            }
        };
        if !surface_diags(&body_warns, sources) {
            std::process::exit(1);
        }
        let body_placement = match spice_layout::place_with(body_checked, &library, &opts) {
            Ok(p) => p,
            Err(diags) => {
                surface_diags(&diags, sources);
                std::process::exit(1);
            }
        };
        child_placements.push((sc.name.clone(), body_placement, sc.ports.clone()));
    }

    // Build sheet blocks for the parent. Map each X instance to its
    // child sheet file by subckt name.
    let sheet_blocks: Vec<kicad_emitter::SheetBlock> = top_sheet_instances
        .iter()
        .filter_map(|inst| {
            let sc = top_subckts.iter().find(|s| s.name == inst.subckt_name)?;
            // Pair each port with the SPICE net wired to the instance
            // at that positional index. If the user passed too few/many
            // nets we just zip the shorter list — diagnostic is a TODO.
            let ports: Vec<kicad_emitter::SheetPort> = sc
                .ports
                .iter()
                .zip(inst.nodes.iter())
                .map(|(p, n)| kicad_emitter::SheetPort {
                    name: p.clone(),
                    net: n.clone(),
                })
                .collect();
            Some(kicad_emitter::SheetBlock {
                refdes: inst.refdes.clone(),
                sheet_file: format!("{}.kicad_sch", inst.subckt_name),
                ports,
                origin: sheet_origins.get(&inst.refdes).copied(),
            })
        })
        .collect();

    let port_pairs: Vec<(String, spice_resolve::PortDir)> =
        top_ports.iter().map(|p| (p.net.clone(), p.dir)).collect();
    let (rendered, root_shift) = kicad_emitter::emit_root(
        &placement,
        &library,
        &sheet_blocks,
        &port_pairs,
        cached_shift(spice_layout::sidecar::ROOT_SHEET_KEY),
    )?;
    snapshot.page_shifts.insert(
        spice_layout::sidecar::ROOT_SHEET_KEY.to_string(),
        spice_layout::sidecar::PageShiftEntry {
            cells_x: root_shift.cells_x,
            cells_y: root_shift.cells_y,
        },
    );

    let Some(out_path) = cli.output.clone() else {
        // No output file: dump parent to stdout, drop children.
        print!("{rendered}");
        return Ok(());
    };
    fs::write(&out_path, &rendered).with_context(|| format!("writing {}", out_path.display()))?;

    // Children land alongside the parent sheet.
    let parent_dir = out_path.parent().map_or_else(
        || std::path::PathBuf::from("."),
        std::path::Path::to_path_buf,
    );
    for (name, body_placement, ports) in &child_placements {
        let instance_refdeses: Vec<String> = top_sheet_instances
            .iter()
            .filter(|inst| &inst.subckt_name == name)
            .map(|inst| inst.refdes.clone())
            .collect();
        let child = kicad_emitter::ChildSheet {
            name: name.clone(),
            placement: body_placement,
            ports: ports.clone(),
            instance_refdeses,
        };
        let (body, child_shift) =
            kicad_emitter::emit_child_sheet(&child, &library, cached_shift(name))?;
        snapshot.page_shifts.insert(
            name.clone(),
            spice_layout::sidecar::PageShiftEntry {
                cells_x: child_shift.cells_x,
                cells_y: child_shift.cells_y,
            },
        );
        let path = parent_dir.join(format!("{name}.kicad_sch"));
        fs::write(&path, &body).with_context(|| format!("writing {}", path.display()))?;
    }

    // Rewrite the layout cache now that every sheet's applied page shift
    // is known.
    if let Some(ref sc_path) = sidecar_path {
        fs::write(sc_path, snapshot.to_json())
            .with_context(|| format!("writing layout cache {}", sc_path.display()))?;
    }

    // Let KiCad judge what we wrote. Everything upstream reasons about a
    // *model* of KiCad; models drift, and when they do the file is
    // well-formed, opens fine, and the circuit is wrong. The written file
    // stays on disk either way — it is the artifact you debug with.
    if !cli.no_verify && verify::kicad_cli_available() {
        let expected: std::collections::BTreeMap<String, Vec<String>> = checked_for_sheets
            .elements
            .iter()
            .filter(|el| !matches!(el.role, spice_resolve::ElementRole::Power(_)))
            .map(|el| {
                (
                    el.refdes.to_ascii_uppercase(),
                    el.nodes.iter().map(|n| n.to_ascii_lowercase()).collect(),
                )
            })
            .collect();
        match verify::check_connectivity(&out_path, &expected) {
            Ok(report) if report.is_clean() => {}
            Ok(report) => {
                eprintln!(
                    "spice2kicad: ERROR: the emitted schematic does not wire up the                      source circuit."
                );
                for group in &report.missing {
                    eprintln!("  net in the source but split in the schematic: {group:?}");
                }
                for group in &report.extra {
                    eprintln!("  net in the schematic but not the source: {group:?}");
                }
                for refdes in &report.dropped {
                    eprintln!("  element missing from the schematic: {refdes}");
                }
                eprintln!(
                    "  This is a converter bug. {} was written and left in place for                      debugging; do not use it as a schematic.",
                    out_path.display()
                );
                return Err(anyhow!(
                    "emitted schematic does not match the source netlist"
                ));
            }
            Err(e) => {
                // A verification that cannot run is a warning, not a
                // failure: it says nothing about the schematic.
                eprintln!("spice2kicad: warning: could not verify connectivity: {e}");
            }
        }
    }

    Ok(())
}

fn main() -> ExitCode {
    env_logger::init();
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
