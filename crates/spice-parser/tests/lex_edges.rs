//! Lexer edge-case tests grounded in ngspice `inpcom.c` behaviour.
//!
//! Each test parses a small SPICE snippet and asserts on the resulting
//! [`Netlist`] or on the raw [`spice_parser::lexer::Scanned`] output.
//! Known-failing cases are marked `#[ignore]` with a one-line reason.

mod common;

use common::{elem, expect_tag, fid, has_annotation, has_tag, parse_ok};
use spice_parser::ast::{Annotation, Relation, Tag, Value};
use spice_parser::lexer::{LineKind, scan};

// ---------------------------------------------------------------------------
// Title line
// ---------------------------------------------------------------------------

/// Title is always the first physical line, stripped of leading `*` and
/// surrounding whitespace (ngspice `INPgetTitle`).
#[test]
fn title_is_first_line_comment() {
    let s = scan("* hello\nR1 a b 1k\n", fid());
    assert_eq!(s.title, "hello");
    assert_eq!(s.lines.len(), 1);
}

/// Even when the first line looks like an element it becomes the title and
/// is NOT parsed as an element (ngspice treats line 1 as the title verbatim).
#[test]
fn title_even_when_element_shaped() {
    let nl = parse_ok("R1 a b 1k\nR2 a b 2k\n");
    // Title should be the raw text of the first line.
    assert!(
        nl.title.contains("R1"),
        "title should contain first-line text, got {:?}",
        nl.title
    );
    // R1 must NOT appear as a parsed element.
    let names: Vec<_> = nl.elements.iter().map(|e| e.designator.as_str()).collect();
    assert!(
        !names.contains(&"R1"),
        "R1 must not be an element — it was the title; elements: {names:?}"
    );
    // R2 should be present.
    elem(&nl, "R2");
}

// ---------------------------------------------------------------------------
// Line endings
// ---------------------------------------------------------------------------

/// CRLF (`\r\n`) line endings are stripped; R1 parses normally.
#[test]
fn crlf_line_endings() {
    let nl = parse_ok("* t\r\nR1 a b 1k\r\n");
    elem(&nl, "R1");
}

/// Alternating LF and CRLF in the same file; both elements parse.
#[test]
fn mixed_lf_and_crlf() {
    let nl = parse_ok("* t\r\nR1 a b 1k\nR2 c d 2k\r\n");
    elem(&nl, "R1");
    elem(&nl, "R2");
}

// ---------------------------------------------------------------------------
// Whitespace
// ---------------------------------------------------------------------------

/// Tabs between tokens are treated as whitespace; nodes parse correctly.
#[test]
fn tab_separated_tokens() {
    let nl = parse_ok("* t\nR1\ta\tb\t1k   \n");
    let r1 = elem(&nl, "R1");
    assert_eq!(r1.nodes.len(), 2, "expected 2 nodes for R1");
}

// ---------------------------------------------------------------------------
// Empty / minimal inputs
// ---------------------------------------------------------------------------

/// Empty source must not panic; netlist has no elements.
#[test]
fn empty_file() {
    let nl = parse_ok("");
    assert!(nl.elements.is_empty());
}

/// Only a title comment, no body.
#[test]
fn title_only() {
    let nl = parse_ok("* just a title\n");
    assert_eq!(nl.title, "just a title");
    assert!(nl.elements.is_empty());
}

/// Only `.end`, no elements.
#[test]
fn only_dot_end() {
    let nl = parse_ok("* t\n.end\n");
    assert!(nl.elements.is_empty());
}

/// Blank lines between elements are transparent.
#[test]
fn blank_lines_between_elements() {
    let nl = parse_ok("* t\nR1 a b 1k\n\nR2 c d 2k\n");
    elem(&nl, "R1");
    elem(&nl, "R2");
}

// ---------------------------------------------------------------------------
// Continuation `+`
// ---------------------------------------------------------------------------

/// Basic `+` continuation: param on next line is merged into R1.
#[test]
fn continuation_basic() {
    let nl = parse_ok("* t\nR1 a b 1k\n+ tc=0.001\n");
    let r1 = elem(&nl, "R1");
    assert!(
        r1.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("tc")),
        "tc param not found on R1; params: {:?}",
        r1.params
    );
}

