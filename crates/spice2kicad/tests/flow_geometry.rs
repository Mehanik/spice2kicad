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
//! **Rail stubs take no part in it.** In ADR-15's role model a
//! two-terminal element with exactly one rail pin does not pass a
//! signal along — it *terminates* a node, and convention draws it as a
//! vertical drop in that node's column, which is exactly where
//! `idioms.rs` idiom 4 places it. Its X is owned by the column, not by
//! the flow order: a collector load belongs directly ABOVE its
//! transistor, and counting `Q1 → RC` as a left→right pair would score
//! the conventional drawing as a defect. The discriminator is
//! structural (pin count + rail-class pin count), never a refdes or an
//! element kind.
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
//! # F5 — series-signal pose (ADR-17 Stage 1)
//!
//! A **series-signal element** is two-terminal, not a power source, and
//! has NEITHER node rail-class: it passes the signal from one node to
//! the next rather than terminating a node. Convention draws it with a
//! **horizontal pin axis**, **upstream pin at the lower X**. F5 counts
//! the elements that fail either half.
//!
//! The discriminator is ADR-15 Stage 5's — validated there — and is
//! recomputed here from the netlist, not imported from `spice-layout`,
//! following the F3 precedent, so the metric can falsify the crate.
//! `series_discriminator_separates_stub_from_series_on_common_emitter`
//! is the assertion that keeps it honest: a bypass capacitor must be
//! classified NON-series and stay vertical, or F5 degenerates into a
//! demand that every two-terminal part be drawn sideways.
//!
//! # P5 — terminal order (ADR-17 Stage 1)
//!
//! Every declared input terminal sits strictly left of every declared
//! output terminal. F4 pins each terminal to the correct end of *its
//! own* net; P5 is the sheet-wide statement F4 cannot make. On
//! `rc_lowpass_ports` both terminals pass F4 while sitting at the same
//! x, stacked vertically — the sheet shows no flow direction at all.
//!
//! # F7 — parallel-partner separation
//!
//! Two elements incident on the **identical set of nets** are
//! electrically in parallel: the R and the C of `compensated_divider`'s
//! `R1 in out` / `C1 in out` arm, or `wien_bridge_osc`'s `RP np 0` /
//! `CP np 0`. Every reference drawing of such an arm puts the two
//! bodies side by side, sharing both nodes, because that adjacency is
//! *what tells the reader they are one arm*.
//!
//! F7 is the drawn distance between them: for every unordered pair of
//! drawn elements whose net sets are equal, and for each net they
//! share, the Manhattan distance between their pins on that net, in
//! whole grid cells (1.27 mm). The per-fixture ratchet records the
//! MAXIMUM. Like F6 it is deliberately a **distance, not a violation
//! count**: there is no threshold at which a separation becomes
//! categorically wrong, and a count would hide a partner drifting from
//! 2 cells to 36.
//!
//! It exists because nothing else in the suite could see the defect.
//! `compensated_divider` was emitted with `C1` **36 cells (45.7 mm)**
//! from its partner `R1` on the net they both span, and the wire-detour
//! ratchet — the instrument that looks most like a wire-length gate —
//! scored that drawing **1.0715**, near-ideal, because its ideal is
//! HPWL over the *emitted pin positions*: moving a symbol 46 mm away
//! inflates numerator and denominator together. See
//! `placement_quality.rs::wire_floor_ratio_within_budget_across_fixtures`
//! for the companion that closes the same blind spot on wire length.
//!
//! # Scope
//!
//! ADR-15 Stage 4 was **positions only**, and the orientation half was
//! blocked by the recorded "flow-orientation wall" (see the ADR-15
//! Stage-5 post-mortem). F5 and P5 are ADR-17 Stage 1: they *measure*
//! that gap so ADR-17 Stage 3 can close it. They land at today's
//! measured, defective counts and change no placer behaviour.

mod common;

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use common::spice_to_kicad;
use kicad_symbols::{Library, Orientation, Rotation};
use lexpr::Value;
use spice_diagnostics::FileId;
use spice_resolve::PortDir;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> common::TempDir {
    common::TempDir::new("flow", name)
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
    y_mm: f64,
    net: String,
}

struct Fixture {
    /// Placed body pins, world coords.
    pins: Vec<BodyPin>,
    /// Declared `*@port` directions, keyed by net.
    ports: Vec<(String, PortDir)>,
    /// Element refdes → its SPICE nets.
    element_nets: Vec<(String, Vec<String>)>,
    /// Refdes of every element resolved with `ElementRole::Power` — a
    /// voltage source lowered to rail glyphs rather than to a body.
    power_elements: HashSet<String>,
    /// Nets carried by `power:*` glyphs rather than by signal wires:
    /// SPICE ground, the canonical rail names, and every net touched by
    /// a `*@power`-tagged source (which is how `named_rails`' `p5`/`n5`
    /// become rails without matching any canonical name).
    rail_nets: HashSet<String>,
    /// Root sheet s-expr.
    root: Value,
}

fn is_canonical_rail_name(net: &str) -> bool {
    let lo = net.to_ascii_lowercase();
    net == "0"
        || matches!(
            lo.as_str(),
            "gnd" | "vss" | "vee" | "v-" | "vminus" | "vcc" | "vdd" | "v+" | "vplus"
        )
}

impl Fixture {
    fn is_rail_net(&self, net: &str) -> bool {
        self.rail_nets.contains(net)
    }

    /// **Rail stub** in ADR-15's role model: a two-terminal element with
    /// exactly one rail pin. It does not pass a signal along, it
    /// *terminates* a node, and convention draws it as a vertical drop
    /// in that node's column (`idioms.rs` idiom 4 places it there). Its
    /// X therefore belongs to the column, not to the flow order — a
    /// collector load sits directly ABOVE its transistor, not right of
    /// it — so it takes no part in the F3 ordering.
    fn is_rail_stub(&self, refdes: &str) -> bool {
        let Some((_, nets)) = self.element_nets.iter().find(|(r, _)| r == refdes) else {
            return false;
        };
        nets.len() == 2
            && nets[0] != nets[1]
            && nets.iter().filter(|n| self.is_rail_net(n)).count() == 1
    }

