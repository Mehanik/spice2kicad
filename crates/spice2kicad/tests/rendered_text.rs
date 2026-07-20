//! Rendered-ink verification — the one test suite that does NOT trust
//! the converter's own geometry model.
//!
//! Every other text-geometry verifier in this crate shares
//! `common::text_model::text_bbox` with the emitter's understanding of
//! where text lands. A model cannot falsify itself: label direction was
//! wrong for months while every V13 budget read zero, because emitter and
//! test agreed with each other and both disagreed with KiCad.
//!
//! So this file executes KiCad. `kicad-cli sch export svg
//! --no-background-color` renders each emitted sheet; every text run is
//! wrapped in `<g …><desc>THE TEXT</desc><path d="M … L …"/>…</g>`, so the
//! min/max of the path coordinates is the *true rendered ink box* of that
//! string, in the same millimetre frame as the `.kicad_sch`. KiCad draws
//! each string twice (fill + stroke pass), hence the dedupe.
//!
//! Two things are asserted:
//!
//!  1. **No two rendered text runs overlap** on any emitted sheet,
//!     including child sheets. Measured directly from ink — independent of
//!     `text_bbox` entirely.
//!  2. **Calibration**: for each text class the emitter models
//!     (plain label, global label, hierarchical label, property
//!     Reference / Value, power-glyph value) the *modelled* bbox must be a
//!     superset of the *rendered* ink, with the slack bounded by a stated
//!     epsilon. This is what makes keeping a model safe: `text_bbox` stops
//!     needing to be right, and starts tripping CI when it drifts away from
//!     what KiCad actually draws.
//!
//! When `kicad-cli` is not installed the tests skip cleanly — same
//! precedent as `common::kicad_to_spice` returning `Ok(None)` — unless
//! `REQUIRE_KICAD_CLI=1` is set.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::spice_to_kicad;
use common::text_model::{Bbox, TextKind, text_bbox};
use lexpr::Value;

/// Every fixture the CLI can convert. Mirrors `electrical_safety::SHEETS`
/// plus `opamp_inverting` (which emits a hierarchical sheet, and is where
/// the hierarchical-label class comes from).
const FIXTURES: &[&str] = &[
    "rc_lowpass",
    "common_emitter",
    "multivibrator",
    "diff_pair",
    "opamp_inverting_real",
    "opamp_inverting",
    "port_shapes",
    "rc_lowpass_ports",
    "opamp_definition_level",
    "named_rails",
];

// --- environment ---------------------------------------------------------

