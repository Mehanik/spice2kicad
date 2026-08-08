//! **Q3 — global flow-monotonicity** (v0.2 roadmap A1, Tier 2).
//!
//! Instruments the Wall-1 / Wall-2 signal-flow work: does the emitted
//! layout actually draw the intended left→right signal flow? A whole-sheet
//! OUTPUT-geometry count, measured on the emitted `.kicad_sch`, ratcheted
//! per fixture at the value measured on `master`.
//!
//! # Ground truth for "forward"
//!
//! The placer's own signal-flow layer assignment,
//! `spice_layout::layers::assign_x_layers`, is taken as the intended flow
//! direction: layer 0 is leftmost (signal sources), higher layers sit
//! further right. This is deliberately the SAME structure the placer seeds
//! from — Q3 does not re-derive an independent flow model (that is F3's job
//! in `flow_geometry.rs`, which reads the netlist + `*@port` declarations
//! precisely so it can *falsify* the layer code). Q3 asks the narrower,
//! complementary question: given the placer's OWN notion of which element
//! is upstream, did the emitted X coordinates actually honour it?
//!
//! # The metric
//!
//! For each unordered pair of DRAWN, non-power components `(u, v)` that
//!
//!   * share at least one **Signal-class** net, and
//!   * sit on **different layers**,
//!
//! let the lower-layer element be the intended-upstream. Count a
//! **violation** when the emitted symbol X order disagrees —
//! `x(upstream) > x(downstream)`, the part drawn against the flow — using
//! each symbol's `(at x y)` origin from the emitted sheet. The per-fixture
//! metric is the integer count of such violating pairs.
//!
//! Power sources / rail glyphs (refdes starting `#`, `lib_id` `power:*`)
//! and `;@ ignore`d elements take no part: they aren't drawn as flow
//! bodies. `no_source_fallback` layerings (every element at layer 0) are
//! handled gracefully — no two elements differ in layer, so the pair set
//! is empty and the fixture scores 0.
//!
//! # Distinct from what already exists
//!
//! * The SA-internal `flow_inversions` gate scores an *intermediate*
//!   placement proxy, not emitted geometry.
//! * F5 (`flow_geometry.rs::series_pose`) scores per-element pose (axis +
//!   direction) of *series* elements only.
//! * F3 (`flow_geometry.rs`) re-derives flow from the netlist + ports to
//!   falsify the layer code, and uses mean-pin X.
//!
//! Q3 is the whole-sheet OUTPUT count keyed on the placer's OWN layer
//! model and the symbol-origin X — a different question from all three.
//!
//! # Ratchet
//!
//! Zero-slack per-fixture high-water marks (CLAUDE.md § "Budgets are
//! ratchets, not knobs"): each literal equals the count measured on
//! `master` and only ever goes **down**. A commit that removes violations
//! SHOULD lower the literal in the same commit; a rise is a geometry
//! regression to diagnose, never a budget to bump.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use common::spice_to_kicad;
use lexpr::Value;
use spice_diagnostics::FileId;
use spice_layout::layers::assign_x_layers;
use spice_layout::net_class::{NetClass, classify_nets};
use spice_resolve::ElementRole;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spice2kicad-q3-{pid}-{seq}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

// --- lexpr helpers (mirrors flow_geometry.rs) ----------------------------

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

/// `refdes -> emitted symbol-origin X (mm)` for every DRAWN flow body:
/// every top-level `(symbol …)` whose refdes is not a `#`-glyph and whose
/// `lib_id` is not `power:*`. Power/ground glyphs are decoration hung off
/// a rail pin, never flow participants.
fn drawn_symbol_x(root: &Value) -> HashMap<String, f64> {
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
        let Some(x) = list_iter(at).nth(1).and_then(as_f64) else {
            continue;
        };
        out.insert(refdes, x);
    }
    out
}

// --- measurement ---------------------------------------------------------

/// One drawn, non-power flow body: its layer, its Signal-class nets, and
/// its emitted symbol-origin X.
struct FlowElem {
    refdes: String,
    layer: u32,
    signal_nets: Vec<String>,
    x_mm: f64,
}

