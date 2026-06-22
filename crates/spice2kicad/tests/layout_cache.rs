//! Position-stability sidecar (ADR-4) integration tests.
//!
//! Proves the `<basename>.layout.json` cache keeps untouched elements
//! in place across re-conversions: convert a netlist, record positions,
//! add a NEW element, re-convert WITH the sidecar present, and assert
//! every original element kept its position (within grid tolerance)
//! while the new element was placed without overlapping anything.
//!
//! Also covers: round-trip (write then read), the `--no-layout-cache`
//! opt-out, and that removing an element drops it from the rewritten
//! sidecar.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use lexpr::Value;

// --- driver ---------------------------------------------------------------

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn tempdir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spice2kicad-cache-{pid}-{seq}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

/// Convert `source` SPICE text to a `.kicad_sch` at `out`, optionally
/// disabling the layout cache. Runs the real CLI binary so the sidecar
/// read/write path in `main.rs` is exercised.
fn convert(source: &str, out: &Path, no_cache: bool) {
    let lib_dir = workspace_root().join("crates/kicad-symbols/tests/fixtures");
    let src_path = out.with_extension("cir");
    std::fs::write(&src_path, source).expect("write source");

    let bin = env!("CARGO_BIN_EXE_spice2kicad");
    let mut cmd = Command::new(bin);
    cmd.arg(&src_path)
        .arg("-t")
        .arg("schematic")
        .arg("-o")
        .arg(out)
        .arg("-l")
        .arg(lib_dir.join("Device.kicad_sym"))
        .arg("-l")
        .arg(lib_dir.join("Simulation_SPICE.kicad_sym"))
        .arg("-l")
        .arg(lib_dir.join("power.kicad_sym"));
    if no_cache {
        cmd.arg("--no-layout-cache");
    }
    let status = cmd.status().expect("invoke spice2kicad");
    assert!(status.success(), "spice2kicad failed: {status}");
}

fn sidecar_path(out: &Path) -> PathBuf {
    out.with_extension("layout.json")
}

// --- .kicad_sch position extraction --------------------------------------

/// Map refdes → (x_mm, y_mm) read from each `(symbol …)` instance's
/// `(property "Reference" "<refdes>")` and `(at x y rot)`.
fn symbol_positions(sch: &Path) -> BTreeMap<String, (f64, f64)> {
    let text = std::fs::read_to_string(sch).expect("read sch");
    let root: Value = lexpr::from_str(&text).expect("parse sch");
    let mut out = BTreeMap::new();
    for sym in children(&root, "symbol") {
        let Some(refdes) = property(sym, "Reference") else {
            continue;
        };
        let Some(at) = find_child(sym, "at") else {
            continue;
        };
        let mut it = list_iter(at);
        it.next(); // head "at"
        let Some(x) = it.next().and_then(as_f64) else {
            continue;
        };
        let Some(y) = it.next().and_then(as_f64) else {
            continue;
        };
        out.insert(refdes, (x, y));
    }
    out
}

fn property(sym: &Value, key: &str) -> Option<String> {
    for p in children(sym, "property") {
        let mut it = list_iter(p);
        it.next(); // head
        let k = it.next().and_then(as_str)?;
        if k == key {
            return it.next().and_then(as_str).map(ToString::to_string);
        }
    }
    None
}

// --- minimal s-expr helpers (mirrors tests/common/sexp.rs) ---------------

fn head(v: &Value) -> Option<&str> {
    let first = list_iter(v).next()?;
    as_str(first)
}

fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    if let Some(it) = v.list_iter() {
        Box::new(it)
    } else {
        Box::new(std::iter::empty())
    }
}

fn children<'a>(root: &'a Value, name: &str) -> Vec<&'a Value> {
    list_iter(root)
        .filter(|c| c.is_list() && head(c) == Some(name))
        .collect()
}

fn find_child<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    children(v, name).into_iter().next()
}

fn as_f64(v: &Value) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))
}

fn as_str(v: &Value) -> Option<&str> {
    if let Some(s) = v.as_symbol() {
        return Some(s);
    }
    if let Some(s) = v.as_str() {
        return Some(s);
    }
    v.as_keyword()
}

// --- fixtures -------------------------------------------------------------

