# Structural Layered Placement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the V6 topology-archetype matcher with a general structural pipeline (net classification → Y-banding → X-layering with cycle-breaking → band-constrained refinement) and prove it on the five existing fixtures.

**Architecture:** Add three new pure modules to `crates/spice-layout/src/` (`net_class.rs`, `bands.rs`, `layers.rs`). Replace the deterministic seed body to consume their outputs. Strengthen `cost.rs` with four new terms (band misalignment, soft Y target, layer order, crossing approximation). Remove `archetype/` and rewire `place_with`. Make `solver::refine` run by default.

**Tech Stack:** Rust 2024, chumsky parser, kicad-symbols crate (existing types `Orientation`, `Symbol`, `Library`), `spice-policy::CheckedNetlist`, `spice-resolve` (`ResolvedElement`, `ElementRole`, `Value`, `Relation`).

---

## File Structure

**New files:**
- `crates/spice-layout/src/net_class.rs` — net classification (Power/Ground/Signal). Pure: `(checked: &CheckedNetlist) -> HashMap<NetId, NetClass>`.
- `crates/spice-layout/src/bands.rs` — element → Y-band assignment. Pure: `(checked, &net_classes) -> Vec<BandAssignment>` indexed by element.
- `crates/spice-layout/src/layers.rs` — directed signal graph + Tarjan cycle break + longest-path layering + barycentric ordering. Pure: `(checked, &net_classes) -> LayerAssignment`.
- `crates/spice-layout/src/seed.rs` — convert (bands, layers) → initial `Placement`. Replaces today's `place_seed` body inline.

**Modified files:**
- `crates/spice-layout/src/lib.rs` — remove `mod archetype`, add new modules, rewire `place_with`, change `place_seed` to consume new modules.
- `crates/spice-layout/src/cost.rs` — extend `CostBreakdown` with four new fields, implement them, update `total` weights.
- `crates/spice-layout/src/solver/anneal.rs` — add "swap same-layer Y rank" move.
- `crates/spice-layout/src/solver/mod.rs` — flip `LayoutOptions::refine` default to `true`.
- `crates/spice2kicad/src/main.rs` (CLI) — `--refine` flag becomes `--refine-iterations N` (or rename, keeping `--no-refine` escape).
- `crates/spice2kicad/tests/placement_quality.rs` — drop V6 archetype tests, add fixture-wide quality tests.
- `CLAUDE.md` — rewrite V6 invariant section, edit V7 cross-reference, add design principle 9.
- `docs/layout-roadmap.md` — one-line update.

**Deleted files:**
- `crates/spice-layout/src/archetype/mod.rs`
- `crates/spice-layout/src/archetype/common_emitter.rs`

---

## Task 1: Net classification module

**Files:**
- Create: `crates/spice-layout/src/net_class.rs`
- Modify: `crates/spice-layout/src/lib.rs` (add `pub mod net_class;`)
- Test: `crates/spice-layout/src/net_class.rs` (unit tests at module bottom)

- [ ] **Step 1: Write failing tests**

```rust
// in crates/spice-layout/src/net_class.rs

#[cfg(test)]
mod tests {
    use super::*;
    use spice_parser::parse;
    use spice_policy::check;

    fn classify_str(src: &str) -> std::collections::HashMap<String, NetClass> {
        let parsed = parse(src).unwrap();
        let resolved = spice_resolve::resolve(parsed).unwrap();
        let checked = check(resolved).unwrap();
        classify_nets(&checked)
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
    }

    #[test]
    fn ground_net_zero_classifies_as_ground() {
        let m = classify_str("R1 a 0 1k\n.end\n");
        assert_eq!(m.get("0"), Some(&NetClass::Ground));
    }

    #[test]
    fn power_tagged_source_positive_terminal_is_power() {
        let m = classify_str("V1 vcc 0 12 ;@ power\nR1 vcc out 1k\n.end\n");
        assert_eq!(m.get("vcc"), Some(&NetClass::Power));
        assert_eq!(m.get("out"), Some(&NetClass::Signal));
    }

    #[test]
    fn untagged_source_does_not_create_power() {
        let m = classify_str("V1 in 0 AC 1\nR1 in out 1k\n.end\n");
        assert_eq!(m.get("in"), Some(&NetClass::Signal));
        assert_eq!(m.get("out"), Some(&NetClass::Signal));
    }

    #[test]
    fn signal_net_default() {
        let m = classify_str("V1 in 0 AC 1\nR1 in mid 1k\nC1 mid 0 1u\n.end\n");
        assert_eq!(m.get("mid"), Some(&NetClass::Signal));
    }
}
```

Run: `cargo test -p spice-layout net_class -- --nocapture`
Expected: FAIL — module doesn't exist.

- [ ] **Step 2: Implement minimal classifier**

