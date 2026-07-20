//! Tier 0 — a `place` / `align` directive must never stack two symbols
//! at one origin.
//!
//! # The defect
//!
//! `rc_lowpass_ports.cir` plus either `*@align horizontal R1 C1` or
//! `C1 … ;@ place=right-of R1` used to emit **both symbols at the same
//! `(at …)`**. Coincident symbols mean coincident pins, and KiCad
//! merges geometric coincidence into electrical connection with no
//! junction marker — so the two nets silently short. The CLI's
//! post-hoc connectivity verifier caught it only *after* writing the
//! file, reporting "emitted schematic does not match the source
//! netlist / This is a converter bug".
//!
//! # Root cause (not what it looks like)
//!
//! `solve_place` is **correct**: it separates the pair by `CELL_W`
//! (7.62 mm) even for two parts whose pins all sit at x = 0 in identity
//! orientation (`Device:R_US`, `Device:C`). The collapse happened
//! *afterwards*, in the rail-stub-column idiom
//! (`spice_layout::apply_rail_stub_columns`): `C1` is a ground-side
//! rail stub on net `out`, and the idiom snapped its X onto that net's
//! anchor column — which is `R1`'s own column. The idiom's revert guard
//! scored only `cost::constraint_residual`, whose `RightOf` X term is a
//! one-sided hinge, so a collapse to *zero* separation still scored
//! zero and passed as "not strictly worse".
//!
//! The fix adds a second, categorical revert condition: the idiom is
//! rolled back wholesale if it *creates* a symbol-body overlap. That
//! covers the whole class (any stub snapping onto an occupied column),
//! not just the annotated case, and leaves the user with the working
//! schematic they asked for rather than a hard diagnostic — the layout
//! is legitimately expressible.

mod common;

use std::path::{Path, PathBuf};

use common::spice_to_kicad;
use lexpr::Value;

const BASE: &str = "\
* RC low-pass filter — single-pole, with I/O ports.
*@symbol Device:R_US for=R*
*@symbol Device:C for=C*
*@port in=input
*@port out=output
V1 in  0   AC 1   ;@ ignore
";

fn tempdir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spice2kicad-coincide-{pid}-{seq}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

/// Every `(symbol (lib_id …) (at x y rot) …)` origin in the sheet,
/// paired with its `Reference` property so failures name the parts.
fn symbol_origins(path: &Path) -> Vec<(String, f64, f64)> {
    let src = std::fs::read_to_string(path).expect("read sch");
    let sexp: Value = lexpr::from_str(&src).expect("parse sch");
    let mut out = Vec::new();
    let Some(top) = sexp.list_iter() else {
        return out;
    };
    for node in top {
        let Some(mut it) = node.list_iter() else {
            continue;
        };
        if it.next().and_then(Value::as_symbol) != Some("symbol") {
            continue;
        }
        // A top-level instance has `(lib_id …)`; a `lib_symbols` entry
        // (which is a string-headed `(symbol "Device:R_US" …)`) does not.
        let rest: Vec<&Value> = it.collect();
        let mut origin = None;
        let mut is_instance = false;
        let mut reference = String::new();
        for field in &rest {
            let Some(mut f) = field.list_iter() else {
                continue;
            };
            match f.next().and_then(Value::as_symbol) {
                Some("lib_id") => is_instance = true,
                Some("at") => {
                    let nums: Vec<f64> = f.filter_map(Value::as_f64).collect();
                    if nums.len() >= 2 {
                        origin = Some((nums[0], nums[1]));
                    }
                }
                Some("property") => {
                    let vals: Vec<&Value> = f.collect();
                    if vals.first().and_then(|v| v.as_str()) == Some("Reference") {
                        reference = vals
                            .get(1)
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_owned();
                    }
                }
                _ => {}
            }
        }
        if let (true, Some((x, y))) = (is_instance, origin) {
            out.push((reference, x, y));
        }
    }
    out
}

