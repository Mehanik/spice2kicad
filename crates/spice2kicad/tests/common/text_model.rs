#![allow(dead_code)]

//! Rendered-text geometry model shared by the test suite.
//!
//! Moved out of `electrical_safety.rs` so that BOTH the model-based V13
//! verifiers and the KiCad-render calibration tests
//! (`rendered_text.rs`) consume the *same* `text_bbox`. That is the
//! whole point of the calibration: a model that only ever checks itself
//! cannot be falsified, so `rendered_text.rs` measures real ink from
//! `kicad-cli sch export svg` and asserts this model is a tight
//! superset of it. If this file drifts from what KiCad draws, CI trips.

pub type Pt = (f64, f64);

#[derive(Debug, Clone, Copy)]
pub struct Bbox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Bbox {
    /// AABB intersection. Inclusive on edges; coincident-edge cases
    /// (a label touching a body's edge at a pin coordinate) are
    /// *quality* defects, not correctness ones, so the verifier
    /// treats them as overlap when both bboxes have non-zero area.
    pub fn intersects(&self, other: &Bbox) -> bool {
        self.x0 < other.x1 && self.x1 > other.x0 && self.y0 < other.y1 && self.y1 > other.y0
    }

    pub fn intersects_segment(&self, a: Pt, b: Pt) -> bool {
        // Strict-interior test mirroring `spice_route::types::Bbox`.
        let eps = 0.1;
        let xlo = self.x0 + eps;
        let xhi = self.x1 - eps;
        let ylo = self.y0 + eps;
        let yhi = self.y1 - eps;
        if xlo >= xhi || ylo >= yhi {
            return false;
        }
        let (x1, y1) = a;
        let (x2, y2) = b;
        if x1.max(x2) <= xlo || x1.min(x2) >= xhi {
            return false;
        }
        if y1.max(y2) <= ylo || y1.min(y2) >= yhi {
            return false;
        }
        if (x1 - x2).abs() < f64::EPSILON {
            x1 > xlo && x1 < xhi && y1.min(y2) < yhi && y1.max(y2) > ylo
        } else if (y1 - y2).abs() < f64::EPSILON {
            y1 > ylo && y1 < yhi && x1.min(x2) < xhi && x1.max(x2) > xlo
        } else {
            // The router only emits axis-aligned segments; treat
            // diagonals (shouldn't exist) as non-intersecting.
            false
        }
    }

    #[allow(dead_code)]
    pub fn contains(&self, p: Pt) -> bool {
        let eps = 0.1;
        p.0 > self.x0 + eps && p.0 < self.x1 - eps && p.1 > self.y0 + eps && p.1 < self.y1 - eps
    }
}

/// Which kind of text we're sizing a bbox for. Determines anchor
/// semantics (left vs centred) and any flavour-specific padding
/// (chevron lead for global labels).
#[derive(Debug, Clone, Copy)]
pub enum TextKind {
    /// Plain `(label …)` — KiCad anchors the text at the left edge.
    PlainLabel,
    /// A tag-bordered label — `(global_label …)` or
    /// `(hierarchical_label …)`. KiCad does NOT start the text at the
    /// anchor: `SCH_GLOBALLABEL::GetSchematicTextOffset` /
    /// `SCH_HIERLABEL::GetSchematicTextOffset` push it along the reading
    /// direction to clear the chevron. `lead_em` is that offset in
    /// multiples of the text size; the modelled box runs from the anchor
    /// (the chevron's own ink) through `lead + width`.
    ///
    /// Use [`TextKind::global_label`] / [`TextKind::hier_label`] rather
    /// than writing the field by hand — the constants come from KiCad's
    /// `DEFAULT_LABEL_SIZE_RATIO` / `DEFAULT_TEXT_OFFSET_RATIO` and are
    /// calibrated against real ink by `rendered_text.rs`.
    GlobalLabel { lead_em: f64 },
    /// `(property "Reference" …)` text — anchor centred or left
    /// depending on `(justify …)`. The emitter now writes `justify
    /// left` (V13 Step 5) so we model it as left-anchored.
    PropertyReference,
    /// `(property "Value" …)` text — same anchor rules as Reference.
    PropertyValue,
    /// A `(property "Value" …)` with NO `(justify …)` token — KiCad
    /// centres such a field horizontally about its anchor. Power-glyph
    /// net-name labels (`GND`/`VCC`/`VEE`) are emitted without a justify,
    /// so they render centred, not left-anchored. Modelling them as
    /// left-anchored over-estimates their rightward reach (a sliver into
    /// a neighbour to the right that KiCad never actually draws).
    CenteredValue,
}

impl TextKind {
    /// A `(global_label …)`, given its `(shape …)` token.
    ///
    /// `SCH_GLOBALLABEL::GetSchematicTextOffset`
    /// (`../kicad-source/eeschema/sch_label.cpp:2044`) offsets the text by
    /// `GetLabelBoxExpansion()` — `DEFAULT_LABEL_SIZE_RATIO` (0.375) ×
    /// text height — plus, for the arrow-headed shapes, three quarters of
    /// the height as a proxy for the triangle.
    #[must_use]
    pub fn global_label(shape: Option<&str>) -> Self {
        let arrowed = matches!(shape, Some("input" | "bidirectional" | "tri_state"));
        TextKind::GlobalLabel {
            lead_em: 0.375 + if arrowed { 0.75 } else { 0.0 },
        }
    }

