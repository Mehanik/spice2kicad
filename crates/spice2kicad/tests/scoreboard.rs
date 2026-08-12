//! The champion/challenger scoreboard (ADR-23).
//!
//! # The two questions
//!
//! * *"Did this change break what we shipped?"* — the per-fixture,
//!   zero-slack, conjunctive ratchets plus `baseline_lock`. Unchanged,
//!   and they remain the gate for every ordinary change.
//! * *"Is placer B better than placer A?"* — this file. Two different
//!   placers land in two different global optima of a chaotic,
//!   globally-coupled map; Pareto non-regression across ~165 correlated
//!   scalars measured *on the incumbent's own output* is achievable
//!   essentially only by a no-op. Selecting an architecture therefore
//!   needs an instrument that permits sideways trades — inside a tier,
//!   and never across the tier boundary.
//!
//! **This is not a licence to bypass a ratchet.** It applies to
//! whole-placer comparisons only; see ADR-23 for the promotion rule and
//! for why an ordinary change may not use it.
//!
//! # How to run it
//!
//! The measurements are produced by the verifiers themselves — each one
//! reports its number through `common::scoreboard::record` right beside
//! the assertion that already computes it, so there is exactly one
//! definition of every metric and it is the one the ratchet asserts on.
//! Collecting a placer's row therefore means running the suite with the
//! sink switched on:
//!
//! ```sh
//! just scoreboard-run champion   # -> target/scoreboard/champion
//! just scoreboard-run m4-ydatum  # -> target/scoreboard/m4-ydatum
//! just scoreboard champion m4-ydatum
//! ```
//!
//! or, without `just`:
//!
//! ```sh
//! S2K_SCOREBOARD_DIR=/tmp/sb/champion cargo test --workspace --no-fail-fast
//! S2K_PLACER=m4-ydatum S2K_SCOREBOARD_DIR=/tmp/sb/m4 cargo test --workspace --no-fail-fast
//! S2K_SCOREBOARD_CHAMPION=/tmp/sb/champion S2K_SCOREBOARD_CHALLENGER=/tmp/sb/m4 \
//!   cargo test -p spice2kicad --test scoreboard -- --ignored --nocapture
//! ```
//!
//! The challenger run is **expected to be red** — every zero-slack
//! ratchet is calibrated on the champion's output, so any real placement
//! difference trips several. `--no-fail-fast` is load-bearing: it is
//! what makes the *measurements* complete even though the assertions
//! fail.
//!
//! The report itself is `#[ignore]`d, so it never runs in the default
//! `cargo test` path (it would have nothing to read).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Metric registry
// ---------------------------------------------------------------------------

/// CLAUDE.md's invariant tiers, as the aggregate consumes them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Tier {
    /// Correctness. Judged **per fixture**, hard, for champion and
    /// challenger alike. Never aggregated, never traded.
    T0,
    /// Readability constraints. Aggregated.
    T1,
    /// Aesthetic refinement. Aggregated, strictly below Tier 1.
    T2,
    /// Measured and printed, but excluded from the aggregate — the
    /// project's own record says the metric is informational (Q6's CoV
    /// is noisy on small fixtures and is not a ratchet).
    Info,
}

/// One graded quantity.
struct Metric {
    /// The id the verifiers record under.
    id: &'static str,
    tier: Tier,
    /// Aggregate points per unit of the metric.
    ///
    /// Every count metric is 1.0 — one violation is one point, which is
    /// the only non-arbitrary choice for a quantity whose ideal is zero.
    /// The one continuous metric (wire detour, a ratio) is scaled so
    /// that **one point = one percentage point of excess wire**, which
    /// puts it on the same order as a count without inventing a
    /// preference between them.
    points_per_unit: f64,
    what: &'static str,
}

const fn m(id: &'static str, tier: Tier, points_per_unit: f64, what: &'static str) -> Metric {
    Metric {
        id,
        tier,
        points_per_unit,
        what,
    }
}