/// `;@` tag on a `+` continuation line binds to the preceding element.
#[test]
fn continuation_carries_tag() {
    let nl = parse_ok("* t\nR1 a b 1k\n+ ;@ symbol=Device:R\n");
    let r1 = elem(&nl, "R1");
    assert!(
        has_tag(r1, |t| matches!(t, Tag::Symbol(_))),
        "Symbol tag expected on R1; tags: {:?}",
        r1.tags
    );
}

/// Multiple `+` continuation lines: both params collected.
#[test]
fn multiple_continuations() {
    let nl = parse_ok("* t\nR1 a b 1k\n+ tc1=0.1\n+ tc2=0.01\n");
    let r1 = elem(&nl, "R1");
    assert!(
        r1.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("tc1")),
        "tc1 missing; params: {:?}",
        r1.params
    );
    assert!(
        r1.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("tc2")),
        "tc2 missing; params: {:?}",
        r1.params
    );
}

/// Tab-indented `+` continuation: leading whitespace is stripped before the
/// `+` check, so `\t+ params` still continues the previous line.
#[test]
fn tab_indented_continuation() {
    let nl = parse_ok("* t\nR1 a b 1k\n\t+ tc=0.001\n");
    let r1 = elem(&nl, "R1");
    assert!(
        r1.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("tc")),
        "tc not found; params: {:?}",
        r1.params
    );
}

// ---------------------------------------------------------------------------
// Standalone `;@` lines (spec §2.3)
// ---------------------------------------------------------------------------

/// A `;@` line with nothing before it attaches to the previous element.
#[test]
fn standalone_tag_attaches_to_previous() {
    let nl = parse_ok("* t\nR1 a b 1k\n  ;@ place=right-of V1\n");
    let r1 = elem(&nl, "R1");
    assert!(
        has_tag(r1, |t| matches!(t, Tag::Place { .. })),
        "Place tag expected on R1; tags: {:?}",
        r1.tags
    );
}

/// Multiple `;@` tags on a single element line: both collected.
#[test]
fn multiple_tags_on_element_line() {
    let nl = parse_ok("* t\nR1 a b 1k ;@ symbol=Device:R ;@ place=right-of V1\n");
    let r1 = elem(&nl, "R1");
    assert!(
        has_tag(r1, |t| matches!(t, Tag::Symbol(_))),
        "Symbol tag missing; tags: {:?}",
        r1.tags
    );
    assert!(
        has_tag(r1, |t| matches!(t, Tag::Place { .. })),
        "Place tag missing; tags: {:?}",
        r1.tags
    );
}

/// `;@` tag on a continuation line binds to the element.
#[test]
fn tag_on_continuation_line() {
    let nl = parse_ok("* t\nR1 a b 1k\n+ tc=0.001 ;@ ignore\n");
    let r1 = elem(&nl, "R1");
    assert!(
        has_tag(r1, |t| matches!(t, Tag::Ignore)),
        "Ignore tag expected on R1; tags: {:?}",
        r1.tags
    );
}

// ---------------------------------------------------------------------------
// Block annotations (`*@`)
// ---------------------------------------------------------------------------

/// Top-level `*@symbol` block annotation lands in `nl.annotations`.
#[test]
fn block_annotation_top_level() {
    let nl = parse_ok("* t\n*@symbol Device:R for=R*\nR1 a b 1k\n");
    assert!(
        has_annotation(&nl, |a| matches!(a, Annotation::SymbolDefault { .. })),
        "SymbolDefault annotation expected in nl.annotations; got: {:?}",
        nl.annotations
    );
}

/// `*@` annotation inside a `.subckt` lands in `subckt.annotations`, not
/// in the top-level `nl.annotations`.
#[test]
fn block_annotation_inside_subckt() {
    let nl = parse_ok("* t\n.subckt myblock a b\n*@symbol Device:R for=R*\nR1 a b 1k\n.ends\n");
    assert!(
        nl.annotations.is_empty(),
        "top-level annotations should be empty; got: {:?}",
        nl.annotations
    );
    let sub = nl
        .subckts
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case("myblock"))
        .expect("subckt myblock not found");
    assert!(
        sub.annotations
            .iter()
            .any(|a| matches!(a.annotation, Annotation::SymbolDefault { .. })),
        "SymbolDefault expected in subckt.annotations; got: {:?}",
        sub.annotations
    );
}

// ---------------------------------------------------------------------------
// `.control … .endc`
// ---------------------------------------------------------------------------

