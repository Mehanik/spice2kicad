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
/// Returns the final `#PWR<n>` counter so a later stage (the PWR_FLAG
/// corner driver block, which draws one more rail glyph per rail) can
/// keep allocating unique refdes from where Stage 1 left off.
pub fn place_power_symbols(req: &RouteRequest<'_>, out: &mut RouteResult) -> usize {
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
    pwr_counter
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

/// Stage 4 — normalise routed geometry into what KiCad will connect.
///
/// Coalesce collinear runs (the own-pin barrier set stops the merge
/// crossing a pin coord and erasing a V5 outward stub), collapse
/// redundant same-net overlaps, then split every interior attachment
/// into a real endpoint and re-add junction dots.
///
/// The split is correctness, not cosmetics, and must precede the
/// junction pass: KiCad connects wires only at endpoints, so a branch
/// left ending on a trunk's interior is a SPLIT NET however many dots
/// sit on it. The collapse is electrically inert (its union is a
/// pointwise subset of the members' span).
fn run_cleanup<S: ::std::hash::BuildHasher>(
    routed: &mut [RoutedNet],
    own_pin_coords: &[std::collections::HashSet<(i64, i64), S>],
) {
    cleanup::drop_zero_length(routed);
    cleanup::coalesce_collinear_with_barriers(routed, own_pin_coords);
    cleanup::collapse_collinear_overlaps(routed);
    cleanup::drop_zero_length(routed);
    cleanup::split_at_interior_attachments(routed);
    cleanup::trim_whiskers(routed, own_pin_coords);
    cleanup::prune_stale_junctions(routed, own_pin_coords);
    cleanup::add_connection_junctions(routed, own_pin_coords);
}

/// Union-find root of `k`, with path compression. Keys are quantised
/// wire endpoints; an unseen key is its own root.
pub(crate) fn uf_find(
    parent: &mut std::collections::HashMap<(i64, i64), (i64, i64)>,
    k: (i64, i64),
) -> (i64, i64) {
    let p = *parent.entry(k).or_insert(k);
    if p == k {
        return k;
    }
    let root = uf_find(parent, p);
    parent.insert(k, root);
    root
}

/// Is every pin of a routed net joined to every other by a path of
/// segments meeting **at endpoints**?
///
/// Endpoint-only is KiCad's own rule (`SCH_LINE::GetConnectionPoints`):
/// a branch that merely touches a trunk's interior is a split net. The
/// cleanup pass normalises interior attachments into real endpoints, so
/// this runs on post-cleanup geometry.
fn net_is_connected<S: ::std::hash::BuildHasher>(
    net: &RoutedNet,
    pins: &std::collections::HashSet<(i64, i64), S>,
) -> bool {
    #[allow(clippy::cast_possible_truncation)]
    let qk = |x: f64, y: f64| ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64);
    if pins.len() < 2 {
        return true;
    }
    // Union-find over endpoint coordinates.
    let mut parent: std::collections::HashMap<(i64, i64), (i64, i64)> =
        std::collections::HashMap::new();
    for seg in &net.segments {
        let (a, b) = (qk(seg.x1, seg.y1), qk(seg.x2, seg.y2));
        let (ra, rb) = (uf_find(&mut parent, a), uf_find(&mut parent, b));
        if ra != rb {
            parent.insert(ra, rb);
        }
    }
    let mut roots = pins.iter().map(|&p| uf_find(&mut parent, p));
    let Some(first) = roots.next() else {
        return true;
    };
    // A pin with no incident segment at all is its own root and fails
    // this just as a pin on a severed branch does.
    roots.all(|r| r == first)
}

/// First pair of distinct routed nets sharing a collinear overlapping
/// run, in ascending index order (deterministic).
fn first_cross_net_overlap(routed: &[RoutedNet]) -> Option<(usize, usize)> {
    for i in 0..routed.len() {
        for j in (i + 1)..routed.len() {
            let hit = routed[i].segments.iter().any(|sa| {
                routed[j]
                    .segments
                    .iter()
                    .any(|sb| conflict::segments_collinearly_overlap(sa, sb))
            });
            if hit {
                return Some((i, j));
            }
        }
    }
    None
}

