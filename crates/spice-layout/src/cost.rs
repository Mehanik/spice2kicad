//! Stage-2 cost function: scores a [`Placement`] against a
//! [`CheckedNetlist`] on multiple readability axes.
//!
//! The functions here are pure: they take `(placement, checked,
//! library)` and return an `f64`. Stage 3 (FR seeding + simulated
//! annealing) will minimise the weighted sum produced by [`total`].
//!
//! See `docs/layout-roadmap.md` §5 for the cost-function spec.
//!
//! # Coordinate system
//!
//! All distance terms work in *millimetres*, not grid units. The
//! roadmap suggests grid units, but stage 2 picks mm so the weight
//! constants below have a stable physical basis as we add or remove
//! components in later stages. (Conversion between the two is a
//! constant factor, so it does not affect ordering.)
//!
//! # Stage-2 limitations
//!
//! * **Overlap** is approximated as the area of intersection of fixed
//!   `CELL_W * CELL_H` cell bounding boxes around each origin. Real
//!   per-symbol bounding boxes land alongside the SA pass that needs
//!   them (TODO: stage 3+).
//! * **Crossings** approximate each multi-pin net as the minimum
//!   spanning tree (in mm) connecting its pins by straight lines, and
//!   count straight-segment intersections across distinct net pairs.
//!   An L-shaped router lives in `spice-route` (v0.2); this is a
//!   placeholder until then.
//! * **Constraint violation** uses *origin* coordinates for `align`
//!   variance. Once orientation/mirror search lands (stage 5, ADR-3),
//!   we will switch to connecting-pin coordinates; for now origin Y
//!   equals pin Y up to a constant offset within an `align` cluster
//!   (all members use the identity orientation), so the variance is
//!   identical.
//! * `non_orthogonal_segments` from roadmap §5 is **not** computed
//!   in stage 2 — it requires continuous-coord output and will be
//!   added alongside the orthogonal-router pass.
//!
//! # Signal-flow scope
//!
//! `signal_flow` is computed only inside `.subckt` blocks, where the
//! port list gives an unambiguous input → output direction. For
//! top-level netlists (no subckts) the term is `0`. Heuristic per the
//! stage-3 plan: the first port is treated as the sole input net and
//! the last port as the sole output net; intermediate ports do not
//! contribute.

use std::collections::{HashMap, HashSet};

use kicad_symbols::Library;
use spice_policy::CheckedNetlist;
use spice_resolve::{Axis, ElementRole, Relation, ResolvedElement};

use crate::bands::{Band, BandAssignment, assign_y_bands};
use crate::layers::assign_x_layers;
use crate::net_class::{NetClass, classify_nets};
use crate::{CELL_H, CELL_W, GridPoint, PlacedElement, Placement};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Per-component cost breakdown. Comparing two breakdowns requires
/// weights; callers must go through [`total`] (no `PartialOrd` impl
/// on purpose).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostBreakdown {
    /// Half-perimeter wirelength, summed over all non-ground nets, in mm.
    pub hpwl: f64,
    /// Cell-bbox overlap area summed over all element pairs, in mm².
    pub overlap: f64,
    /// Straight-line MST-edge crossing count across distinct net pairs.
    pub crossings: f64,
    /// `align`-variance + `place`-residual penalty, in mm² units.
    pub constraint_violation: f64,
    /// Hinged squared distance of power-rail pins below the top edge
    /// and of ground pins above the bottom edge, in mm².
    pub rail_direction: f64,
    /// Hinged squared distance of subckt input pins right of the left
    /// edge and of subckt output pins left of the right edge, in mm².
    pub signal_flow: f64,
    /// Clamp-distance² of elements outside their assigned band (Top/Mid/Bot).
    pub band_misalignment: f64,
    /// Squared distance of Mid-band elements from soft Y target frac.
    pub soft_y_residual: f64,
    /// Sum of (x_pred - x_self)² for layer-order violations on the signal DAG.
    pub layer_order: f64,
    /// Cheap proxy for wire crossings: count of net-bbox cross-pairs across distinct nets.
    pub net_bbox_crossings: f64,
    /// Sum of (yu - yd)² over each pair of elements whose
    /// `soft_y_target_frac` orders them but the placement does not.
    pub band_inversion: f64,
}

/// Linear-combination weights for [`total`].
#[derive(Debug, Clone, Copy)]
pub struct CostWeights {
    pub hpwl: f64,
    pub overlap: f64,
    pub crossings: f64,
    pub constraint_violation: f64,
    pub rail_direction: f64,
    pub signal_flow: f64,
    pub band_misalignment: f64,
    pub soft_y_residual: f64,
    pub layer_order: f64,
    pub net_bbox_crossings: f64,
    pub band_inversion: f64,
}

impl CostWeights {
    /// Recommended starting weights per `docs/layout-roadmap.md` §5:
    /// constraint violations and crossings dominate, HPWL is a tiny
    /// regulariser. These are a *first guess* to be tuned empirically
    /// against `examples/` once stage 3 ships — do not over-fit
    /// fixtures to them.
    pub const DEFAULT: Self = Self {
        crossings: 100.0,
        constraint_violation: 1000.0,
        overlap: 200.0,
        hpwl: 1.0,
        // not yet tuned — see docs/layout-roadmap.md §7
        rail_direction: 200.0,
        // not yet tuned — see docs/layout-roadmap.md §7
        signal_flow: 25.0,
        // Stage-3 structural-layered terms (T6, calibrated in T8).
        // soft_y_residual carries the rail-ordering signal in
        // fixtures with no Top/Bot-only band elements (e.g.
        // common_emitter — VCC sits in Mid because it touches both
        // power and ground), so it must dominate HPWL.
        band_misalignment: 50.0,
        soft_y_residual: 50.0,
        layer_order: 20.0,
        net_bbox_crossings: 4.0,
        band_inversion: 100.0,
    };
}

