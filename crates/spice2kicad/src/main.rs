use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};

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
}

fn run(cli: &Cli) -> Result<()> {
    let source = fs::read_to_string(&cli.input)
        .with_context(|| format!("reading {}", cli.input.display()))?;

    let mut sources = SourceMap::new();
    let file_id = sources.add(cli.input.clone(), source.clone());

    let netlist = match spice_parser::parse(&source, file_id) {
        Ok(n) => n,
        Err(diags) => {
            let stderr = io::stderr();
            let mut handle = stderr.lock();
            // Best-effort render; if rendering fails we still want
            // to surface a non-zero exit, so swallow the io error.
            let _ = render::render_all(&diags, &sources, &mut handle);
            let _ = handle.flush();
            std::process::exit(1);
        }
    };

    let rendered = match cli.target {
        Target::Netlist => kicad_emitter::emit_netlist(&netlist)?,
        Target::Schematic => kicad_emitter::emit_schematic(&netlist)?,
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
