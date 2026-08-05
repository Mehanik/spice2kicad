//! **P10 / P11 — placement stability.**
//!
//! # P10 — determinism
//!
//! Two cache-less conversions of the same netlist produce byte-identical
//! output. This is the weaker half and it **already passes on master**:
//! the SA is seeded deterministically, so the run-to-run variance ADR-17
//! worried about does not exist. The ADR-17 design review expected this
//! test to need `#[ignore]` until the SA retired; measurement said
//! otherwise, so it landed live and guards the property from here on.
//!
//! Note what it does NOT say. Determinism is reproducibility of *one*
//! input, which is orthogonal to sensitivity: a chaotic map is perfectly
//! deterministic and still re-bases globally on the smallest input
//! change.
//!
//! # P11 — cache-path stability
//!
//! P11 originally asserted **basin locality**: adding ONE element to a
//! netlist must move only the poses near it. It was `#[ignore]`d with
//! budgets of 0 against a measured blast radius of 5/5 and 17/17, on
//! the theory (ADR-17) that the SA caused it and determinism would cure
//! it.
//!
//! **That theory is falsified, twice over, and the old P11 is deleted.**
//! ADR-17 Stage 2 measured a deterministic order-preserving compaction
//! at 5/5 and 16/17 — no better. The control that settles it is the bare
//! deterministic seed (`--no-refine`: no SA, no compaction at all),
//! which moves **17/17** on `common_emitter`+1C and **5/5** on
//! `rc_lowpass`+1R — the same as the SA. Global re-basing is intrinsic
//! to any *spacing-derived* placement: classify→bands→layers re-derives
//! strides from global structure, so one insertion re-spaces its column
//! and every coordinate derived after it. **Determinism is not
//! locality**, and no budget-0 locality target is reachable by this
//! architecture. Leaving such a target `#[ignore]`d in the suite was
//! worse than deleting it. See ADR-17's RETIRED record.
//!
//! What replaces it is the property that is both *achievable* and the
//! one users actually experience: **cache-path stability**. Convert a
//! netlist; edit it (add an element); re-convert into the *same* output
//! directory so the ADR-4 layout-cache sidecar is read. Every
//! pre-existing user symbol must keep its exact pose, and the result
//! must still pass the CLI's post-emit connectivity check. That makes an
//! edit's schematic diff attributable to the edit — which was ADR-17's
//! stated primary product, already delivered by the cache for the
//! workflow that needs it.
//!
//! (Developers changing *placer code* can never get per-fixture locality
//! from any spacing-derived algorithm; that workflow is governed by
//! ADR-16's two-instrument protocol, not by this test.)

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::text_model::{Bbox, Pt, TextKind, text_bbox};
use kicad_symbols::{Orientation, Rotation};
use lexpr::Value;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spice2kicad-stability-{pid}-{seq}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

/// Convert with the position-stability cache DISABLED.
///
/// Load-bearing: with the cache on, a re-conversion into a used
/// directory pins every element to its saved position, which would make
/// both tests here trivially pass while measuring nothing (and makes
/// phase 4.5 a silent no-op).
fn convert_no_cache(src: &Path, out_dir: &Path) -> PathBuf {
    let stem = src.file_stem().unwrap().to_string_lossy();
    let out = out_dir.join(format!("{stem}.kicad_sch"));
    convert(src, &out, true);
    out
}

