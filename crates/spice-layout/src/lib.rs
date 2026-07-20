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
pub mod glyph_geom;
mod idioms;
pub mod layers;
pub mod legalize;
pub mod net_class;
pub mod orient;
pub mod sheets;
pub mod sidecar;
mod solver;
mod symmetry;

pub use sheets::place_sheets;
pub use solver::LayoutOptions;

use std::collections::{HashMap, HashSet};

use kicad_symbols::{Library, Orientation, Symbol};
use spice_diagnostics::{Diagnostic, Label, Span};
use spice_policy::CheckedNetlist;
use spice_resolve::{Axis, ElementRole, Relation, Value};

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
    /// True when this element is a voltage source flagged as a power
    /// rail (`ElementRole::Power`, set via `;@ power=` / `*@power`).
    /// Such sources are a power *rail*, not a drawn component: the
    /// emitter suppresses their `(symbol …)` instance and their own
    /// pins (annotation-spec §4.5, V10). The element is still placed
    /// (so parallel index arrays and the negative-rail Y hint stay in
    /// sync); only its rendering is suppressed.
    pub is_power_source: bool,
    /// The raw `;@ power=<rail>` / `*@power` rail string when this
    /// element is a power source (`ElementRole::Power(rail)`), else
    /// `None`. Carried so the emitter can distinguish a *negative* rail
    /// (rail string begins with `-`, e.g. `-12V`) from a positive one
    /// without re-deriving from the checked netlist — the single source
    /// of truth for negative-rail glyph selection (`power:VEE` vs
    /// `power:VCC`/`power:GND`). See `net_class::negative_rail_nets`.
    pub power_rail: Option<String>,
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

/// Minimum clear gap (mm) the placer keeps between two adjacent
/// elements' *resolved* extents (body bbox ∪ pin reach ∪ value-text
/// estimate). This is a hard spacing floor applied at the
/// candidate-generation boundary, NOT a soft cost — derived spacing
/// makes body/pin/text overlap infeasible by construction. One grid
/// cell (1.27 mm) of breathing room reads as a clean gap.
const MIN_CLEARANCE_MM: f64 = GridPoint::STEP_MM;

/// Estimated per-character advance (mm) of the value/property text the
/// emitter renders next to a symbol, at the default 1.27 mm text size.
/// Used so neighbouring elements clear each other's value text too.
/// (Mirrors the emitter's `text_bbox` width estimate ≈ chars*0.6*size;
/// rounded up for margin.)
pub(crate) const VALUE_CHAR_MM: f64 = 0.76;

/// World-frame offset (mm) from a symbol origin at which the emitter
/// left-justifies the value text. The text occupies
/// `[VALUE_TEXT_OFFSET_MM, VALUE_TEXT_OFFSET_MM + width]` on the +X
/// side of the origin. Matches the emitter's value-property anchor,
/// which it places at local `(2.54, 2.54)` (see
/// `kicad-emitter/src/schematic.rs`'s `property_anchor(.., 2.54, 2.54)`
/// call) — modelling it at 0 underestimated the text's right reach by a
/// full 2.54 mm, so align-clustered members crowded their neighbour.
pub(crate) const VALUE_TEXT_OFFSET_MM: f64 = 2.54;

/// Half-height (mm) of a rendered `Reference` / `Value` property text
/// box, at the default 1.27 mm text size. The emitter's `text_bbox`
/// model gives ~1.78 mm total height; half of that, rounded up for
/// margin.
///
/// Together with [`VALUE_TEXT_OFFSET_MM`] this gives the property
/// text's total vertical reach from the symbol origin: **3.44 mm**.
/// That is *inside* the align path's existing 3.81 mm (3-cell) spacing
/// floor, so reserving it is measurably a no-op on every fixture today
/// — see the ADR-14 completion note in `docs/layout-adr.md`. It is
/// reserved anyway because the model should be faithful *before*
/// anything reduces that floor, which is exactly what ADR-17 Stage 2's
/// compaction did (and why it breached four label invariants).
pub(crate) const PROP_TEXT_HALF_H_MM: f64 = 0.9;

/// Guaranteed clear horizontal gap (mm) between a left align-cluster
/// member's rendered value text and the right member's *drawn body*
/// (pins excluded — a pin is a connection point a wire lands on). Two
/// grid cells reads as a clean separation rather than the bare
/// one-cell grid-snap kiss the old stride produced. Applied as a HARD
/// spacing floor at the align-stride candidate boundary only — the seed
/// layer-stride (which deliberately excludes value-text width) is left
/// untouched. Not a soft cost.
const ALIGN_TEXT_GAP_MM: f64 = 2.0 * GridPoint::STEP_MM;

/// Resolved world-frame extents of an element relative to its origin,
/// in millimetres. `min_x`/`max_x` are signed offsets from the origin
/// along +X (right) and -X (left); `min_y`/`max_y` along the world Y
/// axis. The extent unions the orientation-transformed body bbox, the
/// reach of every pin stem, and (on +X) an estimate of the value-text
/// width so neighbours clear it.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct WorldExtent {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