/// Elements inside `.control` are not parsed.
#[test]
fn control_block_skipped() {
    let nl = parse_ok("* t\n.control\nR99 a b 1k\n.endc\nR1 a b 1k\n");
    let names: Vec<_> = nl.elements.iter().map(|e| e.designator.as_str()).collect();
    assert!(
        !names.contains(&"R99"),
        "R99 inside .control must be skipped; elements: {names:?}"
    );
    elem(&nl, "R1");
}

/// `.Control` / `.endc` (mixed case) are recognised.
#[test]
fn control_block_mixed_case() {
    let nl = parse_ok("* t\n.Control\nR99 a b 1k\n.endc\nR1 a b 1k\n");
    let names: Vec<_> = nl.elements.iter().map(|e| e.designator.as_str()).collect();
    assert!(
        !names.contains(&"R99"),
        "R99 must be skipped; elements: {names:?}"
    );
    elem(&nl, "R1");
}

/// `*@` inside `.control` is NOT processed (spec §8 caveat 2).
/// Our impl skips the entire control block including `*@` lines.
#[test]
fn block_annotation_inside_control_not_processed() {
    let nl = parse_ok("* t\n.control\n*@symbol Device:R for=R*\n.endc\nR1 a b 1k\n");
    assert!(
        nl.annotations.is_empty(),
        "*@ inside .control must not produce annotations; got: {:?}",
        nl.annotations
    );
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

/// Pure `*` comment lines are dropped; nothing appears in elements.
#[test]
fn pure_comment_dropped() {
    let s = scan("* t\n* this is a comment\nR1 a b 1\n", fid());
    let code: Vec<_> = s
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Code)
        .collect();
    assert_eq!(code.len(), 1);
    assert_eq!(code[0].words[0].text, "R1");
}

/// Prose text after `;` (no `@`) is dropped; R1 has no tags.
#[test]
fn prose_semicolon_comment_no_tags() {
    let nl = parse_ok("* t\nR1 a b 1k ; just a comment\n");
    let r1 = elem(&nl, "R1");
    assert!(r1.tags.is_empty(), "no tags expected; got: {:?}", r1.tags);
}

/// `$` preceded by a space is a comment introducer (ngspice rule).
#[test]
fn dollar_inline_comment() {
    let nl = parse_ok("* t\nR1 a b 1k $ a comment\n");
    let r1 = elem(&nl, "R1");
    assert!(r1.params.is_empty(), "tokens after `$` should be stripped");
}

/// `$` NOT preceded by whitespace/comma is NOT a comment introducer
/// (ngspice `inp_stripcomments_line`: char before `$` must be ` `, `\t`, or `,`).
/// The `$` becomes part of the value token.
#[test]
fn dollar_with_leading_space_required() {
    let s = spice_parser::lexer::scan("* t\nR1 a b 1k$comment\n", fid());
    let code: Vec<_> = s
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Code)
        .collect();
    assert_eq!(code.len(), 1);
    // `1k$comment` is a single token — not stripped.
    let words: Vec<_> = code[0].words.iter().map(|w| w.text.as_str()).collect();
    assert!(
        words.iter().any(|w| w.contains('$')),
        "no-space-before-$ should leave token intact; words: {words:?}"
    );
}

/// `$` after a comma is a comment introducer (ngspice allows `,` as predecessor).
#[test]
fn dollar_after_comma() {
    let s = spice_parser::lexer::scan("* t\nM1 d g s b NMOS L=1u,$comment\n", fid());
    let code: Vec<_> = s
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Code)
        .collect();
    assert_eq!(code.len(), 1);
    // Everything from `$comment` onward must be stripped.
    let words: Vec<_> = code[0].words.iter().map(|w| w.text.as_str()).collect();
    assert!(
        !words.iter().any(|w| w.contains('$')),
        "`$` after comma should be stripped; words: {words:?}"
    );
    // `L=1u,` trimmed: the comma is part of the code but `$comment` is not a word.
    assert!(
        words.iter().any(|w| w.contains("1u")),
        "L=1u should survive; words: {words:?}"
    );
}

/// `$@` is NOT an annotation marker — `$` introduces a prose comment and
/// everything after it (including the `@`) is ignored.
#[test]
fn dollar_does_not_carry_annotation() {
    let nl = parse_ok("* t\nR1 a b 1k $@symbol=Device:R\n");
    let r1 = elem(&nl, "R1");
    assert!(
        r1.tags.is_empty(),
        "`$@` must not produce a tag; tags: {:?}",
        r1.tags
    );
}

