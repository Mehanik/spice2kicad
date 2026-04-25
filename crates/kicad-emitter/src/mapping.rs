//! SPICE element -> KiCad symbol mapping.

use spice_parser::ast::ElementKind;

#[derive(Debug, Clone, Copy)]
pub struct SymbolRef {
    pub library: &'static str,
    pub name: &'static str,
}

pub fn default_symbol(kind: ElementKind) -> Option<SymbolRef> {
    let (lib, name) = match kind {
        ElementKind::Resistor => ("Device", "R"),
        ElementKind::Capacitor => ("Device", "C"),
        ElementKind::Inductor => ("Device", "L"),
        ElementKind::Diode => ("Device", "D"),
        ElementKind::VoltageSrc => ("Simulation_SPICE", "VDC"),
        ElementKind::CurrentSrc => ("Simulation_SPICE", "IDC"),
        ElementKind::Bjt => ("Device", "Q_NPN_BCE"),
        ElementKind::Mosfet => ("Device", "Q_NMOS_GDS"),
        ElementKind::Jfet => ("Device", "Q_NJFET_GDS"),
        _ => return None,
    };
    Some(SymbolRef { library: lib, name })
}
