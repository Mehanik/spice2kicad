//! **F3 / F4 — signal-flow geometry** (ADR-15 Stage 4, Tier 2).
//!
//! The project owner's conventions, stated while reviewing the emitted
//! schematics:
//!
//! * "signal flows left→right";
//! * "input terminals at the far left, output terminals at the far
//!   right";
//! * concretely on `rc_lowpass_ports`: "input terminal lefter, output
//!   righter".
//!
//! This file turns those into two falsifiable per-fixture counts,
//! measured on the emitted `.kicad_sch` geometry.
//!
//! # F3 — flow monotonicity
//!
//! Build the *signal-net graph*: nets that are neither Power nor
//! Ground, elements as vertices. Seed a BFS at the **input nets** —
//! those declared `*@port <net>=input`, else the leaf-net name
//! convention `layers.rs::no_source_fallback` already uses
//! (`in` / `input` / `vin*`). Net depth is BFS distance; element depth
//! is the minimum depth over its signal nets.
//!
//! For every pair of elements `(u, v)` sharing a signal net with
//! `depth(u) < depth(v)`, the flow runs u → v, so u's pins must sit
//! left of v's. An **inversion** is such a pair whose mean pin X is
//! reversed (`x(u) > x(v)`). F3 is the inversion count.
//!
//! Deliberately NOT defined via `spice_layout::assign_x_layers`: the
//! layer assignment is an *input* to placement and has its own defects
//! (a `*@port …=output` used to push every element on the output net —
//! including the series resistor feeding it — into the same rightmost
//! layer, which made the ordered-pair set empty and the metric blind
//! to the very defect it exists to catch). Deriving the flow from the
//! netlist + port declarations keeps the verifier independent of the
//! code under test.
//!
//! # F4-position — terminal lane
//!
//! For an input terminal (`(global_label … (shape input))`), the label
//! anchor X must be ≤ the minimum symbol-pin X on its net: the terminal
//! is the leftmost thing on its own net. Mirrored for an output
//! terminal (anchor X ≥ max symbol-pin X). F4 is the violation count.
//!
//! Both are zero-slack ratchets per CLAUDE.md § "Budgets are ratchets,
//! not knobs": the literals record the measured count on `master` and
//! only ever go **down**.
//!
//! # Scope
//!
//! ADR-15 Stage 4 is **positions only**. Element *orientation* (the
//! "both horizontal" half of the owner's complaint) is staged
//! separately and deliberately not asserted here — see the recorded
//! "flow-orientation wall".

mod common;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use common::spice_to_kicad;
use kicad_symbols::{Library, Orientation, Rotation};
use lexpr::Value;
use spice_diagnostics::FileId;
use spice_resolve::PortDir;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spice2kicad-flow-{pid}-{seq}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

// --- lexpr helpers (mirrors electrical_safety.rs) ------------------------

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

