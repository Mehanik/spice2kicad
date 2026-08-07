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

    /// V11, Tier 0 — one or more routed *wires* still touch a pin
    /// owned by a different net after the active rerouter ran to its
    /// fixed point. KiCad's connectivity engine silently joins those
    /// nets on schematic load (a wrong netlist on export), so the
    /// emitter refuses to produce a `.kicad_sch` it knows is
    /// electrically incorrect. The string holds the concatenated
    /// `v11:` diagnostics from `spice_route::route` so callers can
    /// show the user which nets are affected.
    ///
    /// **Unconditional** (ADR-21). This used to fire only when
    /// `SPICE2KICAD_V11_STRICT` was set, which meant `--no-verify` —
    /// and every machine lacking `kicad-cli` — shipped the short at
    /// exit 0. The env-gate is gone; there is no way to opt out of a
    /// Tier-0 refusal.
    #[error("V11 correctness invariant (Tier 0): {0}")]
    V11Violation(String),

    /// V11, Tier 0 — two or more pins belonging to *different* nets
    /// share a coordinate in the placement handed to the emitter. KiCad
    /// joins coincident pins unconditionally, so the schematic would be
    /// a different circuit from the source netlist.
    ///
    /// [`EmitError::V11Violation`] covers the sibling case (a routed
    /// *wire* left on a foreign pin) and is equally unconditional. The
    /// two differ only in what the placer would have to move to fix
    /// them: this one needs a symbol moved, that one needs a routable
    /// channel to exist.
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