/// Compute every cost component for `placement`.
///
/// Pure: same input, same output. Element ordering inside `placement`
/// must match the index ordering of `checked.elements` (this is what
/// stage-1 [`crate::place`] returns).
#[must_use]
pub fn breakdown(
    placement: &Placement,
    checked: &CheckedNetlist,
    library: &Library,
) -> CostBreakdown {
    let pin_world = collect_pin_world(placement, &checked.elements, library);
    let nets = build_nets(&checked.elements, &pin_world);

    CostBreakdown {
        hpwl: hpwl(&nets),
        overlap: overlap(&placement.elements),
        crossings: crossings(&nets),
        constraint_violation: constraint_violation(placement, checked, library),
        rail_direction: rail_direction(&checked.elements, &pin_world),
        signal_flow: signal_flow(&checked.elements, &pin_world, &checked.subckts),
        band_misalignment: band_misalignment(placement, checked, None),
        soft_y_residual: soft_y_residual(placement, checked),
        layer_order: layer_order(placement, checked),
        net_bbox_crossings: net_bbox_crossings(&nets),
        band_inversion: band_inversion(placement, checked),
    }
}

/// Weighted sum of `breakdown`'s components.
#[must_use]
pub fn total(breakdown: &CostBreakdown, weights: &CostWeights) -> f64 {
    weights.hpwl * breakdown.hpwl
        + weights.overlap * breakdown.overlap
        + weights.crossings * breakdown.crossings
        + weights.constraint_violation * breakdown.constraint_violation
        + weights.rail_direction * breakdown.rail_direction
        + weights.signal_flow * breakdown.signal_flow
        + weights.band_misalignment * breakdown.band_misalignment
        + weights.soft_y_residual * breakdown.soft_y_residual
        + weights.layer_order * breakdown.layer_order
        + weights.net_bbox_crossings * breakdown.net_bbox_crossings
        + weights.band_inversion * breakdown.band_inversion
}

// ---------------------------------------------------------------------------
// Pin-world cache
// ---------------------------------------------------------------------------

/// `pin_world[element_index]` lists `(kicad_pin_number, x_mm, y_mm)`
/// for that element's pins, taking origin and orientation into
/// account.
type PinWorld = Vec<Vec<(String, f64, f64)>>;

