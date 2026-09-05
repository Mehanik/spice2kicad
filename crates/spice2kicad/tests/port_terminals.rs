//! `*@port` directional-terminal emission (spec §4, readability / V4).
//!
//! A declared `*@port <net>=<dir>` must emit a directional
//! `(global_label "<net>" (shape input|output|bidirectional) …)` that
//! REPLACES the net's plain/global label (V4: ≤ 1 label per net — never
//! a directional terminal *and* a plain label on the same net). This is
//! the explicit-annotation fix for the in/out asymmetry: an output net
//! that today emits a plain 2-pin `(label …)` (e.g. rc_lowpass `out`)
//! becomes a right-facing output terminal.
//!
//! The shape tests were written RED before the feature landed:
//!  * `rc_lowpass_ports` `out` emitted a plain `(label "out")`;
//!  * `port_shapes` ni/no/nb emitted plain `(label …)`.
//!
//! The zero-annotation guard and the ERC guard were GREEN on master and
//! must stay green (they protect the un-annotated path and V2).

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{require_kicad_cli, spice_to_kicad};
use lexpr::Value;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> common::TempDir {
    common::TempDir::new("ports", name)
}

fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    match v.list_iter() {
        Some(it) => Box::new(it),
        None => Box::new(std::iter::empty()),
    }
}

fn head(v: &Value) -> Option<&str> {
    list_iter(v).next().and_then(|h| h.as_symbol())
}

fn as_str(v: &Value) -> Option<&str> {
    v.as_str().or_else(|| v.as_symbol())
}

fn find_child<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    list_iter(v).find(|c| head(c) == Some(name))
}

/// Convert `<name>.cir` and return the parsed `.kicad_sch` root plus the
/// emitted file. The [`common::Emitted`] carries its temp directory, so
/// binding it (even as `_sch`) keeps the file on disk for a follow-up
/// ERC pass; dropping it deletes the directory.
fn convert(name: &str) -> (Value, common::Emitted) {
    let src = fixtures_dir().join(format!("{name}.cir"));
    let tmp = tempdir(name);
    let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
    let text = std::fs::read_to_string(&sch).expect("read sch");
    let root = lexpr::from_str(&text).expect("parse sch as lexpr");
    (root, common::Emitted::new(tmp, sch))
}

/// Every `(<kind> "<net>" … (shape <s>)? …)` in the sheet, as
/// `(net, shape)` where `shape` is `None` for a plain `(label …)`.
fn labels_with_shape(root: &Value, kind: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for item in list_iter(root) {
        if head(item) != Some(kind) {
            continue;
        }
        let Some(name) = list_iter(item).nth(1).and_then(as_str) else {
            continue;
        };
        let shape = find_child(item, "shape")
            .and_then(|s| list_iter(s).nth(1))
            .and_then(as_str)
            .map(str::to_owned);
        out.push((name.to_owned(), shape));
    }
    out
}

/// Count every label of any kind (`label` + `global_label`) on `net`.
fn total_labels_on(root: &Value, net: &str) -> usize {
    ["label", "global_label"]
        .iter()
        .map(|k| {
            labels_with_shape(root, k)
                .into_iter()
                .filter(|(n, _)| n == net)
                .count()
        })
        .sum()
}

/// The one `(shape …)` on the single `global_label` for `net`, or a
/// panic if there isn't exactly one such global label.
fn output_shape_of(root: &Value, net: &str) -> Option<String> {
    let mut globals: Vec<_> = labels_with_shape(root, "global_label")
        .into_iter()
        .filter(|(n, _)| n == net)
        .collect();
    assert_eq!(
        globals.len(),
        1,
        "expected exactly one global_label for {net}; got {globals:?}"
    );
    globals.remove(0).1
}

// ─── Directional terminal emission (RED until the feature lands) ─────────────

#[test]
fn declared_output_net_emits_output_shape() {
    // rc_lowpass `out` — a 2-pin internal net that today emits a plain
    // `(label "out")` — must become `(global_label "out" (shape output))`.
    let (root, _sch) = convert("rc_lowpass_ports");
    assert_eq!(
        output_shape_of(&root, "out").as_deref(),
        Some("output"),
        "declared output net must emit (shape output)"
    );
}

#[test]
fn declared_output_net_replaces_plain_label_v4() {
    // V4: exactly one label of any kind on the net — the directional
    // terminal REPLACES the plain label; the emitter must not draw both.
    let (root, _sch) = convert("rc_lowpass_ports");
    let plain_out: Vec<_> = labels_with_shape(&root, "label")
        .into_iter()
        .filter(|(n, _)| n == "out")
        .collect();
    assert!(
        plain_out.is_empty(),
        "declared output net must not also carry a plain (label \"out\"): {plain_out:?}"
    );
    assert_eq!(
        total_labels_on(&root, "out"),
        1,
        "V4: exactly one label of any kind on the out net"
    );
}

