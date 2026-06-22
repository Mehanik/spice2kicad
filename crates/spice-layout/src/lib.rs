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

pub mod bands;
pub mod cost;
pub mod layers;
pub mod net_class;
pub mod orient;
pub mod sidecar;
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

/// A set of pinned positions seeded from the position-stability sidecar
/// (ADR-4). Each entry maps a SPICE refdes to a saved `(origin,
/// orientation)`.
///
/// The hint is a **seed**, not a hard constraint: a refdes present here
/// is placed at its saved position and marked pinned so the SA refiner
/// leaves it put (reusing the exact same `pinned` mask that `align` /
/// `place` use). But hard constraints still win: an element fixed by
/// `align` / `place` keeps its constraint-solved position. New refdeses
/// absent from the hint fall through to normal seeding and are placed
/// (and de-overlapped) by SA; removed refdeses simply never appear in
/// the next rewrite.
#[derive(Debug, Clone, Default)]
pub struct Hint {
    /// refdes → (saved grid origin, saved orientation).
    pub pins: std::collections::HashMap<String, (GridPoint, Orientation)>,
}

/// Render a parsed SPICE [`Value`] back to its source-equivalent token.
///
/// The schematic emitter uses this to populate the symbol's `Value`
/// property. Numeric values are rendered with an SI prefix that brings
/// the mantissa into `[1, 1000)` per CLAUDE.md "Visual quality
/// invariants V9". Non-numeric values (`Value::String`,
/// `Value::Expr`) pass through verbatim.
fn format_value(v: &Value) -> String {
    match v {
        Value::Number(n) => format_si(*n),
        Value::String(s) => s.clone(),
        Value::Expr(e) => e.clone(),
    }
}

/// SI-prefix table: `(exponent, suffix)` where the multiplier is
/// `10^exponent`. Picked so the mantissa lands in `[1, 1000)`.
/// `Meg` (not `M`) for mega — matches SPICE convention where a bare
/// `M` means milli.
const SI_TABLE: &[(i32, &str)] = &[
    (-15, "f"),
    (-12, "p"),
    (-9, "n"),
    (-6, "u"),
    (-3, "m"),
    (0, ""),
    (3, "k"),
    (6, "Meg"),
    (9, "G"),
    (12, "T"),
];

/// Render an `f64` with an SI prefix per V9.
///
/// - `0.0` → `"0"`.
/// - Negatives carry the sign through: `-0.015` → `"-15m"`.
/// - `NaN` / `±Inf` fall back to `format!("{n}")`.
/// - Values outside `[1e-15, 1e15)` fall back to `format!("{n:e}")`.
/// - Mantissa: up to 3 significant digits, trailing zeros (and a
///   trailing `.`) trimmed.
fn format_si(n: f64) -> String {
    if !n.is_finite() {
        return format!("{n}");
    }
    if n == 0.0 {
        return "0".to_string();
    }
    let negative = n < 0.0;
    let abs = n.abs();

    // Out-of-range fallback. Use a strict bracket: ≥ 1e-15 (so 1f
    // formats) and < 1e15 (so 999T at 9.99e14 fits, but 1e15 does not).
    if !(1e-15..1e15).contains(&abs) {
        return format!("{n:e}");
    }

    // Pick the largest table exponent `e` such that `abs / 10^e >= 1.0`,
    // i.e. the suffix that brings the mantissa into `[1, 1000)`.
    // Use multiplication by `10^(-e)` (a small integer power) rather
    // than `log10` to avoid floating-point boundary issues at e.g.
    // `999.9999999` vs `1000`.
    let mut chosen: (i32, &str) = SI_TABLE[0];
    for &(exp, suffix) in SI_TABLE {
        // mantissa = abs * 10^(-exp)
        let mantissa = abs * pow10(-exp);
        if mantissa >= 1.0 {
            chosen = (exp, suffix);
        } else {
            break;
        }
    }

    let (exp, suffix) = chosen;
    let mantissa = abs * pow10(-exp);

    // Round mantissa to up to 3 significant digits. Mantissa is in
    // `[1, ~1000)`. If the rounded mantissa lands at exactly 1000, we
    // bump to the next suffix (so e.g. 999.95 -> "1k", not "1000").
    let rounded = round_3sf(mantissa);
    let (mantissa_final, exp_final, suffix_final) = if rounded >= 1000.0 {
        // Find next-higher suffix; if none, fall back to scientific.
        let next = SI_TABLE.iter().find(|(e, _)| *e > exp).copied();
        if let Some((e2, s2)) = next {
            // mantissa was ≈1000 at exp `exp`; in the next suffix it's
            // mantissa * 10^(exp - e2). For our 3-decade table that's
            // exactly 1.0.
            let m2 = rounded * pow10(exp - e2);
            (round_3sf(m2), e2, s2)
        } else {
            return format!("{n:e}");
        }
    } else {
        (rounded, exp, suffix)
    };
    let _ = exp_final; // exp value itself unused after suffix selected

    let mantissa_str = format_mantissa(mantissa_final);
    let sign = if negative { "-" } else { "" };
    format!("{sign}{mantissa_str}{suffix_final}")
}

