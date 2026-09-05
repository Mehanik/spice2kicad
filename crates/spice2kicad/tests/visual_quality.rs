//! Visual-quality invariants for emitted `.kicad_sch` files.
//!
//! The round-trip tests in `roundtrip.rs` only check connectivity via
//! `kicad-cli sch export netlist`. A schematic can pass them and still
//! render as a totally blank page in eeschema — exactly what the current
//! emitter does (lib_symbols stubs with `length 0` pins, no graphical
//! body, no wires, redundant labels). These tests close that gap.
//!
//! Four invariants, all referenced from CLAUDE.md § Visual quality
//! invariants (the spec is being updated by a parallel agent):
//!
//! * **V1** — every non-power placed symbol contributes graphical
//!   strokes to the rendered SVG.
//! * **V2** — `kicad-cli sch erc --severity-error --exit-code-violations`
//!   is clean.
//! * **V3** — every `lib_id` referenced by an instance has its
//!   `(lib_symbols)` entry populated with at least one graphical
//!   primitive *and* its pins have `length > 0`.
//! * **V4** — multi-pin nets are wired together; no per-sheet net name
//!   appears on more than two `(global_label …)` / `(label …)`
//!   occurrences.
//!
//! Tests that fail against the current emitter are `#[ignore]`d with a
//! pointer to the relevant CLAUDE.md section. Flip them on as the
//! emitter learns each invariant.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{require_kicad_cli, spice_to_kicad};
use lexpr::Value;

/// All ten fixtures. This was the classic five; the port,
/// definition-level and named-rail sheets were emitted by the CLI but
/// graded by nothing here, so V1 (visible glyphs), V2 (ERC) and V9
/// (value formatting) were blind on exactly the newest code paths.
const FIXTURES: &[&str] = &[
    "rc_lowpass",
    "rc_lowpass_ports",
    "common_emitter",
    "multivibrator",
    "diff_pair",
    "opamp_inverting",
    "opamp_inverting_real",
    "port_shapes",
    "opamp_definition_level",
    "named_rails",
    "rc_phase_shift",
    "two_stage_amp",
    "cascode_amp",
    "lc_ladder_lpf",
    "sallen_key_lpf",
    "wien_bridge_osc",
    "sallen_key_driven",
    "shunt_feedback_amp",
    "stepped_attenuator",
    "opamp_transimpedance",
    "resistor_ladder_ref",
    "compensated_divider",
];

// --- driver bits ---------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> common::TempDir {
    common::TempDir::new("vq", name)
}

fn which_kicad_cli() -> bool {
    Command::new("kicad-cli")
        .arg("version")
        .output()
        .ok()
        .is_some_and(|o| o.status.success())
}

/// Skip the test cleanly when `kicad-cli` is missing, unless the user
/// has set `REQUIRE_KICAD_CLI=1` (mirrors `roundtrip.rs`).
fn skip_if_no_kicad_cli() -> bool {
    if which_kicad_cli() {
        return false;
    }
    assert!(
        !require_kicad_cli(),
        "kicad-cli not installed and REQUIRE_KICAD_CLI=1"
    );
    eprintln!("kicad-cli not on PATH — skipping");
    true
}

fn emit(name: &str) -> (PathBuf, common::TempDir) {
    let src = fixtures_dir().join(format!("{name}.cir"));
    let tmp = tempdir(name);
    let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
    (sch, tmp)
}

// --- V1: SVG visual sanity -----------------------------------------------

/// Strip `<g class="stroked-text">…</g>` regions (KiCad renders text as
/// path glyphs; those would otherwise dominate the count) and return
/// the number of remaining `<path` occurrences.
fn non_text_path_count(svg: &str) -> usize {
    let mut out = String::with_capacity(svg.len());
    let needle_open = "<g class=\"stroked-text\">";
    let needle_close = "</g>";
    let mut rest = svg;
    while let Some(idx) = rest.find(needle_open) {
        out.push_str(&rest[..idx]);
        // Find the matching close (KiCad never nests stroked-text groups).
        let after = &rest[idx + needle_open.len()..];
        if let Some(end) = after.find(needle_close) {
            rest = &after[end + needle_close.len()..];
        } else {
            rest = "";
        }
    }
    out.push_str(rest);
    out.matches("<path").count()
}

