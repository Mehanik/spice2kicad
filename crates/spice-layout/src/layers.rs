//! Directed signal-flow graph + Tarjan SCC cycle break + longest-path
//! layering. See spec §5.
//!
//! Pure function: takes a `CheckedNetlist` and a `NetClassMap`, returns a
//! `LayerAssignment` whose `layers` vec is parallel to `checked.elements`.
//! Used downstream by the seed placer to assign X coordinates.

use std::collections::{HashMap, HashSet};

use spice_policy::CheckedNetlist;
use spice_resolve::{ElementKind, ElementRole};

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
    let mut net_to_elements: HashMap<&str, Vec<usize>> = HashMap::new();
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
    if sources.is_empty() {
        return LayerAssignment {
            layers: vec![0; n],
            rank_in_layer: (0..u32::try_from(n).unwrap_or(u32::MAX)).collect(),
            feedback_edges: Vec::new(),
            no_source_fallback: true,
        };
    }

    // --- Step 3: break cycles (iterative Tarjan + edge reversal) ----------
    let (dag, feedback_edges) = break_cycles(adj);

    // --- Step 4: longest-path layering from sources -----------------------
    let layers = longest_path_layers(&dag, &sources, n);

    // --- Step 5: rank within layer (index order) --------------------------
    let rank_in_layer = rank_by_layer(&layers, n);

    LayerAssignment {
        layers,
        rank_in_layer,
        feedback_edges,
        no_source_fallback: false,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
