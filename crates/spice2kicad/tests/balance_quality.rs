//! **Q6 — visual balance** (v0.2 roadmap A4, Tier 2).
//!
//! Penalises layouts that jam all their content into one region of the
//! sheet beside empty space. A whole-sheet OUTPUT-geometry measure, read
//! off the emitted `.kicad_sch` and ratcheted per fixture at the value
//! measured on `master`.
//!
//! # The metric
//!
//! Absolute page *position* is V15's concern, and `translate_into_page`
//! normalises it, so Q6 deliberately measures balance over the CONTENT
//! bounding box, not the whole page:
//!
//!   1. Take every DRAWN symbol origin — every top-level `(symbol …)`
//!      whose refdes is not a `#`-glyph and whose `lib_id` is not
//!      `power:*`. `;@ ignore`d elements never emit a symbol, so they drop
//!      out for free.
//!   2. The content bbox is the AABB of those origins.
//!   3. Overlay a coarse `G×G` grid (`G = 4`) on that bbox and bin each
//!      component into a cell by its origin. `occupancy[c]` is the count of
//!      components in cell `c`.
//!   4. **Q6 = coefficient of variation of occupancy** across the `G²`
//!      cells = `stddev(occupancy) / mean(occupancy)`.
//!
//! CoV is dimensionless and scale-free, so it is comparable across
//! fixtures of very different physical size — a perfectly uniform spread
//! scores 0, a single dense corner scores high. Lower is better; the
//! ratchet drives it DOWN.
//!
//! Because `mean = n_components / G²` is fixed by the component count, CoV
//! reduces to `stddev(occupancy) · G² / n`, i.e. it is purely a function of
//! how evenly the `n` components fall across the `G²` cells.
//!
//! # Honest caveat — small fixtures pin the metric
//!
//! With `G = 4` the grid has 16 cells. A fixture with only a handful of
//! drawn components (rc_lowpass has 2) cannot populate more than a few
//! cells, so its CoV is dominated by discreteness and takes a fixed,
//! near-maximal value — it is not really measuring "balance" there, just
//! "few components in a 16-cell grid". The `< 2*G` guard in [`q6_balance`]
//! documents this: below that many components the number is discreteness,
//! not aesthetics. It is still locked (a rise past the recorded value would
//! mean the content bbox degenerated), but do not read a high CoV on a
//! 2-component fixture as a balance defect. The metric earns its keep on
//! the larger fixtures (multivibrator, diff_pair, the opamps), where the
//! component count is comparable to the cell count and the CoV genuinely
//! tracks clumping.
//!
//! # Distinct from what already exists
//!
//! * **V15** (`page_bounds`) — content fits the usable A4 area; Q6 is
//!   scale-free and says nothing about absolute position.
//! * **Q5** (`alignment_quality.rs`) — pairwise near-miss alignment; Q6 is
//!   a global density-spread measure, not pairwise.
//! * **Q3** (`flow_monotonicity.rs`) — left→right ordering; Q6 is
//!   order-agnostic.
//!
//! Like V16 this is a Tier-2 aesthetic **measured float**, never a
//! coefficient in any objective (CLAUDE.md § constraints-vs-costs).
//!
//! # Why this is an informational tripwire, NOT a zero-slack ratchet
//!
//! Q6 as defined (CoV over a fixed `G=4` grid) is **too noisy on small
//! fixtures to gate on** — its value is dominated by discreteness below
//! `2*G` components, so a *rise* is not reliably a regression (see the
//! caveat above). A zero-slack per-fixture ratchet would therefore fire on
//! *benign* layout changes (every Milestone B/C placement change moves Q6),
//! obstructing exactly the structure work Milestone A exists to guard —
//! precisely the failure CLAUDE.md's V16 doctrine warns against ("do not
//! admit a metric that isn't a genuinely load-bearing counted quantity").
//!
//! So Q6 is kept as: (a) a **degeneracy tripwire** — a single generous
//! ceiling ([`Q6_DEGENERATE_CEILING`]) that only fires if the content bbox
//! collapses toward a single cell (CoV → its `G=4` maximum ≈ 3.87); and
//! (b) an **informational tracker** — per-fixture reference values measured
//! on `master`, dumped with `S2K_Q6_DUMP=1` and surfaced via an eprintln
//! when a run drifts from them, so balance changes are *visible* during the
//! structure work without being a hard gate. When Milestone E gives Q6 an
//! adaptive grid (`G ≈ √n`) and real cluster structure, revisit promoting
//! it to a proper ratchet.