/// Compute the resolved world extent of `symbol` placed at the origin
/// in the given `orientation`, with an optional `value` text whose
/// estimated width pads the +X side. World frame matches the emitter:
/// a local point `(lx, ly)` maps to `(rx, -ry)` where
/// `(rx, ry) = orientation.apply_point(lx, ly)` (eeschema y-flip).
fn world_extent(symbol: &Symbol, orientation: Orientation, value: Option<&str>) -> WorldExtent {
    let mut min_x = 0.0_f64;
    let mut max_x = 0.0_f64;
    let mut min_y = 0.0_f64;
    let mut max_y = 0.0_f64;
    let mut grow = |dx: f64, dy: f64| {
        min_x = min_x.min(dx);
        max_x = max_x.max(dx);
        min_y = min_y.min(dy);
        max_y = max_y.max(dy);
    };

    if let Some(b) = symbol.body_bbox() {
        for (lx, ly) in [(b.x0, b.y0), (b.x0, b.y1), (b.x1, b.y0), (b.x1, b.y1)] {
            let (rx, ry) = orientation.apply_point(lx, ly);
            grow(rx, -ry);
        }
    }
    for p in symbol.pins_in(orientation) {
        // `pins_in` already applies the orientation; eeschema y-flip on
        // top of that gives the world-relative offset.
        grow(p.x, -p.y);
    }
    if let Some(v) = value {
        let chars = v.chars().count();
        if chars > 0 {
            #[allow(clippy::cast_precision_loss)]
            let w = VALUE_TEXT_OFFSET_MM + (chars as f64) * VALUE_CHAR_MM;
            // ADR-14 completion (partial): reserve the property text as a
            // real BOX, not a zero-height ray on +X. The emitter anchors
            // Reference at local (2.54, -2.54) and Value at (2.54, 2.54),
            // each drawn ~1.78 mm tall, so the text occupies a band above
            // AND below the origin that nothing here reserved.
            //
            // The band is symmetric because both fields are reserved and
            // the placer has no orientation-faithful field-direction
            // model (the emitter's is `field_render_rotation`); symmetric
            // is the conservative reading. The WIDTH is still the Value
            // estimate only — a longer Reference is not modelled, and
            // neither is label text (see the Stage-4 note below).
            let half_h = VALUE_TEXT_OFFSET_MM + PROP_TEXT_HALF_H_MM;
            grow(w, half_h);
            grow(w, -half_h);
        }
    }
    WorldExtent {
        min_x,
        max_x,
        min_y,
        max_y,
    }
}

/// Resolved world extent of `element` placed at the origin in
/// `orientation`, with the **power-glyph reach** of every rail pin
/// folded in (ADR-14 Option A). Beyond [`world_extent`]'s body ∪ pin ∪
/// value-text terms, this also reserves — outward of each rail pin — the
/// cell(s) the `power:*` glyph body and its net-name value text will
/// occupy in decoration, so the seed/align spacing keeps foreign bodies
/// that whole zone clear as a HARD spacing floor (same mechanism and
/// tier as the existing body/pin no-overlap, V6 Tier-1). Adds only
/// outward spacing; never narrows the orientation set (V5 untouched).
pub(crate) fn world_extent_with_glyphs(
    element: &spice_resolve::ResolvedElement,
    orientation: Orientation,
    value: Option<&str>,
    prefs: &HashMap<String, crate::net_class::VertPref>,
) -> WorldExtent {
    let mut ext = world_extent(&element.symbol, orientation, value);
    for (dx, dy) in crate::glyph_geom::glyph_reach(element, orientation, prefs) {
        ext.min_x = ext.min_x.min(dx);
        ext.max_x = ext.max_x.max(dx);
        ext.min_y = ext.min_y.min(dy);
        ext.max_y = ext.max_y.max(dy);
    }
    ext
}

/// World-frame left reach (mm, as a non-negative magnitude) of a
/// symbol's *body* alone — orientation-transformed body bbox, pins
/// excluded. `0.0` if the symbol has no body bbox. Used by the align
/// stride's text-clearance term: a pin is a connection point a wire
/// lands on, so value text need only clear the neighbour's drawn body,
/// not its pin stems.
fn body_left_reach(symbol: &Symbol, orientation: Orientation) -> f64 {
    let Some(b) = symbol.body_bbox() else {
        return 0.0;
    };
    let mut min_x = 0.0_f64;
    for (lx, ly) in [(b.x0, b.y0), (b.x0, b.y1), (b.x1, b.y0), (b.x1, b.y1)] {
        let (rx, _ry) = orientation.apply_point(lx, ly);
        min_x = min_x.min(rx);
    }
    -min_x
}

/// Number of whole grid cells needed to separate two adjacent
/// elements' origins along a vertical column so their resolved extents
/// (plus `MIN_CLEARANCE_MM`) do not intersect. `upper` is the element
/// on the smaller-world-Y side, `lower` on the larger. The required
/// centre-to-centre distance is `upper.max_y - lower.min_y +
/// clearance`, snapped UP to the grid. (The horizontal counterpart is
/// inlined in the align loop, which combines a body/pin no-overlap term
/// with a value-text clear-gap term.)
fn vertical_stride_cells(upper: &WorldExtent, lower: &WorldExtent) -> i32 {
    let need_mm = upper.max_y + (-lower.min_y) + MIN_CLEARANCE_MM;
    mm_up_to_cells(need_mm)
}

