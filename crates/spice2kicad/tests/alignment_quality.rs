//! **Q5 — mutual-alignment near-miss** (v0.2 roadmap A3, Tier 2).
//!
//! A human snaps components that are *almost* on a shared axis onto it, so
//! the connecting wire runs straight instead of jogging. Q5 counts the
//! avoidable near-misses left on the emitted sheet — a whole-sheet
//! OUTPUT-geometry count, measured on the emitted `.kicad_sch` and
//! ratcheted per fixture at the value measured on `master`.
//!
//! # Why this is a leading indicator, not a duplicate of V16
//!
//! V16 (`wire_geometry.rs`) counts bends on *routed* wires. Q5 counts
//! *placement* near-misses — it never looks at a wire. A shared-net pair
//! whose origins sit a cell or two off a common axis almost always costs
//! the router a bend to reconnect, so Q5 is a **pre-routing leading
//! indicator** of V16 bends that is gradable on placement alone. Like
//! V16 it is Tier-2 aesthetic and a **counted quantity**, never a
//! coefficient (CLAUDE.md § constraints-vs-costs / V16 doctrine).
//!
//! # The metric
//!
//! Consider unordered pairs of DRAWN, non-power components `(u, v)` that
//!
//!   * share at least one **Signal-class** net (so a wire runs between
//!     them — an unaligned pair forces a jog), and
//!   * are exactly aligned on **neither** axis.
//!
//! Let `dx`, `dy` be the absolute differences of the two symbols'
//! `(at x y)` origins. The pair is a **near-miss** when
//!
//!   * `dx > 0` and `dy > 0` (aligned on neither axis — a pair already
//!     snapped on one axis is not a near-miss, it is done), AND
//!   * `dx <= NEAR` or `dy <= NEAR` (within a small snap threshold on at
//!     least one axis).
//!
//! `NEAR = NEAR_CELLS` grid cells (1 cell = 1.27 mm). `NEAR_CELLS = 2`: a
//! human would snap a ≤2-cell offset onto the axis; a larger gap is
//! intentional separation, not a near-miss. The per-fixture metric is the
//! integer count of such pairs.
//!
//! Power sources / rail glyphs (refdes starting `#`, `lib_id` `power:*`)
//! and `;@ ignore`d elements take no part: they are decoration hung off a
//! rail pin, never placed flow bodies snapped to a neighbour.
//!
//! # Distinct from what already exists
//!
//! * **V16** (`wire_geometry.rs`) — bends on *routed* wires; Q5 is
//!   pre-routing and reads only symbol origins.
//! * **Q3** (`flow_monotonicity.rs`) — left→right *ordering* against the
//!   placer's layer model; Q5 is order-agnostic and measures *offset*.
//! * **V5** (`placement_quality.rs`) — pin-*facing* orientation; Q5 is
//!   about symbol-origin co-alignment, orientation-agnostic.
//!
//! # Ratchet
//!
//! Zero-slack per-fixture high-water marks (CLAUDE.md § "Budgets are
//! ratchets, not knobs"): each literal equals the count measured on
//! `master` and only ever goes **down**. A commit that removes near-misses
//! SHOULD lower the literal in the same commit; a rise is a placement
//! regression to diagnose, never a budget to bump.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use common::spice_to_kicad;
use lexpr::Value;
use spice_diagnostics::FileId;
use spice_layout::net_class::{NetClass, classify_nets};
use spice_resolve::ElementRole;

/// Snap threshold, in grid cells. 1 cell = 1.27 mm. A ≤2-cell offset off
/// a shared axis is what a human would snap flush; beyond it the gap reads
/// as intentional separation.
const NEAR_CELLS: i64 = 2;

/// The KiCad schematic grid pitch, in micrometres (1.27 mm = 50 mil).
const CELL_UM: i64 = 1270;

