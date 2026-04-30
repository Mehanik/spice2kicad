//! Shared helpers for the integration test suites.
//!
//! Each suite (`numbers`, `elements`, `directives`, `lex_edges`,
//! `corpus`) parses small SPICE snippets and asserts shape on the
//! resulting [`Netlist`]. These helpers keep the boilerplate out.

#![allow(dead_code)] // each test suite uses a different subset

use spice_diagnostics::{Diagnostic, FileId};
use spice_parser::Netlist;
use spice_parser::ast::{Annotation, Element, ElementKind, Tag, Value};
use spice_parser::{ParseOutcome, parse};

pub fn fid() -> FileId {
    FileId(0)
}

/// Parse `source`, panic with a useful message on diagnostics.
/// Returns just the netlist; warnings are dropped (use
/// [`parse_with_diags`] when warnings need inspection).
// TODO: once the public API exposes warnings on Ok, add a
// `parse_clean(src)` helper that asserts `diagnostics.is_empty()`
// for tests that mean "no diagnostics at all".
pub fn parse_ok(source: &str) -> Netlist {
    parse_with_diags(source).netlist
}

/// Parse `source`, returning the full outcome (netlist + warnings).
pub fn parse_with_diags(source: &str) -> ParseOutcome {
    match parse(source, fid()) {
        Ok(o) => o,
        Err(diags) => panic!("expected Ok, got diagnostics: {}", fmt_diags(&diags)),
    }
}

/// Parse `source`, expect at least one error diagnostic.
pub fn parse_err(source: &str) -> Vec<Diagnostic> {
    parse(source, fid()).expect_err("expected parse error")
}

pub fn fmt_diags(diags: &[Diagnostic]) -> String {
    diags
        .iter()
        .map(|d| format!("[{}] {}", d.code, d.message))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Find the first element with a given refdes; panic if missing.
pub fn elem<'a>(nl: &'a Netlist, refdes: &str) -> &'a Element {
    nl.elements
        .iter()
        .find(|e| e.designator.eq_ignore_ascii_case(refdes))
        .unwrap_or_else(|| {
            let names: Vec<_> = nl.elements.iter().map(|e| e.designator.as_str()).collect();
            panic!("no element {refdes}; have: {names:?}")
        })
}

/// Assert the element's value parses to a number close to `expected`
/// (tolerant of float rounding from engineering suffixes).
pub fn assert_value_number(e: &Element, expected: f64) {
    match &e.value {
        Some(Value::Number(n)) => {
            let tol = (expected.abs() * 1e-9).max(1e-15);
            assert!(
                (n - expected).abs() <= tol,
                "{}: expected ~{expected}, got {n}",
                e.designator
            );
        }
        other => panic!("{}: expected Value::Number, got {other:?}", e.designator),
    }
}

pub fn assert_kind(e: &Element, k: ElementKind) {
    assert_eq!(e.kind, k, "{} kind mismatch", e.designator);
}

/// Collect tags of a given variant by closure-driven match.
pub fn has_tag<F: Fn(&Tag) -> bool>(e: &Element, pred: F) -> bool {
    e.tags.iter().any(|t| pred(&t.tag))
}

/// Find the first tag for which `pred` returns `Some(_)` and yield it.
pub fn find_tag<T, F>(e: &Element, pred: F) -> Option<T>
where
    F: Fn(&Tag) -> Option<T>,
{
    e.tags.iter().find_map(|st| pred(&st.tag))
}

/// Like [`find_tag`], but panics with a helpful message if no tag matches.
pub fn expect_tag<T, F>(e: &Element, pred: F) -> T
where
    F: Fn(&Tag) -> Option<T>,
{
    find_tag(e, pred).unwrap_or_else(|| {
        panic!(
            "element {} has no matching tag; tags: {:?}",
            e.designator, e.tags
        )
    })
}

pub fn has_annotation<F: Fn(&Annotation) -> bool>(nl: &Netlist, pred: F) -> bool {
    nl.annotations.iter().any(|a| pred(&a.annotation))
}
