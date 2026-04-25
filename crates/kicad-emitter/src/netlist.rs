//! Emit a KiCad-style flat netlist (`.net`).

use crate::EmitError;
use spice_parser::Netlist;

pub fn emit(_netlist: &Netlist) -> Result<String, EmitError> {
    // TODO: walk netlist, emit (export (version "E") (components ...) (nets ...))
    Ok("(export (version \"E\"))\n".to_string())
}
