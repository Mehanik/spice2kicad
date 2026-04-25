use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};

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

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    let source = fs::read_to_string(&cli.input)
        .with_context(|| format!("reading {}", cli.input.display()))?;

    let netlist = spice_parser::parse(&source).context("parsing SPICE source")?;

    let rendered = match cli.target {
        Target::Netlist => kicad_emitter::emit_netlist(&netlist)?,
        Target::Schematic => kicad_emitter::emit_schematic(&netlist)?,
    };

    match cli.output {
        Some(path) => {
            fs::write(&path, rendered).with_context(|| format!("writing {}", path.display()))?;
        }
        None => print!("{rendered}"),
    }
    Ok(())
}
