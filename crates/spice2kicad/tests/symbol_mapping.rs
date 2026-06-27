//! V8 — Standard symbol mapping for `.subckt` instances.
//!
//! These tests pin the contract described in CLAUDE.md § Visual
//! quality invariants V8 and `docs/annotation-spec.md` §4.1
//! ("Targeting `.subckt` instances"): when `*@symbol` targets an
//! `X<n>` instance (trailing tag or `for=X<n>` block form), the
//! emitter must place the named library symbol on the parent
//! schematic *instead of* lowering the matching `.subckt` body to a
//! hierarchical sheet.
//!
//! Today the resolver promotes every top-level `X<n>` whose subckt is
//! defined in the file to a `SheetInstance` before per-element
//! symbol resolution runs (`crates/spice-resolve/src/lib.rs`,
//! `has_explicit_symbol_tag`). The block form `*@symbol … for=X1`
//! is therefore *not* honoured for the sheet-vs-symbol decision —
//! that gap is what V8 closes. The tests below are `#[ignore]`d
//! until the resolver / emitter learns the override.
//!
//! All tests are gated on the `Amplifier_Operational` fixture
//! library at `crates/kicad-symbols/tests/fixtures/`, which carries
//! a minimal generic `OPAMP` triangle (5 pins on the 1.27 mm grid).

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use lexpr::Value;

// --- driver bits ---------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn lib_dir() -> PathBuf {
    workspace_root().join("crates/kicad-symbols/tests/fixtures")
}

fn tempdir(name: &str) -> PathBuf {
    // Unique per `emit()` call, not per fixture: several tests convert the
    // same fixture (e.g. `opamp_inverting_real`) and run concurrently, so a
    // dir keyed only by fixture name races — one test's `remove_dir_all`
    // wipes another's freshly-written `.kicad_sch` between write and read.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spice2kicad-sm-{pid}-{seq}-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

/// Run `spice2kicad` against a fixture with the three test fixture
/// libraries loaded (Device, Simulation_SPICE, Amplifier_Operational).
fn emit(name: &str) -> (PathBuf, PathBuf) {
    let src = fixtures_dir().join(format!("{name}.cir"));
    let tmp = tempdir(name);
    let out = tmp.join(format!("{name}.kicad_sch"));
    let bin = env!("CARGO_BIN_EXE_spice2kicad");
    let libs = lib_dir();
    let status = Command::new(bin)
        .arg(&src)
        .arg("-t")
        .arg("schematic")
        .arg("-o")
        .arg(&out)
        .arg("-l")
        .arg(libs.join("Device.kicad_sym"))
        .arg("-l")
        .arg(libs.join("Simulation_SPICE.kicad_sym"))
        .arg("-l")
        .arg(libs.join("Amplifier_Operational.kicad_sym"))
        .status()
        .expect("invoke spice2kicad");
    assert!(status.success(), "spice2kicad exited with {status}");
    (out, tmp)
}

fn parse_sch(path: &Path) -> Value {
    let src = std::fs::read_to_string(path).expect("read sch");
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
    list_iter(v).next().and_then(as_str)
}

fn as_str(v: &Value) -> Option<&str> {
    v.as_symbol()
        .or_else(|| v.as_str())
        .or_else(|| v.as_keyword())
}

fn children<'a>(v: &'a Value, name: &str) -> Vec<&'a Value> {
    list_iter(v)
        .filter(|c| c.is_list() && head(c) == Some(name))
        .collect()
}

fn find_child<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    children(v, name).into_iter().next()
}

fn first_string_arg<'a>(v: &'a Value, name: &str) -> Option<&'a str> {
    let node = find_child(v, name)?;
    list_iter(node).nth(1).and_then(as_str)
}

/// All `(symbol …)` instances directly under root (i.e. placed
/// instances, not lib_symbols entries).
fn instance_symbols(root: &Value) -> Vec<&Value> {
    children(root, "symbol")
}

