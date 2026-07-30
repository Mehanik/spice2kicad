//! DC-path signal-flow direction analysis (roadmap B1).
//!
//! A **computed-but-unconsumed** pass that produces a *directed* signal-flow
//! graph over `Signal`-class nets, grounded in each device's DC-path
//! current-flow direction (TUM ESFG: "traverse supply→ground in current-flow
//! direction"). It is the principled successor to `layers.rs`'s ad-hoc
//! source-drives-outward adjacency and, crucially, its `break_cycles` step —
//! which *silently reverses* feedback edges. This pass **marks** feedback
//! edges via a greedy feedback-arc-set instead of reversing them, so a later
//! step (roadmap B2/B3) can layer the forward signal path without the
//! distortion a reversed edge introduces.
//!
//! Nothing in the production pipeline calls [`signal_flow`] yet: it is a pure
//! add with unit tests, registered `pub` so the compiler does not flag it as
//! dead code. Wiring it into `assign_x_layers` is roadmap B3.
//!
//! # Model
//!
//! Nodes are `Signal`-class nets (rails / ground are out of the graph, exactly
//! as `net_class` intends). Edges are directed net→net *contributions*:
//!
//! * **Active devices contribute FIXED directed edges** in current-flow
//!   direction, with the control terminal as an input:
//!   - BJT `Q c b e` → `b→c` (base controls collector) and `c→e`
//!     (collector→emitter current path). Base is the control input.
//!   - MOSFET `M d g s [b]` / JFET `J d g s` → `g→d` and `d→s`
//!     (drain→source). Gate is the control input.
//!   - Diode `D a c` → `a→c` (anode→cathode).
//!   - VCVS `E o+ o- c+ c-` / VCCS `G …` → `c+→o+` (control drives output).
//!   - Voltage / current source `+ -` (NOT `*@power`-tagged) → `+→-`.
//! * **Passives (R/L/C) contribute UNDIRECTED edges** — they have no intrinsic
//!   direction. Their effective direction is imposed by the traversal below:
//!   a passive bridging a device *output* net to a device *input* (control)
//!   net is oriented output→input (a feedback resistor from an op-amp output
//!   back to its summing junction runs output→input); otherwise it is oriented
//!   along the seed-rooted breadth-first rank.
//!
//! # Feedback marking (not reversing)
//!
//! Once every edge is directed, a deterministic **Eades greedy
//! feedback-arc-set** computes a linear vertex sequence; any edge running
//! backward in that sequence is *marked* `feedback = true`. The edge is kept
//! in its original direction — the reverse edge is never synthesised — so the
//! forward layering is undistorted and the feedback span is available for
//! B2/B3 to treat specially.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use spice_policy::CheckedNetlist;
use spice_resolve::{ElementKind, ElementRole, PortDir};

use crate::net_class::{NetClass, NetClassMap};

/// Whether a directed edge comes from a device's fixed current path or from an
/// oriented passive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// A fixed active-device current-flow edge (never re-oriented).
    Device,
    /// A passive (R/L/C) edge whose direction was imposed by traversal.
    Passive,
}

/// One directed contribution to the signal-flow graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowEdge {
    /// Index into `checked.elements` of the element carrying this edge.
    pub element: usize,
    /// Upstream signal net (tail), in current-flow direction.
    pub from: String,
    /// Downstream signal net (head).
    pub to: String,
    pub kind: EdgeKind,
    /// `true` when greedy-FAS identified this as a feedback arc. The edge is
    /// **marked, not reversed** — its `from`/`to` are unchanged.
    pub feedback: bool,
}

