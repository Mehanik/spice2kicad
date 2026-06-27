//! Exhaustive coverage of parser-emitted diagnostic codes
//! (E900/E901/E902/E903/E904/E005, W103, W900) and malformed-input
//! edge cases on `;@` tags and `*@` block annotations.
//!
//! Warning paths: `parse` returns a [`ParseOutcome`] on success that
//! carries non-fatal diagnostics, so W-code tests can assert on the
//! diagnostic directly.

mod common;

use common::{parse_err, parse_ok, parse_with_diags};

// ─── Section A: parser-emitted error codes ───────────────────────────────────

#[test]
fn e900_stray_ends() {
    let diags = parse_err("* t\n.ends\n");
    assert!(
        diags.iter().any(|d| d.code == "E900"),
        "expected E900 in {diags:?}"
    );
}

#[test]
fn e901_subckt_missing_name() {
    let diags = parse_err("* t\n.subckt\n.ends\n");
    assert!(
        diags.iter().any(|d| d.code == "E901"),
        "expected E901 in {diags:?}"
    );
}

#[test]
fn e902_model_missing_type() {
    let diags = parse_err("* t\n.model MYNAME\n");
    assert!(
        diags.iter().any(|d| d.code == "E902"),
        "expected E902 in {diags:?}"
    );
}

#[test]
fn e903_invalid_place_relation() {
    let diags = parse_err("* t\nR1 a b 1k ;@ place=banana V1\n");
    assert!(
        diags.iter().any(|d| d.code == "E903"),
        "expected E903 in {diags:?}"
    );
}

#[test]
fn e903_place_missing_anchor() {
    // `parse_place` consumes the relation, then `?` on the missing
    // anchor returns None; outer `or_else` pushes E903.
    let diags = parse_err("* t\nR1 a b 1k ;@ place=right-of\n");
    assert!(
        diags.iter().any(|d| d.code == "E903"),
        "expected E903 in {diags:?}"
    );
}

#[test]
fn e904_align_unknown_axis() {
    let diags = parse_err("* t\n*@align diagonal R1 R2\n");
    assert!(
        diags.iter().any(|d| d.code == "E904"),
        "expected E904 in {diags:?}"
    );
}

#[test]
fn e904_align_no_refdes() {
    // Axis present but no refdes → tail.len() < 2 → E904.
    let diags = parse_err("* t\n*@align horizontal\n");
    assert!(
        diags.iter().any(|d| d.code == "E904"),
        "expected E904 in {diags:?}"
    );
}

#[test]
fn e005_invalid_pinmap_no_colon() {
    let diags = parse_err("* t\nD1 a k DMOD ;@ pinmap=1\n");
    assert!(
        diags.iter().any(|d| d.code == "E005"),
        "expected E005 in {diags:?}"
    );
}

#[test]
fn pinmap_non_numeric_lhs_parses_as_port_name() {
    // A non-numeric left-hand side is no longer a parse-level E005: it
    // is a `.subckt` port name (spec §4.2), carried through as
    // `PinmapEntry { port_name: Some(_), spice_index: 0 }` for the
    // resolver to bind. Whether the name is actually a valid port (or
    // whether the element is even a `.subckt` instance) is the
    // resolver's call (E009), not the parser's.
    use spice_parser::ast::{PinRef, Tag};
    let nl = parse_ok("* t\nX1 a b OPAMP ;@ pinmap=inp:1\n");
    let x1 = nl
        .elements
        .iter()
        .find(|e| e.designator == "X1")
        .expect("X1 present");
    let entries = x1
        .tags
        .iter()
        .find_map(|t| match &t.tag {
            Tag::Pinmap(es) => Some(es),
            _ => None,
        })
        .expect("pinmap tag present");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].port_name.as_deref(), Some("inp"));
    assert_eq!(entries[0].spice_index, 0);
    assert!(matches!(&entries[0].kicad_pin, PinRef::Number(n) if n == "1"));
}

#[test]
fn e005_invalid_pinmap_empty() {
    // `pinmap=` → rest is empty string → parse_pinmap yields no
    // entries → returns None → E005.
    let diags = parse_err("* t\nD1 a k DMOD ;@ pinmap=\n");
    assert!(
        diags.iter().any(|d| d.code == "E005"),
        "expected E005 in {diags:?}"
    );
}

// ─── Section B: warning-only paths (warnings dropped, parse succeeds) ────────