/// Look up a property value (e.g. `Reference`, `Value`) on a placed
/// `(symbol …)` instance.
fn property_value<'a>(inst: &'a Value, key: &str) -> Option<&'a str> {
    for prop in children(inst, "property") {
        let mut it = list_iter(prop);
        let _ = it.next(); // "property"
        if it.next().and_then(as_str) == Some(key) {
            return it.next().and_then(as_str);
        }
    }
    None
}

// --- V8 framework smoke --------------------------------------------------

/// Sanity check the test driver: the fixture parses, the emitter
/// runs, and the parent `.kicad_sch` exists. Keeps the V8 tests
/// honest if the driver itself regresses.
#[test]
fn v8_driver_smoke_emits_parent_schematic() {
    let (sch, _tmp) = emit("opamp_inverting_real");
    assert!(sch.exists(), "parent .kicad_sch was not written");
    let root = parse_sch(&sch);
    assert_eq!(
        head(&root),
        Some("kicad_sch"),
        "root node must be (kicad_sch …)"
    );
}

// --- V8 contract tests (ignored until the resolver/emitter ships) -------

#[test]
fn v8_opamp_inverting_real_emits_symbol_not_sheet() {
    let (sch, tmp) = emit("opamp_inverting_real");
    let root = parse_sch(&sch);

    // (a) the parent contains a placed (symbol …) with the requested lib_id.
    let opamp_lib_id = "Amplifier_Operational:OPAMP";
    let opamp_instances: Vec<&Value> = instance_symbols(&root)
        .into_iter()
        .filter(|inst| first_string_arg(inst, "lib_id") == Some(opamp_lib_id))
        .collect();
    assert_eq!(
        opamp_instances.len(),
        1,
        "V8: expected exactly one (symbol …) instance with lib_id {opamp_lib_id:?} on the parent sheet"
    );

    // … and that instance is X1.
    let refdes = property_value(opamp_instances[0], "Reference");
    assert_eq!(
        refdes,
        Some("X1"),
        "V8: opamp instance must carry refdes X1, got {refdes:?}"
    );

    // (b) NO (sheet …) block on the parent.
    let sheets = children(&root, "sheet");
    assert!(
        sheets.is_empty(),
        "V8: parent schematic still contains {} (sheet …) block(s); \
         the OPAMP subckt must not be lowered to a hierarchical sheet \
         when X1 has an explicit *@symbol override",
        sheets.len()
    );

    // (c) NO child OPAMP.kicad_sch alongside the parent.
    let child = tmp.join("OPAMP.kicad_sch");
    assert!(
        !child.exists(),
        "V8: child sheet file {child:?} should not be written when X1 \
         is rendered as a flat symbol"
    );
}

#[test]
fn v8_opamp_inverting_real_pin_connectivity() {
    let (sch, _tmp) = emit("opamp_inverting_real");
    let root = parse_sch(&sch);

    // Locate the placed OPAMP symbol.
    let opamp_lib_id = "Amplifier_Operational:OPAMP";
    let opamp = instance_symbols(&root)
        .into_iter()
        .find(|inst| first_string_arg(inst, "lib_id") == Some(opamp_lib_id))
        .expect("V8: opamp instance not present");

    // Every pin on the symbol must be reachable from one of the
    // parent-sheet nets X1 references in SPICE: `0`, `inv`, `out`,
    // `vcc`, `vee` (in port order inp, inn, out, vcc, vee, but with
    // the pinmap inp:3, inn:2, out:1, vcc:8, vee:4).
    //
    // We don't yet have a routed-wire model in this test crate; the
    // operational check is "every pin world-position is the endpoint
    // of at least one (wire …) segment OR carries a (label …) /
    // (global_label …) on one of the expected names". Both are
    // V8-acceptable per the V4 budget. The exact pin world positions
    // depend on the symbol body (5 pins at ±7.62/±2.54 offsets from
    // the instance origin) and the placer's chosen rotation; we
    // assert the *count* here and leave per-pin geometry to the V8
    // implementation work.
    let wires = children(&root, "wire");
    let labels: Vec<&Value> = children(&root, "label")
        .into_iter()
        .chain(children(&root, "global_label"))
        .collect();
    let connectors = wires.len() + labels.len();
    assert!(
        connectors >= 5,
        "V8: opamp instance has 5 pins but parent sheet has only \
         {connectors} wire/label connectors — pins are not connected \
         to RIN/RF/VCC/VEE/GND nets. Found {} wires, {} labels.",
        wires.len(),
        labels.len()
    );

    // Sanity-check the symbol carries the expected refdes too —
    // protects against a future regression where some other library
    // symbol picks up `Amplifier_Operational:OPAMP` by accident.
    assert_eq!(property_value(opamp, "Reference"), Some("X1"));
}