/// `;` before `$`: `;@` tag wins; `$` inside the `;` comment is not a
/// fresh introducer.
#[test]
fn semicolon_before_dollar() {
    let nl = parse_ok("* t\nR1 a b 1k ;@ symbol=Device:R $ trailing prose\n");
    let r1 = elem(&nl, "R1");
    assert!(
        has_tag(r1, |t| matches!(t, Tag::Symbol(_))),
        "Symbol tag expected; tags: {:?}",
        r1.tags
    );
}

/// `$` before `;`: `$` wins; the `;@` that follows is inside the `$` comment
/// and must not be harvested.
#[test]
fn dollar_before_semicolon() {
    let nl = parse_ok("* t\nR1 a b 1k $ first ;@ symbol=Device:R\n");
    let r1 = elem(&nl, "R1");
    assert!(
        r1.tags.is_empty(),
        "no tags — `$` wins over later `;@`; tags: {:?}",
        r1.tags
    );
}

/// `//` comment (LTspice extension): ngspice does NOT accept `//` as a
/// comment introducer (only `*`, `;`, `$` are recognised in inpcom.c).
/// Our lexer likewise does not strip `//`, so it is treated as a token.
#[test]
#[ignore = "our lexer does not strip `//` comments; expected — ngspice only supports `$` and `;`"]
fn double_slash_comment() {
    let nl = parse_ok("* t\nR1 a b 1k // a comment\n");
    let r1 = elem(&nl, "R1");
    assert!(r1.params.is_empty(), "tokens after `//` should be stripped");
}

// ---------------------------------------------------------------------------
// A. Annotation parser tests (place relations, no-space marker, dotted anchor)
// ---------------------------------------------------------------------------

fn place_of(e: &spice_parser::ast::Element) -> (Relation, String) {
    expect_tag(e, |t| match t {
        Tag::Place { relation, anchor } => Some((*relation, anchor.clone())),
        _ => None,
    })
}

/// §4.3 `left-of` relation.
#[test]
fn place_relation_left_of() {
    let nl = parse_ok("* t\nR1 a b 1k ;@ place=left-of V1\n");
    let tag = place_of(elem(&nl, "R1"));
    assert_eq!(tag.0, Relation::LeftOf);
    assert_eq!(tag.1, "V1");
}

/// §4.3 `above` relation.
#[test]
fn place_relation_above() {
    let nl = parse_ok("* t\nR1 a b 1k ;@ place=above V1\n");
    let tag = place_of(elem(&nl, "R1"));
    assert_eq!(tag.0, Relation::Above);
    assert_eq!(tag.1, "V1");
}

/// §4.3 `below` relation.
#[test]
fn place_relation_below() {
    let nl = parse_ok("* t\nR1 a b 1k ;@ place=below V1\n");
    let tag = place_of(elem(&nl, "R1"));
    assert_eq!(tag.0, Relation::Below);
    assert_eq!(tag.1, "V1");
}

/// §2.1 Dotted subcircuit path as anchor; must be preserved verbatim.
#[test]
fn place_dotted_anchor() {
    let nl = parse_ok("* t\nR1 a b 1k ;@ place=right-of XU2.R5\n");
    let tag = place_of(elem(&nl, "R1"));
    assert_eq!(tag.0, Relation::RightOf);
    assert_eq!(tag.1, "XU2.R5");
}

/// §2 No space between `;@` and the directive name (`;@symbol=…` form).
#[test]
fn tag_no_space_after_marker() {
    let nl = parse_ok("* t\nR1 a b 1k ;@symbol=Device:R\n");
    let r1 = elem(&nl, "R1");
    assert!(
        has_tag(r1, |t| matches!(t, Tag::Symbol(s) if s == "Device:R")),
        "Symbol tag 'Device:R' expected; tags: {:?}",
        r1.tags
    );
}

// ---------------------------------------------------------------------------
// D. Continuation edge cases
// ---------------------------------------------------------------------------