mod common;

use std::collections::HashMap;
use std::path::PathBuf;

use common::spice_to_kicad;
use lexpr::Value;

/// Grid resolution: a `G×G` overlay on the content bbox.
const G: usize = 4;

/// Float comparison slack for the ratchet assert (matches the wire-detour
/// epsilon in `placement_quality.rs`): the measured value must not exceed
/// the recorded budget by more than this.
const EPS: f64 = 1e-4;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> common::TempDir {
    common::TempDir::new("q6", name)
}

// --- lexpr helpers (mirrors alignment_quality.rs) ------------------------

fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    v.list_iter().map_or_else(
        || Box::new(std::iter::empty()) as Box<dyn Iterator<Item = &Value>>,
        |it| Box::new(it),
    )
}

fn head(v: &Value) -> Option<&str> {
    list_iter(v).next().and_then(as_str)
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

fn find_child<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    list_iter(v).find(|c| c.is_list() && head(c) == Some(name))
}

fn children<'a>(v: &'a Value, name: &str) -> Vec<&'a Value> {
    list_iter(v)
        .filter(|c| c.is_list() && head(c) == Some(name))
        .collect()
}

/// `refdes -> emitted symbol origin (x, y) in mm` for every DRAWN flow
/// body: every top-level `(symbol …)` whose refdes is not a `#`-glyph and
/// whose `lib_id` is not `power:*`. Power/ground glyphs are decoration hung
/// off a rail pin, never content bodies; `;@ ignore`d elements emit no
/// symbol at all, so they are excluded for free.
fn drawn_symbol_origins(root: &Value) -> HashMap<String, (f64, f64)> {
    let mut out = HashMap::new();
    for sym in children(root, "symbol") {
        let lib_id = find_child(sym, "lib_id")
            .and_then(|l| list_iter(l).nth(1).and_then(as_str))
            .unwrap_or_default();
        if lib_id.starts_with("power:") {
            continue;
        }
        let mut refdes = None;
        for prop in children(sym, "property") {
            let mut it = list_iter(prop);
            it.next();
            if it.next().and_then(as_str) == Some("Reference") {
                refdes = it.next().and_then(as_str).map(str::to_owned);
                break;
            }
        }
        let Some(refdes) = refdes else { continue };
        if refdes.starts_with('#') {
            continue;
        }
        let Some(at) = find_child(sym, "at") else {
            continue;
        };
        let mut it = list_iter(at);
        it.next(); // head "at"
        let (Some(x), Some(y)) = (it.next().and_then(as_f64), it.next().and_then(as_f64)) else {
            continue;
        };
        out.insert(refdes, (x, y));
    }
    out
}

// --- measurement ---------------------------------------------------------

/// Bin coordinate `c` into one of `G` slots spanning `[lo, hi]`. A
/// degenerate span (`hi == lo`, all origins share the axis) puts every
/// component in slot 0. The top edge is clamped into the last slot so
/// `c == hi` does not fall off the grid.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss
)]
fn slot(c: f64, lo: f64, hi: f64) -> usize {
    if hi - lo <= f64::EPSILON {
        return 0;
    }
    let frac = (c - lo) / (hi - lo);
    let idx = (frac * G as f64).floor() as isize;
    idx.clamp(0, G as isize - 1) as usize
}

