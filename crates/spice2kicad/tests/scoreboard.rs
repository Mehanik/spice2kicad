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
    // Registered 2026-08-23 by the blind-cell sweep (ADR-23 D10). Each
    // of these graded a Tier-0 property with NO scoreboard cell at all,
    // so the promotion rule's "no Tier-0 regression" clause was reading
    // five metrics where it believed it was reading ten.
    m(
        "t0.erc_errors",
        Tier::T0,
        1.0,
        "V2: `kicad-cli sch erc` error-severity violations",
    ),
    m(
        "t0.netlist_mismatch",
        Tier::T0,
        1.0,
        "emitted schematic differs from the source netlist (missing element / wrong net)",
    ),
    m(
        "t0.nondeterministic",
        Tier::T0,
        1.0,
        "repeat conversions differing from the first (default path)",
    ),
    m(
        "t0.nondeterministic_nocache",
        Tier::T0,
        1.0,
        "P10: two cache-less conversions of one fixture differ",
    ),
    m(
        "t0.sheet_overlap",
        Tier::T0,
        1.0,
        "symbol / power-glyph extent overlapping a hierarchical-sheet body",
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
    // Registered 2026-08-18, after the flow-seed promotion found the
    // hard way that a verifier which reports nothing to the sink is a
    // cell no scoreboard can see move. Both of these graded Tier-1/2
    // properties silently; both moved under the promotion (one for the
    // worse) and neither appeared in the table that graded it. If you
    // add a fixture-enumerating verifier, give it a `record_count` on
    // the line before its assertion — that is the whole contract (D2).
    m(
        "v13.9_foreign_over_glyph",
        Tier::T1,
        1.0,
        "foreign label/wire over a power-glyph body",
    ),
    // Registered 2026-08-23 by the blind-cell sweep (ADR-23 D10). Every
    // one of these was graded by a zero-budget verifier that reported
    // NOTHING to the sink, so the aggregate scored it as "no change" on
    // both sides of every comparison the scoreboard has ever run.
    m(
        "v4.plain_label_excess",
        Tier::T1,
        1.0,
        "V4: nets carrying more plain labels than the cap allows",
    ),
    m(
        "v4.global_label_misuse",
        Tier::T1,
        1.0,
        "V4: global labels on nets that are not one-pin interface nets",
    ),
    m(
        "v10.orphan_pwrflag",
        Tier::T1,
        1.0,
        "V10: PWR_FLAG markers not sitting on drawn circuit geometry",
    ),
    m(
        "v10.spurious_pwrflag",
        Tier::T1,
        1.0,
        "V10: PWR_FLAG on a signal net that already has a passive/driving pin",
    ),
    m(
        "v10.rail_glyph_kind",
        Tier::T1,
        1.0,
        "V10: rail glyph of the wrong kind (GND triangle on a negative rail)",
    ),
    m(
        "v10.power_source_drawn",
        Tier::T1,
        1.0,
        "V10: `*@power` sources still drawn as a symbol instead of a glyph",
    ),
    m(
        "v13.align_text_gap",
        Tier::T1,
        1.0,
        "align-cluster value-text gaps below the ratchet floor",
    ),
    m(
        "v13.label_in_body",
        Tier::T1,
        1.0,
        "label anchors inside a foreign symbol body",
    ),
    m(
        "v13.glyph_on_sheet_port",
        Tier::T1,
        1.0,
        "power glyph anchored on a sheet port pin (overprints the port label)",
    ),
    m(
        "v13.model_ink_escape",
        Tier::T1,
        1.0,
        "text-bbox model disagreeing with real SVG ink — the V13 family's own fidelity",
    ),
    m(
        "v14.glyph_orientation",
        Tier::T1,
        1.0,
        "V14: power glyphs not at their canonical rotation",
    ),
    m(
        "v14.rail_order",
        Tier::T1,
        1.0,
        "V14/R-6: the Power band is not drawn above the Ground band",
    ),
    m(
        "v15.off_page",
        Tier::T1,
        1.0,
        "V15: content or instance anchors outside the A4 usable area",
    ),
    m(
        "wire.same_net_overlap",
        Tier::T1,
        1.0,
        "redundant collinear same-net wire overlaps (duplicated ink)",
    ),
    m(
        "wire.dangling_whisker",
        Tier::T1,
        1.0,
        "wire ends that attach to nothing",
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
    m(
        "p11.cache_out_of_step",
        Tier::T2,
        1.0,
        "P11 cache path: symbols outside the common page-fit delta",
    ),
    // Junction-dot parity (ADR-27). Recorded by `junction_parity.rs`
    // since it landed, but never registered here, so the ADR-23 D9
    // grading could not see `two_stage_amp`'s four cross-net contact
    // points fall 4 -> 0 under `flow-seed`. Tier 1: a junction dot is a
    // readability/ink-correctness property, and its own ratchet is
    // zero-slack in both directions.
    m(
        "junction.missing",
        Tier::T1,
        1.0,
        "junction dots KiCad's rule requires but the emitter omits",
    ),
    m(
        "junction.spurious",
        Tier::T1,
        1.0,
        "junction dots the emitter draws that KiCad's rule forbids",
    ),
    m(
        "junction.cross_net",
        Tier::T1,
        1.0,
        "cross-net collinear contact points (latent short, ADR-27)",
    ),
    m(
        "junction.missing_mid_span",
        Tier::T1,
        1.0,
        "mid-span same-net T-branches drawn without a junction dot",
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
    // ADR-28 — the two things a human reads first. Registered as
    // informational for the same reason as Q6 and the V16 bend bound:
    // their definitions are still being calibrated (ADR-28 lists the
    // open ambiguities), and a metric that can be wrong about a
    // *correct* drawing must not be able to block work. They exist
    // because the aggregate above has three times scored as an
    // improvement something the owner read as damage, and none of the
    // graded metrics measures axis consistency, orientation uniformity
    // or device stacking. What would justify promoting each of them to
    // a weighted metric — and then to a ratchet — is recorded in
    // ADR-28.
    m(
        "chain.axis",
        Tier::Info,
        0.0,
        "series-chain members off the chain's majority axis (informational)",
    ),
    m(
        "chain.reversal",
        Tier::Info,
        0.0,
        "series-chain members reversed against the chain's direction (informational)",
    ),
    m(
        "chain.members",
        Tier::Info,
        0.0,
        "series-chain members measured — the denominator for chain.* (informational)",
    ),
    // ADR-28's own blind spot, closed. `chain.axis` / `chain.reversal`
    // measure a chain's axis UNIFORMITY and DIRECTION; neither measures
    // adjacency, so a chain shattered into separated columns scores a
    // perfect 0 on both. `port_shapes` is drawn as two vertical stacks
    // of two, 31.75 mm apart, and read 0/0.
    m(
        "chain.stranded",
        Tier::Info,
        0.0,
        "series-chain members drawn away from the rest of their chain (informational)",
    ),
    m(
        "chain.run_members",
        Tier::Info,
        0.0,
        "chain members measured for adjacency — the denominator for chain.stranded \
         (informational)",
    ),
    m(
        "stack.side_by_side",
        Tier::Info,
        0.0,
        "DC-series device pairs drawn side-by-side instead of stacked (informational)",
    ),
    m(
        "stack.pairs",
        Tier::Info,
        0.0,
        "DC-series device pairs measured — the denominator for stack.* (informational)",
    ),
    // F2 — device facing. A reader expects a transistor's
    // higher-DC-potential terminal drawn screen-UP; `two_stage_amp`'s
    // `Q2` is emitted upside down and every metric above reads it as
    // clean, because it is locally violation-free. Informational for the
    // same reason as the rest of ADR-28: the rank is derived from DC
    // reachability and it deliberately DECLINES on ties, on
    // bidirectional use and on floating devices, so it must not be able
    // to block work while its decline set is still being learned.
    m(
        "device.facing_inverted",
        Tier::Info,
        0.0,
        "devices drawn with the higher-DC-potential terminal down (informational)",
    ),
    m(
        "device.facing_resolved",
        Tier::Info,
        0.0,
        "devices whose DC facing resolved — the denominator for device.* (informational)",
    ),
    // ADR-28 metric D. `chain.*` and `stack.*` are provably blind to a
    // port terminal drawn on end: they are byte-identical across the two
    // `terminal-series` arms on all 18 fixtures, and the aggregate
    // therefore ranked the arm that leaves a port label vertical ABOVE
    // the arm that repairs every one of them. Registered informational
    // at birth for the same reason as A/B/C — the ambiguities in ADR-28
    // are open, and a metric that can be wrong about a correct drawing
    // must not be able to reject one.
    m(
        "port.label_vertical",
        Tier::Info,
        0.0,
        "port terminals whose label reads across the signal path (informational)",
    ),
    m(
        "port.label_backwards",
        Tier::Info,
        0.0,
        "declared port terminals whose arrow travels leftward (informational)",
    ),
    m(
        "port.labels",
        Tier::Info,
        0.0,
        "port terminal labels measured — the denominator for port.label_vertical \
         (informational)",
    ),
    m(
        "port.directed",
        Tier::Info,
        0.0,
        "declared input/output terminals — the denominator for port.label_backwards \
         (informational)",
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
                        // An informational metric carries no aggregate
                        // weight, so its `d` is 0 by construction — and
                        // printing `+0.00` beside a cell that moved
                        // 0 -> 3 reads as "unchanged". ADR-23 D6
                        // recorded that as a live defect after `q6.cov`
                        // printed `Δ = +0.00` for a value that moved
                        // 1.2247 -> 1.4142. Print the contribution only
                        // where there is one.
                        if met.tier == Tier::Info {
                            println!(
                                "{:<28} {:<24} {:>10.4} {:>10.4} {:>10}",
                                met.id, f, a, b, "(info)"
                            );
                        } else {
                            println!(
                                "{:<28} {:<24} {:>10.4} {:>10.4} {:>+10.2}",
                                met.id, f, a, b, d
                            );
                        }
                        printed_any = true;
                    }
                }
                (Some(_), None) => missing.push(format!("{}/{f}: challenger", met.id)),
                (None, Some(_)) => missing.push(format!("{}/{f}: champion", met.id)),
                (None, None) => {}
            }
        }
        if printed_any {
            if met.tier == Tier::Info {
                println!(
                    "{:<28} {:<24} {:>10} {:>10} {:>10}   [{:?}] {}",
                    "", "  ^ subtotal", "", "", "(info)", met.tier, met.what
                );
            } else {
                println!(
                    "{:<28} {:<24} {:>10} {:>10} {:>+10.2}   [{:?}] {}",
                    "", "  ^ subtotal", "", "", met_delta, met.tier, met.what
                );
            }
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

    // Orphan records: an id present in the .tsv that is in no METRICS
    // row. The sink accepts any string and this function only ever walks
    // METRICS, so such a measurement is written and then dropped — the
    // *recorded-but-unregistered* half of the blind-cell class (ADR-23
    // D10). The `every_recorded_metric_id_is_registered` lint catches
    // every in-tree case at `cargo test` time; this catches a record made
    // from anywhere else, and refuses the verdict rather than quietly
    // grading a table with a hole in it.
    let registered: BTreeSet<&str> = METRICS.iter().map(|m| m.id).collect();
    let orphan_ids: BTreeSet<&str> = champ
        .keys()
        .chain(chal.keys())
        .map(|(m, _)| m.as_str())
        .filter(|m| !registered.contains(m))
        .collect();
    if !orphan_ids.is_empty() {
        println!(
            "\n!! {} recorded metric id(s) are in no METRICS row, so their cells were \
             DROPPED from the table above — the comparison is INCOMPLETE: {}",
            orphan_ids.len(),
            orphan_ids.iter().copied().collect::<Vec<_>>().join(", ")
        );
    }

    let tier0_regressed = !t0_worse.is_empty();
    let aggregate_improves = totals.t1 < -1e-9 || (totals.t1.abs() <= 1e-9 && totals.t2 < -1e-9);
    let complete = missing.is_empty() && uninstrumented.is_empty() && orphan_ids.is_empty();
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

// ---------------------------------------------------------------------------
// Regression guard: no verifier may assert INSIDE a loop that reports a metric
// ---------------------------------------------------------------------------
//
// # The defect this closes
//
// A verifier that calls `assert!` / `panic!` *inside* its per-fixture loop
// aborts the whole test function at the first violating fixture. Every
// fixture after it is then never measured, so it reports NOTHING to the
// sink above — and a truncated metric is indistinguishable here from a
// metric that had nothing to say. `--no-fail-fast` cannot help: the
// truncation is *within* one test function, not across binaries.
//
// This is not hypothetical. ADR-23 D6 records `v13.1_label_body`
// truncating the `m3-signed-gate` row at `named_rails`, and the same
// verifier later truncated three fixtures out of a live challenger's row
// by panicking on `sallen_key_lpf`. It is the same failure as D9's "a
// blind cell is not conservatively blind", reached from a different
// cause: there, the verifier never recorded; here, it stopped recording.
//
// D2's contract is "record on the line before the assertion that grades
// it". The established fix is collect-then-assert: accumulate per-fixture
// failures into a `Vec`, report every fixture's number, assert once after
// the loop. This test is what makes the contract *enforced* rather than
// merely documented.
//
// # What it does and does not catch
//
// Catches: any `assert!` / `assert_eq!` / `assert_ne!` / `debug_assert!` /
// `unreachable!` / bare `panic!` lexically inside a `for` loop body that
// also contains a `common::scoreboard::record*` call, anywhere under
// `tests/`. That is exactly the shape that truncates a metric mid-loop.
//
// Does NOT catch:
//   * a panic raised inside a *helper function* the loop calls (the lint
//     is lexical, not interprocedural);
//   * `.expect(...)` / `.unwrap()` / `unwrap_or_else(|e| panic!(...))` —
//     deliberately exempt. Those signal "this fixture could not be
//     converted or parsed at all", a different failure from "this fixture
//     violates the budget", and `common::spice_to_kicad` already records
//     `t0.convert_fail` for that fixture before it fails. Making them
//     non-aborting would change what a broken conversion *means*, not
//     just when it is reported;
//   * a `while` / `loop` / `.for_each()` fixture sweep (no verifier uses
//     one today);
//   * a verifier that enumerates fixtures and records NOTHING at all —
//     that is D9's blind-cell rule, which this lint's premise (a
//     `record` call in the loop) cannot see. It is a separate obligation,
//     now enforced by `every_fixture_enumerating_verifier_reports_a_metric`
//     at the bottom of this file; and
//   * a verifier that records under an id no METRICS row declares, which
//     `report` silently drops — enforced by
//     `every_recorded_metric_id_is_registered`.

/// The macros that abort a test function where they stand.
///
/// `word_at` matches on identifier boundaries, so the `assert!` entry does
/// NOT match the tail of `debug_assert!` — the debug forms need their own
/// entries, and have them.
const ASSERT_MACROS: &[&str] = &[
    "assert!",
    "assert_eq!",
    "assert_ne!",
    "debug_assert!",
    "debug_assert_eq!",
    "debug_assert_ne!",
    "unreachable!",
    "panic!",
];

/// Overwrite `text[from..to]` with spaces, keeping newlines so line
/// numbers computed over the result still match the original source.
fn blank_range(text: &mut [char], from: usize, to: usize) {
    let to = to.min(text.len());
    for c in text.iter_mut().take(to).skip(from) {
        if *c != '\n' {
            *c = ' ';
        }
    }
}

/// Index just past a `//` line comment starting at `at`.
fn end_of_line_comment(src: &[char], at: usize) -> usize {
    let mut end = at;
    while end < src.len() && src[end] != '\n' {
        end += 1;
    }
    end
}

/// Index just past a (nesting) `/* … */` comment starting at `at`.
fn end_of_block_comment(src: &[char], at: usize) -> usize {
    let mut end = at + 2;
    let mut depth = 1_u32;
    while end < src.len() && depth > 0 {
        if src[end] == '/' && end + 1 < src.len() && src[end + 1] == '*' {
            depth += 1;
            end += 2;
        } else if src[end] == '*' && end + 1 < src.len() && src[end + 1] == '/' {
            depth -= 1;
            end += 2;
        } else {
            end += 1;
        }
    }
    end
}

/// Index just past an `r###"…"###` raw string starting at `at` (which
/// must point at the `r`), or `None` if `at` does not start one.
fn end_of_raw_string(src: &[char], at: usize) -> Option<usize> {
    if at > 0 && (src[at - 1].is_alphanumeric() || src[at - 1] == '_') {
        return None;
    }
    let mut hashes = 0_usize;
    let mut end = at + 1;
    while end < src.len() && src[end] == '#' {
        hashes += 1;
        end += 1;
    }
    if end >= src.len() || src[end] != '"' {
        return None;
    }
    end += 1;
    while end < src.len() {
        if src[end] == '"' {
            let mut probe = end + 1;
            let mut seen = 0_usize;
            while probe < src.len() && src[probe] == '#' && seen < hashes {
                seen += 1;
                probe += 1;
            }
            if seen == hashes {
                return Some(probe);
            }
        }
        end += 1;
    }
    Some(src.len())
}

/// Index just past a `"…"` string starting at `at`.
fn end_of_string(src: &[char], at: usize) -> usize {
    let mut end = at + 1;
    while end < src.len() {
        if src[end] == '\\' {
            end += 2;
        } else if src[end] == '"' {
            return end + 1;
        } else {
            end += 1;
        }
    }
    end
}

/// Index just past a `'x'` char literal starting at `at`, or `None` when
/// the quote opens a lifetime or a loop label (`'fixture: for …`), which
/// must stay visible to the walk.
fn end_of_char_literal(src: &[char], at: usize) -> Option<usize> {
    let escaped = at + 1 < src.len() && src[at + 1] == '\\';
    let plain = at + 2 < src.len() && src[at + 2] == '\'' && src[at + 1] != '\'';
    if !(escaped || plain) {
        return None;
    }
    let mut end = at + 1;
    while end < src.len() {
        if src[end] == '\\' {
            end += 2;
        } else if src[end] == '\'' {
            return Some(end + 1);
        } else {
            end += 1;
        }
    }
    Some(end)
}

/// Replace every comment, string literal and char literal with spaces,
/// preserving line structure, so the brace walk below cannot be fooled by
/// a `{` inside a format string or a `//` inside a comment.
fn blank_literals(src: &str) -> Vec<char> {
    let source: Vec<char> = src.chars().collect();
    let mut out = source.clone();
    let len = source.len();
    let mut at = 0;
    while at < len {
        let next = match source[at] {
            '/' if at + 1 < len && source[at + 1] == '/' => end_of_line_comment(&source, at),
            '/' if at + 1 < len && source[at + 1] == '*' => end_of_block_comment(&source, at),
            'r' if at + 1 < len && (source[at + 1] == '"' || source[at + 1] == '#') => {
                let Some(end) = end_of_raw_string(&source, at) else {
                    at += 1;
                    continue;
                };
                end
            }
            '"' => end_of_string(&source, at),
            '\'' => {
                let Some(end) = end_of_char_literal(&source, at) else {
                    at += 1;
                    continue;
                };
                end
            }
            _ => {
                at += 1;
                continue;
            }
        };
        blank_range(&mut out, at, next);
        at = next.max(at + 1);
    }
    out
}

/// Does `hay[at..]` start with `needle`, on identifier boundaries?
fn word_at(hay: &[char], at: usize, needle: &str) -> bool {
    let w: Vec<char> = needle.chars().collect();
    if at + w.len() > hay.len() || hay[at..at + w.len()] != w[..] {
        return false;
    }
    if at > 0 && (hay[at - 1].is_alphanumeric() || hay[at - 1] == '_') {
        return false;
    }
    let after = at + w.len();
    // A macro name ends in `!`, which is already part of `needle`; a
    // keyword must not be followed by an identifier char.
    if w.last() == Some(&'!') {
        return true;
    }
    !(after < hay.len() && (hay[after].is_alphanumeric() || hay[after] == '_'))
}

/// One `assert`/`panic` found inside a metric-reporting loop.
struct InLoopAssert {
    assert_line: usize,
    loop_line: usize,
    snippet: String,
}

/// Index of the `{` that opens the body of the `for` at `at`, or `None`
/// when this `for` is not a loop header (`impl Trait for Type {` carries
/// no `in`, and its brace is not a loop body).
fn loop_body_open(src: &[char], at: usize) -> Option<usize> {
    let mut pos = at + 3;
    let (mut paren, mut brack) = (0_i32, 0_i32);
    let mut open = None;
    while pos < src.len() {
        match src[pos] {
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => brack += 1,
            ']' => brack -= 1,
            '{' if paren == 0 && brack == 0 => {
                open = Some(pos);
                break;
            }
            ';' | '}' if paren == 0 && brack == 0 => break,
            _ => {}
        }
        pos += 1;
    }
    let open = open?;
    let header: String = src[at..open].iter().collect();
    header.split_whitespace().any(|t| t == "in").then_some(open)
}

/// Index of the `}` matching the `{` at `open`.
fn matching_brace(src: &[char], open: usize) -> usize {
    let mut depth = 0_i32;
    let mut pos = open;
    while pos < src.len() {
        match src[pos] {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return pos;
                }
            }
            _ => {}
        }
        pos += 1;
    }
    src.len()
}

