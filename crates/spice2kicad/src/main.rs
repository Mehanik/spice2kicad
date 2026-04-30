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

    /// Run the stage-3 force-directed + simulated-annealing refinement
    /// after the deterministic seed placer. Schematic target only.
    #[arg(long)]
    refine: bool,
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
    let top_resolved = spice_resolve::ResolvedNetlist {
        elements: resolved.elements,
        align: resolved.align,
        place: resolved.place,
        subckts: top_subckts.clone(),
        sheet_instances: Vec::new(),
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
        refine: cli.refine,
        ..LayoutOptions::default()
    };
    let placement = match spice_layout::place_with(checked, &library, &opts) {
        Ok(p) => p,
        Err(diags) => {
            surface_diags(&diags, sources);
            std::process::exit(1);
        }
    };

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
            })
        })
        .collect();

    let rendered = kicad_emitter::emit_root(&placement, &library, &sheet_blocks)?;

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
        let body = kicad_emitter::emit_child_sheet(&child, &library)?;
        let path = parent_dir.join(format!("{name}.kicad_sch"));
        fs::write(&path, &body).with_context(|| format!("writing {}", path.display()))?;
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