/// Run the CLI. `no_cache` selects the `--no-layout-cache` opt-out; with
/// it *off* the converter reads/writes `<basename>.layout.json` next to
/// `out`, which is the ADR-4 path P11 exercises.
///
/// The process exit status is load-bearing: the CLI runs a post-emit
/// connectivity check through `kicad-cli` by default and returns a
/// non-zero status when the emitted schematic does not wire up the
/// source netlist. Asserting success therefore *is* the connectivity
/// assertion (vacuous only if `kicad-cli` is not installed).
fn convert(src: &Path, out: &Path, no_cache: bool) {
    let bin = env!("CARGO_BIN_EXE_spice2kicad");
    let lib_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("crates/kicad-symbols/tests/fixtures");
    let mut cmd = Command::new(bin);
    cmd.arg(src)
        .arg("-t")
        .arg("schematic")
        .arg("-o")
        .arg(out)
        .arg("-l")
        .arg(lib_dir.join("Device.kicad_sym"))
        .arg("-l")
        .arg(lib_dir.join("Simulation_SPICE.kicad_sym"))
        .arg("-l")
        .arg(lib_dir.join("Amplifier_Operational.kicad_sym"))
        .arg("-l")
        .arg(lib_dir.join("power.kicad_sym"));
    if no_cache {
        cmd.arg("--no-layout-cache");
    }
    let status = cmd.status().expect("invoke spice2kicad");
    assert!(
        status.success(),
        "spice2kicad exited with {status} converting {} \
         (a non-zero status here is usually the post-emit connectivity check)",
        src.display()
    );
}

// --- P10 ------------------------------------------------------------------

/// **P10 — determinism.** Two cache-less conversions of the same source
/// are byte-identical.
///
/// Landed LIVE, not `#[ignore]`d: measured on master, all ten fixtures
/// already round-trip identically (the SA's RNG is seeded from the
/// netlist, not from entropy). See the module doc.
#[test]
fn conversion_is_byte_deterministic_across_fixtures() {
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
    ];
    let mut failures = Vec::new();
    for name in FIXTURES {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let a = std::fs::read(convert_no_cache(&src, &tempdir(name))).expect("read a");
        let b = std::fs::read(convert_no_cache(&src, &tempdir(name))).expect("read b");
        if a != b {
            failures.push(format!(
                "{name}: two cache-less conversions differ ({} vs {} bytes)",
                a.len(),
                b.len()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "P10: conversion is not deterministic:\n{}",
        failures.join("\n")
    );
}

// --- shared s-expression helpers -----------------------------------------

fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    v.list_iter().map_or_else(
        || Box::new(std::iter::empty()) as Box<dyn Iterator<Item = &Value>>,
        |it| Box::new(it),
    )
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

fn head(v: &Value) -> Option<&str> {
    list_iter(v).next().and_then(as_str)
}

fn children<'a>(v: &'a Value, name: &str) -> Vec<&'a Value> {
    list_iter(v)
        .filter(|c| c.is_list() && head(c) == Some(name))
        .collect()
}

fn find_child<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    children(v, name).first().copied()
}

fn parse_sch(path: &Path) -> Value {
    let src = std::fs::read_to_string(path).expect("read sch");
    lexpr::from_str(&src).expect("parse sch")
}

/// A placed symbol's full pose *and* its library id.
///
/// The `lib_id` is part of the key on purpose: power glyphs are compared
/// by geometry rather than by refdes (see [`glyph_poses`]), and a bare
/// coordinate cannot tell a `power:GND` from a `power:PWR_FLAG` sitting
/// on the same anchor.
type Pose = (f64, f64, f64, bool, String);

/// `refdes -> pose` for every placed symbol.
fn poses(root: &Value) -> BTreeMap<String, Pose> {
    let mut out = BTreeMap::new();
    for sym in children(root, "symbol") {
        let Some(at) = find_child(sym, "at") else {
            continue;
        };
        let mut it = list_iter(at);
        it.next();
        let (Some(x), Some(y)) = (it.next().and_then(as_f64), it.next().and_then(as_f64)) else {
            continue;
        };
        let rotation = it.next().and_then(as_f64).unwrap_or(0.0);
        let mirror = find_child(sym, "mirror")
            .and_then(|m| list_iter(m).nth(1).and_then(as_str))
            .is_some_and(|s| s == "y");
        let lib_id = find_child(sym, "lib_id")
            .and_then(|l| list_iter(l).nth(1).and_then(as_str))
            .unwrap_or_default()
            .to_owned();
        let mut refdes = None;
        for prop in children(sym, "property") {
            let mut pit = list_iter(prop);
            pit.next();
            if pit.next().and_then(as_str) == Some("Reference") {
                refdes = pit.next().and_then(as_str).map(str::to_owned);
                break;
            }
        }
        if let Some(r) = refdes {
            out.insert(r, (x, y, rotation, mirror, lib_id));
        }
    }
    out
}

