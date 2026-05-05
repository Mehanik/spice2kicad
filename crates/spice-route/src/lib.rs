//! Per-net router. Stages: power-symbol placement → RSMT → cleanup.
//!
//! Replaces the channel-and-trunk router previously embedded in
//! `kicad-emitter::route_nets`. This crate is the new home for
//! geometry-level routing decisions.
//!
//! See `docs/superpowers/plans/2026-05-05-wiring-redesign.md` for
//! the staged build-out. Stage 1 (power-symbol placement) is live;
//! Stages 2 / 3 / 4 land in subsequent tasks.

pub mod rails;
mod steiner;
pub mod types;

use spice_layout::net_class::NetClass;
pub use steiner::{route_three_pin, route_two_pin};
pub use types::{Direction, NetSpec, PinRef, RouteRequest, RouteResult, RoutedNet, Segment};

/// Stage 1 entry point — append power-symbol (or fallback label)
/// S-exprs to `out` for every pin on a Power/Ground net in `req`.
///
/// Signal nets are ignored. Library lookup is best-effort: when the
/// chosen `lib_id` is missing, a `(global_label …)` is emitted in its
/// place and a warning is recorded on `out`.
pub fn place_power_symbols(req: &RouteRequest<'_>, out: &mut RouteResult) {
    for net in req.nets {
        match net.class {
            NetClass::Power | NetClass::Ground => {
                rails::emit(net, req.library, &mut out.sexprs, &mut out.warnings);
            }
            NetClass::Signal => {}
        }
    }
}

/// Stage 2a entry point — emit RSMT wires + junctions for every
/// Signal net in `req`. Power / Ground nets are skipped (Stage 1
/// owns those). Nets with N ≥ 4 pins fall through to a stub
/// (Task 4 lands the small-N DP).
pub fn route_signal_nets(req: &RouteRequest<'_>, out: &mut RouteResult) {
    for net in req.nets {
        if !matches!(net.class, NetClass::Signal) {
            continue;
        }
        let (segs, junctions) = steiner::route_signal(net);
        out.sexprs
            .extend(segs.iter().map(steiner::segment_to_sexpr));
        out.sexprs
            .extend(junctions.into_iter().map(steiner::junction_sexpr));
    }
}

/// Route the supplied nets and return their wire / junction / symbol
/// S-expressions for splicing into the emitted schematic.
///
/// Stage skeleton (each stage filled in by a follow-up task):
///
/// 1. Power / Ground nets → `power:*` symbol per pin (no wires).
/// 2. Signal nets → per-net rectilinear Steiner minimum tree.
/// 3. Rip-up & retry on crossings (deferred — Task 6).
/// 4. Cleanup: coalesce collinear segments, dedup junctions.
#[allow(clippy::needless_pass_by_value)] // by-value signature is the public contract
pub fn route(req: RouteRequest<'_>) -> RouteResult {
    let mut out = RouteResult::default();
    place_power_symbols(&req, &mut out);
    route_signal_nets(&req, &mut out);
    // Stages 3–4 land in follow-up tasks.
    out
}
