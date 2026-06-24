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
pub mod pwrflag;
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
    // Pre-build a quantised foreign-pin set per signal net so the
    // Steiner stage can avoid emitting an outward stub that would
    // land on a foreign pin (which the V11 detour cascade can rarely
    // recover from cleanly).
    #[allow(clippy::cast_possible_truncation)]
    let foreign_per_net: Vec<std::collections::HashSet<(i64, i64)>> = req
        .nets
        .iter()
        .filter(|n| matches!(n.class, NetClass::Signal))
        .map(|own| {
            let own_keys: std::collections::HashSet<(i64, i64)> = own
                .pins
                .iter()
                .map(|p| {
                    (
                        (p.x_mm * 1000.0).round() as i64,
                        (p.y_mm * 1000.0).round() as i64,
                    )
                })
                .collect();
            let mut acc: std::collections::HashSet<(i64, i64)> = std::collections::HashSet::new();
            for net in req.nets {
                for p in &net.pins {
                    let k = (
                        (p.x_mm * 1000.0).round() as i64,
                        (p.y_mm * 1000.0).round() as i64,
                    );
                    if own_keys.contains(&k) {
                        continue;
                    }
                    acc.insert(k);
                }
            }
            acc
        })
        .collect();
    let mut signal_idx = 0;
    for net in req.nets {
        if !matches!(net.class, NetClass::Signal) {
            continue;
        }
        // Stage 2 emits the Hwang/MST tree. The V11/V12 enforcement
        // at Stages 3c / 3d (`conflict::avoid_foreign_pins`,
        // `avoid_obstacles`) rolls back detours that would collinearly
        // overlap a sibling routed net. A conflict-aware constructor
        // that subsumes both stages is a v0.2 channel-router work
        // item.
        let (segs, junctions) = steiner::route_signal(net, &foreign_per_net[signal_idx]);
        signal_idx += 1;
        routed.push(RoutedNet {
            segments: segs,
            junctions,
        });
    }
    let _ = out;
    routed
}

/// Pre-compute, in routed-net (signal-only) order, the set of pin
/// coordinates owned by *any other* net (signal, power, or ground)
/// that the corresponding Steiner tree must avoid. Coordinates are
/// quantised to 1 µm via `(x*1000.0).round() as i64`, matching the
/// router-internal `qk` helper.
#[allow(clippy::cast_possible_truncation)]
fn foreign_pin_sets(req: &RouteRequest<'_>) -> Vec<std::collections::HashSet<(i64, i64)>> {
    let signal_nets: Vec<&NetSpec> = req
        .nets
        .iter()
        .filter(|n| matches!(n.class, NetClass::Signal))
        .collect();
    signal_nets
        .iter()
        .map(|own| {
            let own_keys: std::collections::HashSet<(i64, i64)> = own
                .pins
                .iter()
                .map(|p| {
                    (
                        (p.x_mm * 1000.0).round() as i64,
                        (p.y_mm * 1000.0).round() as i64,
                    )
                })
                .collect();
            let mut acc: std::collections::HashSet<(i64, i64)> = std::collections::HashSet::new();
            for net in req.nets {
                for p in &net.pins {
                    let k = (
                        (p.x_mm * 1000.0).round() as i64,
                        (p.y_mm * 1000.0).round() as i64,
                    );
                    if own_keys.contains(&k) {
                        continue;
                    }
                    acc.insert(k);
                }
            }
            acc
        })
        .collect()
}

/// Per-signal-net own-pin quantised coordinates used as no-coalesce
/// barriers by [`cleanup::coalesce_collinear`]. Distinct from
/// `(junction …)` markers — the cleanup pass treats these coords as
/// non-mergeable shared endpoints without producing extra junction
/// glyphs in the emitted schematic.
#[allow(clippy::cast_possible_truncation)]
fn build_signal_own_pin_coords(
    req: &RouteRequest<'_>,
) -> Vec<std::collections::HashSet<(i64, i64)>> {
    req.nets
        .iter()
        .filter(|n| matches!(n.class, NetClass::Signal))
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
        .collect()
}