fn kicad_cli_available() -> bool {
    Command::new("kicad-cli")
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Skip-or-fail gate. Returns `false` when the caller should skip.
fn have_kicad_cli(test: &str) -> bool {
    if kicad_cli_available() {
        return true;
    }
    assert!(
        !common::require_kicad_cli(),
        "{test}: REQUIRE_KICAD_CLI=1 but kicad-cli is not installed",
    );
    eprintln!("{test}: kicad-cli not installed — skipping rendered-ink checks");
    false
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A private output directory per fixture. The converter drops a
/// `.layout.json` placement cache beside its output, so two fixtures
/// sharing a directory read each other's cache and produce corrupt
/// placements.
fn fixture_dir(test: &str, fixture: &str) -> PathBuf {
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("spice2kicad-render-{pid}-{test}-{fixture}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    dir
}

/// Convert a fixture, then render every emitted sheet (root *and* child
/// sheets) to SVG. Returns `(sheet_stem, svg_source)` pairs.
fn convert_and_render(test: &str, fixture: &str) -> Vec<(String, PathBuf, String)> {
    let dir = fixture_dir(test, fixture);
    let src = fixtures_dir().join(format!("{fixture}.cir"));
    let root = spice_to_kicad(&src, &dir).expect("spice2kicad");
    assert!(root.exists(), "{fixture}: converter produced no schematic");

    let mut sheets: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read out dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "kicad_sch"))
        .collect();
    sheets.sort();

    let svg_dir = dir.join("svg");
    let mut out = Vec::new();
    for sch in sheets {
        let status = Command::new("kicad-cli")
            .args(["sch", "export", "svg", "--no-background-color", "-o"])
            .arg(&svg_dir)
            .arg(&sch)
            .output()
            .expect("invoke kicad-cli");
        assert!(
            status.status.success(),
            "{fixture}: kicad-cli sch export svg failed on {}: {}",
            sch.display(),
            String::from_utf8_lossy(&status.stderr),
        );
        let stem = sch.file_stem().unwrap().to_string_lossy().into_owned();
        let svg = svg_dir.join(format!("{stem}.svg"));
        let body = std::fs::read_to_string(&svg)
            .unwrap_or_else(|e| panic!("{fixture}: reading {}: {e}", svg.display()));
        out.push((stem, sch, body));
    }
    assert!(!out.is_empty(), "{fixture}: nothing rendered");
    out
}

// --- SVG ink extraction --------------------------------------------------

/// One rendered text run: the string KiCad drew, and the bounding box of
/// the stroke centrelines it drew it with.
#[derive(Debug, Clone)]
struct InkRun {
    text: String,
    bbox: Bbox,
}

/// Pull every `<g><desc>TEXT</desc> … </g>` group's ink box out of a
/// KiCad-generated SVG, deduped (KiCad emits each string twice).
///
/// Hand-rolled rather than regex-based so the test crate stays
/// dependency-free; the SVG shape is fixed by KiCad's own plotter.
fn ink_runs(svg: &str) -> Vec<InkRun> {
    let mut out: Vec<InkRun> = Vec::new();
    let mut seen: Vec<(String, [i64; 4])> = Vec::new();
    let mut rest = svg;
    while let Some(start) = rest.find("<desc>") {
        let after = &rest[start + "<desc>".len()..];
        let Some(dend) = after.find("</desc>") else {
            break;
        };
        let text = after[..dend].to_owned();
        let body_start = &after[dend + "</desc>".len()..];
        let body_end = body_start.find("</g>").unwrap_or(body_start.len());
        let body = &body_start[..body_end];
        rest = &body_start[body_end..];

        let Some(bbox) = path_bbox(body) else {
            continue;
        };
        #[allow(clippy::cast_possible_truncation)]
        let key = (
            text.clone(),
            [
                (bbox.x0 * 100.0).round() as i64,
                (bbox.y0 * 100.0).round() as i64,
                (bbox.x1 * 100.0).round() as i64,
                (bbox.y1 * 100.0).round() as i64,
            ],
        );
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(InkRun { text, bbox });
    }
    out
}

/// Min/max of every `M x y` / `L x y` coordinate pair in an SVG path body.
fn path_bbox(body: &str) -> Option<Bbox> {
    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == 'M' || c == 'L' {
            let tail = &body[i + 1..];
            let mut it = tail.split_whitespace();
            if let (Some(x), Some(y)) = (it.next(), it.next())
                && let (Ok(x), Ok(y)) = (x.parse::<f64>(), y.parse::<f64>())
            {
                xs.push(x);
                ys.push(y);
            }
        }
        i += 1;
    }
    if xs.is_empty() {
        return None;
    }
    Some(Bbox {
        x0: xs.iter().copied().fold(f64::INFINITY, f64::min),
        y0: ys.iter().copied().fold(f64::INFINITY, f64::min),
        x1: xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        y1: ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    })
}

// --- test 1: rendered text must not overlap ------------------------------

/// Fixtures held out of the zero-overlap assertion, with the defect each
/// one is waiting on. NOT budgets — the assertion stays at zero; these are
/// known-failing inputs, to be deleted from this list (never softened into
/// a count) when the underlying defect is fixed.
///
/// Currently empty: every fixture is held to the zero-overlap assertion.
///
/// (Historical entry, resolved: `rc_lowpass_ports` used to have R1's
/// `Reference` ("R1") and `Value` ("1k") text each clipping C1's rendered
/// **pin-number** text by (0.06, 0.79) mm and (0.06, 0.73) mm. The cause
/// was R1's *orientation*, not R1↔C1 distance: at `rot 180` KiCad renders
/// R1's fields on its left, i.e. inside the 3.81 mm channel between R1 and
/// C1, right on top of C1's left-side pin-number glyphs. The legalizer
/// work now seeds R1 at `rot 0`, so its fields render outward and clear
/// C1's pin numbers by 7.12 mm. R1↔C1 spacing is 3 grid cells before and
/// after — it was never the variable.)
const EXCLUDED_FIXTURES: &[&str] = &[];

/// Overlap tolerance, mm. Matches the prototype: sub-0.05 mm slivers are
/// stroke-width artefacts, not legibility defects.
const OVERLAP_TOL_MM: f64 = 0.05;

