//! Byte-level span correctness tests.
//!
//! Diagnostics depend on `Span` byte ranges pointing at the right
//! source bytes. Each test builds a literal source, parses (or
//! scans) it, and asserts that slicing the source by the span
//! yields the expected substring.
//!
//! Findings (see also the test docstrings):
//! * Trailing-tag spans cover the full `;@…body` including the
//!   `;@` marker bytes — not just the body harvested into
//!   `RawTag.body`.
//! * Block-annotation spans cover the full physical `*@…` line.

mod common;

use common::fid;
use spice_diagnostics::Span;
use spice_parser::ast::{Annotation, Tag};
use spice_parser::lexer::scan;
use spice_parser::parse;

fn slice(src: &str, span: Span) -> &str {
    &src[span.start..span.end]
}

fn first_element_tag_span(nl: &spice_parser::Netlist, refdes: &str, idx: usize) -> Span {
    let e = nl
        .elements
        .iter()
        .find(|e| e.designator.eq_ignore_ascii_case(refdes))
        .expect("element present");
    e.tags[idx].span.expect("parser sets span")
}

// ---------------------------------------------------------------------------
// A. Trailing-tag spans
// ---------------------------------------------------------------------------

/// Canonical regression-anchor: the exact bytes the tag span covers
/// today. All other span tests assert via `contains` to be robust to
/// future tightening; this one stays exact so that any change is
/// noticed.
#[test]
fn tag_span_simple() {
    let src = "* t\nR1 a b 1k ;@ symbol=Device:R\n";
    let nl = parse(src, fid()).expect("parses").netlist;
    let span = first_element_tag_span(&nl, "R1", 0);
    assert_eq!(slice(src, span), " symbol=Device:R");
}

/// Without the space after `;@` the span still covers the directive body.
#[test]
fn tag_span_no_space_marker() {
    let src = "* t\nR1 a b 1k ;@symbol=Device:R\n";
    let nl = parse(src, fid()).expect("parses").netlist;
    let span = first_element_tag_span(&nl, "R1", 0);
    let s = slice(src, span);
    assert!(
        s.contains("symbol=Device:R"),
        "span {s:?} should contain {:?}",
        "symbol=Device:R"
    );
}

/// Two `;@…` tags on one line each get their own span covering its body.
#[test]
fn tag_span_multiple_on_one_line() {
    let src = "* t\nR1 a b 1k ;@ symbol=Device:R ;@ place=right-of V1\n";
    let nl = parse(src, fid()).expect("parses").netlist;
    let s0 = slice(src, first_element_tag_span(&nl, "R1", 0));
    let s1 = slice(src, first_element_tag_span(&nl, "R1", 1));
    assert!(
        s0.contains("symbol=Device:R"),
        "span {s0:?} should contain symbol=Device:R"
    );
    assert!(
        s1.contains("place=right-of V1"),
        "span {s1:?} should contain place=right-of V1"
    );
}

/// Tag on a `+`-continuation line: span byte offsets land on the
/// continuation physical line.
#[test]
fn tag_span_on_continuation() {
    let src = "* t\nR1 a b 1k\n+ tc=0.001 ;@ ignore\n";
    let nl = parse(src, fid()).expect("parses").netlist;
    let span = first_element_tag_span(&nl, "R1", 0);
    let s = slice(src, span);
    assert!(s.contains("ignore"), "span {s:?} should contain ignore");
    let cont_start = src.find("+ ").unwrap();
    assert!(span.start > cont_start);
}

/// Standalone `;@…` line attaches to the previous element; span
/// covers the standalone line's body.
#[test]
fn tag_span_standalone_line() {
    let src = "* t\nR1 a b 1k\n  ;@ place=right-of V1\n";
    let nl = parse(src, fid()).expect("parses").netlist;
    let span = first_element_tag_span(&nl, "R1", 0);
    let s = slice(src, span);
    assert!(
        s.contains("place=right-of V1"),
        "span {s:?} should contain place=right-of V1"
    );
}

/// Multi-line continuation: tag on the `+ W=10u ;@ ignore` line must
/// have offsets on that continuation line, not on the M1 line.
#[test]
fn tag_span_after_continuation_offsets() {
    let src = "* t\nM1 d g s b NMOS L=1u\n+ W=10u ;@ ignore\n";
    let nl = parse(src, fid()).expect("parses").netlist;
    let span = first_element_tag_span(&nl, "M1", 0);
    let s = slice(src, span);
    assert!(s.contains("ignore"), "span {s:?} should contain ignore");
    let m1_line_end = src.find("L=1u\n").unwrap() + "L=1u".len();
    assert!(span.start > m1_line_end, "tag must live on continuation");
    // Verify the Tag itself parsed as Ignore.
    let tag = &nl.elements[0].tags[0].tag;
    assert!(matches!(tag, Tag::Ignore));
}

// ---------------------------------------------------------------------------
// B. Block-annotation spans
// ---------------------------------------------------------------------------

