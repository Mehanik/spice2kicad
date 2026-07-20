//! Directed signal-flow graph + Tarjan SCC cycle break + longest-path
//! layering. See spec §5.
//!
//! Pure function: takes a `CheckedNetlist` and a `NetClassMap`, returns a
//! `LayerAssignment` whose `layers` vec is parallel to `checked.elements`.
//! Used downstream by the seed placer to assign X coordinates.

use std::collections::{BTreeMap, HashMap, HashSet};

use spice_policy::CheckedNetlist;
use spice_resolve::{ElementKind, ElementRole, PortDir};

use crate::net_class::{NetClass, NetClassMap};

/// Result of X-layer assignment for the full netlist.
#[derive(Debug, Clone)]
pub struct LayerAssignment {
    /// Layer index per element (parallel to `checked.elements`).
    /// Layer 0 = leftmost (signal sources). Higher = further right.
    pub layers: Vec<u32>,
    /// Rank within each layer, used to compute initial Y stacking.
    /// Elements in the same layer are stacked vertically in this order.
    pub rank_in_layer: Vec<u32>,
    /// Edges that were reversed during cycle break (src, dst) by element index.
    pub feedback_edges: Vec<(usize, usize)>,
    /// `true` when the graph has no signal sources and we fell back to
    /// "all at layer 0" (e.g. a pure multivibrator with only power
    /// sources). Caller may choose a column-major fallback layout.
    pub no_source_fallback: bool,
}

/// Assign X layers to every element in `checked`.
///
/// Algorithm:
/// 1. Build a directed adjacency list using only Signal nets.
///    Signal sources (`VoltageSrc`/`CurrentSrc` not tagged `Power`) drive
///    edges outward; all other elements get fully-connected undirected
///    edges on their Signal nets (direction resolved by Tarjan + topo).
/// 2. If no signal sources exist, return `no_source_fallback = true`.
/// 3. Run iterative Tarjan SCC + edge reversal to break cycles.
/// 4. Longest-path layering (topological sort, sources at layer 0).
/// 5. Barycentric Y rank within each layer (element index order for v0.1).
pub fn assign_x_layers(checked: &CheckedNetlist, classes: &NetClassMap) -> LayerAssignment {
    let n = checked.elements.len();

    // --- Step 1: build adjacency via Signal nets ---------------------------
    // net_to_elements[net] = list of element indices on that net
    let mut net_to_elements: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (idx, el) in checked.elements.iter().enumerate() {
        for net in &el.nodes {
            if classes
                .get(net.as_str())
                .copied()
                .unwrap_or(NetClass::Signal)
                == NetClass::Signal
            {
                net_to_elements.entry(net.as_str()).or_default().push(idx);
            }
        }
    }

    // Identify signal sources.
    let sources: HashSet<usize> = (0..n).filter(|&i| is_signal_source(checked, i)).collect();

    // Build directed adjacency: source → others on shared net;
    // non-source: add edges to all other net members (undirected).
    // Duplicate edges are harmless; they get deduplicated via HashSet
    // during cycle-break or are absorbed by topo sort.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for members in net_to_elements.values() {
        for &u in members {
            for &v in members {
                if u != v {
                    if sources.contains(&u) {
                        // Source drives outward.
                        adj[u].push(v);
                    } else {
                        // Non-source: undirected (add both directions;
                        // Tarjan + longest-path handle the rest).
                        adj[u].push(v);
                    }
                }
            }
        }
    }

    // --- Step 2: no-source fallback ----------------------------------------
    // When the netlist has no signal source (e.g. only `;@ power`-
    // tagged voltage sources or none at all), fall back to a
    // BFS-based layering rooted at every element that touches a
    // Power-class net. These are the natural left-edge elements:
    // bias resistors, collector resistors, AC-coupling inputs.
    // The result is layer 0 = power-touching elements, layer 1+ =
    // their signal-net neighbours, and so on. We still set
    // `no_source_fallback = true` so callers can identify the
    // fallback path; the layer term in cost is computed from the
    // resulting structure normally.
    if sources.is_empty() {
        return no_source_fallback(checked, classes, &net_to_elements, n);
    }
    // Step 3-5: directed DAG, longest-path layering.
    let (dag, feedback_edges) = break_cycles(adj);
    let layers = longest_path_layers(&dag, &sources, n);
    let rank_in_layer = rank_by_layer(&layers, n);
    LayerAssignment {
        layers,
        rank_in_layer,
        feedback_edges,
        no_source_fallback: false,
    }
}