#[test]
fn rendered_text_does_not_overlap_across_fixtures() {
    if !have_kicad_cli("rendered_text_does_not_overlap_across_fixtures") {
        return;
    }
    // Zero overlaps on every fixture. This is a ratchet, not a knob: if it
    // rises, that is a rendering regression to diagnose — never a budget to
    // bump (CLAUDE.md, "Budgets are ratchets").
    let budget = |_fixture: &str| -> usize { 0 };

    let mut failures = Vec::new();
    for fixture in FIXTURES {
        if EXCLUDED_FIXTURES.contains(fixture) {
            continue;
        }
        let mut hits = 0;
        for (stem, _, svg) in convert_and_render("ov", fixture) {
            let runs = ink_runs(&svg);
            assert!(!runs.is_empty(), "{fixture}/{stem}: no text rendered");
            for i in 0..runs.len() {
                for j in (i + 1)..runs.len() {
                    let (a, b) = (&runs[i].bbox, &runs[j].bbox);
                    let ox = a.x1.min(b.x1) - a.x0.max(b.x0);
                    let oy = a.y1.min(b.y1) - a.y0.max(b.y0);
                    if ox > OVERLAP_TOL_MM && oy > OVERLAP_TOL_MM {
                        eprintln!(
                            "{fixture}/{stem}: rendered text {:?} overlaps {:?} by ({ox:.2}, {oy:.2}) mm",
                            runs[i].text, runs[j].text,
                        );
                        hits += 1;
                    }
                }
            }
        }
        if hits > budget(fixture) {
            failures.push(format!("{fixture}: {hits} rendered-text overlaps"));
        }
    }
    assert!(
        failures.is_empty(),
        "rendered text overlaps (measured from kicad-cli SVG ink):\n  {}",
        failures.join("\n  "),
    );
}

// --- test 2: calibrate the model against the ink -------------------------

/// Which modelled text class a bbox came from. Distinct from
/// [`TextKind`] because two classes can share a `TextKind` (hierarchical
/// labels reuse the global-label chevron model) while wanting separate
/// calibration budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TextClass {
    PlainLabel,
    GlobalLabel,
    HierarchicalLabel,
    PropertyReference,
    PropertyValue,
    PowerGlyphValue,
}

impl TextClass {
    fn name(self) -> &'static str {
        match self {
            TextClass::PlainLabel => "plain label",
            TextClass::GlobalLabel => "global label",
            TextClass::HierarchicalLabel => "hierarchical label",
            TextClass::PropertyReference => "property Reference",
            TextClass::PropertyValue => "property Value",
            TextClass::PowerGlyphValue => "power-glyph value",
        }
    }

    /// Maximum tolerated slack (mm) between the modelled bbox and the
    /// rendered ink, on any single edge.
    ///
    /// Ratchets measured against KiCad 9's renderer, not headroom: each
    /// sits just above the worst case observed across every fixture
    /// (recorded in the comments). Lower them when the model tightens;
    /// never raise them to make a drift pass.
    ///
    /// Labels legitimately carry more slack than properties: the modelled
    /// box starts at the anchor so it covers the label's own chevron /
    /// triangle graphics, which are drawn ink but are not part of the
    /// text run being measured.
    fn slack_epsilon_mm(self) -> f64 {
        match self {
            // Chevron/triangle lead. Worst measured: 1.83 (global),
            // 1.80 (hierarchical).
            TextClass::GlobalLabel | TextClass::HierarchicalLabel => 1.90,
            // Standoff (0.35 mm) plus unused descender. Worst: 0.77.
            TextClass::PlainLabel => 0.85,
            // Trailing side bearing plus unused descender. Worst: 0.46.
            _ => 0.55,
        }
    }
}

/// A modelled text bbox, tagged with the class it belongs to.
struct Modelled {
    class: TextClass,
    text: String,
    bbox: Bbox,
}