/// Q6 for one fixture: the coefficient of variation of per-cell occupancy
/// over a `G×G` grid laid on the content bbox of the drawn symbol origins.
/// Returns 0.0 when there are no drawn components (an empty sheet is
/// trivially "balanced").
fn q6_balance(name: &str) -> f64 {
    let dir = tempdir(name);
    let sch = spice_to_kicad(&fixtures_dir().join(format!("{name}.cir")), &dir)
        .unwrap_or_else(|e| panic!("convert {name}: {e}"));
    let root = lexpr::from_str(&std::fs::read_to_string(&sch).expect("read sch"))
        .expect("parse sch as lexpr");
    let origins: Vec<(f64, f64)> = drawn_symbol_origins(&root).into_values().collect();

    let n = origins.len();
    if n == 0 {
        return 0.0;
    }

    // Content bbox.
    let x0 = origins.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let x1 = origins
        .iter()
        .map(|p| p.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let y0 = origins.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let y1 = origins
        .iter()
        .map(|p| p.1)
        .fold(f64::NEG_INFINITY, f64::max);

    // Bin into the G×G grid.
    let mut occ = [0u32; G * G];
    for &(x, y) in &origins {
        let cx = slot(x, x0, x1);
        let cy = slot(y, y0, y1);
        occ[cy * G + cx] += 1;
    }

    // Coefficient of variation = stddev / mean over all G² cells.
    #[allow(clippy::cast_precision_loss)]
    let cells = (G * G) as f64;
    #[allow(clippy::cast_precision_loss)]
    let mean = n as f64 / cells; // == sum(occ) / (G*G)
    if mean <= 0.0 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let var = occ
        .iter()
        .map(|&c| {
            let d = f64::from(c) - mean;
            d * d
        })
        .sum::<f64>()
        / cells;
    var.sqrt() / mean
}

// --- ratchet -------------------------------------------------------------

/// Gross-degeneracy ceiling: the pass/fail gate. `G=4` bounds CoV at
/// √(G²−1) ≈ 3.87 (all components in one cell); a value near that means the
/// content bbox has collapsed. `3.5` catches that pathology while sitting
/// comfortably above every measured master value (max 2.6458), so it never
/// fires on a normal — even clumpy — layout. This is the only hard assert.
const Q6_DEGENERATE_CEILING: f64 = 3.5;

/// Per-fixture Q6 measured on `master`, kept **informational** (not a hard
/// gate — see the module docs on why a zero-slack Q6 ratchet would obstruct
/// the structure work). A run drifting from these prints a note so balance
/// changes stay visible; only [`Q6_DEGENERATE_CEILING`] fails the test.
const Q6_REFERENCE: &[(&str, f64)] = &[
    ("rc_lowpass", 2.6458),
    ("rc_lowpass_ports", 2.6458),
    // 1.0000 -> 1.2247: the ADR-19 M4 revert (`835e073`) restored the pre-M4
    // Y datum and this fixture's content spread with it. Informational
    // bookkeeping, not a ratchet — the only hard gate here is
    // `Q6_DEGENERATE_CEILING`, and 1.2247 sits far below it. Measured on
    // `c968cbd` with `S2K_Q6_DUMP=1`; every other fixture is unchanged.
    ("common_emitter", 1.2247),
    ("multivibrator", 1.0000),
    ("diff_pair", 1.4832),
    ("opamp_inverting", 2.6458),
    ("opamp_inverting_real", 2.0817),
    ("port_shapes", 1.7321),
    ("opamp_definition_level", 1.2910),
    ("named_rails", 1.7321),
    // F0 (v0.2 roadmap) — informational master reference, not a gate.
    ("rc_phase_shift", 0.8938),
];

#[test]
fn balance_within_ceiling_across_fixtures() {
    let reference: HashMap<&str, f64> = Q6_REFERENCE.iter().copied().collect();
    let mut failures = Vec::new();
    for &(name, _) in Q6_REFERENCE {
        let q6 = q6_balance(name);
        common::scoreboard::record("q6.cov", name, q6);
        if std::env::var("S2K_Q6_DUMP").is_ok() {
            println!("(\"{name}\", {q6:.4}),");
        }
        // Hard gate: only gross content-bbox degeneracy fails the test.
        if q6 > Q6_DEGENERATE_CEILING + EPS {
            failures.push(format!(
                "{name}: Q6 balance CoV {q6:.4} exceeds the degeneracy ceiling \
                 {Q6_DEGENERATE_CEILING:.4} — the content bbox has collapsed toward \
                 a single grid cell. Diagnose the placement, do not raise the ceiling."
            ));
        }
        // Informational: surface drift from the master reference so balance
        // changes stay visible during the structure work (not a failure).
        if let Some(&r) = reference.get(name) {
            if (q6 - r).abs() > 1e-3 {
                eprintln!(
                    "Q6 {name}: balance drifted {r:.4} -> {q6:.4} (informational; \
                     update the Q6_REFERENCE literal if this is the new master)"
                );
            }
        }
    }
    assert!(
        failures.is_empty(),
        "Q6 degeneracy tripwire:\n{}",
        failures.join("\n")
    );
}