/// Snap threshold in micrometres. Coordinates are quantised to µm so the
/// `dx == 0` / `dx <= NEAR` comparisons are exact integer tests on a grid
/// far coarser than µm.
const NEAR_UM: i64 = NEAR_CELLS * CELL_UM;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> common::TempDir {
    common::TempDir::new("q5", name)
}

// --- lexpr helpers (mirrors flow_monotonicity.rs) ------------------------

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

/// Quantise a millimetre coordinate to micrometres for exact comparison.
#[allow(clippy::cast_possible_truncation)]
fn q(mm: f64) -> i64 {
    (mm * 1000.0).round() as i64
}

/// `refdes -> emitted symbol origin (x, y) in µm` for every DRAWN flow
/// body: every top-level `(symbol …)` whose refdes is not a `#`-glyph and
/// whose `lib_id` is not `power:*`. Power/ground glyphs are decoration
/// hung off a rail pin, never placement participants.
fn drawn_symbol_origin(root: &Value) -> HashMap<String, (i64, i64)> {
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
        out.insert(refdes, (q(x), q(y)));
    }
    out
}

// --- measurement ---------------------------------------------------------

/// One drawn, non-power flow body: its Signal-class nets and its emitted
/// symbol-origin coordinates (µm).
struct AlignElem {
    refdes: String,
    signal_nets: Vec<String>,
    x_um: i64,
    y_um: i64,
}

/// Build the Q5 alignment bodies for a fixture: convert it, then join the
/// resolved netlist's Signal-class nets to the emitted symbol origins.
fn align_elems(name: &str) -> Vec<AlignElem> {
    let dir = tempdir(name);
    let sch = spice_to_kicad(&fixtures_dir().join(format!("{name}.cir")), &dir)
        .unwrap_or_else(|e| panic!("convert {name}: {e}"));
    let root = lexpr::from_str(&std::fs::read_to_string(&sch).expect("read sch"))
        .expect("parse sch as lexpr");
    let origins = drawn_symbol_origin(&root);

    // Re-derive the net classification from the same source, exactly as the
    // seed placer does: parse → resolve → check → classify.
    let spice_src =
        std::fs::read_to_string(fixtures_dir().join(format!("{name}.cir"))).expect("read cir");
    let library = load_test_library();
    let parsed = spice_parser::parse(&spice_src, FileId(0)).expect("parse spice");
    let resolved = spice_resolve::resolve(&parsed.netlist, &library).expect("resolve spice");
    let (checked, _diags) = spice_policy::check(resolved).expect("policy check");

    let classes = classify_nets(&checked);

    let mut out = Vec::new();
    for el in &checked.elements {
        // Power sources are lowered to rail glyphs, not flow bodies.
        if matches!(el.role, ElementRole::Power(_)) {
            continue;
        }
        // Only elements actually DRAWN as a (non-glyph) symbol participate;
        // this transparently drops `;@ ignore`d elements and any element
        // lowered to a sheet rather than a body.
        let Some(&(x_um, y_um)) = origins.get(&el.refdes) else {
            continue;
        };
        let signal_nets: Vec<String> = el
            .nodes
            .iter()
            .filter(|n| {
                classes.get(n.as_str()).copied().unwrap_or(NetClass::Signal) == NetClass::Signal
            })
            .cloned()
            .collect();
        out.push(AlignElem {
            refdes: el.refdes.clone(),
            signal_nets,
            x_um,
            y_um,
        });
    }
    out
}