/// True for the auto-generated power/flag glyph refdes (`#PWR…`,
/// `#FLG…`). Everything else is a symbol the *user* wrote in the deck.
fn is_glyph(refdes: &str) -> bool {
    refdes.starts_with('#')
}

/// Poses of the power/ground glyphs, as a multiset keyed by geometry.
///
/// Load-bearing: glyph refdes are assigned by emission order, so
/// inserting one new glyph renumbers every later one. Matching glyphs by
/// refdes therefore reports "moved" for glyphs that did not budge —
/// e.g. adding `CB` to `common_emitter` renumbers four of nine glyphs
/// while all nine geometries are byte-identical. Compare geometry.
///
/// (The renumbering itself is a real, small, separately-fixable defect —
/// a stable glyph-naming scheme keyed on host pin rather than emission
/// order would remove it. This test pins the behaviour so that fix can
/// be made deliberately.)
fn glyph_poses(root: &Value) -> Vec<Pose> {
    let mut v: Vec<Pose> = poses(root)
        .into_iter()
        .filter(|(r, _)| is_glyph(r))
        .map(|(_, p)| p)
        .collect();
    v.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in emitted coordinates"));
    v
}

// --- geometry counters ----------------------------------------------------

/// Fallback half-extent for a symbol whose library body cannot be
/// measured — mirrors `electrical_safety.rs`.
const SYM_HALF_MM: f64 = 2.54;

fn load_test_library() -> kicad_symbols::Library {
    use kicad_symbols::Library;
    let libs_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("crates/kicad-symbols/tests/fixtures");
    let device =
        Library::from_file(libs_dir.join("Device.kicad_sym")).expect("parse Device.kicad_sym");
    let sim = Library::from_file(libs_dir.join("Simulation_SPICE.kicad_sym"))
        .expect("parse Simulation_SPICE.kicad_sym");
    let amp = Library::from_file(libs_dir.join("Amplifier_Operational.kicad_sym"))
        .expect("parse Amplifier_Operational.kicad_sym");
    let power =
        Library::from_file(libs_dir.join("power.kicad_sym")).expect("parse power.kicad_sym");
    device.merge(sim).merge(amp).merge(power)
}

/// Transform a symbol-local `LocalBbox` into the world frame — same
/// convention as `electrical_safety.rs::body_bbox_to_world`.
fn body_bbox_to_world(
    local: kicad_symbols::LocalBbox,
    origin_x: f64,
    origin_y: f64,
    rot_degrees: f64,
    mirror_y: bool,
) -> Bbox {
    let rot_norm = rot_degrees.rem_euclid(360.0).round();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rot = rot_norm as u16;
    let rotation = match rot {
        90 => Rotation::R90,
        180 => Rotation::R180,
        270 => Rotation::R270,
        _ => Rotation::R0,
    };
    let orient = Orientation { rotation, mirror_y };
    let mut x0 = f64::INFINITY;
    let mut y0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for (lx, ly) in [
        (local.x0, local.y0),
        (local.x0, local.y1),
        (local.x1, local.y0),
        (local.x1, local.y1),
    ] {
        let (rx, ry) = orient.apply_point(lx, ly);
        let (wx, wy) = (origin_x + rx, origin_y - ry);
        x0 = x0.min(wx);
        x1 = x1.max(wx);
        y0 = y0.min(wy);
        y1 = y1.max(wy);
    }
    Bbox { x0, y0, x1, y1 }
}