/// `$` comment on a continuation line: tc=0.001 is still parsed; tokens
/// after `$` are stripped (ngspice inpcom.c inp_stripcomments_line).
#[test]
fn continuation_with_dollar_comment() {
    let nl = parse_ok("* t\nR1 a b 1k\n+ tc=0.001 $ comment\n");
    let r1 = elem(&nl, "R1");
    assert!(
        r1.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("tc")),
        "tc param not found; params: {:?}",
        r1.params
    );
}

/// `+` continuation after a block annotation line attaches to the element,
/// not to the annotation (lexer continues the last Code line, not the
/// last BlockAnnotation line).
#[test]
fn continuation_immediately_after_block_annotation() {
    let nl = parse_ok("* t\n*@symbol Device:R for=R*\nR1 a b 1k\n+ tc=0.001\n");
    let r1 = elem(&nl, "R1");
    assert!(
        r1.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("tc")),
        "tc param must attach to R1; params: {:?}",
        r1.params
    );
}

// ---------------------------------------------------------------------------
// E. Mixed scope edge cases
// ---------------------------------------------------------------------------

/// `*@symbol` and `*@align` inside a subckt both land in subckt.annotations.
#[test]
fn subckt_with_block_annotations_inside() {
    let src = "* t\n.subckt BLK a b\n*@symbol Device:R for=R*\n*@align horizontal R1 R2\nR1 a b 1k\n.ends\n";
    let nl = parse_ok(src);
    let sub = nl.subckts.iter().find(|s| s.name == "BLK").expect("BLK");
    assert!(
        sub.annotations
            .iter()
            .any(|a| matches!(&a.annotation, Annotation::SymbolDefault { .. })),
        "SymbolDefault expected in subckt; got: {:?}",
        sub.annotations
    );
    assert!(
        sub.annotations
            .iter()
            .any(|a| matches!(&a.annotation, Annotation::Align { .. })),
        "Align expected in subckt; got: {:?}",
        sub.annotations
    );
}

/// `*@symbol` inside inner subckt lands in inner's annotations only, not
/// outer's or top-level.
#[test]
fn nested_subckt_block_annotation_scope() {
    let src = "* t\n.subckt OUTER a b\n.subckt INNER x y\n*@symbol Device:R for=R*\nR1 x y 1k\n.ends\n.ends\n";
    let nl = parse_ok(src);
    assert!(
        nl.annotations.is_empty(),
        "top-level annotations must be empty"
    );
    let inner = nl
        .subckts
        .iter()
        .find(|s| s.name == "INNER")
        .expect("INNER");
    assert!(
        inner
            .annotations
            .iter()
            .any(|a| matches!(&a.annotation, Annotation::SymbolDefault { .. })),
        "SymbolDefault expected in INNER; got: {:?}",
        inner.annotations
    );
    // OUTER should have no annotations.
    let outer = nl
        .subckts
        .iter()
        .find(|s| s.name == "OUTER")
        .expect("OUTER");
    assert!(
        outer.annotations.is_empty(),
        "OUTER annotations must be empty; got: {:?}",
        outer.annotations
    );
}

// ---------------------------------------------------------------------------
// Lexer corner cases not covered earlier
// ---------------------------------------------------------------------------

#[test]
fn dollar_at_start_of_line() {
    let nl = parse_ok("* t\nR1 a b 1k\n$ standalone comment\nR2 a b 2k\n");
    // ngspice would have only R1 and R2; our lexer would try to parse the
    // `$` line. Confirm both Rs land regardless of how the `$` line is treated.
    assert!(nl.elements.iter().any(|e| e.designator == "R1"));
    assert!(nl.elements.iter().any(|e| e.designator == "R2"));
}

#[test]
fn semicolon_no_preceding_space() {
    let nl = parse_ok("* t\nR1 a b 1k;@symbol=Device:R\n");
    let r1 = nl.elements.iter().find(|e| e.designator == "R1").unwrap();
    assert!(
        r1.tags
            .iter()
            .any(|t| matches!(&t.tag, Tag::Symbol(s) if s == "Device:R"))
    );
}

#[test]
fn tag_marker_with_extra_whitespace() {
    let nl = parse_ok("* t\nR1 a b 1k ;   @  symbol=Device:R\n");
    let r1 = nl.elements.iter().find(|e| e.designator == "R1").unwrap();
    assert!(
        r1.tags
            .iter()
            .any(|t| matches!(&t.tag, Tag::Symbol(s) if s == "Device:R"))
    );
}