/// The load helper mirrors `flow_monotonicity.rs`: the same four fixture
/// libraries the CLI is handed.
fn load_test_library() -> kicad_symbols::Library {
    use kicad_symbols::Library;
    let libs_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
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

/// **Q5** — mutual-alignment near-misses. One entry per offending pair
/// `(a, b)` (refdes-sorted within the pair), sorted.
fn q5_near_misses(elems: &[AlignElem]) -> Vec<(String, String)> {
    // net → the drawn flow bodies that carry it as a Signal net.
    let mut net_members: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, e) in elems.iter().enumerate() {
        for net in &e.signal_nets {
            net_members.entry(net.as_str()).or_default().push(i);
        }
    }

    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut out = Vec::new();
    let mut nets: Vec<&&str> = net_members.keys().collect();
    nets.sort_unstable();
    for net in nets {
        let members = &net_members[*net];
        for &a in members {
            for &b in members {
                if a >= b || !seen.insert((a, b)) {
                    continue;
                }
                let (ea, eb) = (&elems[a], &elems[b]);
                let dx = (ea.x_um - eb.x_um).abs();
                let dy = (ea.y_um - eb.y_um).abs();
                // Exactly aligned on either axis → already snapped, not a
                // near-miss. Require aligned on NEITHER axis.
                if dx == 0 || dy == 0 {
                    continue;
                }
                // Near-miss on at least one axis (within the snap threshold).
                if dx <= NEAR_UM || dy <= NEAR_UM {
                    let (lo, hi) = if ea.refdes <= eb.refdes {
                        (ea.refdes.clone(), eb.refdes.clone())
                    } else {
                        (eb.refdes.clone(), ea.refdes.clone())
                    };
                    out.push((lo, hi));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

// --- ratchet -------------------------------------------------------------

/// Every fixture, with its zero-slack Q5 high-water mark measured on
/// `master`. CLAUDE.md § "Budgets are ratchets, not knobs": these literals
/// only ever go **down**.
const Q5_NEAR_MISS_BUDGET: &[(&str, u32)] = &[
    // --- ADR-23 PROMOTION of `--placer=flow-seed` to the default
    // (owner-approved, 2026-08-18): re-recorded at the new default's
    // measured counts. Q5 is the ONE Tier-2 aggregate the promotion
    // loses: +3 points (two_stage_amp 7 -> 5 and rc_phase_shift 3 -> 2
    // against RISES on opamp_inverting_real 0 -> 3, cascode_amp and
    // common_emitter 3 -> 4, sallen_key_lpf 2 -> 3). Recorded, not
    // argued away; Q5 counts NEAR-misses, so a placer that packs
    // columns tighter naturally produces more of them.
    ("rc_lowpass", 0),
    ("rc_lowpass_ports", 0),
    // R1/R2 (bias divider) and Q1/RC/RE/CIN sit a cell or two off their
    // shared-net neighbours' axes — each near-miss is a candidate straight
    // drop the router currently jogs.
    // 4 -> 3: reclaimed by the ADR-19 M4 revert (`docs/layout-adr.md`,
    // "M4 reverted"). The literal was first measured on the M4 tree; the
    // pre-M4 Y datum this restores costs one fewer near-miss here. Ratchets
    // down only.
    ("common_emitter", 3),
    // C1/RC1, C2/RC2 (cross-coupling caps vs collector loads) and the
    // Q/RC collector columns land just off-axis on this symmetric fixture.
    ("multivibrator", 4),
    // Q1/RC1, Q2/RC2 collector columns a hair off their shared axis.
    ("diff_pair", 2),
    ("opamp_inverting", 0),
    ("opamp_inverting_real", 1),
    ("port_shapes", 1),
    // RF1/X1, RF2/X2 feedback resistors sit just off their opamp's axis.
    ("opamp_definition_level", 2),
    // CL/RIN and RIN/RPU near-miss on the shared `out`/input nets.
    ("named_rails", 2),
    // F0 (v0.2 roadmap) NEW-GEOMETRY BASELINE, owner-approved:
    // C3/CIN, CIN/Q1 and Q1/RB each miss a shared axis by a hair.
    ("rc_phase_shift", 2),
    // F0 (v0.2 roadmap) NEW-GEOMETRY BASELINE: seven near-misses
    // (CC/RB3, CE1/RE1, CIN/Q1, CIN/RB1, CIN/RB2, RB1/RB2, RB3/RB4) —
    // the worst in the suite, roughly double `rc_phase_shift`'s 3. Each
    // is a candidate straight drop the router currently jogs.
    ("two_stage_amp", 8),
    // --- F2 (v0.2 roadmap, second benchmark wave) NEW-GEOMETRY BASELINES.
    // Recorded at their measured values with zero slack; they ratchet
    // DOWN only. No existing fixture's literal moved.
    //
    // Q1/RB3, Q2/RB2 and RC/RB1 each miss a shared axis by a hair — the
    // bias chain is a column the placer does not keep in one column.
    ("cascode_amp", 4),
    // Five near-misses. A doubly-terminated ladder is nothing BUT
    // shared axes, and the placer snaps none of them: worst in the suite
    // after `two_stage_amp`.
    // --- SECOND ADR-23 PROMOTION: `--placer=flow-seed-v4` becomes the
    // default (owner-authorised, 2026-08-24). Re-recorded at the NEW
    // DEFAULT's measured value, read from the scoreboard sink. Only the
    // two drawn-stimulus fixtures move; a whole-placer swap is the ONLY
    // sanctioned way one of these RISES, and it is not available to an
    // ordinary change.
    //
    // Q5 5 -> 1. "A doubly-terminated ladder is nothing BUT shared axes,
    // and the placer snaps none of them" — it snaps them now. Ratchet
    // DOWN.
    ("lc_ladder_lpf", 1),
    ("sallen_key_lpf", 2),
    ("wien_bridge_osc", 2),
    // --- F3 (Tier-0 router fix, ADR-24): the two fixtures promoted out of
    // `tests/f0_defects.rs` once the Steiner-vertex-on-foreign-pin defect was
    // fixed. NEW-GEOMETRY BASELINES, zero slack, ratchet DOWN only. Adding
    // them moved no existing fixture's literal.
    ("sallen_key_driven", 5),
    // RISE 2 -> 4, rail-stub SIDE fix (Tier 2, global-improvement escape,
    // AWAITING OWNER SIGN-OFF): moving RB above its node re-bases the CE
    // stage, leaving Q1 a near-miss against CE/COUT/RE/RF. Paid for four
    // Tier-1 xfail expiries (V14 rail-pin on this fixture and
    // `rc_phase_shift`, plus that fixture's V14 [3] and rail ordering).
    ("shunt_feedback_amp", 0),
    ("stepped_attenuator", 0),
    ("opamp_transimpedance", 2),
    ("resistor_ladder_ref", 1),
    ("compensated_divider", 0),
];

#[test]
fn alignment_near_miss_within_budget_across_fixtures() {
    let mut failures = Vec::new();
    for &(name, budget) in Q5_NEAR_MISS_BUDGET {
        let elems = align_elems(name);
        let viol = q5_near_misses(&elems);
        let count = u32::try_from(viol.len()).unwrap_or(u32::MAX);
        common::scoreboard::record_count("q5", name, viol.len());
        if std::env::var("S2K_Q5_DUMP").is_ok() {
            println!("(\"{name}\", {count}),");
            for (a, b) in &viol {
                println!("    Q5 near-miss: {a} and {b} share an axis by a hair");
            }
        }
        if count > budget {
            failures.push(format!(
                "{name}: Q5 mutual-alignment near-misses rose to {count} (budget {budget}): \
                 {viol:?}. Do NOT raise the budget — diagnose the placement regression."
            ));
        } else if count < budget {
            // Lower-is-better: advertise the reclaimable slack so a fix
            // ratchets the literal down in the same commit.
            eprintln!("Q5 {name}: improved — you may lower the ratchet to (\"{name}\", {count})");
        }
    }
    assert!(
        failures.is_empty(),
        "Q5 mutual-alignment near-miss ratchet regressions:\n{}",
        failures.join("\n")
    );
}
