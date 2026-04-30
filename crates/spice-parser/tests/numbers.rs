//! Exhaustive tests for ngspice-compatible number parsing.
//!
//! Ground truth: `INPevaluate` in ngspice/src/spicelib/parser/inpeval.c
//! (Thomas Quarles, 1985; still canonical in current ngspice).

mod common;

use common::{assert_value_number, elem, parse_ok};
use spice_parser::ast::Value;

// Parse `* t\nR1 a b <token>\n` and return the netlist.
fn r(token: &str) -> spice_parser::Netlist {
    parse_ok(&format!("* t\nR1 a b {token}\n"))
}

fn assert_number(token: &str, expected: f64) {
    let nl = r(token);
    assert_value_number(elem(&nl, "R1"), expected);
}

fn assert_string(token: &str) {
    let nl = r(token);
    match &elem(&nl, "R1").value {
        Some(Value::String(_)) | None => {}
        Some(Value::Number(n)) => {
            panic!("{token:?} should not parse as a number; got {n}")
        }
        other => panic!("{token:?}: unexpected value {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 1. Plain integers and decimals
// ---------------------------------------------------------------------------

#[test]
fn plain_integers_and_decimals() {
    assert_number("1", 1.0);
    assert_number("0", 0.0);
    assert_number("1.5", 1.5);
    assert_number(".5", 0.5);
    assert_number("5.", 5.0);
    assert_number("+7", 7.0);
    assert_number("-3", -3.0);
    assert_number("100", 100.0);
}

// ---------------------------------------------------------------------------
// 2. Scientific notation
// ---------------------------------------------------------------------------

#[test]
fn scientific_notation() {
    assert_number("1e3", 1e3);
    assert_number("1E3", 1e3);
    assert_number("1.5e-9", 1.5e-9);
    assert_number("2e+6", 2e6);
    assert_number("1e0", 1.0);
    assert_number("-2.2e4", -2.2e4);
}

// ---------------------------------------------------------------------------
// 11. d/D as Fortran-style exponent marker (ngspice inpeval.c:120)
// ---------------------------------------------------------------------------

#[test]
fn d_exponent_lowercase() {
    assert_number("1d3", 1000.0);
    assert_number("1.5d-9", 1.5e-9);
}

#[test]
fn d_exponent_uppercase() {
    assert_number("1D3", 1000.0);
}

#[test]
fn d_exponent_with_eng_suffix() {
    // ngspice inpeval.c:120-193: exponent parsed first, then suffix switch.
    // 1.5d3k → mantissa 1.5, expo2=3, suffix 'k' adds 3 → 1.5e6.
    assert_number("1.5d3k", 1.5e6);
}

// ---------------------------------------------------------------------------
// 3. Engineering suffixes (lowercase)
// ---------------------------------------------------------------------------

#[test]
fn eng_suffixes_lowercase() {
    assert_number("1t", 1e12);
    assert_number("1g", 1e9);
    assert_number("1meg", 1e6);
    assert_number("1k", 1e3);
    assert_number("1m", 1e-3);
    assert_number("1u", 1e-6);
    assert_number("1n", 1e-9);
    assert_number("1p", 1e-12);
    assert_number("1f", 1e-15);
    assert_number("1mil", 25.4e-6);
}

// ---------------------------------------------------------------------------
// 4. Engineering suffixes (case-insensitive)
// ---------------------------------------------------------------------------

#[test]
fn eng_suffixes_case_insensitive() {
    // Meg variants — all must equal 1e6.
    assert_number("1MEG", 1e6);
    assert_number("1Meg", 1e6);
    assert_number("1mEg", 1e6);

    // Upper-case single-letter suffixes.
    assert_number("1K", 1e3);
    assert_number("1U", 1e-6);
    assert_number("1N", 1e-9);
    assert_number("1P", 1e-12);
    assert_number("1F", 1e-15);
    assert_number("1T", 1e12);
    assert_number("1G", 1e9);
    assert_number("1Mil", 25.4e-6);
}

// ---------------------------------------------------------------------------
// 5. M = milli (NOT mega); Meg = mega
// ---------------------------------------------------------------------------

#[test]
fn m_is_milli_not_mega() {
    // ngspice inpeval.c line 188: plain 'm'/'M' → expo1 -= 3.
    assert_number("1M", 1e-3);
    assert_number("2M", 2e-3);
}

#[test]
fn meg_precedence_over_m() {
    // "Meg" must resolve before bare "M" (peel_eng_suffix checks "meg" first).
    assert_number("2Meg", 2e6);
    assert_number("2MEG", 2e6);
}

// ---------------------------------------------------------------------------
// 6. Trailing unit-letter run silently dropped
// ngspice inpcom.c line 7544: "skip the `unit', FIXME INPevaluate() should
// do this".  The base INPevaluate does NOT do it (it stops at the suffix
// letter and leaves the rest).  Our parser explicitly drops alphabetic tails.
// ---------------------------------------------------------------------------

#[test]
fn trailing_unit_letters_dropped() {
    assert_number("1kHz", 1e3);
    assert_number("100nF", 100e-9);
    assert_number("2.2uF", 2.2e-6);
    assert_number("1Ohm", 1.0); // no suffix → multiplier=1, tail "Ohm" dropped
}

// ---------------------------------------------------------------------------
// 7. 4k7 infix form
// ngspice: only INPevaluateRKM_{R,C,L} accept this (inpeval.c line 206).
// Base INPevaluate does NOT.  Our parser accepts it (parser.rs:716 shows it
// passing the internal unit test).  Mark as passing — we intentionally
// extend beyond base ngspice for LTspice/PSpice compatibility.
// If this ever needs to be gated, see inpeval.c:209.
// ---------------------------------------------------------------------------

#[test]
fn infix_4k7_form() {
    // Our parser accepts 4k7; ngspice base INPevaluate does not — we extend
    // for LTspice/PSpice compatibility (parser.rs handles it explicitly).
    assert_number("4k7", 4700.0);
    assert_number("2k2", 2200.0);
    assert_number("10k0", 10000.0);
}

// ---------------------------------------------------------------------------
// 8. Combined sign + suffix
// ---------------------------------------------------------------------------

#[test]
fn signed_with_suffix() {
    assert_number("-1k", -1e3);
    assert_number("+2u", 2e-6);
    assert_number("-1.5e-3", -1.5e-3);
    assert_number("+3.3meg", 3.3e6);
}

// ---------------------------------------------------------------------------
// 9. Atto suffix (a/A = 1e-18)
// ngspice inpeval.c lines 173-175: 'a'/'A' → expo1 -= 18.
// Our parser does NOT list 'a' in peel_eng_suffix (parser.rs:679-690).
// These are ignored: Value::String because the parser treats the bare token
// e.g. "1A" as "1" followed by suffix "A" which our table doesn't know,
// so it falls through to no-suffix (mult=1.0) and tail "A" is alphabetic →
// accepted as unit tail... meaning "1A" = 1.0.
// Actually verify: peel_eng_suffix returns (1.0, "A", false) for "A",
// then tail "A" is all alphabetic → returns Some(1.0). So "1A" = 1.0, not 1e-18.
// ---------------------------------------------------------------------------

#[test]
fn atto_suffix() {
    // ngspice inpeval.c:172-175: 'a'/'A' → expo1 -= 18.
    assert_number("1a", 1e-18);
    assert_number("1A", 1e-18);
}

// ---------------------------------------------------------------------------
// 10. Rejections — these must NOT produce Value::Number
// ---------------------------------------------------------------------------

#[test]
fn rejections_non_numeric() {
    // Pure alphabetic tokens.
    assert_string("R1"); // refdes-like
    assert_string("AC"); // SPICE keyword
    assert_string("abc");
}

#[test]
fn rejections_malformed_numbers() {
    // Double decimal point — not a valid number.
    assert_string("1.2.3");
    // Double sign.
    assert_string("--3");
}

// ---------------------------------------------------------------------------
// 12. Numeric edge cases (moved from edge_inputs.rs)
// ---------------------------------------------------------------------------

#[test]
fn number_with_leading_plus() {
    let nl = parse_ok("* t\nR1 a b +1k\n");
    let r1 = elem(&nl, "R1");
    match &r1.value {
        Some(Value::Number(n)) => assert!((n - 1000.0).abs() < 1e-9),
        other => panic!("expected Number(1000), got {other:?}"),
    }
}

#[test]
fn number_zero_variants() {
    for input in ["* t\nR1 a b 0\n", "* t\nR1 a b 0.0\n", "* t\nR1 a b -0\n"] {
        let nl = parse_ok(input);
        let r1 = elem(&nl, "R1");
        match &r1.value {
            Some(Value::Number(n)) => assert!(*n == 0.0, "input {input:?}: got {n}"),
            other => panic!("input {input:?}: expected Number(0), got {other:?}"),
        }
    }
}

/// A bare `.` is not a valid number — must fall back to Value::String.
#[test]
fn number_with_only_decimal_point() {
    let nl = parse_ok("* t\nR1 a b .\n");
    let r1 = elem(&nl, "R1");
    match &r1.value {
        Some(Value::String(s)) => assert_eq!(s, "."),
        other => panic!("expected Value::String(\".\"), got {other:?}"),
    }
}

/// f64 overflow — Rust's parser returns Inf, not an error. Verify no panic.
#[test]
fn number_overflow_input() {
    let nl = parse_ok("* t\nR1 a b 1e500\n");
    let r1 = elem(&nl, "R1");
    match &r1.value {
        Some(Value::Number(n)) => assert!(
            n.is_infinite(),
            "expected infinite Number for 1e500; got {n}"
        ),
        Some(Value::String(_)) => {} // also acceptable
        other => panic!("unexpected value: {other:?}"),
    }
}