/// Build a quantised pin-coord → outward-direction map across every
/// net in the request. The V11/V12 detour passes consult this map to
/// pick corner placements whose leg incident on a pin extends in the
/// pin's outward direction.
#[allow(clippy::cast_possible_truncation)]
fn build_pin_outward_map(
    req: &RouteRequest<'_>,
) -> std::collections::HashMap<(i64, i64), Direction> {
    let mut map: std::collections::HashMap<(i64, i64), Direction> =
        std::collections::HashMap::new();
    for net in req.nets {
        for p in &net.pins {
            let k = (
                (p.x_mm * 1000.0).round() as i64,
                (p.y_mm * 1000.0).round() as i64,
            );
            // Multiple pins on the same coord would already trip V11
            // ("pin overlap is a placer bug"); first writer wins is
            // fine here — the verifier reports the underlying overlap.
            map.entry(k).or_insert(p.outward);
        }
    }
    map
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
    // PWR_FLAG drivers for every net with no driving pin (rails whose
    // pins are all power_in, signal nets whose pins are all input).
    // Single structural predicate, no fixture knowledge — see
    // `pwrflag::emit`.
    let mut flg_counter: usize = 0;
    pwrflag::emit(
        req.nets,
        req.library,
        req.scope,
        req.sheet_uuid,
        req.project_name,
        &mut flg_counter,
        &mut out,
    );
    let mut routed = route_signal_nets(&req, &mut out);
    // Per-net own-pin coords for the cleanup pass below.
    let own_pin_coords_for_cleanup = build_signal_own_pin_coords(&req);
    // Stage 3 — resolve cross-net endpoint conflicts.
    // Build per-routed-net pin-coordinate sets. A conflict at a
    // coordinate that's a pin on net A but only a Steiner / wire
    // crossing on net B should be resolved by jogging B (not A);
    // jogging at A would silently disconnect A's pin.
    #[allow(clippy::cast_possible_truncation)]
    let net_pin_coords: Vec<std::collections::HashSet<(i64, i64)>> = req
        .nets
        .iter()
        .filter(|n| matches!(n.class, NetClass::Signal))
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
    // Stage 3c — V11 enforcement. **Correctness invariant**: wire
    // endpoints, wire interiors, and labels must not coincide with a
    // pin owned by a different net (KiCad's wire-touches-pin rule
    // silently merges those nets on export). Foreign-pin sets here
    // include Power/Ground pins too: routing through a ground pin
    // would silently merge the signal net into ground just as routing
    // through a foreign signal pin would.
    //
    // Runs *before* the V12 (symbol-body) pass: a V11 violation is a
    // wrong netlist, while a V12 violation is just ugly. If we have
    // to choose between the two, take the V12 hit. The rerouter
    // jogs offending segments perpendicular to the violating axis
    // and rolls the change back if it would collinearly overlap a
    // sibling routed net (the symmetric multivibrator failure
    // mode); residual cases drive the v0.2 channel-router work
    // item, with the V11 verifier in
    // `crates/spice2kicad/tests/electrical_safety.rs` holding the
    // budget as a high-water mark.
    let foreign_per_routed = foreign_pin_sets(&req);
    // Global pin-outward map: every routed-net pin's outward direction
    // keyed by its quantised world coord. Used by the V11/V12 detour
    // passes to prefer corner choices whose leg incident on a pin
    // extends in the pin's outward direction.
    let pin_outward = build_pin_outward_map(&req);
    // V11/V12 convergence loop. Each pass runs V11 (correctness) first
    // so a V12 detour can't re-introduce a foreign-pin coincidence,
    // then V12 (quality). Detours land in segment-set signatures that
    // the next V11 pass observes; we iterate until two consecutive
    // signatures agree or 3 passes elapse (a defensive cap — the v0.1
    // fixtures converge in ≤ 2).
    let mut accumulated_warnings: Vec<String> = Vec::new();
    for _ in 0..3 {
        let pre_signatures: Vec<Vec<Segment>> = routed.iter().map(|n| n.segments.clone()).collect();
        let w11 = conflict::avoid_foreign_pins(
            &mut routed,
            &foreign_per_routed,
            &net_pin_coords,
            req.obstacles,
            &pin_outward,
        );
        accumulated_warnings = w11;
        if !req.obstacles.is_empty() {
            let w12 = conflict::avoid_obstacles(
                &mut routed,
                req.obstacles,
                &net_pin_coords,
                &foreign_per_routed,
                req.bounds,
                &pin_outward,
            );
            accumulated_warnings.extend(w12);
        }
        let changed = pre_signatures
            .iter()
            .zip(routed.iter())
            .any(|(pre, now)| pre != &now.segments);
        if !changed {
            break;
        }
    }
    out.warnings.extend(accumulated_warnings);
    // Stage 4 — per-net coalesce of collinear segments + dedup of
    // coincident junctions across nets. The own-pin barrier set
    // prevents the cleanup pass from merging across a pin coord and
    // erasing the V5-aware outward stubs the Steiner stage emits.
    cleanup::drop_zero_length(&mut routed);
    cleanup::coalesce_collinear_with_barriers(&mut routed, &own_pin_coords_for_cleanup);
    cleanup::drop_zero_length(&mut routed);
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