```rust
// crates/spice-layout/src/net_class.rs
//! Net classification (Power/Ground/Signal). See spec §3.
//!
//! Pure function: takes a `CheckedNetlist`, returns a class per net.
//! Used downstream by `bands.rs` (Y-banding) and `layers.rs` (which
//! prunes Power/Ground edges from the signal-flow DAG so feedback
//! through rails doesn't create false cycles).

use std::collections::HashMap;

use spice_policy::CheckedNetlist;
use spice_resolve::{ElementKind, ElementRole};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetClass {
    Power,
    Ground,
    Signal,
}

pub type NetClassMap = HashMap<String, NetClass>;

/// Classify every net referenced by the netlist. Rules in spec §3.
pub fn classify_nets(checked: &CheckedNetlist) -> NetClassMap {
    let mut map: NetClassMap = HashMap::new();

    // Rule 1: ground = "0".
    map.insert("0".to_string(), NetClass::Ground);

    // Rule 2: positive terminal of any *@power-tagged source.
    for el in &checked.elements {
        if matches!(el.role, ElementRole::Power) && !el.nodes.is_empty() {
            map.insert(el.nodes[0].clone(), NetClass::Power);
        }
    }

    // Rule 3: .global names matching supply patterns.
    for g in &checked.globals {
        let lower = g.to_ascii_lowercase();
        if matches_power_name(&lower) {
            map.entry(g.clone()).or_insert(NetClass::Power);
        } else if matches_ground_name(&lower) {
            map.entry(g.clone()).or_insert(NetClass::Ground);
        }
    }

    // Rule 4: any net touched by ≥1 *@power source → Power.
    for el in &checked.elements {
        if matches!(el.role, ElementRole::Power) {
            for n in &el.nodes {
                map.entry(n.clone()).or_insert(NetClass::Power);
            }
        }
    }

    // Rule 5: bypass-cap reclassification — skip in v0.1 (decoupling
    // caps are recognised only by their topology; current heuristic
    // already produces correct results for the five fixtures because
    // bypass caps connect Power+Ground only and don't contaminate
    // anything).

    // Rule 6: everything else → Signal.
    for el in &checked.elements {
        for n in &el.nodes {
            map.entry(n.clone()).or_insert(NetClass::Signal);
        }
    }

    map
}

fn matches_power_name(lower: &str) -> bool {
    matches!(lower, "vcc" | "vdd" | "v+" | "vplus")
}

fn matches_ground_name(lower: &str) -> bool {
    matches!(lower, "gnd" | "vee" | "vss" | "v-" | "vminus")
}
```

Add `pub mod net_class;` to `crates/spice-layout/src/lib.rs` near the existing `pub mod cost;` line.

- [ ] **Step 3: Run tests**

Run: `cargo test -p spice-layout net_class`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spice-layout/src/net_class.rs crates/spice-layout/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(layout): net classification (Power/Ground/Signal)

First module of the structural layered placement pipeline. Pure
function over CheckedNetlist; feeds Y-banding and X-layering downstream.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Y-band assignment module

**Files:**
- Create: `crates/spice-layout/src/bands.rs`
- Modify: `crates/spice-layout/src/lib.rs` (`pub mod bands;`)

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::net_class::classify_nets;
    use spice_parser::parse;
    use spice_policy::check;

    fn assign_str(src: &str) -> Vec<(String, BandAssignment)> {
        let parsed = parse(src).unwrap();
        let resolved = spice_resolve::resolve(parsed).unwrap();
        let checked = check(resolved).unwrap();
        let classes = classify_nets(&checked);
        let bands = assign_y_bands(&checked, &classes);
        checked.elements.iter()
            .zip(bands.iter())
            .map(|(e, b)| (e.designator.clone(), *b))
            .collect()
    }

    #[test]
    fn power_to_signal_resistor_is_mid_top_biased() {
        // RC: vcc → collector. Should be Mid band biased toward Top.
        let v = assign_str("V1 vcc 0 12 ;@ power\nR1 vcc out 1k\n.end\n");
        let r1 = v.iter().find(|(d, _)| d == "R1").unwrap().1;
        assert_eq!(r1.band, Band::Mid);
        assert!(r1.soft_y_target_frac < 0.5, "RC should bias toward top");
    }

    #[test]
    fn signal_to_ground_resistor_is_mid_bot_biased() {
        // RE: emitter → 0. Should be Mid biased toward Bot.
        let v = assign_str("V1 vcc 0 12 ;@ power\nR1 emit 0 1k\n.end\n");
        let r1 = v.iter().find(|(d, _)| d == "R1").unwrap().1;
        assert_eq!(r1.band, Band::Mid);
        assert!(r1.soft_y_target_frac > 0.5, "RE should bias toward bot");
    }

    #[test]
    fn signal_only_resistor_has_no_bias() {
        let v = assign_str("V1 in 0 AC 1\nR1 in mid 1k\nC1 mid 0 1u\n.end\n");
        let r1 = v.iter().find(|(d, _)| d == "R1").unwrap().1;
        assert_eq!(r1.band, Band::Mid);
        assert!((r1.soft_y_target_frac - 0.5).abs() < 1e-6);
    }
}
```

- [ ] **Step 2: Implement**

```rust
// crates/spice-layout/src/bands.rs
//! Element → Y-band assignment. See spec §4.
//!
//! Three bands top→bottom: Top (Power rail), Mid (signal area),
//! Bot (Ground rail). Each element gets a `BandAssignment` with a
//! soft Y target as a fraction of Mid-band height (0.0 = top of Mid,
//! 1.0 = bottom). Hard band placement (Top/Bot) skips the fraction.