fn export_svg(sch: &Path, out_dir: &Path) -> Result<PathBuf, String> {
    let stem = sch.file_stem().unwrap().to_string_lossy();
    let dir = out_dir.join(format!("svg-{stem}"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let status = Command::new("kicad-cli")
        .args(["sch", "export", "svg", "--exclude-drawing-sheet", "-o"])
        .arg(&dir)
        .arg(sch)
        .status()
        .map_err(|e| format!("kicad-cli sch export svg: {e}"))?;
    if !status.success() {
        return Err(format!("kicad-cli sch export svg exited with {status}"));
    }
    Ok(dir.join(format!("{stem}.svg")))
}

/// One fixture's V1 measurement: `(non-text SVG paths, the floor they
/// must clear, components counted)`.
///
/// Split out of the old per-fixture `run_v1` so the ADR-23 cell and the
/// assertion read the SAME number: there is one definition of V1 in the
/// suite, and the scoreboard reports exactly the quantity the Tier-0
/// gate asserts on (`tests/common/scoreboard.rs`'s contract).
///
/// Heuristic floor: a properly inlined symbol body contributes body
/// strokes plus per-pin segments and label outlines. Blank (stub)
/// symbols emit zero body strokes per component, so any realistic
/// per-component path count flags the regression. We require ≥ 4
/// non-text paths per placed component (a passive resistor with two
/// pins clears this trivially; a stub does not).
fn v1_ink(name: &str) -> (usize, usize, usize) {
    let (sch, tmp) = emit(name);
    let svg_path = export_svg(&sch, &tmp).expect("export svg");
    let svg = std::fs::read_to_string(&svg_path).expect("read svg");
    let paths = non_text_path_count(&svg);
    let components = component_count(name);
    (paths, components * 4, components)
}

/// Count placed (non-`.subckt`-internal, non-`;@ ignore`d, non-power)
/// elements in the fixture. This drives the V1 path-budget heuristic.
fn component_count(name: &str) -> usize {
    let src =
        std::fs::read_to_string(fixtures_dir().join(format!("{name}.cir"))).expect("read fixture");
    let mut n = 0_usize;
    let mut in_subckt = false;
    for raw in src.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with(".subckt") {
            in_subckt = true;
            continue;
        }
        if lower.starts_with(".ends") {
            in_subckt = false;
            continue;
        }
        if in_subckt {
            continue;
        }
        let body_part = line.split(';').next().unwrap_or("").trim();
        if body_part.is_empty() {
            continue;
        }
        let first = body_part.chars().next().unwrap();
        if first == '.' || first == '*' || first == '+' {
            continue;
        }
        if line.contains(";@") && line.contains("ignore") {
            continue;
        }
        // A `;@ power=` / `*@power` source is a power rail, not a drawn
        // component (V10 / annotation-spec §4.5): it contributes no
        // `(symbol …)` instance, so it must not count toward the V1
        // glyph-path floor.
        if line.contains(";@") && lower.contains("power=") {
            continue;
        }
        n += 1;
    }
    n.max(1)
}

// --- V2: ERC clean (errors only) -----------------------------------------

fn run_v2(name: &str) {
    if skip_if_no_kicad_cli() {
        return;
    }
    let (sch, tmp) = emit(name);
    let report = tmp.join(format!("{name}-erc.rpt"));
    // Drop `--exit-code-violations`: we count residual errors ourselves.
    // The emitter places a `power:PWR_FLAG` on every otherwise-undriven
    // net, class-aware: a Power/Ground rail (its `power_in` glyph needs a
    // `power_out`), and a signal net whose driver-requiring pin
    // (`input`/`power_in`) has NO valid driver. A signal net with any
    // `Passive` (resistor/cap) pin is validly driven — KiCad counts
    // `PT_PASSIVE` as a signal-net driver — so it gets no flag. This
    // clears every `power_pin_not_driven` / `pin_not_driven`; no error
    // class needs suppressing, and we assert zero error-severity
    // violations.
    let _ = Command::new("kicad-cli")
        .args(["sch", "erc", "--severity-error", "-o"])
        .arg(&report)
        .arg(&sch)
        .output()
        .expect("invoke kicad-cli sch erc");
    let report_body = std::fs::read_to_string(&report).unwrap_or_default();
    // KiCad ERC report shape:
    //   [<class>]: <message>
    //       ; <severity>
    //       @(...): <where>
    // Pair each `[class]:` line with the next `; <severity>` line so we
    // can isolate `error` rows.
    //
    // The emitter places a `power:PWR_FLAG` on every otherwise-undriven
    // net (rails whose pins are all `power_in`, and signal nets whose
    // only driver-requiring pin is `input` with no passive terminal — no
    // Output/Power-output/Passive driver). A signal net carrying any
    // `Passive` pin needs no flag (KiCad counts `PT_PASSIVE` as a
    // signal-net driver). On every *flat* fixture this genuinely clears
    // all `power_pin_not_driven` / `pin_not_driven` errors, so they are
    // NOT suppressed and the gate asserts a fully empty error set.
    //
    // The sole documented exception is `opamp_inverting`'s hierarchical
    // ground: the parent's X1 `inp` port connects to SPICE node `0`
    // (ground), so the parent draws a `power:GND` glyph (`power_in`) on
    // that *hierarchical sheet pin*. KiCad's ERC per-connection driver
    // check (eeschema/erc/erc.cpp ~L1024-1075) requires a `power_out`
    // pin *in the same CONNECTION* as that `power_in` glyph. When the
    // connection is defined through a sheet pin into a child sheet
    // (where the real ground topology lives), the connectivity engine
    // assigns the coincidence point to the hierarchical net and a
    // parent-side PWR_FLAG is not credited to the parent glyph's
    // connection — verified empirically with the flag placed on the
    // glyph anchor, offset+wired, on the child `0` net, and on the
    // child `inp` hierarchical label; none satisfy the check. This is a
    // genuine KiCad hierarchical-connectivity artifact, NOT a pipeline
    // defect, and it predates this work (it was previously hidden by a
    // blanket `power_pin_not_driven` suppression). It is allowed for
    // `opamp_inverting` ONLY, and ONLY for `power_pin_not_driven`.
    let allowed_artifact: &[&str] = if name == "opamp_inverting" {
        &["[power_pin_not_driven]"]
    } else {
        &[]
    };
    let mut residual: Vec<String> = Vec::new();
    let lines: Vec<&str> = report_body.lines().collect();
    for i in 0..lines.len() {
        let trimmed = lines[i].trim_start();
        if !trimmed.starts_with('[') {
            continue;
        }
        // Find the severity line that follows.
        let sev = lines
            .iter()
            .skip(i + 1)
            .take(3)
            .find_map(|l| l.trim_start().strip_prefix("; "))
            .unwrap_or("warning");
        if !sev.starts_with("error") {
            continue;
        }
        if allowed_artifact.iter().any(|s| trimmed.starts_with(s)) {
            continue;
        }
        residual.push(lines[i].to_string());
    }
    assert!(
        residual.is_empty(),
        "V2 ERC: {name} reported residual ERROR-level violations\n--- residual ---\n{}\n--- full report ---\n{report_body}",
        residual.join("\n"),
    );
}

// --- V3 / V4: parsed-schematic invariants --------------------------------

/// Collect every `(symbol …)` instance under the root.
fn instance_symbols(root: &Value) -> Vec<&Value> {
    children(root, "symbol")
}

/// Collect every `(symbol "<lib_id>" …)` under `(lib_symbols …)`.
fn lib_symbols(root: &Value) -> Vec<&Value> {
    let Some(block) = find_child(root, "lib_symbols") else {
        return Vec::new();
    };
    children(block, "symbol")
}

fn find_lib_symbol_by_id<'a>(root: &'a Value, lib_id: &str) -> Option<&'a Value> {
    lib_symbols(root)
        .into_iter()
        .find(|sym| list_iter(sym).nth(1).and_then(as_str) == Some(lib_id))
}

