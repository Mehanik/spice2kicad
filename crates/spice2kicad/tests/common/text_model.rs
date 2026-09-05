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
    /// AABB intersection, exclusive on edges to within
    /// `kicad_symbols::text_geom::TOUCH_EPS_MM`.
    ///
    /// Two boxes that share an edge do NOT overlap — a label anchored on
    /// a pin abuts its host's body box by construction (a global label's
    /// tag is one text size deep, a KiCad pin is 1.27 mm long), and a
    /// bare `<` reported that shared edge as a 6 × 10⁻¹⁵ mm overlap
    /// whenever the arithmetic landed a few ulps low. The epsilon is one
    /// nanometre; see its own docs for why that cannot mask a defect.
    pub fn intersects(&self, other: &Bbox) -> bool {
        use kicad_symbols::text_geom::TOUCH_EPS_MM;
        self.x0 + TOUCH_EPS_MM < other.x1
            && self.x1 - TOUCH_EPS_MM > other.x0
            && self.y0 + TOUCH_EPS_MM < other.y1
            && self.y1 - TOUCH_EPS_MM > other.y0
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
    /// `(global_label …)` — the drawn **tag outline**, which contains
    /// the string. `end_caps` is how many pointed
    /// end-caps the shape draws (0, 1 or 2); see
    /// `kicad_symbols::text_geom::TextKind::GlobalLabel`, which this
    /// mirrors and which `rendered_text.rs` calibrates against real ink.
    GlobalLabel { end_caps: u8 },
    /// `(hierarchical_label …)` — the string box; its template tag is
    /// inside it. See `kicad_symbols::text_geom::TextKind::HierLabel`.
    HierLabel,
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
    /// Delegates to `kicad_symbols::text_geom::TextKind::global_label`.
    #[must_use]
    pub fn global_label(shape: Option<&str>) -> Self {
        let kicad_symbols::text_geom::TextKind::GlobalLabel { end_caps } =
            kicad_symbols::text_geom::TextKind::global_label(shape)
        else {
            unreachable!("global_label is always a global label")
        };
        TextKind::GlobalLabel { end_caps }
    }

    /// A `(hierarchical_label …)`.
    ///
    /// Delegates to `kicad_symbols::text_geom::TextKind::hier_label`.
    #[must_use]
    pub fn hier_label() -> Self {
        TextKind::HierLabel
    }
}

/// Renderer-faithful bbox of a label or property string.
///
/// This is a thin adapter over `kicad_symbols::text_geom::text_bbox` —
/// the ONE definition, shared with the emitter that *places* this text.
/// Keeping the geometry here as a second copy is exactly how the model
/// drifted from what the emitter reserved (a global label's lead was
/// applied symmetrically on both sides of the anchor rather than along
/// the reading direction only); the shared module removes that failure
/// mode. `rendered_text.rs` still calibrates the result against real ink
/// from `kicad-cli sch export svg`, so the shared model cannot drift
/// from KiCad either.
pub fn text_bbox(
    text: &str,
    anchor: Pt,
    size_mm: f64,
    orientation_deg: u16,
    kind: TextKind,
) -> Bbox {
    use kicad_symbols::text_geom as tg;
    let shared = match kind {
        TextKind::PlainLabel => tg::TextKind::PlainLabel,
        TextKind::GlobalLabel { end_caps } => tg::TextKind::GlobalLabel { end_caps },
        TextKind::HierLabel => tg::TextKind::HierLabel,
        TextKind::PropertyReference | TextKind::PropertyValue => tg::TextKind::LeftProperty,
        TextKind::CenteredValue => tg::TextKind::CenteredProperty,
    };
    let b = tg::text_bbox(text, anchor, size_mm, orientation_deg, shared);
    Bbox {
        x0: b.x0,
        y0: b.y0,
        x1: b.x1,
        y1: b.y1,
    }
}
