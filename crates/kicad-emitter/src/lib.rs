//! Emit KiCad outputs from upstream pipeline products.
//!
//! Two targets:
//! - [`netlist`]: KiCad `.net` (logical netlist, no geometry) from a
//!   parsed [`Netlist`].
//! - [`schematic`]: KiCad `.kicad_sch` from a [`spice_layout::Placement`]
//!   plus a resolved [`kicad_symbols::Library`].

pub mod connectivity;
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

    /// V11, Tier 0 — two or more pins belonging to *different* nets
    /// share a coordinate in the placement handed to the emitter. KiCad
    /// joins coincident pins unconditionally, so the schematic would be
    /// a different circuit from the source netlist.
    ///
    /// Fully subsumed by [`EmitError::NetPartition`] — pin-on-pin is a
    /// merge like any other — and kept anyway for one reason: it is
    /// raised *before* the router runs, so it names the two coincident
    /// pins and their nets rather than the component they end up in, and
    /// it does not spend a routing pass on a placement already known to
    /// be wrong. It is a fast, precise pre-flight, not a second
    /// correctness authority.
    #[error("V11 correctness invariant (Tier 0): {0}")]
    PinCoincidence(String),

    /// V11, Tier 0 — the emitted geometry does not reconstruct the
    /// source netlist's net partition: two source nets landed in one
    /// electrical component (a short), or one source net landed in
    /// several (an open).
    ///
    /// **This is the class check** (ADR-22), and it replaced two
    /// mechanism-specific ones. `V11Violation` recognised a merge by
    /// *string-matching* the router's `v11:` warning, and
    /// `DisconnectedNet` recognised a split by union-finding wires only.
    /// Naming mechanisms meant every other mechanism needed its own
    /// recogniser: `conflict:` had none and reached exit 0 for as long as
    /// nobody wrote one. This variant names the **consequence** instead,
    /// so it is blind to mechanism — a new way to fuse or sever a net is
    /// caught with no new code — and it is the check the CLI used to
    /// outsource to `kicad-cli`, which `--no-verify` disables and most
    /// machines lack.
    #[error("V11 correctness invariant (Tier 0): {0}")]
    NetPartition(String),
}

pub fn emit_netlist(netlist: &Netlist) -> Result<String, EmitError> {
    netlist::emit(netlist)
}

pub fn emit_schematic(placement: &Placement, library: &Library) -> Result<String, EmitError> {
    schematic::emit(placement, library)
}

pub use refine::refine_orientations;
pub use schematic::{ChildSheet, PageShift, SheetBlock, SheetPort, emit_child_sheet, emit_root};
