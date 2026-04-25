//! Emit KiCad outputs from a parsed SPICE [`Netlist`].
//!
//! Two targets:
//! - [`netlist`]: KiCad `.net` (logical netlist, no geometry).
//! - [`schematic`]: KiCad `.kicad_sch` (auto-placed schematic).

pub mod mapping;
pub mod netlist;
pub mod schematic;
pub mod sexpr;

use spice_parser::Netlist;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EmitError {
    #[error("no symbol mapping for SPICE element kind {0:?}")]
    UnmappedElement(spice_parser::ast::ElementKind),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn emit_netlist(netlist: &Netlist) -> Result<String, EmitError> {
    netlist::emit(netlist)
}

pub fn emit_schematic(netlist: &Netlist) -> Result<String, EmitError> {
    schematic::emit(netlist)
}
