//! **A1 / A2 / B1 — the two things a human reads first** (ADR-28,
//! informational).
//!
//! # Why this file exists
//!
//! The project's instruments have three times scored as an improvement
//! something the owner read, on sight, as damage:
//!
//! * the SA scored destroying a textbook LC-ladder drawing as a 2.7x
//!   cost improvement — its objective has no orientation term at all;
//! * phase 4.5 scored mangling two inductors as a strict V5 win, and was
//!   correct by its own metric;
//! * the ADR-23 aggregate scored `flow-seed` a large win, after which
//!   the owner said the champion is better on four of eighteen fixtures.
//!
//! Every one of those is a *true* statement about the metric that made
//! it. None of the registered metrics measures **axis consistency**,
//! **orientation uniformity** or **device stacking**, which is what the
//! eye picks up before it reads a single refdes. A defect no verifier
//! measures is invisible to the ratchets AND to the scoreboard (ADR-23
//! "Known limits"), so the fix is a measurement, not a weight.
//!
//! Two are added here, both computed from the **emitted `.kicad_sch`
//! geometry** so they grade any placer, present or future, rather than
//! restating one placer's internals.
//!
//! # A — series-chain axis uniformity (`chain.axis`, `chain.reversal`)
//!
//! A **series chain** is a maximal path of two-terminal signal elements
//! linked by nets of *signal degree 2*: each interior net of the chain
//! touches exactly two drawn, non-rail-stub elements, so the current
//! leaving one member has nowhere to go but into the next. Textbook
//! style draws such a chain along **one shared axis, in one direction** —
//! that is what makes a ladder read as a ladder.
//!
//! The motivating specimen is `lc_ladder_lpf`'s `RS -> L1 -> L2 -> L3`,
//! which the shipping placer emits at rotations 180 / 90 / 0 / 270: one
//! chain, four orientations. The deterministic seed (`--no-refine`)
//! emits all four at 90.
//!
//! For each chain, pick the axis (horizontal or vertical) that reads the
//! drawing most charitably, and report two disjoint counts:
//!
//! * **`chain.axis`** — members whose pin axis differs from the chain's
//!   chosen axis;
//! * **`chain.reversal`** — members that ARE on the chosen axis but run
//!   *against* the chain's majority direction of travel.
//!
//! The two are deliberately disjoint: an off-axis member is counted
//! once, under `chain.axis`, and is not also counted as reversed. A
//! single element cannot be blamed twice for one pose, and keeping them
//! separate is what lets a reader tell "the ladder zig-zags between
//! horizontal and vertical" (an axis defect) from "one inductor is drawn
//! backwards" (a direction defect) — different repairs in the placer.
//!
//! # B — shared-current-path stacking (`stack.side_by_side`)
//!
//! Devices in series on a DC current path — a cascode's two transistors,
//! a collector load above its transistor, a rail-to-rail bias divider —
//! are conventionally **stacked in Y**, not spread in X. The current
//! runs down the page from supply to ground, and a stack is how a reader
//! sees that it is one current.
//!
//! A **DC-series pair** is two drawn elements `u`, `v` that
//!
//! 1. each conduct DC between two *distinct* rail nets (there is a path
//!    supply -> ... -> u -> ... -> ground that does not re-use `u`), and
//! 2. share a non-rail net `N` whose **DC degree is exactly 2** — `u`
//!    and `v` are the only DC conductors on it, so all of `u`'s current
//!    flows into `v`.
//!
//! `stack.side_by_side` counts the pairs drawn wider than tall
//! (`|dx| > |dy|` between element centres). Motivating specimen:
//! `cascode_amp`'s `Q1`/`Q2`, which the champion stacks and the shipping
//! placer sets side by side — the owner's stated reason for preferring
//! the champion on that fixture.
//!
//! Clause 2 is what keeps the metric from demanding nonsense. A
//! differential pair's two transistors share `tail` with `RTAIL`, so
//! that net has DC degree 3 and the pair is NOT counted — which is
//! right, because a diff pair is drawn side by side on purpose. See
//! `stacking_discriminator_separates_the_cascode_from_the_diff_pair`,
//! the assertion that keeps this honest.
//!
//! # Informational at birth
//!
//! Both metrics are registered with the ADR-23 scoreboard as
//! `Tier::Info` — printed per fixture, zero aggregate weight — on the
//! precedent of Q6's balance CoV and `bend_bound.rs`'s V16 bound.
//! **Neither is a zero-slack ratchet.** A metric whose definition is
//! still being calibrated must not be able to block work: an ambiguity
//! resolved the wrong way (see ADR-28's list) would, as a gate, reject
//! correct drawings while being wrong. What would justify promoting each
//! to a ratchet is recorded in ADR-28.
//!
//! The only assertions here are therefore
//!
//! * **soundness / non-vacuity** control arms on synthetic geometry, so
//!   the metrics cannot silently degenerate to "always 0"; and
//! * **specimen rankings** in `<=` form — they fire only if the arm the
//!   owner prefers becomes strictly WORSE than the arm they reject, i.e.
//!   only if the metric's own validation inverts. They can never block a
//!   change that improves the shipping placer.

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

