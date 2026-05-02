//! V9 — SI-suffixed value formatting for emitted `(property "Value" …)`.
//!
//! Today's emitter writes raw `f64::to_string()` decimals: `100n` becomes
//! `"0.0000001"` (rc_lowpass C1) and `100u` becomes
//! `"0.00009999999999999999"` (common_emitter). Both are unreadable.
//! These tests pin the *expected* shape — SI suffixes per CLAUDE.md
//! § Visual quality invariants V9 — and are `#[ignore]`d until the
//! formatter in `crates/spice-layout/src/lib.rs::format_value` learns
//! the suffix table.
//!
//! Nothing here invokes `kicad-cli`; everything reads the emitted
//! `.kicad_sch` text directly through `lexpr`. That keeps the file
//! runnable on CI hosts that don't ship KiCad.

mod common;

use std::path::{Path, PathBuf};

use common::spice_to_kicad;
use lexpr::Value;

// --- driver bits ---------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> PathBuf {
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("spice2kicad-vf-{pid}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn emit_fixture(name: &str) -> PathBuf {
    let src = fixtures_dir().join(format!("{name}.cir"));
    let tmp = tempdir(name);
    spice_to_kicad(&src, &tmp).expect("spice2kicad")
}

/// Emit a one-off fixture written into a temp directory so this file
/// can hold its own minimal SPICE inputs (e.g. expression values,
/// negative voltages) without polluting `tests/fixtures/`.
fn emit_inline(name: &str, source: &str) -> PathBuf {
    let tmp = tempdir(name);
    let src = tmp.join(format!("{name}.cir"));
    std::fs::write(&src, source).expect("write inline fixture");
    spice_to_kicad(&src, &tmp).expect("spice2kicad")
}

// --- sexp helpers --------------------------------------------------------

fn parse_sch(sch: &Path) -> Value {
    let src = std::fs::read_to_string(sch).expect("read sch");
    lexpr::from_str(&src).expect("parse sch as lexpr")
}

fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    if let Some(it) = v.list_iter() {
        Box::new(it)
    } else {
        Box::new(std::iter::empty())
    }
}

fn head(v: &Value) -> Option<&str> {
    let first = list_iter(v).next()?;
    as_str(first)
}

fn as_str(v: &Value) -> Option<&str> {
    v.as_symbol().or_else(|| v.as_str())
}

fn children<'a>(v: &'a Value, name: &str) -> Vec<&'a Value> {
    list_iter(v)
        .filter(|c| c.is_list() && head(c) == Some(name))
        .collect()
}

/// Find the `(property "<key>" "<value>" …)` child of a `(symbol …)`
/// instance and return its text argument.
fn property_text<'a>(symbol: &'a Value, key: &str) -> Option<&'a str> {
    for prop in children(symbol, "property") {
        let mut it = list_iter(prop);
        let _head = it.next();
        let k = it.next().and_then(as_str)?;
        if k == key {
            return it.next().and_then(as_str);
        }
    }
    None
}

/// Walk every `(symbol …)` instance under the root and return
/// `(refdes, value)` for the named refdes if present. Reference
/// property carries the refdes; Value carries the SI text we're
/// asserting against.
pub fn value_property_for_refdes<'a>(root: &'a Value, refdes: &str) -> Option<&'a str> {
    for sym in children(root, "symbol") {
        let Some(r) = property_text(sym, "Reference") else {
            continue;
        };
        if r == refdes {
            return property_text(sym, "Value");
        }
    }
    None
}

// --- V9 verifier ---------------------------------------------------------

/// Per CLAUDE.md V9: every R/C/L instance's Value text must match
/// the SI-suffix shape. Returns `Err(text)` for the first offender.
fn assert_si_shape(text: &str) -> Result<(), String> {
    if !is_si_value(text) {
        return Err(format!("V9: value text {text:?} is not SI-suffixed"));
    }
    Ok(())
}