/// World-frame body boxes of the non-glyph symbols. Power glyphs sit ON
/// a host pin by design (V10) and are not obstacles.
fn placed_symbol_bboxes(root: &Value, library: &kicad_symbols::Library) -> Vec<(String, Bbox)> {
    let mut out = Vec::new();
    for (refdes, (x, y, rot, mirror, lib_id)) in poses(root) {
        if is_glyph(&refdes) || lib_id.starts_with("power:") {
            continue;
        }
        let bbox = library
            .lookup(&lib_id)
            .and_then(kicad_symbols::Symbol::body_bbox)
            .map_or(
                Bbox {
                    x0: x - SYM_HALF_MM,
                    y0: y - SYM_HALF_MM,
                    x1: x + SYM_HALF_MM,
                    y1: y + SYM_HALF_MM,
                },
                |local| body_bbox_to_world(local, x, y, rot, mirror),
            );
        out.push((refdes, bbox));
    }
    out
}

fn wire_segments(root: &Value) -> Vec<(Pt, Pt)> {
    let mut out = Vec::new();
    for w in children(root, "wire") {
        let Some(pts) = find_child(w, "pts") else {
            continue;
        };
        let xys: Vec<&Value> = list_iter(pts).filter(|c| head(c) == Some("xy")).collect();
        if xys.len() < 2 {
            continue;
        }
        let point = |v: &Value| -> Option<Pt> {
            let mut it = list_iter(v);
            it.next();
            Some((it.next().and_then(as_f64)?, it.next().and_then(as_f64)?))
        };
        if let (Some(a), Some(b)) = (point(xys[0]), point(xys[1])) {
            out.push((a, b));
        }
    }
    out
}

/// Labels as `(text, anchor, rotation, kind)` — the input `text_bbox`
/// needs. Mirrors `electrical_safety.rs::labels_with_kind`.
fn labels_with_kind(root: &Value) -> Vec<(String, Pt, u16, TextKind)> {
    let mut out = Vec::new();
    for tag in ["label", "global_label"] {
        for node in children(root, tag) {
            let Some(name) = list_iter(node).nth(1).and_then(as_str) else {
                continue;
            };
            let Some(at) = find_child(node, "at") else {
                continue;
            };
            let mut it = list_iter(at);
            it.next();
            let (Some(x), Some(y)) = (it.next().and_then(as_f64), it.next().and_then(as_f64))
            else {
                continue;
            };
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let rotation = it.next().and_then(as_f64).unwrap_or(0.0).rem_euclid(360.0) as u16;
            let kind = if tag == "label" {
                TextKind::PlainLabel
            } else {
                let shape =
                    find_child(node, "shape").and_then(|s| list_iter(s).nth(1).and_then(as_str));
                TextKind::global_label(shape)
            };
            out.push((name.to_owned(), (x, y), rotation, kind));
        }
    }
    out
}

/// The three geometric quality counts P11 requires not to grow when a
/// sheet is extended through the cache path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Metrics {
    /// V12 — wire segments crossing a foreign symbol body.
    v12_wire_body: usize,
    /// V13 (part 1) — label text boxes overlapping a symbol body.
    v13_label_body: usize,
    /// Symbol bodies overlapping each other.
    body_overlap: usize,
}

fn metrics(sch: &Path) -> Metrics {
    let root = parse_sch(sch);
    let library = load_test_library();
    let bodies = placed_symbol_bboxes(&root, &library);
    let wires = wire_segments(&root);

    let v12_wire_body = bodies
        .iter()
        .map(|(_, b)| {
            wires
                .iter()
                .filter(|(p, q)| b.intersects_segment(*p, *q))
                .count()
        })
        .sum();

    let v13_label_body = labels_with_kind(&root)
        .iter()
        .map(|(text, anchor, rot, kind)| {
            let lb = text_bbox(text, *anchor, 1.27, *rot, *kind);
            bodies.iter().filter(|(_, b)| lb.intersects(b)).count()
        })
        .sum();

    let mut body_overlap = 0;
    for i in 0..bodies.len() {
        for j in (i + 1)..bodies.len() {
            if bodies[i].1.intersects(&bodies[j].1) {
                body_overlap += 1;
            }
        }
    }

    Metrics {
        v12_wire_body,
        v13_label_body,
        body_overlap,
    }
}