#[allow(clippy::too_many_lines)] // BFS + leaf-net heuristic are conceptually one phase.
fn no_source_fallback(
    checked: &CheckedNetlist,
    classes: &NetClassMap,
    net_to_elements: &BTreeMap<&str, Vec<usize>>,
    n: usize,
) -> LayerAssignment {
    // Identify "leaf signal nets" — Signal-class nets touched by
    // exactly one element in the netlist. These are external
    // boundary points of the signal chain and their connecting
    // element should sit on the left/right edge of the layout
    // depending on the net name.
    //
    // Convention (see `boundary_net_role`):
    //   * `in` / `input` / `vin` → leftmost (layer 0 root)
    //   * `out` / `output` / `vout` → rightmost (terminal sink)
    //   * any other leaf net → no special handling
    let mut leaf_input_elements: HashSet<usize> = HashSet::new();
    let mut leaf_output_elements: HashSet<usize> = HashSet::new();
    for (net, members) in net_to_elements {
        if members.len() != 1 {
            continue;
        }
        let owner = members[0];
        match boundary_net_role(net) {
            Some(PortDir::Input) => {
                leaf_input_elements.insert(owner);
            }
            Some(PortDir::Output) => {
                leaf_output_elements.insert(owner);
            }
            _ => {}
        }
    }

    // Declared `*@port <net>=<dir>` directives reinforce the same
    // left/right bias by POSITION only (never orientation): an `Input`
    // port seeds every element on its net toward the left (root layer),
    // an `Output` port toward the right (terminal sink). `Bidir` gets no
    // bias. Additive to the name-based sets above — for a fixture with no
    // `*@port` this loop is empty, so placement is byte-identical.
    for port in &checked.ports {
        let Some(members) = net_to_elements.get(port.net.as_str()) else {
            continue;
        };
        match port.dir {
            PortDir::Input => leaf_input_elements.extend(members.iter().copied()),
            PortDir::Output => leaf_output_elements.extend(members.iter().copied()),
            PortDir::Bidir => {}
        }
    }

    // A power-touching element is a natural left-edge element only when the
    // rail IS its connection — a bias resistor, a collector resistor, a
    // decoupling cap. Such an element touches at most ONE Signal net: it is a
    // *rail stub*, a boundary of the signal graph.
    //
    // An element touching TWO OR MORE Signal nets is an interior node of the
    // signal path that merely happens to be supplied from a rail — an opamp,
    // a buffer, any powered active block. Rooting it at layer 0 places it
    // level with the circuit's true input, so the signal runs backwards into
    // it. Its layer must come from the BFS like any other interior element.
    let signal_degree = |i: usize| -> usize {
        checked.elements[i]
            .nodes
            .iter()
            .filter(|net| {
                classes
                    .get(net.as_str())
                    .copied()
                    .unwrap_or(NetClass::Signal)
                    == NetClass::Signal
            })
            .collect::<HashSet<_>>()
            .len()
    };
    let touches_power = |i: usize| -> bool {
        checked.elements[i]
            .nodes
            .iter()
            .any(|net| matches!(classes.get(net.as_str()).copied(), Some(NetClass::Power)))
    };
    // The same "boundary, not interior" test applies to an element that owns
    // an input net. The input net anchors the LEFT EDGE of the signal path;
    // the element that owns it is a left-edge element only if the signal
    // merely *passes through* it — a series input resistor, an AC-coupling
    // cap, anything with at most two Signal nets. An element with THREE or
    // more Signal nets is a junction or an active block that the input net
    // feeds *into*: a diff-pair transistor whose base is `in1` also carries
    // its collector and tail nodes, and rooting it at layer 0 collapses it
    // onto the same layer as its own collector load.
    //
    // Note the two thresholds differ, and deliberately: a *rail*-touching
    // element must be a true stub (degree ≤ 1, it terminates a node), while
    // an *input*-owning element may be a two-port pass-through (degree ≤ 2).
    // A rail stub does not pass a signal along; a series input element does.
    let input_root =
        |i: usize| -> bool { leaf_input_elements.contains(&i) && signal_degree(i) <= 2 };
    let coarse_roots: HashSet<usize> = (0..n)
        .filter(|&i| input_root(i) || touches_power(i))
        .collect();
    let refined_roots: HashSet<usize> = coarse_roots
        .iter()
        .copied()
        .filter(|&i| input_root(i) || signal_degree(i) <= 1)
        .collect();
    // Well-formedness guard: a root set that touches NO Signal net cannot
    // layer the signal graph at all — the BFS would reach nothing and every
    // element would collapse onto layer 0.
    //
    // The guard must NOT revert to `coarse_roots`. `coarse_roots` is the
    // *unrefined* set, and the whole point of the `signal_degree <= 1`
    // refinement is that a rail-supplied interior node (an opamp, a
    // buffer — anything with two or more Signal nets) is not a left-edge
    // element: rooting it at layer 0 puts it level with the circuit's
    // true input and the signal runs backwards into it. Reverting hands
    // back exactly the root set the refinement was written to reject,
    // through a side door — reached whenever no input anchor exists and
    // every power-toucher is an interior node.
    //
    // Instead, relax the degree threshold by the *smallest* amount that
    // makes the set span the signal graph: take the power-touching
    // elements of minimum signal degree. That is monotone (it never
    // admits a higher-degree element while a lower-degree one is
    // available), it always spans when `coarse_roots` did, and on a
    // netlist where every power-toucher really is degree-3 it degrades
    // to the coarse behaviour rather than to a collapse.
    let roots = if refined_roots.iter().any(|&i| signal_degree(i) >= 1) {
        refined_roots
    } else {
        let min_degree = coarse_roots
            .iter()
            .map(|&i| signal_degree(i))
            .filter(|&d| d >= 1)
            .min();
        match min_degree {
            Some(d) => coarse_roots
                .iter()
                .copied()
                .filter(|&i| signal_degree(i) == d)
                .collect(),
            // No power-touching element reaches the signal graph at all;
            // there is nothing to relax toward. Keep the refined set.
            None => refined_roots,
        }
    };
    if roots.is_empty() {
        return LayerAssignment {
            layers: vec![0; n],
            rank_in_layer: (0..u32::try_from(n).unwrap_or(u32::MAX)).collect(),
            feedback_edges: Vec::new(),
            no_source_fallback: true,
        };
    }
    // Build undirected adjacency on Signal nets.
    let mut sig_adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for members in net_to_elements.values() {
        for &u in members {
            for &v in members {
                if u != v {
                    sig_adj[u].push(v);
                }
            }
        }
    }
    // BFS from all roots simultaneously.
    let mut layers_bfs = vec![u32::MAX; n];
    let mut frontier: Vec<usize> = Vec::new();
    for &r in &roots {
        layers_bfs[r] = 0;
        frontier.push(r);
    }
    let mut depth = 0_u32;
    while !frontier.is_empty() {
        let mut next: Vec<usize> = Vec::new();
        for u in &frontier {
            for &v in &sig_adj[*u] {
                if layers_bfs[v] == u32::MAX {
                    layers_bfs[v] = depth + 1;
                    next.push(v);
                }
            }
        }
        frontier = next;
        depth += 1;
    }
    // Unreachable elements (no signal path to any root) → put
    // at layer 0 so they don't tilt the X axis.
    for layer in &mut layers_bfs {
        if *layer == u32::MAX {
            *layer = 0;
        }
    }
    // Push leaf-output elements one layer past their current
    // assignment, so they sit to the right of the rest of their
    // signal chain. (CE: COUT touches `c` and `out`. `out` is
    // a leaf, so COUT shifts past Q1 / RC.)
    let max_layer = layers_bfs.iter().copied().max().unwrap_or(0);
    for &i in &leaf_output_elements {
        if i < n {
            layers_bfs[i] = layers_bfs[i].max(max_layer) + 1;
        }
    }
    let rank_in_layer = rank_by_layer(&layers_bfs, n);
    LayerAssignment {
        layers: layers_bfs,
        rank_in_layer,
        feedback_edges: Vec::new(),
        no_source_fallback: true,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Classify a *leaf* net name as a circuit input / output boundary.
///
/// This is a **name heuristic and a backstop only** — the explicit,
/// preferred mechanism is a `*@port <net>=<dir>` directive (spec §4.7),
/// which is applied additively by the caller and always wins by being a
/// superset. The heuristic exists so a zero-annotation file still gets a
/// left-to-right signal flow (design principle 2).
///
/// **Channel numbering is stripped before matching.** A multi-channel
/// circuit — a dual opamp, a quad comparator, a stereo stage — *must*
/// number its ports (`in1`, `in2`, `out1`, `out2`), so a matcher that
/// only accepts the bare word silently excludes the entire class of
/// circuits with more than one channel and draws every one of them
/// backwards. Trailing ASCII digits and one optional `_`/`-`/`.`
/// separator are therefore removed first.
///
/// Matching is then **exact against a closed set** in both directions.
/// The previous implementation compared `in`/`out` by equality but
/// `vin`/`vout` by prefix, an accidental asymmetry. Prefix matching is
/// the wrong generalisation regardless: `in_amp`, `input_stage` and
/// `inverting` are ordinary interior nets, not circuit boundaries, and a
/// prefix rule claims all three.
fn boundary_net_role(net: &str) -> Option<PortDir> {
    let lo = net.to_ascii_lowercase();
    let stem = lo.trim_end_matches(|c: char| c.is_ascii_digit());
    // Only strip the separator when digits actually preceded it, so a
    // plain `in_` (no channel number) is not silently accepted.
    let stem = if stem.len() < lo.len() {
        stem.trim_end_matches(['_', '-', '.'])
    } else {
        stem
    };
    match stem {
        "in" | "input" | "vin" => Some(PortDir::Input),
        "out" | "output" | "vout" => Some(PortDir::Output),
        _ => None,
    }
}

fn is_signal_source(checked: &CheckedNetlist, idx: usize) -> bool {
    let el = &checked.elements[idx];
    matches!(el.kind, ElementKind::VoltageSrc | ElementKind::CurrentSrc)
        && !matches!(el.role, ElementRole::Power(_))
}

/// Iteratively detect and break cycles using Tarjan SCC.
///
/// Each iteration: find all non-trivial SCCs; for each, pick the
/// internal edge whose *source* has the highest in-degree within the
/// SCC (heuristic: the most-depended-upon node is the one that
/// represents a feedback path back toward an earlier stage), reverse
/// it, and repeat. Loop terminates because each reversal strictly
/// reduces the number of edges in the original direction within the
/// SCC.
fn break_cycles(mut adj: Vec<Vec<usize>>) -> (Vec<Vec<usize>>, Vec<(usize, usize)>) {
    let mut reversed: Vec<(usize, usize)> = Vec::new();
    loop {
        let sccs = tarjan_sccs(&adj);
        let mut found_nontrivial = false;
        for scc in &sccs {
            if scc.len() < 2 {
                continue;
            }
            found_nontrivial = true;
            let scc_set: HashSet<usize> = scc.iter().copied().collect();

            // Pick the edge (u → v) entirely within the SCC whose
            // source `u` has the highest in-degree within the SCC.
            let mut best: Option<(usize, usize, usize)> = None; // (u, v, score)
            for &u in scc {
                let in_deg = adj
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| scc_set.contains(i))
                    .filter(|(_, succs)| succs.contains(&u))
                    .count();
                for &v in &adj[u] {
                    if scc_set.contains(&v) && best.is_none_or(|(_, _, s)| in_deg > s) {
                        best = Some((u, v, in_deg));
                    }
                }
            }

            if let Some((u, v, _)) = best {
                // Remove u→v; add v→u.
                adj[u].retain(|&x| x != v);
                adj[v].push(u);
                reversed.push((u, v));
            }
            // Re-run Tarjan after each reversal so we always work on
            // a fresh SCC decomposition.
            break;
        }
        if !found_nontrivial {
            break;
        }
    }
    (adj, reversed)
}

/// Iterative Tarjan SCC to avoid stack overflow on deep graphs.
///
/// Returns a list of SCCs; each SCC is a `Vec<usize>` of element indices.
fn tarjan_sccs(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = adj.len();
    let mut index_counter = 0_usize;
    let mut stack: Vec<usize> = Vec::new();
    let mut on_stack = vec![false; n];
    let mut indices: Vec<Option<usize>> = vec![None; n];
    let mut lowlink = vec![0_usize; n];
    let mut sccs: Vec<Vec<usize>> = Vec::new();

    // Explicit DFS stack frame to avoid recursion.
    // Frame: (node, iterator-position-in-adj[node], index-assigned)
    let mut call_stack: Vec<(usize, usize)> = Vec::new();

    for start in 0..n {
        if indices[start].is_some() {
            continue;
        }

        call_stack.push((start, 0));
        indices[start] = Some(index_counter);
        lowlink[start] = index_counter;
        index_counter += 1;
        stack.push(start);
        on_stack[start] = true;

        'outer: while let Some((v, next_child)) = call_stack.last_mut() {
            let v = *v;
            // Look for the next unprocessed neighbour.
            while *next_child < adj[v].len() {
                let w = adj[v][*next_child];
                *next_child += 1;
                if indices[w].is_none() {
                    // Tree edge: push w.
                    indices[w] = Some(index_counter);
                    lowlink[w] = index_counter;
                    index_counter += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    call_stack.push((w, 0));
                    continue 'outer;
                } else if on_stack[w] {
                    // Back edge.
                    lowlink[v] = lowlink[v].min(indices[w].unwrap());
                }
            }

            // All neighbours of v processed — pop.
            call_stack.pop();
            if let Some(&(parent, _)) = call_stack.last() {
                lowlink[parent] = lowlink[parent].min(lowlink[v]);
            }

            // Check if v is the root of an SCC.
            if lowlink[v] == indices[v].unwrap() {
                let mut scc = Vec::new();
                loop {
                    let w = stack.pop().unwrap();
                    on_stack[w] = false;
                    scc.push(w);
                    if w == v {
                        break;
                    }
                }
                sccs.push(scc);
            }
        }
    }
    sccs
}