/// Round a millimetre distance UP to a whole number of grid cells
/// (>= 1), so the result always lands on the schematic grid.
fn mm_up_to_cells(mm: f64) -> i32 {
    let cells = (mm / GridPoint::STEP_MM).ceil();
    #[allow(clippy::cast_possible_truncation)]
    let c = cells as i32;
    c.max(1)
}

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
    // Snapshot the pins that come from *user* directives, before the
    // cache hint, V7 symmetry and the idiom detector add their own.
    // Legality outranks all three of those — they are Tier-2 aesthetic
    // heuristics and a cache is a convenience — but it must never
    // override an explicit `*@place` / `*@align`, so only this mask is
    // treated as immovable when legalizing.
    let user_pinned = pinned.clone();
    apply_hint(&mut placement, &mut pinned, hint);
    // V7: detect structural symmetry in the netlist and mirror paired
    // elements about a common vertical axis. Runs after V6 archetype
    // seeding so the axis is computed from a topology-aware base
    // layout when one exists, and before V5 orientation so the pinned
    // pair geometry guides the orientation chooser for the rest of
    // the circuit.
    let sym_plan = symmetry::detect_pairs(&checked);
    // Elements pinned *by V7 and only by V7* — the difference the
    // symmetry pass makes to the mask. `apply_rail_stub_columns` unlocks
    // exactly these: V7 owns a pair's mirror RELATION, not either
    // member's absolute column. A cache hint (ADR-4) or a user directive
    // on the same element is NOT in this set and still locks.
    let mut sym_pinned = vec![false; pinned.len()];
    let sym_axis = if let Some(plan) = &sym_plan {
        let axis = symmetry::axis_sum(&placement, &pinned, plan);
        let before = pinned.clone();
        symmetry::apply(&mut placement, &mut pinned, plan);
        for (i, s) in sym_pinned.iter_mut().enumerate() {
            *s = pinned[i] && !before[i];
        }
        Some(axis)
    } else {
        None
    };
    // Idiom detection (roadmap §6, v0.2 Item 4): infer placement
    // constraints from recurring analog sub-topologies and emit them
    // through the same `align`/pin channel the user `align` path uses.
    // Runs AFTER V7 symmetry so a symmetry pin always wins (the
    // detector skips already-pinned elements) and BEFORE
    // `pick_orientations` so the newly-pinned pairs guide V5/V3.
    let dividers = idioms::detect_dividers(&checked);
    idioms::apply(&mut placement, &mut pinned, &checked, &dividers);
    apply_position_idioms(&mut placement, &mut pinned, &checked);
    // Idiom 4: pull each rail stub into the column of the node it
    // terminates. Runs last among the seed idioms so it can read every
    // stronger pin (hint / symmetry / divider / shared-centre) and skip
    // those columns, and BEFORE `pick_orientations` like its siblings.
    apply_rail_stub_columns(
        &mut placement,
        &pinned,
        &user_pinned,
        &sym_pinned,
        &checked,
        library,
        sym_plan.as_ref().zip(sym_axis),
    );
    // V14: per-element allowed-orientation set (power pin up / ground
    // pin down). A *hard* candidate-space filter, threaded into both
    // the V5 seed chooser below and the SA refiner so the constraint is
    // hard at *every* stage that can move an element (CLAUDE.md
    // "consistency requirement").
    let allowed = orient::allowed_orientations(&checked);
    pick_orientations(&mut placement, &pinned, &checked, &allowed);

    let glyph_prefs = net_class::vertical_prefs(&checked);
    if !opts.refine {
        legalize_if_needed(
            &mut placement,
            &user_pinned,
            &checked,
            library,
            &glyph_prefs,
        );
        return Ok(placement);
    }
    let mut placement = solver::refine(placement, &pinned, &checked, library, opts, &allowed);
    // Legalization is a **last resort, after refinement** — not a seed pass.
    //
    // The original argument for legalizing the seed was that a hard filter
    // governs moves and cannot repair an infeasible start, so an annealer
    // handed overlapping bodies could only decline to worsen them. That
    // premise turned out to be false in practice: the SA's gate admits
    // moves that *reduce* the overlap count, so it resolves seed overlaps
    // on its own, and every fixture ends refinement with zero overlaps
    // whether or not the seed was legalized.
    //
    // What legalizing the seed *did* do was perturb the SA's starting
    // point, sending it down a different trajectory. On `common_emitter`
    // that trajectory put RE's ground pin directly under the net-`e`
    // trunk, so the router speared it — a Tier-0 V11 short — and cost
    // three more Tier-1 V12/V13 text and obstacle invariants elsewhere,
    // in exchange for one Tier-2 crossing. CLAUDE.md's ordering rule
    // forbids that trade outright, and the placer cannot even see the
    // regression: it lives in emitted geometry (glyph and flag pins,
    // routed wires) that `spice-layout` has no access to.
    //
    // Running the pass *after* refinement removes the trade entirely. When
    // the SA has already produced a legal placement — every fixture today
    // — the pass finds nothing to shove and is a bit-exact no-op, so it
    // cannot perturb anything. When the SA genuinely fails to clear an
    // overlap, the postcondition still has an owner. Legality stops being
    // something the optimiser is merely discouraged from violating without
    // that guarantee costing correctness.
    legalize_if_needed(
        &mut placement,
        &user_pinned,
        &checked,
        library,
        &glyph_prefs,
    );
    Ok(placement)
}

/// Run the legalizer only when the placement actually violates the
/// no-overlap postcondition.
///
/// The guard is not an optimization — it is what makes the pass safe to
/// run at all. `legalize` is deterministic and already leaves a legal
/// placement untouched, but checking first makes "no fixture is perturbed
/// unless it is already broken" explicit rather than an emergent property
/// of the shove loop, and it keeps the debug log quiet on the common path.
fn legalize_if_needed(
    placement: &mut Placement,
    user_pinned: &[bool],
    checked: &spice_policy::CheckedNetlist,
    library: &Library,
    glyph_prefs: &HashMap<String, crate::net_class::VertPref>,
) {
    let overlaps = legalize::overlap_count(placement, checked, library);
    if overlaps == 0 {
        return;
    }
    log::debug!("legalize: {overlaps} overlapping footprint pair(s) survived refinement");
    legalize::legalize(placement, user_pinned, checked, library, glyph_prefs);
}

/// Apply the POSITION-only canonical-placement idioms (Tier-2 V6/V7)
/// through the same `align`/pin channel the divider idiom and the user
/// `*@align` path use. Each detector is strict and skips already-pinned
/// elements, so a user `align`/`place` or V7 symmetry pin always wins.
/// These move (and pin) elements only — never orientation-flow logic
/// (`pick_orientations` / the SA rotate move are untouched).
///
/// Currently wired: **Idiom 3, shared-node center** (differential-pair
/// tail / shared-emitter resistor centred under its transistors), run
/// after the divider idiom and after V7 symmetry.
///
/// Two sibling idioms are implemented+unit-tested in `idioms` but
/// deliberately **not** wired (deferred), each because it cannot land as a
/// *position-only* pass without regressing a higher tier:
///
/// * **Idiom 1, parallel two-terminal pair** (`detect_parallel_pairs`).
///   Stacking a parallel `R‖C` in one X column (what the e2e test
///   requires) interleaves the pins when one shared net is ground: the
///   non-ground net's wire must pass the ground pin, a V11 silent short
///   (Tier 0), plus V12/V14/V5 fallout on `common_emitter`. The only fix
///   is to *flip* one element so the shared inner pins coincide — an
///   orientation change, and the left→right orientation flow is walled
///   (this phase is position-only). Deferred to a v0.2 that owns the flip.
/// * **Idiom 2, collector-load** (`detect_collector_loads`). Repositioning
///   the collector resistor ripples the busiest crossing/wire-length
///   ratchets across `diff_pair` / `common_emitter` / `multivibrator`, and
///   on `diff_pair` V7 symmetry already pins `RC1`/`RC2` (so the idiom
///   would either no-op or fight the symmetry-wins ordering rule).
fn apply_position_idioms(placement: &mut Placement, pinned: &mut [bool], checked: &CheckedNetlist) {
    let centers = idioms::detect_shared_node_centers(checked);
    idioms::apply_shared_centers(placement, pinned, checked, &centers);
}