/// Result of scanning one file: the offences, and how many loop bodies
/// containing a `scoreboard::record*` call were seen (the vacuity guard).
fn scan_source(src: &str) -> (Vec<InLoopAssert>, usize) {
    let ch = blank_literals(src);
    let len = ch.len();
    let line_of = |idx: usize| ch.iter().take(idx.min(len)).filter(|&&c| c == '\n').count() + 1;
    let raw_lines: Vec<&str> = src.lines().collect();

    // Innermost enclosing record-bearing loop, per offending line.
    let mut found: BTreeMap<usize, InLoopAssert> = BTreeMap::new();
    let mut recording_loops = 0_usize;

    let mut at = 0;
    while at < len {
        if !word_at(&ch, at, "for") {
            at += 1;
            continue;
        }
        let Some(open) = loop_body_open(&ch, at) else {
            at += 3;
            continue;
        };
        let close = matching_brace(&ch, open);
        let body: String = ch[open + 1..close.min(len)].iter().collect();
        if body.contains("scoreboard::record") {
            recording_loops += 1;
            let loop_line = line_of(at);
            for pos in (open + 1)..close {
                if !ASSERT_MACROS.iter().any(|m| word_at(&ch, pos, m)) {
                    continue;
                }
                let line = line_of(pos);
                let raw = raw_lines.get(line - 1).unwrap_or(&"").trim();
                // Exempt the "this fixture would not convert/parse"
                // idiom — see the module note above.
                if raw.contains("_or_else") {
                    continue;
                }
                found
                    .entry(line)
                    .and_modify(|e| e.loop_line = e.loop_line.max(loop_line))
                    .or_insert(InLoopAssert {
                        assert_line: line,
                        loop_line,
                        snippet: raw.chars().take(90).collect(),
                    });
            }
        }
        at += 3;
    }
    (found.into_values().collect(), recording_loops)
}