/// Tiny hand-rolled matcher for
/// `^-?(0|[0-9]{1,3}(\.[0-9]{1,2})?)(f|p|n|u|m|k|Meg|G|T)?$`.
/// Avoids pulling in `regex` for one site; the grammar is small.
fn is_si_value(s: &str) -> bool {
    let mut rest = s;
    if let Some(stripped) = rest.strip_prefix('-') {
        rest = stripped;
    }
    if rest.is_empty() {
        return false;
    }
    // Mantissa: "0" is its own legal form; otherwise 1-3 digits with
    // an optional 1-2-digit fraction.
    let split_at = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(rest.len());
    let (mantissa, suffix) = rest.split_at(split_at);
    if mantissa == "0" && suffix.is_empty() {
        return true;
    }
    let mantissa_ok = if let Some((int_part, frac_part)) = mantissa.split_once('.') {
        (1..=3).contains(&int_part.len())
            && int_part.chars().all(|c| c.is_ascii_digit())
            && (1..=2).contains(&frac_part.len())
            && frac_part.chars().all(|c| c.is_ascii_digit())
            // V9: trim trailing zeros — `1.0u` should be `1u`.
            && !frac_part.ends_with('0')
    } else {
        (1..=3).contains(&mantissa.len()) && mantissa.chars().all(|c| c.is_ascii_digit())
    };
    if !mantissa_ok {
        return false;
    }
    matches!(
        suffix,
        "" | "f" | "p" | "n" | "u" | "m" | "k" | "Meg" | "G" | "T"
    )
}

// --- per-fixture tests ---------------------------------------------------

#[test]
#[ignore = "V9: see CLAUDE.md § Visual quality invariants V9. \
            format_value in spice-layout currently emits raw f64 to_string; \
            needs SI suffix logic. Today: C1='0.00000010000000000000001', expected '100n'."]
fn v9_capacitor_value_uses_si_suffix() {
    let sch = emit_fixture("rc_lowpass");
    let root = parse_sch(&sch);
    let v = value_property_for_refdes(&root, "C1").expect("C1 Value");
    assert_eq!(
        v, "100n",
        "V9: rc_lowpass C1 (source `100n`) emitted as {v:?}"
    );
    assert_si_shape(v).unwrap();
}

#[test]
#[ignore = "V9: see CLAUDE.md § Visual quality invariants V9. \
            format_value in spice-layout currently emits raw f64 to_string; \
            needs SI suffix logic. Today: R1='1000', expected '1k'."]
fn v9_resistor_value_uses_si_suffix() {
    // rc_lowpass R1 = 1k.
    let sch = emit_fixture("rc_lowpass");
    let root = parse_sch(&sch);
    let v = value_property_for_refdes(&root, "R1").expect("R1 Value");
    assert_eq!(v, "1k", "V9: rc_lowpass R1 (source `1k`) emitted as {v:?}");
    assert_si_shape(v).unwrap();

    // opamp_inverting RIN = 1k, RF = 10k.
    let sch = emit_fixture("opamp_inverting");
    let root = parse_sch(&sch);
    let rin = value_property_for_refdes(&root, "RIN").expect("RIN Value");
    let rf = value_property_for_refdes(&root, "RF").expect("RF Value");
    assert_eq!(rin, "1k", "V9: opamp_inverting RIN emitted as {rin:?}");
    assert_eq!(rf, "10k", "V9: opamp_inverting RF emitted as {rf:?}");
    assert_si_shape(rin).unwrap();
    assert_si_shape(rf).unwrap();
}

#[test]
#[ignore = "V9: see CLAUDE.md § Visual quality invariants V9. \
            format_value in spice-layout currently emits raw f64 to_string; \
            needs SI suffix logic. Inline fixture writes L1 1m; expected '1m'."]
fn v9_inductor_value_uses_si_suffix() {
    // No tree fixture has an inductor today; ship a minimal one inline
    // so the V9 contract covers L as well as R/C.
    let sch = emit_inline(
        "v9_inductor",
        "* V9 inductor smoke\n\
         V1 in 0 DC 1 ;@ ignore\n\
         L1 in out 1m\n\
         R1 out 0  1k\n\
         .end\n",
    );
    let root = parse_sch(&sch);
    let v = value_property_for_refdes(&root, "L1").expect("L1 Value");
    assert_eq!(v, "1m", "V9: L1 (source `1m`) emitted as {v:?}");
    assert_si_shape(v).unwrap();
}

