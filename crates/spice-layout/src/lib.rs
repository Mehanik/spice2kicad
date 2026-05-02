//! Auto-placer: `CheckedNetlist + Library -> Placement`.
//!
//! Two pipelines share the same crate:
//!
//! * **Stage 1** ([`place`]): trivial deterministic placement that
//!   honours hard constraints from `align` and `place`. Produces a
//!   valid (if ugly) layout in O(n).
//! * **Stage 3** ([`place_with`] with [`LayoutOptions::refine`]):
//!   stage-1 seed → Fruchterman-Reingold continuous seeding → discrete
//!   simulated-annealing refinement. Minimises the cost in
//!   [`cost::CostBreakdown`].
//!
//! See `docs/layout-roadmap.md` §7 (sequencing) and `docs/layout-adr.md`
//! ADR-3 (orientation/mirroring — stage 3 implements 4-rotation moves;
//! mirror moves are deferred), ADR-4 (sidecar — not yet wired), and
//! ADR-7 (property-test strategy).
//!
//! # Diagnostic codes emitted
//!
//! - **E007** — internal: `place` could not be resolved after the
//!   policy pass (worklist stalled). Should never fire on inputs that
//!   passed `spice_policy::check`; if it does, it's a bug.

#![forbid(unsafe_code)]

mod archetype;
pub mod cost;
mod solver;
mod symmetry;

pub use solver::LayoutOptions;

use std::collections::{HashMap, HashSet};

use kicad_symbols::{Library, Orientation, Symbol};
use spice_diagnostics::{Diagnostic, Label, Span};
use spice_policy::CheckedNetlist;
use spice_resolve::{Axis, Relation, Value};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A point in grid coordinates. The KiCad schematic grid is 1.27 mm
/// (50 mil); a `GridPoint` always represents an integer multiple of
/// that step, so by construction every placement is grid-snapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridPoint {
    pub x: i32,
    pub y: i32,
}

impl GridPoint {
    /// One grid step in millimetres (KiCad schematic grid: 50 mil).
    pub const STEP_MM: f64 = 1.27;

    #[must_use]
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Convert to millimetres.
    #[must_use]
    pub fn to_mm(self) -> (f64, f64) {
        (
            f64::from(self.x) * Self::STEP_MM,
            f64::from(self.y) * Self::STEP_MM,
        )
    }
}

/// A single element with a final, grid-snapped position and
/// orientation.
#[derive(Debug, Clone)]
pub struct PlacedElement {
    pub refdes: String,
    pub lib_id: String,
    pub origin: GridPoint,
    pub orientation: Orientation,
    /// SPICE node names in original terminal order. Carried through so
    /// the schematic emitter can drop a label at each pin's world
    /// position (the only mechanism by which KiCad infers connectivity
    /// in the absence of explicit wires).
    pub nodes: Vec<String>,
    /// KiCad pin numbers indexed by SPICE terminal (parallel to
    /// [`nodes`]). `pin_mapping[i]` is the KiCad pin number
    /// corresponding to SPICE terminal `i + 1`.
    pub pin_mapping: Vec<String>,
    /// The element's SPICE value, formatted as the original token
    /// (e.g. `"1k"`, `"100n"`, `"QGENERIC"`). Carried so the schematic
    /// emitter can populate the symbol's `Value` property and the
    /// round-trip through kicad-cli preserves component values.
    pub value: Option<String>,
}

impl PlacedElement {
    /// Each pin of this element in *world* millimetre coordinates,
    /// taking the placed origin and orientation into account. Useful
    /// for property tests that assert pin-anchored relations
    /// (`docs/layout-roadmap.md` §2).
    ///
    /// Returns `(number, x_mm, y_mm)` per pin, in the symbol's
    /// declared pin order.
    #[must_use]
    pub fn world_pin_mm(&self, symbol: &Symbol) -> Vec<(String, f64, f64)> {
        let (ox, oy) = self.origin.to_mm();
        symbol
            .pins_in(self.orientation)
            .into_iter()
            .map(|p| (p.number, ox + p.x, oy + p.y))
            .collect()
    }
}