/// Seed pass: move rail stubs into the column of the node they
/// terminate (idiom 4, `idioms::apply_rail_stub_columns`).
///
/// # Why this is a seed pass and not left to the SA
///
/// A stub's X comes from `assign_x_layers`, which prunes rail edges from
/// the signal DAG — so a part whose *only* signal connection is the node
/// it hangs off gets a column with no relation to that node. That is a
/// **seed** defect, and `docs/layout-adr.md` ("Symbol-body overlap … is a
/// *seed* defect") records the rule that seed defects must not be
/// attacked from inside the annealer. Measured here too: with only the
/// `cost::rail_stub_alignment` term added and no seed pass,
/// `common_emitter`'s `RC` did not move at all, and `diff_pair`'s
/// `RC1`/`RC2` *cannot* move — they are pinned by the fixture's own
/// `*@align horizontal RC1 RC2`, so the SA never gets a vote.
///
/// # What may move, and the guarantee that user intent survives
///
/// `x_locked` marks every element whose **X** is owned by something
/// stronger than this heuristic: anything pinned by the position-cache
/// hint, by V7 symmetry, or by an earlier idiom (all of which are
/// `pinned` after the seed but were not `user_pinned` by it), plus every
/// member of an `*@align vertical` cluster, whose shared column *is* the
/// constraint.
///
/// Members of an `*@align horizontal` cluster and targets of a `*@place`
/// directive are deliberately **not** locked. `align horizontal`
/// constrains a shared *row*; the X spread the seeder gives its members
/// is an arbitrary by-product, and correcting it is exactly defect 4
/// (`diff_pair`'s `RC1`/`RC2` sitting left of the transistors they load).
/// `place=right-of` constrains an *ordering*, which survives moving both
/// sides. To make that safe rather than merely likely, the pass is
/// applied to a clone and **reverted wholesale** if the user-constraint
/// residual got worse.
///
/// That check goes through [`cost::constraint_residual`] — the very
/// function the SA objective scores — rather than re-deriving what each
/// relation means. Re-deriving is what broke it the first time: the
/// original guard only compared X *orderings* for `RightOf` / `LeftOf`
/// and waved `Above` / `Below` through on the reasoning "those are
/// vertical and this pass never changes Y". But `place_residual`'s
/// `Above` / `Below` arms also carry an `(ax - tx)²` term — "above"
/// means *directly* above, i.e. sharing a column — so moving X alone is
/// enough to violate them. Caught by
/// `spice-layout/tests/cost.rs::stage1_clean_placement_has_zero_constraint_violation`
/// on a generated `place=above` scenario. Scoring through the shared
/// function means a future relation gaining a new residual term is
/// honoured here for free.
fn apply_rail_stub_columns(
    placement: &mut Placement,
    pinned: &[bool],
    user_pinned: &[bool],
    sym_pinned: &[bool],
    checked: &CheckedNetlist,
    library: &Library,
    symmetry: Option<(&symmetry::SymmetryPlan, i32)>,
) {
    let stubs = idioms::detect_rail_stubs(checked);
    if stubs.is_empty() {
        return;
    }

    // A V7 symmetry pin owns the mirror RELATION between a pair, not
    // either member's absolute column — so a stub pinned by V7 *alone*
    // is free to move here, provided the relation is restored afterwards
    // (`symmetry::remirror`, below). Without that unlock the idiom is a
    // total no-op on any circuit whose symmetry V7 detects but whose
    // members the user did not also pin: measured on `multivibrator`,
    // V7 pins all eight elements, so every rail-stub group contained a
    // pinned member and was skipped wholesale, leaving `RC1` 17.8 mm off
    // `Q1`'s collector column — the exact defect this idiom exists to
    // fix on `common_emitter`, silently excluded on the symmetric
    // fixtures. `sym_pinned` is the V7-ONLY difference, so a cache hint
    // (ADR-4 position stability) or an earlier idiom on the same element
    // still locks it.
    let mut x_locked: Vec<bool> = (0..pinned.len())
        .map(|i| pinned[i] && !user_pinned[i] && !sym_pinned[i])
        .collect();

    let refdes_to_index: HashMap<&str, usize> = placement
        .elements
        .iter()
        .enumerate()
        .map(|(i, p)| (p.refdes.as_str(), i))
        .collect();
    for spec in &checked.align {
        if spec.axis != Axis::Vertical {
            continue;
        }
        for r in &spec.refdes {
            if let Some(&i) = refdes_to_index.get(r.as_str()) {
                x_locked[i] = true;
            }
        }
    }

    let before = placement.clone();
    let residual_before = cost::constraint_residual(placement, checked, library);
    let overlaps_before =
        legalize::immovable_overlap_count(placement, checked, library, user_pinned);
    // Elements this pass released from their V7 pin (see `x_locked`).
    let sym_released: Vec<bool> = (0..pinned.len())
        .map(|i| sym_pinned[i] && !user_pinned[i])
        .collect();
    idioms::apply_rail_stub_columns(placement, &x_locked, &sym_released, checked, &stubs);
    // Restore the V7 mirror relation the unlock above allowed the idiom
    // to perturb. The anchors are themselves symmetric when the netlist
    // is (each pair's anchor device is the other's mirror image), so
    // this is usually a no-op; it is here so that V7 is guaranteed by
    // construction rather than by that coincidence.
    if let Some((plan, axis)) = symmetry {
        symmetry::remirror(placement, plan, user_pinned, axis);
    }
    let residual_after = cost::constraint_residual(placement, checked, library);
    let overlaps_after =
        legalize::immovable_overlap_count(placement, checked, library, user_pinned);
    // Strictly-worse only: an exactly-equal residual (the common case,
    // both zero) keeps the improvement.
    if residual_after > residual_before + 1e-9 {
        log::debug!(
            "rail-stub columns: reverted (user-constraint residual {residual_before} -> {residual_after})"
        );
        *placement = before;
        return;
    }
    // ...and reverted just as hard if the snap CREATED a body overlap
    // between two elements NOTHING DOWNSTREAM CAN SEPARATE.
    //
    // Load-bearing, Tier 0. `constraint_residual` alone cannot see this
    // failure: `place_residual`'s X term for `RightOf` / `LeftOf` is a
    // one-sided hinge (`(ax - tx).max(0)`, ε = 0), so collapsing the
    // target onto the anchor's *own* column scores an unchanged
    // residual of zero — "not strictly worse" — and the revert above
    // waves it through. A `place`d element is `user_pinned`, so the
    // post-refinement legalizer will not move it either, and the two
    // symbols reach the emitter stacked at one origin. Their pins then
    // coincide, KiCad shorts the two nets together, and the CLI's
    // connectivity verifier fails the conversion *after* writing the
    // file ("emitted schematic does not match the source netlist").
    //
    // Reproduced on `rc_lowpass_ports.cir` + `;@ place=right-of R1` on
    // `C1`: C1 is a ground-side rail stub on `out`, whose anchor column
    // is R1's own, so this idiom undid the 7.62 mm the `place` phase had
    // correctly opened up. Note the defect is NOT in `solve_place` — it
    // separates the pair correctly — nor is it specific to `place`: any
    // stub whose anchor column is already occupied by a symbol the
    // legalizer may not move hits it. Guarding on measured overlap
    // therefore fixes the whole class, and — unlike a pre-flight hard
    // error — leaves the user with the working schematic they asked
    // for. See `spice2kicad/tests/place_no_coincidence.rs`.
    //
    // **Why `user_pinned` and not the total overlap count.** This runs
    // at SEED time, where transient overlaps are normal and the SA plus
    // the post-refinement legalizer clear them. Gating on the total
    // count therefore reverts the idiom over overlaps that were never
    // going to survive — measured: it moved `rc_lowpass` (R1 rot
    // 270→90, C1 x 35.56→46.99, three glyphs with them) for no
    // corresponding gain. Only a pair where BOTH members are
    // `user_pinned` is genuinely unrepairable, and that is exactly the
    // defect case: a `place` target and its anchor are both pinned, as
    // are two `align` members.
    //
    // **Why `user_pinned` and not the total overlap count.** This runs
    // at SEED time, where transient overlaps are normal and the SA plus
    // the post-refinement legalizer clear them. Gating on the total
    // count therefore reverts the idiom over overlaps that were never
    // going to survive — measured: it moved `rc_lowpass` (R1 rot
    // 270→90, C1 x 35.56→46.99, three glyphs with them) for no
    // corresponding gain. Only a pair where BOTH members are
    // `user_pinned` is genuinely unrepairable, and that is exactly the
    // defect case: a `place` target and its anchor are both pinned, as
    // are two `align` members.
    if overlaps_after > overlaps_before {
        log::debug!(
            "rail-stub columns: reverted (body overlaps {overlaps_before} -> {overlaps_after})"
        );
        *placement = before;
    }
}

