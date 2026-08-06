//! Routing-aware orientation refinement (CLAUDE.md "Layout phase 4.5").
//!
//! V5 (first wire segment leaves each pin outward) is *routing-determined*:
//! a violation is born inside the router's post-construction
//! conflict-resolution passes
//! (`spice_route::conflict::{avoid_foreign_pins, avoid_obstacles}`), which
//! are invisible to any placement-side model. So orientation selection
//! that wants to minimise V5 must use the **real router as its oracle**.
//!
//! `spice-layout` cannot depend on `spice-route` (that edge would form a
//! cycle — `spice-route` already depends on `spice-layout`). `kicad-emitter`
//! depends on *both*, so this is the one place the router can be put in the
//! loop. The phase runs AFTER `spice_layout::place_with_hint` and BEFORE the
//! final `emit_root`/`route_nets` + decoration, and may only change an
//! element's **orientation** — never its position. Decoration stays a strict
//! consumer of the placement it receives.
//!
//! The pass is greedy and deterministic (no clock / RNG, stable iteration
//! order), iterating to a fixed point under a small cap. For each at-risk,
//! non-pinned, non-symmetry element it trial-routes each V14-allowed
//! orientation (reusing `spice_layout::orient::allowed_orientations` — it
//! never widens V14) and keeps a candidate ONLY if it *strictly* improves the
//! lexicographic objective
//! `(severed, coincident, V11, V13, V12, V5, bends)` — Tier-0 keys first —
//! without increasing symbol-body overlap or foreign-body (V12) crossings.
//!
//! **Ordering contract for the objective** (`docs/invariants.md` V16): V16
//! bends are the FINAL key and must stay there. They may appear in the
//! acceptance predicate only as that last key or as a non-regression guard
//! alongside `v11`/`overlap`/`v12` — never earlier in the tuple, and never
//! as a weighted term. Last place is what makes the subordination
//! structural: a candidate raising `v12` or `v13` yields a strictly greater
//! tuple however many bends it saves, so bends can never buy a wire through
//! a body or across a label. The quantity must also be the *ink-graph* bend
//! count ([`bend_count`]), never a raw segment count — `cleanup.rs` splits
//! segments for Tier-0 correctness, and a raw count would push against it.

use kicad_symbols::{Library, Orientation, Symbol};
use spice_layout::{Placement, RefinementMeta};

use crate::schematic::{
    LabelObstacles, TextBbox, collect_net_pins, label_rotation_obstacles, label_specs,
    placement_obstacles_with_refdes, placement_property_bboxes, rail_glyph_body_bboxes, text_bbox,
    trial_route,
};
use crate::v5::{PinProbe, Violation, count_outward_violations};

/// Maximum greedy sweeps over the element list before giving up on
/// further improvement. A handful is plenty for the small fixtures;
/// the cap bounds worst-case cost on a pathological large sheet.
const MAX_SWEEPS: usize = 4;

/// Cap on the number of orientation *combinations* the combinatorial
/// joint search will trial-route. A V5 violation is frequently only
/// removable by rotating an offending element AND a shared-net neighbour
/// *together* (e.g. the inverting-amp's RIN + RF + X1), which a purely
/// greedy single-element sweep cannot reach without a strictly-improving
/// intermediate step. The joint search enumerates the cartesian product
/// of the active elements' allowed orientations; this cap bounds the
/// trial-route count so a large active set degrades gracefully (the
/// search is skipped and only the greedy sweep runs).
const MAX_COMBINATIONS: usize = 512;

/// Cap on the number of *active* elements the joint search considers at
/// once (offenders + their direct shared-signal-net neighbours,
/// non-pinned). Bounds the product size together with [`MAX_COMBINATIONS`].
const MAX_ACTIVE: usize = 4;

/// Refine element orientations to minimise the router's *real* V5
/// (first-segment-outward) count, in place.
///
/// `meta` carries the same `pinned` mask and V14-`allowed` orientation
/// sets the placer used (see [`spice_layout::refinement_meta`]). Pinned
/// elements (user `align`/`place`, V7 symmetry, position-stability hint)
/// are never touched; every candidate orientation comes from the
/// element's V14-allowed set, so the phase honours V14 by construction.
///
/// Acceptance is conservative: a candidate is taken only if it strictly
/// reduces total real V5 violations AND does not increase the V11
/// foreign-pin residue, the symbol-body overlap count, or the V12
/// foreign-body wire-crossing count. The phase therefore can only
/// improve (or no-op) the higher-/equal-tier invariants while improving
/// the V5 quality metric — never trade one off against another.
pub fn refine_orientations(placement: &mut Placement, library: &Library, meta: &RefinementMeta) {
    let n = placement.elements.len();
    if n == 0 {
        return;
    }

    // Baseline measurement of the placement as received.
    let mut baseline = measure(placement, library);
    log::debug!("refine: baseline {}", summarise(&baseline));
    // Nothing to chase only when every count the objective can act on is
    // already zero. Returning on `v5 == 0` alone would skip the pass on a
    // placement whose only defect is a wire speared through a body (V12,
    // higher tier) — or, worse, one that is Tier-0 broken: a severed net
    // or a pin-on-pin short can exist with V5 and V12 both clean, and
    // that is precisely the case the pass must not walk away from.
    if is_settled(&baseline) {
        return;
    }

    // Greedy single-element descent first: cheap, each accepted step
    // *strictly* reduces real V5, so it converges in at most `v5` steps.
    greedy_descent(placement, library, meta, &mut baseline);
    if is_settled(&baseline) {
        return;
    }

    // If greedy stalled with V5 still positive, fall back to a bounded
    // *joint* search over the offending elements and their shared-net
    // neighbours. Many V5 violations are removable only by rotating an
    // offender together with a neighbour (e.g. RIN+RF+X1 on the inverting
    // amp), which the strictly-improving greedy descent cannot reach on
    // its own. The joint search early-exits the moment it finds a
    // zero-V5 combination, so its worst-case cost binds only when no full
    // fix exists.
    joint_search(placement, library, meta, &mut baseline);
    log::debug!("refine: final {}", summarise(&baseline));
}

/// One-line rendering of a [`Measure`] for the debug log, in objective
/// order. Cheap: it reads counts already computed, never re-routes.
fn summarise(m: &Measure) -> String {
    format!(
        "severed={} coincident={} v11={} v13={} v12={} v5={} bends={} (overlap={})",
        m.severed, m.coincident, m.v11, m.v13, m.v12, m.v5, m.bends, m.overlap
    )
}