/// Re-route the signal nets, suppressing the V5 outward stub on any net
/// flagged in `suppress_outward` (indexed in routed-net order).
fn route_signal_nets_suppressing(
    req: &RouteRequest<'_>,
    foreign_per_net: &[std::collections::HashSet<(i64, i64)>],
    suppress_outward: &[bool],
) -> Vec<RoutedNet> {
    let mut routed = Vec::new();
    let mut idx = 0;
    for net in req.nets {
        if !matches!(net.class, NetClass::Signal) {
            continue;
        }
        let (segments, junctions) = if suppress_outward.get(idx).copied().unwrap_or(false) {
            steiner::route_signal_without_collinear_stub(net, &foreign_per_net[idx])
        } else {
            steiner::route_signal(net, &foreign_per_net[idx])
        };
        idx += 1;
        routed.push(RoutedNet {
            segments,
            junctions,
        });
    }
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
// The staged pipeline (power symbols → Steiner → conflict/cleanup retry
// → serialise) reads as one sequence; splitting it would scatter the
// shared `routed` / `out` state across helpers that each need most of it.
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)] // by-value signature is the public contract
pub fn route(req: RouteRequest<'_>) -> RouteResult {
    let mut out = RouteResult::default();
    let mut pwr_counter = place_power_symbols(&req, &mut out);
    // PWR_FLAG drivers for every net with no driving pin (rails whose
    // pins are all power_in, signal nets whose pins are all input).
    // Single structural predicate, no fixture knowledge — see
    // `pwrflag::emit`. Global rails are driven from a corner block in
    // the bottom-right (which needs `obstacles` to know where the
    // circuit ends, and `pwr_counter` to number the rail glyphs it
    // draws there); sheet-local signal nets keep an on-pin flag.
    let mut flg_counter: usize = 0;
    pwrflag::emit(
        req.nets,
        req.obstacles,
        req.sheet_bodies,
        req.library,
        req.scope,
        req.sheet_uuid,
        req.project_name,
        &mut pwr_counter,
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
    // Stages 3–3d run as a unit, because their outcome can send us back
    // to Stage 2 (see the outward-stub rollback below).
    //
    // A net's V5 outward stub lifts its whole trunk one grid cell off
    // its pin row so each wire leaves its pin outward. On the symmetric
    // fixtures two sibling nets can be lifted onto the SAME channel —
    // `common_emitter`'s C and E both onto y = 48.26 — where they run
    // collinear and overlap. That is a latent V11 short (Tier 0): the
    // nets stay distinct only for want of a junction dot. The
    // single-track jog in Stage 3d moves one net over by a cell, which
    // cannot separate trunks that overlap along their whole length, so
    // it reports the pair unresolved.
    //
    // V5 is Tier 2 and V11 is Tier 0, so the stub yields: re-route the
    // lower-priority net of the unresolved pair with its outward
    // directions suppressed — the plain tree, which sits back on the pin
    // row and vacates the contested channel — and run the conflict
    // stages again over the new geometry. Only nets in an unresolved
    // pair lose their stub; everyone else keeps V5.
    //
    // Each attempt suppresses at least one more net, so the loop is
    // bounded by the net count. Two attempts settle every current
    // fixture; the cap is defensive.
    let mut suppress_outward = vec![false; routed.len()];
    let max_attempts = routed.len().saturating_add(1).clamp(1, 4);
    for attempt in 0..max_attempts {
        let warnings = conflict::resolve_conflicts(&mut routed, &net_pin_coords);

        let mut stage_warnings = warnings;
        // V11/V12 convergence loop. Each pass runs V11 (correctness)
        // first so a V12 detour can't re-introduce a foreign-pin
        // coincidence, then V12 (quality). Detours land in segment-set
        // signatures that the next V11 pass observes; we iterate until
        // two consecutive signatures agree or 3 passes elapse (a
        // defensive cap — the v0.1 fixtures converge in ≤ 2).
        let mut accumulated_warnings: Vec<String> = Vec::new();
        for _ in 0..3 {
            let pre_signatures: Vec<Vec<Segment>> =
                routed.iter().map(|n| n.segments.clone()).collect();
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
            // Stage 3e — re-resolve cross-net endpoint conflicts the
            // detours above just created (ADR-24).
            //
            // `resolve_conflicts` ran once, at the top of the attempt,
            // over the *pristine* Steiner trees. The V11 and V12 detour
            // passes then rewrite whole legs, and neither has any term
            // for "this new corner lands on a coordinate a SIBLING net
            // already terminates on" — `avoid_foreign_pins` keys on
            // foreign *pins*, `avoid_obstacles` on symbol *bodies*, and
            // `deconflict_cross_net_overlaps` only on *collinear*
            // overlap. A shared endpoint is none of those, and it is
            // exactly what KiCad joins.
            //
            // Measured on `sallen_key_driven` at the default iteration
            // count: `avoid_obstacles` detoured net `np` around a body
            // and parked a corner on `(59.69, 44.45)`, which is where
            // net `out`'s trunk already turned — a Tier-0 MERGE of the
            // op-amp's non-inverting input into its own output, shipped
            // with no warning, because the attempt loop's exit test asks
            // only about severance and collinear overlap.
            //
            // This is inert on geometry that has no such conflict:
            // `find_conflicts` returns empty and the pass returns
            // immediately. It sits inside the convergence loop so the
            // V11/V12 passes get to see (and re-judge) whatever it jogs.
            let wconf = conflict::resolve_conflicts(&mut routed, &net_pin_coords);
            accumulated_warnings.extend(wconf);
            let changed = pre_signatures
                .iter()
                .zip(routed.iter())
                .any(|(pre, now)| pre != &now.segments);
            if !changed {
                break;
            }
        }
        stage_warnings.extend(accumulated_warnings);

        // Stage 3d — cross-net collinear-overlap deconfliction. The V11
        // pass above keys on foreign *pin points*; it cannot see two
        // *different* nets whose trunks share a collinear run on the
        // same channel (the symmetric diff_pair / multivibrator failure
        // mode). Jog the lower-priority net's overlapping trunk one grid
        // cell onto an adjacent free track, wires only, guarded so it
        // cannot regress V11/V12 or raise the crossing count. Runs
        // before cleanup so coalesce / junction re-add normalise the
        // jogged geometry.
        let (w_deconf, unresolved) = conflict::deconflict_cross_net_overlaps(
            &mut routed,
            &foreign_per_routed,
            &net_pin_coords,
            req.obstacles,
            &pin_outward,
        );
        stage_warnings.extend(w_deconf);

        // Stage 4 — cleanup. Inside the retry loop because only the
        // FINAL geometry says whether a cross-net overlap really
        // survives: `unresolved` above is a pre-cleanup, conservative
        // signal, and `coalesce`/`collapse` routinely resolve pairs it
        // reported. Rolling back on the pre-cleanup signal suppressed
        // stubs on nets that were never in trouble (measured:
        // `multivibrator` V5 4 → 6).
        run_cleanup(&mut routed, &own_pin_coords_for_cleanup);

        // Two reasons to drop a net's outward stub and retry, both of
        // them higher-tier than the V5 the stub buys.
        //
        // 1. Tier 0, connectivity. The stub makes a leg three segments
        //    where the plain route is one, and the Stage-3 jog does not
        //    always carry a 3-segment leg across intact: it can move the
        //    trunk and leave the far riser behind, severing the net.
        //    (`examples/rc_lowpass.cir`: net `in` came apart exactly so.)
        //    That is a latent defect in the jog, not in the stub — but a
        //    severed net must never reach the page, so the stub yields
        //    until the jog is fixed. The check runs on post-cleanup
        //    geometry using KiCad's own endpoint-only rule.
        //
        // 2. Tier 0, cross-net separation: see the note above the loop.
        //
        // Whiskers are deliberately NOT a trigger here. A dangling stem
        // is dead wire, so `cleanup::trim_whiskers` deletes
        // it outright rather than paying a whole net's V5 to re-route
        // around it. Suppressing the stub was measured as the wrong
        // lever: it merely moved the orphan to another net
        // (`opamp_inverting` 1 -> 2), because the larger orphans are
        // jog/cleanup debris rather than stubs.
        //
        // Only nets that actually hit one of these lose their stub.
        let any_broken =
            (0..routed.len()).any(|i| !net_is_connected(&routed[i], &net_pin_coords[i]));
        let newly_suppressed = if any_broken {
            // Suppress the broken net first, then — since a *sibling*
            // net's stub can be what displaced the trunk that severed
            // this one — widen to the remaining nets one at a time. The
            // sequence therefore degrades monotonically toward "no
            // stubs anywhere", which is exactly the geometry this
            // router produced before the stub was reinstated. Worst
            // case we match that; we can never ship worse.
            (0..routed.len())
                .find(|&i| {
                    !net_is_connected(&routed[i], &net_pin_coords[i]) && !suppress_outward[i]
                })
                .or_else(|| (0..routed.len()).find(|&i| !suppress_outward[i]))
                .is_some_and(|i| {
                    suppress_outward[i] = true;
                    true
                })
        } else if let Some((a, b)) = (!unresolved.is_empty())
            .then(|| first_cross_net_overlap(&routed))
            .flatten()
        {
            [a.max(b), a.min(b)]
                .into_iter()
                .find(|&n| !suppress_outward[n])
                .is_some_and(|n| {
                    suppress_outward[n] = true;
                    true
                })
        } else {
            false
        };
        if !newly_suppressed || attempt + 1 == max_attempts {
            // Converged, or out of retries: keep this geometry and its
            // warnings (including any residual unresolved pairs, which
            // are the v0.2 channel router's work item).
            out.warnings.extend(stage_warnings);
            break;
        }
        routed = route_signal_nets_suppressing(&req, &foreign_per_routed, &suppress_outward);
    }
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