/// Top-level `*@…` line: span is the full physical line.
#[test]
fn block_annotation_span() {
    let src = "* t\n*@symbol Device:R for=R*\nR1 a b 1\n";
    let nl = parse(src, fid()).expect("parses").netlist;
    let ann = &nl.annotations[0];
    let span = ann.span.expect("parser sets span");
    let s = slice(src, span);
    assert!(
        s.contains("symbol Device:R for=R*"),
        "span {s:?} should contain symbol Device:R for=R*"
    );
    assert!(matches!(&ann.annotation, Annotation::SymbolDefault { .. }));
}

/// Block annotation inside a subckt: span sits on its own line, not
/// on the `.subckt` header.
#[test]
fn block_annotation_span_inside_subckt() {
    let src = "* t\n.subckt amp in out\n*@symbol Device:R for=R*\nR1 in out 1\n.ends\n";
    let nl = parse(src, fid()).expect("parses").netlist;
    let sub = &nl.subckts[0];
    let span = sub.annotations[0].span.expect("set");
    let s = slice(src, span);
    assert!(
        s.contains("symbol Device:R for=R*"),
        "span {s:?} should contain symbol Device:R for=R*"
    );
    // Must not begin with `.subckt`.
    assert!(!s.starts_with(".subckt"));
}

// ---------------------------------------------------------------------------
// C. Cross-line offset correctness
// ---------------------------------------------------------------------------

/// Blank lines before the element shift offsets; spans must follow.
#[test]
fn spans_remain_valid_after_blank_lines() {
    let src = "* t\n\n\n\nR1 a b 1k ;@ ignore\n";
    let nl = parse(src, fid()).expect("parses").netlist;
    let span = first_element_tag_span(&nl, "R1", 0);
    let s = slice(src, span);
    assert!(s.contains("ignore"), "span {s:?} should contain ignore");
}

/// CRLF input: lexer strips the `\r` for line text but byte offsets
/// in the source still count the `\r`. Slicing the original source
/// with the span must yield the expected substring.
#[test]
fn spans_remain_valid_after_crlf() {
    let src = "* t\r\nR1 a b 1k ;@ ignore\r\n";
    let nl = parse(src, fid()).expect("parses").netlist;
    let span = first_element_tag_span(&nl, "R1", 0);
    let s = slice(src, span);
    assert!(s.contains("ignore"), "span {s:?} should contain ignore");
}

// ---------------------------------------------------------------------------
// E. Pinmap entry within tag span
// ---------------------------------------------------------------------------

/// Pinmap tag's outer span covers `;@ pinmap=1:A,2:K`. Per-entry
/// sub-spans are not modelled today.
#[test]
fn pinmap_tag_span_full() {
    let src = "* t\nD1 a k DMOD ;@ pinmap=1:A,2:K\n";
    let nl = parse(src, fid()).expect("parses").netlist;
    let span = first_element_tag_span(&nl, "D1", 0);
    let s = slice(src, span);
    assert!(
        s.contains("pinmap=1:A,2:K"),
        "span {s:?} should contain pinmap=1:A,2:K"
    );
    assert!(matches!(&nl.elements[0].tags[0].tag, Tag::Pinmap(_)));
}

// ---------------------------------------------------------------------------
// F. Block-annotation word-level spans (lexer-level)
// ---------------------------------------------------------------------------

/// Each tokenised `Word` in a `*@` line has a span pointing at the
/// exact bytes of that token.
#[test]
fn block_annotation_word_spans_align() {
    let src = "* t\n*@symbol Device:R for=R*\n";
    let s = scan(src, fid());
    let line = &s.lines[0];
    let words: Vec<&str> = line.words.iter().map(|w| w.text.as_str()).collect();
    assert_eq!(words, ["symbol", "Device:R", "for", "=", "R*"]);
    assert_eq!(slice(src, line.words[0].span), "symbol");
    assert_eq!(slice(src, line.words[1].span), "Device:R");
    assert_eq!(slice(src, line.words[2].span), "for");
    assert_eq!(slice(src, line.words[3].span), "=");
    assert_eq!(slice(src, line.words[4].span), "R*");
}

/// `=`, `(`, and `)` each get a 1-byte span at their exact source
/// position.
#[test]
fn equals_paren_word_spans() {
    let src = "* t\n.model M NPN (BF=200 IS=1e-15)\n";
    let s = scan(src, fid());
    let line = &s.lines[0];
    let by_text = |t: &str| -> Vec<Span> {
        line.words
            .iter()
            .filter(|w| w.text == t)
            .map(|w| w.span)
            .collect()
    };
    for sp in by_text("=") {
        assert_eq!(sp.end - sp.start, 1);
        assert_eq!(slice(src, sp), "=");
    }
    let lps = by_text("(");
    assert_eq!(lps.len(), 1);
    assert_eq!(slice(src, lps[0]), "(");
    assert_eq!(lps[0].end - lps[0].start, 1);
    let rps = by_text(")");
    assert_eq!(rps.len(), 1);
    assert_eq!(slice(src, rps[0]), ")");
    assert_eq!(rps[0].end - rps[0].start, 1);
}