#[test]
fn w900_unterminated_subckt_returns_ok() {
    let outcome = parse_with_diags("* t\n.subckt M a b\nR1 a b 1k\n");
    assert!(
        outcome.netlist.subckts.iter().any(|s| s.name == "M"),
        "subckt M missing"
    );
    assert!(
        outcome.diagnostics.iter().any(|d| d.code == "W900"),
        "expected W900 in {:?}",
        outcome.diagnostics
    );
}

#[test]
fn w103_unknown_tag_directive() {
    let outcome = parse_with_diags("* t\nR1 a b 1k ;@ frobnicate=1\n");
    let r1 = outcome
        .netlist
        .elements
        .iter()
        .find(|e| e.designator == "R1")
        .expect("R1");
    assert!(r1.tags.is_empty(), "expected no tags, got {:?}", r1.tags);
    assert!(
        outcome.diagnostics.iter().any(|d| d.code == "W103"),
        "expected W103 in {:?}",
        outcome.diagnostics
    );
}

#[test]
fn w103_unknown_block_directive() {
    let outcome = parse_with_diags("* t\n*@bogus arg1 arg2\nR1 a b 1k\n");
    assert!(
        outcome.netlist.annotations.is_empty(),
        "expected no annotations, got {:?}",
        outcome.netlist.annotations
    );
    assert!(
        outcome.diagnostics.iter().any(|d| d.code == "W103"),
        "expected W103 in {:?}",
        outcome.diagnostics
    );
}

// ─── Section C: malformed tag/annotation edge cases ──────────────────────────

#[test]
fn tag_no_value_after_equals() {
    // `;@ symbol=` — `rest_after_eq` is Some(""), so the symbol arm
    // takes that path and produces `Tag::Symbol("")`. This is arguably
    // wrong (an empty lib_id is not useful) but the parser currently
    // accepts it. Documented here.
    let nl = parse_ok("* t\nR1 a b 1k ;@ symbol=\n");
    let r1 = nl
        .elements
        .iter()
        .find(|e| e.designator == "R1")
        .expect("R1");
    // Either zero tags (if the parser is later tightened) or one tag
    // with empty Symbol payload; both are documented current behaviour.
    if let Some(t) = r1.tags.first() {
        match &t.tag {
            spice_parser::ast::Tag::Symbol(s) => assert_eq!(s, ""),
            other => panic!("unexpected tag variant: {other:?}"),
        }
    }
}

#[test]
fn tag_directive_only_no_args() {
    // `;@ symbol` — neither = nor whitespace+value present → `?`
    // returns None inside the symbol arm → tag dropped silently.
    let nl = parse_ok("* t\nR1 a b 1k ;@ symbol\n");
    let r1 = nl
        .elements
        .iter()
        .find(|e| e.designator == "R1")
        .expect("R1");
    assert!(r1.tags.is_empty(), "expected no tags, got {:?}", r1.tags);
}

#[test]
fn block_symbol_no_libid() {
    // `*@symbol for=R*` with no positional lib_id → `positional.first()?`
    // None → annotation dropped silently (no diagnostic).
    let nl = parse_ok("* t\n*@symbol for=R*\n");
    assert!(
        nl.annotations.is_empty(),
        "expected no annotations, got {:?}",
        nl.annotations
    );
}

#[test]
fn pinmap_with_repeated_spice_index() {
    let diags = parse_err("* t\nD1 a k DMOD ;@ pinmap=1:A,1:K\n");
    assert!(
        diags.iter().any(|d| d.code == "E005"),
        "expected E005 in {diags:?}"
    );
}

#[test]
fn pinmap_with_repeated_kicad_pin() {
    let diags = parse_err("* t\nD1 a k DMOD ;@ pinmap=1:A,2:A\n");
    assert!(
        diags.iter().any(|d| d.code == "E005"),
        "expected E005 in {diags:?}"
    );
}

// ─── Section D: sanity / regression ──────────────────────────────────────────

#[test]
fn successful_parse_yields_no_error_diagnostics() {
    // The happy path returns Ok — the absence of Err is the assertion.
    let nl = parse_ok("* t\nR1 a b 1k\n");
    assert_eq!(nl.elements.len(), 1);
    assert_eq!(nl.elements[0].designator, "R1");
}

#[test]
fn multiple_errors_collected() {
    // Two independent errors: stray .ends (E900) and bad align axis (E904).
    let diags = parse_err("* t\n.ends\n*@align diagonal R1 R2\n");
    assert!(
        diags.iter().any(|d| d.code == "E900"),
        "expected E900 in {diags:?}"
    );
    assert!(
        diags.iter().any(|d| d.code == "E904"),
        "expected E904 in {diags:?}"
    );
}