/// The output of stage 1.
#[derive(Debug, Clone, Default)]
pub struct Placement {
    pub elements: Vec<PlacedElement>,
    // Future: cluster bounding boxes, sheet hierarchy. Stage 1 carries
    // only the per-element list.
}

/// Render a parsed SPICE [`Value`] back to its source-equivalent token.
///
/// The schematic emitter uses this to populate the symbol's `Value`
/// property so the round-trip through kicad-cli preserves component
/// values. This is a coarse one-way formatter — it does not attempt
/// to reconstruct engineering-suffixed forms (`1k`, `100n`); a numeric
/// `1000` and the original `1k` both come out as decimal here. The
/// canonicaliser in the round-trip tests collapses these to the same
/// equivalence class, so topology-level checks still pass.
fn format_value(v: &Value) -> String {
    match v {
        Value::Number(n) => format!("{n}"),
        Value::String(s) => s.clone(),
        Value::Expr(e) => e.clone(),
    }
}

// ---------------------------------------------------------------------------
// Geometry constants
// ---------------------------------------------------------------------------

/// Width of a "cell" each element occupies, in grid units. Generous
/// enough for `Device:R`, `Device:C`, `Device:Q_NPN_BCE` without
/// computing real bounding boxes (a stage-3 problem).
pub(crate) const CELL_W: i32 = 6;
/// Height of a cell, in grid units.
pub(crate) const CELL_H: i32 = 6;
/// One-cell gap between an aligned cluster's anchor row/column and the
/// next, so clusters do not pile up at the origin.
const CLUSTER_GAP: i32 = 1;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the stage-1 placer with default options (no refinement).
pub fn place(checked: CheckedNetlist, library: &Library) -> Result<Placement, Vec<Diagnostic>> {
    place_with(checked, library, &LayoutOptions::default())
}

/// Run the placer.
///
/// With [`LayoutOptions::refine`] disabled (default), this is the
/// stage-1 deterministic placer. With refinement enabled, the stage-1
/// output is fed to the FR seeder and the SA refiner; constrained
/// (`align`/`place`-fixed) elements remain pinned through both passes.
// Takes the netlist by value for parity with `place`. The body only
// reads it, but the by-value signature mirrors `spice_policy::check`
// and lets future callers stop holding the resolved netlist after
// placement.
#[allow(clippy::needless_pass_by_value)]
pub fn place_with(
    checked: CheckedNetlist,
    library: &Library,
    opts: &LayoutOptions,
) -> Result<Placement, Vec<Diagnostic>> {
    let (mut placement, mut pinned) = place_seed(&checked)?;
    // V6: overlay topology archetype seeds (if any matched) on top of
    // the stage-1 placement before the V5 orientation pass runs. The
    // archetype owns the *origins* of its matched elements; V5 still
    // chooses orientations against neighbours that aren't pinned.
    let seeds = archetype::detect_and_seed(&checked);
    if !seeds.is_empty() {
        archetype::apply_seeds(&mut placement, &mut pinned, &seeds);
    }
    // V7: detect structural symmetry in the netlist and mirror paired
    // elements about a common vertical axis. Runs after V6 archetype
    // seeding so the axis is computed from a topology-aware base
    // layout when one exists, and before V5 orientation so the pinned
    // pair geometry guides the orientation chooser for the rest of
    // the circuit.
    if let Some(plan) = symmetry::detect_pairs(&checked) {
        symmetry::apply(&mut placement, &mut pinned, &plan);
    }
    pick_orientations(&mut placement, &pinned, &checked);
    if !opts.refine {
        return Ok(placement);
    }
    Ok(solver::refine(placement, &pinned, &checked, library, opts))
}