#[test]
fn no_verifier_asserts_inside_a_loop_that_reports_a_metric() {
    let tests_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut sources: Vec<PathBuf> = Vec::new();
    for dir in [tests_dir.clone(), tests_dir.join("common")] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut v: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "rs"))
            .collect();
        v.sort();
        sources.append(&mut v);
    }

    let mut offences: Vec<String> = Vec::new();
    let mut recording_loops = 0_usize;
    for path in &sources {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let (hits, loops) = scan_source(&src);
        recording_loops += loops;
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        for h in hits {
            offences.push(format!(
                "{name}:{} `{}` — inside the loop opened at line {}, which reports a \
                 metric to the ADR-23 sink. A violating fixture aborts the loop here, \
                 so every LATER fixture is never measured and its cell reads as \
                 \"nothing to say\". Use collect-then-assert: push the failure into a \
                 Vec, keep recording, assert once after the loop.",
                h.assert_line, h.snippet, h.loop_line,
            ));
        }
    }

    // Vacuity guard on the lint itself. If the sources stop being found
    // (a moved directory, a renamed sink) this test would pass while
    // checking nothing — which is the very failure mode it exists to
    // prevent, one level up.
    assert!(
        sources.len() >= 20,
        "scanned only {} test source(s) under {} — the lint is not seeing the suite",
        sources.len(),
        tests_dir.display(),
    );
    assert!(
        recording_loops >= 25,
        "found only {recording_loops} metric-reporting loop(s) — the lint's premise \
         (a `scoreboard::record*` call inside a `for` body) stopped matching, so it \
         would pass vacuously",
    );

    assert!(
        offences.is_empty(),
        "{} verifier assertion(s) sit inside a metric-reporting loop and would \
         truncate the scoreboard record:\n  {}",
        offences.len(),
        offences.join("\n  "),
    );
}