const BASE: &str = "\
* RC low-pass filter
*@symbol Device:R_US for=R*
*@symbol Device:C for=C*
V1 in  0   AC 1   ;@ ignore
R1 in  out 1k
C1 out 0   100n
.end
";

// BASE plus one extra resistor on the existing `out` net. The new R2
// shares nets with the original elements so it must be placed somewhere
// among them — the interesting stability case.
const PLUS_ONE: &str = "\
* RC low-pass filter
*@symbol Device:R_US for=R*
*@symbol Device:C for=C*
V1 in  0   AC 1   ;@ ignore
R1 in  out 1k
C1 out 0   100n
R2 out 0   2k
.end
";

// --- tests ----------------------------------------------------------------

#[test]
fn sidecar_written_on_every_run() {
    let dir = tempdir("written");
    let out = dir.join("rc.kicad_sch");
    convert(BASE, &out, false);

    let sc = sidecar_path(&out);
    assert!(
        sc.exists(),
        "sidecar must be written next to the .kicad_sch"
    );

    let text = std::fs::read_to_string(&sc).unwrap();
    let parsed = spice_layout::sidecar::Sidecar::from_json(&text).expect("sidecar parses");
    // Every placed element appears in the cache (V1 is ;@ ignore'd, so
    // only R1 + C1 are placed).
    assert!(parsed.positions.contains_key("R1"));
    assert!(parsed.positions.contains_key("C1"));
}

#[test]
fn no_layout_cache_opt_out_writes_nothing() {
    let dir = tempdir("optout");
    let out = dir.join("rc.kicad_sch");
    convert(BASE, &out, true);
    assert!(out.exists(), "schematic still emitted");
    assert!(
        !sidecar_path(&out).exists(),
        "--no-layout-cache must not write the sidecar"
    );
}

#[test]
fn original_elements_keep_position_when_one_is_added() {
    let dir = tempdir("stability");
    let out = dir.join("rc.kicad_sch");

    // First run: no sidecar yet. Records positions + writes the cache.
    convert(BASE, &out, false);
    let first = symbol_positions(&out);
    assert!(first.contains_key("R1") && first.contains_key("C1"));
    assert!(sidecar_path(&out).exists());

    // Second run with the SAME output path: the sidecar from run 1 is
    // present, so R1/C1 must keep their positions while R2 is newly
    // placed.
    convert(PLUS_ONE, &out, false);
    let second = symbol_positions(&out);

    assert!(second.contains_key("R2"), "new element placed");

    // Grid tolerance: positions are integer multiples of 1.27 mm; a
    // half-grid epsilon comfortably distinguishes "unchanged" from any
    // real move (the SA stride is whole cells).
    let tol = 1.27 / 2.0;
    for refdes in ["R1", "C1"] {
        let (x0, y0) = first[refdes];
        let (x1, y1) = second[refdes];
        assert!(
            (x0 - x1).abs() < tol && (y0 - y1).abs() < tol,
            "{refdes} moved across re-conversion: {:?} -> {:?}",
            (x0, y0),
            (x1, y1)
        );
    }

    // The new element must not land on top of an existing one. Symbol
    // origins must be distinct (no two symbols at the same coordinate).
    let r2 = second["R2"];
    for refdes in ["R1", "C1"] {
        let p = second[refdes];
        assert!(
            (r2.0 - p.0).abs() > tol || (r2.1 - p.1).abs() > tol,
            "R2 overlaps {refdes} at {p:?}"
        );
    }
}

#[test]
fn removing_an_element_drops_it_from_the_sidecar() {
    let dir = tempdir("removal");
    let out = dir.join("rc.kicad_sch");

    // Run with R2 present.
    convert(PLUS_ONE, &out, false);
    let sc = sidecar_path(&out);
    let with_r2 =
        spice_layout::sidecar::Sidecar::from_json(&std::fs::read_to_string(&sc).unwrap()).unwrap();
    assert!(with_r2.positions.contains_key("R2"));

    // Re-run without R2: it must vanish from the rewritten sidecar.
    convert(BASE, &out, false);
    let without_r2 =
        spice_layout::sidecar::Sidecar::from_json(&std::fs::read_to_string(&sc).unwrap()).unwrap();
    assert!(
        !without_r2.positions.contains_key("R2"),
        "removed element must drop out of the rewritten sidecar"
    );
    assert!(without_r2.positions.contains_key("R1"));
}