/// Every metric the verifiers report, with its tier.
///
/// Tier assignment is read off CLAUDE.md's invariant table, not chosen
/// here: V11/V1/V2 correctness → Tier 0; V12/V13/V14/V15 → Tier 1;
/// V5/V6/V7/V16 → Tier 2. The project's own Q-/F-/P-series metrics are
/// not V-numbered invariants; they are structural/aesthetic gradients
/// (flow monotonicity, alignment near-misses, series pose, stub runs,
/// locality), so they sit in Tier 2 by the constraints-vs-costs decision
/// rule — a continuous gradient with no single correct value is Tier 2.
const METRICS: &[Metric] = &[
    // --- Tier 0 — correctness ---------------------------------------
    m(
        "t0.convert_fail",
        Tier::T0,
        1.0,
        "conversion refused / failed",
    ),
    m(
        "t0.partition",
        Tier::T0,
        1.0,
        "V11 net-partition findings (merge/split) from emitted geometry",
    ),
    m(
        "t0.v11_pin_overlap",
        Tier::T0,
        1.0,
        "placer-level pin-on-pin across nets",
    ),
    m(
        "t0.v11_wire_label",
        Tier::T0,
        1.0,
        "V11 wire/label-on-foreign-pin coincidences",
    ),
    m(
        "t0.sym_overlap",
        Tier::T0,
        1.0,
        "symbol/symbol body overlaps",
    ),
    m(
        "t0.cross_net_overlap",
        Tier::T0,
        1.0,
        "cross-net collinear wire overlaps (latent V11 short)",
    ),
    // --- Tier 1 — readability constraints ---------------------------
    m("v12", Tier::T1, 1.0, "wires crossing foreign symbol bodies"),
    m("v13.1_label_body", Tier::T1, 1.0, "label bbox over body"),
    m(
        "v13.2_label_prop",
        Tier::T1,
        1.0,
        "label bbox over property text",
    ),
    m(
        "v13.3_label_wire_interior",
        Tier::T1,
        1.0,
        "label anchor inside a foreign-net wire",
    ),
    m(
        "v13.4_text_mutual",
        Tier::T1,
        1.0,
        "visible text mutual overlap",
    ),
    m(
        "v13.5_prop_pintext",
        Tier::T1,
        1.0,
        "property text over pin text",
    ),
    m(
        "v13.6_label_glyphvalue",
        Tier::T1,
        1.0,
        "label over power-glyph value text",
    ),
    m(
        "v13.6a_glyphtext",
        Tier::T1,
        1.0,
        "power-glyph value text over bodies/pin text",
    ),
    m(
        "v13.6b_pwrflag_glyph",
        Tier::T1,
        1.0,
        "PWR_FLAG graphic over power-glyph graphic",
    ),
    m("v13.7_label_pintext", Tier::T1, 1.0, "label over pin text"),
    m("v13.8_label_label", Tier::T1, 1.0, "label over label"),
    m(
        "v13.ink_overlap",
        Tier::T1,
        1.0,
        "REAL `kicad-cli` SVG-ink text overlap (the only falsifier of the model)",
    ),
    m(
        "v13.glyph_neighbour_value",
        Tier::T1,
        1.0,
        "sheet glyph over neighbour value text",
    ),
    m(
        "v14.rail_pin",
        Tier::T1,
        1.0,
        "rail pin faces into the body (R-5)",
    ),
    m(
        "v14.glyph_body",
        Tier::T1,
        1.0,
        "power glyph over a foreign body (issue [3])",
    ),
    // --- Tier 2 — aesthetic refinement ------------------------------
    m("v5", Tier::T2, 1.0, "pin outward-direction violations"),
    m("v16.bends", Tier::T2, 1.0, "V16 bends (B)"),
    m("v16.branches", Tier::T2, 1.0, "V16 branches (J)"),
    m("crossings", Tier::T2, 1.0, "wire crossings"),
    m(
        "detour",
        Tier::T2,
        100.0,
        "wire detour ratio (1 point = 1 pp of excess wire)",
    ),
    m("q3", Tier::T2, 1.0, "Q3 flow-monotonicity violations"),
    m("q5", Tier::T2, 1.0, "Q5 alignment near-misses"),
    m("f3", Tier::T2, 1.0, "F3 flow inversions"),
    m("f4", Tier::T2, 1.0, "F4 terminal-lane violations"),
    m("f5", Tier::T2, 1.0, "F5 series-pose violations"),
    m("p5", Tier::T2, 1.0, "P5 terminal-order violations"),
    m(
        "f6",
        Tier::T2,
        1.0,
        "F6 worst rail-stub lateral run (cells)",
    ),
    m(
        "p11b.movers",
        Tier::T2,
        1.0,
        "P11b cache-less locality: pre-existing symbols moved",
    ),
    // --- informational ----------------------------------------------
    m("q6.cov", Tier::Info, 0.0, "Q6 balance CoV (informational)"),
    // V16 against an ABSOLUTE reference (`bend_bound.rs`). Informational
    // by design, for the reason `docs/invariants.md` V16 gives for not
    // admitting speculative metrics: the bound is provably admissible but
    // deliberately loose (0 or 1 per ink component), so as a gate it
    // would grade noise. It answers the question the ratchets structurally
    // cannot — "how many of these bends were ever avoidable?" — and it
    // must not acquire a vote in the promotion rule on the strength of
    // that.
    m(
        "v16.bend_bound",
        Tier::Info,
        0.0,
        "V16 provable bend lower bound (informational)",
    ),
    m(
        "v16.bend_gap",
        Tier::Info,
        0.0,
        "V16 bends above the provable bound (informational)",
    ),
    m(
        "v16.bend_excess_exact",
        Tier::Info,
        0.0,
        "V16 bends above the EXACT optimum on 2-terminal ink (informational)",
    ),
];