    /// A `(hierarchical_label …)`.
    ///
    /// `SCH_HIERLABEL::GetSchematicTextOffset`
    /// (`../kicad-source/eeschema/sch_label.cpp:2336`) offsets by
    /// `GetTextOffset()` — `DEFAULT_TEXT_OFFSET_RATIO` (0.15) × height —
    /// plus one full `GetTextWidth()` (the square glyph cell), regardless
    /// of shape.
    #[must_use]
    pub fn hier_label() -> Self {
        TextKind::GlobalLabel { lead_em: 1.15 }
    }
}

/// Approximate the rendered text bbox of a label or property string.
///
/// References: KiCad's Newstroke font has an average advance of
/// roughly 0.6 × glyph height (see `../kicad-source/eeschema/sch_field.cpp`
/// and `../kicad-source/eeschema/sch_label.cpp`); we add 0.8 × size of
/// slack to absorb hinting variance and the small lead/trail margins
/// KiCad's renderer applies. Height is taken as 1.4 × size to cover
/// ascender + descender + line spacing.
///
/// `orientation_deg` rotates the unrotated bbox about the anchor and
/// the function returns the axis-aligned bounding box of the rotated
/// shape (matches what eeschema considers the field's visible bbox
/// for collision purposes).
pub fn text_bbox(
    text: &str,
    anchor: Pt,
    size_mm: f64,
    orientation_deg: u16,
    kind: TextKind,
) -> Bbox {
    let width = kicad_symbols::text_metrics::text_width(text, size_mm);
    let height = 1.4 * size_mm;
    // A plain label does not straddle its anchor. KiCad runs the file
    // angle through `EDA_ANGLE::KeepUpright()` (180 → 0, 270 → 90) and
    // `SCH_LABEL_BASE::SetSpinStyle` leaves the text bottom-justified, so
    // the body always sits on the −y side of a horizontal label and the
    // −x side of a vertical one, offset by the standoff — while the
    // *advance* direction still follows the full 0/90/180/270 angle.
    // The generic rotate-a-centred-box path below cannot express that.
    // Measured against `kicad-cli sch export svg` for all four rotations.
    if matches!(kind, TextKind::PlainLabel) {
        let depth = height + 0.35;
        let (ax, ay) = anchor;
        let (x0, y0, x1, y1) = match orientation_deg % 360 {
            90 => (ax - depth, ay - width, ax, ay),
            180 => (ax - width, ay - depth, ax, ay),
            270 => (ax - depth, ay, ax, ay + width),
            _ => (ax, ay - depth, ax + width, ay),
        };
        return Bbox { x0, y0, x1, y1 };
    }
    let chevron_lead = match kind {
        TextKind::GlobalLabel { lead_em } => lead_em * size_mm,
        _ => 0.0,
    };
    // Unrotated bbox in the anchor's local frame. Anchor is the
    // *left edge* for left-justified text; the bbox extends to the
    // right by `width`, half above and half below the baseline.
    // Property text is also left-anchored (the emitter writes
    // `(justify left)`); plain/global labels are likewise anchored
    // on the leftmost edge for `orientation 0`.
    // KiCad's stroke font is not vertically centred on the anchor: the
    // cap line sits ~0.54 × size above it while a descender (`p`, `g`,
    // `y`) drops ~0.79 × size below. `height / 2` (0.7 × size) covers the
    // ascender side but clips the descender — measured on the
    // `opamp_inverting` `inp` hierarchical label, which escaped the old
    // box by 0.11 mm. Calibrated by `rendered_text.rs`.
    let descender = 0.12 * size_mm;
    let (top, bot) = (-height / 2.0, height / 2.0 + descender);
    let (lx, rx, ty, by) = match kind {
        TextKind::PlainLabel | TextKind::PropertyReference | TextKind::PropertyValue => {
            (-0.0, width, top, bot)
        }
        TextKind::CenteredValue => (-width / 2.0, width / 2.0, top, bot),
        // The tag's own ink starts AT the anchor (chevron / triangle) and
        // the text only begins `chevron_lead` further along the reading
        // direction — it does not straddle the anchor backwards, which is
        // what the pre-calibration model wrongly assumed.
        TextKind::GlobalLabel { .. } => (0.0, chevron_lead + width, top, bot),
    };
    // Rotate the four corners about the anchor. KiCad's schematic
    // file Y axis points DOWN on screen (eeschema renders with the
    // Y-flip on load), and rotation tokens are CCW *on screen*. To
    // produce a file-frame AABB matching what KiCad draws, we negate
    // the sine component so that rot=90 maps right-extending text to
    // upward (i.e. decreasing file Y).
    let theta = f64::from(orientation_deg).to_radians();
    let (s, c) = (theta.sin(), theta.cos());
    let corners = [(lx, ty), (rx, ty), (rx, by), (lx, by)];
    let mut x0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y0 = f64::INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for (px, py) in corners {
        let wx = anchor.0 + c * px + s * py;
        let wy = anchor.1 - s * px + c * py;
        x0 = x0.min(wx);
        x1 = x1.max(wx);
        y0 = y0.min(wy);
        y1 = y1.max(wy);
    }
    Bbox { x0, y0, x1, y1 }
}
