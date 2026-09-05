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
//! # A label's extent is its outline, not its string
//!
//! For a `(global_label …)` the string is not the drawn item: KiCad
//! wraps it in a tag polygon that is *wider than the string on every
//! side* (`SCH_GLOBALLABEL::CreateGraphicShape`, `sch_label.cpp:2146`).
//! Sizing the box to `lead + text_width` therefore under-covered the
//! drawn tag by ~24% — measured on `named_rails`' `in`, 1.017 mm of
//! tail plus 0.229 mm above and 0.381 mm below — so every pass that
//! moves a label off an obstacle was aiming a box a quarter too small,
//! and could "clear" an obstacle the rendered tag still sat on.
//! [`TextKind::GlobalLabel`] is the outline, transcribed from that
//! function. A `(hierarchical_label …)` is the opposite case: its tag
//! is a fixed template inside the string's own box, so its box stays
//! the string's — see [`TextKind::HierLabel`].
//!
//! ## Sub-micron faithfulness, and which side to err on
//!
//! KiCad computes that polygon in integer internal units (100 nm) with a
//! `KiROUND` at every step, and then adds a literal `+ 3` IU of drafting
//! slop to both half-extents. Transcribing all of it exactly would make
//! the model a ~400 nm *superset* of the drawn ink — and, because a tag's
//! half-depth is one text size and a KiCad pin is 1.27 mm long, it would
//! make EVERY global label anchored on a standard pin overlap its own
//! host's body box by 0.0004 mm. That promotes KiCad's drafting slop to a
//! Tier-1 V13 violation on a shared edge, which is not a legibility
//! defect in any sense a reader would recognise.
//!
//! So the model is computed in real arithmetic and lands ~400 nm INSIDE
//! the drawn polygon. That is the right side to err on for an
//! edge-touching predicate — a shared edge reads as "clear", not as
//! "overlapping" — and it is three orders of magnitude below the 0.05 mm
//! `rendered_text.rs` already treats as a stroke artefact, which is
//! exactly what its `INK_OVERHANG_TOL_MM` grades.
//!
//! The numbers here are calibrated against real ink measured from
//! `kicad-cli sch export svg` by
//! `crates/spice2kicad/tests/rendered_text.rs`; that suite asserts this
//! model is a tight superset of what KiCad renders, so the model cannot
//! silently drift back.