use spice_policy::CheckedNetlist;

use crate::net_class::{NetClass, NetClassMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    Top,
    Mid,
    Bot,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandAssignment {
    pub band: Band,
    /// Y target within Mid band as fraction [0, 1]. Ignored when
    /// `band != Mid`. 0.5 = no bias.
    pub soft_y_target_frac: f64,
}

pub fn assign_y_bands(
    checked: &CheckedNetlist,
    classes: &NetClassMap,
) -> Vec<BandAssignment> {
    checked
        .elements
        .iter()
        .map(|el| classify_element(&el.nodes, classes))
        .collect()
}

fn classify_element(nodes: &[String], classes: &NetClassMap) -> BandAssignment {
    let class_of = |n: &String| classes.get(n).copied().unwrap_or(NetClass::Signal);
    let touches_power = nodes.iter().any(|n| class_of(n) == NetClass::Power);
    let touches_ground = nodes.iter().any(|n| class_of(n) == NetClass::Ground);
    let touches_signal = nodes.iter().any(|n| class_of(n) == NetClass::Signal);

    match (touches_power, touches_ground, touches_signal) {
        (true, false, false) => BandAssignment { band: Band::Top, soft_y_target_frac: 0.0 },
        (false, true, false) => BandAssignment { band: Band::Bot, soft_y_target_frac: 1.0 },
        (true, true, _) => BandAssignment { band: Band::Mid, soft_y_target_frac: 0.5 }, // spans both rails
        (true, false, true) => BandAssignment { band: Band::Mid, soft_y_target_frac: 1.0 / 3.0 },
        (false, true, true) => BandAssignment { band: Band::Mid, soft_y_target_frac: 2.0 / 3.0 },
        (false, false, true) | (false, false, false) => BandAssignment { band: Band::Mid, soft_y_target_frac: 0.5 },
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spice-layout bands`
Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spice-layout/src/bands.rs crates/spice-layout/src/lib.rs
git commit -m "feat(layout): Y-band assignment from net classification"
```

---

## Task 3: Signal DAG + cycle break + layer assignment

**Files:**
- Create: `crates/spice-layout/src/layers.rs`
- Modify: `crates/spice-layout/src/lib.rs` (`pub mod layers;`)

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::net_class::classify_nets;
    use spice_parser::parse;

    fn layer_str(src: &str) -> std::collections::HashMap<String, u32> {
        let parsed = parse(src).unwrap();
        let resolved = spice_resolve::resolve(parsed).unwrap();
        let checked = spice_policy::check(resolved).unwrap();
        let classes = classify_nets(&checked);
        let asg = assign_x_layers(&checked, &classes);
        checked.elements.iter().enumerate()
            .map(|(i, e)| (e.designator.clone(), asg.layers[i]))
            .collect()
    }

    #[test]
    fn rc_lowpass_layers_strict_left_to_right() {
        // V1 = layer 0, R1 = layer 1, C1 = layer 2 (linear chain).
        let m = layer_str("V1 in 0 AC 1\nR1 in mid 1k\nC1 mid 0 1u\n.end\n");
        assert!(m["V1"] < m["R1"]);
        assert!(m["R1"] <= m["C1"]); // C1 may co-layer with R1 or sit later
    }

    #[test]
    fn cycle_is_broken() {
        // Multivibrator skeleton: Q1 ↔ Q2 cross-coupling forms a cycle.
        // Layer assignment must terminate (no infinite loop) and
        // produce a finite layer for both.
        let src = "V1 vcc 0 12 ;@ power\n\
                   Q1 c1 b2 0 QGENERIC\n\
                   Q2 c2 b1 0 QGENERIC\n\
                   R1 vcc c1 1k\nR2 vcc c2 1k\n\
                   R3 vcc b1 10k\nR4 vcc b2 10k\n\
                   C1 c1 b2 1n\nC2 c2 b1 1n\n.end\n";
        let m = layer_str(src);
        assert!(m.contains_key("Q1"));
        assert!(m.contains_key("Q2"));
    }
}
```

- [ ] **Step 2: Implement**

```rust
// crates/spice-layout/src/layers.rs
//! Directed signal-flow graph + Tarjan cycle break + longest-path
//! layering + barycentric ordering. See spec §5.

use std::collections::{HashMap, HashSet};

use spice_policy::CheckedNetlist;
use spice_resolve::{ElementKind, ElementRole};

use crate::net_class::{NetClass, NetClassMap};

#[derive(Debug, Clone)]
pub struct LayerAssignment {
    /// Layer index per element (parallel to checked.elements).
    pub layers: Vec<u32>,
    /// Rank within layer, used to compute initial Y stacking.
    pub rank_in_layer: Vec<u32>,
    /// Edges that were reversed during cycle break (feedback edges).
    pub feedback_edges: Vec<(usize, usize)>,
    /// True when the graph had no signal sources and we fell back to
    /// "no preferred X" (e.g. multivibrator). Caller may choose to
    /// emit a column-major fallback layout instead.
    pub no_source_fallback: bool,
}

pub fn assign_x_layers(
    checked: &CheckedNetlist,
    classes: &NetClassMap,
) -> LayerAssignment {
    let n = checked.elements.len();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut sources: HashSet<usize> = HashSet::new();

    // Build directed edges from element-pair connections through Signal nets.
    let net_to_elements = build_net_to_elements(checked, classes);
    for (_net, members) in &net_to_elements {
        // Decide a direction within this net by element kinds.
        for (idx, role) in members {
            if is_signal_source(checked, *idx) {
                sources.insert(*idx);
                for (other_idx, other_role) in members {
                    if *other_idx != *idx && other_role.is_some() {
                        adj[*idx].push(*other_idx);
                    }
                }
            } else if role.is_some() {
                // Non-source: add an undirected hint; resolved by Tarjan.
                for (other_idx, _) in members {
                    if *other_idx != *idx {
                        adj[*idx].push(*other_idx);
                    }
                }
            }
        }
    }

    if sources.is_empty() {
        return LayerAssignment {
            layers: vec![0; n],
            rank_in_layer: (0..n as u32).collect(),
            feedback_edges: Vec::new(),
            no_source_fallback: true,
        };
    }

    // Tarjan SCC, reverse highest-feedback-score edge in each non-trivial SCC.
    let (dag, feedback_edges) = break_cycles(&adj);

    // Longest-path layering from sources.
    let layers = longest_path_layers(&dag, &sources, n);

    // Barycentric Y rank within each layer.
    let rank_in_layer = barycentric_ranks(&layers, &dag);

    LayerAssignment { layers, rank_in_layer, feedback_edges, no_source_fallback: false }
}

fn is_signal_source(checked: &CheckedNetlist, idx: usize) -> bool {
    let el = &checked.elements[idx];
    matches!(el.kind, ElementKind::Vsource | ElementKind::Isource)
        && !matches!(el.role, ElementRole::Power)
}

fn build_net_to_elements(
    checked: &CheckedNetlist,
    classes: &NetClassMap,
) -> HashMap<String, Vec<(usize, Option<TerminalRole>)>> {
    // Stub: collect (element_idx, terminal_role_optional) per Signal net.
    // Power/Ground nets pruned per spec §5.
    let mut out: HashMap<String, Vec<(usize, Option<TerminalRole>)>> = HashMap::new();
    for (idx, el) in checked.elements.iter().enumerate() {
        for n in &el.nodes {
            if classes.get(n).copied().unwrap_or(NetClass::Signal) == NetClass::Signal {
                out.entry(n.clone()).or_default().push((idx, None));
            }
        }
    }
    out
}

#[derive(Debug, Clone, Copy)]
struct TerminalRole; // placeholder; concrete pin role inference is a v0.2 polish

fn break_cycles(adj: &[Vec<usize>]) -> (Vec<Vec<usize>>, Vec<(usize, usize)>) {
    // Iteratively run Tarjan and reverse one edge per non-trivial SCC.
    let mut dag: Vec<Vec<usize>> = adj.to_vec();
    let mut reversed = Vec::new();
    loop {
        let sccs = tarjan(&dag);
        let mut found_cycle = false;
        for scc in &sccs {
            if scc.len() < 2 {
                continue;
            }
            found_cycle = true;
            let scc_set: HashSet<usize> = scc.iter().copied().collect();
            // Pick the edge whose source has the highest in-degree within
            // the SCC (heuristic: prefer reversing the explicit feedback
            // path).
            let mut best: Option<(usize, usize, usize)> = None; // (src, dst, score)
            for &u in scc {
                let in_deg = dag.iter().enumerate()
                    .filter(|(i, _)| scc_set.contains(i))
                    .filter(|(_, vs)| vs.contains(&u))
                    .count();
                for &v in &dag[u] {
                    if scc_set.contains(&v) {
                        let score = in_deg;
                        if best.map_or(true, |b| score > b.2) {
                            best = Some((u, v, score));
                        }
                    }
                }
            }
            if let Some((u, v, _)) = best {
                dag[u].retain(|&x| x != v);
                dag[v].push(u);
                reversed.push((u, v));
            }
            break; // re-run Tarjan after each reversal
        }
        if !found_cycle {
            break;
        }
    }
    (dag, reversed)
}

fn tarjan(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = adj.len();
    let mut index = 0_usize;
    let mut stack: Vec<usize> = Vec::new();
    let mut on_stack = vec![false; n];
    let mut indices = vec![usize::MAX; n];
    let mut lowlink = vec![0; n];
    let mut sccs: Vec<Vec<usize>> = Vec::new();

    fn strongconnect(
        v: usize,
        adj: &[Vec<usize>],
        index: &mut usize,
        stack: &mut Vec<usize>,
        on_stack: &mut [bool],
        indices: &mut [usize],
        lowlink: &mut [usize],
        sccs: &mut Vec<Vec<usize>>,
    ) {
        indices[v] = *index;
        lowlink[v] = *index;
        *index += 1;
        stack.push(v);
        on_stack[v] = true;
        for &w in &adj[v] {
            if indices[w] == usize::MAX {
                strongconnect(w, adj, index, stack, on_stack, indices, lowlink, sccs);
                lowlink[v] = lowlink[v].min(lowlink[w]);
            } else if on_stack[w] {
                lowlink[v] = lowlink[v].min(indices[w]);
            }
        }
        if lowlink[v] == indices[v] {
            let mut scc = Vec::new();
            loop {
                let w = stack.pop().unwrap();
                on_stack[w] = false;
                scc.push(w);
                if w == v { break; }
            }
            sccs.push(scc);
        }
    }

    for v in 0..n {
        if indices[v] == usize::MAX {
            strongconnect(v, adj, &mut index, &mut stack, &mut on_stack,
                          &mut indices, &mut lowlink, &mut sccs);
        }
    }
    sccs
}

fn longest_path_layers(dag: &[Vec<usize>], sources: &HashSet<usize>, n: usize) -> Vec<u32> {
    let mut layers = vec![0_u32; n];
    let mut order = topo_order(dag, n);
    for v in order.drain(..) {
        if !sources.contains(&v) {
            // layer(v) = 1 + max(layer(pred(v)))
            let mut max_pred = 0_u32;
            let mut has_pred = false;
            for (u, succs) in dag.iter().enumerate() {
                if succs.contains(&v) {
                    has_pred = true;
                    max_pred = max_pred.max(layers[u]);
                }
            }
            if has_pred {
                layers[v] = max_pred + 1;
            }
        }
    }
    layers
}

fn topo_order(dag: &[Vec<usize>], n: usize) -> Vec<usize> {
    let mut indeg = vec![0_usize; n];
    for succs in dag {
        for &v in succs {
            indeg[v] += 1;
        }
    }
    let mut q: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut out = Vec::with_capacity(n);
    while let Some(u) = q.pop() {
        out.push(u);
        for &v in &dag[u] {
            indeg[v] -= 1;
            if indeg[v] == 0 {
                q.push(v);
            }
        }
    }
    out
}

fn barycentric_ranks(layers: &[u32], _dag: &[Vec<usize>]) -> Vec<u32> {
    // v0.1: rank by element index within layer. One Sugiyama iteration
    // (median of neighbours) is a v0.2 polish; the cost-function
    // y-target term plus the annealer's swap move take care of
    // refinement.
    let mut counts: HashMap<u32, u32> = HashMap::new();
    let mut ranks = vec![0_u32; layers.len()];
    for (i, &layer) in layers.iter().enumerate() {
        let r = counts.entry(layer).or_insert(0);
        ranks[i] = *r;
        *r += 1;
    }
    ranks
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spice-layout layers`
Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spice-layout/src/layers.rs crates/spice-layout/src/lib.rs
git commit -m "feat(layout): X-layer assignment with cycle break"
```

---

## Task 4: Replace seed body with band+layer driven placement

**Files:**
- Modify: `crates/spice-layout/src/lib.rs` (`place_seed` function body)

- [ ] **Step 1: Read existing `place_seed`**

Read `place_seed` (lib.rs ~line 478) so the new implementation preserves: returning `Result<(Placement, Vec<bool>), Vec<Diagnostic>>`, honouring `align`/`place` constraints, building `Placement::elements: Vec<PlacedElement>`.

- [ ] **Step 2: Rewrite `place_seed`**

Replace the existing four-phase body with: classify nets → assign bands → assign layers → emit grid coordinates from `(band, layer, rank_in_layer)` using the constants `X_STRIDE = 5 * CELL_W`, `Y_TOP = 0`, `Y_BOT = N * CELL_H` (N = element count + 2), `Y_MID = (Y_TOP + Y_BOT) / 2`. Then run the existing `align`/`place` constraint resolution on top, leaving its `pinned` mask intact.

```rust
fn place_seed(checked: &CheckedNetlist) -> Result<(Placement, Vec<bool>), Vec<Diagnostic>> {
    use crate::{bands::{assign_y_bands, Band}, layers::assign_x_layers, net_class::classify_nets};

    let n = checked.elements.len();
    let classes = classify_nets(checked);
    let bands = assign_y_bands(checked, &classes);
    let layers = assign_x_layers(checked, &classes);

    let x_stride = 5_i32; // grid cells
    let y_top = 0_i32;
    let y_bot = (n as i32 + 2) * 4; // 4 grid cells per layer slot
    let y_mid_top = y_top + 4;
    let y_mid_bot = y_bot - 4;

    let mut elements: Vec<PlacedElement> = Vec::with_capacity(n);
    for (i, el) in checked.elements.iter().enumerate() {
        let layer = layers.layers[i] as i32;
        let rank = layers.rank_in_layer[i] as i32;
        let x = layer * x_stride;
        let y_mid_target = y_mid_top + ((y_mid_bot - y_mid_top) as f64 * bands[i].soft_y_target_frac) as i32;
        let y = match bands[i].band {
            Band::Top => y_top,
            Band::Bot => y_bot,
            Band::Mid => y_mid_target + rank * 2,
        };
        elements.push(PlacedElement {
            origin: GridPoint { x, y },
            orientation: Orientation::IDENTITY,
            // …other PlacedElement fields kept from the existing impl…
        });
    }

    let placement = Placement { elements };
    let mut pinned = vec![false; n];

    // Re-run existing align/place constraint resolution against the new
    // seed — these are hard constraints that override the band+layer
    // initial coordinates. (Lift the body of the previous resolver pass
    // into a helper `apply_user_constraints(&mut placement, &mut pinned, checked)`
    // and call it here.)
    apply_user_constraints(&mut placement, &mut pinned, checked)?;

    Ok((placement, pinned))
}
```

- [ ] **Step 3: Run existing tests to find regressions**

Run: `cargo test -p spice-layout`
Expected: many to pass; some may fail (expected — old V6 archetype tests cover behaviour that is changing). Fix only those with clear logic errors; *don't* hack tests to pass yet.

- [ ] **Step 4: Run wider workspace**

Run: `cargo test --workspace -- --skip placement_quality`
Expected: layout/cost/parser/policy tests pass. The placement-quality fixture-wide tests change in Task 8 — skip them for now.

- [ ] **Step 5: Commit**

```bash
git add crates/spice-layout/src/lib.rs
git commit -m "feat(layout): seed placement from bands and layers"
```

---

## Task 5: Remove archetype module

**Files:**
- Delete: `crates/spice-layout/src/archetype/mod.rs`, `crates/spice-layout/src/archetype/common_emitter.rs`
- Modify: `crates/spice-layout/src/lib.rs` (drop `mod archetype;` and the two call sites in `place_with`)

- [ ] **Step 1: Delete the module directory**

```bash
git rm -r crates/spice-layout/src/archetype/
```

- [ ] **Step 2: Strip references from `lib.rs`**

Edit `place_with`:
- Remove the `mod archetype;` declaration near the top.
- Remove the block:
  ```rust
  let seeds = archetype::detect_and_seed(&checked);
  if !seeds.is_empty() {
      archetype::apply_seeds(&mut placement, &mut pinned, &seeds);
  }
  ```

- [ ] **Step 3: Verify build**

Run: `cargo check -p spice-layout`
Expected: compiles clean.

Run: `cargo test --workspace -- --skip placement_quality`
Expected: no archetype-related test failures (there are no archetype tests outside placement_quality).

- [ ] **Step 4: Commit**

```bash
git add crates/spice-layout/src/lib.rs
git commit -m "refactor(layout): remove topology archetype matcher"
```

---

## Task 6: Cost function — band misalignment + soft Y target + layer order + crossing approx

**Files:**
- Modify: `crates/spice-layout/src/cost.rs`

- [ ] **Step 1: Extend `CostBreakdown`**

Add four fields:

```rust
pub struct CostBreakdown {
    pub hpwl: f64,
    pub overlap: f64,
    pub crossings: f64,
    pub constraint_violation: f64,
    pub rail_direction: f64,
    pub signal_flow: f64,
    /// NEW: clamp-distance² of elements outside their assigned band.
    pub band_misalignment: f64,
    /// NEW: squared distance from soft Y target for Mid-band elements.
    pub soft_y_residual: f64,
    /// NEW: clamped sum of (x_pred - x_self)² for layer-order violations.
    pub layer_order: f64,
    /// NEW: net-bbox cross-pair count (cheap crossing proxy).
    pub net_bbox_crossings: f64,
}
```

- [ ] **Step 2: Implement four new functions**

Each takes `(placement, checked, bands_or_layers)` and returns `f64`. Add unit tests in `cost.rs::tests` for each: a 2-element fixture with known coordinates, expected cost.

- [ ] **Step 3: Update `total` weights**

```rust
const W_HPWL: f64 = 1.0;
const W_OVERLAP: f64 = 5.0;
const W_CROSSINGS: f64 = 0.5;
const W_CONSTRAINT: f64 = 100.0;
const W_RAIL: f64 = 2.0;
const W_SIGNAL_FLOW: f64 = 1.0;
const W_BAND: f64 = 10.0;             // hard-ish: bands matter
const W_SOFT_Y: f64 = 0.5;            // gentle bias
const W_LAYER_ORDER: f64 = 2.0;
const W_NET_BBOX_CROSSINGS: f64 = 0.5;
```

Calibrate against fixtures during Task 8 (the placement-quality tests will tell us when weights are wrong).

- [ ] **Step 4: Run cost tests**

Run: `cargo test -p spice-layout cost`
Expected: existing tests pass, four new tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spice-layout/src/cost.rs
git commit -m "feat(layout): cost terms for bands, soft-Y, layer order, crossings"
```

---

## Task 7: Refine-by-default + new annealer move

**Files:**
- Modify: `crates/spice-layout/src/solver/mod.rs` (default `refine: true`)
- Modify: `crates/spice-layout/src/solver/anneal.rs` (add swap move)
- Modify: `crates/spice2kicad/src/main.rs` (CLI flag rename)

- [ ] **Step 1: Default refine to true**

In `solver/mod.rs`:

```rust
impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            refine: true,
            refine_iterations: 200,
            // …
        }
    }
}
```

Add `refine_iterations: u32` field to `LayoutOptions` if not present.

- [ ] **Step 2: Add same-layer Y-rank swap move to annealer**

In `solver/anneal.rs::propose_move`, add a 4th move type that picks two same-layer elements (using the `LayerAssignment` passed in via context) and swaps their Y coordinates. Probability ≈ 0.2.

- [ ] **Step 3: CLI flag rename**

In `crates/spice2kicad/src/main.rs`:
- Replace `--refine` boolean with `--no-refine` (escape hatch) and `--refine-iterations N` (default 200).
- Wire through to `LayoutOptions`.

- [ ] **Step 4: Run solver tests**

Run: `cargo test -p spice-layout solver`
Expected: pass.

- [ ] **Step 5: Run CLI smoke test**

Run: `cargo run --bin spice2kicad -- examples/rc_lowpass.cir -l crates/kicad-symbols/tests/fixtures/Device.kicad_sym -l crates/kicad-symbols/tests/fixtures/Simulation_SPICE.kicad_sym -o /tmp/rc.kicad_sch`
Expected: emits .kicad_sch without `--refine` flag.

- [ ] **Step 6: Commit**

```bash
git add crates/spice-layout/src/solver/ crates/spice2kicad/src/main.rs
git commit -m "feat(layout): refine-by-default + Y-rank swap move"
```

---

## Task 8: Replace V6 archetype tests with fixture-wide quality tests

**Files:**
- Modify: `crates/spice2kicad/tests/placement_quality.rs`

- [ ] **Step 1: Drop the three V6 archetype tests**

Delete tests `v6_common_emitter_rails_horizontal`, `v6_common_emitter_signal_flow_ordering`, `v6_common_emitter_q1_central` and their helpers.

- [ ] **Step 2: Add fixture iterator**

```rust
fn fixtures() -> Vec<(&'static str, &'static str)> {
    vec![
        ("rc_lowpass",          "tests/fixtures/rc_lowpass.cir"),
        ("common_emitter",      "tests/fixtures/common_emitter.cir"),
        ("multivibrator",       "tests/fixtures/multivibrator.cir"),
        ("diff_pair",           "tests/fixtures/diff_pair.cir"),
        ("opamp_inverting_real","tests/fixtures/opamp_inverting_real.cir"),
    ]
}
```

- [ ] **Step 3: Implement six fixture-wide tests**

For each item from spec §7:

```rust
#[test]
fn no_symbol_symbol_overlap_across_fixtures() {
    for (name, path) in fixtures() {
        let placement = run_placer(path);
        let bboxes: Vec<_> = placement.elements.iter().map(symbol_bbox_padded_1_27mm).collect();
        for i in 0..bboxes.len() {
            for j in (i+1)..bboxes.len() {
                assert!(!bboxes[i].intersects(&bboxes[j]),
                    "{}: symbols {} and {} overlap", name, i, j);
            }
        }
    }
}