    /// **Series-signal element** in ADR-15's role model, and the subject
    /// of F5: a two-terminal element, not a power source, with NEITHER
    /// node rail-class. It lies *on* the signal path — it passes the
    /// signal from one node to the next — so convention draws it with a
    /// horizontal pin axis, upstream pin on the left.
    ///
    /// This is exactly the discriminator ADR-15 Stage 5 validated
    /// (`is_series_signal_element` in the reverted `orient.rs` patch),
    /// re-derived here from the netlist so the metric can falsify
    /// `spice-layout` rather than restate it — the same independence
    /// rule `is_rail_stub` above follows.
    ///
    /// It is the strict complement of `is_rail_stub` among two-terminal
    /// non-power elements: one rail pin ⇒ stub, zero rail pins ⇒ series.
    /// (Two rail pins is neither; nothing in the fixtures has that.)
    fn is_series_signal(&self, refdes: &str) -> bool {
        if self.power_elements.contains(refdes) {
            return false;
        }
        let Some((_, nets)) = self.element_nets.iter().find(|(r, _)| r == refdes) else {
            return false;
        };
        nets.len() == 2
            && nets[0] != nets[1]
            && !self.is_rail_net(&nets[0])
            && !self.is_rail_net(&nets[1])
    }
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

    let mut rail_nets: HashSet<String> = HashSet::new();
    let mut power_elements: HashSet<String> = HashSet::new();
    for el in &resolved.elements {
        let is_power_source = matches!(el.role, spice_resolve::ElementRole::Power(_));
        if is_power_source {
            power_elements.insert(el.refdes.clone());
        }
        for net in &el.nodes {
            if is_power_source || is_canonical_rail_name(net) {
                rail_nets.insert(net.clone());
            }
        }
    }