/// The direction a symbol's `Reference` / `Value` field text actually
/// reads on screen, expressed as the rotation [`text_bbox`] needs.
///
/// A field's own `(at … 0)` token is *not* what KiCad draws. The parent
/// symbol's transform is applied on top of it: `SCH_FIELD::GetDrawRotation`
/// swaps horizontal ↔ vertical whenever the symbol is rotated 90° or 270°
/// (`transform.y1 != 0`), and `SCH_FIELD::GetEffectiveHorizJustify` flips
/// left ↔ right whenever the rendered text lands on the other side of its
/// anchor — which is exactly what a 180° rotation or a Y mirror does
/// (`../kicad-source/eeschema/sch_field.cpp:396-415, 446-501`).
///
/// Net effect, measured against `kicad-cli sch export svg` for every
/// orientation the placer emits (rot 0/90/180/270 × mirror-y on/off): the
/// text advances along the symbol's own rotation, and a Y mirror reflects
/// that direction about the vertical axis — leaving vertical text (90/270)
/// untouched and reversing horizontal text (0 ↔ 180).
///
/// It lives here, beside the bbox model, because **three** places now
/// need the same answer: the emitter (which draws the field), the
/// phase-4.5 V13 model (which grades it), and — since ADR-19 M3 — the
/// placer's [`crate::text_geom`]-based property reservation
/// (`spice_layout::footprint::property_text`). A private copy in any one
/// of them is exactly the drift this module exists to prevent.
#[must_use]
pub fn field_render_rotation(orient: crate::Orientation) -> u16 {
    let rot = match orient.rotation {
        crate::Rotation::R0 => 0,
        crate::Rotation::R90 => 90,
        crate::Rotation::R180 => 180,
        crate::Rotation::R270 => 270,
    };
    if orient.mirror_y {
        (540 - rot) % 360
    } else {
        rot
    }
}

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
    /// `(global_label …)` — a string wrapped in a drawn **tag
    /// outline**.
    ///
    /// The outline, not the string, is the item's drawn extent, and it
    /// is a strict superset of the string: `CreateGraphicShape`
    /// (`../kicad-source/eeschema/sch_label.cpp:2146`) builds a box of
    /// `GetTextBox().GetWidth() + 2·margin + linewidth` along the
    /// reading direction by `±(halfSize + linewidth)` across it, then
    /// extends it by one `halfSize` end-cap per pointed end (one for
    /// `input` / `output`, two for `bidirectional` / `tri_state`, none
    /// for `passive` / unspecified). `end_caps` is how many of those
    /// caps this shape draws (0, 1 or 2); each one `halfSize` long.
    ///
    /// Modelling only the string — anchor through `lead + text_width`,
    /// which is what this variant used to do — under-covered the drawn
    /// tag by ~24% (1.017 mm of tail, 0.229 mm above and 0.381 mm below
    /// on `named_rails`' `in`), so every pass that nudges a label away
    /// from an obstacle was aiming a box a quarter too small.
    ///
    /// Use [`TextKind::global_label`] rather than writing the field by
    /// hand.
    GlobalLabel { end_caps: u8 },
    /// `(hierarchical_label …)` — a string preceded by a **fixed
    /// template polygon**.
    ///
    /// Unlike the global-label tag, `SCH_HIERLABEL::CreateGraphicShape`
    /// (`sch_label.cpp:2259`) scales a shape template by `halfSize`
    /// (`GetTextHeight() / 2`) alone, so the tag never grows with the
    /// string: it reaches at most `1.0 · size` along the reading
    /// direction and `±0.5 · size` across it. Both are inside the
    /// string's own box (which starts `1.15 · size` along — see
    /// [`TextKind::hier_label`]), so the drawn extent here *is* the
    /// string box and no outline term is needed.
    HierLabel,
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
    /// The shape decides only how many pointed **end-caps** the tag
    /// outline grows — `SCH_GLOBALLABEL::CreateGraphicShape`
    /// (`sch_label.cpp:2146`) pushes `aPoints[0]` out by `halfSize` for
    /// the shapes with an arrow head at the anchor (`input`,
    /// `bidirectional`, `tri_state`) and `aPoints[3]` out by `halfSize`
    /// for the shapes with one at the far end (`output`,
    /// `bidirectional`, `tri_state`). `passive` / unspecified is a plain
    /// rectangle.
    #[must_use]
    pub fn global_label(shape: Option<&str>) -> Self {
        let end_caps = match shape {
            Some("bidirectional" | "tri_state") => 2,
            Some("input" | "output") => 1,
            _ => 0,
        };
        TextKind::GlobalLabel { end_caps }
    }

    /// A `(hierarchical_label …)`.
    ///
    /// `SCH_HIERLABEL::GetSchematicTextOffset` (`sch_label.cpp:2336`)
    /// offsets by `GetTextOffset()` — `DEFAULT_TEXT_OFFSET_RATIO` (0.15)
    /// × height — plus one full `GetTextWidth()` (the square glyph
    /// cell), regardless of shape.
    #[must_use]
    pub fn hier_label() -> Self {
        TextKind::HierLabel
    }
}

/// KiCad's default schematic text size (mm).
pub const DEFAULT_TEXT_SIZE_MM: f64 = 1.27;