/// Phase 4.5's acceptance predicate: may the candidate measurement `m`
/// replace `baseline`?
///
/// Two mechanisms, and which one a property uses is load-bearing
/// (CLAUDE.md, "Constraints vs. costs"):
///
/// 1. **The lexicographic objective**
///    `(severed, coincident, v11, v13, v12, v5, bends)` — read in strict
///    CLAUDE.md tier order: **Tier-0 first** (`severed`, `coincident`,
///    `v11`), then Tier-1 (`v13`, `v12`), then Tier-2 (`v5`, and V16
///    `bends` LAST). A candidate must strictly improve this tuple to be
///    considered at all. The ordering contract on `bends` is documented
///    at the module head and in `docs/invariants.md` V16: bends must
///    stay the final key.
///
/// 2. **Hard non-regression guards** — `overlap` / `v12`, each
///    `<=` its baseline. These are *categorical* properties with one
///    correct answer, so they filter the candidate space outright rather
///    than trading against anything. They are lifted for — and *only*
///    for — a candidate that strictly repairs Tier 0, which is the one
///    trade CLAUDE.md's ordering rule mandates rather than forbids.
///
/// ## Why the two Tier-0 counts lead the tuple
///
/// Both used to be `<=` guards, and an earlier revision of this comment
/// argued `severed` *must* stay one, on the reasoning that as a tuple key
/// "a *reduction* in `severed` could outrank a Tier-1 V13/V12
/// regression". That reasoning inverts CLAUDE.md's ordering rule. The
/// rule is "never trade a Tier-0 violation for any Tier-1/2 gain" — it
/// forbids *losing* Tier 0 to buy Tier 1, and mandates exactly the
/// opposite direction: a Tier-0 repair outranks any Tier-1 cost.
///
/// As leading keys the two counts subsume their old guards — a candidate
/// with a higher `severed` or `coincident` is strictly greater in the
/// tuple and rejected however much V13/V12/V5/bend it saves — and they
/// additionally make a Tier-0 defect something this phase can *seek*.
/// That is the difference that mattered: on `shunt_feedback_amp` the SA
/// handed phase 4.5 a placement with **two severed signal nets**, and
/// because `severed` was only a floor, the phase had no reason to repair
/// it. It repaired it by accident instead — rotating `Q1` into the one
/// pose that reconnects the nets by putting `Q1`'s base pin exactly on
/// `RE`'s pin, i.e. by *shorting the base to the emitter*. Every metric
/// the tuple could see improved. With Tier 0 leading, the phase looks
/// for a pose that fixes the severance without the short, and `Q1`'s
/// upright R0 pose is one.
///
/// The blast radius of putting them in front is provably nil for a
/// placement that is not already Tier-0 broken: when `severed` and
/// `coincident` are 0 on both sides the comparison falls straight
/// through to `(v13, v12, v5, bends)`. All eleven pre-existing fixtures
/// measure 0/0 at both baseline and final.
///
/// `coincident` also closes a hole neither `v11` nor `overlap` covered:
/// `v11` counts *wire*-touches-foreign-pin warnings, and the router
/// emits none when two **pins** coincide (there is no wire it could
/// detour); `overlap` uses strict body-bbox interiors, so two bodies
/// that merely kiss — exactly what abutting pin tips look like — are not
/// an overlap. A reorientation that shorted two nets therefore scored as
/// a clean improvement.
///
/// Demonstrated hazard the `severed` term closes: on `common_emitter`,
/// rotating `COUT` to 180 boxes its `c` pin between a foreign pin (V11
/// blocks one L-route) and `Q1`'s body (V12 blocks the other); the
/// router's conflict cascade exhausts its detours and drops the branch,
/// and the CLI's post-emit connectivity check refuses the file. See
/// `severed_guard_tests`.
fn accepts(baseline: &Measure, m: &Measure) -> bool {
    if objective(m) >= objective(baseline) {
        return false;
    }
    // A candidate that strictly repairs Tier 0 is taken even if a Tier-1
    // guard would otherwise veto it. This is CLAUDE.md ordering rule 1
    // read in the direction it actually points: "never trade a Tier-0
    // violation for any Tier-1/2 gain" forbids losing Tier 0 to buy
    // Tier 1, and therefore requires paying Tier 1 to recover Tier 0. It
    // is unreachable on a placement that is not already Tier-0 broken.
    if tier0(m) < tier0(baseline) {
        return true;
    }
    m.overlap <= baseline.overlap && m.v12 <= baseline.v12
}

/// The lexicographic objective tuple — see [`accepts`] for the ordering
/// contract. Used both by the acceptance predicate and by the
/// `best`-candidate selection, so the two can never disagree.
fn objective(m: &Measure) -> (usize, usize, usize, usize, usize, usize, usize) {
    (m.severed, m.coincident, m.v11, m.v13, m.v12, m.v5, m.bends)
}

/// The Tier-0 prefix of [`objective`]: connectivity, pin-on-pin shorts,
/// and unresolved wire-on-foreign-pin residue. All three are V11/V2
/// correctness, so the Tier-1 guard exemption in [`accepts`] must NOT
/// reach them — lifting `v11` for a `severed` repair would be trading
/// Tier 0 for Tier 0, and it measurably was: phase 4.5 answered
/// `shunt_feedback_amp`'s two severed nets with a pose that left one
/// unresolved `v11:` residue behind.
fn tier0(m: &Measure) -> (usize, usize, usize) {
    (m.severed, m.coincident, m.v11)
}

/// Is there nothing left for this phase to chase? True only when every
/// count the objective can act on is zero — Tier 0 included, so a
/// severed or shorted placement is never mistaken for a finished one.
fn is_settled(m: &Measure) -> bool {
    tier0(m) == (0, 0, 0) && m.v5 == 0 && m.v12 == 0
}

