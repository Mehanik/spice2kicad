//! Emit KiCad outputs from upstream pipeline products.
//!
//! Two targets:
//! - [`netlist`]: KiCad `.net` (logical netlist, no geometry) from a
//!   parsed [`Netlist`].
//! - [`schematic`]: KiCad `.kicad_sch` from a [`spice_layout::Placement`]
//!   plus a resolved [`kicad_symbols::Library`].

pub mod mapping;
pub mod netlist;
pub mod schematic;
pub mod sexpr;

use kicad_symbols::Library;
use spice_layout::Placement;
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

pub fn emit_schematic(placement: &Placement, library: &Library) -> Result<String, EmitError> {
    schematic::emit(placement, library)
}
