//! Renderer-faithful text bounding boxes — the ONE definition.
//!
//! Both the emitter (which *places* labels and property text, and must
//! know what footprint it is reserving) and the V13 verifiers (which
//! *grade* the result) need the same answer to "where does KiCad
//! actually draw this string?". They used to carry two hand-maintained
//! copies, which drifted: the emitter modelled a global label as a box
//! straddling its anchor with a symmetric `0.6 · size` chevron lead on
//! both ends, so it reserved ~2.5 mm of empty space *behind* the anchor
//! while real rendered ink escaped up to 0.68 mm past the far end. The
//! emitter was therefore positioning labels against a footprint sitting
//! ~1.4 mm from where KiCad draws them.
//!
//! What KiCad really does: `SCH_LABEL_BASE::GetSchematicTextOffset`
//! pushes the text *along the reading direction* only — it never
//! straddles the anchor backwards. The tag's own ink (chevron /
//! triangle) starts at the anchor, and the string begins `lead` further
//! along. See `../kicad-source/eeschema/sch_label.cpp:2044` (global) and
//! `:2336` (hierarchical).
//!
//! The numbers here are calibrated against real ink measured from
//! `kicad-cli sch export svg` by
//! `crates/spice2kicad/tests/rendered_text.rs`; that suite asserts this
//! model is a tight superset of what KiCad renders, so the model cannot
//! silently drift back.

/// Axis-aligned bounding box in world mm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextBox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

/// Which kind of text we're sizing a bbox for. Determines anchor
/// semantics (left vs centred) and any flavour-specific lead.
#[derive(Debug, Clone, Copy)]
pub enum TextKind {
    /// Plain `(label …)` — KiCad anchors the text at the left edge and
    /// leaves it bottom-justified, so the body sits entirely on one
    /// side of the anchor.
    PlainLabel,
    /// A tag-bordered label — `(global_label …)` or
    /// `(hierarchical_label …)`. `lead_em` is
    /// `GetSchematicTextOffset`'s along-reading-direction push, in
    /// multiples of the text size. The modelled box runs from the
    /// anchor (the tag's own ink) through `lead + width`.
    ///
    /// Use [`TextKind::global_label`] / [`TextKind::hier_label`] rather
    /// than writing the field by hand.
    TaggedLabel { lead_em: f64 },
    /// `(property "Reference" …)` / `(property "Value" …)` text carrying
    /// an explicit `(justify left)` — anchored on its left edge.
    LeftProperty,
    /// A property with NO `(justify …)` token — KiCad centres such a
    /// field horizontally about its anchor. Power-glyph net-name labels
    /// (`GND`/`VCC`/`VEE`) are emitted without a justify.
    CenteredProperty,
}

impl TextKind {
    /// A `(global_label …)`, given its `(shape …)` token.
    ///
    /// `SCH_GLOBALLABEL::GetSchematicTextOffset` (`sch_label.cpp:2044`)
    /// offsets the text by `GetLabelBoxExpansion()` —
    /// `DEFAULT_LABEL_SIZE_RATIO` (0.375) × text height — plus, for the
    /// arrow-headed shapes, three quarters of the height as a proxy for
    /// the triangle.
    #[must_use]
    pub fn global_label(shape: Option<&str>) -> Self {
        let arrowed = matches!(shape, Some("input" | "bidirectional" | "tri_state"));
        TextKind::TaggedLabel {
            lead_em: 0.375 + if arrowed { 0.75 } else { 0.0 },
        }
    }

    /// A `(hierarchical_label …)`.
    ///
    /// `SCH_HIERLABEL::GetSchematicTextOffset` (`sch_label.cpp:2336`)
    /// offsets by `GetTextOffset()` — `DEFAULT_TEXT_OFFSET_RATIO` (0.15)
    /// × height — plus one full `GetTextWidth()` (the square glyph
    /// cell), regardless of shape.
    #[must_use]
    pub fn hier_label() -> Self {
        TextKind::TaggedLabel { lead_em: 1.15 }
    }
}

/// KiCad's default schematic text size (mm).
pub const DEFAULT_TEXT_SIZE_MM: f64 = 1.27;