#[test]
fn the_in_loop_assert_lint_is_sensitive() {
    // Mutation guard. A lint validated only against a clean tree is
    // validated against nothing: these two snippets differ by exactly the
    // defect, and the lint must separate them.
    let bad = r#"
fn v() {
    for name in FIXTURES {
        let hits = measure(name);
        common::scoreboard::record_count("v13.1_label_body", name, hits);
        assert!(hits <= 0, "{name}: {hits} overlaps");
    }
}
"#;
    let good = r#"
fn v() {
    let mut failures: Vec<String> = Vec::new();
    for name in FIXTURES {
        let hits = measure(name);
        common::scoreboard::record_count("v13.1_label_body", name, hits);
        if hits > 0 {
            failures.push(format!("{name}: {hits} overlaps"));
        }
    }
    assert!(failures.is_empty(), "V13(1): {}", failures.join("\n"));
}
"#;
    let (bad_hits, bad_loops) = scan_source(bad);
    assert_eq!(bad_loops, 1, "the reporting loop was not recognised");
    assert_eq!(bad_hits.len(), 1, "the in-loop assert was not caught");
    assert_eq!(bad_hits[0].assert_line, 6);

    let (good_hits, good_loops) = scan_source(good);
    assert_eq!(good_loops, 1, "the reporting loop was not recognised");
    assert!(
        good_hits.is_empty(),
        "collect-then-assert was flagged: {:?}",
        good_hits.iter().map(|h| h.assert_line).collect::<Vec<_>>(),
    );

    // A `{` inside a format string must not shift the brace walk, or the
    // loop body would be mis-delimited and the lint would drift silently.
    let braces_in_strings = r#"
fn v() {
    for name in FIXTURES {
        println!("{name} } { ");
        common::scoreboard::record_count("m", name, 1);
        assert!(false, "} {");
    }
}
"#;
    let (h, l) = scan_source(braces_in_strings);
    assert_eq!(
        l, 1,
        "a closing brace inside a string closed the body early"
    );
    assert_eq!(h.len(), 1, "the in-loop assert was lost to a string brace");

    // `impl Trait for Type` is not a loop.
    let impl_block = r#"
impl Foo for Bar {
    fn go(&self) {
        common::scoreboard::record_count("m", "f", 1);
        assert!(true);
    }
}
"#;
    let (_, l) = scan_source(impl_block);
    assert_eq!(l, 0, "an `impl … for …` block was mistaken for a loop");

    // The conversion-failure idiom stays exempt.
    let helper_panic = r#"
fn v() {
    for name in FIXTURES {
        let r = analyse(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        common::scoreboard::record_count("m", name, r);
    }
}
"#;
    let (h, l) = scan_source(helper_panic);
    assert_eq!(l, 1);
    assert!(h.is_empty(), "`unwrap_or_else(|e| panic!(…))` is exempt");
}

// ---------------------------------------------------------------------------
// Regression guard: no verifier and no metric id may be BLIND
// ---------------------------------------------------------------------------
//
// # The defect this closes (ADR-23 D9 / D10)
//
// The report above prints "comparison complete" from the metrics that
// *reported*. A verifier that reports nothing is indistinguishable, in
// every cell of that table, from a metric with nothing to say — so the
// promotion rule can pass while whole invariants are unwatched. D9's own
// wording: **a blind cell is not conservatively blind.** It is scored as
// `0.00` change, which is a claim, not an abstention.
//
// This is measured history, not a worry. Registering five silent metrics
// during the `flow-seed` promotion moved the Tier-1 aggregate from −1.00
// to −4.00, and one of the five was a genuine Tier-1 regression that no
// table had shown. Blindness has two distinct causes and needs two
// distinct lints:
//
//   1. **The verifier never records.** Caught by
//      [`every_fixture_enumerating_verifier_reports_a_metric`].
//   2. **The verifier records under an id nobody registered.** The sink
//      accepts any string; [`report`] only ever iterates [`METRICS`], so
//      an unregistered id lands in the `.tsv` and is then dropped on the
//      floor. Caught by [`every_recorded_metric_id_is_registered`], and —
//      for records made outside this crate's `tests/` tree — by the
//      orphan-record check inside [`report`] itself.
//
// Cause 2 is not hypothetical either: `pwr_flags_sit_on_existing_drawn_geometry`
// shipped in `9296edc` recording `v10.orphan_pwrflag`, an id that was in
// no registry, so its Tier-1 floor was invisible to the scoreboard from
// the day it landed.
//
// The sibling lint above (`no_verifier_asserts_inside_a_loop_that_reports_a_metric`)
// catches a THIRD cause — a verifier that *stops* recording mid-loop —
// and by construction cannot catch either of these two: its premise is a
// `record` call the blind verifier does not have.

/// Blank out comments, keeping string literals and byte offsets.
///
/// The complement of [`blank_literals`]: that one hides strings (so the
/// brace walk is not fooled by a `{` inside a message) and is what the
/// in-loop lint needs. Here the string literal *is* the payload — a
/// metric id — while a `record` call named in a comment must not count.
fn blank_comments(src: &str) -> Vec<char> {
    let s: Vec<char> = src.chars().collect();
    let mut out = s.clone();
    let mut at = 0;
    while at < s.len() {
        // Skip over string / char literals verbatim, so a `//` inside one
        // does not start a comment.
        let next = match s[at] {
            '/' if at + 1 < s.len() && s[at + 1] == '/' => {
                let e = end_of_line_comment(&s, at);
                blank_range(&mut out, at, e);
                e
            }
            '/' if at + 1 < s.len() && s[at + 1] == '*' => {
                let e = end_of_block_comment(&s, at);
                blank_range(&mut out, at, e);
                e
            }
            'r' if end_of_raw_string(&s, at).is_some() => {
                end_of_raw_string(&s, at).unwrap_or(at + 1)
            }
            '"' => end_of_string(&s, at),
            '\'' => end_of_char_literal(&s, at).unwrap_or(at + 1),
            _ => at + 1,
        };
        at = next.max(at + 1);
    }
    out
}

/// Every metric id recorded through the sink in one source file, with the
/// 1-based line it sits on.
fn recorded_metric_ids(src: &str) -> Vec<(usize, String)> {
    let ch = blank_comments(src);
    let mut out = Vec::new();
    let mut at = 0;
    while at < ch.len() {
        if !(ch[at] == 's' && word_at(&ch, at, "scoreboard")) {
            at += 1;
            continue;
        }
        // `scoreboard::record` / `scoreboard::record_count`, then `(`,
        // then (possibly after a newline) the id literal.
        let mut p = at + "scoreboard".len();
        if !(p + 2 < ch.len() && ch[p] == ':' && ch[p + 1] == ':') {
            at += 1;
            continue;
        }
        p += 2;
        if !word_at(&ch, p, "record") && !word_at(&ch, p, "record_count") {
            at += 1;
            continue;
        }
        while p < ch.len() && ch[p] != '(' {
            p += 1;
        }
        while p < ch.len() && ch[p] != '"' {
            p += 1;
        }
        let start = p + 1;
        let mut end = start;
        while end < ch.len() && ch[end] != '"' {
            end += 1;
        }
        if end < ch.len() {
            let line = ch.iter().take(at).filter(|&&c| c == '\n').count() + 1;
            out.push((line, ch[start..end].iter().collect::<String>()));
        }
        at = end.max(at + 1);
    }
    out
}

/// Every `.rs` file under `tests/` and `tests/common/`.
fn suite_sources() -> Vec<PathBuf> {
    let tests_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut sources: Vec<PathBuf> = Vec::new();
    for dir in [tests_dir.clone(), tests_dir.join("common")] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut v: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "rs"))
            .collect();
        v.sort();
        sources.append(&mut v);
    }
    sources
}