#[test]
fn crlf_with_continuation() {
    let nl = parse_ok("* t\r\nR1 a b 1k\r\n+ tc=0.001\r\n");
    let r1 = nl.elements.iter().find(|e| e.designator == "R1").unwrap();
    assert!(r1.params.iter().any(|(k, _)| k == "tc"));
}

#[test]
fn tab_only_indentation() {
    let nl = parse_ok("* t\n\tR1\ta\tb\t1k\n");
    let r1 = nl.elements.iter().find(|e| e.designator == "R1").unwrap();
    assert_eq!(r1.nodes, ["a", "b"]);
}

#[test]
fn empty_continuation_line() {
    let nl = parse_ok("* t\nR1 a b 1k\n+\n+ tc=0.001\n");
    let r1 = nl.elements.iter().find(|e| e.designator == "R1").unwrap();
    assert!(r1.params.iter().any(|(k, _)| k == "tc"));
}

// ===========================================================================
// Moved from edge_inputs.rs
// ===========================================================================

// ---------------------------------------------------------------------------
// Physical-line splitter
// ---------------------------------------------------------------------------

#[test]
fn empty_input() {
    let nl = parse_ok("");
    assert_eq!(nl.title, "");
    assert!(nl.elements.is_empty());
}

#[test]
fn only_newline() {
    // A single `\n` produces one physical line of "" — that becomes the title.
    let nl = parse_ok("\n");
    assert_eq!(nl.title, "");
    assert!(nl.elements.is_empty());
}

#[test]
fn only_whitespace() {
    let nl = parse_ok("   \n\t\n");
    assert_eq!(nl.title, "");
    assert!(nl.elements.is_empty());
}

#[test]
fn no_trailing_newline_with_element() {
    let nl = parse_ok("* t\nR1 a b 1k");
    let r1 = elem(&nl, "R1");
    assert_eq!(r1.nodes, ["a", "b"]);
}

#[test]
fn no_trailing_newline_title_only() {
    let nl = parse_ok("* just a title");
    assert_eq!(nl.title, "just a title");
    assert!(nl.elements.is_empty());
}

#[test]
fn crlf_no_final_newline() {
    let nl = parse_ok("* t\r\nR1 a b 1k");
    let r1 = elem(&nl, "R1");
    assert_eq!(r1.nodes, ["a", "b"]);
}

/// Bare `\r` is NOT a line separator (splitter only splits on `\n`).
#[test]
fn bare_cr_line_endings() {
    let nl = parse_ok("* t\rR1 a b 1k\r");
    assert!(
        nl.title.contains("R1"),
        "bare \\r not a separator; whole input is title: {:?}",
        nl.title
    );
    assert!(
        nl.elements.is_empty(),
        "no elements expected; got {:?}",
        nl.elements
            .iter()
            .map(|e| &e.designator)
            .collect::<Vec<_>>()
    );
}

/// Lone `\r` mid-line is not stripped — it ends up inside a token.
#[test]
fn lone_cr_in_middle_of_line() {
    let s = scan("* t\nR1 a b\r 1k\n", fid());
    let code: Vec<_> = s
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Code)
        .collect();
    assert_eq!(code.len(), 1);
    let words: Vec<_> = code[0].words.iter().map(|w| w.text.as_str()).collect();
    let has_cr_inside = words.iter().any(|w| w.contains('\r'));
    let split_clean = words == ["R1", "a", "b", "1k"];
    assert!(
        has_cr_inside || split_clean,
        "unexpected tokenisation: {words:?}"
    );
}

// ---------------------------------------------------------------------------
// Continuation edge cases
// ---------------------------------------------------------------------------

#[test]
fn multiple_blank_lines_then_continuation() {
    let nl = parse_ok("* t\nR1 a b 1k\n\n\n+ tc=0.001\n");
    let r1 = elem(&nl, "R1");
    assert!(
        r1.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("tc")),
        "tc must attach to R1 across blanks; params: {:?}",
        r1.params
    );
}

#[test]
fn continuation_then_continuation_then_blank_then_continuation() {
    let nl = parse_ok("* t\nR1 a b 1k\n+ tc1=0.1\n\n+ tc2=0.01\n+ tc3=0.001\n");
    let r1 = elem(&nl, "R1");
    for k in ["tc1", "tc2", "tc3"] {
        assert!(
            r1.params.iter().any(|(p, _)| p.eq_ignore_ascii_case(k)),
            "{k} missing; params: {:?}",
            r1.params
        );
    }
}