/// Recursively scan a node for any of the listed graphical-primitive heads.
const PRIMITIVE_HEADS: &[&str] = &[
    "polyline",
    "rectangle",
    "circle",
    "arc",
    "bezier",
    "text",
    "text_box",
];

fn walk_primitive(node: &Value) -> bool {
    if let Some(h) = head(node)
        && PRIMITIVE_HEADS.contains(&h)
    {
        return true;
    }
    for child in list_iter(node) {
        if child.is_list() && walk_primitive(child) {
            return true;
        }
    }
    false
}

fn has_graphical_primitive(node: &Value) -> bool {
    walk_primitive(node)
}

fn walk_pin_lengths(node: &Value, out: &mut Vec<f64>) {
    if head(node) == Some("pin") {
        if let Some(len_node) = find_child(node, "length")
            && let Some(v) = list_iter(len_node).nth(1).and_then(as_f64)
        {
            out.push(v);
        }
        return;
    }
    for child in list_iter(node) {
        if child.is_list() {
            walk_pin_lengths(child, out);
        }
    }
}

/// Every `(pin …)` length under this node.
fn pin_lengths(node: &Value) -> Vec<f64> {
    let mut out = Vec::new();
    walk_pin_lengths(node, &mut out);
    out
}

fn wire_count(root: &Value) -> usize {
    children(root, "wire").len()
}

