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
        Ok(n) => n,
        Err(diags) => {
            surface_diags(&diags, &sources);
            std::process::exit(1);
        }
    };

    let rendered = match cli.target {
        Target::Netlist => kicad_emitter::emit_netlist(&netlist)?,
        Target::Schematic => {
            let library = load_library(&cli.libs)?;

            let resolved = match spice_resolve::resolve(&netlist, &library) {
                Ok(r) => r,
                Err(diags) => {
                    surface_diags(&diags, &sources);
                    std::process::exit(1);
                }
            };

            let (checked, warnings) = match spice_policy::check(resolved) {
                Ok(ok) => ok,
                Err(diags) => {
                    surface_diags(&diags, &sources);
                    std::process::exit(1);
                }
            };
            if !surface_diags(&warnings, &sources) {
                std::process::exit(1);
            }

            let opts = LayoutOptions {
                refine: cli.refine,
                ..LayoutOptions::default()
            };
            let placement = match spice_layout::place_with(checked, &library, &opts) {
                Ok(p) => p,
                Err(diags) => {
                    surface_diags(&diags, &sources);
                    std::process::exit(1);
                }
            };

            kicad_emitter::emit_schematic(&placement, &library)?
        }
    };

    match &cli.output {
        Some(path) => {
            fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))?;
        }
        None => print!("{rendered}"),
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
