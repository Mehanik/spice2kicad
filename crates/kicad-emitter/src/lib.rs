//! Emit KiCad outputs from upstream pipeline products.
//!
//! Two targets:
//! - [`netlist`]: KiCad `.net` (logical netlist, no geometry) from a
//!   parsed [`Netlist`].
//! - [`schematic`]: KiCad `.kicad_sch` from a [`spice_layout::Placement`]
//!   plus a resolved [`kicad_symbols::Library`].

pub mod mapping;
pub mod netlist;
pub mod refine;
pub mod schematic;
pub mod sexpr;
pub mod v5;

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

    /// V11 — one or more routed nets still touch a pin owned by a
    /// different net after the active rerouter ran. KiCad's
    /// connectivity engine silently shorts those nets on schematic
    /// load (a wrong netlist on export), so the emitter refuses to
    /// produce a `.kicad_sch` it knows is electrically incorrect.
    /// The string holds the concatenated `v11:` diagnostics from
    /// `spice_route::route` so callers can show the user which nets
    /// are affected. The single non-router-fixable case
    /// (`opamp_inverting_real`'s placer-level pin overlap) does not
    /// emit a `v11:` warning — the router does not generate a
    /// detour for it — so this error path never fires there.
    #[error("V11 correctness invariant: {0}")]
    V11Violation(String),

    /// V11, Tier 0 — two or more pins belonging to *different* nets
    /// share a coordinate in the placement handed to the emitter. KiCad
    /// joins coincident pins unconditionally, so the schematic would be
    /// a different circuit from the source netlist.
    ///
    /// Unlike [`EmitError::V11Violation`] (unresolved `v11:` *wire*
    /// residue, which the router can often detour and which is therefore
    /// gated behind `SPICE2KICAD_V11_STRICT`), this one is
    /// unconditional: no routing pass can move a pin, so there is no
    /// downstream stage that could repair it and nothing to trade.
    #[error("V11 correctness invariant (Tier 0): {0}")]
    PinCoincidence(String),

    /// Tier 0 — the emitted wires leave at least one net's pins in two
    /// or more electrically separate islands. The file is well-formed
    /// and opens fine; the circuit is simply not the source circuit.
    ///
    /// Previously this condition only printed a line on stderr and let
    /// the emit succeed, which meant a severed net shipped silently
    /// unless the CLI's optional `kicad-cli` connectivity check happened
    /// to be available *and* to agree. It is now a refusal.
    #[error("Tier-0 connectivity: {0}")]
    DisconnectedNet(String),
}

pub fn emit_netlist(netlist: &Netlist) -> Result<String, EmitError> {
    netlist::emit(netlist)
}

pub fn emit_schematic(placement: &Placement, library: &Library) -> Result<String, EmitError> {
    schematic::emit(placement, library)
}

pub use refine::refine_orientations;
pub use schematic::{ChildSheet, PageShift, SheetBlock, SheetPort, emit_child_sheet, emit_root};