/// Greedy single-element orientation descent: repeatedly pick, for each
/// offending non-pinned element, the V14-allowed orientation that most
/// reduces real V5 without regressing V11 / overlap / V12 / V13. Each
/// accepted move strictly lowers V5, so the sweep converges quickly.
fn greedy_descent(
    placement: &mut Placement,
    library: &Library,
    meta: &RefinementMeta,
    baseline: &mut Measure,
) {
    let n = placement.elements.len();
    for _ in 0..MAX_SWEEPS {
        let mut improved_this_sweep = false;
        for i in 0..n {
            if meta.pinned.get(i).copied().unwrap_or(false) {
                continue;
            }
            let Some(allowed) = meta.allowed.get(i) else {
                continue;
            };
            // Skip elements that cannot currently contribute a V5 or a
            // V12 violation: only those whose own pins are flagged, or
            // whose body a wire currently spears, are worth re-orienting.
            // This bounds the trial-route count without losing any
            // improvable element. V12 offenders must be included — an
            // element can be speared without having any V5 violation of
            // its own, and filtering on V5 alone made those unreachable.
            let refdes = &placement.elements[i].refdes;
            let is_v5_offender = baseline.offenders.iter().any(|v| &v.refdes == refdes);
            let is_v12_offender = baseline.v12_offenders.contains(refdes);
            // Tier-0 repair mode: when the placement arrives severed or
            // shorted, the at-risk filter does not apply. Neither
            // `offenders` nor `v12_offenders` names the element whose
            // pose would reconnect a dropped branch — the V5/V12
            // violations sit on the *wires*, and the element to rotate
            // may have none of its own. Restricting the sweep to
            // offenders is a cost bound for a healthy placement; on a
            // broken one it would hide the only available repair. Costs
            // a full sweep of trial routes, and only ever on a placement
            // the CLI would otherwise refuse to ship.
            let tier0_repair = tier0(baseline) != (0, 0, 0);
            if !tier0_repair && !is_v5_offender && !is_v12_offender {
                continue;
            }

            let current = placement.elements[i].orientation;
            let candidates = distinct_orientations(
                allowed,
                current,
                library.lookup(&placement.elements[i].lib_id),
            );
            let mut best: Option<(Orientation, Measure)> = None;
            for cand in candidates {
                if cand == current {
                    continue;
                }
                placement.elements[i].orientation = cand;
                let m = measure(placement, library);
                placement.elements[i].orientation = current;

                // Accept when the (V13, V12, V5, bends) tuple strictly
                // improves and no equal-/higher-tier guard regresses. V13
                // and V12 are Tier 1, V5 and bends (V16) are Tier 2, so
                // the Tier-1 counts lead: a candidate that removes a
                // label overlap or a wire speared through a body wins
                // even if V5/bends are unchanged, and one that adds
                // either is never taken for a V5 or bend gain. Selection
                // is lexicographic for the same reason.
                //
                // V12 belongs in the objective, not just the guard. It
                // used to appear only as the `m.v12 <= baseline.v12`
                // non-regression check, which meant the search could
                // never *seek* a V12 fix — it only ever landed one as a
                // side effect of a V5 improvement that happened to
                // straighten the same wire. That is a tier inversion:
                // a Tier-1 defect was reachable only while a Tier-2
                // gradient still pointed at it, so any change that
                // flattened V5 first (e.g. a router fix that removes V5
                // violations outright) silently stranded the V12
                // crossing. Keeping the `<=` guard as well means V12 can
                // now only fall, never rise — no sideways trade against
                // V13 within the tier.
                //
                // ORDERING CONTRACT (docs/invariants.md V16). V16 bends
                // are the FINAL key of this tuple and must stay there.
                // They may appear in this predicate in exactly two
                // shapes — the last lexicographic key, or a
                // non-regression guard alongside `v11`/`overlap`/`v12` —
                // and never earlier in the tuple, never as a weighted
                // term. Last-place lexicographic ordering is what makes
                // the subordination structural rather than a matter of
                // tuning: a candidate that raises `v12` or `v13` yields a
                // strictly greater tuple no matter how many bends it
                // saves, and is independently refused by the `<=` guards
                // below. Bends can therefore never buy a wire through a
                // body or across a label. Moving them earlier would make
                // that trade reachable — don't.
                if accepts(baseline, &m) {
                    let take = match &best {
                        None => true,
                        Some((_, bm)) => objective(&m) < objective(bm),
                    };
                    if take {
                        best = Some((cand, m));
                    }
                }
            }

            if let Some((orient, m)) = best {
                placement.elements[i].orientation = orient;
                *baseline = m;
                improved_this_sweep = true;
                if is_settled(baseline) && baseline.v13 == 0 {
                    return;
                }
            }
        }
        if !improved_this_sweep {
            break;
        }
    }
}

/// Bounded combinatorial joint orientation search.
///
/// Builds the *active set* — the non-pinned elements currently producing
/// a V5 violation, plus their non-pinned neighbours sharing a signal net
/// (rotating a neighbour can swing a pin's connecting wire outward) —
/// then enumerates the cartesian product of each active element's
/// V14-allowed orientations. The combination minimising real V5 (subject
/// to no V11 / overlap / V12 / V13 regression vs `baseline`) is applied.
///
/// Deterministic: active elements are taken in ascending index order and
/// orientations in their allowed-set order, so the lexicographically
/// first minimal-V5 combination wins. Skipped (leaving the greedy sweep
/// to handle it) when the product would exceed [`MAX_COMBINATIONS`].
// Active-set construction + mixed-radix enumeration share local state
// (active / cand / counter / best) that helper-splitting would obscure.
#[allow(clippy::too_many_lines)]
fn joint_search(
    placement: &mut Placement,
    library: &Library,
    meta: &RefinementMeta,
    baseline: &mut Measure,
) {
    let n = placement.elements.len();

    // Offending element indices (non-pinned, V14-allowed known).
    let movable = |i: usize| {
        !meta.pinned.get(i).copied().unwrap_or(false)
            && meta.allowed.get(i).is_some_and(|a| !a.is_empty())
    };
    let offending: Vec<usize> = (0..n)
        .filter(|&i| {
            movable(i)
                && baseline
                    .offenders
                    .iter()
                    .any(|v| v.refdes == placement.elements[i].refdes)
        })
        .collect();
    if offending.is_empty() {
        return;
    }

    // Neighbours: any movable element sharing a non-ground signal net
    // with an offender. Build net → element-indices, then expand.
    let mut net_to_els: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, el) in placement.elements.iter().enumerate() {
        if el.is_power_source {
            continue;
        }
        for node in &el.nodes {
            if node == "0" {
                continue;
            }
            net_to_els.entry(node.as_str()).or_default().push(i);
        }
    }
    let mut active: Vec<usize> = offending.clone();
    for &o in &offending {
        for node in &placement.elements[o].nodes {
            if node == "0" {
                continue;
            }
            if let Some(els) = net_to_els.get(node.as_str()) {
                for &j in els {
                    if movable(j) && !active.contains(&j) {
                        active.push(j);
                    }
                }
            }
        }
    }
    // Truncate BEFORE sorting. `active` is built offenders-first, then
    // neighbours, so truncating in that order keeps the elements the
    // search actually exists to re-orient. Sorting first was a defect:
    // on `common_emitter` the offenders (CIN=6, Q1=8) sorted *after*
    // their six neighbours, so a `MAX_ACTIVE` of 4 cut both offenders
    // out and the joint search enumerated 16 combinations of four
    // elements that had no V5 violation to fix. Sort only afterwards,
    // to keep the enumeration order deterministic.
    active.truncate(MAX_ACTIVE);
    active.sort_unstable();

    // Per-active-element candidate orientations: the V14-allowed set with
    // geometrically-equivalent orientations collapsed (a symmetric 2-pin
    // resistor's eight orientations reduce to the handful that yield
    // distinct world pin layouts), so the product stays small. The
    // element's *current* orientation is forced in first so it is always
    // a candidate (the "no change" option).
    let cand: Vec<Vec<Orientation>> = active
        .iter()
        .map(|&i| {
            let symbol = library.lookup(&placement.elements[i].lib_id);
            distinct_orientations(&meta.allowed[i], placement.elements[i].orientation, symbol)
        })
        .collect();

    // Product size guard.
    let mut product: usize = 1;
    for c in &cand {
        product = product.saturating_mul(c.len().max(1));
        if product > MAX_COMBINATIONS {
            return; // Too large — leave it to the greedy sweep.
        }
    }

    let originals: Vec<Orientation> = active
        .iter()
        .map(|&i| placement.elements[i].orientation)
        .collect();

    // Enumerate the cartesian product via a mixed-radix counter.
    let radices: Vec<usize> = cand.iter().map(Vec::len).collect();
    let mut best: Option<(Vec<Orientation>, Measure)> = None;
    let mut counter = vec![0usize; active.len()];
    'enumerate: loop {
        // Apply this combination.
        for (k, &i) in active.iter().enumerate() {
            placement.elements[i].orientation = cand[k][counter[k]];
        }
        let m = measure(placement, library);
        // Same lexicographic (V13, V12, V5, bends) objective as
        // `greedy_descent` — Tier-1 counts lead, Tier-2 V5 next, V16
        // bends last. See the ordering contract documented on that
        // function's acceptance gate: bends must stay the FINAL key.
        if accepts(baseline, &m) {
            let take = match &best {
                None => true,
                Some((_, bm)) => objective(&m) < objective(bm),
            };
            if take {
                // Stop only when NOTHING is left to improve on ANY key,
                // bends included. The old exit fired on
                // (V13, V12, V5) == 0 alone, which returned the
                // lexicographically FIRST zero-cost combination and hid
                // every equally-clean but straighter alternative behind
                // it — on `rc_lowpass_ports` exactly what masked R1 at
                // rot 180 (B = 2) behind rot 0 (B = 4), since rot 0 is
                // enumerated first and already scores a clean zero.
                //
                // Continuing is bounded: the enumeration is already hard
                // capped at `MAX_COMBINATIONS`, so the worst case is
                // unchanged and only the typical case does more work.
                let perfect = m.v13 == 0 && m.v12 == 0 && m.v5 == 0 && m.bends == 0;
                let chosen: Vec<Orientation> = active
                    .iter()
                    .map(|&i| placement.elements[i].orientation)
                    .collect();
                best = Some((chosen, m));
                if perfect {
                    break 'enumerate;
                }
            }
        }
        // Increment mixed-radix counter; overflow ends enumeration.
        let mut k = 0;
        loop {
            if k == active.len() {
                break 'enumerate;
            }
            counter[k] += 1;
            if counter[k] < radices[k] {
                break;
            }
            counter[k] = 0;
            k += 1;
        }
    }
    // Restore originals and apply best (if any).
    for (idx, &i) in active.iter().enumerate() {
        placement.elements[i].orientation = originals[idx];
    }
    if let Some((orients, m)) = best {
        for (idx, &i) in active.iter().enumerate() {
            placement.elements[i].orientation = orients[idx];
        }
        *baseline = m;
    }
}