#[test]
fn no_symbol_label_overlap_across_fixtures() { /* symmetric, label bbox vs symbol bbox */ }

#[test]
fn rails_correctly_ordered_across_fixtures() { /* max(power_y) < min(ground_y) */ }

#[test]
fn wire_length_within_budget_across_fixtures() {
    let budget_per_fixture: HashMap<&str, f64> = [
        ("rc_lowpass", 1.5),
        ("common_emitter", 2.5),
        ("multivibrator", 3.0),
        ("diff_pair", 2.5),
        ("opamp_inverting_real", 2.5),
    ].into();
    /* assert total wire / pin-pair-Manhattan ≤ budget */
}

#[test]
fn crossing_count_within_budget_across_fixtures() {
    let budget: HashMap<&str, u32> = [
        ("rc_lowpass", 0), ("common_emitter", 2),
        ("multivibrator", 4), ("opamp_inverting_real", 2), ("diff_pair", 2),
    ].into();
    /* assert true wire-segment crossings ≤ budget per fixture */
}

#[test]
fn common_emitter_signal_flows_left_to_right() {
    let p = run_placer("tests/fixtures/common_emitter.cir");
    let vin_x = pin_x(&p, "VIN", 0); // positive terminal
    let collector_x = pin_x_for_net(&p, "vc"); // collector net
    assert!(vin_x < collector_x);
}
```

- [ ] **Step 4: Run quality tests; calibrate weights**

Run: `cargo test -p spice2kicad placement_quality`

If any fail, *adjust the cost weights* in `cost.rs` (Task 6) — not the test thresholds. If a cost weight pegs the layout against a constraint, that's the calibration loop. Bias toward tightening thresholds once they pass.

Iterate until all six tests pass on all five fixtures.

- [ ] **Step 5: Commit**

```bash
git add crates/spice2kicad/tests/placement_quality.rs crates/spice-layout/src/cost.rs
git commit -m "test(layout): fixture-wide structural quality checks (replaces V6 archetype tests)"
```

---

## Task 9: Visual sanity check via SVG export

**Files:** none (manual)

- [ ] **Step 1: Generate schematics for all fixtures**

```bash
mkdir -p /tmp/v6r && cargo build --release --bin spice2kicad
LIBS="-l crates/kicad-symbols/tests/fixtures/Device.kicad_sym -l crates/kicad-symbols/tests/fixtures/Simulation_SPICE.kicad_sym -l crates/kicad-symbols/tests/fixtures/Amplifier_Operational.kicad_sym"
for f in examples/rc_lowpass.cir crates/spice2kicad/tests/fixtures/*.cir; do
  name=$(basename "$f" .cir); mkdir -p /tmp/v6r/$name
  ./target/release/spice2kicad $LIBS "$f" -o /tmp/v6r/$name/$name.kicad_sch
  kicad-cli sch export svg --no-background-color -o /tmp/v6r/$name/ /tmp/v6r/$name/$name.kicad_sch
done
```

- [ ] **Step 2: Inspect each SVG**

Open each `/tmp/v6r/*/<name>.svg` (or convert to PNG) and confirm visually:
- Power rail at top, ground at bottom
- Signal flows left to right where applicable
- No overlapping symbols
- Reasonable wire density

If any fixture looks bad despite tests passing, *the tests are too lax* — tighten thresholds in Task 8 and re-run.

- [ ] **Step 3: No commit unless tightening required.**

---

## Task 10: CLAUDE.md + roadmap updates

**Files:**
- Modify: `CLAUDE.md`
- Modify: `docs/layout-roadmap.md`

- [ ] **Step 1: Rewrite V6 invariant section**

Open `CLAUDE.md`, find the "V6 — Topology-aware placement" section. Replace title with "V6 — Structural layered placement". Replace body with a summary of net classification (§3), Y-banding (§4), X-layering with cycle break (§5), refinement cost terms (§6). Verifier paragraph: list the six fixture-wide tests from `placement_quality.rs`. Drop the "common-emitter archetype" example.

- [ ] **Step 2: Light edit to V7 section**

Find phrases like "many archetype templates have symmetry baked in" — drop them. V7 still composes with V6 via the pipeline order in §2 of the spec.

- [ ] **Step 3: Add design principle 9**

In "Core design principles", append:

> **9. Structural placement, not pattern recognition.** The placer infers structure from net classification and signal-flow direction; it does not match named topologies. Adding a new circuit type should require zero placer code changes. The escape hatch when heuristics fail is `*@place` / `*@align` — already in v0.1.

- [ ] **Step 4: Update layout-roadmap.md**

One-line update: change the §7 sequencing description from "stage-1 seed + archetype overlay + symmetry + orientation + optional refine" to "classify → bands → layers → seed + symmetry + orientation + refine (default)".

- [ ] **Step 5: Run `just check`**

Run: `just check`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add CLAUDE.md docs/layout-roadmap.md
git commit -m "docs: V6 reframed as structural layered placement"
```