/// V5: pin-facing orientation pass.
///
/// For each element whose origin is **not** pinned by `align` or
/// `place`, pick the orientation in [`Orientation::ALL`] that
/// minimises the sum of Manhattan distances over each shared-net pin
/// pair against neighbours that have already been oriented (in
/// deterministic index order). Origins are held fixed; only the
/// orientation varies. Tie-break: prefer [`Orientation::IDENTITY`],
/// then earlier in [`Orientation::ALL`] — this keeps tests that
/// assume identity defaults stable when the V5 score is flat.
///
/// Elements whose origin is fixed by `align`/`place` keep identity
/// orientation: their position was solved against identity and
/// changing it would invalidate the pin-anchored math in
/// [`solve_place`].
#[allow(clippy::similar_names)] // ox_i/oy_i, ox_j/oy_j: i/j identify the two elements in a pair.
fn pick_orientations(placement: &mut Placement, pinned: &[bool], checked: &CheckedNetlist) {
    let n = placement.elements.len();
    if n == 0 {
        return;
    }

    // Build adjacency: element pairs sharing a non-ground net. We
    // also remember which (terminal_idx pairs) each adjacency uses,
    // so the scorer can directly compare connecting-pin world
    // positions.
    //
    // adjacency[i] = Vec<(j, term_i, term_j)>
    let mut adjacency: Vec<Vec<(usize, usize, usize)>> = vec![Vec::new(); n];
    // Map net name -> Vec<(element_idx, terminal_idx)>.
    let mut net_pins: HashMap<&str, Vec<(usize, usize)>> = HashMap::new();
    for (i, elem) in checked.elements.iter().enumerate() {
        for (term_idx, node_name) in elem.nodes.iter().enumerate() {
            if node_name == "0" {
                continue;
            }
            net_pins
                .entry(node_name.as_str())
                .or_default()
                .push((i, term_idx));
        }
    }
    for pins in net_pins.values() {
        for a in 0..pins.len() {
            for b in (a + 1)..pins.len() {
                let (i, ti) = pins[a];
                let (j, tj) = pins[b];
                if i == j {
                    continue;
                }
                adjacency[i].push((j, ti, tj));
                adjacency[j].push((i, tj, ti));
            }
        }
    }

    // Iterate until orientations stabilise or we hit the pass cap.
    // First pass establishes initial orientations (each element sees
    // earlier-indexed neighbours' identity defaults); subsequent
    // passes re-evaluate each element against the now-decided
    // orientations of its later-indexed neighbours. Two passes are
    // enough for small fixtures; cap at 8 to bound worst-case cost.
    let max_passes = 8;
    for _ in 0..max_passes {
        let mut changed = false;
        for i in 0..n {
            if pinned[i] {
                continue;
            }
            let symbol_i = &checked.elements[i].symbol;
            let pin_mapping_i = &checked.elements[i].pin_mapping;

            // After the first pass every neighbour has an orientation
            // worth scoring against. On the first pass, later-indexed
            // neighbours score against their identity defaults — also
            // a valid starting point.
            let neighbours: &[(usize, usize, usize)] = &adjacency[i];

            if neighbours.is_empty() {
                continue;
            }

            let mut best: Option<(i64, usize, Orientation)> = None;
            for (rank, &orient) in Orientation::ALL.iter().enumerate() {
                let pins_i = symbol_i.pins_in(orient);
                let (ox_i, oy_i) = placement.elements[i].origin.to_mm();
                let mut score: f64 = 0.0;
                for &(j, ti, tj) in neighbours {
                    let symbol_j = &checked.elements[j].symbol;
                    let pin_mapping_j = &checked.elements[j].pin_mapping;
                    let pins_j = symbol_j.pins_in(placement.elements[j].orientation);
                    let (ox_j, oy_j) = placement.elements[j].origin.to_mm();

                    let Some(kicad_pin_i) = pin_mapping_i.get(ti) else {
                        continue;
                    };
                    let Some(kicad_pin_j) = pin_mapping_j.get(tj) else {
                        continue;
                    };
                    let Some(p_i) = pins_i.iter().find(|p| &p.number == kicad_pin_i) else {
                        continue;
                    };
                    let Some(p_j) = pins_j.iter().find(|p| &p.number == kicad_pin_j) else {
                        continue;
                    };
                    let xi = ox_i + p_i.x;
                    let yi = oy_i + p_i.y;
                    let xj = ox_j + p_j.x;
                    let yj = oy_j + p_j.y;
                    score += (xi - xj).abs() + (yi - yj).abs();
                }
                // Convert to integer (mm * 1000) for stable comparison
                // and deterministic tie-break. Pin coords are grid-aligned.
                #[allow(clippy::cast_possible_truncation)]
                let score_int = (score * 1000.0).round() as i64;
                let identity_rank = if orient == Orientation::IDENTITY {
                    0
                } else {
                    rank + 1
                };
                let candidate = (score_int, identity_rank, orient);
                let take = match best {
                    None => true,
                    Some((bs, br, _)) => {
                        candidate.0 < bs || (candidate.0 == bs && candidate.1 < br)
                    }
                };
                if take {
                    best = Some(candidate);
                }
            }

            if let Some((_, _, orient)) = best
                && placement.elements[i].orientation != orient
            {
                placement.elements[i].orientation = orient;
                changed = true;
            }
        }
        // Detect convergence: if no orientation moved this pass we're done.
        if !changed {
            break;
        }
    }
}