/// The metrics the acceptance gate compares. `offenders` carries the V5
/// violations so the sweep can skip elements that aren't offending, and
/// `v12_offenders` does the same for V12: the refdes of every element
/// whose body a wire currently spears. Without the latter the sweep
/// could only ever fix a V12 crossing on an element that *also* had a
/// V5 violation — see the tier-inversion note on the acceptance gate.
/// `v13` is the combined label↔body + label↔property-text overlap count
/// (V13 parts 1 and 2), measured on the exact labels the emitter will
/// plant ([`label_specs`]).
#[derive(Clone)]
struct Measure {
    v5: usize,
    v11: usize,
    overlap: usize,
    v12: usize,
    v13: usize,
    /// V16 bend count of the trial route, as the **ink-graph** quantity
    /// (see [`bend_count`]) — never a raw segment or route-corner count.
    /// Always the FINAL key of the acceptance tuple; see the ordering
    /// contract on `greedy_descent`'s gate.
    bends: usize,
    /// Signal nets the trial route leaves severed — **Tier 0**. A pure
    /// non-regression guard, never part of the objective tuple: there is
    /// nothing to *seek* here, connectivity is a categorical floor, not a
    /// quality gradient (CLAUDE.md "constraints vs costs"). Putting it in
    /// the lexicographic tuple would make it tradeable against V13/V12/V5
    /// in the `best`-selection comparison; as a `<=` guard it is instead
    /// excluded from the candidate space outright.
    severed: usize,
    /// Tier-0 **short** count: places where two different nets end up
    /// electrically joined by geometry no later pass can separate —
    /// pin-on-pin (including a rail glyph's anchor pin), plus a wire
    /// running through a rail glyph's anchor pin. See
    /// [`crate::schematic::tier0_short_count`] for both hazards.
    ///
    /// A pure non-regression guard, alongside `severed` and for the same
    /// reason: this is a categorical floor, not a gradient. It is also
    /// the one hazard the `v11` field CANNOT see. `v11` counts the
    /// router's `v11:` warnings — *wires* that still touch a foreign
    /// pin — and the router emits none for a pin-on-pin overlap, because
    /// there is no detour it could even attempt. Rotating an element
    /// moves its pins, so phase 4.5 can create the overlap outright:
    /// on `shunt_feedback_amp` it reoriented `Q1` until its base pin sat
    /// exactly on `RE`'s pin 1, merging the base and emitter nets, and
    /// every other metric in `Measure` reported an improvement.
    coincident: usize,
    offenders: Vec<Violation>,
    v12_offenders: Vec<String>,
}

/// Trial-route `placement` and measure V5, V11 residue, symbol-body
/// overlap, V12 foreign-body crossings, and V13 label overlaps.
fn measure(placement: &Placement, library: &Library) -> Measure {
    let route = trial_route(placement, library);
    let pins = pin_probes(placement, library);
    let offenders = count_outward_violations(&pins, &route.segments);
    let overlap = symbol_overlap_count(placement, library);
    let (v12, v12_offenders) = v12_crossings(placement, library, &route.segments);
    let v13 = v13_overlap_count(placement, library);
    let bends = bend_count(&route.segments);
    Measure {
        v5: offenders.len(),
        v11: route.v11_count,
        severed: route.severed,
        coincident: route.shorts,
        overlap,
        v12,
        v13,
        bends,
        offenders,
        v12_offenders,
    }
}

/// V16 bend count — the L-corners of the emitted **ink**.
///
/// This is deliberately the ink-graph quantity defined in
/// `docs/invariants.md` V16 and computed by its verifier
/// (`spice2kicad/tests/wire_geometry.rs`), NOT a raw segment or
/// route-corner count. Group segments by line, merge
/// touching-or-overlapping collinear spans into **maximal straight
/// runs**, then count vertices carrying exactly two rays of opposite
/// orientation.
///
/// The maximal-run merge is the load-bearing part. `spice_route`'s
/// `cleanup::split_at_interior_attachments` is a **Tier-0 correctness**
/// pass that deliberately INCREASES the segment count (KiCad joins wires
/// only at endpoints), and `coalesce_collinear` /
/// `collapse_collinear_overlaps` re-segment identical ink the other way.
/// A raw count in the acceptance gate would therefore put optimisation
/// pressure against a correctness pass — the metric must be invariant
/// under re-segmentation of identical ink, and merging runs before
/// counting is what makes it so.
fn bend_count(segments: &[crate::v5::WireSegment]) -> usize {
    // Quantise to µm so float noise cannot split a run.
    #[allow(clippy::cast_possible_truncation)]
    let q = |v: f64| (v * 1000.0).round() as i64;

    // (is_vertical, fixed coordinate) -> spans along the free axis.
    let mut lines: std::collections::BTreeMap<(bool, i64), Vec<(i64, i64)>> =
        std::collections::BTreeMap::new();
    for &((x1, y1), (x2, y2)) in segments {
        let (x1, y1, x2, y2) = (q(x1), q(y1), q(x2), q(y2));
        if x1 == x2 && y1 == y2 {
            continue; // degenerate
        }
        if x1 == x2 {
            lines
                .entry((true, x1))
                .or_default()
                .push((y1.min(y2), y1.max(y2)));
        } else if y1 == y2 {
            lines
                .entry((false, y1))
                .or_default()
                .push((x1.min(x2), x1.max(x2)));
        }
        // Diagonals are an outright V16 failure the verifier catches;
        // nothing in the pipeline emits them, so they are ignored here.
    }

    // Merge each line's spans into maximal runs.
    let mut runs: Vec<(bool, i64, i64, i64)> = Vec::new();
    for ((vertical, fixed), mut spans) in lines {
        spans.sort_unstable();
        let (mut lo, mut hi) = spans[0];
        for (a, b) in spans.into_iter().skip(1) {
            if a <= hi {
                hi = hi.max(b); // touching or overlapping — same run
            } else {
                runs.push((vertical, fixed, lo, hi));
                (lo, hi) = (a, b);
            }
        }
        runs.push((vertical, fixed, lo, hi));
    }

    // Candidate vertices: every run endpoint.
    let mut points: Vec<(i64, i64)> = Vec::new();
    for &(vertical, fixed, lo, hi) in &runs {
        for end in [lo, hi] {
            points.push(if vertical { (fixed, end) } else { (end, fixed) });
        }
    }
    points.sort_unstable();
    points.dedup();

    // A bend is a 2-ray vertex with one horizontal and one vertical ray.
    // Rays are counted as `cleanup::rays_at` does: a run ENDING here
    // contributes one, a run whose strict INTERIOR contains it two.
    points
        .into_iter()
        .filter(|&(px, py)| {
            let (mut rays, mut has_v, mut has_h) = (0usize, false, false);
            for &(vertical, fixed, lo, hi) in &runs {
                let (along, across) = if vertical { (py, px) } else { (px, py) };
                if across != fixed || along < lo || along > hi {
                    continue;
                }
                rays += if along > lo && along < hi { 2 } else { 1 };
                if vertical {
                    has_v = true;
                } else {
                    has_h = true;
                }
            }
            rays == 2 && has_v && has_h
        })
        .count()
}