#[test]
fn text_bbox_model_covers_rendered_ink() {
    if !have_kicad_cli("text_bbox_model_covers_rendered_ink") {
        return;
    }
    // (max overhang, max slack, sample count) per class.
    let mut stats: BTreeMap<TextClass, (f64, f64, usize)> = BTreeMap::new();
    let mut escapes: Vec<String> = Vec::new();

    for fixture in FIXTURES {
        for (stem, sch, svg) in convert_and_render("cal", fixture) {
            let root = parse_sch(&sch);
            let modelled = modelled_text(&root);
            let mut ink = ink_runs(&svg);

            for m in modelled {
                // Match this modelled box to the nearest unconsumed ink
                // run drawing the same string.
                let Some(idx) = nearest_ink(&ink, &m) else {
                    continue;
                };
                let run = ink.remove(idx);
                let over = [
                    m.bbox.x0 - run.bbox.x0,
                    m.bbox.y0 - run.bbox.y0,
                    run.bbox.x1 - m.bbox.x1,
                    run.bbox.y1 - m.bbox.y1,
                ]
                .into_iter()
                .fold(f64::NEG_INFINITY, f64::max);
                let slack = [
                    run.bbox.x0 - m.bbox.x0,
                    run.bbox.y0 - m.bbox.y0,
                    m.bbox.x1 - run.bbox.x1,
                    m.bbox.y1 - run.bbox.y1,
                ]
                .into_iter()
                .fold(f64::NEG_INFINITY, f64::max);

                let e = stats
                    .entry(m.class)
                    .or_insert((f64::NEG_INFINITY, f64::NEG_INFINITY, 0));
                e.0 = e.0.max(over);
                e.1 = e.1.max(slack);
                e.2 += 1;

                if over > INK_OVERHANG_TOL_MM {
                    escapes.push(format!(
                        "{fixture}/{stem}: {} {:?} — ink escapes model by {over:.2} mm",
                        m.class.name(),
                        m.text,
                    ));
                }
                if slack > m.class.slack_epsilon_mm() {
                    escapes.push(format!(
                        "{fixture}/{stem}: {} {:?} — model over-reserves by {slack:.2} mm (> {:.2})",
                        m.class.name(),
                        m.text,
                        m.class.slack_epsilon_mm(),
                    ));
                }
            }
        }
    }

    for (class, (over, slack, n)) in &stats {
        eprintln!(
            "{:<20} n={n:<4} max ink-escape {over:+.3} mm   max slack {slack:.3} mm",
            class.name(),
        );
    }
    // Every class the emitter can produce must actually have been
    // exercised — a silently-empty class calibrates nothing.
    for class in [
        TextClass::PlainLabel,
        TextClass::GlobalLabel,
        TextClass::HierarchicalLabel,
        TextClass::PropertyReference,
        TextClass::PropertyValue,
        TextClass::PowerGlyphValue,
    ] {
        assert!(
            stats.contains_key(&class),
            "no {} was rendered by any fixture — calibration is vacuous",
            class.name(),
        );
    }
    assert!(
        escapes.is_empty(),
        "text_bbox has drifted from KiCad's rendering:\n  {}",
        escapes.join("\n  "),
    );
}

/// How far rendered ink may poke outside the modelled bbox — i.e. how far
/// `text_bbox` is allowed to be *wrong in the unsafe direction*.
///
/// Kept below half a pen width (KiCad strokes text with ~0.15 mm), since
/// the SVG path coordinates are stroke centrelines and the real ink
/// already extends that far. The measured worst case across every fixture
/// is −0.02 mm: the model strictly contains every centreline today. A
/// positive number here means the model has drifted and some verifier is
/// silently under-reserving space.
const INK_OVERHANG_TOL_MM: f64 = 0.05;