    let mut pins = Vec::new();
    for sym in children(&root, "symbol") {
        let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
            continue;
        };
        if refdes.starts_with("#PWR") {
            continue;
        }
        let Some((ox, oy, orient)) = placed_symbol_pose(sym) else {
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
                y_mm: oy - tp.y,
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
        power_elements,
        rail_nets,
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

/// The signal-flow graph both F3 and F5 read: rail nets and rail stubs
/// dropped, BFS depth measured from the input nets.
struct FlowGraph<'a> {
    /// Signal net → the non-stub elements on it.
    net_members: HashMap<&'a str, Vec<&'a str>>,
    /// Non-stub element → its signal nets.
    elem_nets: HashMap<&'a str, Vec<&'a str>>,
    /// BFS distance from the input nets, in nets.
    net_depth: HashMap<&'a str, u32>,
}

fn flow_graph(f: &Fixture) -> FlowGraph<'_> {
    // net → elements, signal nets only.
    let mut net_members: HashMap<&str, Vec<&str>> = HashMap::new();
    for (refdes, nets) in &f.element_nets {
        if f.is_rail_stub(refdes) {
            continue;
        }
        for net in nets {
            if f.is_rail_net(net) {
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
        .filter(|(r, _)| !f.is_rail_stub(r))
        .map(|(r, nets)| {
            (
                r.as_str(),
                nets.iter()
                    .filter(|n| !f.is_rail_net(n))
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

    FlowGraph {
        net_members,
        elem_nets,
        net_depth,
    }
}

/// **F3** — flow inversions. See the module doc.
fn f3_inversions(f: &Fixture) -> Vec<(String, String)> {
    let FlowGraph {
        net_members,
        elem_nets,
        net_depth,
    } = flow_graph(f);

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

/// Two pins on the same row are "horizontal" within this slop. Grid
/// coordinates are exact multiples of 1.27 mm, so anything above f64
/// round-trip noise is a real axis difference.
const AXIS_TOL_MM: f64 = 0.01;

/// **F5 — series-signal pin axis and direction.** See the module doc.
///
/// One violation per offending *element*, so the count is a count of
/// badly-drawn parts, not of failed sub-checks.
fn f5_violations(f: &Fixture) -> Vec<String> {
    let g = flow_graph(f);
    let mut out = Vec::new();

    let mut refdes: Vec<&str> = f
        .element_nets
        .iter()
        .map(|(r, _)| r.as_str())
        .filter(|r| f.is_series_signal(r))
        .collect();
    refdes.sort_unstable();

    for r in refdes {
        // The element's two placed pins, tagged with their nets.
        let pins: Vec<&BodyPin> = f.pins.iter().filter(|p| p.refdes == r).collect();
        if pins.len() != 2 {
            // Not emitted as a two-pin body (ignored, or lowered to a
            // sheet). Nothing to measure.
            continue;
        }
        let (a, b) = (pins[0], pins[1]);

        if (a.y_mm - b.y_mm).abs() > AXIS_TOL_MM {
            out.push(format!(
                "{r}: series element is not horizontal — pins at y={:.2} and y={:.2}",
                a.y_mm, b.y_mm
            ));
            continue;
        }

        // Direction: the pin on the shallower net must be the left one.
        let (Some(&da), Some(&db)) = (
            g.net_depth.get(a.net.as_str()),
            g.net_depth.get(b.net.as_str()),
        ) else {
            continue; // unreachable from any input net — no flow order
        };
        if da == db {
            continue;
        }
        let (up, down) = if da < db { (a, b) } else { (b, a) };
        if up.x_mm > down.x_mm + AXIS_TOL_MM {
            out.push(format!(
                "{r}: upstream pin (net `{}`) at x={:.2} is right of downstream pin \
                 (net `{}`) at x={:.2}",
                up.net, up.x_mm, down.net, down.x_mm
            ));
        }
    }
    out.sort();
    out
}

/// **P5 — terminal order.** Every declared input terminal must sit left
/// of every declared output terminal on the sheet. One violation per
/// offending (input, output) label pair.
///
/// F4 already pins each terminal to the correct end of *its own net*;
/// P5 is the sheet-wide statement F4 cannot make — on `rc_lowpass_ports`
/// both terminals satisfy F4 while sitting at the SAME x, stacked
/// vertically, which reads as no flow direction at all.
fn p5_violations(f: &Fixture) -> Vec<String> {
    let labels = directional_labels(&f.root);
    let inputs: Vec<&(String, String, f64)> =
        labels.iter().filter(|(_, s, _)| s == "input").collect();
    let outputs: Vec<&(String, String, f64)> =
        labels.iter().filter(|(_, s, _)| s == "output").collect();
    let mut out = Vec::new();
    for (inet, _, ix) in &inputs {
        for (onet, _, ox) in &outputs {
            if *ix >= *ox - TERMINAL_TOL_MM {
                out.push(format!(
                    "input terminal `{inet}` at x={ix:.2} is not left of output terminal \
                     `{onet}` at x={ox:.2}"
                ));
            }
        }
    }
    out.sort();
    out
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

/// **F6 — rail-stub lateral run.** A rail stub does not pass a signal
/// along; it *terminates* a node, and the conventional drawing hangs it
/// straight off that node — a vertical drop in the node's column, with a
/// lateral run of ZERO. `idioms.rs`'s rail-stub column idiom exists to
/// produce exactly that.
///
/// The idiom deliberately anchors only on **vertically-facing** pins,
/// which is load-bearing and must not be widened (see the doc comment on
/// `idioms::rail_stub_anchor_x`: anchoring on any pin dragged bias
/// dividers onto horizontal base pins and cost V5 on three fixtures).
/// The consequence is a blind spot with no measurement: a stub whose
/// anchor node presents only HORIZONTAL pins — a bias resistor feeding a
/// transistor BASE — gets no column opinion at all and keeps whatever
/// column the layer seeder gave it. On `multivibrator` that leaves
/// `RB1`/`RB2` at the extreme columns while the transistors they bias
/// sit ~16 mm inboard, so each base is reached by a long horizontal run.
///
/// F6 makes that visible and bounded. For every rail stub, take its
/// non-rail (signal) pin and the NEAREST other pin on the same net —
/// the node it terminates — and measure the horizontal offset between
/// them, in whole grid cells (1.27 mm). A stub hanging correctly in its
/// node's column scores 0. The per-fixture ratchet records the MAXIMUM
/// over the fixture's stubs.
///
/// Deliberately a *distance*, not a violation count: there is no
/// threshold at which a lateral run becomes categorically wrong, and a
/// count would hide a stub drifting from 2 cells to 12. It is Tier 2
/// (an aesthetic gradient, like V5/V6), measured on emitted geometry,
/// and derived from the netlist's pin roles — no fixture or refdes is
/// named.
fn f6_stub_lateral_runs(f: &Fixture) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let refdeses: Vec<String> = f.element_nets.iter().map(|(r, _)| r.clone()).collect();
    for refdes in refdeses {
        if !f.is_rail_stub(&refdes) {
            continue;
        }
        // The stub's own pin on its signal (non-rail) net.
        let Some(own) = f
            .pins
            .iter()
            .find(|p| p.refdes == refdes && !f.is_rail_net(&p.net))
        else {
            continue;
        };
        // The node it terminates: the nearest foreign pin on that net.
        let Some(anchor) = f
            .pins
            .iter()
            .filter(|p| p.refdes != refdes && p.net == own.net)
            .min_by(|a, b| {
                let da = (a.x_mm - own.x_mm).abs() + (a.y_mm - own.y_mm).abs();
                let db = (b.x_mm - own.x_mm).abs() + (b.y_mm - own.y_mm).abs();
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
        else {
            continue;
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let cells = ((anchor.x_mm - own.x_mm).abs() / 1.27).round() as u32;
        out.push((refdes, cells));
    }
    out.sort();
    out
}

/// **F7 — parallel-partner separation.** Two elements incident on the
/// *identical* set of nets are electrically parallel — one arm of the
/// circuit, drawn by every reference as two bodies side by side sharing
/// both nodes. `compensated_divider`'s `R1`/`C1` (both on `in`,`out`)
/// and `wien_bridge_osc`'s `RP`/`CP` (both on `np`,`0`) are the suite's
/// specimens.
///
/// For every unordered pair of drawn elements whose net SETS are equal,
/// and for each net they share, measure the Manhattan distance between
/// their pins on that net, in whole grid cells (1.27 mm). Partners drawn
/// adjacent score a cell or two; a partner exiled across the sheet
/// scores tens.
///
/// **Why the identical-net-SET discriminator.** It is structural and
/// derived from the netlist alone — no refdes, no element kind, no
/// "looks like an RC" pattern (CLAUDE.md principle 9). It is also the
/// strictest possible reading: sharing *one* net is ordinary fan-out and
/// says nothing about how the two should be drawn, while sharing *all*
/// of them means the two devices are interchangeable at every terminal.
///
/// **Rail nets are included deliberately.** A pair sharing `(out, 0)`
/// drops to two separate ground glyphs, and the distance between those
/// two rail pins is exactly the lateral spread a reader sees as "these
/// two were not drawn together". Excluding it would blind the metric to
/// every rail-referenced parallel arm — `compensated_divider`'s own
/// `R2`/`C2` among them.
///
/// Power sources (lowered to glyphs, never to a body) and elements with
/// no emitted body pins take no part.
fn f7_parallel_partner_runs(f: &Fixture) -> Vec<(String, String, String, u32)> {
    let mut sets: Vec<(String, BTreeSet<String>)> = Vec::new();
    for (refdes, nets) in &f.element_nets {
        if f.power_elements.contains(refdes) {
            continue;
        }
        let set: BTreeSet<String> = nets.iter().cloned().collect();
        // A one-net element is a short across a single node, not a
        // parallel partner of anything.
        if set.len() < 2 {
            continue;
        }
        if !f.pins.iter().any(|p| &p.refdes == refdes) {
            continue;
        }
        sets.push((refdes.clone(), set));
    }
    sets.sort();

    let mut out = Vec::new();
    for (i, (u, su)) in sets.iter().enumerate() {
        for (v, sv) in sets.iter().skip(i + 1) {
            if su != sv {
                continue;
            }
            for net in su {
                let (Some(pu), Some(pv)) = (
                    f.pins.iter().find(|p| &p.refdes == u && &p.net == net),
                    f.pins.iter().find(|p| &p.refdes == v && &p.net == net),
                ) else {
                    continue;
                };
                let mm = (pu.x_mm - pv.x_mm).abs() + (pu.y_mm - pv.y_mm).abs();
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let cells = (mm / 1.27).round() as u32;
                out.push((u.clone(), v.clone(), net.clone(), cells));
            }
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
    // --- ADR-23 PROMOTION of `--placer=flow-seed` to the default
    // (owner-approved, 2026-08-18): re-recorded at the new default's
    // measured counts. **F3 is now ZERO on all eighteen fixtures** —
    // every one of the three remaining flow inversions
    // (`rc_phase_shift`, `two_stage_amp`, `sallen_key_lpf`) disappears
    // when X measures signal depth instead of rail hops, which is the
    // single most direct confirmation the promotion did what it claims.
    // F4 was already clean everywhere and stays clean.

    // fixture                  F3  F4
    ("rc_lowpass", 0, 0),
    ("rc_lowpass_ports", 0, 0),
    ("common_emitter", 0, 0),
    ("multivibrator", 0, 0),
    ("diff_pair", 0, 0),
    ("opamp_inverting", 0, 0),
    ("opamp_inverting_real", 0, 0),
    ("port_shapes", 1, 0),
    ("opamp_definition_level", 0, 0),
    ("named_rails", 0, 0),
    // F0 (v0.2 roadmap) NEW-GEOMETRY BASELINE, owner-approved:
    // two F3 flow inversions (CIN→Q1, R3→CIN); F4 clean. Ratchet DOWN.
    // F3 2 -> 1: the rail-stub SIDE fix removed the CIN->Q1 inversion
    // (RB no longer sits between them). Ratchet DOWN.
    ("rc_phase_shift", 0, 0),
    // F0 (v0.2 roadmap) NEW-GEOMETRY BASELINE: one F3 flow inversion
    // (the CC interstage coupling cap drawn downstream of the Q2 it
    // feeds); F4 terminal lanes clean. Ratchet DOWN.
    ("two_stage_amp", 0, 0),
    // --- F2 (v0.2 roadmap, second benchmark wave) NEW-GEOMETRY
    // BASELINES, zero slack, ratchet DOWN only. F4 is clean on all four.
    ("cascode_amp", 0, 0),
    ("lc_ladder_lpf", 0, 0),
    // One F3 inversion: C1, the Sallen-Key feedback capacitor, is drawn
    // upstream of the node it feeds back from. That is the FIRST visible
    // top-level feedback arc the F3 metric has ever had to grade.
    ("sallen_key_lpf", 2, 0),
    ("wien_bridge_osc", 0, 0),
    // --- F3 (Tier-0 router fix, ADR-24): the two fixtures promoted out of
    // `tests/f0_defects.rs` once the Steiner-vertex-on-foreign-pin defect was
    // fixed. NEW-GEOMETRY BASELINES, zero slack, ratchet DOWN only. Adding
    // them moved no existing fixture's literal.
    ("sallen_key_driven", 0, 0),
    ("shunt_feedback_amp", 0, 0),
    ("stepped_attenuator", 0, 0),
    ("opamp_transimpedance", 0, 0),
    ("resistor_ladder_ref", 0, 0),
    ("compensated_divider", 0, 0),
];

#[test]
fn flow_monotonicity_and_terminal_lanes_within_ratchet() {
    let mut failures = Vec::new();
    let mut reclaim = Vec::new();
    for &(name, f3_budget, f4_budget) in FLOW_RATCHET {
        let f = load(name);
        let inv = f3_inversions(&f);
        let viol = f4_violations(&f);
        common::scoreboard::record_count("f3", name, inv.len());
        common::scoreboard::record_count("f4", name, viol.len());
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

// --- ADR-17 Stage 1: F5 (P4) and P5 --------------------------------------

/// Every fixture, with its zero-slack `(F5, P5)` high-water marks —
/// the counts measured on `master` at ADR-17 Stage 1, when the placer
/// still has both defects. CLAUDE.md § "Budgets are ratchets, not
/// knobs": these literals only ever go **down**. ADR-17 Stage 3 drives
/// them to zero; nothing before Stage 3 may raise one.
/// Measured total at Stage 1: **F5 = 16, P5 = 1**. The defect is
/// systemic, not the two isolated cases the ADR-17 design review
/// expected — only `diff_pair` (which has no series element at all) is
/// clean, and eight of the sixteen are plain "drawn vertical". Recorded
/// here because it materially raises what Stage 3 must deliver.
///
/// `rc_lowpass` is the instructive one: R1 IS horizontal and still
/// fails, on *direction* — its upstream `in` pin sits at x=54.61, right
/// of the downstream `out` pin at x=46.99. That is the failure mode
/// ADR-15's Stage-5 post-mortem called "axis is only half the
/// constraint" (mirror state unconstrained), caught here by F5's second
/// half.
const FLOW_POSE_RATCHET: &[(&str, usize, usize)] = &[
    // --- ADR-23 PROMOTION of `--placer=flow-seed` to the default
    // (owner-approved, 2026-08-18): re-recorded at the new default's
    // measured counts. F5 net -1 (cascode_amp 2->1, opamp_inverting
    // 2->1, rc_phase_shift 1->0, and a RISE sallen_key_lpf 0->2).
    // P5 is 0 everywhere on both sides.

    // fixture                  F5  P5
    // F5 1 -> 0. The series-horizontal flow-root fallback
    // (`idioms::signal_net_depth`) now draws `rc_lowpass` identically to
    // `rc_lowpass_ports`: R1 horizontal with its upstream `in` pin at the
    // lower X (in-left, out-right), series pose correct AND terminals
    // ordered. Ratchet DOWN.
    ("rc_lowpass", 0, 0),
    // F5/P5: the series-horizontal flow construction
    // (`idioms::apply_series_horizontal`) now draws R1 HORIZONTAL with its
    // upstream `in` pin at the lower X, so `in` (x=31.75) sits strictly
    // left of `out` (x=39.37): series pose correct AND terminals ordered.
    ("rc_lowpass_ports", 0, 0),
    // F5: COUT is drawn vertical though it is the series element
    // between `c` and `out`. CIN, the other series element, is
    // correctly horizontal — see
    // `series_discriminator_separates_stub_from_series_on_common_emitter`.
    ("common_emitter", 0, 0),
    // F5: both cross-coupling caps drawn vertical.
    ("multivibrator", 2, 0),
    // No series-signal element: RC1/RC2/RTAIL are all rail stubs.
    ("diff_pair", 0, 0),
    // F5: both RIN and RF drawn vertical.
    ("opamp_inverting", 0, 0),
    // F5: RF AND RIN drawn vertical — same state as the sibling
    // `opamp_inverting`, whose topology is identical. Rose 1 -> 2 when
    // the `layers.rs` root refinement moved X1 downstream of RIN;
    // ESCAPE REQUEST, pending owner sign-off (see the commit message).
    ("opamp_inverting_real", 1, 0),
    // F5: R1/R2/R3 drawn vertical (R4 is a rail stub).
    ("port_shapes", 0, 0),
    // F5: 0. Was 1, and 4 before that — all of RIN1/RF1/RIN2/RF2 drawn
    // vertical, with both channels drawn right-to-left and X-interleaved.
    // The two channels are electrically independent and share only the
    // rails, so V7's mirror pinned six of eight elements about an axis
    // they do not nest about, and neither channel had an input anchor
    // (`in1`/`in2` matched no boundary-net name). With the coupling
    // predicate, the channel-numbered port match, the geometry-derived
    // stacked-bucket Y stride, and channel-row banding (Option B) that
    // pins each channel's seed facing through phase 4.5, both channels
    // layer left-to-right and ALL FOUR series resistors land horizontal.
    // Ratchet DOWN.
    ("opamp_definition_level", 0, 0),
    // F5: RIN drawn vertical (RPU/RPD/CL are rail stubs).
    ("named_rails", 1, 0),
    // F0 (v0.2 roadmap) NEW-GEOMETRY BASELINE, owner-approved: COUT,
    // a series coupling cap, is drawn vertical (the exact `COUT`-drawn-
    // vertical defect Milestone D targets). P5 clean. Ratchet DOWN.
    ("rc_phase_shift", 0, 0),
    // F0 (v0.2 roadmap) NEW-GEOMETRY BASELINE: both series coupling
    // caps (CC interstage, CIN input) are drawn VERTICAL — the same
    // `COUT`-drawn-vertical defect `rc_phase_shift` carries, here twice
    // over. P5 clean. Ratchet DOWN.
    ("two_stage_amp", 0, 0),
    // --- F2 (v0.2 roadmap, second wave) NEW-GEOMETRY BASELINES. P5 is
    // clean on all four; F5 is not. Ratchet DOWN.
    //
    // Two series parts drawn VERTICAL (CIN and COUT, the input and
    // output coupling caps) — the same `COUT`-drawn-vertical defect
    // `rc_phase_shift` and `two_stage_amp` carry, and the defect
    // Milestone D targets.
    // ADR-40 PROMOTION re-record (`dc-series-column-pinned` becomes
    // the default): F5 0 -> 1, exactly the rise ADR-40's B-vs-F5
    // section predicted for this arm — a column member the previous
    // default drew horizontal is now drawn vertical. Read from the
    // scoreboard sink; the promotion's own re-record had missed this
    // literal, so the branch was already red on it. PRE-EXISTING:
    // NOT caused by the pin-anchoring fix (the control sink reads 1).
    ("cascode_amp", 1, 0),
    // THREE series parts drawn vertical (L1, L2, L3) — the worst F5 in
    // the suite. Every inductor in a ladder is a series element on the
    // main signal path, so a placer that draws series parts vertical has
    // nowhere to hide here. This is the F5 headroom F2 exists to expose.
    // --- SECOND ADR-23 PROMOTION: `--placer=flow-seed-v4` becomes the
    // default (owner-authorised, 2026-08-24). Re-recorded at the NEW
    // DEFAULT's measured value, read from the scoreboard sink. Only the
    // two drawn-stimulus fixtures move; a whole-placer swap is the ONLY
    // sanctioned way one of these RISES, and it is not available to an
    // ordinary change.
    //
    // F5 3 -> 1, P5 unchanged. The three series-pose violations were the
    // three ladder members phase 4.5 had rotated off-axis. Ratchet DOWN.
    ("lc_ladder_lpf", 1, 0),
    ("sallen_key_lpf", 0, 0),
    ("wien_bridge_osc", 0, 0),
    // --- F3 (Tier-0 router fix, ADR-24): the two fixtures promoted out of
    // `tests/f0_defects.rs` once the Steiner-vertex-on-foreign-pin defect was
    // fixed. NEW-GEOMETRY BASELINES, zero slack, ratchet DOWN only. Adding
    // them moved no existing fixture's literal.
    // --- SECOND ADR-23 PROMOTION: `--placer=flow-seed-v4` becomes the
    // default (owner-authorised, 2026-08-24). Re-recorded at the NEW
    // DEFAULT's measured value, read from the scoreboard sink. Only the
    // two drawn-stimulus fixtures move; a whole-placer swap is the ONLY
    // sanctioned way one of these RISES, and it is not available to an
    // ordinary change.
    //
    // F5 3 -> 1, P5 unchanged. Ratchet DOWN.
    ("sallen_key_driven", 1, 0),
    // F5 1 -> 0: with RB above `b`, every series part on this fixture is
    // drawn horizontal. Ratchet DOWN.
    // ADR-40 PROMOTION re-record (`dc-series-column-pinned` becomes
    // the default): F5 0 -> 1, exactly the rise ADR-40's B-vs-F5
    // section predicted for this arm — a column member the previous
    // default drew horizontal is now drawn vertical. Read from the
    // scoreboard sink; the promotion's own re-record had missed this
    // literal, so the branch was already red on it. PRE-EXISTING:
    // NOT caused by the pin-anchoring fix (the control sink reads 1).
    ("shunt_feedback_amp", 1, 0),
    ("stepped_attenuator", 1, 0),
    ("opamp_transimpedance", 0, 0),
    ("resistor_ladder_ref", 4, 0),
    ("compensated_divider", 0, 0),
];

#[test]
fn series_pose_and_terminal_order_within_ratchet() {
    let mut failures = Vec::new();
    let mut reclaim = Vec::new();
    for &(name, f5_budget, p5_budget) in FLOW_POSE_RATCHET {
        let f = load(name);
        let f5 = f5_violations(&f);
        let p5 = p5_violations(&f);
        common::scoreboard::record_count("f5", name, f5.len());
        common::scoreboard::record_count("p5", name, p5.len());
        if std::env::var("S2K_FLOW_DUMP").is_ok() {
            println!("(\"{name}\", {}, {}),", f5.len(), p5.len());
            for v in &f5 {
                println!("    F5: {v}");
            }
            for v in &p5 {
                println!("    P5: {v}");
            }
        }
        if f5.len() > f5_budget {
            failures.push(format!(
                "{name}: F5 series-pose violations rose to {} (budget {f5_budget}): {f5:?}",
                f5.len()
            ));
        } else if f5.len() < f5_budget {
            reclaim.push(format!("{name}: F5 may be lowered to {}", f5.len()));
        }
        if p5.len() > p5_budget {
            failures.push(format!(
                "{name}: P5 terminal-order violations rose to {} (budget {p5_budget}): {p5:?}",
                p5.len()
            ));
        } else if p5.len() < p5_budget {
            reclaim.push(format!("{name}: P5 may be lowered to {}", p5.len()));
        }
    }
    assert!(
        failures.is_empty(),
        "F5/P5 ratchet regressions (do NOT raise the budget — diagnose the geometry):\n{}",
        failures.join("\n")
    );
    assert!(
        reclaim.is_empty(),
        "F5/P5 ratchet has slack; lower these literals in the same commit:\n{}",
        reclaim.join("\n")
    );
}

/// The assertion that makes F5 falsifiable rather than a restatement of
/// the placer.
///
/// F5 says "series elements are horizontal". If the predicate silently
/// widened to "every two-terminal element", the metric would demand a
/// bypass capacitor be drawn sideways — which is *wrong*, and worse,
/// a placer changed to satisfy it would still score 0. ADR-15's
/// "capacitors are horizontal is WRONG" correction is exactly this trap.
///
/// `common_emitter` carries the discriminating pair: `CIN` (`in` → `b`,
/// both signal nets) is series and IS horizontal; `CE` (`e` → `0`, one
/// rail pin) is a rail stub, must be classified non-series, and must
/// stay vertical. Same element kind, opposite verdicts, decided from pin
/// roles alone.
#[test]
fn series_discriminator_separates_stub_from_series_on_common_emitter() {
    let f = load("common_emitter");

    assert!(
        f.is_rail_stub("CE") && !f.is_series_signal("CE"),
        "CE (bypass cap, one pin on ground) must be a rail stub, never a series element"
    );
    assert!(
        f.is_series_signal("CIN") && !f.is_rail_stub("CIN"),
        "CIN (in → b, both signal nets) must be a series element"
    );
    assert!(
        f.is_series_signal("COUT") && !f.is_rail_stub("COUT"),
        "COUT (c → out, both signal nets) must be a series element"
    );

    let pin_ys = |refdes: &str| -> Vec<f64> {
        f.pins
            .iter()
            .filter(|p| p.refdes == refdes)
            .map(|p| p.y_mm)
            .collect()
    };

    let ce = pin_ys("CE");
    assert_eq!(ce.len(), 2, "CE should emit two pins");
    assert!(
        (ce[0] - ce[1]).abs() > AXIS_TOL_MM,
        "CE must stay VERTICAL — a rail stub hangs off its node's column. \
         Got pins at y={:.2}, y={:.2}",
        ce[0],
        ce[1]
    );

    let cin = pin_ys("CIN");
    assert_eq!(cin.len(), 2, "CIN should emit two pins");
    assert!(
        (cin[0] - cin[1]).abs() <= AXIS_TOL_MM,
        "CIN is a series element and is horizontal on master; if this fires the \
         fixture regressed. Got pins at y={:.2}, y={:.2}",
        cin[0],
        cin[1]
    );
}

/// Per-fixture zero-slack high-water mark for F6 (see
/// `f6_stub_lateral_runs`): the MAXIMUM rail-stub lateral run on the
/// fixture, in grid cells. Ratchets DOWN only.
const STUB_RUN_RATCHET: &[(&str, u32)] = &[
    // --- ADR-23 PROMOTION of `--placer=flow-seed` to the default
    // (owner-approved, 2026-08-18): re-recorded at the new default's
    // measured counts. F6 falls 32 cells suite-wide — `rc_phase_shift`
    // 24 -> 8 and `two_stage_amp` 19 -> 6, the two worst stub runs in
    // the suite. A rail stub demoted to a FOLLOWER lands in its
    // neighbour's column, so it no longer has to run laterally to reach
    // it; that is exactly what this metric measures. One rise:
    // `common_emitter` 4 -> 5.

    // fixture                  max lateral run, grid cells
    // F6 9 -> 0. The series-horizontal flow-root fallback
    // (`idioms::signal_net_depth`) now fires on `rc_lowpass` (the `in` leaf
    // net roots the flow graph without a `*@port`), re-columning C1 straight
    // beneath R1's downstream `out` pin exactly as in `rc_lowpass_ports`, so
    // its stub drops with zero lateral run. Ratchet DOWN.
    ("rc_lowpass", 0),
    // Series-horizontal construction re-columns C1 straight beneath R1's
    // downstream pin, so its stub drops with zero lateral run.
    ("rc_lowpass_ports", 0),
    // CE/RE share `e` with Q1's emitter (vertical pin, so the idiom does
    // fire); the 4 is the side-by-side spread of the two stubs on the
    // same node, which is deliberate — `apply_rail_stub_columns` spreads
    // a group symmetrically about the anchor so they do not stack.
    ("common_emitter", 4),
    // RB1/RB2 bias a BASE — a horizontally-facing pin. The column idiom
    // now seats them one geometry-derived stride to the base pin's
    // OUTWARD side and reaches the pin with a short run in, so they
    // score the same 2 as RC1/RC2 (collector loads, vertical pin, whose
    // 2 is the two-stub group spread about the anchor). Was 9.
    ("multivibrator", 2),
    // RTAIL terminates `tail`, shared by BOTH transistors' emitters; the
    // shared-centre idiom seats it at their midpoint, so a non-zero
    // offset from either one is correct, not a defect.
    ("diff_pair", 4),
    ("opamp_inverting", 0),
    ("opamp_inverting_real", 0),
    ("port_shapes", 0),
    ("opamp_definition_level", 0),
    ("named_rails", 4),
    // F0 (v0.2 roadmap) NEW-GEOMETRY BASELINE, owner-approved: RC hangs
    // 23 cells (29.21 mm) sideways of its node — nearly 4x the previous
    // suite worst (named_rails, 6). This sprawl is the compaction
    // headroom F0 exists to expose. Ratchet DOWN.
    // RISE 23 -> 24, rail-stub SIDE fix (Tier 2, global-improvement
    // escape, AWAITING OWNER SIGN-OFF): RC's lateral run grows by one
    // cell as the CE stage re-bases around RB's new column.
    // ADR-40 PROMOTION re-record: F6 4 -> 3. Ratchet DOWN.
    ("rc_phase_shift", 3),
    // F0 (v0.2 roadmap) NEW-GEOMETRY BASELINE: RC2 hangs 19 cells
    // (24.13 mm) sideways of its node, with RE2/CE2 at 8 and RC1 at 6 —
    // every one of the ten rail stubs drifts. Not the suite worst
    // (`rc_phase_shift` reaches 23) but the most widespread sprawl in
    // the suite. Ratchet DOWN.
    // ADR-40 PROMOTION re-record: F6 8 -> 5. Ratchet DOWN.
    ("two_stage_amp", 5),
    // --- F2 (v0.2 roadmap, second wave) NEW-GEOMETRY BASELINES.
    // Ratchet DOWN.
    // ADR-40 PROMOTION re-record: F6 7 -> 3. Ratchet DOWN.
    ("cascode_amp", 3),
    // Nine cells: the shunt capacitors of the ladder drift sideways of
    // the nodes they terminate, the length of the chain.
    // --- SECOND ADR-23 PROMOTION: `--placer=flow-seed-v4` becomes the
    // default (owner-authorised, 2026-08-24). Re-recorded at the NEW
    // DEFAULT's measured value, read from the scoreboard sink. Only the
    // two drawn-stimulus fixtures move; a whole-placer swap is the ONLY
    // sanctioned way one of these RISES, and it is not available to an
    // ordinary change.
    //
    // F6 9 -> 10. RISE of one cell, and it is the drawn source `VIN`,
    // not a rail stub in the usual sense: straightening the ladder onto
    // one lane leaves `VIN` reaching 10 cells (12.70 mm) sideways to its
    // node instead of 9. One cell, against B 16 -> 5 and detour
    // -13.66 pp on the same fixture.
    ("lc_ladder_lpf", 10),
    ("sallen_key_lpf", 0),
    ("wien_bridge_osc", 4),
    // --- F3 (Tier-0 router fix, ADR-24): the two fixtures promoted out of
    // `tests/f0_defects.rs` once the Steiner-vertex-on-foreign-pin defect was
    // fixed. NEW-GEOMETRY BASELINES, zero slack, ratchet DOWN only. Adding
    // them moved no existing fixture's literal.
    // --- SECOND ADR-23 PROMOTION: `--placer=flow-seed-v4` becomes the
    // default (owner-authorised, 2026-08-24). Re-recorded at the NEW
    // DEFAULT's measured value, read from the scoreboard sink. Only the
    // two drawn-stimulus fixtures move; a whole-placer swap is the ONLY
    // sanctioned way one of these RISES, and it is not available to an
    // ordinary change.
    //
    // F6 7 -> 13. RISE, and the largest single Tier-2 cost of this
    // promotion: `VIN` now sits 13 cells (16.51 mm) sideways of its
    // node. Same mechanism as `lc_ladder_lpf` above — the drawn source
    // is rooted at the head of the chain and the chain got longer in X
    // — and it is what the fixture's detour rise (+8.36 pp) is made of.
    // Weighed against crossings 3 -> 0, J 4 -> 1, Q3 3 -> 1, F5 3 -> 1
    // and the V13(4) xfail discharge on the same fixture.
    ("sallen_key_driven", 2),
    // F6 9 -> 5: RB's column is no longer dragged sideways by the
    // below-the-node re-column. Ratchet DOWN.
    // ADR-40 PROMOTION re-record: F6 8 -> 4. Ratchet DOWN.
    ("shunt_feedback_amp", 4),
    ("stepped_attenuator", 10),
    ("opamp_transimpedance", 7),
    // ADR-40 PROMOTION re-record: F6 10 -> 6. Ratchet DOWN.
    ("resistor_ladder_ref", 6),
    ("compensated_divider", 9),
];

/// F6 ratchet. A rail stub should hang straight off the node it
/// terminates; this bounds how far sideways it is allowed to drift.
#[test]
fn stub_lateral_run_within_ratchet() {
    let mut failures = Vec::new();
    let mut reclaim = Vec::new();
    for &(name, budget) in STUB_RUN_RATCHET {
        let f = load(name);
        let runs = f6_stub_lateral_runs(&f);
        let worst = runs.iter().map(|(_, c)| *c).max().unwrap_or(0);
        common::scoreboard::record_count("f6", name, worst as usize);
        if std::env::var("S2K_FLOW_DUMP").is_ok() {
            println!("(\"{name}\", {worst}),  // {runs:?}");
        }
        if worst > budget {
            let detail: Vec<String> = runs
                .iter()
                .filter(|(_, c)| *c > budget)
                .map(|(r, c)| {
                    format!(
                        "{r}: {c} cells ({:.2} mm) sideways of its node",
                        f64::from(*c) * 1.27
                    )
                })
                .collect();
            failures.push(format!(
                "{name}: worst rail-stub lateral run rose to {worst} cells (budget {budget}): {}",
                detail.join("; ")
            ));
        } else if worst < budget {
            reclaim.push(format!("{name}: F6 may be lowered to {worst}"));
        }
    }
    assert!(
        failures.is_empty(),
        "F6 ratchet regressions (do NOT raise the budget — diagnose the geometry):\n{}",
        failures.join("\n")
    );
    assert!(
        reclaim.is_empty(),
        "F6 ratchet has slack; lower these literals in the same commit:\n{}",
        reclaim.join("\n")
    );
}

/// Per-fixture zero-slack high-water mark for F7 (see
/// [`f7_parallel_partner_runs`]): the MAXIMUM separation, in grid cells,
/// between two elements incident on the identical set of nets. Ratchets
/// DOWN only, per CLAUDE.md § "Budgets are ratchets, not knobs".
///
/// A fixture with no parallel pair at all reads 0, which is a real
/// measurement here and not an abstention: `f7_parallel_partner_pairs_exist`
/// below asserts that the population is non-empty suite-wide, so a
/// column of zeros cannot mean the discriminator stopped matching.
const PARALLEL_SEPARATION_RATCHET: &[(&str, u32)] = &[
    // fixture                  max parallel-partner separation, grid cells
    ("rc_lowpass", 0),
    ("rc_lowpass_ports", 0),
    // `RE`/`CE`, both on (`e`,`0`): a two-stub group that
    // `apply_rail_stub_columns` spreads symmetrically about its anchor on
    // purpose, so the 4 is deliberate (F6 records the same 4 for it).
    ("common_emitter", 4),
    ("multivibrator", 0),
    ("diff_pair", 0),
    ("opamp_inverting", 0),
    ("opamp_inverting_real", 0),
    ("port_shapes", 0),
    ("opamp_definition_level", 0),
    ("named_rails", 0),
    ("rc_phase_shift", 7),
    ("two_stage_amp", 13),
    ("cascode_amp", 8),
    ("lc_ladder_lpf", 6),
    ("sallen_key_lpf", 0),
    ("wien_bridge_osc", 5),
    ("sallen_key_driven", 0),
    ("shunt_feedback_amp", 4),
    ("stepped_attenuator", 0),
    ("opamp_transimpedance", 12),
    ("resistor_ladder_ref", 0),
    // THE MOTIVATING CELL: `C1` is drawn 36 cells (45.72 mm) from
    // `R1`, the partner it shares BOTH nodes with, on a five-element
    // circuit. The detour ratchet scored that drawing 1.0715.
    ("compensated_divider", 36),
];

/// F7 ratchet. Two devices on the identical set of nets are one arm of
/// the circuit; this bounds how far apart they may be drawn.
#[test]
fn parallel_partner_separation_within_ratchet() {
    let mut failures = Vec::new();
    let mut reclaim = Vec::new();
    for &(name, budget) in PARALLEL_SEPARATION_RATCHET {
        let f = load(name);
        let runs = f7_parallel_partner_runs(&f);
        let worst = runs.iter().map(|(_, _, _, c)| *c).max().unwrap_or(0);
        common::scoreboard::record_count("f7", name, worst as usize);
        if std::env::var("S2K_FLOW_DUMP").is_ok() {
            println!("(\"{name}\", {worst}),  // {runs:?}");
        }
        if worst > budget {
            let detail: Vec<String> = runs
                .iter()
                .filter(|(_, _, _, c)| *c > budget)
                .map(|(u, v, net, c)| {
                    format!(
                        "{u}..{v} on `{net}`: {c} cells ({:.2} mm) apart",
                        f64::from(*c) * 1.27
                    )
                })
                .collect();
            failures.push(format!(
                "{name}: worst parallel-partner separation rose to {worst} cells \
                 (budget {budget}): {}",
                detail.join("; ")
            ));
        } else if worst < budget {
            reclaim.push(format!("{name}: F7 may be lowered to {worst}"));
        }
    }
    assert!(
        failures.is_empty(),
        "F7 ratchet regressions (do NOT raise the budget — diagnose the geometry):\n{}",
        failures.join("\n")
    );
    assert!(
        reclaim.is_empty(),
        "F7 ratchet has slack; lower these literals in the same commit:\n{}",
        reclaim.join("\n")
    );
}

/// Non-vacuity guard on F7's discriminator.
///
/// A metric that silently stops matching reads as a clean sweep of
/// zeros, which is indistinguishable from a repaired drawing — the exact
/// failure ADR-23 D9 names ("a blind cell is not conservatively blind"),
/// reached one level down. The suite contains parallel arms by
/// construction (`compensated_divider`'s `R1`/`C1` and `R2`/`C2`,
/// `wien_bridge_osc`'s `RP`/`CP`), so if the identical-net-set predicate
/// ever finds NONE of them the metric has broken, not the placer.
#[test]
fn f7_parallel_partner_pairs_exist() {
    let f = load("compensated_divider");
    let runs = f7_parallel_partner_runs(&f);
    let pairs: BTreeSet<(String, String)> = runs
        .iter()
        .map(|(u, v, _, _)| (u.clone(), v.clone()))
        .collect();
    assert!(
        pairs.contains(&("C1".to_string(), "R1".to_string())),
        "F7 did not recognise C1 || R1 (both on `in`,`out`) as a parallel pair: {pairs:?}"
    );
    assert!(
        pairs.contains(&("C2".to_string(), "R2".to_string())),
        "F7 did not recognise C2 || R2 (both on `out`,`0`) as a parallel pair: {pairs:?}"
    );
    // Two elements sharing ONE net are ordinary fan-out and must not be
    // counted: R1 and R2 share `out` but not `in`/`0`.
    assert!(
        !pairs.contains(&("R1".to_string(), "R2".to_string())),
        "F7 counted a merely-fan-out pair R1/R2 as parallel: {pairs:?}"
    );
}