/// Directed signal-flow graph produced by [`signal_flow`].
///
/// Deterministic: every field is derived through sorted (`BTree*`) iteration,
/// so two runs on the same netlist produce byte-identical output regardless of
/// `HashMap` ordering (cf. the T8 determinism note in `solver/anneal.rs`).
#[derive(Debug, Clone)]
pub struct SignalFlow {
    /// All directed edges, sorted by `(from, to, element)`. Feedback arcs are
    /// flagged in-place via [`FlowEdge::feedback`].
    pub edges: Vec<FlowEdge>,
    /// The `Signal`-class nets that form the graph's node set.
    pub signal_nets: BTreeSet<String>,
    /// A topological order over `signal_nets` with feedback edges removed —
    /// the forward flow order B3 can feed into longest-path layering. Nets
    /// with no forward edges appear last, in name order.
    pub net_order: Vec<String>,
}

impl SignalFlow {
    /// The set of feedback arcs, as `(from, to)` net pairs. Marked, never
    /// reversed: each pair also appears in [`SignalFlow::edges`] with its
    /// original orientation and `feedback = true`.
    #[must_use]
    pub fn feedback_pairs(&self) -> BTreeSet<(String, String)> {
        self.edges
            .iter()
            .filter(|e| e.feedback)
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect()
    }

    /// The `checked.elements` indices whose edge is a marked feedback arc.
    #[must_use]
    pub fn feedback_elements(&self) -> BTreeSet<usize> {
        self.edges
            .iter()
            .filter(|e| e.feedback)
            .map(|e| e.element)
            .collect()
    }
}