#[test]
fn declared_input_net_emits_input_shape() {
    let (root, _sch) = convert("rc_lowpass_ports");
    assert_eq!(
        output_shape_of(&root, "in").as_deref(),
        Some("input"),
        "declared input net must emit (shape input)"
    );
    assert_eq!(total_labels_on(&root, "in"), 1, "V4: one label on in");
}

#[test]
fn declared_directions_map_to_shapes() {
    // port_shapes declares ni=input, no=output, nb=bidir on three 2-pin
    // internal nets that all emit plain `(label …)` today. Each must
    // become the corresponding directional terminal, plain label gone.
    let (root, _sch) = convert("port_shapes");
    for (net, want) in [("ni", "input"), ("no", "output"), ("nb", "bidirectional")] {
        assert_eq!(
            output_shape_of(&root, net).as_deref(),
            Some(want),
            "port {net} must emit (shape {want})"
        );
        let plain: Vec<_> = labels_with_shape(&root, "label")
            .into_iter()
            .filter(|(n, _)| n == net)
            .collect();
        assert!(
            plain.is_empty(),
            "port {net} must replace its plain label (V4): {plain:?}"
        );
        assert_eq!(total_labels_on(&root, net), 1, "V4: one label on {net}");
    }
}

// ─── Zero-annotation invariance (GREEN guard: must stay passing) ─────────────

#[test]
fn zero_annotation_label_kinds_unchanged() {
    // Core principle 2: an un-annotated fixture behaves EXACTLY as today.
    // The wire/junction *ordering* is nondeterministic run-to-run, so a
    // raw byte-compare would be flaky; the stable, meaningful property is
    // the label-KIND assigned to each net. Un-annotated rc_lowpass keeps
    // `in` a one-pin interface (global_label, shape input) and `out` a
    // plain 2-pin `(label …)`. The `*@port` feature must never leak a
    // directional shape into a file that declares no port.
    let (root, _sch) = convert("rc_lowpass");
    let plain: Vec<String> = labels_with_shape(&root, "label")
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    let globals = labels_with_shape(&root, "global_label");
    assert!(
        plain.contains(&"out".to_owned()),
        "un-annotated `out` must stay a plain label; plain={plain:?} globals={globals:?}"
    );
    assert!(
        globals
            .iter()
            .any(|(n, s)| n == "in" && s.as_deref() == Some("input")),
        "un-annotated `in` must stay (global_label … (shape input)); got {globals:?}"
    );
    for (n, s) in globals
        .iter()
        .chain(labels_with_shape(&root, "label").iter())
    {
        assert!(
            s.as_deref() != Some("output") && s.as_deref() != Some("bidirectional"),
            "un-annotated fixture leaked a directional shape on {n}: {s:?}"
        );
    }
}

// ─── ERC stays clean with ports declared (V2 — GREEN guard) ──────────────────

/// Run `kicad-cli sch erc` at error severity. `None` ⇒ kicad-cli is not
/// installed (caller may skip); `Some(n)` ⇒ n error-severity violations.
fn run_erc(sch: &Path, out_dir: &Path) -> Option<usize> {
    let report = out_dir.join("erc.rpt");
    let output = Command::new("kicad-cli")
        .args([
            "sch",
            "erc",
            "--severity-error",
            "--exit-code-violations",
            "-o",
        ])
        .arg(&report)
        .arg(sch)
        .output()
        .ok()?;
    // `--exit-code-violations` returns nonzero iff there were violations
    // at the requested severity; parse the report to report the count.
    let body = std::fs::read_to_string(&report).unwrap_or_default();
    let violations = body
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("Found ")
                .and_then(|r| r.strip_suffix(" violations"))
                .and_then(|n| n.trim().parse::<usize>().ok())
        })
        .unwrap_or(usize::from(!output.status.success()));
    Some(violations)
}

#[test]
fn erc_clean_on_port_annotated_fixtures() {
    // Declaring a port is a readability + position marker, not an ERC
    // driver: it must not add/remove PWR_FLAGs or otherwise disturb ERC.
    for name in [
        "rc_lowpass_ports",
        "port_shapes",
        "rc_lowpass",
        "rc_phase_shift",
        "two_stage_amp",
        "cascode_amp",
        "lc_ladder_lpf",
        "sallen_key_lpf",
        "wien_bridge_osc",
        "sallen_key_driven",
        "shunt_feedback_amp",
        "opamp_transimpedance",
        "resistor_ladder_ref",
        "compensated_divider",
    ] {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        match run_erc(&sch, &tmp) {
            None => {
                assert!(
                    !require_kicad_cli(),
                    "kicad-cli not installed and REQUIRE_KICAD_CLI=1"
                );
                eprintln!("kicad-cli not on PATH — skipping ERC check");
                return;
            }
            Some(n) => assert_eq!(n, 0, "{name}: {n} ERC error-severity violations"),
        }
    }
}