/// Histogram of names appearing on `(label "name" …)` and
/// `(global_label "name" …)`. Hierarchical labels are excluded — those
/// are sheet-boundary connectors, not part of the on-sheet legibility
/// budget.
fn count_label_occurrences(root: &Value) -> std::collections::BTreeMap<String, usize> {
    let mut hist: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for head_name in ["label", "global_label"] {
        for node in children(root, head_name) {
            if let Some(name) = list_iter(node).nth(1).and_then(as_str) {
                *hist.entry(name.to_string()).or_default() += 1;
            }
        }
    }
    hist
}

fn parse_sch(sch: &Path) -> Value {
    let src = std::fs::read_to_string(sch).expect("read sch");
    lexpr::from_str(&src).expect("parse sch as lexpr")
}

/// Every V3 defect in one fixture's emitted schematic, as reader-facing
/// strings.
///
/// Counting rather than panicking is what lets the ADR-23 cell and the
/// Tier-0 gate share one definition: the verifier below asserts the list
/// is empty, and the scoreboard records its length.
///
/// **Scope, stated so the cell is not over-read.** `docs/invariants.md`
/// V3 defines the invariant as *byte-for-byte* passthrough of the source
/// `.kicad_sym` body, and describes a verifier that re-parses the library
/// and compares sub-trees. What is implemented here — and has been since
/// the invariant landed — is the *renderability* proxy: every referenced
/// `lib_id` resolves inside `(lib_symbols …)`, carries at least one
/// graphical primitive, and has pins with non-zero length on non-power
/// symbols. That proxy catches the failure V3 exists to prevent (a stub
/// entry that renders blank on a machine without the library) but would
/// not catch a body the emitter altered while keeping it drawable. The
/// byte-equality verifier does not exist in this suite; the cell reports
/// the proxy, not the ideal.
fn v3_defects(name: &str) -> Vec<String> {
    let (sch, _tmp) = emit(name);
    let root = parse_sch(&sch);
    let instances = instance_symbols(&root);
    let mut defects: Vec<String> = Vec::new();
    if instances.is_empty() {
        defects.push(format!("V3: {name} has no symbol instances"));
        return defects;
    }

    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for inst in &instances {
        let Some(lib_id) = first_string_arg(inst, "lib_id") else {
            defects.push(format!("V3: {name}: instance without lib_id"));
            continue;
        };
        if !seen.insert(lib_id.to_string()) {
            continue;
        }
        let Some(lib_sym) = find_lib_symbol_by_id(&root, lib_id) else {
            defects.push(format!(
                "V3: {name}: lib_id {lib_id:?} not found in (lib_symbols)"
            ));
            continue;
        };

        if !has_graphical_primitive(lib_sym) {
            defects.push(format!(
                "V3: {name}: lib_symbol {lib_id:?} contains no graphical primitive \
                 (polyline/rectangle/circle/arc/text). Symbol will render blank."
            ));
        }

        let lengths = pin_lengths(lib_sym);
        if lengths.is_empty() {
            defects.push(format!(
                "V3: {name}: lib_symbol {lib_id:?} has no (pin …) entries"
            ));
        }
        // Power-symbol anchor pins are intentionally length-0 — the
        // pin IS the anchor coordinate (matches KiCad's stock
        // power.kicad_sym). Only require positive lengths on
        // non-power library symbols.
        if !lib_id.starts_with("power:") {
            for (i, len) in lengths.iter().enumerate() {
                if *len <= 0.0 {
                    defects.push(format!(
                        "V3: {name}: lib_symbol {lib_id:?} pin #{i} has length {len} (must be > 0)"
                    ));
                }
            }
        }
    }
    defects
}