/// Longest-path layering: layer(v) = 1 + max over all predecessors.
/// Signal sources are anchored at layer 0. Nodes with no predecessors
/// that aren't sources also start at 0.
fn longest_path_layers(dag: &[Vec<usize>], sources: &HashSet<usize>, n: usize) -> Vec<u32> {
    // Topological sort via Kahn's algorithm.
    let order = topo_order(dag, n);
    let mut layers = vec![0_u32; n];
    // Build reverse adjacency (predecessors) for efficient lookup.
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (u, succs) in dag.iter().enumerate() {
        for &v in succs {
            preds[v].push(u);
        }
    }
    for v in order {
        if sources.contains(&v) {
            layers[v] = 0;
        } else {
            let max_pred = preds[v].iter().map(|&u| layers[u]).max();
            layers[v] = max_pred.map_or(0, |m| m + 1);
        }
    }
    layers
}

/// Kahn topological order. On a true DAG this visits every node once.
fn topo_order(dag: &[Vec<usize>], n: usize) -> Vec<usize> {
    let mut indeg = vec![0_usize; n];
    for succs in dag {
        for &v in succs {
            indeg[v] += 1;
        }
    }
    let mut queue: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut out = Vec::with_capacity(n);
    while let Some(u) = queue.pop() {
        out.push(u);
        for &v in &dag[u] {
            indeg[v] -= 1;
            if indeg[v] == 0 {
                queue.push(v);
            }
        }
    }
    // Any node not visited (should not happen after cycle break) gets
    // appended at the end so the layer assignment is always complete.
    if out.len() < n {
        for i in 0..n {
            if !out.contains(&i) {
                out.push(i);
            }
        }
    }
    out
}

