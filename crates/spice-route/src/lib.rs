//! Per-net router. Stages: power-symbol placement → RSMT → cleanup.
//!
//! Replaces the channel-and-trunk router previously embedded in
//! `kicad-emitter::route_nets`. This crate is the new home for
//! geometry-level routing decisions.
//!
//! See `docs/superpowers/plans/2026-05-05-wiring-redesign.md` for
//! the staged build-out. Stage 1 (power-symbol placement) is live;
//! Stages 2 / 3 / 4 land in subsequent tasks.

pub mod cleanup;
pub mod conflict;
pub mod rails;
mod steiner;
pub mod types;

use spice_layout::net_class::NetClass;
pub use steiner::{route_n_pin, route_three_pin, route_two_pin};
pub use types::{Bbox, Direction, NetSpec, PinRef, RouteRequest, RouteResult, RoutedNet, Segment};

/// Stage 1 entry point — append power-symbol (or fallback label)
/// S-exprs to `out` for every pin on a Power/Ground net in `req`.
///
/// Signal nets are ignored. Library lookup is best-effort: when the
/// chosen `lib_id` is missing, a `(global_label …)` is emitted in its
/// place and a warning is recorded on `out`.
pub fn place_power_symbols(req: &RouteRequest<'_>, out: &mut RouteResult) {
    let mut pwr_counter: usize = 0;
    for net in req.nets {
        match net.class {
            NetClass::Power | NetClass::Ground => {
                rails::emit(
                    net,
                    req.library,
                    req.sheet_uuid,
                    req.project_name,
                    &mut pwr_counter,
                    &mut out.sexprs,
                    &mut out.warnings,
                );
            }
            NetClass::Signal => {}
        }
    }
}

/// Stage 2 entry point — emit RSMT wires + junctions for every
/// Signal net in `req`. Power / Ground nets are skipped (Stage 1
/// owns those). Pin counts dispatch as N=2 (L-shape), N=3 (Hwang),
/// 4 ≤ N ≤ 9 (Hanan-grid + Borah-Owens-Irwin Steinerization),
/// N ≥ 10 (rectilinear MST, no Steiner refinement).
///
/// Returns the routed nets so downstream stages (conflict, cleanup)
/// can still operate on the structured `RoutedNet` form before final
/// serialisation to `out.sexprs`.
pub fn route_signal_nets(req: &RouteRequest<'_>, out: &mut RouteResult) -> Vec<RoutedNet> {
    let mut routed: Vec<RoutedNet> = Vec::new();
    for net in req.nets {
        if !matches!(net.class, NetClass::Signal) {
            continue;
        }
        let (segs, junctions) = steiner::route_signal(net);
        routed.push(RoutedNet {
            segments: segs,
            junctions,
        });
    }
    let _ = out;
    routed
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
    let mut routed = route_signal_nets(&req, &mut out);
    // Stage 3 — resolve cross-net endpoint conflicts.
    // Build per-routed-net pin-coordinate sets. A conflict at a
    // coordinate that's a pin on net A but only a Steiner / wire
    // crossing on net B should be resolved by jogging B (not A);
    // jogging at A would silently disconnect A's pin.
    let signal_nets: Vec<&NetSpec> = req
        .nets
        .iter()
        .filter(|n| matches!(n.class, NetClass::Signal))
        .collect();
    #[allow(clippy::cast_possible_truncation)]
    let net_pin_coords: Vec<std::collections::HashSet<(i64, i64)>> = signal_nets
        .iter()
        .map(|n| {
            n.pins
                .iter()
                .map(|p| {
                    (
                        (p.x_mm * 1000.0).round() as i64,
                        (p.y_mm * 1000.0).round() as i64,
                    )
                })
                .collect()
        })
        .collect();
    let warnings = conflict::resolve_conflicts(&mut routed, &net_pin_coords);
    out.warnings.extend(warnings);
    // Stage 3b — avoid wires crossing symbol bodies.
    if !req.obstacles.is_empty() {
        let warnings = conflict::avoid_obstacles(&mut routed, req.obstacles, &net_pin_coords);
        out.warnings.extend(warnings);
    }
    // Stage 4 — per-net coalesce of collinear segments + dedup of
    // coincident junctions across nets.
    cleanup::coalesce_collinear(&mut routed);
    let junctions = cleanup::dedup_junctions(&routed);
    // Serialise routed nets to s-exprs.
    for net in &routed {
        out.sexprs
            .extend(net.segments.iter().map(steiner::segment_to_sexpr));
    }
    out.sexprs
        .extend(junctions.into_iter().map(steiner::junction_sexpr));
    out
}
