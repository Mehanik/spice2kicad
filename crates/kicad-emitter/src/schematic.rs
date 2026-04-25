//! Emit a KiCad schematic (`.kicad_sch`) with auto-placed symbols and wires.

use crate::EmitError;
use spice_parser::Netlist;

pub fn emit(_netlist: &Netlist) -> Result<String, EmitError> {
    // TODO: build symbol instances via mapping, place on a grid, route wires.
    Ok("(kicad_sch (version 20231120) (generator spice2kicad))\n".to_string())
}