use common::spice_to_kicad;
use kicad_symbols::{Library, Orientation, Rotation};
use lexpr::Value;
use spice_diagnostics::FileId;
use spice_resolve::ElementKind;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> common::TempDir {
    common::TempDir::new("readability", name)
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

fn load_test_library() -> Library {
    let libs_dir = libs_dir();
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

fn libs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("crates/kicad-symbols/tests/fixtures")
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

// --- the measured model --------------------------------------------------

/// Two pins on the same row/column are co-axial within this slop. Grid
/// coordinates are exact multiples of 1.27 mm, so anything above f64
/// round-trip noise is a real difference.
const TOL_MM: f64 = 0.01;

/// One placed body pin in world coordinates, tagged with its SPICE net.
/// `#PWR*` glyph pins are excluded: a glyph is decoration hung off a
/// rail pin, not a device terminal.
#[derive(Debug, Clone)]
struct BodyPin {
    refdes: String,
    x_mm: f64,
    y_mm: f64,
    net: String,
}

#[derive(Debug, Clone)]
struct Elem {
    refdes: String,
    kind: ElementKind,
    /// SPICE node names, terminal order preserved.
    nodes: Vec<String>,
    /// True for a `;@ power=` source: lowered to rail glyphs, never a body.
    is_power_source: bool,
    /// True for a top-level `X<n>` lowered to a hierarchical `(sheet …)`
    /// block. It is *on the drawing* and occupies its nets, so it must
    /// raise a net's degree — but it has no body pins here and no single
    /// current path, so it is never a chain member and never a DC edge.
    is_sheet: bool,
}

#[derive(Debug)]
struct Fixture {
    name: String,
    pins: Vec<BodyPin>,
    elements: Vec<Elem>,
    rail_nets: HashSet<String>,
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

    fn elem(&self, refdes: &str) -> Option<&Elem> {
        self.elements.iter().find(|e| e.refdes == refdes)
    }

    /// The element's emitted body pins.
    fn pins_of(&self, refdes: &str) -> Vec<&BodyPin> {
        self.pins.iter().filter(|p| p.refdes == refdes).collect()
    }

    /// The element has an emitted two-pin body whose geometry can be
    /// measured. A hierarchical-sheet instance never does.
    fn has_body(&self, refdes: &str) -> bool {
        self.pins.iter().any(|p| p.refdes == refdes)
    }

    /// Element centre = the mean of its emitted body pins.
    fn centre(&self, refdes: &str) -> Option<(f64, f64)> {
        let ps = self.pins_of(refdes);
        if ps.is_empty() {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let n = ps.len() as f64;
        Some((
            ps.iter().map(|p| p.x_mm).sum::<f64>() / n,
            ps.iter().map(|p| p.y_mm).sum::<f64>() / n,
        ))
    }

    /// **Rail stub** in ADR-15's role model: a two-terminal element with
    /// exactly one rail pin. It terminates a node rather than passing a
    /// signal along, and convention hangs it off that node's column, so
    /// it takes no part in a series chain and does not raise a net's
    /// signal degree.
    ///
    /// Re-derived from the netlist here rather than imported from
    /// `spice-layout`, following the `flow_geometry.rs` precedent: a
    /// metric that borrows the placer's own classification can only
    /// restate it, never falsify it.
    fn is_rail_stub(&self, refdes: &str) -> bool {
        let Some(e) = self.elem(refdes) else {
            return false;
        };
        e.nodes.len() == 2
            && e.nodes[0] != e.nodes[1]
            && e.nodes.iter().filter(|n| self.is_rail_net(n)).count() == 1
    }

    /// **Series-signal element**: two-terminal, not a power source, with
    /// NEITHER node rail-class. It lies *on* the signal path. This is the
    /// `flow_geometry.rs` F5 discriminator, and the candidate set for a
    /// series chain.
    fn is_series_signal(&self, refdes: &str) -> bool {
        let Some(e) = self.elem(refdes) else {
            return false;
        };
        !e.is_power_source
            && !e.is_sheet
            && e.nodes.len() == 2
            && e.nodes[0] != e.nodes[1]
            && !self.is_rail_net(&e.nodes[0])
            && !self.is_rail_net(&e.nodes[1])
    }

    /// Drawn, non-rail-stub elements touching `net` — the "signal
    /// degree" members. A net of signal degree 2 passes all of one
    /// member's signal into the other, which is exactly the link a
    /// series chain is made of.
    fn signal_members(&self, net: &str) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .elements
            .iter()
            .filter(|e| {
                !e.is_power_source
                    && !self.is_rail_stub(&e.refdes)
                    && e.nodes.iter().any(|n| n == net)
            })
            .map(|e| e.refdes.as_str())
            .collect();
        out.sort_unstable();
        out
    }
}

fn convert(name: &str, extra: &[&str]) -> (common::TempDir, PathBuf) {
    let dir = tempdir(name);
    if extra.is_empty() {
        let sch = spice_to_kicad(&fixtures_dir().join(format!("{name}.cir")), &dir)
            .unwrap_or_else(|e| panic!("convert {name}: {e}"));
        return (dir, sch);
    }
    // A pinned-arm conversion (a specific `--placer`, or `--no-refine`).
    // Deliberately NOT routed through `common::spice_to_kicad`: that
    // helper appends `common::placer_args()` — which would collide with
    // an explicit `--placer` under a scoreboard challenger run — and
    // records `t0.convert_fail` to the sink, which would file a pinned
    // arm's measurement under the row being collected for another placer.
    let out = dir.path().join(format!("{name}.kicad_sch"));
    let libs = libs_dir();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spice2kicad"));
    cmd.arg(fixtures_dir().join(format!("{name}.cir")))
        .arg("-t")
        .arg("schematic")
        .arg("-o")
        .arg(&out)
        .arg("-l")
        .arg(libs.join("Device.kicad_sym"))
        .arg("-l")
        .arg(libs.join("Simulation_SPICE.kicad_sym"))
        .arg("-l")
        .arg(libs.join("Amplifier_Operational.kicad_sym"))
        .arg("-l")
        .arg(libs.join("power.kicad_sym"))
        .arg("--no-layout-cache")
        .args(extra);
    let status = cmd.status().expect("invoke spice2kicad");
    assert!(status.success(), "convert {name} {extra:?}: {status}");
    (dir, out)
}

fn load_arm(name: &str, extra: &[&str]) -> Fixture {
    let (_guard, sch) = convert(name, extra);
    let src = std::fs::read_to_string(&sch).expect("read sch");
    let root = lexpr::from_str(&src).expect("parse sch");

    let library = load_test_library();
    let spice_src =
        std::fs::read_to_string(fixtures_dir().join(format!("{name}.cir"))).expect("read cir");
    let parsed = spice_parser::parse(&spice_src, FileId(0)).expect("parse spice");
    let resolved = spice_resolve::resolve(&parsed.netlist, &library).expect("resolve spice");

    let mut by_refdes: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut elements: Vec<Elem> = Vec::new();
    let mut rail_nets: HashSet<String> = HashSet::new();
    for el in &resolved.elements {
        let mut pairs = Vec::with_capacity(el.pin_mapping.len());
        for (i, kicad_pin) in el.pin_mapping.iter().enumerate() {
            if let Some(net) = el.nodes.get(i) {
                pairs.push((kicad_pin.clone(), net.clone()));
            }
        }
        by_refdes.insert(el.refdes.clone(), pairs);
        let is_power_source = matches!(el.role, spice_resolve::ElementRole::Power(_));
        for net in &el.nodes {
            if is_power_source || is_canonical_rail_name(net) {
                rail_nets.insert(net.clone());
            }
        }
        elements.push(Elem {
            refdes: el.refdes.clone(),
            kind: el.kind,
            nodes: el.nodes.clone(),
            is_power_source,
            is_sheet: false,
        });
    }
    // A top-level `X<n>` lowered to a hierarchical `(sheet …)` block is a
    // resolved *sheet instance*, not a resolved element, so it is absent
    // from `resolved.elements`. Leaving it out would be a silent
    // falsification: `opamp_inverting`'s `inv` node would read as degree
    // 2 (RIN, RF) and the metric would report a series CHAIN across an
    // op-amp's virtual ground — two resistors that are not in series at
    // all. It is on the drawing and it occupies its nets, so it counts.
    for si in &resolved.sheet_instances {
        for net in &si.nodes {
            if is_canonical_rail_name(net) {
                rail_nets.insert(net.clone());
            }
        }
        elements.push(Elem {
            refdes: si.refdes.clone(),
            kind: ElementKind::Subckt,
            nodes: si.nodes.clone(),
            is_power_source: false,
            is_sheet: true,
        });
    }

    let mut pins = Vec::new();
    for sym in children(&root, "symbol") {
        let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
            continue;
        };
        if refdes.starts_with("#PWR") || refdes.starts_with("#FLG") {
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

    Fixture {
        name: name.to_string(),
        pins,
        elements,
        rail_nets,
    }
}

fn load(name: &str) -> Fixture {
    load_arm(name, &[])
}

// --- A: series-chain axis uniformity -------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Axis {
    Horizontal,
    Vertical,
}

/// A member's drawn pin axis, or `None` when it is neither (which can
/// only happen off-grid, and is always counted as off-axis).
fn member_axis(f: &Fixture, refdes: &str) -> Option<Axis> {
    let ps = f.pins_of(refdes);
    if ps.len() != 2 {
        return None;
    }
    let dx = (ps[0].x_mm - ps[1].x_mm).abs();
    let dy = (ps[0].y_mm - ps[1].y_mm).abs();
    match (dx > TOL_MM, dy > TOL_MM) {
        (true, false) => Some(Axis::Horizontal),
        (false, true) => Some(Axis::Vertical),
        _ => None,
    }
}

/// One series chain, in path order.
#[derive(Debug)]
struct Chain {
    /// Members, ordered along the path.
    members: Vec<String>,
    /// `entry`/`exit` net for each member, in the same order — the net
    /// the chain arrives on and the net it leaves by. Endpoints use
    /// their free (non-chain) net for the missing side.
    ports: Vec<(String, String)>,
}

/// Every maximal series chain of two or more members.
///
/// Chain candidates are the drawn, two-pin, series-signal elements; two
/// candidates are linked when they share a non-rail net of signal degree
/// exactly 2. Every candidate has at most two nets, so every vertex has
/// chain-degree <= 2 and each component is a path or a cycle by
/// construction.
#[allow(clippy::too_many_lines)] // one cohesive walk; splitting it hides the traversal
fn chains(f: &Fixture) -> Vec<Chain> {
    let mut candidates: Vec<String> = f
        .elements
        .iter()
        .map(|e| e.refdes.clone())
        .filter(|r| f.is_series_signal(r) && f.pins_of(r).len() == 2)
        .collect();
    candidates.sort();
    let candidate_set: BTreeSet<&str> = candidates.iter().map(String::as_str).collect();

    // refdes -> the (net, neighbour) links it takes part in.
    let mut links: BTreeMap<&str, Vec<(String, String)>> = BTreeMap::new();
    let mut nets: BTreeSet<&str> = BTreeSet::new();
    for r in &candidates {
        for n in &f.elem(r).expect("candidate is an element").nodes {
            nets.insert(n.as_str());
        }
    }
    for net in nets {
        if f.is_rail_net(net) {
            continue;
        }
        let members = f.signal_members(net);
        if members.len() != 2 {
            continue;
        }
        let (a, b) = (members[0], members[1]);
        if !candidate_set.contains(a) || !candidate_set.contains(b) {
            continue;
        }
        links
            .entry(a)
            .or_default()
            .push((net.to_string(), b.to_string()));
        links
            .entry(b)
            .or_default()
            .push((net.to_string(), a.to_string()));
    }

    // Walk each component from an endpoint (chain-degree <= 1). A cycle
    // has no endpoint; break it at its lexicographically smallest member
    // so the walk is deterministic (ADR-28 records this choice).
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut out: Vec<Chain> = Vec::new();
    let starts = candidates
        .iter()
        .filter(|r| links.get(r.as_str()).map_or(0, Vec::len) <= 1)
        .chain(candidates.iter());
    for start in starts {
        let start = start.as_str();
        if seen.contains(start) || !candidate_set.contains(start) {
            continue;
        }
        let mut order: Vec<&str> = vec![start];
        let mut via: Vec<String> = Vec::new(); // net joining order[i] and order[i+1]
        seen.insert(start);
        let mut cur = start;
        let mut prev: Option<&str> = None;
        loop {
            let Some(next) = links.get(cur).and_then(|ls| {
                ls.iter()
                    .find(|(_, nb)| Some(nb.as_str()) != prev && !seen.contains(nb.as_str()))
            }) else {
                break;
            };
            let nb = candidate_set
                .get(next.1.as_str())
                .copied()
                .expect("neighbour is a candidate");
            via.push(next.0.clone());
            order.push(nb);
            seen.insert(nb);
            prev = Some(cur);
            cur = nb;
        }
        if order.len() < 2 {
            continue;
        }
        // Entry/exit nets per member.
        let mut ports: Vec<(String, String)> = Vec::with_capacity(order.len());
        for (i, r) in order.iter().enumerate() {
            let nodes = &f.elem(r).expect("member is an element").nodes;
            let other = |used: &str| -> String {
                nodes
                    .iter()
                    .find(|n| n.as_str() != used)
                    .cloned()
                    .unwrap_or_else(|| used.to_string())
            };
            let exit = if i < via.len() {
                via[i].clone()
            } else {
                other(&via[i - 1])
            };
            let entry = if i == 0 {
                other(&via[0])
            } else {
                via[i - 1].clone()
            };
            ports.push((entry, exit));
        }
        // The walk direction is arbitrary (both endpoints are valid
        // starts) and every count below is invariant under reversing it,
        // so orient the chain the way a reader scans the sheet:
        // left-to-right, then top-to-bottom. Cosmetic only.
        let mut members: Vec<String> = order.iter().map(|s| (*s).to_string()).collect();
        let key = |r: &str| f.centre(r).unwrap_or((0.0, 0.0));
        let (first, last) = (key(&members[0]), key(members.last().expect("non-empty")));
        if (last.0, last.1) < (first.0, first.1) {
            members.reverse();
            ports.reverse();
            for p in &mut ports {
                std::mem::swap(&mut p.0, &mut p.1);
            }
        }
        out.push(Chain { members, ports });
    }
    out.sort_by(|a, b| a.members.cmp(&b.members));
    out
}

/// A member's direction of travel along the chain, as a unit step: the
/// sign of `exit_pin - entry_pin` on its dominant component.
fn travel(f: &Fixture, refdes: &str, entry: &str, exit: &str) -> Option<(i32, i32)> {
    let ps = f.pins_of(refdes);
    if ps.len() != 2 {
        return None;
    }
    let e = ps.iter().find(|p| p.net == entry)?;
    let x = ps.iter().find(|p| p.net == exit)?;
    let dx = x.x_mm - e.x_mm;
    let dy = x.y_mm - e.y_mm;
    if dx.abs() > dy.abs() {
        Some((if dx > 0.0 { 1 } else { -1 }, 0))
    } else if dy.abs() > TOL_MM {
        Some((0, if dy > 0.0 { 1 } else { -1 }))
    } else {
        None
    }
}

/// Per-chain `(off-axis members, reversed members)`, with the offending
/// refdeses, under the axis that reads the chain most charitably.
///
/// Charitable, not canonical: the metric picks whichever of the two axes
/// minimises `(off_axis, reversals)` lexicographically, so it never
/// invents a violation by insisting on an axis the drawing did not
/// choose. Ties break toward HORIZONTAL — the project's own convention
/// is that signal flows left to right (F3/F5), so a chain drawn half
/// horizontal and half vertical is graded against the horizontal half.
fn score_chain(f: &Fixture, c: &Chain) -> (Vec<String>, Vec<String>) {
    let mut best: Option<(usize, usize, Vec<String>, Vec<String>)> = None;
    for axis in [Axis::Horizontal, Axis::Vertical] {
        let mut off: Vec<String> = Vec::new();
        let mut on: Vec<(&str, (i32, i32))> = Vec::new();
        for (i, r) in c.members.iter().enumerate() {
            if member_axis(f, r) == Some(axis) {
                let (entry, exit) = &c.ports[i];
                if let Some(t) = travel(f, r, entry, exit) {
                    on.push((r.as_str(), t));
                } else {
                    off.push(r.clone());
                }
            } else {
                off.push(r.clone());
            }
        }
        // Majority direction among the on-axis members.
        let mut tally: BTreeMap<(i32, i32), usize> = BTreeMap::new();
        for (_, t) in &on {
            *tally.entry(*t).or_default() += 1;
        }
        let majority = tally
            .iter()
            .max_by_key(|(k, v)| (**v, **k))
            .map(|(k, _)| *k);
        let reversed: Vec<String> = on
            .iter()
            .filter(|(_, t)| Some(*t) != majority)
            .map(|(r, _)| (*r).to_string())
            .collect();
        let cand = (off.len(), reversed.len(), off, reversed);
        if best.as_ref().is_none_or(|b| (cand.0, cand.1) < (b.0, b.1)) {
            best = Some(cand);
        }
    }
    let (_, _, off, rev) = best.expect("two axes were tried");
    (off, rev)
}

/// `(chain.axis, chain.reversal, chain.members)` for a fixture, plus the
/// human-readable detail.
fn chain_metrics(f: &Fixture) -> (usize, usize, usize, Vec<String>) {
    let cs = chains(f);
    let (mut axis, mut rev, mut members) = (0, 0, 0);
    let mut detail = Vec::new();
    for c in &cs {
        let (off, reversed) = score_chain(f, c);
        members += c.members.len();
        axis += off.len();
        rev += reversed.len();
        detail.push(format!(
            "chain [{}]: axes {:?}, off-axis {off:?}, reversed {reversed:?}",
            c.members.join(" -> "),
            c.members
                .iter()
                .map(|r| member_axis(f, r))
                .collect::<Vec<_>>()
        ));
    }
    (axis, rev, members, detail)
}

// --- B: shared-current-path stacking -------------------------------------

/// The two terminals a DC current flows between, for the elements that
/// conduct DC at all.
///
/// * two-terminal conductors (R, L, D, and a *drawn* V/I source) use
///   their two nodes;
/// * a BJT uses collector-emitter, a FET drain-source. The base / gate
///   is deliberately NOT a DC edge: SPICE order is `c b e` / `d g s`,
///   and the control terminal carries either no DC (a gate) or the
///   current path's `1/beta` (a base). Treating a base as a conductor
///   would raise the DC degree of every bias node and dissolve exactly
///   the rail-to-rail divider this metric exists to see. ADR-28 records
///   this as a deliberate choice with a stated alternative;
/// * capacitors conduct no DC and have no edge — which is why an RC
///   low-pass's `R1`/`C1` is not asked to stack;
/// * `;@ power=` sources are glyphs, not bodies, and multi-terminal
///   subckt instances have no single current path, so neither gets an edge.
fn dc_edge(f: &Fixture, e: &Elem) -> Option<(String, String)> {
    if e.is_power_source || e.is_sheet || !f.has_body(&e.refdes) {
        return None;
    }
    let pick = |i: usize, j: usize| -> Option<(String, String)> {
        let (a, b) = (e.nodes.get(i)?, e.nodes.get(j)?);
        if a == b {
            return None;
        }
        Some((a.clone(), b.clone()))
    };
    match e.kind {
        ElementKind::Resistor
        | ElementKind::Inductor
        | ElementKind::Diode
        | ElementKind::VoltageSrc
        | ElementKind::CurrentSrc => pick(0, 1),
        ElementKind::Bjt | ElementKind::Mosfet | ElementKind::Jfet => pick(0, 2),
        _ => None,
    }
}

/// Rail nets reachable from `from` in the DC graph with `skip`'s edge
/// removed.
fn rails_reachable(
    f: &Fixture,
    adj: &BTreeMap<&str, Vec<(&str, &str)>>, // net -> [(other net, via refdes)]
    from: &str,
    skip: &str,
) -> BTreeSet<String> {
    let mut rails = BTreeSet::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut q: VecDeque<&str> = VecDeque::new();
    let Some((k, _)) = adj.get_key_value(from).map(|(k, v)| (*k, v)) else {
        if f.is_rail_net(from) {
            rails.insert(from.to_string());
        }
        return rails;
    };
    seen.insert(k);
    q.push_back(k);
    while let Some(n) = q.pop_front() {
        if f.is_rail_net(n) {
            rails.insert(n.to_string());
            // A rail is a terminus: current entering it leaves through
            // the supply, not through the next signal net.
            continue;
        }
        for (other, via) in adj.get(n).into_iter().flatten() {
            if *via == skip || seen.contains(other) {
                continue;
            }
            seen.insert(other);
            q.push_back(other);
        }
    }
    if f.is_rail_net(from) {
        rails.insert(from.to_string());
    }
    rails
}

/// The nets at an element's DC-relevant terminals — every terminal a DC
/// current can enter or leave by.
///
/// A *stronger* notion than "has a DC edge", and deliberately so. An
/// op-amp symbol or a hierarchical `(sheet …)` instance conducts DC at
/// its pins without having any single current path *through* it, so it
/// has no DC edge — but a net it sits on is emphatically not a
/// two-element series node, and must not be read as one. Counting its
/// terminals here is what stops `opamp_inverting`'s virtual ground from
/// reading as `RIN` in series with `RF`.
fn dc_terminals(e: &Elem) -> Vec<&str> {
    if e.is_power_source {
        return Vec::new();
    }
    match e.kind {
        // No DC through a capacitor, at either end.
        ElementKind::Capacitor => Vec::new(),
        // `c b e` / `d g s [b]`: index 1 is the control terminal.
        ElementKind::Bjt | ElementKind::Mosfet | ElementKind::Jfet => e
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .map(|(_, n)| n.as_str())
            .collect(),
        _ => e.nodes.iter().map(String::as_str).collect(),
    }
}

/// Every DC-series pair, as `(u, v, shared net)`. See the module docs.
fn dc_series_pairs(f: &Fixture) -> Vec<(String, String, String)> {
    let edges: BTreeMap<&str, (String, String)> = f
        .elements
        .iter()
        .filter_map(|e| dc_edge(f, e).map(|ab| (e.refdes.as_str(), ab)))
        .collect();

    let mut adj: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for (r, (a, b)) in &edges {
        adj.entry(a.as_str()).or_default().push((b.as_str(), r));
        adj.entry(b.as_str()).or_default().push((a.as_str(), r));
    }

    // Which elements sit on a path between two DISTINCT rail nets?
    let mut conducts: BTreeSet<&str> = BTreeSet::new();
    for (r, (a, b)) in &edges {
        let ra = rails_reachable(f, &adj, a, r);
        let rb = rails_reachable(f, &adj, b, r);
        if ra.iter().any(|x| rb.iter().any(|y| x != y)) {
            conducts.insert(r);
        }
    }

    // DC degree, over EVERY element's DC terminals — not just the ones
    // with an edge. See `dc_terminals`.
    let mut degree: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for e in &f.elements {
        for net in dc_terminals(e) {
            degree.entry(net).or_default().insert(e.refdes.as_str());
        }
    }

    let mut out = Vec::new();
    for (net, on) in &degree {
        if f.is_rail_net(net) || on.len() != 2 {
            continue;
        }
        let mut it = on.iter();
        let (u, v) = (*it.next().expect("two"), *it.next().expect("two"));
        // Both must actually carry the current *through* themselves…
        if !edges.contains_key(u) || !edges.contains_key(v) {
            continue;
        }
        // …and both must sit on a supply-to-ground path.
        if !conducts.contains(u) || !conducts.contains(v) {
            continue;
        }
        out.push((u.to_string(), v.to_string(), (*net).to_string()));
    }
    out.sort();
    out
}

/// `(stack.side_by_side, stack.pairs)` plus detail.
///
/// A pair is stacked when it is drawn taller than wide. `|dx| > |dy|` is
/// a violation; the exactly-diagonal case (`|dx| == |dy|`) is genuinely
/// ambiguous and is NOT counted, so the metric never invents a defect
/// out of a tie.
fn stacking_metrics(f: &Fixture) -> (usize, usize, Vec<String>) {
    let pairs = dc_series_pairs(f);
    let mut bad = Vec::new();
    for (u, v, net) in &pairs {
        let (Some((ux, uy)), Some((vx, vy))) = (f.centre(u), f.centre(v)) else {
            continue;
        };
        let (dx, dy) = ((ux - vx).abs(), (uy - vy).abs());
        if dx > dy + TOL_MM {
            bad.push(format!(
                "{u}/{v} (series on `{net}`) drawn side-by-side: |dx|={dx:.2} > |dy|={dy:.2}"
            ));
        }
    }
    (bad.len(), pairs.len(), bad)
}

// --- the fixtures --------------------------------------------------------

/// Every fixture the suite converts, in `flow_geometry.rs`'s order.
/// There are no budget literals here on purpose — see the module docs.
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
    "rc_phase_shift",
    "two_stage_amp",
    "cascode_amp",
    "lc_ladder_lpf",
    "sallen_key_lpf",
    "wien_bridge_osc",
    "sallen_key_driven",
    "shunt_feedback_amp",
];

/// Report both metrics for every fixture to the ADR-23 sink.
///
/// **Informational: this test asserts no budget.** ADR-23 D2's contract
/// is that a fixture-enumerating verifier records on the line before its
/// assertion; the promotion's own post-mortem ("two blind cells") is
/// what a metric that reports nothing costs. So the recording is the
/// point of this test, and the only assertion is that the measurement
/// actually ran on every fixture — a metric silently absent is worse
/// than a metric that reads badly.
///
/// `S2K_READABILITY_DUMP=1` prints the per-fixture table and the
/// offending refdeses.
#[test]
fn readability_metrics_are_reported_for_every_fixture() {
    let dump = std::env::var("S2K_READABILITY_DUMP").is_ok();
    if dump {
        println!(
            "{:<24} {:>10} {:>10} {:>10} {:>10} {:>10}",
            "fixture", "chain.axis", "chain.rev", "members", "stack.sbs", "pairs"
        );
    }
    let mut measured = 0usize;
    for name in FIXTURES {
        let f = load(name);
        let (axis, rev, members, chain_detail) = chain_metrics(&f);
        let (sbs, pairs, stack_detail) = stacking_metrics(&f);
        common::scoreboard::record_count("chain.axis", name, axis);
        common::scoreboard::record_count("chain.reversal", name, rev);
        common::scoreboard::record_count("chain.members", name, members);
        common::scoreboard::record_count("stack.side_by_side", name, sbs);
        common::scoreboard::record_count("stack.pairs", name, pairs);
        measured += 1;
        if dump {
            println!(
                "{:<24} {axis:>10} {rev:>10} {members:>10} {sbs:>10} {pairs:>10}",
                f.name
            );
            for d in chain_detail.iter().chain(stack_detail.iter()) {
                println!("      {d}");
            }
        }
    }
    assert_eq!(
        measured,
        FIXTURES.len(),
        "every fixture must be measured — a blind cell is not conservatively blind (ADR-23)"
    );
}

// --- specimen rankings ---------------------------------------------------

/// **The acceptance test for metric A.** `lc_ladder_lpf`'s
/// `RS -> L1 -> L2 -> L3` is one series chain; the deterministic seed
/// draws all four members on one axis in one direction, and the shipped
/// drawing draws them at four different rotations. A metric that does
/// not rank the seed better is measuring the wrong thing.
///
/// Asserted in `<=` form on purpose. It fires only if the drawing the
/// owner calls textbook becomes strictly WORSE than the one they call
/// damage — i.e. only if the metric's own validation inverts. It can
/// never block a change that *improves* the shipping placer, which is
/// what "informational at birth" requires (ADR-28). Non-vacuity is
/// carried by the synthetic control arms below, not by this test.
#[test]
fn chain_axis_ranks_the_textbook_ladder_seed_above_the_shipped_drawing() {
    let seed = load_arm("lc_ladder_lpf", &["--no-refine"]);
    let shipped = load("lc_ladder_lpf");

    let seed_chains = chains(&seed);
    assert_eq!(
        seed_chains.len(),
        1,
        "lc_ladder_lpf must present exactly one series chain; got {:?}",
        seed_chains
            .iter()
            .map(|c| c.members.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        seed_chains[0].members,
        vec!["RS", "L1", "L2", "L3"],
        "the chain must be the ladder's series path, in order"
    );

    let (s_axis, s_rev, _, s_detail) = chain_metrics(&seed);
    let (p_axis, p_rev, _, p_detail) = chain_metrics(&shipped);
    println!("lc_ladder_lpf seed     : axis={s_axis} reversal={s_rev}  {s_detail:?}");
    println!("lc_ladder_lpf shipped  : axis={p_axis} reversal={p_rev}  {p_detail:?}");

    assert!(
        s_axis <= p_axis && s_rev <= p_rev,
        "metric A inverted the specimen ranking: the --no-refine seed \
         (axis={s_axis}, reversal={s_rev}) must not read worse than the shipped drawing \
         (axis={p_axis}, reversal={p_rev}).\n  seed:    {s_detail:?}\n  shipped: {p_detail:?}"
    );
}

/// **The acceptance test for metric B.** The owner prefers the
/// pre-promotion placer on `cascode_amp` specifically because it stacks
/// `Q1`/`Q2`; the shipping placer sets them side by side. Same `<=`
/// form, same reason, as the chain test above.
#[test]
fn stacking_ranks_the_champion_above_flow_seed_on_the_cascode() {
    let champion = load_arm("cascode_amp", &["--placer", "champion"]);
    let shipped = load("cascode_amp");

    let (c_sbs, c_pairs, c_detail) = stacking_metrics(&champion);
    let (s_sbs, s_pairs, s_detail) = stacking_metrics(&shipped);
    println!("cascode_amp champion : side_by_side={c_sbs} of {c_pairs} pairs  {c_detail:?}");
    println!("cascode_amp shipped  : side_by_side={s_sbs} of {s_pairs} pairs  {s_detail:?}");

    assert_eq!(
        c_pairs, s_pairs,
        "the DC-series pair set is a property of the NETLIST and must not depend on \
         the placer; champion saw {c_pairs} pairs, the shipping placer {s_pairs}"
    );
    assert!(
        c_pairs >= 5,
        "cascode_amp's own header names a two-transistor stack and a three-resistor \
         rail-to-ground ladder; the metric must see at least 5 DC-series pairs, saw {c_pairs}"
    );
    assert!(
        c_sbs <= s_sbs,
        "metric B inverted the specimen ranking: the champion ({c_sbs}) must not read \
         worse than the shipping placer ({s_sbs}) on the fixture the owner prefers the \
         champion for.\n  champion: {c_detail:?}\n  shipped:  {s_detail:?}"
    );
}

// --- discriminators: the assertions that keep the metrics falsifiable ----

/// Metric B says "devices in series on a DC current path stack in Y". If
/// the predicate silently widened to "any two devices sharing a net",
/// the metric would demand a **differential pair** be stacked — which is
/// wrong, and worse, a placer changed to satisfy it would score 0.
///
/// `cascode_amp` and `diff_pair` carry the discriminating pair: the
/// cascode's `Q1`/`Q2` share `c1`, whose only DC conductors are those
/// two, so all of Q1's collector current is Q2's emitter current — one
/// current, one column. `diff_pair`'s `Q1`/`Q2` share `tail` with
/// `RTAIL`, so the net has DC degree 3, the current *splits*, and the
/// conventional drawing is side by side. Same element kind, opposite
/// verdicts, decided from the netlist alone.
#[test]
fn stacking_discriminator_separates_the_cascode_from_the_diff_pair() {
    let cascode = load("cascode_amp");
    let pairs = dc_series_pairs(&cascode);
    let has = |a: &str, b: &str| {
        pairs
            .iter()
            .any(|(u, v, _)| (u == a && v == b) || (u == b && v == a))
    };
    assert!(
        has("Q1", "Q2"),
        "the cascode stack Q1/Q2 must be a DC-series pair; got {pairs:?}"
    );
    assert!(
        has("RC", "Q2"),
        "a collector load sits directly above its transistor; RC/Q2 must be a pair"
    );
    assert!(
        has("Q1", "RE"),
        "an emitter degeneration resistor sits directly below its transistor"
    );
    assert!(
        has("RB1", "RB2") && has("RB2", "RB3"),
        "the rail-to-ground bias ladder must be three stacked resistors; got {pairs:?}"
    );

    let dp = load("diff_pair");
    let dp_pairs = dc_series_pairs(&dp);
    assert!(
        !dp_pairs
            .iter()
            .any(|(u, v, _)| (u == "Q1" && v == "Q2") || (u == "Q2" && v == "Q1")),
        "a differential pair is drawn SIDE BY SIDE on purpose — its two transistors \
         share `tail` with RTAIL, so the current splits and they are not in series. \
         If this fires, metric B has widened into a demand that every pair stack: \
         {dp_pairs:?}"
    );
}

/// Metric A's mirror of the same rule. If the chain predicate widened to
/// "every two-terminal element", a bypass capacitor would be dragged
/// into the chain and asked to lie down sideways — ADR-15's recorded
/// "capacitors are horizontal is WRONG" trap.
///
/// `common_emitter` carries the discriminating pair: `CIN` (`in` -> `b`,
/// both signal nets) is a chain candidate; `CE` (`e` -> `0`, one rail
/// pin) is a rail stub and must never be one.
#[test]
fn chain_discriminator_keeps_rail_stubs_out_of_the_chain() {
    let f = load("common_emitter");
    assert!(
        f.is_rail_stub("CE") && !f.is_series_signal("CE"),
        "CE (bypass cap, one pin on ground) is a rail stub, never a chain member"
    );
    assert!(
        f.is_series_signal("CIN") && !f.is_rail_stub("CIN"),
        "CIN (in -> b, both signal nets) is a chain candidate"
    );
    for c in chains(&f) {
        assert!(
            !c.members.iter().any(|r| r == "CE"),
            "a rail stub reached a series chain: {:?}",
            c.members
        );
    }
}

// --- synthetic control arms: the metrics are not vacuously zero ----------

/// Build a fixture out of hand-placed pins, bypassing conversion.
///
/// The specimen tests above are `<=` rankings, which a metric that
/// always returns 0 would satisfy vacuously. These control arms are what
/// make the metrics falsifiable: hand-drawn geometry with a known answer,
/// independent of any placer. Same role as
/// `placement_stability.rs::p11_delta_grouping_catches_one_symbol_out_of_step`.
fn synthetic(elements: Vec<Elem>, pins: Vec<BodyPin>, rails: &[&str]) -> Fixture {
    Fixture {
        name: "synthetic".to_string(),
        pins,
        elements,
        rail_nets: rails.iter().map(|s| (*s).to_string()).collect(),
    }
}

fn two_pin(kind: ElementKind, refdes: &str, a: &str, b: &str) -> Elem {
    Elem {
        refdes: refdes.to_string(),
        kind,
        nodes: vec![a.to_string(), b.to_string()],
        is_power_source: false,
        is_sheet: false,
    }
}

fn pin(refdes: &str, net: &str, x: f64, y: f64) -> BodyPin {
    BodyPin {
        refdes: refdes.to_string(),
        net: net.to_string(),
        x_mm: x,
        y_mm: y,
    }
}

/// A four-element chain `n0 -> n1 -> n2 -> n3 -> n4`, drawn the textbook
/// way (all horizontal, all left-to-right) and then three ways it can go
/// wrong. The counts are the metric's definition, checked against a
/// drawing whose right answer is obvious by eye.
#[test]
fn chain_metric_counts_a_known_synthetic_ladder() {
    let els = vec![
        two_pin(ElementKind::Resistor, "E1", "n0", "n1"),
        two_pin(ElementKind::Inductor, "E2", "n1", "n2"),
        two_pin(ElementKind::Inductor, "E3", "n2", "n3"),
        two_pin(ElementKind::Inductor, "E4", "n3", "n4"),
    ];

    // (a) textbook: four horizontal members, all running +X.
    let good = synthetic(
        els.clone(),
        vec![
            pin("E1", "n0", 0.0, 0.0),
            pin("E1", "n1", 10.0, 0.0),
            pin("E2", "n1", 20.0, 0.0),
            pin("E2", "n2", 30.0, 0.0),
            pin("E3", "n2", 40.0, 0.0),
            pin("E3", "n3", 50.0, 0.0),
            pin("E4", "n3", 60.0, 0.0),
            pin("E4", "n4", 70.0, 0.0),
        ],
        &[],
    );
    let (axis, rev, members, detail) = chain_metrics(&good);
    assert_eq!(
        (axis, rev, members),
        (0, 0, 4),
        "the textbook ladder must score zero: {detail:?}"
    );

    // (b) one member rotated 90 degrees: one off-axis, no reversal.
    let mut pins = good.pins.clone();
    for p in &mut pins {
        if p.refdes == "E3" {
            p.x_mm = 45.0;
            p.y_mm = if p.net == "n2" { 0.0 } else { 10.0 };
        }
    }
    let tilted = synthetic(els.clone(), pins, &[]);
    let (axis, rev, _, detail) = chain_metrics(&tilted);
    assert_eq!(
        (axis, rev),
        (1, 0),
        "one rotated member is ONE axis violation and no reversal: {detail:?}"
    );

    // (c) one member drawn backwards: on-axis, reversed.
    let mut pins = good.pins.clone();
    for p in &mut pins {
        if p.refdes == "E3" {
            p.x_mm = if p.net == "n2" { 50.0 } else { 40.0 };
        }
    }
    let flipped = synthetic(els.clone(), pins, &[]);
    let (axis, rev, _, detail) = chain_metrics(&flipped);
    assert_eq!(
        (axis, rev),
        (0, 1),
        "one backwards member is ONE reversal and no axis violation — the two counts \
         are disjoint by construction: {detail:?}"
    );

    // (d) the `lc_ladder_lpf` shape: four members, four orientations —
    // two horizontal (one backwards) and two vertical (one backwards).
    // Both readings score (2 off-axis, 1 reversed), so the HORIZONTAL
    // tie-break decides which two are named, and the totals are (2, 1).
    let mixed = synthetic(
        els,
        vec![
            pin("E1", "n0", 0.0, 10.0),
            pin("E1", "n1", 0.0, 0.0),
            pin("E2", "n1", 20.0, 0.0),
            pin("E2", "n2", 30.0, 0.0),
            pin("E3", "n2", 40.0, 0.0),
            pin("E3", "n3", 40.0, 10.0),
            pin("E4", "n3", 70.0, 0.0),
            pin("E4", "n4", 60.0, 0.0),
        ],
        &[],
    );
    let (axis, rev, _, detail) = chain_metrics(&mixed);
    assert_eq!(
        (axis, rev),
        (2, 1),
        "the four-orientation ladder must read as two off-axis members plus one \
         reversal: {detail:?}"
    );
    let chosen = chains(&mixed);
    let (off, _) = score_chain(&mixed, &chosen[0]);
    assert_eq!(
        off,
        vec!["E1".to_string(), "E3".to_string()],
        "on a tie the metric grades against the HORIZONTAL reading, so the two \
         VERTICAL members are the ones named off-axis"
    );
}

/// A rail-to-rail divider drawn stacked, then drawn side by side. Also
/// pins down the two exclusions the definition depends on: a capacitor
/// carries no DC and never forms a pair, and a resistor whose far end
/// reaches no second rail is not on a supply path.
#[test]
fn stacking_metric_counts_a_known_synthetic_divider() {
    let els = vec![
        two_pin(ElementKind::Resistor, "R1", "VCC", "mid"),
        two_pin(ElementKind::Resistor, "R2", "mid", "0"),
    ];
    let stacked = synthetic(
        els.clone(),
        vec![
            pin("R1", "VCC", 0.0, 0.0),
            pin("R1", "mid", 0.0, 10.0),
            pin("R2", "mid", 0.0, 20.0),
            pin("R2", "0", 0.0, 30.0),
        ],
        &["VCC", "0"],
    );
    let (sbs, pairs, detail) = stacking_metrics(&stacked);
    assert_eq!(
        (sbs, pairs),
        (0, 1),
        "a stacked divider is one pair and no violation: {detail:?}"
    );

    let spread = synthetic(
        els,
        vec![
            pin("R1", "VCC", 0.0, 0.0),
            pin("R1", "mid", 10.0, 0.0),
            pin("R2", "mid", 20.0, 0.0),
            pin("R2", "0", 30.0, 0.0),
        ],
        &["VCC", "0"],
    );
    let (sbs, pairs, detail) = stacking_metrics(&spread);
    assert_eq!(
        (sbs, pairs),
        (1, 1),
        "the same divider drawn sideways is ONE violation: {detail:?}"
    );

    // A capacitor carries no DC: R + C is not a current path, so an RC
    // low-pass is never asked to stack.
    let rc = synthetic(
        vec![
            two_pin(ElementKind::Resistor, "R1", "in", "out"),
            two_pin(ElementKind::Capacitor, "C1", "out", "0"),
        ],
        vec![
            pin("R1", "in", 0.0, 0.0),
            pin("R1", "out", 10.0, 0.0),
            pin("C1", "out", 20.0, 0.0),
            pin("C1", "0", 30.0, 0.0),
        ],
        &["0"],
    );
    let (sbs, pairs, _) = stacking_metrics(&rc);
    assert_eq!(
        (sbs, pairs),
        (0, 0),
        "an RC low-pass has no DC current path and must form no stacking pair"
    );

    // Two resistors in series on a SIGNAL path (no second rail at the
    // far end) are a chain, not a stack — they belong to metric A.
    let ladder = synthetic(
        vec![
            two_pin(ElementKind::Resistor, "R1", "in", "n1"),
            two_pin(ElementKind::Resistor, "R2", "n1", "out"),
        ],
        vec![
            pin("R1", "in", 0.0, 0.0),
            pin("R1", "n1", 10.0, 0.0),
            pin("R2", "n1", 20.0, 0.0),
            pin("R2", "out", 30.0, 0.0),
        ],
        &["0"],
    );
    let (sbs, pairs, _) = stacking_metrics(&ladder);
    assert_eq!(
        (sbs, pairs),
        (0, 0),
        "a signal-path resistor ladder reaches no second rail and must NOT be asked \
         to stack — that is metric A's business, and demanding both would be a \
         contradiction"
    );
}