/// `10^e` for small integer `e` in our table range. Uses
/// `f64::powi` — exact for the powers in `SI_TABLE`.
fn pow10(e: i32) -> f64 {
    10f64.powi(e)
}

/// Round a mantissa in `[1, 1000)` to at most three significant
/// digits. Picks the decimal precision based on the integer-part
/// width: 1.xy, 12.x, 123.
fn round_3sf(m: f64) -> f64 {
    let int_part = m.trunc().abs();
    let scale = if int_part < 10.0 {
        100.0 // two fractional digits
    } else if int_part < 100.0 {
        10.0 // one fractional digit
    } else {
        1.0 // none
    };
    (m * scale).round() / scale
}

/// Format a mantissa value with up to two fractional digits, trimming
/// trailing zeros and a trailing `.`.
fn format_mantissa(m: f64) -> String {
    let mut s = format!("{m:.2}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
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
    place_with_hint(checked, library, opts, &Hint::default())
}

/// Run the placer with a position-stability hint (ADR-4).
///
/// Identical to [`place_with`] except that any refdes present in `hint`
/// is seeded at its saved `(origin, orientation)` and pinned, so the SA
/// refiner leaves it put. This reuses the same per-element `pinned` mask
/// that `align` / `place` constraints use — no parallel path. Hard
/// constraints win over a stale hint: an `align` / `place`-fixed element
/// keeps its constraint-solved coordinate (the hint never overwrites an
/// already-pinned element).
#[allow(clippy::needless_pass_by_value)]
pub fn place_with_hint(
    checked: CheckedNetlist,
    library: &Library,
    opts: &LayoutOptions,
    hint: &Hint,
) -> Result<Placement, Vec<Diagnostic>> {
    let (mut placement, mut pinned) = place_seed(&checked)?;
    apply_hint(&mut placement, &mut pinned, hint);
    // V7: detect structural symmetry in the netlist and mirror paired
    // elements about a common vertical axis. Runs after V6 archetype
    // seeding so the axis is computed from a topology-aware base
    // layout when one exists, and before V5 orientation so the pinned
    // pair geometry guides the orientation chooser for the rest of
    // the circuit.
    if let Some(plan) = symmetry::detect_pairs(&checked) {
        symmetry::apply(&mut placement, &mut pinned, &plan);
    }
    // V14: per-element allowed-orientation set (power pin up / ground
    // pin down). A *hard* candidate-space filter, threaded into both
    // the V5 seed chooser below and the SA refiner so the constraint is
    // hard at *every* stage that can move an element (CLAUDE.md
    // "consistency requirement").
    let allowed = orient::allowed_orientations(&checked);
    pick_orientations(&mut placement, &pinned, &checked, &allowed);
    if !opts.refine {
        return Ok(placement);
    }
    Ok(solver::refine(
        placement, &pinned, &checked, library, opts, &allowed,
    ))
}

/// Apply a position-stability [`Hint`] (ADR-4) over a seeded placement.
///
/// For each placed element whose refdes appears in the hint **and which
/// is not already pinned by a hard constraint** (`align` / `place`,
/// applied in [`place_seed`] before this runs), overwrite its origin and
/// orientation with the saved values and mark it pinned. Pinning it via
/// the same `pinned` mask the constraint solver uses means the SA refiner
/// treats it as immovable.
///
/// Elements absent from the hint keep their fresh seed coordinates and
/// stay unpinned, so SA places them and resolves any overlap. Hard
/// constraints win: an already-pinned element is skipped, so a stale hint
/// never overrides an `align` / `place` directive.
fn apply_hint(placement: &mut Placement, pinned: &mut [bool], hint: &Hint) {
    if hint.pins.is_empty() {
        return;
    }
    for (i, elem) in placement.elements.iter_mut().enumerate() {
        if pinned[i] {
            continue;
        }
        if let Some(&(origin, orient)) = hint.pins.get(&elem.refdes) {
            elem.origin = origin;
            elem.orientation = orient;
            pinned[i] = true;
        }
    }
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
#[allow(clippy::too_many_lines)] // adjacency build + V14-filtered scorer read clearer inline.
fn pick_orientations(
    placement: &mut Placement,
    pinned: &[bool],
    checked: &CheckedNetlist,
    allowed: &[Vec<Orientation>],
) {
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

            // V14 hard filter: only score orientations in this
            // element's allowed set (power pin up / ground pin down).
            // `rank` is the index in the *full* `Orientation::ALL`
            // order so the identity tie-break stays stable across the
            // filtered subset.
            let mut best: Option<(i64, usize, Orientation)> = None;
            for &orient in &allowed[i] {
                let rank = Orientation::ALL
                    .iter()
                    .position(|o| *o == orient)
                    .unwrap_or(0);
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
///
/// Pipeline: classify nets → assign Y bands → assign X layers → emit
/// initial grid coordinates from `(band, layer, rank_in_layer)`. User
/// `align`/`place`/`power` directives then override the heuristic seed
/// via [`apply_user_constraints`], which pins the affected elements.
/// Y-band sub-slot used for band-aware seed stacking.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
enum Slot {
    Top,
    MidUp,
    MidCtr,
    MidLo,
    Bot,
}

fn place_seed(checked: &CheckedNetlist) -> Result<(Placement, Vec<bool>), Vec<Diagnostic>> {
    use crate::bands::{Band, assign_y_bands};
    use crate::layers::assign_x_layers;
    use crate::net_class::classify_nets;

    // Geometry constants in grid cells (1.27 mm each). Strides are
    // chosen so two adjacent columns/rows cannot horizontally clip
    // each other given typical KiCad symbol body half-extents
    // (~2.54 mm = 2 cells) plus ~1 cell of padding for label glyphs
    // and refdes/value text. A 12-cell stride leaves ~7.6 mm of
    // clear space between symbol bodies on the same row, comfortably
    // wider than any symbol body in the fixtures.
    const X_STRIDE: i32 = 12; // grid cells per layer column
    const Y_BAND_GAP: i32 = 6; // gap from rail edge into Mid band
    const Y_RANK_STRIDE: i32 = 5; // vertical step per rank within layer

    let n = checked.elements.len();
    let classes = classify_nets(checked);
    let band_asg = assign_y_bands(checked, &classes);
    let layer_asg = assign_x_layers(checked, &classes);

    // Group elements per (layer, band) for band-aware Y stacking.
    // Within a layer, Top elements stack tightly at the top, Bot at
    // the bottom, and Mid is sub-grouped by `soft_y_target_frac`
    // class (≤ 0.4: upper-Mid, ≥ 0.6: lower-Mid, else centre).
    // This ordering preserves rail-above-Mid-above-rail without
    // letting `rank_in_layer` drift Power-only elements past
    // Ground-only ones (V6/T8).
    let n_i32 = i32::try_from(n).unwrap_or(i32::MAX);
    let y_top: i32 = 0;
    let y_bot: i32 = (n_i32 + 4) * Y_RANK_STRIDE;
    let y_mid_top = y_top + Y_BAND_GAP;
    let y_mid_bot = y_bot - Y_BAND_GAP;

    // Buckets: within a layer, classify each element into one of
    // five bands (Top, MidUp, MidCtr, MidLo, Bot) and stack within
    // bucket. Three Mid sub-buckets keep Power-Signal above Signal
    // above Ground-Signal even when the longest-path layering put
    // them in the same column.
    let mut element_slot: Vec<Slot> = Vec::with_capacity(n);
    for ba in &band_asg {
        let s = match ba.band {
            Band::Top => Slot::Top,
            Band::Bot => Slot::Bot,
            Band::Mid => {
                if ba.soft_y_target_frac < 0.4 {
                    Slot::MidUp
                } else if ba.soft_y_target_frac > 0.6 {
                    Slot::MidLo
                } else {
                    Slot::MidCtr
                }
            }
        };
        element_slot.push(s);
    }

    // Per-(layer, slot) running rank.
    let mut bucket_rank: HashMap<(u32, Slot), i32> = HashMap::new();
    let mut placed: Vec<PlacedElement> = Vec::with_capacity(n);
    for (i, e) in checked.elements.iter().enumerate() {
        let layer = i32::try_from(layer_asg.layers[i]).unwrap_or(i32::MAX);
        let slot = element_slot[i];
        let rank = bucket_rank
            .entry((layer_asg.layers[i], slot))
            .and_modify(|r| *r += 1)
            .or_insert(0);
        let rank = *rank;
        // Within a (layer, slot) bucket, alternate elements left/
        // right of the layer column so multiple elements at the
        // same Y target don't pile on the same X. The jitter is
        // bounded to ±2 cells (well under X_STRIDE/2) to keep
        // adjacent columns from clipping into each other.
        let max_jitter = (X_STRIDE / 4).max(1);
        let raw_jitter = if rank % 2 == 0 {
            -(rank / 2)
        } else {
            (rank + 1) / 2
        };
        let x_jitter = raw_jitter.clamp(-max_jitter, max_jitter);
        let x = layer * X_STRIDE + x_jitter;

        // Reserve three sub-rows in Mid: upper / centre / lower.
        let mid_span = (y_mid_bot - y_mid_top).max(1);
        let mid_up_y = y_mid_top + mid_span / 4;
        let mid_ctr_y = y_mid_top + mid_span / 2;
        let mid_lo_y = y_mid_top + (3 * mid_span) / 4;
        let y = match slot {
            Slot::Top => y_top + rank * Y_RANK_STRIDE,
            Slot::MidUp => mid_up_y + rank * Y_RANK_STRIDE,
            Slot::MidCtr => mid_ctr_y + rank * Y_RANK_STRIDE,
            Slot::MidLo => mid_lo_y + rank * Y_RANK_STRIDE,
            Slot::Bot => y_bot - rank * Y_RANK_STRIDE,
        };
        placed.push(PlacedElement {
            refdes: e.refdes.clone(),
            lib_id: e.lib_id.clone(),
            origin: GridPoint::new(x, y),
            orientation: Orientation::IDENTITY,
            nodes: e.nodes.clone(),
            pin_mapping: e.pin_mapping.clone(),
            value: e.value.as_ref().map(format_value),
        });
    }

    let mut placement = Placement { elements: placed };
    let mut pinned = vec![false; n];

    apply_user_constraints(&mut placement, &mut pinned, checked)?;

    Ok((placement, pinned))
}

/// Apply user `align` / `place` directives over an existing seed
/// placement, overriding heuristic coordinates and marking each
/// affected element as pinned.
///
/// This is the second half of the previous four-phase placer (phases
/// 2/3/4): align-cluster anchors, place-relation worklist, and a final
/// auto-fill row for elements untouched by either directive but whose
/// anchor was implicitly defaulted. The first half (initial coords)
/// has been replaced by the bands+layers seed in [`place_seed`].
// Long body retained: align/place/auto-fill share state (`placed`,
// `fixed`, `free_anchor_col`) and ordering between sub-phases that
// helper splitting would obscure.
#[allow(clippy::too_many_lines)]
fn apply_user_constraints(
    placement: &mut Placement,
    pinned: &mut [bool],
    checked: &CheckedNetlist,
) -> Result<(), Vec<Diagnostic>> {
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

    let placed = &mut placement.elements;
    let fixed = pinned;

    // ---- Phase 2: align ---------------------------------------------------
    // Members of an `align horizontal` cluster all take the *first
    // member's seed Y* (the seed already classifies elements into
    // bands so this Y is band-correct), and spread along X at one
    // cluster-stride per member. Symmetric for vertical clusters.
    // This keeps `align` from dragging an element out of its band
    // (e.g. multivibrator's `align horizontal Q1 Q2` would otherwise
    // pin Q1 at the cluster-row Y regardless of band, V6/T8).
    for (cluster_index, spec) in align.iter().enumerate() {
        let cluster_index_i32 = i32::try_from(cluster_index + 1).unwrap_or(i32::MAX);
        // Take the first cluster member's seed coordinate as the
        // anchor row/column. (If the first member is itself already
        // pinned by an earlier cluster, we fall through to its
        // pinned coord.)
        let anchor_idx = spec
            .refdes
            .iter()
            .find_map(|r| refdes_to_index.get(r.as_str()).copied());
        let Some(anchor_idx) = anchor_idx else {
            continue;
        };
        let anchor_x_seed = placed[anchor_idx].origin.x;
        let row_y_seed = placed[anchor_idx].origin.y;
        // Stride: one cluster gap per cluster, biased away from
        // other clusters by `cluster_index` so they don't collide
        // when seeds happen to coincide.
        let stride = CELL_W + CLUSTER_GAP;
        let row_offset = cluster_index_i32 * (CELL_H + CLUSTER_GAP);
        for (member_index, refdes) in spec.refdes.iter().enumerate() {
            let member_index_i32 = i32::try_from(member_index).unwrap_or(i32::MAX);
            let Some(&idx) = refdes_to_index.get(refdes.as_str()) else {
                continue;
            };
            if fixed[idx] {
                continue;
            }
            let (x, y) = match spec.axis {
                Axis::Horizontal => (anchor_x_seed + member_index_i32 * stride, row_y_seed),
                Axis::Vertical => (
                    anchor_x_seed + row_offset, // small per-cluster X bias
                    row_y_seed + member_index_i32 * stride,
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

    // No phase-4 auto-fill: elements untouched by `align`/`place`
    // keep their bands+layers seed coordinates from `place_seed`.

    Ok(())
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

#[cfg(test)]
mod si_format_tests {
    use super::format_si;

    #[test]
    fn zero() {
        assert_eq!(format_si(0.0), "0");
    }

    #[test]
    fn basic_suffixes() {
        assert_eq!(format_si(1e-6), "1u");
        assert_eq!(format_si(4.7e3), "4.7k");
        assert_eq!(format_si(1.5e6), "1.5Meg");
        assert_eq!(format_si(1e3), "1k");
        assert_eq!(format_si(1e-3), "1m");
        assert_eq!(format_si(1e-9), "1n");
        assert_eq!(format_si(1e-12), "1p");
        assert_eq!(format_si(1e-15), "1f");
        assert_eq!(format_si(1e9), "1G");
        assert_eq!(format_si(1e12), "1T");
    }

    #[test]
    fn fractional_prefers_smaller_suffix() {
        // 1e-4 = 0.0001 -> "100u", not "0.1m"
        assert_eq!(format_si(1e-4), "100u");
        // 0.015 -> "15m"
        assert_eq!(format_si(0.015), "15m");
    }

    #[test]
    fn negatives() {
        assert_eq!(format_si(-1e-3), "-1m");
        assert_eq!(format_si(-0.015), "-15m");
        assert_eq!(format_si(-1000.0), "-1k");
    }

    #[test]
    fn boundary_values() {
        assert_eq!(format_si(999.0), "999");
        assert_eq!(format_si(1000.0), "1k");
        // 999.5 rounds to 1000 -> "1k"
        assert_eq!(format_si(999.5), "1k");
        assert_eq!(format_si(0.999), "999m");
        // 0.0009999 -> 1m (rounded)
        assert_eq!(format_si(0.000_999_9), "1m");
    }

    #[test]
    fn rc_lowpass_capacitor() {
        // 100n stored as 1e-7
        assert_eq!(format_si(100e-9), "100n");
        // 1k
        assert_eq!(format_si(1000.0), "1k");
    }

    #[test]
    fn common_emitter_capacitor() {
        // 100u stored after parser may be 9.999...e-5 or 1e-4.
        // Both must format to "100u".
        assert_eq!(format_si(100e-6), "100u");
        assert_eq!(format_si(0.000_099_999_999_999_999_99), "100u");
    }

    #[test]
    fn nan_and_inf_passthrough() {
        assert_eq!(format_si(f64::NAN), format!("{}", f64::NAN));
        assert_eq!(format_si(f64::INFINITY), format!("{}", f64::INFINITY));
    }

    #[test]
    fn out_of_range_uses_scientific() {
        let s = format_si(1e16);
        assert!(s.contains('e'), "expected scientific, got {s}");
    }

    #[test]
    fn no_trailing_zeros_in_mantissa() {
        assert_eq!(format_si(1.0e-6), "1u");
        assert_eq!(format_si(1.10e3), "1.1k");
        assert_eq!(format_si(10.0e3), "10k");
    }
}