// --- definition-level subckt symbol (spec §4.1 / §4.2) -----------------

/// A single definition-level `;@ symbol=` + `;@ pinmap=` on the
/// `.subckt OPAMP …` header makes EVERY `X` instance emit the flat
/// OPAMP triangle — no per-instance tags, no hierarchical sheet (V8).
#[test]
fn definition_level_symbol_inherited_by_all_instances() {
    let (sch, tmp) = emit("opamp_definition_level");
    let root = parse_sch(&sch);

    let opamp_lib_id = "Amplifier_Operational:OPAMP";
    let opamps: Vec<&Value> = instance_symbols(&root)
        .into_iter()
        .filter(|inst| first_string_arg(inst, "lib_id") == Some(opamp_lib_id))
        .collect();
    assert_eq!(
        opamps.len(),
        2,
        "both X1 and X2 must inherit the definition-level OPAMP symbol"
    );

    let mut refs: Vec<&str> = opamps
        .iter()
        .filter_map(|inst| property_value(inst, "Reference"))
        .collect();
    refs.sort_unstable();
    assert_eq!(refs, vec!["X1", "X2"]);

    // No hierarchical sheet, and no child OPAMP.kicad_sch.
    let sheets = children(&root, "sheet");
    assert!(
        sheets.is_empty(),
        "definition-annotated subckt must not lower to a sheet; found {}",
        sheets.len()
    );
    assert!(
        !tmp.join("OPAMP.kicad_sch").exists(),
        "no child OPAMP.kicad_sch should be written"
    );
}

// --- default pinmap regression (V11) -----------------------------------

#[test]
fn bjt_default_pinmap_uses_pin_names() {
    // Regression for the V11-violating positional zip: SPICE BJT order
    // is (C, B, E) and Device:Q_NPN_BCE numbers pins B=1, C=2, E=3.
    // Without an explicit pinmap, the resolver must still map
    // SPICE term 1 (collector) → KiCad pin "2", term 2 → "1", term 3
    // → "3".
    use kicad_symbols::Library;
    use spice_diagnostics::FileId;
    use spice_resolve::resolve;

    let libs_dir = lib_dir();
    let library = {
        let device =
            Library::from_file(libs_dir.join("Device.kicad_sym")).expect("parse Device.kicad_sym");
        let sim = Library::from_file(libs_dir.join("Simulation_SPICE.kicad_sym"))
            .expect("parse Simulation_SPICE.kicad_sym");
        device.merge(sim)
    };

    let source = "* default pinmap regression\n\
                  *@symbol Device:Q_NPN_BCE for=Q*\n\
                  Q1 c b e QGENERIC\n\
                  V1 c 0 5\n\
                  V2 b 0 1\n\
                  V3 e 0 0\n\
                  .end\n";

    let parsed = spice_parser::parse(source, FileId(0)).expect("parse ok");
    let resolved = resolve(&parsed.netlist, &library).expect("resolve ok");
    let q1 = resolved
        .elements
        .iter()
        .find(|e| e.refdes == "Q1")
        .expect("Q1 in resolved netlist");
    assert_eq!(
        q1.pin_mapping,
        vec!["2".to_owned(), "1".to_owned(), "3".to_owned()],
        "SPICE (C, B, E) must map to KiCad pin numbers (2, 1, 3) by name"
    );
}