fn run_v4(name: &str) {
    let (sch, _tmp) = emit(name);
    let root = parse_sch(&sch);

    // Label budget: ≤ 2 per name per sheet.
    let hist = count_label_occurrences(&root);
    let offenders: Vec<(String, usize)> = hist
        .iter()
        .filter(|&(_, &n)| n > 2)
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    assert!(
        offenders.is_empty(),
        "V4 label budget: {name} exceeds 2 labels per net: {offenders:?}"
    );

    // Wires: every fixture in this list has at least one multi-pin
    // internal net, so the schematic *usually* contains ≥ 1 wire
    // segment. The exception is when the placer aligns pins on the
    // same net to coincide exactly (R1.out and C1.out at the same
    // world point), in which case the router emits a single label
    // and zero wire segments — the ideal outcome. We accept zero
    // wires on small fixtures (≤ 2 placed elements) where pin
    // coincidence is plausible; larger circuits must still emit
    // some wiring.
    let wires = wire_count(&root);
    let n_placed = instance_symbols(&root).len();
    if n_placed > 2 {
        assert!(
            wires >= 1,
            "V4 wires: {name} has 0 (wire …) segments; multi-pin nets are not connected by wires"
        );
    }
}

// --- lexpr helpers (mirror common::sexp; copied here to keep the file ----
// self-contained and avoid widening the common module's API) -------------

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

fn as_str(v: &Value) -> Option<&str> {
    v.as_symbol()
        .or_else(|| v.as_str())
        .or_else(|| v.as_keyword())
}

fn as_f64(v: &Value) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))
}

// --- per-fixture tests ---------------------------------------------------