fn collect_pin_world(
    placement: &Placement,
    elements: &[ResolvedElement],
    library: &Library,
) -> PinWorld {
    placement
        .elements
        .iter()
        .map(|pe| {
            // Prefer the resolver's owned symbol clone (always
            // present on a `CheckedNetlist`); fall back to the
            // library lookup if for some reason it is absent.
            let symbol = elements.iter().find(|e| e.refdes == pe.refdes).map_or_else(
                || library.lookup(&pe.lib_id).cloned().expect("symbol in lib"),
                |re| re.symbol.clone(),
            );
            pe.world_pin_mm(&symbol)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Net detection
// ---------------------------------------------------------------------------

/// A net's pin positions in world mm coords.
#[derive(Debug, Clone)]
struct Net {
    /// SPICE node name (informational).
    #[allow(dead_code)]
    name: String,
    /// `(x_mm, y_mm)` for each pin connected to this net.
    pins: Vec<(f64, f64)>,
}

/// Build the net → pin-positions map. Skips ground (`"0"`).
///
/// `terminal_index` (0-based) on element `i` corresponds to KiCad pin
/// `pin_mapping[terminal_index]` and SPICE node
/// `nodes[terminal_index]`. We look up the (x, y) of that KiCad pin
/// number in `pin_world[i]`.
///
/// TODO(v0.2): generalise the ground filter — `.global vss` etc. are
/// not parsed yet. For v0.1 hard-coding `"0"` matches Berkeley SPICE
/// semantics.
fn build_nets(elements: &[ResolvedElement], pin_world: &PinWorld) -> Vec<Net> {
    let mut nets: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
    for (i, elem) in elements.iter().enumerate() {
        for (term_idx, node_name) in elem.nodes.iter().enumerate() {
            if node_name == "0" {
                continue;
            }
            let Some(kicad_pin) = elem.pin_mapping.get(term_idx) else {
                continue;
            };
            let Some(world_pins) = pin_world.get(i) else {
                continue;
            };
            let Some(&(_, x, y)) = world_pins.iter().find(|(num, _, _)| num == kicad_pin) else {
                continue;
            };
            nets.entry(node_name.clone()).or_default().push((x, y));
        }
    }
    let mut out: Vec<Net> = nets
        .into_iter()
        .map(|(name, pins)| Net { name, pins })
        .collect();
    // Stable order so output is deterministic across HashMap iteration.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

// ---------------------------------------------------------------------------
// HPWL
// ---------------------------------------------------------------------------

/// Sum of `(max_x - min_x) + (max_y - min_y)` over all non-ground
/// nets. A net with one or zero pins contributes 0.
fn hpwl(nets: &[Net]) -> f64 {
    let mut total = 0.0;
    for net in nets {
        if net.pins.len() < 2 {
            continue;
        }
        let (mut min_x, mut max_x) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut min_y, mut max_y) = (f64::INFINITY, f64::NEG_INFINITY);
        for &(x, y) in &net.pins {
            if x < min_x {
                min_x = x;
            }
            if x > max_x {
                max_x = x;
            }
            if y < min_y {
                min_y = y;
            }
            if y > max_y {
                max_y = y;
            }
        }
        total += (max_x - min_x) + (max_y - min_y);
    }
    total
}

// ---------------------------------------------------------------------------
// Overlap
// ---------------------------------------------------------------------------

/// Area of cell-bbox intersection summed over all element pairs.
///
/// Each element occupies a `CELL_W × CELL_H` grid-unit box centred on
/// its origin. Two non-overlapping cells contribute 0; identical
/// origins contribute `CELL_W * CELL_H` (in mm²).
///
/// TODO(stage 3+): replace with real per-symbol bounding-box overlap
/// once `kicad-symbols` exposes the symbol bbox.
#[allow(clippy::similar_names)] // half_w/h_mm and a/b coordinate pairs are conventional.
fn overlap(elements: &[PlacedElement]) -> f64 {
    let cell_w_mm = f64::from(CELL_W) * GridPoint::STEP_MM;
    let cell_h_mm = f64::from(CELL_H) * GridPoint::STEP_MM;

    let mut total = 0.0;
    for i in 0..elements.len() {
        for j in (i + 1)..elements.len() {
            let (ai_x, ai_y) = elements[i].origin.to_mm();
            let (bj_x, bj_y) = elements[j].origin.to_mm();
            let dx = (ai_x - bj_x).abs();
            let dy = (ai_y - bj_y).abs();
            let overlap_w = (cell_w_mm - dx).max(0.0);
            let overlap_h = (cell_h_mm - dy).max(0.0);
            total += overlap_w * overlap_h;
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Constraint violation
// ---------------------------------------------------------------------------

fn constraint_violation(placement: &Placement, checked: &CheckedNetlist, library: &Library) -> f64 {
    let mut total = 0.0;

    let refdes_to_index: HashMap<&str, usize> = placement
        .elements
        .iter()
        .enumerate()
        .map(|(i, p)| (p.refdes.as_str(), i))
        .collect();

    // ---- align: variance of origin Y (Horizontal) / X (Vertical).
    // Stage 2: origin coordinate is sufficient because all aligned
    // members share orientation (identity, ADR-3 stage-1 invariant)
    // — origin offset == pin offset by a constant. TODO(stage 5):
    // switch to connecting-pin coordinates once orientation search
    // lands.
    for spec in &checked.align {
        let coords: Vec<f64> = spec
            .refdes
            .iter()
            .filter_map(|r| {
                let idx = *refdes_to_index.get(r.as_str())?;
                let (x_mm, y_mm) = placement.elements[idx].origin.to_mm();
                Some(match spec.axis {
                    Axis::Horizontal => y_mm,
                    Axis::Vertical => x_mm,
                })
            })
            .collect();
        if coords.len() < 2 {
            continue;
        }
        #[allow(clippy::cast_precision_loss)] // cluster size is tiny.
        let n_f = coords.len() as f64;
        let mean = coords.iter().sum::<f64>() / n_f;
        for c in &coords {
            let d = c - mean;
            total += d * d;
        }
    }

    // ---- place: pin-anchored relation residual.
    for spec in &checked.place {
        let (Some(&t_idx), Some(&a_idx)) = (
            refdes_to_index.get(spec.refdes.as_str()),
            refdes_to_index.get(spec.anchor.as_str()),
        ) else {
            continue;
        };
        let target = &placement.elements[t_idx];
        let anchor = &placement.elements[a_idx];

        // Resolve symbol via resolved element when available, else library.
        let target_sym = checked
            .elements
            .iter()
            .find(|e| e.refdes == target.refdes)
            .map_or_else(
                || library.lookup(&target.lib_id).cloned().expect("lib lookup"),
                |re| re.symbol.clone(),
            );
        let anchor_sym = checked
            .elements
            .iter()
            .find(|e| e.refdes == anchor.refdes)
            .map_or_else(
                || library.lookup(&anchor.lib_id).cloned().expect("lib lookup"),
                |re| re.symbol.clone(),
            );

        let target_pins = target.world_pin_mm(&target_sym);
        let anchor_pins = anchor.world_pin_mm(&anchor_sym);

        total += place_residual(spec.relation, &anchor_pins, &target_pins);
    }

    total
}

/// Hinged-X + always-Y (or hinged-Y + always-X for vertical relations)
/// residual for one [`PlaceSpec`].
///
/// For `RightOf`: anchor's *rightmost* pin should be at-or-left-of
/// target's *leftmost* pin (X term hinged), and their Y's should
/// match (Y term always penalised). Symmetric for the other three.
///
/// `ε = 0` in stage 2 (no minimum-gap enforcement; gap is the SA's
/// job).
fn place_residual(
    rel: Relation,
    anchor_pins: &[(String, f64, f64)],
    target_pins: &[(String, f64, f64)],
) -> f64 {
    // Pick by the same criterion as stage-1 `solve_place`:
    //   RightOf → anchor's rightmost (max-x, tie min-y),
    //              target's leftmost (min-x, tie min-y).
    //   LeftOf  → anchor's leftmost,
    //              target's rightmost.
    //   Above   → anchor's topmost,    target's bottommost.
    //   Below   → anchor's bottommost, target's topmost.
    let eps = 0.0;
    match rel {
        Relation::RightOf => {
            let (ax, ay) = pick_pin(anchor_pins, |x, y| (-x, y));
            let (tx, ty) = pick_pin(target_pins, |x, y| (x, y));
            let x_excess = (ax - tx + eps).max(0.0);
            x_excess * x_excess + (ay - ty) * (ay - ty)
        }
        Relation::LeftOf => {
            let (ax, ay) = pick_pin(anchor_pins, |x, y| (x, y));
            let (tx, ty) = pick_pin(target_pins, |x, y| (-x, y));
            let x_excess = (tx - ax + eps).max(0.0);
            x_excess * x_excess + (ay - ty) * (ay - ty)
        }
        Relation::Above => {
            let (ax, ay) = pick_pin(anchor_pins, |x, y| (-y, x));
            let (tx, ty) = pick_pin(target_pins, |x, y| (y, x));
            let y_excess = (ay - ty + eps).max(0.0);
            y_excess * y_excess + (ax - tx) * (ax - tx)
        }
        Relation::Below => {
            let (ax, ay) = pick_pin(anchor_pins, |x, y| (y, x));
            let (tx, ty) = pick_pin(target_pins, |x, y| (-y, x));
            let y_excess = (ty - ay + eps).max(0.0);
            y_excess * y_excess + (ax - tx) * (ax - tx)
        }
    }
}

/// Pick the pin minimising `key(x, y)` (lexicographic on the tuple
/// returned). Returns `(x_mm, y_mm)`.
fn pick_pin<K, F>(pins: &[(String, f64, f64)], key: F) -> (f64, f64)
where
    K: PartialOrd,
    F: Fn(f64, f64) -> K,
{
    let mut best: Option<(K, f64, f64)> = None;
    for &(_, x, y) in pins {
        let k = key(x, y);
        let replace = match &best {
            None => true,
            Some((bk, _, _)) => k.partial_cmp(bk) == Some(std::cmp::Ordering::Less),
        };
        if replace {
            best = Some((k, x, y));
        }
    }
    let (_, x, y) = best.expect("symbol has at least one pin");
    (x, y)
}

// ---------------------------------------------------------------------------
// Crossings (straight-line MST approximation)
// ---------------------------------------------------------------------------

/// Compute the MST of each net's pin set in mm, then count crossings
/// between MST edges across distinct net pairs.
///
/// Limitation: real KiCad wires are orthogonal, so two wires that
/// "cross" diagonally here may route around each other in a real
/// schematic. This is a placeholder until `spice-route` (v0.2) lands
/// — see `docs/layout-roadmap.md` §4 step 6.
fn crossings(nets: &[Net]) -> f64 {
    let edges: Vec<Vec<Segment>> = nets.iter().map(|n| net_mst_edges(&n.pins)).collect();
    let mut count = 0u64;
    for i in 0..edges.len() {
        for j in (i + 1)..edges.len() {
            for s1 in &edges[i] {
                for s2 in &edges[j] {
                    if segments_cross(s1, s2) {
                        count += 1;
                    }
                }
            }
        }
    }
    // u64 → f64 widening for tiny counts; precision loss only at >2^53.
    #[allow(clippy::cast_precision_loss)]
    let out = count as f64;
    out
}

#[derive(Debug, Clone, Copy)]
struct Segment {
    ax: f64,
    ay: f64,
    bx: f64,
    by: f64,
}

/// Straight-line MST via Prim's algorithm, O(n²). Pin counts per net
/// are tiny (single digits in practice), so the constant matters more
/// than the asymptotic.
fn net_mst_edges(pins: &[(f64, f64)]) -> Vec<Segment> {
    let n = pins.len();
    if n < 2 {
        return Vec::new();
    }
    let mut in_tree = vec![false; n];
    let mut best_dist = vec![f64::INFINITY; n];
    let mut best_parent = vec![0usize; n];
    let mut edges: Vec<Segment> = Vec::with_capacity(n - 1);

    in_tree[0] = true;
    for v in 1..n {
        best_dist[v] = dist(pins[0], pins[v]);
        best_parent[v] = 0;
    }

    for _ in 1..n {
        // Pick the not-in-tree vertex with smallest best_dist.
        let mut next = usize::MAX;
        let mut nd = f64::INFINITY;
        for v in 0..n {
            if !in_tree[v] && best_dist[v] < nd {
                nd = best_dist[v];
                next = v;
            }
        }
        if next == usize::MAX {
            break; // shouldn't happen for a connected complete graph
        }
        in_tree[next] = true;
        let p = best_parent[next];
        edges.push(Segment {
            ax: pins[p].0,
            ay: pins[p].1,
            bx: pins[next].0,
            by: pins[next].1,
        });
        for v in 0..n {
            if !in_tree[v] {
                let d = dist(pins[next], pins[v]);
                if d < best_dist[v] {
                    best_dist[v] = d;
                    best_parent[v] = next;
                }
            }
        }
    }
    edges
}

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    dx.hypot(dy)
}

/// True iff the open interiors of `s1` and `s2` cross.
///
/// Uses the standard orientation-sign test. Endpoint touches and
/// collinear overlaps return `false` — they are common when
/// neighbouring pins on a row share a coordinate, and counting them
/// as crossings would penalise reasonable layouts.
fn segments_cross(s1: &Segment, s2: &Segment) -> bool {
    let d1 = orient(s2.ax, s2.ay, s2.bx, s2.by, s1.ax, s1.ay);
    let d2 = orient(s2.ax, s2.ay, s2.bx, s2.by, s1.bx, s1.by);
    let d3 = orient(s1.ax, s1.ay, s1.bx, s1.by, s2.ax, s2.ay);
    let d4 = orient(s1.ax, s1.ay, s1.bx, s1.by, s2.bx, s2.by);
    // Strict opposite signs on both: proper interior crossing.
    let eps = 1e-9;
    (d1 > eps && d2 < -eps || d1 < -eps && d2 > eps)
        && (d3 > eps && d4 < -eps || d3 < -eps && d4 > eps)
}

/// Signed area of the triangle (a, b, c) × 2.
fn orient(ax: f64, ay: f64, bx: f64, by: f64, cx: f64, cy: f64) -> f64 {
    (bx - ax) * (cy - ay) - (by - ay) * (cx - ax)
}

// ---------------------------------------------------------------------------
// Rail direction (ζ)
// ---------------------------------------------------------------------------

/// Sum of hinged squared distances pulling rail pins to the top of
/// the placement and ground pins to the bottom.
///
/// The placement's own pin extents (`y_top` = max pin Y, `y_bot` =
/// min pin Y) provide a self-normalising reference: there is no
/// absolute "top of sheet" until the emitter pins one down. When the
/// extents collapse (single row of pins) both hinges read zero.
///
/// Rails are identified by `ElementRole::Power(rail)`; ground is the
/// literal node `"0"` per Berkeley SPICE convention. Power-source
/// elements' own pins participate so the power-flag symbol is pulled
/// up alongside the rail it labels.
fn rail_direction(elements: &[ResolvedElement], pin_world: &PinWorld) -> f64 {
    let power_rails: HashSet<&str> = elements
        .iter()
        .filter_map(|e| match &e.role {
            ElementRole::Power(rail) => Some(rail.as_str()),
            ElementRole::Normal => None,
        })
        .collect();

    let (y_top, y_bot) = pin_extents_y(pin_world);

    let mut total = 0.0;
    for (i, elem) in elements.iter().enumerate() {
        for (term_idx, node_name) in elem.nodes.iter().enumerate() {
            let Some(kicad_pin) = elem.pin_mapping.get(term_idx) else {
                continue;
            };
            let Some(world_pins) = pin_world.get(i) else {
                continue;
            };
            let Some(&(_, _, y)) = world_pins.iter().find(|(num, _, _)| num == kicad_pin) else {
                continue;
            };
            if power_rails.contains(node_name.as_str()) {
                let d = (y_top - y).max(0.0);
                total += d * d;
            } else if node_name == "0" {
                let d = (y - y_bot).max(0.0);
                total += d * d;
            }
        }
    }
    total
}

fn pin_extents_y(pin_world: &PinWorld) -> (f64, f64) {
    let mut y_min = f64::INFINITY;
    let mut y_max = f64::NEG_INFINITY;
    for pins in pin_world {
        for &(_, _, y) in pins {
            if y < y_min {
                y_min = y;
            }
            if y > y_max {
                y_max = y;
            }
        }
    }
    if y_min.is_infinite() || y_max.is_infinite() {
        (0.0, 0.0)
    } else {
        (y_max, y_min)
    }
}

fn pin_extents_x(pin_world: &PinWorld) -> (f64, f64) {
    let mut x_min = f64::INFINITY;
    let mut x_max = f64::NEG_INFINITY;
    for pins in pin_world {
        for &(_, x, _) in pins {
            if x < x_min {
                x_min = x;
            }
            if x > x_max {
                x_max = x;
            }
        }
    }
    if x_min.is_infinite() || x_max.is_infinite() {
        (0.0, 0.0)
    } else {
        (x_min, x_max)
    }
}

// ---------------------------------------------------------------------------
// Signal flow (η)
// ---------------------------------------------------------------------------

/// Sum of hinged squared distances pulling subckt input pins to the
/// left edge and subckt output pins to the right edge.
///
/// For each subckt with two or more ports, the first port is treated
/// as the sole input net and the last as the sole output net.
/// Top-level netlists (no subckts) contribute zero.
fn signal_flow(
    elements: &[ResolvedElement],
    pin_world: &PinWorld,
    subckts: &[spice_resolve::SubcktPorts],
) -> f64 {
    if subckts.is_empty() {
        return 0.0;
    }
    let mut input_nets: HashSet<&str> = HashSet::new();
    let mut output_nets: HashSet<&str> = HashSet::new();
    for sc in subckts {
        if sc.ports.len() < 2 {
            continue;
        }
        input_nets.insert(sc.ports[0].as_str());
        output_nets.insert(sc.ports.last().expect("len >= 2").as_str());
    }
    if input_nets.is_empty() && output_nets.is_empty() {
        return 0.0;
    }

    let (x_left, x_right) = pin_extents_x(pin_world);

    let mut total = 0.0;
    for (i, elem) in elements.iter().enumerate() {
        for (term_idx, node_name) in elem.nodes.iter().enumerate() {
            let Some(kicad_pin) = elem.pin_mapping.get(term_idx) else {
                continue;
            };
            let Some(world_pins) = pin_world.get(i) else {
                continue;
            };
            let Some(&(_, x, _)) = world_pins.iter().find(|(num, _, _)| num == kicad_pin) else {
                continue;
            };
            if input_nets.contains(node_name.as_str()) {
                let d = (x - x_left).max(0.0);
                total += d * d;
            }
            if output_nets.contains(node_name.as_str()) {
                let d = (x_right - x).max(0.0);
                total += d * d;
            }
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Band misalignment
// ---------------------------------------------------------------------------

/// One grid-cell tolerance for Top/Bot bands (mm).
const BAND_TOL_MM: f64 = GridPoint::STEP_MM;

/// Penalise elements outside their assigned band.
///
/// `Top` elements are pulled toward `y_top` (the smallest Y origin
/// among Top-band elements, in mm); `Bot` elements toward `y_bot`
/// (the largest Y origin among Bot-band elements). The penalty is the
/// hinged squared deviation from those targets, with a one grid-cell
/// tolerance — matching what the seed placer (`place_seed`) emits when
/// no better information is available.
///
/// `Mid` elements are constrained to lie inside the open `(y_top,
/// y_bot)` interval; their soft Y target inside that interval is the
/// concern of [`soft_y_residual`], not this term.
///
/// If `bands` is `None`, the function recomputes net classes and band
/// assignments from `checked`.
#[must_use]
pub fn band_misalignment(
    placement: &Placement,
    checked: &CheckedNetlist,
    bands: Option<&[BandAssignment]>,
) -> f64 {
    let owned: Vec<BandAssignment>;
    let band_asg: &[BandAssignment] = if let Some(b) = bands {
        b
    } else {
        let classes = classify_nets(checked);
        owned = assign_y_bands(checked, &classes);
        &owned
    };

    if placement.elements.is_empty() {
        return 0.0;
    }

    // Reference Y for each rail. Defaults: if no Top elements, use the
    // smallest origin Y across all elements (i.e. nothing to violate).
    // Symmetric for Bot.
    let mut y_top = f64::INFINITY;
    let mut y_bot = f64::NEG_INFINITY;
    for (pe, ba) in placement.elements.iter().zip(band_asg) {
        let (_, y_mm) = pe.origin.to_mm();
        match ba.band {
            Band::Top => {
                if y_mm < y_top {
                    y_top = y_mm;
                }
            }
            Band::Bot => {
                if y_mm > y_bot {
                    y_bot = y_mm;
                }
            }
            Band::Mid => {}
        }
    }
    if !y_top.is_finite() {
        // No Top elements — fall back to the placement's overall min Y
        // so Mid elements above it are not penalised.
        y_top = placement
            .elements
            .iter()
            .map(|p| p.origin.to_mm().1)
            .fold(f64::INFINITY, f64::min);
    }
    if !y_bot.is_finite() {
        y_bot = placement
            .elements
            .iter()
            .map(|p| p.origin.to_mm().1)
            .fold(f64::NEG_INFINITY, f64::max);
    }

    let mut total = 0.0;
    for (pe, ba) in placement.elements.iter().zip(band_asg) {
        let (_, y_mm) = pe.origin.to_mm();
        match ba.band {
            Band::Top => {
                // Penalise being below y_top (i.e. y > y_top + tol).
                let excess = (y_mm - (y_top + BAND_TOL_MM)).max(0.0);
                total += excess * excess;
            }
            Band::Bot => {
                // Penalise being above y_bot (y < y_bot - tol).
                let excess = ((y_bot - BAND_TOL_MM) - y_mm).max(0.0);
                total += excess * excess;
            }
            Band::Mid => {
                // Wider window: only penalise leaving the (y_top, y_bot)
                // interval entirely (no tolerance — Mid is permissive).
                if y_mm < y_top {
                    let d = y_top - y_mm;
                    total += d * d;
                } else if y_mm > y_bot {
                    let d = y_mm - y_bot;
                    total += d * d;
                }
            }
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Soft-Y residual
// ---------------------------------------------------------------------------

/// Squared distance of every Mid-band element from the soft-Y target
/// implied by its `soft_y_target_frac`.
///
/// The target is `y_mid_top + frac * (y_mid_bot - y_mid_top)`, where
/// `y_mid_top` and `y_mid_bot` are the smallest and largest origin Y
/// across *Mid-band* elements (not the rails — the soft target lives
/// inside the Mid window). When fewer than two Mid elements exist the
/// term is 0 (no span to measure against).
/// Hard pairwise penalty for band-order inversions: any pair of
/// elements `(i, j)` where `i`'s `soft_y_target_frac` is meaningfully
/// smaller than `j`'s but `i.y > j.y` adds `(i.y - j.y)²` to the cost.
/// This is the *absolute* counterpart to `soft_y_residual`: the
/// latter pulls each element to its frac-target inside the placement
/// span (which is a moving reference); this term anchors on
/// pairwise *ordering*, which is invariant under uniform Y shifts.
///
/// "Meaningfully smaller" is defined as a frac difference of at
/// least 0.1; pairs within that tolerance are not penalised because
/// they are deliberately allowed to swap (e.g. two Mid 0.5 elements).
#[must_use]
pub fn band_inversion(placement: &Placement, checked: &CheckedNetlist) -> f64 {
    let classes = classify_nets(checked);
    let band_asg = assign_y_bands(checked, &classes);
    if placement.elements.len() != band_asg.len() {
        return 0.0;
    }
    let n = placement.elements.len();
    let mut total = 0.0;
    let ys: Vec<f64> = placement
        .elements
        .iter()
        .map(|e| e.origin.to_mm().1)
        .collect();
    let fracs: Vec<f64> = band_asg.iter().map(|b| b.soft_y_target_frac).collect();
    for i in 0..n {
        for j_off in 1..(n - i) {
            let j = i + j_off;
            // i should be above j (yi < yj) iff fi < fj. Penalise
            // only when |fi - fj| > 0.1 (clear ordering required).
            if (fracs[i] - fracs[j]).abs() < 0.1 {
                continue;
            }
            let (above, below) = if fracs[i] < fracs[j] { (i, j) } else { (j, i) };
            let yu = ys[above];
            let yd = ys[below];
            if yu > yd {
                let d = yu - yd;
                total += d * d;
            }
        }
    }
    total
}

#[must_use]
pub fn soft_y_residual(placement: &Placement, checked: &CheckedNetlist) -> f64 {
    let classes = classify_nets(checked);
    let band_asg = assign_y_bands(checked, &classes);

    if placement.elements.len() != band_asg.len() {
        return 0.0;
    }

    // Self-anchor on the *full* placement Y extent, not just the Mid
    // sub-band. This stops SA from "drifting" all Mid elements
    // together to nullify the residual: Power-Signal (frac < 0.5)
    // is always pulled toward the upper third of the actual canvas,
    // Ground-Signal (frac > 0.5) toward the lower third.
    let mut y_min = f64::INFINITY;
    let mut y_max = f64::NEG_INFINITY;
    for pe in &placement.elements {
        let (_, y) = pe.origin.to_mm();
        if y < y_min {
            y_min = y;
        }
        if y > y_max {
            y_max = y;
        }
    }
    let span = y_max - y_min;
    if span.abs() < f64::EPSILON || !y_min.is_finite() {
        return 0.0;
    }

    let mut total = 0.0;
    for (pe, ba) in placement.elements.iter().zip(&band_asg) {
        if ba.band != Band::Mid {
            continue;
        }
        let (_, y) = pe.origin.to_mm();
        let target = y_min + ba.soft_y_target_frac * span;
        let d = y - target;
        total += d * d;
    }
    total
}

// ---------------------------------------------------------------------------
// Layer order
// ---------------------------------------------------------------------------

/// Penalise X-coordinate inversions on the signal-flow DAG.
///
/// For each pair of elements `(u, v)` such that:
///   * `layer(u) < layer(v)` per [`assign_x_layers`]; and
///   * `u` and `v` share at least one Signal-class net,
///
/// add `(x_u - x_v)²` whenever `x_u > x_v` (u is supposed to feed v
/// from the left, so its X must be ≤ v's X). Within-layer pairs and
/// pairs not sharing a Signal net contribute zero.
#[must_use]
pub fn layer_order(placement: &Placement, checked: &CheckedNetlist) -> f64 {
    let classes = classify_nets(checked);
    let layer_asg = assign_x_layers(checked, &classes);

    if layer_asg.no_source_fallback {
        return 0.0;
    }
    let layers = &layer_asg.layers;
    if layers.len() != placement.elements.len() {
        return 0.0;
    }

    // Build adjacency on Signal nets.
    let n = checked.elements.len();
    let mut net_to_elements: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, el) in checked.elements.iter().enumerate() {
        for net in &el.nodes {
            if classes
                .get(net.as_str())
                .copied()
                .unwrap_or(NetClass::Signal)
                == NetClass::Signal
            {
                net_to_elements.entry(net.as_str()).or_default().push(i);
            }
        }
    }

    // For each Signal net's element set, sum penalty across each
    // ordered pair (u, v) with layer(u) < layer(v) and x_u > x_v.
    // A `HashSet` keeps each pair counted once even if two distinct
    // nets connect the same two elements.
    let mut counted: HashSet<(usize, usize)> = HashSet::new();
    let mut total = 0.0;
    for members in net_to_elements.values() {
        for &u in members {
            for &v in members {
                if u >= n || v >= n || u == v {
                    continue;
                }
                if layers[u] >= layers[v] {
                    continue;
                }
                if !counted.insert((u, v)) {
                    continue;
                }
                let (xu, _) = placement.elements[u].origin.to_mm();
                let (xv, _) = placement.elements[v].origin.to_mm();
                if xu > xv {
                    let d = xu - xv;
                    total += d * d;
                }
            }
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Net-bbox crossings (cheap proxy)
// ---------------------------------------------------------------------------

/// Count pairs of distinct nets whose pin bounding boxes overlap (in mm).
///
/// Two nets whose pin-bounding rectangles intersect are likely to
/// produce wires that cross. This proxy is O(N²) over net pairs but
/// orders of magnitude cheaper than the segment-cross product used by
/// [`crossings`], and is robust to MST-tie wobble. Single-pin and
/// zero-pin nets are skipped (they have no rectangle).
/// Public entry: build nets from `placement` + `checked` (+ `library`)
/// and compute the bbox-crossing proxy.
#[must_use]
pub fn net_bbox_crossings_for(
    placement: &Placement,
    checked: &CheckedNetlist,
    library: &Library,
) -> f64 {
    let pin_world = collect_pin_world(placement, &checked.elements, library);
    let nets = build_nets(&checked.elements, &pin_world);
    net_bbox_crossings(&nets)
}

fn net_bbox_crossings(nets: &[Net]) -> f64 {
    let boxes: Vec<(f64, f64, f64, f64)> = nets
        .iter()
        .filter(|n| n.pins.len() >= 2)
        .map(|n| {
            let (mut min_x, mut max_x) = (f64::INFINITY, f64::NEG_INFINITY);
            let (mut min_y, mut max_y) = (f64::INFINITY, f64::NEG_INFINITY);
            for &(x, y) in &n.pins {
                if x < min_x {
                    min_x = x;
                }
                if x > max_x {
                    max_x = x;
                }
                if y < min_y {
                    min_y = y;
                }
                if y > max_y {
                    max_y = y;
                }
            }
            (min_x, min_y, max_x, max_y)
        })
        .collect();

    let mut count = 0_u64;
    for i in 0..boxes.len() {
        for j in (i + 1)..boxes.len() {
            let (a_min_x, a_min_y, a_max_x, a_max_y) = boxes[i];
            let (b_min_x, b_min_y, b_max_x, b_max_y) = boxes[j];
            // Strict overlap (touching edges do not count).
            let eps = 1e-9;
            if a_min_x < b_max_x - eps
                && b_min_x < a_max_x - eps
                && a_min_y < b_max_y - eps
                && b_min_y < a_max_y - eps
            {
                count += 1;
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let out = count as f64;
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use kicad_symbols::{Library, Orientation};
    use spice_diagnostics::FileId;
    use spice_policy::check;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    fn fixture_library() -> &'static Library {
        static LIB: OnceLock<Library> = OnceLock::new();
        LIB.get_or_init(|| {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let fixture_dir = manifest
                .parent()
                .and_then(std::path::Path::parent)
                .expect("workspace root")
                .join("crates/kicad-symbols/tests/fixtures");
            let device = Library::from_file(fixture_dir.join("Device.kicad_sym"))
                .expect("load Device fixture library");
            let spice = Library::from_file(fixture_dir.join("Simulation_SPICE.kicad_sym"))
                .expect("load Simulation_SPICE fixture library");
            device.merge(spice)
        })
    }

    /// Parse SPICE source, resolve, check.
    fn checked_str(src: &str) -> CheckedNetlist {
        let file_id = FileId(0);
        let parsed = spice_parser::parse(src, file_id)
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let (checked, _w) = check(resolved).expect("policy check failed");
        checked
    }

    /// Build a manual placement matching `checked.elements` index order.
    fn manual_placement(checked: &CheckedNetlist, origins: &[(i32, i32)]) -> Placement {
        let elements = checked
            .elements
            .iter()
            .zip(origins)
            .map(|(e, &(x, y))| PlacedElement {
                refdes: e.refdes.clone(),
                lib_id: e.lib_id.clone(),
                origin: GridPoint::new(x, y),
                orientation: Orientation::IDENTITY,
                nodes: e.nodes.clone(),
                pin_mapping: e.pin_mapping.clone(),
                value: None,
                is_power_source: matches!(e.role, ElementRole::Power(_)),
            })
            .collect();
        Placement { elements }
    }

    // -- band_misalignment -------------------------------------------------

    /// Two elements: V1 (Power-tagged, Top band) and R1 (Mid band).
    /// Place them at the seed-placer-style coordinates: V1 at y=0 (top),
    /// R1 below. R1 sits inside `(y_top, y_bot)` so band_misalignment is 0.
    #[test]
    fn band_misalignment_zero_when_aligned() {
        let checked =
            checked_str("test\nV1 vcc 0 12 ;@ power=vcc\nR1 vcc out 1k\nR2 out 0 1k\n.end\n");
        // V1 → Top, R1/R2 connect to ground → Bot or Mid depending on classes.
        // Place V1 at y=0, R1 in middle, R2 at y=10.
        let placement = manual_placement(&checked, &[(0, 0), (5, 5), (10, 10)]);
        let cost = band_misalignment(&placement, &checked, None);
        assert!(
            cost.abs() < 1e-9,
            "expected 0 for aligned placement, got {cost}"
        );
    }

    /// Two resistors that touch only the power rail (no ground) → both
    /// classified Top. Place them far apart vertically → the second one
    /// is well below `y_top` and incurs a positive band penalty.
    #[test]
    fn band_misalignment_nonzero_when_offset() {
        // R1 and R2 both connect only to vcc-class nets — no ground —
        // so the Power-only rule lands them in the Top band.
        let checked = checked_str(
            "test\n\
             V1 vcc 0 12 ;@ power=vcc\n\
             R1 vcc vdd 1k\n\
             R2 vcc vdd 1k\n\
             R3 vdd 0 1k\n.end\n",
        );
        // R1 sits at y=0 (sets y_top); R2 (also Top) shoved down to y=30.
        let placement = manual_placement(&checked, &[(0, 0), (5, 0), (10, 30), (15, 10)]);
        let cost = band_misalignment(&placement, &checked, None);
        assert!(
            cost > 0.0,
            "expected positive cost when R2 (Top band) is far below y_top, got {cost}"
        );
    }

    // -- soft_y_residual ---------------------------------------------------

    /// Two Mid-band elements (signal-only resistors, both frac=0.5)
    /// placed at the same Y → both sit at their soft target → residual 0.
    #[test]
    fn soft_y_residual_zero_at_target() {
        // V1 in 0 AC 1 → V1 is Mid frac=0.5 (Ground+Signal). R1 (Signal
        // only) and R2 (Signal only) are Mid frac=0.5 as well.
        let checked = checked_str("test\nV1 in 0 AC 1\nR1 in mid 1k\nR2 mid out 1k\n.end\n");
        // All Mid elements at the same Y → mid span collapses to 0,
        // function returns 0 by definition; that *is* "every element
        // at its target" when all targets coincide.
        let placement = manual_placement(&checked, &[(0, 5), (5, 5), (10, 5)]);
        let cost = soft_y_residual(&placement, &checked);
        assert!(
            cost.abs() < 1e-9,
            "expected 0 residual when Mid Y span is collapsed, got {cost}"
        );
    }

    // -- layer_order -------------------------------------------------------

    /// All elements at the same X coordinate → no pair has `x_u > x_v`,
    /// so the layer-order cost is 0 regardless of the (nondeterministic
    /// in v0.1) layering returned by `assign_x_layers`.
    #[test]
    fn layer_order_zero_when_left_to_right() {
        let checked = checked_str("test\nV1 in 0 AC 1\nR1 in mid 1k\nR2 mid out 1k\n.end\n");
        // Every element at X=0 → no inversions possible.
        let placement = manual_placement(&checked, &[(0, 0), (0, 5), (0, 10)]);
        let cost = layer_order(&placement, &checked);
        assert!(
            cost.abs() < 1e-9,
            "expected 0 when all X coordinates coincide, got {cost}"
        );
    }

    /// V1 is forced to layer 0 by `assign_x_layers` (it is the sole
    /// signal source). Place V1 at X=20 — well to the right of R1 (X=5)
    /// and R2 (X=10). V1 shares net "in" with R1, so `(V1, R1)` is a
    /// signal-DAG edge with `layer(V1) < layer(R1)` and `x(V1) > x(R1)`
    /// → at least one inversion → strictly positive cost.
    #[test]
    fn layer_order_positive_when_inverted() {
        let checked = checked_str("test\nV1 in 0 AC 1\nR1 in mid 1k\nR2 mid out 1k\n.end\n");
        let placement = manual_placement(&checked, &[(20, 0), (5, 0), (10, 0)]);
        let cost = layer_order(&placement, &checked);
        assert!(
            cost > 0.0,
            "expected positive cost when V1 is right of R1, got {cost}"
        );
    }

    // -- net_bbox_crossings ------------------------------------------------

    /// Two single-net pairs placed far apart — bboxes don't overlap.
    #[test]
    fn net_bbox_crossings_zero_for_isolated_nets() {
        // R1 on net "a"-"b"; R2 on net "c"-"d"; no shared nets at all
        // (we use an extra resistor on each so the nets actually have
        // ≥ 2 pins each). Still — the two clusters are separated.
        let checked = checked_str(
            "test\n\
             R1 a b 1k\n\
             R2 a b 1k\n\
             R3 c d 1k\n\
             R4 c d 1k\n.end\n",
        );
        // Cluster (R1,R2) at far left; (R3,R4) at far right.
        let placement = manual_placement(&checked, &[(0, 0), (0, 5), (100, 0), (100, 5)]);
        let nets = build_nets(
            &checked.elements,
            &collect_pin_world(&placement, &checked.elements, fixture_library()),
        );
        let count = net_bbox_crossings(&nets);
        assert!(
            count.abs() < 1e-9,
            "expected 0 crossings for isolated clusters, got {count}"
        );
    }

    /// Two distinct nets whose pin bboxes overlap → at least one crossing.
    #[test]
    fn net_bbox_crossings_positive_when_overlap() {
        // R1/R2 share net "a"; R3/R4 share net "b". Place R1 and R2 at
        // diagonal corners (0,0) and (10,10); R3 and R4 at the other
        // diagonal (0,10) and (10,0). Both nets' pin bboxes cover the
        // same rectangle → guaranteed overlap.
        let checked = checked_str(
            "test\n\
             R1 a x 1k\n\
             R2 a y 1k\n\
             R3 b p 1k\n\
             R4 b q 1k\n.end\n",
        );
        let placement = manual_placement(&checked, &[(0, 0), (10, 10), (0, 10), (10, 0)]);
        let nets = build_nets(
            &checked.elements,
            &collect_pin_world(&placement, &checked.elements, fixture_library()),
        );
        let count = net_bbox_crossings(&nets);
        assert!(
            count > 0.0,
            "expected ≥ 1 net-bbox crossing when nets are interleaved, got {count}"
        );
    }
}