/// Count V13 label overlaps for `placement`: a label's text bbox against
/// (1) any symbol body bbox, or (2) any Reference/Value property bbox.
///
/// This is a deliberate *approximation* of decoration, not a replica of
/// it, and the gap is load-bearing — see the ADR-11 post-mortem in
/// `docs/layout-adr.md`. The gate necessarily scores PRE-nudge property
/// anchors (`placement_property_bboxes`), because the real anchors are
/// chosen later by `nudge_property_text`, which needs the emitted item
/// list this function does not have. It therefore also passes an empty
/// pin-text set and `anchor_search: false`, keeping the whole model
/// consistently one step upstream of decoration.
///
/// Making the label side faithful while the property side stays upstream
/// was measured and is strictly worse: the gate then sees label/property
/// overlaps that decoration goes on to resolve (property text nudges away;
/// labels have four rotations at every pin on their net), and refuses real
/// V5 improvements to avoid those phantoms. common_emitter V5 0 -> 1 and
/// opamp_inverting_real 1 -> 2, with measured V13 already 0 in both cases.
///
/// Closing the gap for real means simulating the whole decoration text
/// pipeline per candidate orientation — route, labels, property nudge,
/// glyph-value nudge. That is feasible (the gate already trial-routes with
/// the real router) but is its own project. Until then, prefer the
/// consistently-upstream model over a half-aligned one.
fn v13_overlap_count(placement: &Placement, library: &Library) -> usize {
    let net_pins = collect_net_pins(placement, library, &[]);
    let props = placement_property_bboxes(placement);
    // Interface-label rotation obstacles, matching the emitter
    // (`emit_root`): host symbol bodies plus foreign rail-glyph bodies,
    // so the refinement gate measures the SAME rotated global-label
    // geometry the final decoration will emit (V13 item 2B).
    let negative_rails = spice_layout::net_class::negative_rail_nets(placement);
    let rail_tags = spice_layout::net_class::rail_tags(placement);
    let glyph_bodies = rail_glyph_body_bboxes(&net_pins, library, &negative_rails, &rail_tags);
    let label_obstacles = label_rotation_obstacles(placement, library, &glyph_bodies);
    // Consistently upstream of decoration — see the doc comment: no
    // pin-text set, no wires, no anchor search.
    let obs = LabelObstacles {
        properties: &props,
        bodies: &label_obstacles,
        pin_texts: &[],
        wires: &[],
    };
    let specs = label_specs(
        &net_pins,
        &[],
        &obs,
        false,
        &std::collections::BTreeMap::new(),
        &rail_tags,
    );
    // World body bboxes (as TextBboxes) for the label↔body check.
    let bodies: Vec<TextBbox> = placement
        .elements
        .iter()
        .filter_map(|el| {
            if el.is_power_source || el.lib_id.starts_with("power:") {
                return None;
            }
            let (ox, oy) = el.origin.to_mm();
            library
                .lookup(&el.lib_id)
                .and_then(Symbol::body_bbox)
                .map(|b| {
                    let w = body_bbox_world(b, ox, oy, el.orientation);
                    TextBbox {
                        x0: w.x0,
                        y0: w.y0,
                        x1: w.x1,
                        y1: w.y1,
                    }
                })
        })
        .collect();
    let mut hits = 0;
    for spec in &specs {
        let lbbox = text_bbox(&spec.net, (spec.x, spec.y), spec.rot);
        for body in &bodies {
            if lbbox.intersects(*body) {
                hits += 1;
            }
        }
        for p in &props {
            if lbbox.intersects(*p) {
                hits += 1;
            }
        }
    }
    hits
}

/// World-frame pin probes for every placed (non-power-source) element,
/// matching `schematic::collect_net_pins`' transform: a library pin at
/// local `(x, y)` placed at origin `(ox, oy)` lands at world
/// `(ox + x, oy - y)` (eeschema y-flip), carrying the library-frame pin
/// `angle`. Power-rail sources contribute no pins (they are not drawn).
fn pin_probes(placement: &Placement, library: &Library) -> Vec<PinProbe> {
    let mut out = Vec::new();
    for el in &placement.elements {
        if el.is_power_source {
            continue;
        }
        let Some(symbol) = library.lookup(&el.lib_id) else {
            continue;
        };
        let pins = symbol.pins_in(el.orientation);
        let (ox, oy) = el.origin.to_mm();
        for kicad_pin in &el.pin_mapping {
            let Some(pin) = pins.iter().find(|p| &p.number == kicad_pin) else {
                continue;
            };
            out.push(PinProbe {
                refdes: el.refdes.clone(),
                pin_number: pin.number.clone(),
                x_mm: ox + pin.x,
                y_mm: oy - pin.y,
                angle: pin.angle,
            });
        }
    }
    out
}

/// Count pairs of placed elements whose world-frame body bboxes overlap.
/// Mirrors the no-symbol-symbol-overlap verifier's intent (body extent,
/// orientation-aware) so the gate can only ever decline an orientation
/// that introduces a body collision the SA gate would also reject.
fn symbol_overlap_count(placement: &Placement, library: &Library) -> usize {
    let boxes: Vec<Option<spice_route::Bbox>> = placement
        .elements
        .iter()
        .map(|el| {
            if el.is_power_source || el.lib_id.starts_with("power:") {
                return None;
            }
            let (ox, oy) = el.origin.to_mm();
            library
                .lookup(&el.lib_id)
                .and_then(Symbol::body_bbox)
                .map(|b| body_bbox_world(b, ox, oy, el.orientation))
        })
        .collect();
    let mut count = 0;
    for i in 0..boxes.len() {
        for j in (i + 1)..boxes.len() {
            if let (Some(a), Some(b)) = (&boxes[i], &boxes[j])
                && bboxes_overlap(a, b)
            {
                count += 1;
            }
        }
    }
    count
}