/// Compute the directed signal-flow graph for `checked`.
///
/// `classes` must be the [`crate::net_class::classify_nets`] result for the
/// same netlist. Rails / ground never enter the graph.
#[must_use]
#[allow(clippy::too_many_lines)] // device-edge extraction + orientation are one phase.
pub fn signal_flow(checked: &CheckedNetlist, classes: &NetClassMap) -> SignalFlow {
    let is_signal = |net: &str| -> bool {
        classes.get(net).copied().unwrap_or(NetClass::Signal) == NetClass::Signal
    };

    // --- Gather device edges, passive pairs, and driver/sink roles ---------
    let mut device_edges: Vec<(usize, String, String)> = Vec::new();
    let mut passive_pairs: Vec<(usize, String, String)> = Vec::new();
    // A net is a `driver` if some device drives current onto it (collector,
    // emitter, drain, source, cathode, source-`+`, VCVS output); a `sink` if
    // it is a device control input (base, gate, VCVS control). Used to orient
    // passives that bridge the two.
    let mut drivers: BTreeSet<String> = BTreeSet::new();
    let mut sinks: BTreeSet<String> = BTreeSet::new();
    let mut signal_nets: BTreeSet<String> = BTreeSet::new();

    for (idx, el) in checked.elements.iter().enumerate() {
        for net in &el.nodes {
            if is_signal(net) {
                signal_nets.insert(net.clone());
            }
        }
        let n = &el.nodes;
        // Only add an edge when both endpoints are signal nets.
        let dev = |a: &str, b: &str, edges: &mut Vec<(usize, String, String)>| {
            if a != b && is_signal(a) && is_signal(b) {
                edges.push((idx, a.to_string(), b.to_string()));
            }
        };
        let mark = |set: &mut BTreeSet<String>, net: &str| {
            if is_signal(net) {
                set.insert(net.to_string());
            }
        };

        match el.kind {
            ElementKind::Bjt => {
                // c b e [sub]
                if let (Some(c), Some(b), Some(e)) = (n.first(), n.get(1), n.get(2)) {
                    dev(b, c, &mut device_edges); // base controls collector
                    dev(c, e, &mut device_edges); // collector→emitter current path
                    mark(&mut sinks, b);
                    mark(&mut drivers, c);
                    mark(&mut drivers, e);
                }
            }
            ElementKind::Mosfet | ElementKind::Jfet => {
                // d g s [b]
                if let (Some(d), Some(g), Some(s)) = (n.first(), n.get(1), n.get(2)) {
                    dev(g, d, &mut device_edges); // gate controls drain
                    dev(d, s, &mut device_edges); // drain→source current path
                    mark(&mut sinks, g);
                    mark(&mut drivers, d);
                    mark(&mut drivers, s);
                }
            }
            ElementKind::Diode => {
                // a c
                if let (Some(a), Some(c)) = (n.first(), n.get(1)) {
                    dev(a, c, &mut device_edges); // anode→cathode
                    mark(&mut drivers, c);
                }
            }
            ElementKind::Vcvs | ElementKind::Vccs => {
                // out+ out- ctrl+ ctrl-
                if let (Some(op), Some(cp)) = (n.first(), n.get(2)) {
                    dev(cp, op, &mut device_edges); // control drives output
                    mark(&mut drivers, op);
                    mark(&mut sinks, cp);
                    if let Some(cn) = n.get(3) {
                        mark(&mut sinks, cn);
                    }
                }
            }
            ElementKind::VoltageSrc | ElementKind::CurrentSrc => {
                // A `*@power`-tagged source is a supply datum, out of the
                // signal graph; only untagged sources drive signal.
                if !matches!(el.role, ElementRole::Power(_))
                    && let (Some(p), Some(m)) = (n.first(), n.get(1))
                {
                    dev(p, m, &mut device_edges); // + → -
                    mark(&mut drivers, p);
                }
            }
            ElementKind::Resistor | ElementKind::Capacitor | ElementKind::Inductor => {
                if let (Some(a), Some(b)) = (n.first(), n.get(1))
                    && a != b
                    && is_signal(a)
                    && is_signal(b)
                {
                    passive_pairs.push((idx, a.clone(), b.clone()));
                }
            }
            // Subckt instances upgraded to a real symbol, controlled sources
            // keyed on a Vname, mutual inductance, and anything else carry no
            // intrinsic DC-path direction here — their nets still participate
            // through whatever passives touch them.
            _ => {}
        }
    }

    // --- Seed nets: declared input ports, signal-source outputs, name hints -
    let mut seeds: BTreeSet<String> = BTreeSet::new();
    for port in &checked.ports {
        if port.dir == PortDir::Input && is_signal(&port.net) {
            seeds.insert(port.net.clone());
        }
    }
    for el in &checked.elements {
        if matches!(el.kind, ElementKind::VoltageSrc | ElementKind::CurrentSrc)
            && !matches!(el.role, ElementRole::Power(_))
            && let Some(p) = el.nodes.first()
            && is_signal(p)
        {
            seeds.insert(p.clone());
        }
    }
    // Name heuristic on leaf signal nets, so a zero-annotation file still gets
    // a rooted left→right flow (mirrors `layers::boundary_net_role`).
    let mut net_degree: BTreeMap<&str, usize> = BTreeMap::new();
    for el in &checked.elements {
        for net in &el.nodes {
            if is_signal(net) {
                *net_degree.entry(net.as_str()).or_default() += 1;
            }
        }
    }
    for (net, deg) in &net_degree {
        if *deg == 1 && crate::layers::boundary_net_role(net) == Some(PortDir::Input) {
            seeds.insert((*net).to_string());
        }
    }

    // --- BFS rank over the undirected signal adjacency, for passive fallback -
    let mut undirected: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut add_und = |a: &str, b: &str| {
        undirected
            .entry(a.to_string())
            .or_default()
            .insert(b.to_string());
        undirected
            .entry(b.to_string())
            .or_default()
            .insert(a.to_string());
    };
    for (_, a, b) in &device_edges {
        add_und(a, b);
    }
    for (_, a, b) in &passive_pairs {
        add_und(a, b);
    }
    let rank = bfs_rank(&signal_nets, &seeds, &undirected);

    // --- Orient passives ---------------------------------------------------
    let mut edges: Vec<FlowEdge> = Vec::new();
    for (idx, a, b) in &device_edges {
        edges.push(FlowEdge {
            element: *idx,
            from: a.clone(),
            to: b.clone(),
            kind: EdgeKind::Device,
            feedback: false,
        });
    }
    for (idx, a, b) in &passive_pairs {
        let a_drv = drivers.contains(a) && !sinks.contains(a);
        let a_snk = sinks.contains(a) && !drivers.contains(a);
        let b_drv = drivers.contains(b) && !sinks.contains(b);
        let b_snk = sinks.contains(b) && !drivers.contains(b);
        let (from, to) = if a_drv && b_snk {
            (a.clone(), b.clone())
        } else if b_drv && a_snk {
            (b.clone(), a.clone())
        } else {
            // Fall back to seed-rooted BFS rank; tie-break by net name so the
            // orientation is fully deterministic.
            let ra = rank.get(a).copied().unwrap_or(u32::MAX);
            let rb = rank.get(b).copied().unwrap_or(u32::MAX);
            if (ra, a) <= (rb, b) {
                (a.clone(), b.clone())
            } else {
                (b.clone(), a.clone())
            }
        };
        edges.push(FlowEdge {
            element: *idx,
            from,
            to,
            kind: EdgeKind::Passive,
            feedback: false,
        });
    }

    // --- Greedy feedback-arc-set: mark backward edges ----------------------
    let seq = greedy_fas_sequence(&signal_nets, &edges);
    for e in &mut edges {
        if e.from == e.to {
            continue;
        }
        if let (Some(&pf), Some(&pt)) = (seq.get(&e.from), seq.get(&e.to))
            && pf > pt
        {
            e.feedback = true;
        }
    }

    edges.sort_by(|x, y| (&x.from, &x.to, x.element).cmp(&(&y.from, &y.to, y.element)));

    let net_order = topo_order(&signal_nets, &edges);

    SignalFlow {
        edges,
        signal_nets,
        net_order,
    }
}