fn load_test_library() -> Library {
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

fn placed_symbol_pose(sym: &Value) -> Option<(f64, f64, Orientation)> {
    let at = find_child(sym, "at")?;
    let mut it = list_iter(at);
    it.next();
    let x = it.next().and_then(as_f64)?;
    let y = it.next().and_then(as_f64)?;
    let rot_deg = it.next().and_then(as_f64).unwrap_or(0.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rot_u = ((rot_deg.round() as i64).rem_euclid(360)) as u16;
    let rotation = match rot_u {
        0 => Rotation::R0,
        90 => Rotation::R90,
        180 => Rotation::R180,
        270 => Rotation::R270,
        _ => return None,
    };
    let mirror_y = find_child(sym, "mirror")
        .and_then(|m| list_iter(m).nth(1).and_then(as_str))
        .is_some_and(|s| s == "y");
    Some((x, y, Orientation { rotation, mirror_y }))
}

fn placed_symbol_refdes_and_lib_id(sym: &Value) -> Option<(String, String)> {
    let lib_id = find_child(sym, "lib_id")
        .and_then(|l| list_iter(l).nth(1).and_then(as_str))
        .map(str::to_owned)?;
    let mut refdes = None;
    for prop in children(sym, "property") {
        let mut it = list_iter(prop);
        it.next();
        if it.next().and_then(as_str) == Some("Reference") {
            refdes = it.next().and_then(as_str).map(str::to_owned);
            break;
        }
    }
    Some((refdes?, lib_id))
}

// --- measurement ---------------------------------------------------------

/// One placed body pin in world coordinates, tagged with its SPICE net.
/// `power:*` glyph pins are excluded: a glyph is decoration hung off a
/// rail pin, not a flow participant.
struct BodyPin {
    refdes: String,
    x_mm: f64,
    net: String,
}

struct Fixture {
    /// Placed body pins, world coords.
    pins: Vec<BodyPin>,
    /// Declared `*@port` directions, keyed by net.
    ports: Vec<(String, PortDir)>,
    /// Element refdes → its SPICE nets.
    element_nets: Vec<(String, Vec<String>)>,
    /// Root sheet s-expr.
    root: Value,
}

/// True for a net carried by `power:*` glyphs rather than by signal
/// wires: SPICE ground plus the canonical rail names. Mirrors
/// `spice_layout::net_class` closely enough for a flow verifier — a
/// rail net is not part of the left→right signal path.
fn is_rail_net(net: &str) -> bool {
    let lo = net.to_ascii_lowercase();
    net == "0"
        || matches!(
            lo.as_str(),
            "gnd" | "vss" | "vee" | "v-" | "vminus" | "vcc" | "vdd" | "v+" | "vplus"
        )
}

fn load(name: &str) -> Fixture {
    let dir = tempdir(name);
    let sch = spice_to_kicad(&fixtures_dir().join(format!("{name}.cir")), &dir)
        .unwrap_or_else(|e| panic!("convert {name}: {e}"));
    let src = std::fs::read_to_string(&sch).expect("read sch");
    let root = lexpr::from_str(&src).expect("parse sch");

    let library = load_test_library();
    let spice_src =
        std::fs::read_to_string(fixtures_dir().join(format!("{name}.cir"))).expect("read cir");
    let parsed = spice_parser::parse(&spice_src, FileId(0)).expect("parse spice");
    let resolved = spice_resolve::resolve(&parsed.netlist, &library).expect("resolve spice");

    let mut by_refdes: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut element_nets: Vec<(String, Vec<String>)> = Vec::new();
    for el in &resolved.elements {
        let mut pairs = Vec::with_capacity(el.pin_mapping.len());
        for (i, kicad_pin) in el.pin_mapping.iter().enumerate() {
            if let Some(net) = el.nodes.get(i) {
                pairs.push((kicad_pin.clone(), net.clone()));
            }
        }
        by_refdes.insert(el.refdes.clone(), pairs);
        element_nets.push((el.refdes.clone(), el.nodes.clone()));
    }

    let mut pins = Vec::new();
    for sym in children(&root, "symbol") {
        let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
            continue;
        };
        if refdes.starts_with("#PWR") {
            continue;
        }
        let Some((ox, _oy, orient)) = placed_symbol_pose(sym) else {
            continue;
        };
        let Some(lib_sym) = library.lookup(&lib_id) else {
            continue;
        };
        let pin_to_net: HashMap<&str, &str> = by_refdes
            .get(&refdes)
            .map(|v| v.iter().map(|(p, n)| (p.as_str(), n.as_str())).collect())
            .unwrap_or_default();
        for tp in lib_sym.pins_in(orient) {
            let Some(net) = pin_to_net.get(tp.number.as_str()) else {
                continue;
            };
            pins.push(BodyPin {
                refdes: refdes.clone(),
                x_mm: ox + tp.x,
                net: (*net).to_string(),
            });
        }
    }

    let ports = resolved
        .ports
        .iter()
        .map(|p| (p.net.clone(), p.dir))
        .collect();

    Fixture {
        pins,
        ports,
        element_nets,
        root,
    }
}

/// Element mean pin X, world mm, keyed by refdes.
fn element_x(f: &Fixture) -> HashMap<String, f64> {
    let mut sums: HashMap<String, (f64, f64)> = HashMap::new();
    for p in &f.pins {
        let e = sums.entry(p.refdes.clone()).or_insert((0.0, 0.0));
        e.0 += p.x_mm;
        e.1 += 1.0;
    }
    sums.into_iter().map(|(k, (sum, n))| (k, sum / n)).collect()
}

/// Nets that seed the flow BFS: declared `*@port …=input`, else the
/// `in` / `input` / `vin*` name convention.
fn input_nets(f: &Fixture) -> HashSet<String> {
    let declared: HashSet<String> = f
        .ports
        .iter()
        .filter(|(_, d)| *d == PortDir::Input)
        .map(|(n, _)| n.clone())
        .collect();
    if !declared.is_empty() {
        return declared;
    }
    let mut out = HashSet::new();
    for (_, nets) in &f.element_nets {
        for net in nets {
            let lo = net.to_ascii_lowercase();
            if lo == "in" || lo == "input" || lo.starts_with("vin") {
                out.insert(net.clone());
            }
        }
    }
    out
}

/// **F3** — flow inversions. See the module doc.
fn f3_inversions(f: &Fixture) -> Vec<(String, String)> {
    // net → elements, signal nets only.
    let mut net_members: HashMap<&str, Vec<&str>> = HashMap::new();
    for (refdes, nets) in &f.element_nets {
        for net in nets {
            if is_rail_net(net) {
                continue;
            }
            net_members
                .entry(net.as_str())
                .or_default()
                .push(refdes.as_str());
        }
    }
    // element → its signal nets.
    let elem_nets: HashMap<&str, Vec<&str>> = f
        .element_nets
        .iter()
        .map(|(r, nets)| {
            (
                r.as_str(),
                nets.iter()
                    .filter(|n| !is_rail_net(n))
                    .map(String::as_str)
                    .collect(),
            )
        })
        .collect();

    // BFS over nets from the input nets.
    let seeds = input_nets(f);
    let mut net_depth: HashMap<&str, u32> = HashMap::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    for net in net_members.keys() {
        if seeds.contains(*net) {
            net_depth.insert(net, 0);
            queue.push_back(net);
        }
    }
    while let Some(net) = queue.pop_front() {
        let d = net_depth[net];
        for member in net_members.get(net).into_iter().flatten() {
            for next in elem_nets.get(member).into_iter().flatten() {
                if !net_depth.contains_key(next) {
                    net_depth.insert(next, d + 1);
                    queue.push_back(next);
                }
            }
        }
    }

    // element depth = min over its signal nets.
    let mut depth: HashMap<&str, u32> = HashMap::new();
    for (refdes, nets) in &elem_nets {
        if let Some(d) = nets.iter().filter_map(|n| net_depth.get(n)).min() {
            depth.insert(refdes, *d);
        }
    }

    let xs = element_x(f);
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    let mut inversions = Vec::new();
    let mut nets: Vec<&&str> = net_members.keys().collect();
    nets.sort_unstable();
    for net in nets {
        let mut members = net_members[*net].clone();
        members.sort_unstable();
        for &u in &members {
            for &v in &members {
                if u == v {
                    continue;
                }
                let (Some(&du), Some(&dv)) = (depth.get(u), depth.get(v)) else {
                    continue;
                };
                if du >= dv || !seen.insert((u, v)) {
                    continue;
                }
                let (Some(&xu), Some(&xv)) = (xs.get(u), xs.get(v)) else {
                    continue;
                };
                if xu > xv {
                    inversions.push((u.to_string(), v.to_string()));
                }
            }
        }
    }
    inversions.sort();
    inversions
}

/// Every `(global_label "net" (shape …) (at x y …))` on the sheet.
fn directional_labels(root: &Value) -> Vec<(String, String, f64)> {
    let mut out = Vec::new();
    for gl in children(root, "global_label") {
        let Some(net) = list_iter(gl).nth(1).and_then(as_str) else {
            continue;
        };
        let Some(shape) = find_child(gl, "shape")
            .and_then(|s| list_iter(s).nth(1))
            .and_then(as_str)
        else {
            continue;
        };
        let Some(at) = find_child(gl, "at") else {
            continue;
        };
        let Some(x) = list_iter(at).nth(1).and_then(as_f64) else {
            continue;
        };
        out.push((net.to_string(), shape.to_string(), x));
    }
    out
}

/// A terminal sitting exactly ON its net's extreme pin satisfies the
/// invariant (it is not "inside" the circuit); only a strict excursion
/// past that pin counts. This tolerance absorbs f64 round-trip noise on
/// grid-exact coordinates.
const TERMINAL_TOL_MM: f64 = 0.01;

/// **F4-position** — terminal lane violations. See the module doc.
fn f4_violations(f: &Fixture) -> Vec<String> {
    let mut out = Vec::new();
    for (net, shape, lx) in directional_labels(&f.root) {
        let xs: Vec<f64> = f
            .pins
            .iter()
            .filter(|p| p.net.eq_ignore_ascii_case(&net))
            .map(|p| p.x_mm)
            .collect();
        if xs.is_empty() {
            continue;
        }
        let min = xs.iter().copied().fold(f64::INFINITY, f64::min);
        let max = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        match shape.as_str() {
            "input" => {
                if lx > min + TERMINAL_TOL_MM {
                    out.push(format!(
                        "input terminal `{net}` at x={lx:.2} is right of its net's leftmost pin x={min:.2}"
                    ));
                }
            }
            "output" => {
                if lx < max - TERMINAL_TOL_MM {
                    out.push(format!(
                        "output terminal `{net}` at x={lx:.2} is left of its net's rightmost pin x={max:.2}"
                    ));
                }
            }
            _ => {}
        }
    }
    out.sort();
    out
}

// --- ratchets ------------------------------------------------------------

/// Every fixture, with its zero-slack `(F3, F4)` high-water marks.
/// CLAUDE.md § "Budgets are ratchets, not knobs": these literals only
/// ever go **down**.
const FLOW_RATCHET: &[(&str, usize, usize)] = &[
    // fixture                  F3  F4
    ("rc_lowpass", 1, 0),
    ("rc_lowpass_ports", 1, 1),
    ("common_emitter", 0, 0),
    ("multivibrator", 0, 0),
    ("diff_pair", 0, 0),
    ("opamp_inverting", 0, 0),
    ("opamp_inverting_real", 0, 0),
    ("port_shapes", 0, 1),
    ("opamp_definition_level", 0, 0),
    ("named_rails", 1, 0),
];

#[test]
fn flow_monotonicity_and_terminal_lanes_within_ratchet() {
    let mut failures = Vec::new();
    let mut reclaim = Vec::new();
    for &(name, f3_budget, f4_budget) in FLOW_RATCHET {
        let f = load(name);
        let inv = f3_inversions(&f);
        let viol = f4_violations(&f);
        if std::env::var("S2K_FLOW_DUMP").is_ok() {
            println!("(\"{name}\", {}, {}),", inv.len(), viol.len());
            for (u, v) in &inv {
                println!("    F3 inversion: {u} is right of downstream {v}");
            }
            for v in &viol {
                println!("    F4 violation: {v}");
            }
        }
        if inv.len() > f3_budget {
            failures.push(format!(
                "{name}: F3 flow inversions rose to {} (budget {f3_budget}): {inv:?}",
                inv.len()
            ));
        } else if inv.len() < f3_budget {
            reclaim.push(format!("{name}: F3 may be lowered to {}", inv.len()));
        }
        if viol.len() > f4_budget {
            failures.push(format!(
                "{name}: F4 terminal-lane violations rose to {} (budget {f4_budget}): {viol:?}",
                viol.len()
            ));
        } else if viol.len() < f4_budget {
            reclaim.push(format!("{name}: F4 may be lowered to {}", viol.len()));
        }
    }
    assert!(
        failures.is_empty(),
        "flow-geometry ratchet regressions (do NOT raise the budget — diagnose the geometry):\n{}",
        failures.join("\n")
    );
    assert!(
        reclaim.is_empty(),
        "flow-geometry ratchet has slack; lower these literals in the same commit:\n{}",
        reclaim.join("\n")
    );
}