/// Stage-1 placer body: returns the seed placement plus a per-element
/// `pinned` mask (`true` for elements whose position is fixed by an
/// `align` or `place` directive).
// Placer is a four-phase pipeline (init / align / place / auto-fill).
// Splitting it into helpers per phase obscures the shared state
// (`placed`, `fixed`) and the careful ordering between phases. Allow
// the long body here.
#[allow(clippy::too_many_lines)]
fn place_seed(checked: &CheckedNetlist) -> Result<(Placement, Vec<bool>), Vec<Diagnostic>> {
    let CheckedNetlist {
        elements,
        align,
        place,
        subckts: _,
        sheet_instances: _,
    } = checked;

    // Index elements by refdes for O(1) lookups.
    let refdes_to_index: HashMap<String, usize> = elements
        .iter()
        .enumerate()
        .map(|(i, e)| (e.refdes.clone(), i))
        .collect();

    // Initial state: every element at the origin, identity orientation.
    let mut placed: Vec<PlacedElement> = elements
        .iter()
        .map(|e| PlacedElement {
            refdes: e.refdes.clone(),
            lib_id: e.lib_id.clone(),
            origin: GridPoint::new(0, 0),
            orientation: Orientation::IDENTITY,
            nodes: e.nodes.clone(),
            pin_mapping: e.pin_mapping.clone(),
            value: e.value.as_ref().map(format_value),
        })
        .collect();

    // `fixed[i] == true` once element `i`'s origin has been finalised
    // by an `align` or `place` directive (or auto-fill).
    let mut fixed: Vec<bool> = vec![false; elements.len()];

    // ---- Phase 2: align ---------------------------------------------------
    // Each cluster gets its own anchor on the diagonal so clusters do
    // not collide. Within a cluster, members spread along the cluster
    // axis at one-cell stride.
    for (cluster_index, spec) in align.iter().enumerate() {
        // Cluster index `i` starts at `((i+1) * stride, (i+1) * stride)`
        // — leaving the row/column at (0, 0) free for "default-pinned"
        // place anchors (see phase 3 below).
        let cluster_index_i32 = i32::try_from(cluster_index + 1).unwrap_or(i32::MAX);
        let anchor_x = cluster_index_i32 * (CELL_W + CLUSTER_GAP);
        let anchor_y = cluster_index_i32 * (CELL_H + CLUSTER_GAP);
        for (member_index, refdes) in spec.refdes.iter().enumerate() {
            let member_index_i32 = i32::try_from(member_index).unwrap_or(i32::MAX);
            let Some(&idx) = refdes_to_index.get(refdes) else {
                // Policy pass guarantees every refdes is known; defensive.
                continue;
            };
            if fixed[idx] {
                // Earlier `align` already pinned this element; spec
                // §5 says later phases never override earlier. (And
                // earlier within the same phase: first-cluster wins
                // when an element appears in multiple align clusters.)
                continue;
            }
            let (x, y) = match spec.axis {
                Axis::Horizontal => (
                    anchor_x + member_index_i32 * (CELL_W + CLUSTER_GAP),
                    anchor_y,
                ),
                Axis::Vertical => (
                    anchor_x,
                    anchor_y + member_index_i32 * (CELL_H + CLUSTER_GAP),
                ),
            };
            placed[idx].origin = GridPoint::new(x, y);
            fixed[idx] = true;
        }
    }

    // ---- Phase 3: place ---------------------------------------------------
    // Worklist: process directives whose anchor is already fixed,
    // iterate until fixpoint. The policy pass guarantees no axis
    // cycles, so a topological ordering exists.
    // Build a quick "is this refdes the target of a place directive"
    // set so we can distinguish anchors-fixed-by-default from anchors
    // pending-resolution.
    let place_targets: HashSet<&str> = place.iter().map(|p| p.refdes.as_str()).collect();

    let mut pending: Vec<usize> = (0..place.len()).collect();
    let mut diags: Vec<Diagnostic> = Vec::new();

    // Counter for "default-pinned" free anchors. We give each its
    // own column at y=0 (the row align clusters deliberately avoid),
    // so two unrelated chains don't collide at the origin.
    let mut free_anchor_col: i32 = 0;

    loop {
        let before = pending.len();
        let mut still_pending: Vec<usize> = Vec::with_capacity(before);
        for pi in pending.drain(..) {
            let spec = &place[pi];
            let (Some(&b_idx), Some(&a_idx)) = (
                refdes_to_index.get(&spec.refdes),
                refdes_to_index.get(&spec.anchor),
            ) else {
                // Policy pass guarantees these refdeses exist; skip.
                continue;
            };

            // Anchor must be resolved before we can solve for `b`.
            // It's resolved if it's already `fixed` *or* if it isn't
            // itself a place target (so its default (0,0) is final).
            if !fixed[a_idx] {
                if place_targets.contains(spec.anchor.as_str()) {
                    still_pending.push(pi);
                    continue;
                }
                // Free-floating anchor: pin at the next free
                // column on the y=0 row.
                placed[a_idx].origin = GridPoint::new(free_anchor_col * (CELL_W + CLUSTER_GAP), 0);
                free_anchor_col += 1;
                fixed[a_idx] = true;
            }

            let new_origin = solve_place(
                spec.relation,
                placed[a_idx].origin,
                placed[a_idx].orientation,
                &elements[a_idx].symbol,
                placed[b_idx].orientation,
                &elements[b_idx].symbol,
            );
            placed[b_idx].origin = new_origin;
            fixed[b_idx] = true;
        }
        pending = still_pending;
        if pending.is_empty() {
            break;
        }
        if pending.len() == before {
            // Stalled. Should never happen post-policy.
            for pi in pending {
                let spec = &place[pi];
                push_err(
                    &mut diags,
                    "E007",
                    format!(
                        "internal: could not resolve `place` for `{}` (anchor `{}` never became fixed)",
                        spec.refdes, spec.anchor
                    ),
                    spec.span,
                );
            }
            return Err(diags);
        }
    }

    // ---- Phase 4: auto-fill ----------------------------------------------
    // Any element not touched by phases 2 or 3 lands in a "default
    // row" below the constrained content.
    let max_y = placed
        .iter()
        .zip(fixed.iter())
        .filter_map(|(p, f)| if *f { Some(p.origin.y) } else { None })
        .max()
        .unwrap_or(0);
    let fill_y = max_y + (CELL_H + CLUSTER_GAP) * 2;
    let mut fill_col: i32 = 0;
    for (i, is_fixed) in fixed.iter().enumerate() {
        if *is_fixed {
            continue;
        }
        placed[i].origin = GridPoint::new(fill_col * (CELL_W + CLUSTER_GAP), fill_y);
        fill_col += 1;
    }

    Ok((Placement { elements: placed }, fixed))
}