/// Below this, an overlap between two boxes from this model is IEEE-754
/// noise, not geometry (mm).
///
/// Schematic geometry lands on a 1.27 mm grid, and the boxes here are
/// built from sums and products of grid coordinates and font ratios — so
/// two boxes that *share an edge* routinely differ by a few units in the
/// last place rather than comparing equal. That is not hypothetical: a
/// `(global_label …)` tag is exactly one text size deep and a KiCad pin is
/// exactly 1.27 mm long, so a label anchored on a pin abuts its host's
/// body box on every fixture, and `49.529999999999994 < 49.53` made the
/// V13 body-overlap verifier report a 6 × 10⁻¹⁵ mm "overlap".
///
/// One nanometre: eight orders of magnitude above the noise it absorbs
/// and three below the 0.001 mm anything in this project calls a defect,
/// so it cannot mask one. Both the emitter's candidate scorer and the V13
/// verifiers read it, because a predicate they disagree on is a predicate
/// the emitter optimises and the verifier then fails.
pub const TOUCH_EPS_MM: f64 = 1.0e-6;

/// `DEFAULT_LABEL_SIZE_RATIO` (`eeschema/default_values.h:75`), the
/// ratio `SCH_LABEL_BASE::GetLabelBoxExpansion` scales the text height
/// by to get a label's box margin.
const LABEL_SIZE_RATIO: f64 = 0.375;

/// `CreateGraphicShape`'s `halfSize` — `GetTextHeight() / 2 + margin` —
/// in multiples of the text size. Also the length of each pointed
/// end-cap the arrow-headed shapes add.
const TAG_HALF_SIZE_EM: f64 = 0.5 + LABEL_SIZE_RATIO;

/// `SCH_HIERLABEL::GetSchematicTextOffset`'s along-reading push of the
/// string: `DEFAULT_TEXT_OFFSET_RATIO` (0.15) × height plus one full
/// `GetTextWidth()` (the square template cell).
const HIER_LEAD_EM: f64 = 1.15;

