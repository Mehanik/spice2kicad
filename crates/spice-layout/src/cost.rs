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
        overlap: 50.0,
        hpwl: 1.0,
        // not yet tuned — see docs/layout-roadmap.md §7
        rail_direction: 50.0,
        // not yet tuned — see docs/layout-roadmap.md §7
        signal_flow: 25.0,
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