/// Renderer-faithful bbox of `text` drawn at `anchor`, rotated
/// `orientation_deg` CCW *on screen*.
///
/// The schematic file's Y axis points down (eeschema applies the Y-flip
/// on load), so the sine component is negated to produce a file-frame
/// AABB matching what KiCad draws.
#[must_use]
pub fn text_bbox(
    text: &str,
    anchor: (f64, f64),
    size_mm: f64,
    orientation_deg: u16,
    kind: TextKind,
) -> TextBox {
    let width = crate::text_metrics::text_width(text, size_mm);
    let height = 1.4 * size_mm;

    // A plain label does not straddle its anchor. KiCad runs the file
    // angle through `EDA_ANGLE::KeepUpright()` (180 → 0, 270 → 90) and
    // `SCH_LABEL_BASE::SetSpinStyle` leaves the text bottom-justified,
    // so the body always sits on the −y side of a horizontal label and
    // the −x side of a vertical one, offset by the standoff — while the
    // *advance* direction still follows the full 0/90/180/270 angle.
    // The generic rotate-a-centred-box path below cannot express that.
    // Measured against `kicad-cli sch export svg` for all four
    // rotations.
    if matches!(kind, TextKind::PlainLabel) {
        let depth = height + 0.35;
        let (ax, ay) = anchor;
        let (x0, y0, x1, y1) = match orientation_deg % 360 {
            90 => (ax - depth, ay - width, ax, ay),
            180 => (ax - width, ay - depth, ax, ay),
            270 => (ax - depth, ay, ax, ay + width),
            _ => (ax, ay - depth, ax + width, ay),
        };
        return TextBox { x0, y0, x1, y1 };
    }

    // KiCad's stroke font is not vertically centred on the anchor: the
    // cap line sits ~0.54 × size above it while a descender (`p`, `g`,
    // `y`) drops ~0.79 × size below. `height / 2` (0.7 × size) covers
    // the ascender side but clips the descender — measured on the
    // `opamp_inverting` `inp` hierarchical label, which escaped the old
    // box by 0.11 mm.
    let descender = 0.12 * size_mm;
    let (top, bot) = (-height / 2.0, height / 2.0 + descender);
    let (lx, rx, ty, by) = match kind {
        TextKind::PlainLabel | TextKind::LeftProperty => (0.0, width, top, bot),
        TextKind::CenteredProperty => (-width / 2.0, width / 2.0, top, bot),
        // The tag's own ink starts AT the anchor (chevron / triangle)
        // and the text only begins `lead` further along the reading
        // direction — it does not straddle the anchor backwards, which
        // is what the pre-calibration model wrongly assumed.
        TextKind::TaggedLabel { lead_em } => (0.0, lead_em * size_mm + width, top, bot),
    };

    let theta = f64::from(orientation_deg).to_radians();
    let (s, c) = (theta.sin(), theta.cos());
    let corners = [(lx, ty), (rx, ty), (rx, by), (lx, by)];
    let (mut x0, mut y0, mut x1, mut y1) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for (px, py) in corners {
        let wx = anchor.0 + c * px + s * py;
        let wy = anchor.1 - s * px + c * py;
        x0 = x0.min(wx);
        y0 = y0.min(wy);
        x1 = x1.max(wx);
        y1 = y1.max(wy);
    }
    TextBox { x0, y0, x1, y1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect this module exists to fix: a global label reserves NO
    /// space behind its anchor, and enough space ahead of it to contain
    /// the lead plus the full string.
    #[test]
    fn global_label_lead_is_one_sided() {
        let b = text_bbox(
            "ni",
            (100.0, 100.0),
            DEFAULT_TEXT_SIZE_MM,
            0,
            TextKind::global_label(Some("input")),
        );
        assert!((b.x0 - 100.0).abs() < 1e-9, "reserved space behind anchor");
        let lead = 1.125 * DEFAULT_TEXT_SIZE_MM;
        let want = 100.0 + lead + crate::text_metrics::text_width("ni", DEFAULT_TEXT_SIZE_MM);
        assert!((b.x1 - want).abs() < 1e-9);
    }

    /// A passive (non-arrowed) shape gets only the box expansion.
    #[test]
    fn passive_shape_has_no_triangle_lead() {
        let TextKind::TaggedLabel { lead_em } = TextKind::global_label(Some("passive")) else {
            panic!("expected a tagged label");
        };
        assert!((lead_em - 0.375).abs() < 1e-9);
    }

    /// Descenders extend below the em box.
    #[test]
    fn descender_is_reserved() {
        let b = text_bbox(
            "pg",
            (0.0, 0.0),
            DEFAULT_TEXT_SIZE_MM,
            0,
            TextKind::LeftProperty,
        );
        assert!(b.y1 > 0.7 * DEFAULT_TEXT_SIZE_MM);
    }
}