fn file_name_of(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[test]
fn every_recorded_metric_id_is_registered() {
    let registered: BTreeSet<&str> = METRICS.iter().map(|m| m.id).collect();
    let mut offences: Vec<String> = Vec::new();
    let mut sites = 0_usize;
    for path in &suite_sources() {
        let name = file_name_of(path);
        // This file records nothing; its `record_count("m", …)` strings
        // are the lint fixtures below, deliberately naming ids that are
        // not in the registry.
        if name == "scoreboard.rs" {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        for (line, id) in recorded_metric_ids(&src) {
            sites += 1;
            if !registered.contains(id.as_str()) {
                offences.push(format!(
                    "{name}:{line} records `{id}`, which is in no METRICS entry. \
                     `report` only ever iterates METRICS, so this measurement is \
                     written to the .tsv and then dropped: the cell reads as \
                     \"nothing to say\" on both sides of every comparison. Add an \
                     `m(\"{id}\", Tier::…, …)` row, or stop recording it."
                ));
            }
        }
    }
    assert!(
        sites >= 40,
        "found only {sites} recording site(s) — the id scanner stopped matching, \
         so this lint would pass vacuously",
    );
    assert!(
        offences.is_empty(),
        "{} recorded metric id(s) are invisible to the scoreboard:\n  {}",
        offences.len(),
        offences.join("\n  "),
    );
}

#[test]
fn every_registered_metric_has_a_recording_site() {
    let mut recorded: BTreeSet<String> = BTreeSet::new();
    for path in &suite_sources() {
        if file_name_of(path) == "scoreboard.rs" {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        recorded.extend(recorded_metric_ids(&src).into_iter().map(|(_, id)| id));
    }
    let orphans: Vec<&str> = METRICS
        .iter()
        .map(|m| m.id)
        .filter(|id| !recorded.contains(*id))
        .collect();
    assert!(
        orphans.is_empty(),
        "{} registered metric(s) have no recording site in the suite, so their \
         column is permanently empty and `report`'s completeness check would \
         veto every comparison: {}",
        orphans.len(),
        orphans.join(", "),
    );
}

// --- the fixture-enumeration scanner ---------------------------------------

/// One `fn` item: its name, the offset of the `fn` keyword, and its body.
struct FnItem {
    name: String,
    header: usize,
    open: usize,
    close: usize,
    is_test: bool,
}

/// Every `fn` item in a literal-blanked source, with `#[test]` resolved by
/// *association* rather than by a look-back window: each `#[test]`
/// attribute belongs to the next `fn` keyword after it, which a fixed-size
/// look-back cannot say (a short test body puts the previous `#[test]`
/// inside the next item's window).
fn scan_fn_items(ch: &[char]) -> Vec<FnItem> {
    let mut items: Vec<FnItem> = Vec::new();
    let mut at = 0;
    while at < ch.len() {
        if !word_at(ch, at, "fn") {
            at += 1;
            continue;
        }
        let mut p = at + 2;
        while p < ch.len() && ch[p].is_whitespace() {
            p += 1;
        }
        let ns = p;
        while p < ch.len() && (ch[p].is_alphanumeric() || ch[p] == '_') {
            p += 1;
        }
        if p == ns {
            at += 2;
            continue;
        }
        let name: String = ch[ns..p].iter().collect();
        // First `{` at paren/bracket depth 0 opens the body.
        let (mut paren, mut brack) = (0_i32, 0_i32);
        let mut open = None;
        while p < ch.len() {
            match ch[p] {
                '(' => paren += 1,
                ')' => paren -= 1,
                '[' => brack += 1,
                ']' => brack -= 1,
                '{' if paren == 0 && brack == 0 => {
                    open = Some(p);
                    break;
                }
                ';' if paren == 0 && brack == 0 => break, // a trait/extern signature
                _ => {}
            }
            p += 1;
        }
        let Some(open) = open else {
            at += 2;
            continue;
        };
        let close = matching_brace(ch, open);
        items.push(FnItem {
            name,
            header: at,
            open,
            close,
            is_test: false,
        });
        at += 2;
    }
    // Associate each `#[test]` with the next `fn` after it.
    let mut at = 0;
    while at < ch.len() {
        if word_at(ch, at, "#[test]") || (ch[at] == '#' && word_at(ch, at + 1, "[test]")) {
            if let Some(i) = items.iter().position(|f| f.header > at) {
                items[i].is_test = true;
            }
            at += 7;
        } else {
            at += 1;
        }
    }
    items
}

/// Text spans that could name a list of fixtures: `const`/`static`/`let`
/// initialisers and non-test `fn` bodies.
fn candidate_bindings(ch: &[char], items: &[FnItem]) -> Vec<(String, usize, usize)> {
    let mut out: Vec<(String, usize, usize)> = Vec::new();
    let mut at = 0;
    while at < ch.len() {
        let kw = ["const", "static", "let"]
            .into_iter()
            .find(|k| word_at(ch, at, k));
        let Some(kw) = kw else {
            at += 1;
            continue;
        };
        let mut p = at + kw.len();
        while p < ch.len() && ch[p].is_whitespace() {
            p += 1;
        }
        if word_at(ch, p, "mut") {
            p += 3;
            while p < ch.len() && ch[p].is_whitespace() {
                p += 1;
            }
        }
        let ns = p;
        while p < ch.len() && (ch[p].is_alphanumeric() || ch[p] == '_') {
            p += 1;
        }
        if p == ns {
            at += kw.len();
            continue;
        }
        let name: String = ch[ns..p].iter().collect();
        // Skip the type annotation, then take everything to the `;` that
        // closes the initialiser at depth 0.
        let mut d = 0_i32;
        let mut eq = None;
        while p < ch.len() {
            match ch[p] {
                '(' | '[' | '{' => d += 1,
                ')' | ']' | '}' => d -= 1,
                '=' if d == 0 && ch.get(p + 1) != Some(&'=') => {
                    eq = Some(p);
                    break;
                }
                ';' if d == 0 => break,
                _ => {}
            }
            p += 1;
        }
        let Some(eq) = eq else {
            at += kw.len();
            continue;
        };
        let mut d = 0_i32;
        let mut q = eq + 1;
        while q < ch.len() {
            match ch[q] {
                '(' | '[' | '{' => d += 1,
                ')' | ']' | '}' => d -= 1,
                ';' if d == 0 => break,
                _ => {}
            }
            q += 1;
        }
        out.push((name, eq + 1, q.min(ch.len())));
        at = eq + 1;
    }
    for f in items.iter().filter(|f| !f.is_test) {
        out.push((f.name.clone(), f.open, f.close));
    }
    out
}

/// One fixture-enumerating verifier and how it was recognised.
struct Enumerating {
    name: String,
    line: usize,
    via: String,
    records: bool,
}

/// Find every fixture-enumerating `#[test]` fn in one source file.
///
/// "Fixture-enumerating" is *measured*, not listed: `fixtures` is the set
/// of `.cir` stems that actually exist on disk, and a binding (a `const`,
/// a `let`, or a helper `fn`) counts as a fixture list when it names three
/// or more of them. A test that loops over such a binding — directly, or
/// through a helper that wraps one — grades emitted output on several
/// circuits, which is exactly the shape that owes the sink a number.
fn scan_fixture_enumerating(src: &str, fixtures: &BTreeSet<String>) -> Vec<Enumerating> {
    let ch = blank_literals(src);
    let raw: Vec<char> = src.chars().collect();
    let items = scan_fn_items(&ch);
    let bindings = candidate_bindings(&ch, &items);

    let names_in = |lo: usize, hi: usize| -> usize {
        let text: String = raw[lo.min(raw.len())..hi.min(raw.len())].iter().collect();
        fixtures
            .iter()
            .filter(|f| text.contains(&format!("\"{f}\"")))
            .count()
    };

    // A binding is a fixture list when it names >= 3 fixtures directly,
    // or when it *wraps* one that does — `placement_quality::fixtures()`
    // joins `FIXTURES_FOR_QUALITY` to paths, and `electrical_safety`'s
    // `with_sheets` prepends one name to `SHEETS`. Exactly ONE hop, and
    // only from a directly-qualifying list: making the relation
    // transitive turns it into ordinary dataflow — `let root =
    // parse(&sch)` inside a fixture loop would inherit it — and then
    // every local in the file reads as a "fixture list".
    let direct: BTreeSet<String> = bindings
        .iter()
        .filter(|(_, lo, hi)| names_in(*lo, *hi) >= 3)
        .map(|(n, _, _)| n.clone())
        .collect();
    let mut sources = direct.clone();
    for (name, lo, hi) in &bindings {
        if sources.contains(name) {
            continue;
        }
        let code: String = ch[(*lo).min(ch.len())..(*hi).min(ch.len())]
            .iter()
            .collect();
        if direct.iter().any(|s| {
            code.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .any(|t| t == s)
        }) {
            sources.insert(name.clone());
        }
    }

    let line_of = |idx: usize| {
        ch.iter()
            .take(idx.min(ch.len()))
            .filter(|&&c| c == '\n')
            .count()
            + 1
    };
    let mut out = Vec::new();
    for f in items.iter().filter(|f| f.is_test) {
        let body_code: String = ch[f.open..f.close.min(ch.len())].iter().collect();
        let mut via: Option<String> = None;
        let mut at = f.open;
        while at < f.close {
            if word_at(&ch, at, "for")
                && let Some(open) = loop_body_open(&ch, at)
                && open < f.close
            {
                let header: String = ch[at..open].iter().collect();
                for s in &sources {
                    if header
                        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .any(|t| t == s)
                    {
                        via.get_or_insert_with(|| format!("loops over `{s}`"));
                    }
                }
            }
            at += 1;
        }
        if via.is_none() && body_code.contains("for ") && names_in(f.open, f.close) >= 3 {
            via = Some(format!(
                "names {} fixtures inline and loops",
                names_in(f.open, f.close)
            ));
        }
        if let Some(via) = via {
            out.push(Enumerating {
                name: f.name.clone(),
                line: line_of(f.header),
                via,
                records: body_code.contains("scoreboard::record"),
            });
        }
    }
    out
}

/// Fixture-enumerating verifiers that deliberately report NOTHING.
///
/// **This list is the point of the lint.** Silence about an invariant is
/// only defensible when someone wrote down why, so a blind verifier is
/// either registered here with a reason or it is a defect. Entries expire
/// on their own: the lint fails on a stale row exactly as it fails on an
/// unregistered blind cell, so a verifier that later learns to record
/// forces its exemption to be deleted.
///
/// The bar for adding a row is the same one `docs/invariants.md` sets for
/// admitting a metric, read backwards: the verifier must have no scalar
/// that would mean anything in a champion/challenger table — because it
/// grades the *verifier* rather than the drawing, because its number is a
/// tautology of the comparison itself, or because its violations are
/// already counted under an id that IS registered (a second id would
/// double-count them).
const BLIND_CELL_EXEMPT: &[(&str, &str, &str)] = &[
    (
        "baseline_lock.rs",
        "baseline_lock_all_fixtures",
        "the OTHER instrument. Its scalar is \"differs from the champion's recorded \
         geometry\", which ADR-23's opening finding says is 1 for essentially every \
         element of any real challenger — \"regression and difference are the same \
         measurement\" against a baseline sampled from the incumbent. A cell that is \
         0 on the champion by construction and saturated on the challenger by \
         construction grades nothing; the blast radius it does measure is reported in \
         the commit message, where a human reads it.",
    ),
    (
        "electrical_safety.rs",
        "item3_interface_global_labels_clear_foreign_bodies",
        "a focused GUARD on a strict subset of `v13_labels_dont_overlap_symbol_body` \
         (global labels only, same fixtures, same geometry), and that verifier already \
         records every one of these hits under `v13.1_label_body`. A second id would \
         count the same overlap twice in the Tier-1 aggregate.",
    ),
    (
        "junction_parity.rs",
        "the_junction_rule_reproduction_is_sensitive",
        "a mutation guard: it perturbs the ink on purpose and asserts the reproduction \
         of KiCad's junction rule NOTICES. Its number grades the verifier, not the \
         drawing, and a challenger cannot move it.",
    ),
    (
        "junction_parity.rs",
        "report_pin_anchored_branch_share",
        "an informational reporter for a PROPOSED V16 `J` redefinition (ADR-27), \
         explicitly \"reported, never asserted\". Registering it would put a candidate \
         metric definition — one the project has deliberately not adopted — into the \
         instrument that selects placers, which is what `docs/invariants.md` V16 \
         forbids when it refuses speculative metrics. It gets a cell on the day the \
         redefinition is signed off, not before.",
    ),
    (
        "port_terminals.rs",
        "erc_clean_on_port_annotated_fixtures",
        "a focused re-run of ERC on the port-annotated fixtures; all eleven are \
         already in `PHASE1_ERC_FIXTURES`, which records the same violation count \
         under `t0.erc_errors`. A second id would be a second name for one ERC \
         property on one set of files.",
    ),
    (
        "readability_metrics.rs",
        "the_facing_rank_is_a_property_of_the_netlist_not_the_placer",
        "a falsifiability guard on metric D, not a measurement of any drawing: it \
         asserts that `device.facing_resolved` — which devices the DC rank resolves \
         at all — is identical under two placers, i.e. that the DENOMINATOR is a \
         property of the netlist. A challenger cannot move it without falsifying the \
         assertion itself. It also converts one arm under a pinned `--placer champion`, \
         and recording a pinned arm's numbers would file them under the row being \
         collected for a different placer (see `convert`'s own note). The number it \
         does grade — the inverted COUNT — is already recorded per fixture by \
         `readability_metrics_are_reported_for_every_fixture` under \
         `device.facing_inverted`.",
    ),
    (
        "readability_metrics.rs",
        "port_label_direction_ranks_the_divider_arm_above_terminal_series",
        "a specimen RANKING over two transcribed, non-`master` placer arms (ADR-28's \
         second amendment). Its own scalar is the frozen arms' totals, which no \
         challenger can move — it grades the metric's arithmetic, not the drawing. \
         The live loop it does run only re-reads the shipping default's per-fixture \
         values, every one of which `readability_metrics_are_reported_for_every_fixture` \
         already records under `port.label_vertical` / `port.label_backwards`; a second \
         id would count the same terminals twice.",
    ),
    (
        "roundtrip_connectivity.rs",
        "the_reconstruction_is_sensitive_on_real_fixtures",
        "a mutation guard on the ADR-22 net-partition reconstruction: it corrupts the \
         emitted ink and asserts the certificate fails. Grades the verifier, not the \
         drawing.",
    ),
    (
        "visual_quality.rs",
        "smoke_fixtures_list_complete",
        "asserts that each `.cir` named in `FIXTURES` exists on disk. A property of \
         the repository, not of any emitted schematic — identical for every placer.",
    ),
];

#[test]
fn every_fixture_enumerating_verifier_reports_a_metric() {
    let tests_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let fixtures: BTreeSet<String> = std::fs::read_dir(tests_dir.join("fixtures"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "cir"))
                .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
                .collect()
        })
        .unwrap_or_default();

    let sources = suite_sources();
    // `S2K_BLIND_DUMP=1` prints the whole audit — every verifier the
    // scanner classifies as fixture-enumerating, and whether it reports.
    // The classification is the load-bearing half of this lint, so it has
    // to be readable without editing the lint.
    let dump = std::env::var_os("S2K_BLIND_DUMP").is_some();
    let mut enumerating = 0_usize;
    let mut offences: Vec<String> = Vec::new();
    let mut seen_exempt: BTreeSet<(String, String)> = BTreeSet::new();
    for path in &sources {
        let name = file_name_of(path);
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        for e in scan_fixture_enumerating(&src, &fixtures) {
            enumerating += 1;
            if dump {
                let tag = if e.records { "reports" } else { "BLIND  " };
                println!("{tag}  {name}:{:<5} {:<58} ({})", e.line, e.name, e.via);
            }
            if e.records {
                continue;
            }
            if let Some((_, _, why)) = BLIND_CELL_EXEMPT
                .iter()
                .find(|(f, t, _)| *f == name && *t == e.name)
            {
                seen_exempt.insert((name.clone(), e.name.clone()));
                if dump {
                    println!("          exempt: {why}");
                }
                continue;
            }
            offences.push(format!(
                "{name}:{} `{}` ({}) records NO metric. A verifier that reports \
                 nothing is scored by the scoreboard as \"no change\", which is a \
                 claim and not an abstention (ADR-23 D9: a blind cell is not \
                 conservatively blind). Add a `common::scoreboard::record_count(…)` \
                 on the line before the assertion that already grades it — most \
                 pass-fail gates have an obvious count, and a boolean gate records \
                 0-or-1 — and register the id in METRICS. If the verifier genuinely \
                 has no scalar worth comparing, add a justified row to \
                 BLIND_CELL_EXEMPT.",
                e.line, e.name, e.via,
            ));
        }
    }

    if dump {
        println!(
            "-- {enumerating} fixture-enumerating verifier(s) over {} fixtures",
            fixtures.len()
        );
    }

    // Vacuity guards. A lint whose premise stopped matching passes while
    // checking nothing — which is the very failure it exists to prevent.
    assert!(
        fixtures.len() >= 10,
        "found only {} fixture(s) under tests/fixtures — the scanner cannot \
         recognise a fixture list without them",
        fixtures.len(),
    );
    assert!(
        sources.len() >= 20,
        "scanned only {} test source(s) — the lint is not seeing the suite",
        sources.len(),
    );
    assert!(
        enumerating >= 40,
        "found only {enumerating} fixture-enumerating verifier(s) — the scanner's \
         premise stopped matching, so this lint would pass vacuously",
    );

    // Exemptions expire on their own: a row that no longer names a blind
    // fixture-enumerating verifier is stale and must be deleted, exactly
    // as `common::xfail`'s registry expires.
    let stale: Vec<String> = BLIND_CELL_EXEMPT
        .iter()
        .filter(|(f, t, _)| !seen_exempt.contains(&((*f).to_string(), (*t).to_string())))
        .map(|(f, t, _)| format!("{f}::{t}"))
        .collect();
    assert!(
        stale.is_empty(),
        "{} BLIND_CELL_EXEMPT row(s) no longer name a blind fixture-enumerating \
         verifier — it now records, or it was renamed or deleted. Delete the row: \
         {}",
        stale.len(),
        stale.join(", "),
    );

    assert!(
        offences.is_empty(),
        "{} fixture-enumerating verifier(s) are blind to the ADR-23 scoreboard:\n  {}",
        offences.len(),
        offences.join("\n  "),
    );
}

#[test]
fn the_blind_cell_lint_is_sensitive() {
    // Mutation guard. A lint validated only against a clean tree is
    // validated against nothing: these snippets differ by exactly the
    // defect, and the scanners must separate them.
    let fixtures: BTreeSet<String> = ["alpha", "beta", "gamma", "delta"]
        .into_iter()
        .map(str::to_string)
        .collect();

    let blind = r#"
const SHEETS: &[&str] = &["alpha", "beta", "gamma"];
#[test]
fn v() {
    let mut failures = Vec::new();
    for name in SHEETS {
        if measure(name) > 0 { failures.push(name); }
    }
    assert!(failures.is_empty());
}
"#;
    let found = scan_fixture_enumerating(blind, &fixtures);
    assert_eq!(
        found.len(),
        1,
        "the fixture-enumerating verifier was missed"
    );
    assert!(
        !found[0].records,
        "a verifier with no `record` read as reporting"
    );

    let sighted = blind.replace(
        "        if measure(name) > 0",
        "        common::scoreboard::record_count(\"m.x\", name, 0);\n        if measure(name) > 0",
    );
    let found = scan_fixture_enumerating(&sighted, &fixtures);
    assert_eq!(found.len(), 1);
    assert!(found[0].records, "a recording verifier read as blind");

    // A list of two fixtures is not a fixture list (a focused,
    // single-case regression guard is out of scope by design).
    let two = blind.replace("\"alpha\", \"beta\", \"gamma\"", "\"alpha\", \"beta\"");
    assert!(
        scan_fixture_enumerating(&two, &fixtures).is_empty(),
        "a two-fixture list was treated as an enumeration",
    );

    // The list reached through a helper `fn` still counts — that is how
    // `placement_quality::fixtures()` and `electrical_safety`'s
    // `with_sheets` are written, and a scanner that missed it would let
    // twelve real verifiers through.
    let indirect = r#"
const SHEETS: &[&str] = &["alpha", "beta", "gamma"];
fn cases() -> Vec<&'static str> { SHEETS.to_vec() }
#[test]
fn v() {
    for name in cases() {
        let _ = measure(name);
    }
}
"#;
    let found = scan_fixture_enumerating(indirect, &fixtures);
    assert_eq!(found.len(), 1, "the indirect fixture list was missed");
    assert!(!found[0].records);

    // A non-test helper that loops over the list is not a verifier.
    let helper_only = r#"
const SHEETS: &[&str] = &["alpha", "beta", "gamma"];
fn measure_all() {
    for name in SHEETS {
        let _ = measure(name);
    }
}
"#;
    assert!(
        scan_fixture_enumerating(helper_only, &fixtures).is_empty(),
        "a non-test helper was graded as a verifier",
    );

    // `#[test]` binds to the NEXT fn, not to whatever sits within a
    // fixed look-back window of one.
    let short_bodies = r#"
const SHEETS: &[&str] = &["alpha", "beta", "gamma"];
#[test]
fn a() { assert!(true); }
fn b() { for name in SHEETS { let _ = name; } }
"#;
    assert!(
        scan_fixture_enumerating(short_bodies, &fixtures).is_empty(),
        "a plain fn following a short test was mistaken for a test",
    );

    // --- the id scanner --------------------------------------------------
    let ids = recorded_metric_ids(
        "fn v() {\n    common::scoreboard::record_count(\n        \"v16.bends\",\n        name,\n        1,\n    );\n}\n",
    );
    assert_eq!(ids.len(), 1, "a multi-line record call was missed");
    assert_eq!(ids[0].1, "v16.bends");

    let ids = recorded_metric_ids("// common::scoreboard::record_count(\"ghost\", n, 1);\n");
    assert!(ids.is_empty(), "a record named in a comment was counted");
}