// --- P11 ------------------------------------------------------------------

/// One P11 case: a fixture, and ONE element spliced into it.
struct CacheCase {
    name: &'static str,
    base: &'static str,
    added: &'static str,
    /// Zero-slack ratchet: how many pre-existing *glyph* geometries fail
    /// to survive the edit.
    ///
    /// `common_emitter` is 0 — every one of its nine glyphs keeps its
    /// exact pose (four merely renumber). `rc_lowpass` is 2: adding
    /// `C2`'s ground moves the `PWR_FLAG` to the new last ground pin and
    /// re-offsets `C1`'s GND glyph. Both are *decoration* re-anchoring
    /// around new neighbours, downstream of placement — the user symbols
    /// do not move in either case. Recorded to pin it, and it ratchets
    /// down only.
    glyph_pose_budget: usize,
}

const CACHE_CASES: &[CacheCase] = &[
    CacheCase {
        name: "rc_lowpass_plus_r",
        base: "rc_lowpass",
        // A second series resistor splitting `out` into `out`/`mid`.
        added: "R2 out mid 1k\nC2 mid 0 100n\n",
        glyph_pose_budget: 2,
    },
    CacheCase {
        name: "common_emitter_plus_c",
        base: "common_emitter",
        // One more bypass capacitor on the existing `b` node.
        added: "CB b 0 10n\n",
        glyph_pose_budget: 0,
    },
];

