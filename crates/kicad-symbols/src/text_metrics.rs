//! Newstroke (KiCad's built-in stroke font) horizontal text metrics.
//!
//! Every text bbox this workspace computes — property fields, plain and
//! global labels, power-glyph net names — needs to know how wide a string
//! renders. A fixed per-character estimate cannot do that: Newstroke is
//! *proportional*, and its glyphs vary by more than 3× (`'i'` is 10/21 em,
//! `'m'` is 28/21). A uniform average is simultaneously too wide for
//! narrow lowercase strings and far too narrow for uppercase ones — an
//! 8-character `QGENERIC` under-measured by ~25%, which is how genuine
//! text collisions stayed invisible to the V13 verifiers.
//!
//! The table below is the real advance of each printable ASCII glyph,
//! extracted from `../kicad-source/common/newstroke_font.cpp`. KiCad
//! encodes a glyph's advance in the first two bytes of its stroke string
//! as `(byte[1] - 'R') - (byte[0] - 'R')`, scaled by `STROKE_FONT_SCALE`
//! (`1/21`) — see `common/font/stroke_font.cpp:45,144-147`. Values here
//! are kept in those integer 21ths so the table is exact.
//!
//! Advance includes each glyph's side bearings, so a string's summed
//! advance is very slightly wider than its rendered ink (~0.4 mm at the
//! 1.27 mm default size) — exactly the direction a collision model wants.

/// Advance of each printable ASCII glyph (0x20..=0x7E), in 21ths of the
/// text size. Index is `ch as usize - 0x20`.
const ADVANCE_21THS: [u8; 95] = [
    16, 10, 16, 21, 20, 24, 26, 10, 14, 14, 16, 26, 10, 26, 10, 22, 20, 20, 20, 20, 20, 20, 20, 20,
    20, 20, 10, 10, 26, 26, 26, 18, 27, 18, 21, 21, 21, 19, 18, 21, 22, 10, 16, 21, 17, 24, 22, 22,
    21, 22, 21, 20, 16, 22, 18, 24, 20, 18, 20, 14, 14, 14, 12, 16, 8, 19, 19, 18, 19, 18, 12, 19,
    19, 10, 10, 17, 11, 28, 19, 19, 19, 19, 13, 17, 12, 19, 16, 22, 17, 16, 17, 14, 20, 14, 15,
];

/// Advance of a single character, in multiples of the text size.
/// Non-ASCII and unprintable characters fall back to `'?'`, matching
/// `STROKE_FONT::GetTextAsGlyphs`'s own substitution.
fn advance_em(ch: char) -> f64 {
    let idx = match u32::from(ch) {
        c @ 0x20..=0x7E => (c - 0x20) as usize,
        _ => (u32::from('?') - 0x20) as usize,
    };
    f64::from(ADVANCE_21THS[idx]) / 21.0
}

/// Rendered width of `text` at the given text size, in the same units as
/// `size` (mm for schematic geometry).
#[must_use]
pub fn text_width(text: &str, size: f64) -> f64 {
    text.chars().map(advance_em).sum::<f64>() * size
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spot-checks against widths measured from `kicad-cli sch export svg`
    /// renderings. Summed advance should sit just above the rendered ink
    /// (the trailing side bearing), never below it.
    #[test]
    fn matches_rendered_ink() {
        // (text, measured ink width at size 1.27 mm)
        for (text, ink) in [
            ("QGENERIC", 8.89),
            ("RTAIL", 4.54),
            ("out", 2.66),
            ("2.2k", 3.63),
            ("tail", 2.84),
            ("GND", 3.38),
        ] {
            let w = text_width(text, 1.27);
            assert!(w >= ink, "{text}: modelled {w:.2} < rendered ink {ink:.2}");
            assert!(
                w <= ink + 0.75,
                "{text}: modelled {w:.2} unreasonably wider than ink {ink:.2}"
            );
        }
    }

    #[test]
    fn proportional_not_uniform() {
        assert!(text_width("iiii", 1.0) < text_width("MMMM", 1.0) / 2.0);
    }

    #[test]
    fn unprintable_falls_back_to_question_mark() {
        assert!((text_width("\u{1F600}", 1.0) - text_width("?", 1.0)).abs() < 1e-9);
    }
}