/// Newstroke's inter-character gap, in em. `STROKE_FONT::GetTextAsGlyphs`
/// (`common/font/stroke_font.cpp:305`) closes the run's bounding box one
/// `INTER_CHAR` short of the final cursor, so a string's *box* is one gap
/// narrower than its summed advance.
const INTER_CHAR_EM: f64 = 0.2;

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
        // The template polygon is inside the string's own box, so the
        // drawn extent is the string: it starts `lead` along the reading
        // direction from the anchor and does not straddle the anchor
        // backwards.
        TextKind::HierLabel => (0.0, HIER_LEAD_EM * size_mm + width, top, bot),
        // The tag OUTLINE is the drawn extent (it contains the string),
        // so the box is `CreateGraphicShape`'s polygon, transcribed:
        //
        //   margin    = GetLabelBoxExpansion()      = 0.375·size
        //   halfSize  = GetTextHeight()/2 + margin  = 0.875·size
        //   linewidth = GetPenWidth()               = size/8
        //   symb_len  = GetTextBox().GetWidth() + 2·margin
        //   x = symb_len + linewidth + 3 IU     (+ halfSize per end-cap)
        //   y = halfSize + linewidth + 3 IU
        //
        // and `GetTextBox().GetWidth()` is `FONT::StringBoundaryLimits`:
        // the stroke run's own box — the summed advance less one
        // `INTER_CHAR` gap — inflated by `1.5·thickness` on each side.
        //
        // Arithmetic is in f64 mm where KiCad's is in `KiROUND`ed
        // integer IU, so the result can differ from the drawn polygon by
        // ~1 µm; `rendered_text.rs` grades that against real ink.
        TextKind::GlobalLabel { end_caps } => {
            let margin = LABEL_SIZE_RATIO * size_mm;
            let half_size = TAG_HALF_SIZE_EM * size_mm;
            let linewidth = size_mm / 8.0;
            // `FONT::StringBoundaryLimits`: the stroke run's own box —
            // the summed advance less one `INTER_CHAR` gap — inflated by
            // `1.5 · thickness` on each side.
            let text_box_w = width - INTER_CHAR_EM * size_mm + 3.0 * linewidth;
            let symb_len = text_box_w + 2.0 * margin;
            (
                0.0,
                symb_len + linewidth + f64::from(end_caps) * half_size,
                -(half_size + linewidth),
                half_size + linewidth,
            )
        }
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
    /// the whole tag outline.
    #[test]
    fn global_label_tag_is_one_sided() {
        let b = text_bbox(
            "ni",
            (100.0, 100.0),
            DEFAULT_TEXT_SIZE_MM,
            0,
            TextKind::global_label(Some("input")),
        );
        assert!((b.x0 - 100.0).abs() < 1e-9, "reserved space behind anchor");
        // symb_len + linewidth + one end-cap = width + 1.925 em, to
        // within the integer-IU rounding the exact model carries.
        let want = 100.0
            + crate::text_metrics::text_width("ni", DEFAULT_TEXT_SIZE_MM)
            + 1.925 * DEFAULT_TEXT_SIZE_MM;
        assert!((b.x1 - want).abs() < 1e-3, "{} != {want}", b.x1);
    }

    /// The tag is the drawn extent, so it must strictly contain the
    /// string KiCad prints inside it — anchor + lead through
    /// anchor + lead + width, and half a text height either side.
    #[test]
    fn global_label_tag_contains_its_string() {
        for shape in ["input", "output", "bidirectional", "tri_state", "passive"] {
            let b = text_bbox(
                "descender_pgy",
                (0.0, 0.0),
                DEFAULT_TEXT_SIZE_MM,
                0,
                TextKind::global_label(Some(shape)),
            );
            let arrowed = matches!(shape, "input" | "bidirectional" | "tri_state");
            let lead = (0.375 + if arrowed { 0.75 } else { 0.0 }) * DEFAULT_TEXT_SIZE_MM;
            let string_end =
                lead + crate::text_metrics::text_width("descender_pgy", DEFAULT_TEXT_SIZE_MM);
            assert!(b.x1 >= string_end, "{shape}: tag {} < string end", b.x1);
            assert!(b.y1 >= 0.7 * DEFAULT_TEXT_SIZE_MM, "{shape}: descender");
            assert!(b.y0 <= -0.7 * DEFAULT_TEXT_SIZE_MM, "{shape}: ascender");
        }
    }

    /// A passive (non-arrowed) shape gets no pointed end-cap.
    #[test]
    fn passive_shape_has_no_end_caps() {
        let TextKind::GlobalLabel { end_caps } = TextKind::global_label(Some("passive")) else {
            panic!("expected a global label");
        };
        assert_eq!(end_caps, 0);
        let TextKind::GlobalLabel { end_caps } = TextKind::global_label(Some("bidirectional"))
        else {
            panic!("expected a global label");
        };
        assert_eq!(end_caps, 2);
    }

    /// A hierarchical label's template tag is inside its string box, so
    /// the model stays the string — never the wider global-label tag.
    #[test]
    fn hier_label_box_is_the_string() {
        let b = text_bbox(
            "clk",
            (0.0, 0.0),
            DEFAULT_TEXT_SIZE_MM,
            0,
            TextKind::hier_label(),
        );
        let want = HIER_LEAD_EM * DEFAULT_TEXT_SIZE_MM
            + crate::text_metrics::text_width("clk", DEFAULT_TEXT_SIZE_MM);
        assert!((b.x1 - want).abs() < 1e-9);
        // …and it covers the template polygon (≤ 1.0 em long, ±0.5 em deep).
        assert!(b.x1 >= DEFAULT_TEXT_SIZE_MM);
        assert!(b.y1 >= 0.5 * DEFAULT_TEXT_SIZE_MM);
        assert!(b.y0 <= -0.5 * DEFAULT_TEXT_SIZE_MM);
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