/// Per-element refinement metadata for the routing-aware orientation
/// refinement phase (CLAUDE.md "Layout phase 4.5"). Recomputed from the
/// same seed → hint → symmetry sequence [`place_with_hint`] runs, so the
/// `pinned` mask and `allowed` orientation sets are identical to the ones
/// the placer used. The downstream refinement (in `kicad-emitter`, the
/// only crate that can see both the placer and the real router) reads
/// these to decide which elements it may rotate and to which orientations.
#[derive(Debug, Clone)]
pub struct RefinementMeta {
    /// `true` for an element whose orientation/position is fixed by a
    /// hard constraint (`align` / `place`), by V7 symmetry, or by a
    /// position-stability hint. The refinement phase must not touch it.
    pub pinned: Vec<bool>,
    /// Per-element V14-allowed orientation set (the same hard
    /// candidate-space filter [`orient::allowed_orientations`] feeds the
    /// V5 seed chooser and the SA refiner). The refinement phase may only
    /// pick orientations from this set — it never widens V14.
    pub allowed: Vec<Vec<Orientation>>,
}

/// Compute the [`RefinementMeta`] for a netlist + hint, mirroring exactly
/// the pinned/allowed state [`place_with_hint`] establishes before the SA
/// refiner. Used by the routing-aware orientation-refinement phase so it
/// honours the same constraints (user `align`/`place`, V7 symmetry, hint
/// pins) and the same V14 candidate filter as the placer itself.
///
/// This deliberately recomputes (rather than threading extra return
/// values out of `place_with_hint`) so the existing entry-point signature
/// stays unchanged; the seed pass is cheap and side-effect-free.
pub fn refinement_meta(
    checked: &CheckedNetlist,
    hint: &Hint,
) -> Result<RefinementMeta, Vec<Diagnostic>> {
    let (mut placement, mut pinned) = place_seed(checked)?;
    apply_hint(&mut placement, &mut pinned, hint);
    if let Some(plan) = symmetry::detect_pairs(checked) {
        symmetry::apply(&mut placement, &mut pinned, &plan);
    }
    // Mirror the exact seed→hint→symmetry→idiom sequence
    // `place_with_hint` runs, so the refinement phase sees the same
    // `pinned` mask (the divider-pinned pair must not be reoriented).
    let dividers = idioms::detect_dividers(checked);
    idioms::apply(&mut placement, &mut pinned, checked, &dividers);
    apply_position_idioms(&mut placement, &mut pinned, checked);
    let allowed = orient::allowed_orientations(checked);
    Ok(RefinementMeta { pinned, allowed })
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

/// Baseline vertical step (grid cells) per rank within a (layer, slot)
/// bucket. A floor: [`bucket_y_strides`] may widen it for a bucket that
/// stacks two oversized bodies, never narrow it.
const Y_RANK_STRIDE: i32 = 5;

/// Y-band sub-slot used for band-aware seed stacking.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
enum Slot {
    Top,
    MidUp,
    MidCtr,
    MidLo,
    Bot,
}

/// Per-(layer, slot) vertical rank stride in grid cells, geometry-derived
/// (HARD, at the spacing boundary) — the Y counterpart of `place_seed`'s
/// per-layer X derivation, and the same shape as the align path's
/// [`vertical_stride_cells`].
///
/// [`Y_RANK_STRIDE`] alone is a fixed 5 cells (6.35 mm) no matter what is
/// stacked. That is ample for resistors and capacitors and far too tight
/// for an oversized body: two `Amplifier_Operational:OPAMP` triangles in
/// one bucket — a dual-opamp deck, i.e. every multi-channel circuit —
/// landed 6.35 mm apart with a 10.16 mm body and a 15.24 mm pin span, so
/// the SEED was already infeasible. Neither downstream owner can recover
/// from that: the SA overlap gate is a never-INCREASE filter, and
/// `legalize` shoves greedily in index order, which on two mutually
/// overlapping triangles merely relocates the clash. A hard constraint
/// cannot repair an infeasible start, so the start must not be
/// infeasible.
///
/// **Derivation.** Within a bucket the stride must cover the lower
/// element's upward reach plus the upper element's downward reach plus
/// [`MIN_CLEARANCE_MM`]. Taking the bucket-wide maxima of each gives one
/// uniform, order-independent stride that bounds every adjacent pair.
/// Floored at [`Y_RANK_STRIDE`], so it only ever *widens*.
///
/// **Reach = drawn body ∪ own rail-glyph reach.** Pins are excluded,
/// exactly as the align stride's `body_left_reach` excludes them: a pin
/// is a connection point a wire lands on, not ink that must clear a
/// neighbour. (Including pin reach makes a plain resistor demand 7 cells
/// and widens every bucket on every fixture — a whole-suite reshuffle for
/// no defect; measured.) The glyph reach *is* included, because two
/// stacked opamps must clear each other's VCC / VEE glyph body and
/// net-name text, not merely the two triangles: without that term the
/// channel between them is too tight and X2's VEE glyph collides three
/// ways — its net name lands on RF2's body, the `INV2` trunk crosses its
/// body, and RIN2 loses its outward first segment.
///
/// **Scope: a bucket holding TWO OR MORE oversized bodies.** That is the
/// categorically infeasible case and the only one measured to need
/// fixing. A bucket with one large body among small neighbours is left
/// alone deliberately: widening it too was implemented and measured, and
/// it regresses a second fixture (`opamp_inverting_real` V5 0→1, V16 B
/// 5→7) for no gain — the within-tier sideways trade the ratchet rule
/// forbids. "Oversized" is the same key the SA overlap gate uses: a body
/// half-extent past the cost's uniform cell. Every all-small-symbol
/// fixture therefore keeps its previous, well-tuned 5-cell spacing
/// exactly, and no such fixture moves.
fn bucket_y_strides(
    checked: &CheckedNetlist,
    layers: &[u32],
    element_slot: &[Slot],
    prefs: &HashMap<String, crate::net_class::VertPref>,
) -> HashMap<(u32, Slot), i32> {
    let cell_hh = f64::from(CELL_H) * GridPoint::STEP_MM / 2.0;
    let mut bucket_big: HashMap<(u32, Slot), Vec<(f64, f64)>> = HashMap::new();
    for (i, e) in checked.elements.iter().enumerate() {
        let mut down = 0.0_f64;
        let mut up = 0.0_f64;
        if let Some(b) = e.symbol.body_bbox() {
            for (lx, ly) in [(b.x0, b.y0), (b.x0, b.y1), (b.x1, b.y0), (b.x1, b.y1)] {
                let (_rx, ry) = Orientation::IDENTITY.apply_point(lx, ly);
                down = down.max(-ry);
                up = up.max(ry);
            }
        }
        if down.max(up) > cell_hh + 1e-6 {
            for (_dx, dy) in crate::glyph_geom::glyph_reach(e, Orientation::IDENTITY, prefs) {
                down = down.max(dy);
                up = up.max(-dy);
            }
            bucket_big
                .entry((layers[i], element_slot[i]))
                .or_default()
                .push((down, up));
        }
    }
    bucket_big
        .into_iter()
        .filter(|(_, big)| big.len() >= 2)
        .map(|(k, big)| {
            let down = big.iter().map(|r| r.0).fold(0.0_f64, f64::max);
            let up = big.iter().map(|r| r.1).fold(0.0_f64, f64::max);
            (
                k,
                mm_up_to_cells(down + up + MIN_CLEARANCE_MM).max(Y_RANK_STRIDE),
            )
        })
        .collect()
}

/// Stage-1 placer body: returns the seed placement plus a per-element
/// `pinned` mask (`true` for elements whose position is fixed by an
/// `align` or `place` directive).
///
/// Pipeline: classify nets → assign Y bands → assign X layers → emit
/// initial grid coordinates from `(band, layer, rank_in_layer)`. User
/// `align`/`place`/`power` directives then override the heuristic seed
/// via [`apply_user_constraints`], which pins the affected elements.
fn place_seed(checked: &CheckedNetlist) -> Result<(Placement, Vec<bool>), Vec<Diagnostic>> {
    use crate::bands::{Band, assign_y_bands};
    use crate::layers::assign_x_layers;
    use crate::net_class::classify_nets;

    // Geometry constants in grid cells (1.27 mm each).
    const Y_BAND_GAP: i32 = 6; // gap from rail edge into Mid band
    // Bound on the per-bucket left/right jitter (cells). Adjacent
    // layers' X positions must clear each other even after both layers
    // jitter toward one another, so the layer-spacing derivation adds
    // `2 * MAX_JITTER` cells of margin.
    const MAX_JITTER: i32 = 2;
    // Historical fixed per-layer stride (cells). Used as a *floor* so
    // all-small-symbol fixtures keep their previous spacing exactly;
    // the geometry derivation only widens layers that need more room.
    const X_STRIDE_FLOOR: i32 = 12;

    let n = checked.elements.len();
    let classes = classify_nets(checked);
    let band_asg = assign_y_bands(checked, &classes);
    let layer_asg = assign_x_layers(checked, &classes);
    // Per-net screen-vertical preference (Power → up, Ground/negative →
    // down). Used to identify rail pins whose power-glyph footprint the
    // layer stride must reserve (ADR-14 Option A).
    let prefs = crate::net_class::vertical_prefs(checked);

    // Per-layer X positions, geometry-derived (HARD, at the spacing
    // boundary). Each element's resolved world extent (identity
    // orientation — the seed default; `pick_orientations` may later
    // rotate movable elements, and the SA overlap gate guards that)
    // gives the layer its max left/right reach. Adjacent layers are
    // then spaced so the right reach of the left layer plus the left
    // reach of the right layer plus MIN_CLEARANCE_MM (plus jitter
    // margin) never lets two bodies clip — replacing the old fixed
    // 12-cell X_STRIDE that ignored the opamp triangle's 15 mm width.
    let max_layer = layer_asg.layers.iter().copied().max().unwrap_or(0);
    let mut layer_max_right = vec![0.0_f64; max_layer as usize + 1];
    let mut layer_max_left = vec![0.0_f64; max_layer as usize + 1];
    for (i, e) in checked.elements.iter().enumerate() {
        // Layer spacing uses body ∪ pin reach only (no value-text
        // pad): value text is justify-left and V13's concern, and
        // padding it here would shove a whole layer asymmetrically.
        // The align path *does* include value text (tighter, pinned
        // rows where text genuinely abuts a neighbour).
        //
        // It DOES reserve the power-glyph reach of every rail pin
        // (ADR-14 Option A): a glyph's body + net-name text is real
        // decoration geometry that lands outward of the pin, so the
        // layer stride must keep a foreign body that zone clear — a hard
        // spacing floor, same tier as the body/pin no-overlap.
        let ext = world_extent_with_glyphs(e, Orientation::IDENTITY, None, &prefs);
        let l = layer_asg.layers[i] as usize;
        layer_max_right[l] = layer_max_right[l].max(ext.max_x);
        layer_max_left[l] = layer_max_left[l].max(-ext.min_x);
    }
    let jitter_margin_mm = f64::from(MAX_JITTER) * GridPoint::STEP_MM;
    let mut layer_x = vec![0_i32; max_layer as usize + 1];
    for l in 1..=max_layer as usize {
        let gap_mm =
            layer_max_right[l - 1] + layer_max_left[l] + MIN_CLEARANCE_MM + 2.0 * jitter_margin_mm;
        // Floor at the historical fixed stride (X_STRIDE_FLOOR) so
        // all-small-symbol layers keep their previous, well-tuned
        // spacing (no V12/V13/V5 perturbation); only an oversized
        // layer (e.g. an opamp triangle) widens beyond it. The
        // derivation only ever *widens*, never narrows below baseline.
        let stride = mm_up_to_cells(gap_mm).max(X_STRIDE_FLOOR);
        layer_x[l] = layer_x[l - 1] + stride;
    }

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

    let bucket_stride = bucket_y_strides(checked, &layer_asg.layers, &element_slot, &prefs);

    // Reserve three sub-rows in Mid: upper / centre / lower.
    let mid_span = (y_mid_bot - y_mid_top).max(1);
    let mid_up_y = y_mid_top + mid_span / 4;
    let mid_ctr_y = y_mid_top + mid_span / 2;
    let mid_lo_y = y_mid_top + (3 * mid_span) / 4;

    // Per-(layer, slot) running rank.
    let mut bucket_rank: HashMap<(u32, Slot), i32> = HashMap::new();
    let mut placed: Vec<PlacedElement> = Vec::with_capacity(n);
    for (i, e) in checked.elements.iter().enumerate() {
        let layer = layer_asg.layers[i] as usize;
        let slot = element_slot[i];
        let rank = bucket_rank
            .entry((layer_asg.layers[i], slot))
            .and_modify(|r| *r += 1)
            .or_insert(0);
        let rank = *rank;
        // Within a (layer, slot) bucket, alternate elements left/
        // right of the layer column so multiple elements at the
        // same Y target don't pile on the same X. The jitter is
        // bounded to ±MAX_JITTER cells; the per-layer X spacing above
        // reserves matching margin so adjacent columns never clip.
        let raw_jitter = if rank % 2 == 0 {
            -(rank / 2)
        } else {
            (rank + 1) / 2
        };
        let x_jitter = raw_jitter.clamp(-MAX_JITTER, MAX_JITTER);
        let x = layer_x[layer] + x_jitter;

        let y_stride = bucket_stride.get(&(layer_asg.layers[i], slot)).copied();
        let y_stride = y_stride.unwrap_or(Y_RANK_STRIDE);
        let y = match slot {
            Slot::Top => y_top + rank * y_stride,
            Slot::MidUp => mid_up_y + rank * y_stride,
            Slot::MidCtr => mid_ctr_y + rank * y_stride,
            Slot::MidLo => mid_lo_y + rank * y_stride,
            Slot::Bot => y_bot - rank * y_stride,
        };
        placed.push(PlacedElement {
            refdes: e.refdes.clone(),
            lib_id: e.lib_id.clone(),
            origin: GridPoint::new(x, y),
            orientation: Orientation::IDENTITY,
            nodes: e.nodes.clone(),
            pin_mapping: e.pin_mapping.clone(),
            value: e.value.as_ref().map(format_value),
            is_power_source: matches!(e.role, ElementRole::Power(_)),
            power_rail: match &e.role {
                ElementRole::Power(rail) => Some(rail.clone()),
                ElementRole::Normal => None,
            },
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
    // Rail-pin screen-vertical preferences, for the align stride's
    // power-glyph reach reservation (ADR-14 Option A).
    let prefs = crate::net_class::vertical_prefs(checked);
    let CheckedNetlist {
        elements,
        align,
        place,
        subckts: _,
        sheet_instances: _,
        ports: _,
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
        let row_offset = cluster_index_i32 * (CELL_H + CLUSTER_GAP);
        // Spread members along the cluster axis. The stride between
        // each adjacent pair is *geometry-derived* (HARD, at the
        // spacing boundary): the gap covers both elements' resolved
        // extents (orientation-transformed body bbox ∪ pin reach ∪
        // value-text estimate) plus MIN_CLEARANCE_MM, snapped up to
        // the grid. Align-pinned members keep identity orientation
        // (`pick_orientations` skips pinned elements), so we compute
        // extents in `Orientation::IDENTITY`.
        //
        // `cursor` is the running offset (in grid cells) from the
        // anchor along the cluster axis; the first member sits at the
        // anchor coord.
        let mut cursor: i32 = 0;
        let mut prev_ext: Option<WorldExtent> = None;
        for refdes in &spec.refdes {
            let Some(&idx) = refdes_to_index.get(refdes.as_str()) else {
                continue;
            };
            let extent = world_extent_with_glyphs(
                &elements[idx],
                Orientation::IDENTITY,
                placed[idx].value.as_deref(),
                &prefs,
            );
            if let Some(prev_ext) = prev_ext {
                let geom = match spec.axis {
                    Axis::Horizontal => {
                        // Two independent hard spacing requirements; the
                        // stride is the larger. Both monotone-widen the
                        // gap, never shrink it.
                        //
                        // (1) Body/pin no-overlap: the left member's
                        //     full resolved extent (body ∪ pin ∪ value
                        //     text) plus MIN_CLEARANCE must clear the
                        //     right member's full extent.
                        let overlap_mm = prev_ext.max_x + (-extent.min_x) + MIN_CLEARANCE_MM;
                        // (2) Value-text clear gap: the left member's
                        //     value text must clear the right member's
                        //     *body* (pins excluded — a pin is a
                        //     connection point) by ALIGN_TEXT_GAP_MM. The
                        //     text reach is `prev_ext.max_x` (value width
                        //     folded in by `world_extent`); the right
                        //     body's left reach uses the body bbox only.
                        let text_gap_mm = prev_ext.max_x
                            + body_left_reach(&elements[idx].symbol, Orientation::IDENTITY)
                            + ALIGN_TEXT_GAP_MM;
                        mm_up_to_cells(overlap_mm.max(text_gap_mm))
                    }
                    Axis::Vertical => vertical_stride_cells(&prev_ext, &extent),
                };
                // Floor at the historical fixed cluster stride so
                // all-small-symbol clusters (e.g. diff_pair's RC1/RC2)
                // keep their previous, well-tuned spacing; only a
                // cluster with a wide member (a BJT) widens beyond it.
                // The derivation only ever widens, never narrows below
                // baseline (no V13 label/body perturbation on the
                // small-symbol clusters).
                let step = geom.max(CELL_W + CLUSTER_GAP);
                cursor += step;
            }
            prev_ext = Some(extent);
            if !fixed[idx] {
                let (x, y) = match spec.axis {
                    Axis::Horizontal => (anchor_x_seed + cursor, row_y_seed),
                    Axis::Vertical => (
                        anchor_x_seed + row_offset, // small per-cluster X bias
                        row_y_seed + cursor,
                    ),
                };
                placed[idx].origin = GridPoint::new(x, y);
                fixed[idx] = true;
            }
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
        // NOTE: Y grows DOWNWARD (KiCad screen coords), so `a`'s
        // *topmost* pin is its MINIMUM y and its *bottommost* pin is
        // its MAXIMUM y. These two arms once had the sign backwards
        // (picking max-y for "top"), which made `above`/`below` emit
        // the opposite of what spec §4.3 defines — the same
        // screen-Y-sign class of bug as the `cost::rail_direction`
        // inversion. See `tests/place_direction.rs`.
        Relation::Above => {
            // `b` sits ABOVE `a` (spec §4.3: anchor's top edge → the
            // element's bottom edge), i.e. at SMALLER y.
            //   b.origin.y + b_bottom.y = a.origin.y + a_top.y - CELL_H
            //   shared X.
            let (ax, ay) = pick(&a_pins, |p| (p.1, p.0));
            let (bx, by) = pick(&b_pins, |p| (-p.1, p.0));
            GridPoint::new(a_origin.x + ax - bx, a_origin.y + ay - CELL_H - by)
        }
        Relation::Below => {
            // `b` sits BELOW `a`, i.e. at LARGER y.
            //   b.origin.y + b_top.y = a.origin.y + a_bottom.y + CELL_H
            let (ax, ay) = pick(&a_pins, |p| (-p.1, p.0));
            let (bx, by) = pick(&b_pins, |p| (p.1, p.0));
            GridPoint::new(a_origin.x + ax - bx, a_origin.y + ay + CELL_H - by)
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

/// The placement-side property-text reservation (ADR-14 completion, partial).
///
/// These assert the *model*, not an emitted layout, on purpose. The
/// reservation's whole vertical reach (3.44 mm) currently fits inside the
/// align path's 3.81 mm spacing floor, so it moves no fixture and no
/// output test can pin it. Measured: exaggerating the half-height until
/// the total reach exceeds ~3.8 mm is what first perturbs `baseline_lock`.
/// Without these tests the term would be silently deletable.
#[cfg(test)]
mod property_text_reservation_tests {
    use super::{PROP_TEXT_HALF_H_MM, VALUE_TEXT_OFFSET_MM, world_extent};
    use kicad_symbols::{Library, Orientation};

    fn resistor() -> kicad_symbols::Symbol {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/")
            .join("kicad-symbols/tests/fixtures/Device.kicad_sym");
        Library::from_file(path)
            .expect("load Device")
            .lookup("Device:R")
            .expect("Device:R")
            .clone()
    }

    /// Property text is reserved as a BOX. Before this the term
    /// was `grow(w, 0.0)` — width only, zero height — so nothing reserved
    /// the band the Reference and Value text actually occupy above and
    /// below the origin.
    #[test]
    fn property_text_reserves_height_not_just_width() {
        let sym = resistor();
        let bare = world_extent(&sym, Orientation::IDENTITY, None);
        let texted = world_extent(&sym, Orientation::IDENTITY, Some("4.7k"));

        assert!(
            texted.max_x > bare.max_x,
            "the value text must still reserve its width on +X"
        );

        let reach = VALUE_TEXT_OFFSET_MM + PROP_TEXT_HALF_H_MM;
        assert!(
            texted.max_y >= reach && texted.min_y <= -reach,
            "property text must reserve its full vertical band on BOTH sides \
             (Value below, Reference above): got min_y={} max_y={}, want ±{reach}",
            texted.min_y,
            texted.max_y,
        );
    }

    /// An element with no value text reserves no property band — the term
    /// must not become an unconditional halo on every symbol.
    #[test]
    fn no_value_text_reserves_no_property_band() {
        let sym = resistor();
        let bare = world_extent(&sym, Orientation::IDENTITY, None);
        let empty = world_extent(&sym, Orientation::IDENTITY, Some(""));
        assert_eq!(
            (bare.min_y, bare.max_y),
            (empty.min_y, empty.max_y),
            "an empty value must reserve nothing"
        );
    }
}