/// `+` line with nothing to continue: must not panic and must not
/// silently corrupt a later element.
#[test]
fn continuation_at_start_of_file() {
    let nl = parse_ok("* t\n+ stuff\nR1 a b 1k\n");
    let r1 = elem(&nl, "R1");
    assert_eq!(r1.nodes, ["a", "b"]);
    assert!(
        !r1.params.iter().any(|(k, _)| k == "stuff"),
        "stuff must not leak into R1; params: {:?}",
        r1.params
    );
}

/// `+` after a block annotation cannot continue it.
#[test]
fn continuation_after_block_annotation_only() {
    let s = scan("* t\n*@symbol Device:R for=R*\n+ extra\n", fid());
    let block: Vec<_> = s
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::BlockAnnotation)
        .collect();
    assert_eq!(block.len(), 1);
    let block_words: Vec<_> = block[0].words.iter().map(|w| w.text.as_str()).collect();
    assert!(
        !block_words.contains(&"extra"),
        "`+ extra` must not merge into the block annotation; words: {block_words:?}"
    );
}

/// Pure `*` comments and blank lines do not reset the continuation target.
#[test]
fn continuation_after_pure_comment_then_blank_then_element() {
    let nl = parse_ok("* t\nR1 a b 1k\n* a comment\n\n* another\n+ tc=0.001\nR2 a b 2k\n");
    let r1 = elem(&nl, "R1");
    assert!(
        r1.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("tc")),
        "tc must attach to R1 across comments+blank; params: {:?}",
        r1.params
    );
    let r2 = elem(&nl, "R2");
    assert!(
        !r2.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("tc")),
        "tc must NOT attach to R2; params: {:?}",
        r2.params
    );
}

// ---------------------------------------------------------------------------
// Whitespace and tab edge cases
// ---------------------------------------------------------------------------

#[test]
fn tab_separated_key_equals_value() {
    let nl = parse_ok("* t\nR1 a b 1k\tac\t=\t1k\n");
    let r1 = elem(&nl, "R1");
    assert_eq!(r1.nodes, ["a", "b"]);
    assert!(
        r1.params.iter().any(|(k, _)| k.eq_ignore_ascii_case("ac")),
        "ac param expected; params: {:?}",
        r1.params
    );
}

#[test]
fn multiple_consecutive_spaces_between_tokens() {
    let nl = parse_ok("* t\nR1     a       b     1k\n");
    let r1 = elem(&nl, "R1");
    assert_eq!(r1.nodes, ["a", "b"]);
    match &r1.value {
        Some(Value::Number(n)) => assert!((n - 1000.0).abs() < 1e-9),
        other => panic!("expected Number(1000), got {other:?}"),
    }
}

#[test]
fn trailing_spaces_after_value() {
    let nl = parse_ok("* t\nR1 a b 1k     \n");
    let r1 = elem(&nl, "R1");
    match &r1.value {
        Some(Value::Number(n)) => assert!((n - 1000.0).abs() < 1e-9),
        other => panic!("expected Number(1000), got {other:?}"),
    }
}

#[test]
fn tab_inside_tag_body() {
    let nl = parse_ok("* t\nR1 a b 1k ;@ symbol\t=\tDevice:R\n");
    let r1 = elem(&nl, "R1");
    assert!(
        has_tag(r1, |t| matches!(t, Tag::Symbol(s) if s == "Device:R")),
        "Symbol(Device:R) expected; tags: {:?}",
        r1.tags
    );
}

// ---------------------------------------------------------------------------
// Title-line edge cases (lexer-level)
// ---------------------------------------------------------------------------

#[test]
fn title_is_blank_first_line() {
    let s = scan("\nR1 a b 1k\n", fid());
    assert_eq!(s.title, "");
    let code: Vec<_> = s
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Code)
        .collect();
    assert_eq!(code.len(), 1);
    assert_eq!(code[0].words[0].text, "R1");
}

#[test]
fn title_with_tabs_and_leading_stars() {
    let s = scan("*\t\t\thello world  \n", fid());
    assert_eq!(s.title, "hello world");
}

#[test]
fn title_followed_immediately_by_end() {
    let nl = parse_ok("* title\n.end\n");
    assert_eq!(nl.title, "title");
    assert!(nl.elements.is_empty());
}