/// Nearest unconsumed ink run drawing the same string as `m`, by
/// centre distance. Returns `None` when the string was never rendered
/// (e.g. a field KiCad chose to hide).
fn nearest_ink(ink: &[InkRun], m: &Modelled) -> Option<usize> {
    let mc = (
        f64::midpoint(m.bbox.x0, m.bbox.x1),
        f64::midpoint(m.bbox.y0, m.bbox.y1),
    );
    ink.iter()
        .enumerate()
        .filter(|(_, r)| r.text == m.text)
        .map(|(i, r)| {
            let c = (
                f64::midpoint(r.bbox.x0, r.bbox.x1),
                f64::midpoint(r.bbox.y0, r.bbox.y1),
            );
            (i, (c.0 - mc.0).hypot(c.1 - mc.1))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Every text bbox the emitter's model claims for one sheet.
#[allow(clippy::similar_names)]
fn modelled_text(root: &Value) -> Vec<Modelled> {
    let mut out = Vec::new();

    for (tag, class) in [
        ("label", TextClass::PlainLabel),
        ("global_label", TextClass::GlobalLabel),
        ("hierarchical_label", TextClass::HierarchicalLabel),
    ] {
        for node in children(root, tag) {
            let Some(name) = list_iter(node).nth(1).and_then(as_str) else {
                continue;
            };
            let Some((x, y, rot)) = at_xy_rot(node) else {
                continue;
            };
            let shape =
                find_child(node, "shape").and_then(|s| list_iter(s).nth(1).and_then(as_str));
            let kind = match class {
                TextClass::PlainLabel => TextKind::PlainLabel,
                TextClass::HierarchicalLabel => TextKind::hier_label(),
                _ => TextKind::global_label(shape),
            };
            let size = effects_font_size(node).unwrap_or(1.27);
            out.push(Modelled {
                class,
                text: name.to_owned(),
                bbox: text_bbox(name, (x, y), size, rot, kind),
            });
        }
    }

    for sym in children(root, "symbol") {
        let refdes = property_value(sym, "Reference").unwrap_or_default();
        let is_power = refdes.starts_with("#PWR") || refdes.starts_with("#FLG");
        for prop in children(sym, "property") {
            if property_hidden(prop) {
                continue;
            }
            let mut it = list_iter(prop);
            it.next();
            let key = it.next().and_then(as_str).unwrap_or("");
            let val = it.next().and_then(as_str).unwrap_or("");
            if !matches!(key, "Reference" | "Value") || val.is_empty() {
                continue;
            }
            let centred = !property_has_justify(prop);
            let (class, kind) = match (is_power, centred, key) {
                (true, _, _) => (TextClass::PowerGlyphValue, TextKind::CenteredValue),
                (false, true, "Reference") => {
                    (TextClass::PropertyReference, TextKind::CenteredValue)
                }
                (false, true, _) => (TextClass::PropertyValue, TextKind::CenteredValue),
                (false, false, "Reference") => {
                    (TextClass::PropertyReference, TextKind::PropertyReference)
                }
                (false, false, _) => (TextClass::PropertyValue, TextKind::PropertyValue),
            };
            if is_power && key != "Value" {
                continue;
            }
            let Some((px, py, _)) = at_xy_rot(prop) else {
                continue;
            };
            let size = effects_font_size(prop).unwrap_or(1.27);
            let rot = if is_power {
                at_xy_rot(prop).map_or(0, |(_, _, r)| r)
            } else {
                field_render_rotation(sym)
            };
            out.push(Modelled {
                class,
                text: val.to_owned(),
                bbox: text_bbox(val, (px, py), size, rot, kind),
            });
        }
    }
    out
}

// --- minimal s-expression helpers ---------------------------------------

fn parse_sch(path: &Path) -> Value {
    let src = std::fs::read_to_string(path).expect("read sch");
    lexpr::from_str(&src).expect("parse sch as lexpr")
}

fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    match v.list_iter() {
        Some(it) => Box::new(it),
        None => Box::new(std::iter::empty()),
    }
}

fn head(v: &Value) -> Option<&str> {
    list_iter(v).next().and_then(lexpr::Value::as_symbol)
}

fn as_str(v: &Value) -> Option<&str> {
    v.as_str().or_else(|| v.as_symbol())
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_symbol().and_then(|s| s.parse().ok()))
}

fn children<'a>(v: &'a Value, name: &str) -> Vec<&'a Value> {
    list_iter(v).filter(|c| head(c) == Some(name)).collect()
}

fn find_child<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    children(v, name).into_iter().next()
}

fn at_xy_rot(node: &Value) -> Option<(f64, f64, u16)> {
    let at = find_child(node, "at")?;
    let mut it = list_iter(at);
    it.next();
    let x = it.next().and_then(as_f64)?;
    let y = it.next().and_then(as_f64)?;
    let rot = it.next().and_then(as_f64).unwrap_or(0.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rot_u = ((rot.round() as i64).rem_euclid(360)) as u16;
    Some((x, y, rot_u))
}

fn effects_font_size(node: &Value) -> Option<f64> {
    let size = find_child(find_child(find_child(node, "effects")?, "font")?, "size")?;
    let mut it = list_iter(size);
    it.next();
    it.next().and_then(as_f64)
}

fn property_value(sym: &Value, key: &str) -> Option<String> {
    for prop in children(sym, "property") {
        let mut it = list_iter(prop);
        it.next();
        if it.next().and_then(as_str) == Some(key) {
            return it.next().and_then(as_str).map(ToOwned::to_owned);
        }
    }
    None
}

fn property_hidden(prop: &Value) -> bool {
    for c in list_iter(prop) {
        if head(c) == Some("hide") {
            return true;
        }
        if head(c) == Some("effects") {
            for e in list_iter(c) {
                if head(e) == Some("hide") {
                    let v = list_iter(e).nth(1).and_then(as_str);
                    if v == Some("yes") || v.is_none() {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn property_has_justify(prop: &Value) -> bool {
    children(prop, "effects")
        .iter()
        .any(|e| !children(e, "justify").is_empty())
}

/// Mirrors `electrical_safety::field_render_rotation` (and the emitter's):
/// a field's own angle is not what KiCad draws — the parent symbol's
/// transform applies on top.
fn field_render_rotation(sym: &Value) -> u16 {
    let rot = at_xy_rot(sym).map_or(0, |(_, _, r)| r);
    let mirrored_y =
        find_child(sym, "mirror").and_then(|m| list_iter(m).nth(1).and_then(as_str)) == Some("y");
    if mirrored_y { (540 - rot) % 360 } else { rot }
}