// ---------------------------------------------------------------------------
// Pin-anchored placement math
// ---------------------------------------------------------------------------

/// Solve for the origin of `b` such that the connecting pins of `a`
/// and `b` satisfy [`Relation`] with a one-cell gap between the
/// symbols' bounding boxes.
///
/// All math is in *grid units*. Pin offsets come from
/// [`Symbol::pins_in`] in millimetres; we round to grid units. KiCad
/// library symbols put their pins on grid intersections, so the
/// rounding is exact.
fn solve_place(
    relation: Relation,
    a_origin: GridPoint,
    a_orient: Orientation,
    a_symbol: &Symbol,
    b_orient: Orientation,
    b_symbol: &Symbol,
) -> GridPoint {
    let a_pins = pin_offsets_grid(a_symbol, a_orient);
    let b_pins = pin_offsets_grid(b_symbol, b_orient);

    match relation {
        Relation::RightOf => {
            // Pick `a`'s rightmost pin (max-x, tie min-y) and `b`'s
            // leftmost pin (min-x, tie min-y). Want:
            //   b.origin.x + b_left.x = a.origin.x + a_right.x + CELL_W
            //   b.origin.y + b_left.y = a.origin.y + a_right.y
            let (ax, ay) = pick(&a_pins, |p| (-p.0, p.1));
            let (bx, by) = pick(&b_pins, |p| (p.0, p.1));
            GridPoint::new(a_origin.x + ax + CELL_W - bx, a_origin.y + ay - by)
        }
        Relation::LeftOf => {
            // `b`'s rightmost pin lands one CELL_W left of `a`'s leftmost.
            //   b.origin.x + b_right.x = a.origin.x + a_left.x - CELL_W
            //   shared Y on the connecting pins
            let (ax, ay) = pick(&a_pins, |p| (p.0, p.1));
            let (bx, by) = pick(&b_pins, |p| (-p.0, p.1));
            GridPoint::new(a_origin.x + ax - CELL_W - bx, a_origin.y + ay - by)
        }
        Relation::Above => {
            // `b` sits above `a`: b's bottom pin connects to a's top.
            //   b.origin.y + b_bottom.y = a.origin.y + a_top.y + CELL_H
            //   shared X.
            let (ax, ay) = pick(&a_pins, |p| (-p.1, p.0));
            let (bx, by) = pick(&b_pins, |p| (p.1, p.0));
            GridPoint::new(a_origin.x + ax - bx, a_origin.y + ay + CELL_H - by)
        }
        Relation::Below => {
            //   b.origin.y + b_top.y = a.origin.y + a_bottom.y - CELL_H
            let (ax, ay) = pick(&a_pins, |p| (p.1, p.0));
            let (bx, by) = pick(&b_pins, |p| (-p.1, p.0));
            GridPoint::new(a_origin.x + ax - bx, a_origin.y + ay - CELL_H - by)
        }
    }
}