/// Multi-source BFS distance from `seeds` over the undirected signal graph.
/// Unreached nets get `u32::MAX`.
fn bfs_rank(
    signal_nets: &BTreeSet<String>,
    seeds: &BTreeSet<String>,
    undirected: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, u32> {
    let mut rank: BTreeMap<String, u32> = BTreeMap::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    // Seed with declared/source seeds that are actually signal nets; if none
    // exist, seed from the lexicographically-first signal net so the rank is
    // still total and deterministic.
    let effective: BTreeSet<&String> = if seeds.iter().any(|s| signal_nets.contains(s)) {
        seeds.iter().filter(|s| signal_nets.contains(*s)).collect()
    } else {
        signal_nets.iter().take(1).collect()
    };
    for s in effective {
        rank.insert(s.clone(), 0);
        queue.push_back(s.clone());
    }
    while let Some(u) = queue.pop_front() {
        let du = rank[&u];
        if let Some(neigh) = undirected.get(&u) {
            for v in neigh {
                if !rank.contains_key(v) {
                    rank.insert(v.clone(), du + 1);
                    queue.push_back(v.clone());
                }
            }
        }
    }
    rank
}

/// Eades' linear-time greedy feedback-arc-set heuristic, returning a position
/// map (net → sequence index). Edges `u→v` with `pos(u) > pos(v)` are the
/// feedback arcs. Fully deterministic: sinks/sources and the max-`(out-in)`
/// pick all tie-break by net name.
fn greedy_fas_sequence(
    signal_nets: &BTreeSet<String>,
    edges: &[FlowEdge],
) -> BTreeMap<String, usize> {
    // Simple directed edge set (dedup, drop self-loops) over signal nets.
    let mut succ: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut pred: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for net in signal_nets {
        succ.entry(net.as_str()).or_default();
        pred.entry(net.as_str()).or_default();
    }
    for e in edges {
        if e.from == e.to || !signal_nets.contains(&e.from) || !signal_nets.contains(&e.to) {
            continue;
        }
        succ.get_mut(e.from.as_str()).unwrap().insert(e.to.as_str());
        pred.get_mut(e.to.as_str()).unwrap().insert(e.from.as_str());
    }

    let mut remaining: BTreeSet<&str> = signal_nets.iter().map(String::as_str).collect();
    let mut left: Vec<&str> = Vec::new();
    let mut right: VecDeque<&str> = VecDeque::new();

    let out_deg = |u: &str, succ: &BTreeMap<&str, BTreeSet<&str>>, rem: &BTreeSet<&str>| {
        succ[u].iter().filter(|v| rem.contains(*v)).count()
    };
    let in_deg = |u: &str, pred: &BTreeMap<&str, BTreeSet<&str>>, rem: &BTreeSet<&str>| {
        pred[u].iter().filter(|v| rem.contains(*v)).count()
    };

    while !remaining.is_empty() {
        // Drain sinks (out-degree 0) to the right, in name order.
        loop {
            let sink = remaining
                .iter()
                .copied()
                .find(|u| out_deg(u, &succ, &remaining) == 0);
            match sink {
                Some(u) => {
                    right.push_front(u);
                    remaining.remove(u);
                }
                None => break,
            }
            if remaining.is_empty() {
                break;
            }
        }
        // Drain sources (in-degree 0) to the left, in name order.
        loop {
            let source = remaining
                .iter()
                .copied()
                .find(|u| in_deg(u, &pred, &remaining) == 0);
            match source {
                Some(u) => {
                    left.push(u);
                    remaining.remove(u);
                }
                None => break,
            }
            if remaining.is_empty() {
                break;
            }
        }
        if remaining.is_empty() {
            break;
        }
        // Pick the net maximising out-degree − in-degree; tie-break by name
        // (BTreeSet iterates in sorted order, so the first max wins).
        let pick = remaining
            .iter()
            .copied()
            .max_by_key(|u| {
                let out = i64::try_from(out_deg(u, &succ, &remaining)).unwrap_or(i64::MAX);
                let inn = i64::try_from(in_deg(u, &pred, &remaining)).unwrap_or(i64::MAX);
                // Tie-break equal `out − in` by *smallest* net name via a
                // reversed secondary key (BTreeSet already iterates sorted).
                (out - inn, std::cmp::Reverse(*u))
            })
            .expect("remaining non-empty");
        left.push(pick);
        remaining.remove(pick);
    }

    let mut pos: BTreeMap<String, usize> = BTreeMap::new();
    let mut i = 0usize;
    for u in left {
        pos.insert(u.to_string(), i);
        i += 1;
    }
    for u in right {
        pos.insert(u.to_string(), i);
        i += 1;
    }
    pos
}

/// Kahn topological order over signal nets using only non-feedback forward
/// edges. Ties broken by net name; leftover nets (in a residual cycle, which
/// should not occur after FAS marking) appended in name order.
fn topo_order(signal_nets: &BTreeSet<String>, edges: &[FlowEdge]) -> Vec<String> {
    let mut succ: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut indeg: BTreeMap<&str, usize> = BTreeMap::new();
    for net in signal_nets {
        succ.entry(net.as_str()).or_default();
        indeg.entry(net.as_str()).or_insert(0);
    }
    for e in edges {
        if e.feedback || e.from == e.to {
            continue;
        }
        if !signal_nets.contains(&e.from) || !signal_nets.contains(&e.to) {
            continue;
        }
        if succ.get_mut(e.from.as_str()).unwrap().insert(e.to.as_str()) {
            *indeg.get_mut(e.to.as_str()).unwrap() += 1;
        }
    }
    let mut ready: BTreeSet<&str> = indeg
        .iter()
        .filter(|&(_, &d)| d == 0)
        .map(|(&n, _)| n)
        .collect();
    let mut out: Vec<String> = Vec::new();
    while let Some(&u) = ready.iter().next() {
        ready.remove(u);
        out.push(u.to_string());
        for &v in &succ[u] {
            let d = indeg.get_mut(v).unwrap();
            *d -= 1;
            if *d == 0 {
                ready.insert(v);
            }
        }
    }
    // Any net left (residual cycle) appended in name order.
    for net in signal_nets {
        if !out.iter().any(|o| o == net) {
            out.push(net.clone());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net_class::classify_nets;
    use kicad_symbols::Library;
    use spice_diagnostics::FileId;
    use spice_policy::check;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    fn fixture_library() -> &'static Library {
        static LIB: OnceLock<Library> = OnceLock::new();
        LIB.get_or_init(|| {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let fixture_dir = manifest
                .parent()
                .and_then(std::path::Path::parent)
                .expect("workspace root")
                .join("crates/kicad-symbols/tests/fixtures");
            let mut lib = Library::from_file(fixture_dir.join("Device.kicad_sym"))
                .expect("load Device fixture library");
            for extra in [
                "Simulation_SPICE.kicad_sym",
                "Amplifier_Operational.kicad_sym",
            ] {
                if let Ok(other) = Library::from_file(fixture_dir.join(extra)) {
                    lib = lib.merge(other);
                }
            }
            lib
        })
    }

    fn flow_of(src: &str) -> SignalFlow {
        let file_id = FileId(0);
        let parsed = spice_parser::parse(src, file_id)
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let (checked, _warns) = check(resolved).expect("policy check failed");
        let classes = classify_nets(&checked);
        signal_flow(&checked, &classes)
    }

    /// Does a directed edge `from→to` exist (regardless of feedback flag)?
    fn has_edge(f: &SignalFlow, from: &str, to: &str) -> bool {
        f.edges.iter().any(|e| e.from == from && e.to == to)
    }

    fn before(f: &SignalFlow, a: &str, b: &str) -> bool {
        let pa = f.net_order.iter().position(|n| n == a);
        let pb = f.net_order.iter().position(|n| n == b);
        matches!((pa, pb), (Some(pa), Some(pb)) if pa < pb)
    }

    /// rc_lowpass: V1 drives `in`, R1 bridges in→out, C1 out→gnd. The only
    /// signal edge is the R1 passive, oriented in→out by the seed rank. No
    /// feedback.
    #[test]
    fn rc_lowpass_simple_chain() {
        let f = flow_of("test\nV1 in 0 AC 1\nR1 in out 1k\nC1 out 0 1u\n.end\n");
        assert!(
            has_edge(&f, "in", "out"),
            "R1 should orient in→out: {:?}",
            f.edges
        );
        assert!(before(&f, "in", "out"), "flow order must be in before out");
        assert!(
            f.feedback_pairs().is_empty(),
            "no feedback expected, got {:?}",
            f.feedback_pairs()
        );
    }

    /// common_emitter skeleton: input at the base, collector→emitter current
    /// path, output taken off the collector. Signal flows in→b→c→{e,out}, no
    /// feedback. `vcc`/`0` are out of the graph.
    #[test]
    fn common_emitter_flows_forward_no_feedback() {
        let src = "test\n\
                   VCC vcc 0 DC 12 ;@ power=+12V\n\
                   R1 vcc b 47k\n\
                   R2 b 0 10k\n\
                   RC vcc c 3k3\n\
                   RE e 0 1k\n\
                   CE e 0 100u\n\
                   CIN in b 1u\n\
                   COUT c out 1u\n\
                   Q1 c b e QGENERIC\n\
                   .model QGENERIC NPN (BF=200 IS=1e-15)\n.end\n";
        let f = flow_of(src);
        // Device edges: base controls collector, collector→emitter.
        assert!(
            has_edge(&f, "b", "c"),
            "base→collector missing: {:?}",
            f.edges
        );
        assert!(
            has_edge(&f, "c", "e"),
            "collector→emitter missing: {:?}",
            f.edges
        );
        // Input coupling and output coupling oriented along the flow.
        assert!(
            has_edge(&f, "in", "b"),
            "CIN should orient in→b: {:?}",
            f.edges
        );
        assert!(
            has_edge(&f, "c", "out"),
            "COUT should orient c→out: {:?}",
            f.edges
        );
        // Whole chain is acyclic.
        assert!(
            f.feedback_pairs().is_empty(),
            "common_emitter has no feedback, got {:?}",
            f.feedback_pairs()
        );
        assert!(before(&f, "in", "b"));
        assert!(before(&f, "b", "c"));
        assert!(before(&f, "c", "out"));
    }

    /// Inverting op-amp (VCVS model, the same `E`-source these fixtures use):
    /// the amp drives `inv→out`, and the feedback resistor RF bridges the
    /// output back to the summing junction. RF is a passive from a device
    /// OUTPUT net (`out`) to a device INPUT net (`inv`), so it orients
    /// out→inv — closing the loop inv→out→inv. Greedy-FAS must MARK exactly
    /// one arc as feedback, and must NOT reverse anything: both directed edges
    /// remain present.
    #[test]
    fn inverting_opamp_feedback_is_marked_not_reversed() {
        // in→inv via RIN; inv→out via the VCVS (control=inv, output=out);
        // out→inv via RF (feedback). Vin seeds `in`.
        let src = "test\n\
                   Vin in 0 AC 1\n\
                   RIN in inv 1k\n\
                   RF inv out 10k\n\
                   E1 out 0 inv 0 100000\n.end\n";
        let f = flow_of(src);
        // The forward device edge and the feedback passive both exist.
        assert!(
            has_edge(&f, "inv", "out"),
            "VCVS should drive inv→out: {:?}",
            f.edges
        );
        assert!(
            has_edge(&f, "out", "inv"),
            "RF should orient out→inv (output→input): {:?}",
            f.edges
        );
        // Exactly one of the two is MARKED feedback; NOTHING is reversed
        // (both directions still present above).
        let fb = f.feedback_pairs();
        assert_eq!(fb.len(), 1, "exactly one feedback arc expected, got {fb:?}");
        let marked = fb.iter().next().unwrap();
        assert!(
            *marked == ("inv".to_string(), "out".to_string())
                || *marked == ("out".to_string(), "inv".to_string()),
            "feedback arc must be one leg of the inv↔out loop, got {marked:?}"
        );
        // The marked edge is still in the graph in its original direction.
        assert!(
            has_edge(&f, &marked.0, &marked.1),
            "marked feedback edge must remain (not reversed)"
        );
        // Feedback is attributed to a real element index.
        assert!(!f.feedback_elements().is_empty());
    }

    /// Astable multivibrator: cross-coupled BJTs form a genuine directed cycle
    /// (b1→c1→b2→c2→b1 through the coupling caps). The pass must terminate and
    /// mark feedback arcs rather than looping or reversing.
    #[test]
    fn multivibrator_cycle_is_marked() {
        let src = "test\n\
                   VCC vcc 0 12 ;@ power=vcc\n\
                   Q1 c1 b1 0 QGENERIC\n\
                   Q2 c2 b2 0 QGENERIC\n\
                   R1 vcc c1 1k\nR2 vcc c2 1k\n\
                   R3 vcc b1 10k\nR4 vcc b2 10k\n\
                   C1 c1 b2 1n\nC2 c2 b1 1n\n\
                   .model QGENERIC NPN (BF=200 IS=1e-15)\n.end\n";
        let f = flow_of(src);
        // At least one arc marked (the cycle cannot be fully forward).
        assert!(
            !f.feedback_pairs().is_empty(),
            "cross-coupled cycle must yield ≥1 feedback arc"
        );
        // Nothing exploded and the order covers every signal net.
        assert_eq!(f.net_order.len(), f.signal_nets.len());
        for net in ["c1", "c2", "b1", "b2"] {
            assert!(f.signal_nets.contains(net), "{net} should be a signal net");
        }
    }
}