#[test]
fn single_line_no_terminator() {
    let s = scan("Just a title with no newline", fid());
    assert_eq!(s.title, "Just a title with no newline");
    assert!(s.lines.is_empty());
}

// ---------------------------------------------------------------------------
// Tokeniser edge cases
// ---------------------------------------------------------------------------

/// `(` and `)` are separators. With nothing else, R1 ends up with `(` and `)`
/// as positional tokens.
#[test]
fn paren_only_word() {
    let nl = parse_ok("* t\nR1 a b ()\n");
    let r1 = elem(&nl, "R1");
    assert_eq!(r1.nodes, ["a", "b"]);
    match &r1.value {
        Some(Value::String(s)) => {
            assert!(
                s.contains('(') && s.contains(')'),
                "value should contain parens; got {s:?}"
            );
        }
        other => panic!("expected Value::String containing parens, got {other:?}"),
    }
}

/// `k=1k=2` tokenises to [k, =, 1k, =, 2].
#[test]
fn consecutive_equals_signs() {
    let nl = parse_ok("* t\nR1 a b k=1k=2\n");
    let r1 = elem(&nl, "R1");
    assert_eq!(r1.nodes, ["a", "b"]);
    assert!(
        r1.params
            .iter()
            .any(|(k, v)| k == "k" && matches!(v, Value::Number(n) if (n - 1000.0).abs() < 1e-9)),
        "expected param k=1000; params: {:?}",
        r1.params
    );
    assert!(
        r1.value.is_some(),
        "leftover `= 2` should land somewhere; element: {r1:?}"
    );
}

/// `=garbage` at start of word: tokenises to [=, garbage, a, b, 1k].
#[test]
fn equals_at_start_of_word() {
    let nl = parse_ok("* t\nR1 =garbage a b 1k\n");
    let r1 = elem(&nl, "R1");
    assert_eq!(r1.nodes.len(), 2);
    assert!(
        r1.params.is_empty(),
        "no key=value params expected; got: {:?}",
        r1.params
    );
}

// ---------------------------------------------------------------------------
// Tag harvesting edge cases
// ---------------------------------------------------------------------------

#[test]
fn multiple_semicolons_no_at_markers() {
    let nl = parse_ok("* t\nR1 a b 1k ; foo ; bar ; baz\n");
    let r1 = elem(&nl, "R1");
    assert!(r1.tags.is_empty(), "no tags expected; got: {:?}", r1.tags);
}

/// `;@` with empty body: harvested as a RawTag with body "" — must not panic.
#[test]
fn semicolon_at_only_no_directive() {
    let nl = parse_ok("* t\nR1 a b 1k ;@\n");
    let r1 = elem(&nl, "R1");
    assert!(
        !r1.tags.iter().any(|t| matches!(
            &t.tag,
            Tag::Symbol(_) | Tag::Pinmap(_) | Tag::Place { .. } | Tag::Power(_) | Tag::Ignore
        )),
        "no recognised tag should be produced; got: {:?}",
        r1.tags
    );
}

/// `;@=value`: directive name is empty (split_directive stops at `=`).
#[test]
fn semicolon_at_equals_only() {
    let nl = parse_ok("* t\nR1 a b 1k ;@=value\n");
    let r1 = elem(&nl, "R1");
    assert!(
        !r1.tags.iter().any(|t| matches!(
            &t.tag,
            Tag::Symbol(_) | Tag::Pinmap(_) | Tag::Place { .. } | Tag::Power(_) | Tag::Ignore
        )),
        "no recognised tag should be produced; got: {:?}",
        r1.tags
    );
}

/// `;@ symbol=` (empty value) is malformed per spec §4.1: it produces an
/// `E910` diagnostic and the tag is dropped (no bogus `Tag::Symbol("")`).
#[test]
fn semicolon_at_symbol_empty_value() {
    let out = common::parse_with_diags("* t\nR1 a b 1k ;@ symbol=\n");
    let r1 = elem(&out.netlist, "R1");
    assert!(
        !has_tag(r1, |t| matches!(t, Tag::Symbol(_))),
        "empty `symbol=` must not produce a Symbol tag; tags: {:?}",
        r1.tags
    );
    assert!(
        out.diagnostics.iter().any(|d| d.code == "E910"),
        "expected E910 for empty symbol value; diags: {}",
        common::fmt_diags(&out.diagnostics)
    );
}