/// Weight applied to the Tier-1 total in the single-scalar aggregate.
///
/// The *rule* is lexicographic — `(T1, T2)`, Tier 1 first — because that
/// is exactly CLAUDE.md's ordering rule ("never introduce a Tier-1
/// regression to improve a Tier-2 metric") lifted from per-fixture to
/// aggregate. The scalar exists only so the verdict has a single
/// readable number; it is order-isomorphic to the lexicographic
/// comparison **provided** `|T2| < TIER1_WEIGHT`, which the report
/// checks and prints rather than assuming.
const TIER1_WEIGHT: f64 = 1000.0;

// ---------------------------------------------------------------------------
// Record loading
// ---------------------------------------------------------------------------

type Cells = BTreeMap<(String, String), f64>; // (metric, fixture) -> value

/// Load every `records-*.tsv` in `dir`.
///
/// Duplicate `(metric, fixture)` rows are expected — several verifiers
/// convert the same fixture — and are collapsed. A duplicate with a
/// *different* value means two measurement sites disagree about one
/// name, which is a defect in the instrumentation, so it is reported
/// rather than silently last-wins.
fn load(dir: &Path) -> (Cells, Vec<String>) {
    let mut cells: Cells = Cells::new();
    let mut conflicts: Vec<String> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (cells, vec![format!("cannot read {}", dir.display())]);
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "tsv"))
        .collect();
    files.sort();
    for f in &files {
        let Ok(body) = std::fs::read_to_string(f) else {
            continue;
        };
        for line in body.lines() {
            let mut it = line.split('\t');
            let (Some(metric), Some(fixture), Some(raw)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            let Ok(v) = raw.parse::<f64>() else { continue };
            let key = (metric.to_string(), fixture.to_string());
            if let Some(&prev) = cells.get(&key) {
                if (prev - v).abs() > 1e-9 {
                    conflicts.push(format!(
                        "{metric} / {fixture}: recorded both {prev} and {v} — two \
                         measurement sites disagree under one metric id"
                    ));
                }
            } else {
                cells.insert(key, v);
            }
        }
    }
    (cells, conflicts)
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

struct TierTotals {
    t1: f64,
    t2: f64,
}

#[allow(clippy::too_many_lines)] // one cohesive report; splitting hides the layout
fn report(champ_dir: &Path, chal_dir: &Path, champ_name: &str, chal_name: &str) -> bool {
    let (champ, c_conf) = load(champ_dir);
    let (chal, x_conf) = load(chal_dir);

    println!("\n=== champion/challenger scoreboard (ADR-23) ===");
    println!("champion   : {champ_name}  ({})", champ_dir.display());
    println!("challenger : {chal_name}  ({})", chal_dir.display());

    for c in c_conf.iter().chain(x_conf.iter()) {
        println!("!! instrumentation conflict: {c}");
    }

    let fixtures: BTreeSet<String> = champ
        .keys()
        .chain(chal.keys())
        .map(|(_, f)| f.clone())
        .collect();

    // --- Tier 0: per-fixture hard, both sides -------------------------
    //
    // Three lists, not two. The absolute state of each side is reported
    // (`*_bad`), but the *promotion gate* keys off `t0_worse` — the
    // cells where the challenger is strictly worse than the champion.
    //
    // That distinction became load-bearing when `f0_defects` was made
    // placer-aware: the `shunt_feedback_amp` Tier-0 net-merge refusal is
    // now instrumented, and it is non-zero **on the champion**. ADR-23
    // could write the gate as "every Tier-0 metric is 0" only because it
    // also recorded that Tier 0 was "cheap to satisfy, since every
    // fixture measures 0 today" — an artefact of that fixture being
    // uninstrumented. Keeping the absolute form would now veto EVERY
    // challenger, including one that leaves the refusal exactly as it
    // found it, which turns the project's strongest acceptance test
    // (ADR-20) into a gate no placer can pass. "No Tier-0 regression" is
    // the rule; against an all-zero champion the two forms coincide
    // exactly, so nothing about the M4 replay's verdict changes.
    let mut t0_champ_bad: Vec<String> = Vec::new();
    let mut t0_chal_bad: Vec<String> = Vec::new();
    let mut t0_worse: Vec<String> = Vec::new();
    for met in METRICS.iter().filter(|m| m.tier == Tier::T0) {
        for f in &fixtures {
            let k = (met.id.to_string(), f.clone());
            if champ.get(&k).is_some_and(|v| *v > 0.0) {
                t0_champ_bad.push(format!("{}/{f} = {}", met.id, champ[&k]));
            }
            if chal.get(&k).is_some_and(|v| *v > 0.0) {
                t0_chal_bad.push(format!("{}/{f} = {}", met.id, chal[&k]));
            }
            // One-sided cells are caught by the `missing` check below,
            // which blocks the verdict outright; only compare pairs.
            if let (Some(&a), Some(&b)) = (champ.get(&k), chal.get(&k))
                && b > a + 1e-9
            {
                t0_worse.push(format!("{}/{f}: {a} -> {b}", met.id));
            }
        }
    }

    // --- Tier 1 / Tier 2 aggregate ------------------------------------
    let mut totals = TierTotals { t1: 0.0, t2: 0.0 };
    let mut missing: Vec<String> = Vec::new();

    println!(
        "\n{:<28} {:<24} {:>10} {:>10} {:>10}",
        "metric", "fixture", champ_name, chal_name, "Δpoints"
    );
    println!("{}", "-".repeat(88));

    for met in METRICS {
        let mut printed_any = false;
        let mut met_delta = 0.0_f64;
        for f in &fixtures {
            let k = (met.id.to_string(), f.clone());
            let (a, b) = (champ.get(&k), chal.get(&k));
            match (a, b) {
                (Some(&a), Some(&b)) => {
                    let d = (b - a) * met.points_per_unit;
                    if met.tier != Tier::Info {
                        match met.tier {
                            Tier::T1 => totals.t1 += d,
                            Tier::T2 => totals.t2 += d,
                            Tier::T0 | Tier::Info => {}
                        }
                    }
                    met_delta += d;
                    if (b - a).abs() > 1e-9 {
                        println!(
                            "{:<28} {:<24} {:>10.4} {:>10.4} {:>+10.2}",
                            met.id, f, a, b, d
                        );
                        printed_any = true;
                    }
                }
                (Some(_), None) => missing.push(format!("{}/{f}: challenger", met.id)),
                (None, Some(_)) => missing.push(format!("{}/{f}: champion", met.id)),
                (None, None) => {}
            }
        }
        if printed_any {
            println!(
                "{:<28} {:<24} {:>10} {:>10} {:>+10.2}   [{:?}] {}",
                "", "  ^ subtotal", "", "", met_delta, met.tier, met.what
            );
        }
    }
    println!("{}", "-".repeat(88));
    println!("(only cells that MOVED are listed; unchanged cells contribute 0)");

    // --- coverage ------------------------------------------------------
    //
    // A cell absent from BOTH sides contributes nothing and is invisible
    // in the table above, so coverage is printed rather than assumed: a
    // verifier that aborted before recording anything would otherwise
    // read as "no change". A registered metric with zero champion cells
    // means its instrumentation did not run at all, which invalidates
    // the comparison.
    println!("\nCoverage (fixtures measured, champion / challenger):");
    let mut uninstrumented: Vec<&str> = Vec::new();
    for met in METRICS {
        let n_a = champ.keys().filter(|(m, _)| m == met.id).count();
        let n_b = chal.keys().filter(|(m, _)| m == met.id).count();
        if n_a == 0 {
            uninstrumented.push(met.id);
        }
        println!("  {:<28} {n_a:>3} / {n_b:>3}", met.id);
    }

    // --- verdict -------------------------------------------------------
    println!("\nTier 0 (per-fixture hard, never aggregated):");
    println!(
        "  champion   : {}",
        if t0_champ_bad.is_empty() {
            "clean".to_string()
        } else {
            t0_champ_bad.join(", ")
        }
    );
    println!(
        "  challenger : {}",
        if t0_chal_bad.is_empty() {
            "clean".to_string()
        } else {
            t0_chal_bad.join(", ")
        }
    );
    println!(
        "  regressed  : {}",
        if t0_worse.is_empty() {
            "none (no Tier-0 cell is worse than the champion)".to_string()
        } else {
            t0_worse.join(", ")
        }
    );

    println!("\nAggregate (lower is better; Δ = challenger − champion):");
    println!("  Tier 1 total Δ = {:+.2} points", totals.t1);
    println!("  Tier 2 total Δ = {:+.2} points", totals.t2);
    let scalar = TIER1_WEIGHT.mul_add(totals.t1, totals.t2);
    let scalar_faithful = totals.t2.abs() < TIER1_WEIGHT;
    println!(
        "  scalar  S = {TIER1_WEIGHT}·T1 + T2 = {scalar:+.2}  \
         (order-isomorphic to the lexicographic rule: {})",
        if scalar_faithful {
            "yes, |T2| < weight"
        } else {
            "NO — |T2| >= weight, read T1/T2 directly"
        }
    );

    if !missing.is_empty() {
        println!(
            "\n!! {} metric cell(s) present on one side only — the comparison is \
             INCOMPLETE and no verdict is issued:\n  {}",
            missing.len(),
            missing.join("\n  ")
        );
    }

    if !uninstrumented.is_empty() {
        println!(
            "\n!! {} registered metric(s) produced NO champion measurement — their \
             verifier did not run, so the comparison is INCOMPLETE: {}",
            uninstrumented.len(),
            uninstrumented.join(", ")
        );
    }

    let tier0_regressed = !t0_worse.is_empty();
    let aggregate_improves = totals.t1 < -1e-9 || (totals.t1.abs() <= 1e-9 && totals.t2 < -1e-9);
    let complete = missing.is_empty() && uninstrumented.is_empty();
    let promotable = !tier0_regressed && aggregate_improves && complete && c_conf.is_empty();

    println!("\nPromotion rule (ADR-23):");
    println!(
        "  no Tier-0 cell worse than the champion .. {}",
        yes_no(!tier0_regressed)
    );
    println!(
        "  (T1, T2) strictly improves lexicographically {}",
        yes_no(aggregate_improves)
    );
    println!(
        "  comparison complete ..................... {}",
        yes_no(complete)
    );
    println!(
        "\nVERDICT: challenger `{chal_name}` is {} against `{champ_name}`.",
        if promotable {
            "PROMOTABLE"
        } else {
            "NOT promotable"
        }
    );
    if promotable {
        println!(
            "On promotion, `baseline_lock` and every per-fixture literal are \
             regenerated at the challenger's values and the zero-slack regime resumes."
        );
    }
    promotable
}

fn yes_no(b: bool) -> &'static str {
    if b { "YES" } else { "no" }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Cheap, always-on guard on the registry itself.
///
/// The aggregate is only meaningful if every metric has exactly one
/// entry and exactly one tier; a duplicated id would double-count a
/// column, and a typo'd id would silently drop one.
#[test]
fn metric_registry_is_wellformed() {
    let mut ids: Vec<&str> = METRICS.iter().map(|m| m.id).collect();
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(n, ids.len(), "duplicate metric id in METRICS");
    for met in METRICS {
        assert!(!met.id.is_empty(), "empty metric id");
        assert!(!met.what.is_empty(), "metric {} has no description", met.id);
        match met.tier {
            Tier::Info => assert!(
                (met.points_per_unit - 0.0).abs() < f64::EPSILON,
                "informational metric {} must not carry aggregate weight",
                met.id
            ),
            _ => assert!(
                met.points_per_unit > 0.0,
                "graded metric {} must carry a positive weight",
                met.id
            ),
        }
    }
    // Tier 0 is never aggregated, so it must not be the only tier a
    // metric family lives in by accident: assert each tier is populated.
    for t in [Tier::T0, Tier::T1, Tier::T2] {
        assert!(
            METRICS.iter().any(|m| m.tier == t),
            "no metric registered in tier {t:?}"
        );
    }
}

/// The report. `#[ignore]`d — it reads measurement directories produced
/// by two prior whole-suite runs and has nothing to say in the default
/// `cargo test` path.
#[test]
#[ignore = "reads two whole-suite measurement runs; see the module docs / `just scoreboard`"]
fn champion_challenger_report() {
    let champ_dir = PathBuf::from(
        std::env::var("S2K_SCOREBOARD_CHAMPION")
            .expect("set S2K_SCOREBOARD_CHAMPION to the champion's record directory"),
    );
    let chal_dir = PathBuf::from(
        std::env::var("S2K_SCOREBOARD_CHALLENGER")
            .expect("set S2K_SCOREBOARD_CHALLENGER to the challenger's record directory"),
    );
    let champ_name = std::env::var("S2K_SCOREBOARD_CHAMPION_NAME").unwrap_or_else(|_| {
        champ_dir
            .file_name()
            .map_or_else(|| "champion".to_string(), |s| s.to_string_lossy().into())
    });
    let chal_name = std::env::var("S2K_SCOREBOARD_CHALLENGER_NAME").unwrap_or_else(|_| {
        chal_dir
            .file_name()
            .map_or_else(|| "challenger".to_string(), |s| s.to_string_lossy().into())
    });

    let promotable = report(&champ_dir, &chal_dir, &champ_name, &chal_name);

    // The report is an instrument, not a gate: it prints the verdict and
    // exits green either way. Promotion is an owner decision taken on
    // the printed table, exactly as CLAUDE.md's global-improvement
    // escape requires — the scoreboard supplies the evidence, it does
    // not grant the exception.
    println!("\n(report complete; promotable = {promotable})");
}