/// Rank elements within each layer by their index order (v0.1 baseline;
/// barycentric refinement is a v0.2 polish).
fn rank_by_layer(layers: &[u32], n: usize) -> Vec<u32> {
    let mut counts: HashMap<u32, u32> = HashMap::new();
    let mut ranks = vec![0_u32; n];
    for (i, &layer) in layers.iter().enumerate() {
        let r = counts.entry(layer).or_insert(0);
        ranks[i] = *r;
        *r += 1;
    }
    ranks
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
            let device = Library::from_file(fixture_dir.join("Device.kicad_sym"))
                .expect("load Device fixture library");
            let spice = Library::from_file(fixture_dir.join("Simulation_SPICE.kicad_sym"))
                .expect("load Simulation_SPICE fixture library");
            device.merge(spice)
        })
    }

    /// Parse, resolve, check, classify nets, then assign X layers.
    /// Returns a map from refdes to layer index.
    fn layer_str(src: &str) -> HashMap<String, u32> {
        let file_id = FileId(0);
        let parsed = spice_parser::parse(src, file_id)
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let (checked, _warns) = check(resolved).expect("policy check failed");
        let classes = classify_nets(&checked);
        let asg = assign_x_layers(&checked, &classes);
        checked
            .elements
            .iter()
            .enumerate()
            .map(|(i, e)| (e.refdes.clone(), asg.layers[i]))
            .collect()
    }

    /// RC low-pass: V1 drives `in`, R1 bridges `in`→`mid`, C1 bridges
    /// `mid`→`0`. Signal flows V1 → R1 → C1. Invariant: strict ordering.
    #[test]
    fn rc_lowpass_layers_strict_left_to_right() {
        let m = layer_str("test\nV1 in 0 AC 1\nR1 in mid 1k\nC1 mid 0 1u\n.end\n");
        assert!(
            m["V1"] < m["R1"],
            "V1 (layer {}) should be left of R1 (layer {})",
            m["V1"],
            m["R1"]
        );
        assert!(
            m["R1"] <= m["C1"],
            "R1 (layer {}) should be ≤ C1 (layer {})",
            m["R1"],
            m["C1"]
        );
    }

    /// `boundary_net_role` must treat numbered channel ports exactly like
    /// their unnumbered singular form. A multi-channel circuit MUST number
    /// its ports, so a matcher that only accepts the bare word silently
    /// excludes every dual / quad / stereo design — the defect this
    /// function was extracted to fix.
    #[test]
    fn channel_numbered_ports_are_boundary_nets() {
        for n in ["in", "in1", "in2", "IN3", "input", "input2", "vin", "vin1"] {
            assert_eq!(
                boundary_net_role(n),
                Some(PortDir::Input),
                "{n} should read as a circuit input"
            );
        }
        for n in ["out", "out1", "out2", "OUT12", "output", "vout", "vout2"] {
            assert_eq!(
                boundary_net_role(n),
                Some(PortDir::Output),
                "{n} should read as a circuit output"
            );
        }
    }

    /// The matcher is exact against a closed set once channel digits are
    /// stripped — never a prefix. `in_amp` / `input_stage` / `inverting`
    /// are ordinary interior nets, and a prefix rule claims all three.
    #[test]
    fn interior_nets_are_not_boundary_nets() {
        for n in [
            "in_amp",
            "input_stage",
            "inverting",
            "inv1",
            "inn",
            "inp",
            "outer",
            "vintage",
            "in_",
        ] {
            assert_eq!(
                boundary_net_role(n),
                None,
                "{n} is an interior net, not a circuit boundary"
            );
        }
    }

    /// Two uncoupled channels with numbered ports must each layer
    /// left-to-right: the input resistor strictly left of the block it
    /// feeds. Before the numbered-port fix neither channel had an input
    /// anchor, the root set collapsed to the rails, and the well-formedness
    /// guard reverted to the coarse roots — re-rooting both active blocks
    /// at layer 0 and drawing the whole sheet backwards.
    #[test]
    fn multi_channel_numbered_ports_layer_left_to_right() {
        let src = "dual channel\n\
                   VCC vcc 0 DC 15 ;@ power=+15V\n\
                   R1 in1 mid1 1k\n\
                   R2 in2 mid2 1k\n\
                   Q1 c1 mid1 0 QGENERIC\n\
                   Q2 c2 mid2 0 QGENERIC\n\
                   RC1 vcc c1 4k7\n\
                   RC2 vcc c2 4k7\n\
                   .model QGENERIC NPN (BF=200 IS=1e-15)\n.end\n";
        let m = layer_str(src);
        assert!(
            m["R1"] < m["Q1"],
            "channel 1 runs backwards: R1 layer {} vs Q1 layer {}",
            m["R1"],
            m["Q1"]
        );
        assert!(
            m["R2"] < m["Q2"],
            "channel 2 runs backwards: R2 layer {} vs Q2 layer {}",
            m["R2"],
            m["Q2"]
        );
    }

    /// Multivibrator skeleton: Q1 and Q2 are cross-coupled through C1/C2,
    /// which forms a cycle in the signal graph. Layer assignment must
    /// terminate and produce a finite layer for both transistors.
    #[test]
    fn cycle_is_broken() {
        let src = "test\n\
                   V1 vcc 0 12 ;@ power=vcc\n\
                   Q1 c1 b2 0 QGENERIC\n\
                   Q2 c2 b1 0 QGENERIC\n\
                   R1 vcc c1 1k\nR2 vcc c2 1k\n\
                   R3 vcc b1 10k\nR4 vcc b2 10k\n\
                   C1 c1 b2 1n\nC2 c2 b1 1n\n.end\n";
        let m = layer_str(src);
        assert!(m.contains_key("Q1"), "Q1 must have a layer");
        assert!(m.contains_key("Q2"), "Q2 must have a layer");
        // Both layers must be finite (u32 is always finite; just confirm
        // the test terminates and both keys are present).
        let _ = m["Q1"];
        let _ = m["Q2"];
    }
}