---

## Task 11: Final workspace test + visual confirmation

- [ ] **Step 1: Full workspace test**

Run: `just check`
Expected: fmt + clippy + all tests green.

- [ ] **Step 2: Re-export all fixtures and confirm visual quality**

Re-run the script from Task 9. Confirm rendered schematics are noticeably better than the pre-redesign baseline (`/tmp/spice2kicad-output/` from current state).

- [ ] **Step 3: Optional polish commit**

If anything was tweaked in Step 2, commit it: `git commit -m "polish(layout): final calibration"`.

---

## Self-review notes

- All spec sections (§1–§9) are covered: §1 → Task 5; §2 → Task 4; §3 → Task 1; §4 → Task 2; §5 → Task 3; §6 → Task 6/7; §7 → Task 8; §8 → Task 10; §9 (risks) → calibration loops in Tasks 6/8.
- Cost-weight calibration is intentionally a loop in Task 8 ("adjust weights, not thresholds"), addressing Risk 3.
- Cycle-break heuristic (Risk 1) is implemented in Task 3 with the explicit "highest in-degree source" rule from the spec.
- Annealer runtime risk (Risk 4) is mitigated by `refine_iterations: 200` cap in Task 7.
- The placeholder-scan check: cost-weight values in Task 6 are concrete starting numbers; the "calibrate during Task 8" is a real iteration step, not a TBD.