/// Pin offsets in grid units (rounded to the nearest grid step).
fn pin_offsets_grid(symbol: &Symbol, orient: Orientation) -> Vec<(i32, i32)> {
    symbol
        .pins_in(orient)
        .into_iter()
        .map(|p| (mm_to_grid(p.x), mm_to_grid(p.y)))
        .collect()
}

#[allow(clippy::cast_possible_truncation)] // pin coords are bounded; KiCad symbols fit in i32 grid units.
fn mm_to_grid(v_mm: f64) -> i32 {
    (v_mm / GridPoint::STEP_MM).round() as i32
}

/// Pick the pin minimising `key`; returns `(x, y)` in grid units.
/// Tie-break is the natural ordering of `key`'s output.
fn pick<K: Ord, F: Fn(&(i32, i32)) -> K>(pins: &[(i32, i32)], key: F) -> (i32, i32) {
    *pins
        .iter()
        .min_by_key(|p| key(p))
        .expect("symbol has at least one pin")
}

// ---------------------------------------------------------------------------
// Diagnostic helpers
// ---------------------------------------------------------------------------

fn push_err(diags: &mut Vec<Diagnostic>, code: &'static str, message: String, span: Option<Span>) {
    let primary = span.map_or_else(
        || Label::new(Span::point(spice_diagnostics::FileId(0), 0), ""),
        |s| Label::new(s, ""),
    );
    let mut d = Diagnostic::error(code, message, primary);
    if span.is_none() {
        d = d.with_help("source span unavailable for this diagnostic");
    }
    diags.push(d);
}