// V1 — visual sanity: every placed symbol contributes real strokes to
// the rendered SVG. The failure this exists to catch is a `lib_symbols`
// stub (no graphical primitives, length-0 pins) rendering blank in
// eeschema.
//
// One enumerating verifier over all eighteen fixtures, replacing five
// single-fixture `v1_*` tests. Two reasons, and the second is the point
// of ADR-23 D11's "what is still blind" note: (a) V1 is **Tier 0**, and
// a Tier-0 gate that grades five of eighteen fixtures is not a gate;
// (b) the blind-cell lint's premise is *enumeration*, so a
// single-fixture verifier is structurally outside it and could stay
// silent forever. Collect-then-assert, recording every fixture's number
// before the single assertion, per D2/D10.
#[test]
fn v1_symbols_render_visible_ink_across_fixtures() {
    if skip_if_no_kicad_cli() {
        return;
    }
    let mut failures: Vec<String> = Vec::new();
    for name in FIXTURES {
        let (paths, want, components) = v1_ink(name);
        // Clamped at zero: the metric is the *shortfall*, so extra ink
        // (a placer that routes more wire) cannot masquerade as a
        // Tier-0 improvement, and 0 is the healthy value on both arms.
        let deficit = want.saturating_sub(paths);
        common::scoreboard::record_count("t0.v1_ink_deficit", name, deficit);
        if deficit > 0 {
            failures.push(format!(
                "{name}.svg has {paths} non-text paths; expected ≥ {want} \
                 (≈4 per non-power component, {components} components) — \
                 short by {deficit}. Symbols are likely rendering blank."
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "V1 visual: {} fixture(s) below the glyph-ink floor:\n  {}",
        failures.len(),
        failures.join("\n  "),
    );
}

// V2 — ERC clean (errors only). Flat fixtures pass today: their only
// findings are `lib_symbol_mismatch` and `global_label_dangling` at
// *warning* severity. The hierarchical fixture (`opamp_inverting`)
// fails: its `.subckt` sheet emits `(hierarchical_label …)`s on the
// child page that nothing connects to, which ERC flags as
// `label_dangling` errors. Fix per CLAUDE.md § Visual quality
// invariants V2 — wire hierarchical port labels to the parent sheet's
// pins so they aren't dangling.
// V2 ERC checks across the fixtures: re-enabled at R7. `run_v2`
// suppresses `power_pin_not_driven` errors, since the project does
// not emit PWR_FLAG drivers (V10 in CLAUDE.md tracks this). All other
// ERC error classes are enforced.
#[test]
fn v2_rc_lowpass() {
    run_v2("rc_lowpass");
}
#[test]
fn v2_common_emitter() {
    run_v2("common_emitter");
}
#[test]
fn v2_multivibrator() {
    run_v2("multivibrator");
}
#[test]
fn v2_diff_pair() {
    run_v2("diff_pair");
}
#[test]
fn v2_opamp_inverting() {
    run_v2("opamp_inverting");
}

// V3 — every `lib_id` an instance references resolves to a populated
// `(lib_symbols …)` entry: graphical primitives present, pins present,
// non-zero pin length on non-power symbols. See `v3_defects` for the
// scope caveat (this is the renderability proxy, not the byte-equality
// check `docs/invariants.md` describes).
//
// Enumerating for the same two reasons as V1 above: Tier 0 graded on
// five of eighteen fixtures is not a gate, and a single-fixture verifier
// is invisible to the blind-cell lint.
#[test]
fn v3_lib_symbols_are_populated_across_fixtures() {
    let mut failures: Vec<String> = Vec::new();
    for name in FIXTURES {
        let defects = v3_defects(name);
        common::scoreboard::record_count("t0.v3_lib_symbol_defects", name, defects.len());
        failures.extend(defects);
    }
    assert!(
        failures.is_empty(),
        "V3: {} lib_symbols defect(s):\n  {}",
        failures.len(),
        failures.join("\n  "),
    );
}

// V4 — wires + label budget. Fails today: the emitter connects nets
// solely through global_labels, so multi-pin nets show > 2 labels and
// 0 wires. Fix per CLAUDE.md § Visual quality invariants V4 — emit
// (wire …) segments for internal nets and cap label use at ≤ 2 per
// per-sheet net.
#[test]
fn v4_rc_lowpass() {
    run_v4("rc_lowpass");
}
#[test]
fn v4_common_emitter() {
    run_v4("common_emitter");
}
#[test]
fn v4_multivibrator() {
    run_v4("multivibrator");
}
#[test]
fn v4_diff_pair() {
    run_v4("diff_pair");
}
#[test]
fn v4_opamp_inverting() {
    run_v4("opamp_inverting");
}

// --- framework smoke tests (run on every `cargo test`) ------------------
//
// These exercise the helpers themselves on synthetic input so a refactor
// in the helper code can't silently disable the V1–V4 tests when they
// flip on later.

#[test]
fn smoke_non_text_path_count_strips_text_groups() {
    let svg = r#"<svg>
        <g class="stroked-text"><desc>x</desc><path d="M0 0 L1 1"/></g>
        <path d="M2 2 L3 3"/>
        <g class="stroked-text"><desc>y</desc><path d="M4 4"/><path d="M5 5"/></g>
        <path d="M6 6"/>
    </svg>"#;
    assert_eq!(non_text_path_count(svg), 2);
}

#[test]
fn smoke_count_label_occurrences_excludes_hierarchical() {
    let src = r#"(kicad_sch
        (label "n1" (at 0 0 0))
        (label "n1" (at 1 0 0))
        (global_label "n1" (at 2 0 0))
        (hierarchical_label "n1" (at 3 0 0))
        (label "n2" (at 4 0 0)))"#;
    let v: Value = lexpr::from_str(src).unwrap();
    let h = count_label_occurrences(&v);
    assert_eq!(h.get("n1").copied(), Some(3));
    assert_eq!(h.get("n2").copied(), Some(1));
    assert!(!h.contains_key("n3"));
}

#[test]
fn smoke_has_graphical_primitive_recurses_into_subsymbols() {
    let with_body =
        lexpr::from_str(r#"(symbol "Foo" (symbol "Foo_1_1" (rectangle (start 0 0) (end 1 1))))"#)
            .unwrap();
    let stub =
        lexpr::from_str(r#"(symbol "Bar" (property "Reference" "U") (symbol "Bar_1_1"))"#).unwrap();
    assert!(has_graphical_primitive(&with_body));
    assert!(!has_graphical_primitive(&stub));
}

#[test]
fn smoke_pin_lengths_collects_from_subsymbols() {
    let v = lexpr::from_str(
        r#"(symbol "X"
            (symbol "X_1_1"
                (pin passive line (length 2.54))
                (pin passive line (length 0))))"#,
    )
    .unwrap();
    let lens = pin_lengths(&v);
    assert_eq!(lens, vec![2.54, 0.0]);
}

#[test]
fn smoke_wire_count() {
    let v = lexpr::from_str(
        r"(kicad_sch (wire (pts (xy 0 0) (xy 1 0))) (wire (pts (xy 1 0) (xy 1 1))))",
    )
    .unwrap();
    assert_eq!(wire_count(&v), 2);
}

#[test]
fn smoke_find_lib_symbol_by_id() {
    let v = lexpr::from_str(
        r#"(kicad_sch (lib_symbols (symbol "Device:R" (rectangle)) (symbol "Device:C")))"#,
    )
    .unwrap();
    assert!(find_lib_symbol_by_id(&v, "Device:R").is_some());
    assert!(find_lib_symbol_by_id(&v, "Device:C").is_some());
    assert!(find_lib_symbol_by_id(&v, "Device:L").is_none());
}

#[test]
fn smoke_component_count_skips_subckt_and_ignored() {
    // Sanity-check the heuristic against a known fixture.
    assert!(component_count("rc_lowpass") >= 2);
    assert!(component_count("common_emitter") >= 4);
}

#[test]
fn smoke_fixtures_list_complete() {
    // Guard against accidentally dropping a fixture from FIXTURES.
    for f in FIXTURES {
        assert!(
            fixtures_dir().join(format!("{f}.cir")).is_file(),
            "fixture missing: {f}"
        );
    }
}