/// Count routed wire segments whose interior penetrates a foreign
/// element's body bbox (V12). `placement_obstacles` already excludes
/// power glyphs / suppressed rail sources and adds the router's clearance
/// margin, so an interior intersection here is a genuine V12 crossing.
fn v12_crossings(
    placement: &Placement,
    library: &Library,
    segments: &[crate::v5::WireSegment],
) -> (usize, Vec<String>) {
    let obstacles = placement_obstacles_with_refdes(placement, library);
    let mut count = 0;
    let mut offenders: Vec<String> = Vec::new();
    for (refdes, bbox) in &obstacles {
        let mut hit = false;
        for (a, b) in segments {
            if bbox.intersects_segment(a.0, a.1, b.0, b.1) {
                count += 1;
                hit = true;
            }
        }
        if hit {
            offenders.push(refdes.clone());
        }
    }
    (count, offenders)
}

/// Transform a symbol-local body bbox into a world-frame `Bbox`, using
/// the same convention as pin coordinates: rotate/mirror via
/// [`Orientation::apply_point`], then apply the eeschema y-flip
/// (`world_y = origin_y - local_y`), and take the AABB of the four
/// transformed corners.
fn body_bbox_world(
    local: kicad_symbols::LocalBbox,
    ox: f64,
    oy: f64,
    orient: Orientation,
) -> spice_route::Bbox {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (lx, ly) in [
        (local.x0, local.y0),
        (local.x0, local.y1),
        (local.x1, local.y0),
        (local.x1, local.y1),
    ] {
        let (rx, ry) = orient.apply_point(lx, ly);
        let wx = ox + rx;
        let wy = oy - ry;
        min_x = min_x.min(wx);
        max_x = max_x.max(wx);
        min_y = min_y.min(wy);
        max_y = max_y.max(wy);
    }
    spice_route::Bbox {
        x0: min_x,
        y0: min_y,
        x1: max_x,
        y1: max_y,
    }
}

/// Strict (interior) overlap of two world-frame bboxes. A shared edge
/// (touching, e.g. abutting bodies that merely kiss) is not an overlap.
fn bboxes_overlap(a: &spice_route::Bbox, b: &spice_route::Bbox) -> bool {
    let eps = 1e-6;
    a.x0 < b.x1 - eps && b.x0 < a.x1 - eps && a.y0 < b.y1 - eps && b.y0 < a.y1 - eps
}