/// Build the Q3 flow bodies for a fixture: convert it, then join the
/// placer's OWN layer assignment to the emitted symbol X coordinates.
fn flow_elems(name: &str) -> Vec<FlowElem> {
    let dir = tempdir(name);
    let sch = spice_to_kicad(&fixtures_dir().join(format!("{name}.cir")), &dir)
        .unwrap_or_else(|e| panic!("convert {name}: {e}"));
    let root = lexpr::from_str(&std::fs::read_to_string(&sch).expect("read sch"))
        .expect("parse sch as lexpr");
    let xs = drawn_symbol_x(&root);

    // Re-derive the placer's layer assignment from the same source, exactly
    // as the seed placer does: parse → resolve → check → classify → layer.
    let spice_src =
        std::fs::read_to_string(fixtures_dir().join(format!("{name}.cir"))).expect("read cir");
    let library = load_test_library();
    let parsed = spice_parser::parse(&spice_src, FileId(0)).expect("parse spice");
    let resolved = spice_resolve::resolve(&parsed.netlist, &library).expect("resolve spice");
    let (checked, _diags) = spice_policy::check(resolved).expect("policy check");

    let classes = classify_nets(&checked);
    let layers = assign_x_layers(&checked, &classes);

    let mut out = Vec::new();
    for (idx, el) in checked.elements.iter().enumerate() {
        // Power sources are lowered to rail glyphs, not flow bodies.
        if matches!(el.role, ElementRole::Power(_)) {
            continue;
        }
        // Only elements actually DRAWN as a (non-glyph) symbol participate;
        // this transparently drops `;@ ignore`d elements and any element
        // lowered to a sheet rather than a body.
        let Some(&x_mm) = xs.get(&el.refdes) else {
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
        out.push(FlowElem {
            refdes: el.refdes.clone(),
            layer: layers.layers[idx],
            signal_nets,
            x_mm,
        });
    }
    out
}

/// The load helper mirrors `flow_geometry.rs`: the same four fixture
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

/// **Q3** — flow-monotonicity violations. One entry per offending
/// upstream→downstream pair (`(upstream, downstream)`), sorted.
fn q3_violations(elems: &[FlowElem]) -> Vec<(String, String)> {
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
                if a >= b {
                    continue;
                }
                let (ea, eb) = (&elems[a], &elems[b]);
                if ea.layer == eb.layer || !seen.insert((a, b)) {
                    continue;
                }
                let (up, down) = if ea.layer < eb.layer {
                    (ea, eb)
                } else {
                    (eb, ea)
                };
                if up.x_mm > down.x_mm {
                    out.push((up.refdes.clone(), down.refdes.clone()));
                }
            }
        }
    }
    out.sort();
    out
}

// --- ratchet -------------------------------------------------------------

/// Every fixture, with its zero-slack Q3 high-water mark measured on
/// `master`. CLAUDE.md § "Budgets are ratchets, not knobs": these
/// literals only ever go **down**.
const Q3_FLOW_MONOTONICITY_BUDGET: &[(&str, u32)] = &[
    ("rc_lowpass", 0),
    ("rc_lowpass_ports", 0),
    // R1 (layer < R2) is drawn right of R2 on the emitted sheet: the
    // rail-stub column idiom re-columns the parts against the placer's
    // own layer order. A Wall-1/Wall-2 flow fix should drive this to 0.
    ("common_emitter", 1),
    // RB2/RC1 (bias + collector loads) land right of the transistors /
    // cross-coupling caps their layer places upstream. Systemic on the
    // symmetric multivibrator; the flow work targets it.
    ("multivibrator", 4),
    // RC1 and RTAIL drawn right of Q1 despite lower layers — the diff
    // pair's rail stubs re-column against the layer order.
    ("diff_pair", 2),
    ("opamp_inverting", 0),
    ("opamp_inverting_real", 0),
    ("port_shapes", 0),
    ("opamp_definition_level", 0),
    // RPU (pull-up rail stub) drawn right of the CL it shares `out` with.
    ("named_rails", 1),
    // F0 (v0.2 roadmap) NEW-GEOMETRY BASELINE, owner-approved.
    // Four inversions (Q1→RE, RB→CIN, RB→Q1, RC→Q1) against a suite
    // whose previous worst was 1. Deliberately POOR: this is precisely
    // the flow-monotonicity headroom F0 exists to expose. Ratchet DOWN.
    ("rc_phase_shift", 4),
];

#[test]
fn flow_monotonicity_within_budget_across_fixtures() {
    let mut failures = Vec::new();
    for &(name, budget) in Q3_FLOW_MONOTONICITY_BUDGET {
        let elems = flow_elems(name);
        let viol = q3_violations(&elems);
        let count = u32::try_from(viol.len()).unwrap_or(u32::MAX);
        common::scoreboard::record_count("q3", name, viol.len());
        if std::env::var("S2K_Q3_DUMP").is_ok() {
            println!("(\"{name}\", {count}),");
            for (u, v) in &viol {
                println!("    Q3 violation: upstream {u} drawn right of downstream {v}");
            }
        }
        if count > budget {
            failures.push(format!(
                "{name}: Q3 flow-monotonicity violations rose to {count} (budget {budget}): \
                 {viol:?}. Do NOT raise the budget — diagnose the geometry regression."
            ));
        } else if count < budget {
            // Lower-is-better: advertise the reclaimable slack so a fix
            // ratchets the literal down in the same commit.
            eprintln!("Q3 {name}: improved — you may lower the ratchet to (\"{name}\", {count})");
        }
    }
    assert!(
        failures.is_empty(),
        "Q3 flow-monotonicity ratchet regressions:\n{}",
        failures.join("\n")
    );
}