/// **P11 — cache-path stability.** See the module doc.
///
/// Editing a netlist and re-converting into the same output directory
/// (so the ADR-4 layout-cache sidecar is read) must leave every
/// pre-existing user symbol at its exact pose, must still pass the CLI's
/// post-emit connectivity check, and must not make any measured
/// geometric defect count worse than the base sheet's.
///
/// **Measured: 0 of 2 user symbols move on `rc_lowpass`+R2/C2, and 0 of
/// 8 on `common_emitter`+CB** — with both conversions passing the
/// connectivity check. This is the attributability property ADR-17 set
/// out to deliver; the cache already delivers it.
#[test]
fn cache_path_keeps_pre_existing_symbols_in_place() {
    let mut failures = Vec::new();
    for case in CACHE_CASES {
        let base_src = fixtures_dir().join(format!("{}.cir", case.base));
        let text = std::fs::read_to_string(&base_src).expect("read base fixture");
        let grown = text.replace(".end", &format!("{}\n.end", case.added));
        assert_ne!(grown, text, "{}: failed to splice element", case.name);

        // ONE directory for both conversions: the sidecar written by the
        // first run is what the second run reads. A fresh directory per
        // run would silently measure the cache-less path instead.
        let dir = tempdir(case.name);
        let src = dir.join(format!("{}.cir", case.base));
        let out = dir.join(format!("{}.kicad_sch", case.base));

        std::fs::copy(&base_src, &src).expect("copy base fixture");
        convert(&src, &out, false);
        let before = poses(&parse_sch(&out));
        let before_glyphs = glyph_poses(&parse_sch(&out));
        let before_metrics = metrics(&out);

        // Edit the deck in place, then re-convert over the same output.
        std::fs::write(&src, &grown).expect("write grown fixture");
        convert(&src, &out, false);
        let after = poses(&parse_sch(&out));
        let after_glyphs = glyph_poses(&parse_sch(&out));
        let after_metrics = metrics(&out);

        // (1) No pre-existing USER symbol changes pose.
        let moved: Vec<&String> = before
            .iter()
            .filter(|(r, _)| !is_glyph(r))
            .filter(|(r, p)| after.get(*r).is_none_or(|q| q != *p))
            .map(|(r, _)| r)
            .collect();
        if !moved.is_empty() {
            failures.push(format!(
                "{}: adding `{}` moved {} pre-existing user symbol(s) through the layout \
                 cache: {moved:?}",
                case.name,
                case.added.trim().replace('\n', "; "),
                moved.len(),
            ));
        }

        // (2) Pre-existing glyph GEOMETRY survives, matched by pose and
        //     lib_id rather than by refdes.
        let lost = before_glyphs
            .iter()
            .filter(|p| !after_glyphs.contains(p))
            .count();
        if lost > case.glyph_pose_budget {
            failures.push(format!(
                "{}: {lost} pre-existing power-glyph pose(s) did not survive the edit \
                 (budget {}); before={before_glyphs:?} after={after_glyphs:?}",
                case.name, case.glyph_pose_budget,
            ));
        }

        // (3) No geometric defect count grows on the extended sheet.
        for (label, b, a) in [
            (
                "V12 wire↔body",
                before_metrics.v12_wire_body,
                after_metrics.v12_wire_body,
            ),
            (
                "V13 label↔body",
                before_metrics.v13_label_body,
                after_metrics.v13_label_body,
            ),
            (
                "symbol body overlap",
                before_metrics.body_overlap,
                after_metrics.body_overlap,
            ),
        ] {
            if a > b {
                failures.push(format!(
                    "{}: {label} rose {b} → {a} on the grown sheet",
                    case.name,
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "P11: the layout-cache path is not stable:\n{}",
        failures.join("\n")
    );
}

// --- P11b — cache-less placement locality bound (ADR-19 Milestone 1) -------

/// One P11b case: a base fixture and ONE element spliced in.
struct LocalityCase {
    name: &'static str,
    base: &'static str,
    added: &'static str,
    /// Zero-slack ratchet: the page-pan-normalized count of pre-existing
    /// USER symbols whose pose is perturbed by the edit, measured on the
    /// CACHE-LESS path. Ratchets DOWN only.
    mover_budget: usize,
}

const LOCALITY_CASES: &[LocalityCase] = &[
    LocalityCase {
        name: "rc_lowpass_plus_r",
        base: "rc_lowpass",
        // A second series resistor splitting `out` into `out`/`mid`.
        added: "R2 out mid 1k\nC2 mid 0 100n\n",
        mover_budget: 0,
    },
    LocalityCase {
        name: "common_emitter_plus_c",
        base: "common_emitter",
        // One more bypass capacitor on the existing `b` node.
        added: "CB b 0 10n\n",
        // Back at 8 with the ADR-19 M4 REVERT (see `docs/layout-adr.md`,
        // "M4 reverted"): M4's content-derived Y datum had bought 8 -> 7
        // and cost `flow_geometry`'s F6 ratchet 18 cells on
        // `multivibrator`. Restoration of the pre-M4 measured value, not a
        // budget bump; it ratchets DOWN only, and can only fall again once
        // M3 (the footprint precondition) lands and M4 is re-attempted.
        mover_budget: 8,
    },
];

/// Count pre-existing USER symbols whose pose is perturbed by an edit,
/// after factoring out the single uniform page translation V15 may apply.
///
/// `translate_into_page` re-anchors the whole sheet when the content bbox
/// grows (invariants.md V15), so without normalization every element reads
/// as "moved" by the uniform pan and the metric could never ratchet toward
/// locality. The uniform shift is taken as the MODAL integer-grid delta
/// over the shared user symbols; a symbol counts as a mover when its delta
/// differs from the mode, its rotation/mirror changed, or it vanished.
/// Glyphs are decoration (downstream of placement) and are excluded,
/// matching the ADR-17 ablation's "poses moved = non-power symbols".
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn residual_movers(before: &BTreeMap<String, Pose>, after: &BTreeMap<String, Pose>) -> Vec<String> {
    let round = |v: f64| (v * 1000.0).round() as i64;
    let user = |m: &BTreeMap<String, Pose>| -> Vec<(String, Pose)> {
        m.iter()
            .filter(|(r, _)| !is_glyph(r))
            .map(|(r, p)| (r.clone(), p.clone()))
            .collect()
    };
    // Modal (dx, dy) over the user symbols present in both layouts.
    let mut counts: BTreeMap<(i64, i64), usize> = BTreeMap::new();
    for (r, p) in user(before) {
        if let Some(q) = after.get(&r) {
            *counts
                .entry((round(q.0 - p.0), round(q.1 - p.1)))
                .or_insert(0) += 1;
        }
    }
    let modal = counts
        .iter()
        .max_by_key(|(_, c)| **c)
        .map_or((0, 0), |(k, _)| *k);

    let mut movers = Vec::new();
    for (r, p) in user(before) {
        match after.get(&r) {
            None => movers.push(r),
            Some(q) => {
                let delta = (round(q.0 - p.0), round(q.1 - p.1));
                let rot_or_mirror_changed = round(q.2) != round(p.2) || q.3 != p.3;
                if delta != modal || rot_or_mirror_changed {
                    movers.push(r);
                }
            }
        }
    }
    movers
}

/// **P11b — cache-less placement locality bound.** ADR-19 Milestone 1.
///
/// placer-redesign.md's root engine R-A: because coordinates are derived
/// from *global* structure (`n`-scaled `y_bot`, the `layer_x` prefix-sum),
/// adding one element re-bases the whole page. This is the acceptance test
/// criterion (1) asks for — "bounds how many pre-existing elements move
/// when one is added, and that bound ratchets down" — made a governed
/// number. It is distinct from P11 above: that measures the ADR-4 cache
/// path (which already delivers 0-movement for the *user editing a deck*
/// workflow); this measures the CACHE-LESS path a placer redesign must
/// actually make local.
///
/// Measured on this tree, page-pan-normalized over user symbols:
/// `rc_lowpass` **0**, `common_emitter` **8**. (This *corrects* the stale
/// "17/17" the design doc cites — that figure counts the V15 uniform pan
/// and the glyph renumbering as movement; the honest, ratchetable quantity
/// is smaller.) These are high-water marks that ratchet DOWN only: an
/// ADR-19 stage that lowers them updates the literal; a change that raises
/// either is an R-A regression to diagnose, never a budget to bump.
#[test]
fn cache_less_placement_perturbation_within_bound() {
    let mut failures = Vec::new();
    for case in LOCALITY_CASES {
        let base_src = fixtures_dir().join(format!("{}.cir", case.base));
        let text = std::fs::read_to_string(&base_src).expect("read base fixture");
        let grown = text.replace(".end", &format!("{}\n.end", case.added));
        assert_ne!(grown, text, "{}: failed to splice element", case.name);

        // Base and grown each convert CACHE-LESS into their own fresh
        // directory, so neither reads a sidecar. (Sharing a directory would
        // measure the cache path — that is P11's job, not this one.)
        let base_out = convert_no_cache(&base_src, &tempdir(case.name));

        let grow_dir = tempdir(case.name);
        let grow_src = grow_dir.join(format!("{}.cir", case.base));
        std::fs::write(&grow_src, &grown).expect("write grown fixture");
        let grown_out = convert_no_cache(&grow_src, &tempdir(case.name));

        let before = poses(&parse_sch(&base_out));
        let after = poses(&parse_sch(&grown_out));
        let movers = residual_movers(&before, &after);
        if movers.len() > case.mover_budget {
            failures.push(format!(
                "{}: adding `{}` perturbed {} pre-existing user symbol(s) cache-lessly \
                 (budget {}): {movers:?}",
                case.name,
                case.added.trim().replace('\n', "; "),
                movers.len(),
                case.mover_budget,
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "P11b: cache-less placement locality regressed (an R-A blast-radius \
         increase — diagnose the geometry, do not raise the budget):\n{}",
        failures.join("\n")
    );
}