/// Collapse a V14-allowed orientation set to one representative per
/// *distinct pin geometry*. Two orientations are equivalent when they
/// place every pin (by number) at the same local offset and outward
/// angle — e.g. a symmetric 2-pin resistor's `(mirror y)` variant is
/// identical to its un-mirrored one, so eight orientations reduce to the
/// few that actually move pins. This shrinks the joint-search product
/// without losing any reachable layout.
///
/// `current` is forced to be the first representative so the "no change"
/// option is always trialled (and wins ties via the lexicographic-first
/// rule in the caller). When no symbol is available, the allowed set is
/// returned unchanged (no geometry to dedupe on).
#[allow(clippy::cast_possible_truncation)]
fn distinct_orientations(
    allowed: &[Orientation],
    current: Orientation,
    symbol: Option<&Symbol>,
) -> Vec<Orientation> {
    let Some(symbol) = symbol else {
        return allowed.to_vec();
    };
    // Geometry key: quantised (number, x, y, angle) per pin, sorted.
    let key_of = |o: Orientation| -> Vec<(String, i64, i64, u16)> {
        let mut v: Vec<(String, i64, i64, u16)> = symbol
            .pins_in(o)
            .into_iter()
            .map(|p| {
                (
                    p.number,
                    (p.x * 1000.0).round() as i64,
                    (p.y * 1000.0).round() as i64,
                    p.angle,
                )
            })
            .collect();
        v.sort();
        v
    };
    let mut seen: Vec<Vec<(String, i64, i64, u16)>> = Vec::new();
    let mut out: Vec<Orientation> = Vec::new();
    // Force `current` first if it is in the allowed set.
    let mut ordered: Vec<Orientation> = Vec::with_capacity(allowed.len());
    if allowed.contains(&current) {
        ordered.push(current);
    }
    for &o in allowed {
        if o != current {
            ordered.push(o);
        }
    }
    for o in ordered {
        let k = key_of(o);
        if !seen.contains(&k) {
            seen.push(k);
            out.push(o);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::bend_count;

    /// A plain L has exactly one bend.
    #[test]
    fn single_l_corner_is_one_bend() {
        let segs = [((0.0, 0.0), (0.0, 10.0)), ((0.0, 10.0), (10.0, 10.0))];
        assert_eq!(bend_count(&segs), 1);
    }

    /// A straight run has no bends however many pieces it is cut into.
    #[test]
    fn collinear_run_has_no_bends() {
        let whole = [((0.0, 0.0), (30.0, 0.0))];
        let split = [
            ((0.0, 0.0), (10.0, 0.0)),
            ((10.0, 0.0), (20.0, 0.0)),
            ((20.0, 0.0), (30.0, 0.0)),
        ];
        assert_eq!(bend_count(&whole), 0);
        assert_eq!(bend_count(&split), 0);
    }

    /// The metric is invariant under re-segmentation of identical ink.
    ///
    /// This is the property that lets `bends` sit in phase 4.5's
    /// acceptance gate: `spice_route::cleanup::split_at_interior_attachments`
    /// is a Tier-0 correctness pass that deliberately splits runs at
    /// same-net attachment points. A raw segment/corner count would move
    /// when it runs, creating optimisation pressure against correctness.
    /// The ink-graph count does not move.
    #[test]
    fn bend_count_is_invariant_under_resegmentation() {
        // A 2-bend staple drawn as 3 maximal runs.
        let coarse = [
            ((0.0, 0.0), (0.0, 10.0)),
            ((0.0, 10.0), (20.0, 10.0)),
            ((20.0, 10.0), (20.0, 0.0)),
        ];
        // Identical ink, every run chopped at interior points.
        let fine = [
            ((0.0, 0.0), (0.0, 4.0)),
            ((0.0, 4.0), (0.0, 10.0)),
            ((0.0, 10.0), (7.0, 10.0)),
            ((7.0, 10.0), (13.0, 10.0)),
            ((13.0, 10.0), (20.0, 10.0)),
            ((20.0, 10.0), (20.0, 6.0)),
            ((20.0, 6.0), (20.0, 0.0)),
        ];
        assert_eq!(bend_count(&coarse), 2);
        assert_eq!(bend_count(&fine), 2, "re-segmentation must not change B");
    }

    /// Overlapping duplicate ink merges into one run, adding no bends.
    #[test]
    fn overlapping_collinear_spans_merge() {
        let segs = [
            ((0.0, 0.0), (10.0, 0.0)),
            ((5.0, 0.0), (15.0, 0.0)), // overlaps the first
            ((15.0, 0.0), (15.0, 5.0)),
        ];
        assert_eq!(bend_count(&segs), 1);
    }

    /// A T-junction is a branch, not a bend: 3 rays, so it is not counted.
    #[test]
    fn t_junction_is_not_a_bend() {
        let segs = [
            ((0.0, 0.0), (20.0, 0.0)),  // trunk
            ((10.0, 0.0), (10.0, 8.0)), // stub off its interior
        ];
        assert_eq!(bend_count(&segs), 0);
    }

    /// A 4-ray crossing is not a bend either.
    #[test]
    fn crossing_is_not_a_bend() {
        let segs = [((0.0, 5.0), (20.0, 5.0)), ((10.0, 0.0), (10.0, 10.0))];
        assert_eq!(bend_count(&segs), 0);
    }
}

/// The two Tier-0 terms of phase 4.5's acceptance predicate.
///
/// The predicate's Tier-1/2 half — `(v13, v12, v5, bends)` scored,
/// `v11` / `overlap` / `v12` guarded — is blind to both ways a
/// reorientation can produce an electrically wrong schematic:
///
///  * **`severed`.** A candidate that boxes a pin in between a foreign
///    pin (V11 blocks one L-route) and a symbol body (V12 blocks the
///    other) makes the router's conflict cascade exhaust its detours and
///    drop the branch — while every metric the predicate *can* see
///    improves.
///  * **`coincident`.** A candidate that lands two foreign nets' pins on
///    one coordinate shorts them, and no router pass can undo it (the
///    router moves wires, not pins). It emits no `v11:` warning, because
///    there is no wire it could detour, and two bodies whose pin tips
///    abut do not strictly overlap — so `v11` and `overlap` both read
///    clean.
///
/// Both are the LEADING keys of the objective, which makes them
/// untradeable downward *and* seekable upward — see [`accepts`].
#[cfg(test)]
mod severed_guard_tests {
    use super::{Measure, accepts, measure, refine_orientations};
    use kicad_symbols::{Library, Orientation, Rotation};
    use spice_layout::{LayoutOptions, Placement, RefinementMeta};

    /// Every fixture the CLI converts, so the "refinement never ships a
    /// severed net" assertion is not a single-fixture accident.
    const FIXTURES: &[&str] = &[
        "rc_lowpass",
        "common_emitter",
        "multivibrator",
        "diff_pair",
        "opamp_inverting_real",
        "opamp_inverting",
        "port_shapes",
        "rc_lowpass_ports",
        "opamp_definition_level",
        "named_rails",
    ];

    /// A neutral baseline to perturb one field at a time.
    fn m(v13: usize, v12: usize, v5: usize, bends: usize, severed: usize) -> Measure {
        Measure {
            v5,
            v11: 0,
            severed,
            coincident: 0,
            overlap: 0,
            v12,
            v13,
            bends,
            offenders: Vec::new(),
            v12_offenders: Vec::new(),
        }
    }

    /// As [`m`], with the Tier-0 pin-on-pin count set too.
    fn m_coinc(
        v13: usize,
        v12: usize,
        v5: usize,
        bends: usize,
        severed: usize,
        coincident: usize,
    ) -> Measure {
        Measure {
            coincident,
            ..m(v13, v12, v5, bends, severed)
        }
    }

    /// Sanity: with `severed` held at zero the predicate is the
    /// pre-existing one — a strict improvement of the objective tuple is
    /// taken.
    #[test]
    fn a_strictly_better_candidate_is_accepted() {
        assert!(accepts(&m(1, 1, 3, 9, 0), &m(0, 0, 1, 4, 0)));
    }

    /// THE regression this module exists for. Identical candidate, but it
    /// severs a net: rejected, however much better everything else gets.
    #[test]
    fn a_candidate_that_severs_a_net_is_rejected() {
        let baseline = m(1, 1, 3, 9, 0);
        let severing = m(0, 0, 1, 4, 1);
        assert!(
            accepts(&baseline, &m(0, 0, 1, 4, 0)),
            "control: the same candidate is accepted when it severs nothing"
        );
        assert!(
            !accepts(&baseline, &severing),
            "Tier-0: a candidate that disconnects a net must never be accepted, \
             no matter how much V13/V12/V5/bends improve"
        );
    }

    /// `severed` is the LEADING key of the objective, and a Tier-0
    /// repair outranks a Tier-1 regression.
    ///
    /// This test previously asserted the opposite ("a `severed`
    /// reduction must not buy a Tier-1 V13 regression"), on the reading
    /// that connectivity is "a floor to hold, never a gradient to
    /// trade". That reading inverts CLAUDE.md's ordering rule, which is
    /// explicitly asymmetric: rule 1 is "never trade a **Tier-0
    /// violation** for any Tier-1/2 gain", i.e. Tier 0 must be satisfied
    /// *first*, and paying Tier 1 to recover Tier 0 is the mandated
    /// direction, not the forbidden one. Held as a floor, `severed`
    /// meant that a placement arriving at phase 4.5 already severed had
    /// no route back: on `shunt_feedback_amp` the phase could only
    /// repair the severance *by accident*, and the accident it found was
    /// rotating `Q1` until its base pin sat on `RE`'s — a Tier-0 short
    /// traded for a Tier-0 severance.
    ///
    /// Note both readings agree whenever the baseline is Tier-0 clean,
    /// which is every fixture in [`FIXTURES`]; the change is reachable
    /// only on a placement the CLI would otherwise refuse to ship.
    #[test]
    fn a_tier0_repair_outranks_a_tier1_regression() {
        let baseline = m(0, 0, 1, 4, 1);
        // Tier-1 V13 regresses 0 → 3; `severed` falls 1 → 0.
        let tier0_repair = m(3, 0, 1, 4, 0);
        assert!(
            accepts(&baseline, &tier0_repair),
            "reconnecting a severed net outranks a Tier-1 V13 cost — CLAUDE.md \
             tier rule 1 forbids the *other* direction, not this one"
        );
    }

    /// The forbidden direction, which is the half that has not changed:
    /// no Tier-1/2 gain buys a Tier-0 loss.
    #[test]
    fn a_tier1_gain_never_buys_a_tier0_loss() {
        let baseline = m(3, 1, 5, 9, 0);
        assert!(
            !accepts(&baseline, &m(0, 0, 0, 0, 1)),
            "a candidate that severs a net is refused however much V13/V12/V5/\
             bends improve"
        );
        assert!(
            !accepts(&baseline, &m_coinc(0, 0, 0, 0, 0, 1)),
            "a candidate that lands two foreign nets' pins on one coordinate is \
             refused however much V13/V12/V5/bends improve"
        );
    }

    /// A pin-on-pin short is invisible to every OTHER field of
    /// [`Measure`], which is why it needed its own term: `v11` counts
    /// router `v11:` *wire* warnings (none are emitted for coincident
    /// pins — there is no wire to detour) and `overlap` uses strict body
    /// interiors (abutting pin tips do not overlap). Without
    /// `coincident` the shorting candidate below is a clean win.
    #[test]
    fn a_pin_on_pin_short_is_invisible_without_the_coincident_term() {
        let baseline = m(1, 1, 3, 9, 0);
        let shorting = m_coinc(0, 0, 1, 4, 0, 1);
        assert_eq!(
            (shorting.v11, shorting.overlap),
            (baseline.v11, baseline.overlap),
            "the short leaves v11 and overlap untouched — that is the hole"
        );
        assert!(
            accepts(
                &baseline,
                &Measure {
                    coincident: 0,
                    ..shorting.clone()
                }
            ),
            "control: the same candidate without the short is accepted"
        );
        assert!(!accepts(&baseline, &shorting));
    }

    /// Build a fixture's placement exactly as the CLI does
    /// (parse → resolve → policy → place), with no layout-cache hint.
    fn fixture(name: &str) -> (Placement, Library, RefinementMeta) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/ dir");
        let mut library = Library::default();
        for lib in [
            "kicad-symbols/tests/fixtures/Device.kicad_sym",
            "kicad-symbols/tests/fixtures/Simulation_SPICE.kicad_sym",
            "kicad-symbols/tests/fixtures/Amplifier_Operational.kicad_sym",
            "kicad-symbols/tests/fixtures/power.kicad_sym",
        ] {
            library = library.merge(Library::from_file(root.join(lib)).expect("load lib"));
        }
        let src =
            std::fs::read_to_string(root.join(format!("spice2kicad/tests/fixtures/{name}.cir")))
                .expect("read fixture");
        let outcome =
            spice_parser::parse(&src, spice_diagnostics::FileId(0)).expect("parse fixture");
        let resolved = spice_resolve::resolve(&outcome.netlist, &library).expect("resolve");
        let (checked, _) = spice_policy::check(resolved).expect("policy check");
        let meta = spice_layout::refinement_meta(&checked, &spice_layout::Hint::default())
            .expect("refinement meta");
        let opts = LayoutOptions {
            refine: true,
            refine_iterations: 200,
            ..LayoutOptions::default()
        };
        let placement =
            spice_layout::place_with_hint(checked, &library, &opts, &spice_layout::Hint::default())
                .expect("place");
        (placement, library, meta)
    }

    /// The demonstrated case, measured on the real fixture rather than
    /// asserted from the ADR: on `common_emitter`, rotating `COUT` to 180
    /// severs a signal net, and the guard rejects it.
    ///
    /// Master's SA does not currently *offer* this candidate a winning
    /// objective tuple, which is exactly why the defect was latent — so
    /// this test measures the candidate directly instead of waiting for
    /// the search to reach it.
    #[test]
    fn common_emitter_cout_rot180_severs_a_net_and_the_guard_rejects_it() {
        let (mut placement, library, meta) = fixture("common_emitter");
        // The candidate arises on the placement phase 4.5 actually
        // works from, i.e. after the pass has settled — that is the
        // state the ADR measured.
        refine_orientations(&mut placement, &library, &meta);
        let i = placement
            .elements
            .iter()
            .position(|e| e.refdes == "COUT")
            .expect("common_emitter has COUT");

        let base = measure(&placement, &library);
        assert_eq!(base.severed, 0, "the settled placement wires up every net");

        let current = placement.elements[i].orientation;
        placement.elements[i].orientation = Orientation {
            rotation: Rotation::R180,
            mirror_y: false,
        };
        let severing = measure(&placement, &library);
        placement.elements[i].orientation = current;

        assert!(
            severing.severed > base.severed,
            "the ADR-17 demonstrated case must still reproduce: rotating COUT to \
             180 boxes its `c` pin between a foreign pin and Q1's body, and the \
             router drops the branch (got severed={})",
            severing.severed
        );
        assert!(
            !accepts(&base, &severing),
            "the severed-net guard must reject the demonstrated COUT rot-180 \
             candidate"
        );
        // …and it must be the `severed` TERM that does the rejecting,
        // not the Tier-1/2 tuple happening to disagree as well. Master's
        // SA is what makes it disagree here — that coincidence is
        // precisely the mask this defect was hiding behind, and any
        // future placer change can remove it. So re-ask the predicate
        // with the REAL measured `severed` count but a Tier-1/2 tail
        // that strictly improves, i.e. the situation the ADR observed
        // under compaction: the leading key must still refuse.
        let tempting = Measure {
            v13: base.v13,
            v12: base.v12,
            v5: base.v5.saturating_sub(1),
            bends: base.bends.saturating_sub(1),
            ..severing
        };
        assert!(
            accepts(
                &base,
                &Measure {
                    severed: base.severed,
                    ..tempting.clone()
                }
            ),
            "control: with connectivity intact this tuple is strictly better and \
             would be accepted"
        );
        assert!(
            !accepts(&base, &tempting),
            "the leading `severed` key — not the Tier-1/2 tail — is what must \
             reject the COUT rot-180 candidate; without it phase 4.5 ships a \
             broken netlist"
        );
    }

    /// Whatever the search picks, phase 4.5 never hands decoration a
    /// placement whose trial route leaves a signal net disconnected.
    #[test]
    fn refinement_never_leaves_a_net_severed_across_fixtures() {
        for name in FIXTURES {
            let (mut placement, library, meta) = fixture(name);
            assert_eq!(
                measure(&placement, &library).severed,
                0,
                "{name}: the placement entering phase 4.5 is already severed"
            );
            refine_orientations(&mut placement, &library, &meta);
            assert_eq!(
                measure(&placement, &library).severed,
                0,
                "{name}: phase 4.5 accepted an orientation that disconnects a net"
            );
        }
    }

    /// The Tier-0 case this reordering was built for, end to end.
    ///
    /// `shunt_feedback_amp` is the one fixture whose SA end-state (at the
    /// default 200 iterations) hands phase 4.5 a placement that is
    /// **already Tier-0 broken** — two signal nets severed. It is still a
    /// defect lock (`spice2kicad/tests/f0_defects.rs`), because the
    /// placement is not repairable by orientation alone; what this test
    /// pins down is that phase 4.5 does not make it *worse*, which is
    /// exactly what it used to do.
    ///
    /// Three assertions, each load-bearing:
    ///
    ///  * the incoming placement really is severed, so this exercises the
    ///    repair path rather than a clean no-op. If the placer is ever
    ///    fixed so the seed arrives clean, this fires and tells you to
    ///    retire the test instead of letting it rot into a tautology;
    ///  * phase 4.5 reconnects the severed nets. Under the old objective
    ///    — Tier-1/2 tuple scored, `severed` held as a floor it could
    ///    never *seek* — it had no reason to;
    ///  * it does **not** reconnect them by shorting. The repair the old
    ///    objective stumbled into was rotating `Q1` until its base pin sat
    ///    exactly on `RE`'s pin 1, merging base into emitter: a Tier-0
    ///    short bought with a Tier-0 severance.
    ///
    /// A `v11` residue remains (the `c` trunk terminating on `RC`'s `vcc`
    /// pin), and the final assertion is its unexpected-pass tripwire: the
    /// day it reaches zero the fixture is convertible and belongs in the
    /// graded suite.
    #[test]
    fn shunt_feedback_amp_tier0_repair_does_not_short() {
        let (mut placement, library, meta) = fixture("shunt_feedback_amp");
        let before = measure(&placement, &library);
        assert!(
            before.severed > 0,
            "the placement entering phase 4.5 is no longer severed — the SA-side \
             defect this test exercises is gone, so retire the test (or re-point \
             it) instead of leaving a tautology behind"
        );
        assert_eq!(
            before.coincident, 0,
            "the placer must not hand phase 4.5 a pin-on-pin short"
        );

        refine_orientations(&mut placement, &library, &meta);

        let after = measure(&placement, &library);
        assert_eq!(after.severed, 0, "phase 4.5 must repair the severed nets");
        assert_eq!(
            after.coincident, 0,
            "…and must not repair them by shorting two pins together"
        );
        assert!(
            after.v11 > 0,
            "UNEXPECTED PASS: no V11 residue is left on `shunt_feedback_amp`. The \
             remaining Tier-0 defect is FIXED — promote the fixture out of \
             tests/f0_defects.rs into the graded suite and re-point this test."
        );
    }
}