/// Convert `source` and assert no two *component* symbols share an
/// origin. Power glyphs (`#PWR` / `#FLG`) are excluded: a GND glyph and
/// a `PWR_FLAG` legitimately stack on the same rail point.
fn assert_no_coincident_symbols(name: &str, source: &str) {
    let dir = tempdir(name);
    let cir = dir.join(format!("{name}.cir"));
    std::fs::write(&cir, source).expect("write fixture");

    // The conversion itself must succeed. It used to fail with
    // "emitted schematic does not match the source netlist" — the
    // connectivity verifier catching the short after the fact.
    let sch = spice_to_kicad(&cir, &dir)
        .unwrap_or_else(|e| panic!("{name}: conversion failed (Tier 0 netlist mismatch?): {e}"));

    let origins: Vec<(String, f64, f64)> = symbol_origins(&sch)
        .into_iter()
        .filter(|(r, _, _)| !r.starts_with('#'))
        .collect();
    assert!(
        origins.len() >= 2,
        "{name}: expected at least two component symbols, got {origins:?}"
    );
    for (i, (a, ax, ay)) in origins.iter().enumerate() {
        for (b, bx, by) in origins.iter().skip(i + 1) {
            assert!(
                (ax - bx).abs() > 1e-6 || (ay - by).abs() > 1e-6,
                "{name}: `{a}` and `{b}` are coincident at ({ax}, {ay}) — \
                 coincident symbols short their nets (Tier 0, V11)"
            );
        }
    }
}

#[test]
fn place_right_of_between_two_net_sharing_two_pin_parts() {
    assert_no_coincident_symbols(
        "place_right_of",
        &format!("{BASE}R1 in  out 1k\nC1 out 0   100n ;@ place=right-of R1\n.end\n"),
    );
}

#[test]
fn place_left_of_between_two_net_sharing_two_pin_parts() {
    assert_no_coincident_symbols(
        "place_left_of",
        &format!("{BASE}R1 in  out 1k\nC1 out 0   100n ;@ place=left-of R1\n.end\n"),
    );
}

#[test]
fn place_above_between_two_net_sharing_two_pin_parts() {
    assert_no_coincident_symbols(
        "place_above",
        &format!("{BASE}R1 in  out 1k\nC1 out 0   100n ;@ place=above R1\n.end\n"),
    );
}

#[test]
fn place_below_between_two_net_sharing_two_pin_parts() {
    assert_no_coincident_symbols(
        "place_below",
        &format!("{BASE}R1 in  out 1k\nC1 out 0   100n ;@ place=below R1\n.end\n"),
    );
}

#[test]
fn align_horizontal_between_two_net_sharing_two_pin_parts() {
    assert_no_coincident_symbols(
        "align_horizontal",
        &format!("{BASE}*@align horizontal R1 C1\nR1 in  out 1k\nC1 out 0   100n\n.end\n"),
    );
}

#[test]
fn align_vertical_between_two_net_sharing_two_pin_parts() {
    assert_no_coincident_symbols(
        "align_vertical",
        &format!("{BASE}*@align vertical R1 C1\nR1 in  out 1k\nC1 out 0   100n\n.end\n"),
    );
}

/// The spec §4.3 direction, measured end-to-end on a real conversion
/// (the unit-level counterpart lives in
/// `spice-layout/tests/place_direction.rs`). `above` must put the
/// annotated element at a SMALLER screen y than its anchor.
#[test]
fn above_and_below_match_the_spec_direction_end_to_end() {
    for (name, rel, expect_target_above) in [
        ("dir_above", "above", true),
        ("dir_below", "below", false),
    ] {
        let dir = tempdir(name);
        let cir = dir.join(format!("{name}.cir"));
        std::fs::write(
            &cir,
            format!("{BASE}R1 in  out 1k\nC1 out 0   100n ;@ place={rel} R1\n.end\n"),
        )
        .expect("write fixture");
        let sch = spice_to_kicad(&cir, &dir).expect("conversion");
        let origins = symbol_origins(&sch);
        let y = |r: &str| {
            origins
                .iter()
                .find(|(name, _, _)| name == r)
                .unwrap_or_else(|| panic!("{r} not placed"))
                .2
        };
        let (c1, r1) = (y("C1"), y("R1"));
        if expect_target_above {
            assert!(
                c1 < r1,
                "spec §4.3: `C1 ;@ place=above R1` must put C1 ABOVE R1 \
                 (smaller screen y); got C1 y={c1}, R1 y={r1}"
            );
        } else {
            assert!(
                c1 > r1,
                "spec §4.3: `C1 ;@ place=below R1` must put C1 BELOW R1 \
                 (larger screen y); got C1 y={c1}, R1 y={r1}"
            );
        }
    }
}
