//! Per-net router. Stages: power-symbol placement → RSMT → cleanup.
//!
//! Replaces the channel-and-trunk router previously embedded in
//! `kicad-emitter::route_nets`. This crate is the new home for
//! geometry-level routing decisions.
//!
//! See `docs/superpowers/plans/2026-05-05-wiring-redesign.md` for
//! the staged build-out. The current scaffold is Task 1 only — the
//! `route` entry point is wired but every stage is a stub.

pub mod types;

pub use types::{Direction, NetSpec, PinRef, RouteRequest, RouteResult, RoutedNet, Segment};

/// Route the supplied nets and return their wire / junction / symbol
/// S-expressions for splicing into the emitted schematic.
///
/// Stage skeleton (each stage filled in by a follow-up task):
///
/// 1. Power / Ground nets → `power:*` symbol per pin (no wires).
/// 2. Signal nets → per-net rectilinear Steiner minimum tree.
/// 3. Rip-up & retry on crossings (deferred — Task 6).
/// 4. Cleanup: coalesce collinear segments, dedup junctions.
#[allow(clippy::needless_pass_by_value)] // stub: subsequent tasks consume req
pub fn route(req: RouteRequest<'_>) -> RouteResult {
    let _ = req;
    RouteResult::default()
}