#[test]
#[ignore = "V9: see CLAUDE.md § Visual quality invariants V9. \
            format_value must pass non-numeric Value::Expr / Value::String through verbatim."]
fn v9_value_passthrough_for_non_numeric() {
    // Brace expressions stay literal — the formatter only touches numeric f64.
    let sch = emit_inline(
        "v9_expr",
        "* V9 expression passthrough\n\
         .param RBASE=1k\n\
         V1 in 0 DC 1 ;@ ignore\n\
         R1 in out {2*RBASE}\n\
         R2 out 0  1k\n\
         .end\n",
    );
    let root = parse_sch(&sch);
    let v = value_property_for_refdes(&root, "R1").expect("R1 Value");
    assert_eq!(
        v, "{2*RBASE}",
        "V9: expression value should pass through verbatim, got {v:?}"
    );
}

#[test]
#[ignore = "V9: see CLAUDE.md § Visual quality invariants V9. \
            Negative numerics must keep their sign through the SI formatter \
            (-0.015 -> '-15m')."]
fn v9_negative_value_preserved() {
    // Use a current source with a bare negative numeric value so the
    // parser produces Value::Number(-1e-3) rather than Value::String("DC -15").
    let sch = emit_inline(
        "v9_negative",
        "* V9 negative numeric\n\
         I1 in out -1m\n\
         R1 out 0   1k\n\
         V1 in  0   DC 1 ;@ ignore\n\
         .end\n",
    );
    let root = parse_sch(&sch);
    let v = value_property_for_refdes(&root, "I1").expect("I1 Value");
    assert_eq!(v, "-1m", "V9: negative numeric emitted as {v:?}");
    // Sign must round-trip through the SI checker.
    assert_si_shape(v).unwrap();
}

// --- framework smoke tests (run on every `cargo test`) ------------------
//
// Cover the new helpers so a refactor can't silently disable the V9
// tests when they flip on later. These do NOT depend on the emitter
// behaviour — they only exercise `is_si_value` and
// `value_property_for_refdes` against synthetic input.

#[test]
fn smoke_is_si_value_accepts_expected_forms() {
    for ok in [
        "0", "1k", "10k", "100k", "1u", "100n", "4.7k", "1.5Meg", "-1m", "-15", "999", "1.5G",
        "100T", "1f", "1p",
    ] {
        assert!(is_si_value(ok), "should accept {ok:?}");
    }
}

#[test]
fn smoke_is_si_value_rejects_raw_decimals_and_units() {
    for bad in [
        "0.0000001", // raw decimal — what the emitter writes today
        "1000",      // unsuffixed thousand — should be `1k`
        "1.0u",      // trailing zero in mantissa
        "1uF",       // unit letter — V9 omits it for v0.1
        "10kΩ",      // ohm sign — same
        "1e-6",      // scientific — should be SI
        "",          // empty
        "u",         // bare suffix
        ".5k",       // missing integer part
        "1.k",       // trailing dot
        "1234k",     // four-digit mantissa
        "1.234k",    // three-digit fraction
        "1M",        // ambiguous SPICE 'M' — V9 uses 'Meg' for mega
    ] {
        assert!(!is_si_value(bad), "should reject {bad:?}");
    }
}

#[test]
fn smoke_value_property_for_refdes_finds_match() {
    let src = r#"(kicad_sch
        (symbol (lib_id "Device:R")
            (property "Reference" "R1" (at 0 0 0))
            (property "Value" "1k" (at 0 0 0)))
        (symbol (lib_id "Device:C")
            (property "Reference" "C1" (at 0 0 0))
            (property "Value" "100n" (at 0 0 0))))"#;
    let v: Value = lexpr::from_str(src).unwrap();
    assert_eq!(value_property_for_refdes(&v, "R1"), Some("1k"));
    assert_eq!(value_property_for_refdes(&v, "C1"), Some("100n"));
    assert_eq!(value_property_for_refdes(&v, "L1"), None);
}
