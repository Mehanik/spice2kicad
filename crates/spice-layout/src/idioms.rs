//! Idiom detection → constraint emission (roadmap §6, "Analog
//! readability strategy"; v0.2 Item 4).
//!
//! An *idiom detector* recognises a recurring analog sub-topology in the
//! resolved netlist and emits the **same placement constraint a user
//! would have written by hand** — never a raw coordinate. This keeps the
//! constraint pipeline (`align` / `place` / symmetry-pin) the single
//! source of truth: a detection is just an inferred `align`, and an
//! explicit user annotation always wins because detectors run *after*
//! the user constraints are already pinned and skip anything pinned.
//!
//! # What is implemented
//!
//! The **resistor divider**: two resistors in series (`Ra.tap ==
//! Rb.tap`) whose shared tap node connects to *exactly* those two
//! resistors, forming a chain between two distinct outer nets. The
//! conventional schematic stacks the divider vertically, so the detector
//! emits a **vertical `align`** of the pair — exactly the constraint a
//! user would write as `*@align vertical Ra Rb`.
//!
//! # Why this validates the channel
//!
//! The detector inspects only the resolved netlist + the seed placement,
//! produces a list of `(upper, lower)` element-index pairs, and applies
//! them through the **same mechanism** the user `align` path and V7
//! symmetry use: it sets the lower element's origin to a vertical stride
//! below the upper (sharing the upper's X column), then marks both
//! `pinned`. It writes *relative* geometry (a stack), never an absolute
//! page coordinate, and the downstream SA refiner / orientation chooser
//! leave the pinned pair put — proving detector → constraint → placer
//! end-to-end.
//!
//! # Specificity over recall (roadmap §6)
//!
//! A false-positive idiom is worse than none: it pins devices wrongly.
//! The detector is therefore strict —
//!
//! * both elements must be resistors (`ElementKind::Resistor`),
//! * each must be exactly two-terminal,
//! * they must share *exactly one* net (the tap),
//! * that tap net must have **degree exactly 2** (only the two
//!   resistors touch it — no third consumer, so it is genuinely a
//!   divider midpoint and not an arbitrary shared node) — but see
//!   `Placer::DividerRails`, which replaces this clause: the degree test
//!   matches every interior net of a plain *series chain* while
//!   rejecting every *loaded* bias divider, so it both over- and
//!   under-matches. Under that challenger the gate is instead "the two
//!   outer nets are rails of opposite `VertPref`, and the tap is a
//!   Signal net",
//! * the two *outer* nets must be distinct from each other and from the
//!   tap, and
//! * neither resistor may already be pinned (an explicit user
//!   `align`/`place` or a V7 symmetry pin wins).
//!
//! A resistor that already participates in one accepted divider is not
//! reused for a second, so a three-resistor chain `R1–R2–R3` yields the
//! single pair `(R1, R2)` (the lower-indexed greedy match) rather than
//! an overlapping `(R1,R2)+(R2,R3)`.

use std::collections::{HashMap, HashSet};

use spice_policy::CheckedNetlist;
use spice_resolve::{ElementKind, ElementRole, PortDir, ResolvedElement};

use kicad_symbols::{Orientation, Rotation, Symbol};

use crate::net_class::{NetClass, NetClassMap, VertPref, classify_nets, vertical_prefs};
use crate::placer::Placer;
use crate::{CELL_W, GridPoint, Placement, WorldExtent, vertical_stride_cells, world_extent};

/// True for a two-terminal passive (`R` / `C` / `L`) — the element kinds
/// the parallel-pair and shared-node idioms treat as stackable loads.
fn is_two_terminal_passive(e: &ResolvedElement) -> bool {
    e.nodes.len() == 2
        && matches!(
            e.kind,
            ElementKind::Resistor | ElementKind::Capacitor | ElementKind::Inductor
        )
}

/// World `x` mm of the pin of placed element `pe` that connects to SPICE
/// `net`, given its resolved `symbol`. `None` if `pe` has no terminal on
/// `net`. Pin-anchored (resolves the KiCad pin # via `pin_mapping`, then
/// the orientation-transformed pin set). Only `x` is returned because the
/// sole consumer (shared-node centering) centers on the pin's column; the
/// vertical drop is computed from grid origins, not the pin's world `y`.
fn world_pin_x_of(pe: &crate::PlacedElement, symbol: &Symbol, net: &str) -> Option<f64> {
    let ti = pe.nodes.iter().position(|n| n == net)?;
    let want = pe.pin_mapping.get(ti)?;
    pe.world_pin_mm(symbol)
        .into_iter()
        .find(|(num, _, _)| num == want)
        .map(|(_, x, _)| x)
}

/// A detected resistor-divider pair, by element index into
/// `Placement.elements` / `CheckedNetlist.elements`. `upper` is the
/// element placed on the smaller-world-Y side of the vertical stack;
/// `lower` sits one vertical stride below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DividerPair {
    pub upper: usize,
    pub lower: usize,
}

/// Detect every resistor-divider pair in `checked`.
///
/// Returns the pairs in a deterministic order (sorted by `upper` index).
/// Pairs never share an element (greedy lowest-index matching), so the
/// caller can apply them independently.
///
/// # Two acceptance predicates
///
/// The **shipping** one (every placer except
/// [`Placer::DividerRails`]) gates on the tap net's degree being
/// **exactly 2**. See [`Placer::DividerRails`] for why that matches the
/// wrong thing in both directions.
///
/// The **rail-gated** one, behind `--placer=divider-rails`, requires
/// instead that
///
/// * the tap is a [`NetClass::Signal`] net (a rail-class "midpoint" is a
///   short across the two resistors, not a divider tap), and
/// * the two outer nets are **rails of opposite [`VertPref`]** — one
///   positive supply (`Up`), one ground or negative rail (`Down`).
///
/// Everything else is shared: both resistors two-terminal, exactly two
/// resistors meeting at the tap, distinct outer nets, greedy
/// lowest-index matching, deterministic order.
///
/// The rail polarity additionally fixes the stack **order**: the
/// supply-side resistor is `upper`, the return-side one `lower`, rather
/// than whichever happens to have the smaller element index.
pub(crate) fn detect_dividers(checked: &CheckedNetlist, placer: Placer) -> Vec<DividerPair> {
    let rail_gated = placer.rail_gated_dividers();
    // Conservative reading (`divider-rails-strict`): keep the shipping
    // tap-degree gate ON TOP of the rail test, so the predicate only
    // ever narrows. See `Placer::DividerRailsStrict`.
    let unloaded_tap_only = placer.divider_tap_must_be_unloaded();
    // Only consulted on the rail-gated path; both are cheap pure
    // functions of `checked`, but computing them unconditionally would
    // put work on the shipping path for nothing.
    let (classes, prefs) = if rail_gated {
        (Some(classify_nets(checked)), Some(vertical_prefs(checked)))
    } else {
        (None, None)
    };
    let elems = &checked.elements;

    // net name -> number of terminals touching it (degree). Counts
    // both ordinary elements AND hierarchical-sheet instance ports: a
    // `.subckt` instance (e.g. an opamp lowered to a `(sheet …)`)
    // connects through its port nets exactly like any element, so a
    // tap node wired into a sheet port is genuinely degree > 2 and must
    // NOT be mistaken for a bare two-resistor divider midpoint. Missing
    // this is a false positive (the `opamp_inverting` `inv` net).
    let mut net_degree: HashMap<&str, usize> = HashMap::new();
    for e in elems {
        for node in &e.nodes {
            *net_degree.entry(node.as_str()).or_insert(0) += 1;
        }
    }
    for si in &checked.sheet_instances {
        for node in &si.nodes {
            *net_degree.entry(node.as_str()).or_insert(0) += 1;
        }
    }

    // net name -> resistor indices touching it (two-terminal resistors
    // only). Used to find the two resistors that meet at a tap node.
    let mut net_resistors: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, e) in elems.iter().enumerate() {
        if e.kind == ElementKind::Resistor && e.nodes.len() == 2 {
            for node in &e.nodes {
                net_resistors.entry(node.as_str()).or_default().push(i);
            }
        }
    }

    let mut used = vec![false; elems.len()];
    let mut pairs: Vec<DividerPair> = Vec::new();

    // Iterate candidate tap nets deterministically (by net name) so the
    // output order is stable regardless of HashMap iteration order.
    let mut tap_nets: Vec<&str> = net_resistors.keys().copied().collect();
    tap_nets.sort_unstable();

    for tap in tap_nets {
        if rail_gated {
            // A rail-to-rail divider's tap is what the circuit is FOR:
            // it drives a base, a gate or an op-amp input, so its degree
            // is routinely 3+. What it must not be is a rail — a
            // rail-class "midpoint" shorts one of the two resistors.
            if classes.as_ref().and_then(|c| c.get(tap)) != Some(&NetClass::Signal) {
                continue;
            }
            if unloaded_tap_only && net_degree.get(tap).copied() != Some(2) {
                continue;
            }
        } else {
            // A divider midpoint connects exactly two terminals, both of
            // which are the two resistors meeting here.
            if net_degree.get(tap).copied() != Some(2) {
                continue;
            }
        }
        let rs = &net_resistors[tap];
        if rs.len() != 2 {
            continue;
        }
        let (a, b) = (rs[0].min(rs[1]), rs[0].max(rs[1]));
        if used[a] || used[b] {
            continue;
        }

        // The two outer nets (the non-tap terminal of each resistor)
        // must be distinct from the tap and from each other — otherwise
        // this is a parallel pair or a self-loop, not a series divider.
        let (Some(outer_a), Some(outer_b)) = (
            other_net(&elems[a].nodes, tap),
            other_net(&elems[b].nodes, tap),
        ) else {
            continue;
        };
        if outer_a == tap || outer_b == tap || outer_a == outer_b {
            continue;
        }

        let (upper, lower) = if rail_gated {
            // Both outer nets must be rails, and of OPPOSITE vertical
            // preference: a supply above, a ground / negative rail
            // below. That is exactly the topology whose conventional
            // drawing is the vertical stack this idiom emits — and it is
            // what a plain series chain (Signal outer nets) is not.
            let prefs = prefs.as_ref().expect("rail-gated path computes prefs");
            let (Some(&pa), Some(&pb)) = (prefs.get(outer_a), prefs.get(outer_b)) else {
                continue;
            };
            if pa == pb {
                continue;
            }
            // Supply-side resistor on top, return-side one beneath it.
            if pa == VertPref::Up { (a, b) } else { (b, a) }
        } else {
            (a, b)
        };

        used[a] = true;
        used[b] = true;
        pairs.push(DividerPair { upper, lower });
    }

    pairs.sort_unstable_by_key(|p| p.upper);
    pairs
}

/// The single net of a two-terminal element that is *not* `net`.
/// Returns `None` if the element does not have exactly one other net
/// (i.e. both terminals are on `net`, a degenerate short).
fn other_net<'a>(nodes: &'a [String], net: &str) -> Option<&'a str> {
    let mut found: Option<&str> = None;
    for n in nodes {
        if n != net {
            if found.is_some() {
                return None; // more than one "other" net
            }
            found = Some(n.as_str());
        }
    }
    found
}

/// Apply detected divider pairs as a **vertical `align`** constraint:
/// stack the lower element directly below the upper, sharing the upper's
/// X column, separated by a geometry-derived vertical stride, then pin
/// both so the SA refiner and orientation chooser leave them put.
///
/// This is the exact mechanism the user `*@align vertical` path uses
/// (an X-shared, stride-separated column with both members pinned). It
/// emits *relative* geometry only — never a page coordinate — and
/// honours existing pins: a member already fixed by a user `align` /
/// `place` directive or by V7 symmetry is skipped, so an explicit
/// annotation always wins.
pub(crate) fn apply(
    placement: &mut Placement,
    pinned: &mut [bool],
    checked: &CheckedNetlist,
    pairs: &[DividerPair],
) {
    for &DividerPair { upper, lower } in pairs {
        stack_below(placement, pinned, checked, upper, lower);
    }
}

/// Stack `lower` one grid-snapped vertical stride below `upper`, sharing
/// `upper`'s X column, both at identity orientation, and pin both. A
/// no-op if either is already pinned by a stronger (user / V7) constraint.
///
/// This is the common mechanism behind the divider and parallel-pair
/// idioms: an X-shared, stride-separated vertical column (exactly what a
/// user `*@align vertical` writes). It emits *relative* geometry only —
/// never a page coordinate. For vertical two-terminal passives the local
/// pin-x offset is 0, so sharing the origin's X column == sharing the
/// connecting pins' X (pin-anchored).
fn stack_below(
    placement: &mut Placement,
    pinned: &mut [bool],
    checked: &CheckedNetlist,
    upper: usize,
    lower: usize,
) {
    if pinned[upper] || pinned[lower] {
        return;
    }
    // The vertical stride covers both resolved extents plus clearance,
    // snapped to the grid, so bodies/pins/value-text never clip.
    let upper_ext: WorldExtent =
        world_extent(&checked.elements[upper].symbol, Orientation::IDENTITY, None);
    let lower_ext: WorldExtent =
        world_extent(&checked.elements[lower].symbol, Orientation::IDENTITY, None);
    let stride = vertical_stride_cells(&upper_ext, &lower_ext);

    // Anchor the column at the upper member's seed coordinate (its
    // band-correct X/Y from `place_seed`), then drop the lower one
    // stride below in the same column.
    let anchor = placement.elements[upper].origin;
    placement.elements[upper].orientation = Orientation::IDENTITY;
    placement.elements[lower].orientation = Orientation::IDENTITY;
    placement.elements[lower].origin = GridPoint::new(anchor.x, anchor.y + stride);
    pinned[upper] = true;
    pinned[lower] = true;
}

// ===========================================================================
// Idiom 1 — PARALLEL two-terminal pair
// ===========================================================================

/// A detected parallel pair: two two-terminal passives sharing **both**
/// of their (distinct) nets. `a < b` by element index.
///
/// DEFERRED (not wired into the placer): a position-only same-column
/// parallel stack shorts one shared net past the other's pin when a
/// shared net is ground (V11), and the clean fix needs an orientation
/// flip the flow-wall forbids. See `lib::apply_position_idioms`. The
/// detector + its unit tests are retained to lock the detection semantics
/// for a v0.2 that owns the flip.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParallelPair {
    pub a: usize,
    pub b: usize,
}

/// Detect every parallel two-terminal-passive pair in `checked`.
///
/// A parallel pair is two passives (`R`/`C`/`L`), each exactly
/// two-terminal, that connect the **same two distinct nets** (share both
/// terminals). This is the deliberate complement of [`detect_dividers`]
/// (which requires sharing *exactly one* net): here we require sharing
/// *both*. Self-loops (both terminals on one net) are rejected. Greedy
/// lowest-index matching, each element used at most once, deterministic
/// order (sorted by `a`). The conventional schematic stacks a parallel
/// pair vertically, adjacent — but that stack is a V11 short under the
/// orientation wall, so this detector is currently **deferred** (unit
/// tested, not wired). See [`ParallelPair`] and
/// `lib::apply_position_idioms`.
#[allow(dead_code)]
pub(crate) fn detect_parallel_pairs(checked: &CheckedNetlist) -> Vec<ParallelPair> {
    let elems = &checked.elements;

    // Unordered net-pair {n0, n1} -> passive element indices connecting
    // exactly those two distinct nets.
    let mut by_net_pair: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (i, e) in elems.iter().enumerate() {
        if !is_two_terminal_passive(e) {
            continue;
        }
        let (n0, n1) = (e.nodes[0].as_str(), e.nodes[1].as_str());
        if n0 == n1 {
            continue; // self-loop, not a parallel pair
        }
        let key = if n0 <= n1 { (n0, n1) } else { (n1, n0) };
        by_net_pair.entry(key).or_default().push(i);
    }

    let mut keys: Vec<(&str, &str)> = by_net_pair.keys().copied().collect();
    keys.sort_unstable();

    let mut out: Vec<ParallelPair> = Vec::new();
    for key in keys {
        let mut idxs = by_net_pair[&key].clone();
        idxs.sort_unstable();
        // Greedy consecutive pairing: (idxs[0], idxs[1]), (idxs[2], …).
        for chunk in idxs.chunks(2) {
            if let [a, b] = *chunk {
                out.push(ParallelPair { a, b });
            }
        }
    }
    out.sort_unstable_by_key(|p| p.a);
    out
}

// ===========================================================================
// Idiom 2 — COLLECTOR-LOAD above transistor
// ===========================================================================

/// A resistor acting as a BJT collector load: exactly one of its two
/// terminals is on a transistor's collector net (and the other is not any
/// collector net). `transistor` is the lowest-index BJT owning that
/// collector net.
///
/// DEFERRED (not wired into the placer): repositioning the collector
/// resistor ripples the busiest crossing/wire-length ratchets across
/// `diff_pair` / `common_emitter` / `multivibrator`, and V7 symmetry
/// already pins `RC1`/`RC2` on `diff_pair`. See
/// `lib::apply_position_idioms`. The detector + its unit tests are
/// retained to lock the detection semantics for a v0.2.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CollectorLoad {
    pub resistor: usize,
    pub transistor: usize,
}

/// Detect every collector-load resistor in `checked`.
///
/// Strict discriminator (matching the unit tests): a two-terminal
/// `Resistor` with **exactly one** terminal on some BJT's **collector**
/// net (SPICE terminal 0 of a `Bjt`, `nodes[0]`), the other terminal on a
/// non-collector net. A base-net resistor (`nodes[1]`) or emitter-net
/// resistor (`nodes[2]`) is rejected; a resistor bridging two distinct
/// collectors is rejected. Deterministic order (sorted by resistor idx).
///
/// **Deferred** (unit tested, not wired) — see [`CollectorLoad`] and
/// `lib::apply_position_idioms`.
#[allow(dead_code)]
pub(crate) fn detect_collector_loads(checked: &CheckedNetlist) -> Vec<CollectorLoad> {
    let elems = &checked.elements;

    // Collector net -> lowest-index BJT owning it.
    let mut collector_of: HashMap<&str, usize> = HashMap::new();
    for (i, e) in elems.iter().enumerate() {
        if e.kind == ElementKind::Bjt {
            if let Some(c) = e.nodes.first() {
                collector_of.entry(c.as_str()).or_insert(i);
            }
        }
    }

    let mut out: Vec<CollectorLoad> = Vec::new();
    for (i, e) in elems.iter().enumerate() {
        if e.kind != ElementKind::Resistor || e.nodes.len() != 2 {
            continue;
        }
        let (n0, n1) = (e.nodes[0].as_str(), e.nodes[1].as_str());
        let c0 = collector_of.contains_key(n0);
        let c1 = collector_of.contains_key(n1);
        let coll_net = match (c0, c1) {
            (true, false) => n0,
            (false, true) => n1,
            _ => continue, // neither, or both -> not a clean collector load
        };
        out.push(CollectorLoad {
            resistor: i,
            transistor: collector_of[coll_net],
        });
    }
    out.sort_unstable_by_key(|h| (h.resistor, h.transistor));
    out
}

// ===========================================================================
// Idiom 3 — SHARED-NODE centering
// ===========================================================================

/// A shared-node center: a two-terminal passive `element` sitting on a
/// signal net shared by `transistors` (>= 2 BJTs) — a differential-pair
/// tail / shared-emitter node. `net` is the shared node's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SharedNodeCenter {
    pub element: usize,
    pub transistors: Vec<usize>,
    pub net: String,
}

/// Detect every shared-node-center idiom in `checked`.
///
/// Predicate: a net whose touching elements include **>= 2 BJTs** and
/// **exactly one** two-terminal passive, AND the net is neither a power
/// nor a ground/rail net (the rail guard is load-bearing — without it any
/// ground node with two transistor emitters plus a passive would mis-fire).
/// Deterministic order (iterated by net name). `transistors` is sorted by
/// element index.
pub(crate) fn detect_shared_node_centers(checked: &CheckedNetlist) -> Vec<SharedNodeCenter> {
    let elems = &checked.elements;
    let classes = classify_nets(checked);

    // Net -> element indices touching it (an element counted once even if
    // it touches the net on two terminals).
    let mut net_members: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, e) in elems.iter().enumerate() {
        let mut seen: HashSet<&str> = HashSet::new();
        for node in &e.nodes {
            if seen.insert(node.as_str()) {
                net_members.entry(node.as_str()).or_default().push(i);
            }
        }
    }

    let mut nets: Vec<&str> = net_members.keys().copied().collect();
    nets.sort_unstable();

    let mut out: Vec<SharedNodeCenter> = Vec::new();
    for net in nets {
        // Rail guard: a shared tail/emitter node is a *signal* net, never
        // a supply or ground rail. Excludes the multivibrator's `0` node.
        if matches!(classes.get(net), Some(NetClass::Power | NetClass::Ground)) {
            continue;
        }
        let members = &net_members[net];
        let transistors: Vec<usize> = members
            .iter()
            .copied()
            .filter(|&i| elems[i].kind == ElementKind::Bjt)
            .collect();
        if transistors.len() < 2 {
            continue;
        }
        let passives: Vec<usize> = members
            .iter()
            .copied()
            .filter(|&i| is_two_terminal_passive(&elems[i]))
            .collect();
        if passives.len() != 1 {
            continue;
        }
        out.push(SharedNodeCenter {
            element: passives[0],
            transistors,
            net: net.to_string(),
        });
    }
    out
}

/// Apply shared-node centers: X-center the passive's shared-net pin at the
/// midpoint of the transistors' pins on that net, and drop it one grid
/// stride below the lowest transistor. Only the passive is moved and
/// pinned; the transistors are *read*, never moved. Honours existing pins.
pub(crate) fn apply_shared_centers(
    placement: &mut Placement,
    pinned: &mut [bool],
    checked: &CheckedNetlist,
    hits: &[SharedNodeCenter],
) {
    /// Extra vertical clearance, in grid cells, between the lowest
    /// transistor and the centred passive — see the comment at the
    /// origin assignment below for why one cell is reserved.
    const TRUNK_STUB_CELLS: i32 = 1;

    for hit in hits {
        let el = hit.element;
        if pinned[el] {
            continue;
        }
        // Midpoint X of the transistors' pins on the shared net, and the
        // lowest (largest-Y) transistor origin for the vertical drop.
        let mut sum_x = 0.0_f64;
        let mut count = 0_u32;
        let mut max_q_y = i32::MIN;
        for &t in &hit.transistors {
            if let Some(x) = world_pin_x_of(
                &placement.elements[t],
                &checked.elements[t].symbol,
                &hit.net,
            ) {
                sum_x += x;
                count += 1;
            }
            max_q_y = max_q_y.max(placement.elements[t].origin.y);
        }
        if count == 0 {
            continue;
        }
        let mid = sum_x / f64::from(count);

        // Identity orientation, then shift X so the passive's shared-net
        // pin lands on the midpoint (pin-anchored centering).
        placement.elements[el].orientation = Orientation::IDENTITY;
        let Some(cur_x) = world_pin_x_of(
            &placement.elements[el],
            &checked.elements[el].symbol,
            &hit.net,
        ) else {
            continue;
        };
        #[allow(clippy::cast_possible_truncation)]
        let dx_cells = ((mid - cur_x) / GridPoint::STEP_MM).round() as i32;

        // One stride below the lowest transistor.
        let el_ext = world_extent(&checked.elements[el].symbol, Orientation::IDENTITY, None);
        let t0 = hit.transistors[0];
        let q_ext = world_extent(
            &checked.elements[t0].symbol,
            placement.elements[t0].orientation,
            None,
        );
        // One grid cell BELOW the clearance stride, so the passive's
        // shared-net pin cannot land on the trunk row itself.
        //
        // At the bare stride the pin coincides exactly with the row the
        // router picks for the trunk (the row is chosen *because* the pin
        // is there). The trunk then arrives horizontally and stops dead on
        // a pin whose outward direction is vertical — a V5 violation, and
        // visually a wire ending sideways on a pin. Reserving a single
        // cell of vertical clearance forces the router to drop a stub from
        // the pin up to the trunk, which is a proper Steiner T: the form a
        // schematic reader expects at a three-way node.
        //
        // The cost is one branch vertex (V16 J) in exchange for the V5
        // violation and the sideways stub; a T is the readable form of a
        // three-way join, so this is a genuine improvement rather than a
        // sideways trade. Measured on `diff_pair`: V5 1 → 0, J 0 → 1, with
        // B unchanged at 2.
        let stride = vertical_stride_cells(&q_ext, &el_ext);
        placement.elements[el].origin = GridPoint::new(
            placement.elements[el].origin.x + dx_cells,
            max_q_y + stride + TRUNK_STUB_CELLS,
        );
        pinned[el] = true;
    }
}

// ===========================================================================
// Idiom 4 — RAIL STUB column
// ===========================================================================

/// A two-terminal element with exactly one pin on a supply/ground rail
/// and the other on signal net `signal_net` (its terminal index is
/// `signal_term`). `side` is the screen direction the rail pin faces —
/// [`VertPref::Up`] for a positive supply, [`VertPref::Down`] for ground
/// or a negative rail.
///
/// A stub does not pass a signal along, it *terminates* one: a collector
/// load, an emitter resistor, a bypass capacitor, a pull-up. Schematic
/// convention draws it as a straight vertical drop from the node it
/// serves, which is what [`apply_rail_stub_columns`] enforces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RailStub {
    pub element: usize,
    pub signal_net: String,
    pub signal_term: usize,
    pub side: VertPref,
}

/// Detect every rail stub in `checked`.
///
/// Predicate, entirely structural (CLAUDE.md principle 9 — no refdes,
/// element-kind or named-topology matching): the element has exactly two
/// terminals on two *distinct* nets, exactly one of which carries a
/// [`VertPref`] (i.e. is a power, ground or negative-supply rail per
/// [`crate::net_class::vertical_prefs`]). An element with both terminals
/// on rails (a decoupling capacitor, a supply source) or neither (a
/// series element) is not a stub. Deterministic order, sorted by element
/// index.
pub(crate) fn detect_rail_stubs(checked: &CheckedNetlist) -> Vec<RailStub> {
    let prefs = crate::net_class::vertical_prefs(checked);
    let mut out = Vec::new();
    for (i, e) in checked.elements.iter().enumerate() {
        if e.nodes.len() != 2 {
            continue;
        }
        let (a, b) = (e.nodes[0].as_str(), e.nodes[1].as_str());
        if a == b {
            continue; // degenerate short
        }
        let (pa, pb) = (prefs.get(a).copied(), prefs.get(b).copied());
        let (signal_net, signal_term, side) = match (pa, pb) {
            (Some(side), None) => (b, 1, side),
            (None, Some(side)) => (a, 0, side),
            _ => continue,
        };
        out.push(RailStub {
            element: i,
            signal_net: signal_net.to_string(),
            signal_term,
            side,
        });
    }
    out
}

/// The X column a rail stub on `net` should occupy: the mean world X of
/// `net`'s **vertically-facing** pins on **multi-terminal (>= 3 pin)
/// elements**, falling back to every non-stub vertically-facing pin on
/// `net` when the net touches no such element.
///
/// Preferring the active device is what makes a collector load land on
/// the transistor's collector rather than halfway between the transistor
/// and the next series element. The discriminator is pin *count* — a
/// structural fact of the resolved symbol — so no circuit type needs
/// special-casing.
///
/// # Why only vertically-facing anchor pins (load-bearing)
///
/// A stub is drawn as a vertical drop, so it can only hang off a pin
/// that actually points up or down. Anchoring on a *horizontally*-facing
/// pin puts the stub column straight through that pin, and the net's
/// trunk then runs vertically across it — leaving the pin with no
/// outward-extending first segment, which is a **V5 violation**.
///
/// Measured, not assumed. An earlier revision anchored on any pin and
/// regressed V5 on three fixtures at once: `common_emitter` 0 -> 2
/// (`Q1`'s base pin, angle 180, with the `R1`/`R2` bias divider snapped
/// into its exact column), `multivibrator` 4 -> 5, `opamp_inverting`
/// 0 -> 1 — all the same shape, a divider column landing on a
/// horizontal base/input pin. Restricting the anchor to vertical pins
/// keeps every collector/emitter stub (the cases this idiom exists for,
/// and the ones the user reported) while leaving base-fed dividers where
/// the layer seeder put them, which is where they must stay for the base
/// pin to get its horizontal first segment.
///
/// Returns `None` when `net` has no usable anchor pin — every member is
/// itself a stub on that net, or every candidate pin faces sideways. A
/// `None` anchor means "no opinion": the stub keeps its seed column.
/// The column a rail stub should occupy, plus how much authority that
/// column carries.
///
/// `strong` means the column came from a **multi-terminal (active)
/// device's** own pin — the collector/emitter/base case this idiom
/// exists for. When it is false the column is the weaker `any`-pin
/// fallback (some two-terminal neighbour on the same net), which can sit
/// anywhere on the sheet.
///
/// `outward` is `0` when `x` is a column to occupy directly (the anchor
/// pin faces up or down, so the stub drops straight through it). It is
/// `+1` / `-1` when the anchor pin faces **sideways**: the stub cannot
/// share that pin's column without robbing it of an outward-extending
/// first segment (V5), so it takes a column one stride along the pin's
/// outward direction and reaches the pin with a short horizontal run.
/// The caller owns the stride because it depends on the *stub's* own
/// resolved extent, which this function does not see.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RailStubAnchor {
    pub x: f64,
    pub strong: bool,
    pub outward: i32,
}

pub(crate) fn rail_stub_anchor_x(
    placement: &Placement,
    checked: &CheckedNetlist,
    stubs: &[RailStub],
    net: &str,
) -> Option<RailStubAnchor> {
    let mut multi: Vec<f64> = Vec::new();
    let mut any: Vec<f64> = Vec::new();
    // Sideways-facing pins on multi-terminal (active) devices, as
    // `(pin x, outward sign)`. See `RailStubAnchor::outward`.
    let mut multi_sideways: Vec<(f64, i32)> = Vec::new();
    for (i, e) in checked.elements.iter().enumerate() {
        // A stub is never an anchor for the net it stubs off.
        if stubs.iter().any(|s| s.element == i && s.signal_net == net) {
            continue;
        }
        let Some(ti) = e.nodes.iter().position(|n| n == net) else {
            continue;
        };
        let Some(want) = e.pin_mapping.get(ti) else {
            continue;
        };
        let pe = &placement.elements[i];
        // Vertically-facing pins only (see the doc comment above).
        let Some(pin) = e
            .symbol
            .pins_in(pe.orientation)
            .into_iter()
            .find(|p| &p.number == want)
        else {
            continue;
        };
        let Some(x) = world_pin_x_of(pe, &e.symbol, net) else {
            continue;
        };
        if pin.angle % 180 == 0 {
            // Faces left/right — the stub cannot share this pin's column
            // (that robs the pin of its outward first segment, V5), but
            // an *active* device's sideways pin still says where the
            // node lives. Record it with the pin's outward direction so
            // the caller can seat the stub one stride to that side.
            // `pins_in` yields WORLD-OUTWARD angles, so 0 => +x.
            if e.nodes.len() >= 3 {
                let sign = if pin.angle % 360 == 0 { 1 } else { -1 };
                multi_sideways.push((x, sign));
            }
            continue;
        }
        if e.nodes.len() >= 3 {
            multi.push(x);
        }
        any.push(x);
    }
    // Priority: an active device's own vertical pin (share its column) >
    // an active device's sideways pin (one stride outward of it) > the
    // weak `any`-pin fallback (an arbitrary two-terminal neighbour, which
    // can sit anywhere on the sheet).
    if !multi.is_empty() {
        #[allow(clippy::cast_precision_loss)] // pin counts are tiny.
        let mean = multi.iter().sum::<f64>() / multi.len() as f64;
        return Some(RailStubAnchor {
            x: mean,
            strong: true,
            outward: 0,
        });
    }
    // A node carrying stubs on BOTH sides is a divider THROUGH the node:
    // the two groups already share one column (each takes the anchor
    // outright — see `apply_rail_stub_columns`) and the node is tapped
    // off that column. There is nothing to reach from one stride away,
    // so a sideways pin has no opinion to offer about such a node, and
    // offering one only perturbs the divider. Declining here restores
    // exactly the behaviour this shape had before sideways anchors
    // existed; snapping the divider onto the sideways pin's own column
    // is the variant already measured and rejected above.
    let node_has_both_sides = {
        let mut up = false;
        let mut down = false;
        for s in stubs.iter().filter(|s| s.signal_net == net) {
            match s.side {
                VertPref::Up => up = true,
                VertPref::Down => down = true,
            }
        }
        up && down
    };
    if !node_has_both_sides && !multi_sideways.is_empty() {
        #[allow(clippy::cast_precision_loss)] // pin counts are tiny.
        let mean =
            multi_sideways.iter().map(|(x, _)| *x).sum::<f64>() / multi_sideways.len() as f64;
        // Pins facing opposite ways cancel; only a consistent direction
        // tells us which side is "outward" for the whole group.
        let sum: i32 = multi_sideways.iter().map(|(_, s)| *s).sum();
        if sum != 0 {
            return Some(RailStubAnchor {
                x: mean,
                strong: true,
                outward: sum.signum(),
            });
        }
    }
    if any.is_empty() {
        return None;
    }
    #[allow(clippy::cast_precision_loss)] // pin counts are tiny.
    let mean = any.iter().sum::<f64>() / any.len() as f64;
    Some(RailStubAnchor {
        x: mean,
        strong: false,
        outward: 0,
    })
}

/// Resolve a [`RailStubAnchor`] into the actual X column the given stub
/// group should occupy.
///
/// For a column anchor (`outward == 0`) that is just the anchor X. For a
/// **sideways** anchor it is one stride along the anchor pin's outward
/// direction: the stub cannot stand in that pin's column without robbing
/// it of an outward-extending first segment (V5), so it seats itself
/// beside the pin and reaches it with a short horizontal run — the
/// conventional drawing of a bias resistor feeding a transistor base.
///
/// The stride is geometry-derived, never a tuned constant: the widest
/// group member's own resolved half-extent facing the pin, plus
/// [`crate::MIN_CLEARANCE_MM`], snapped up to the grid. That is the
/// smallest offset at which the stub's body clears the pin's connection
/// point, so the run in is as short as the symbols allow.
///
/// **Both** the seed pass ([`apply_rail_stub_columns`]) and the SA
/// objective (`cost::rail_stub_alignment`) resolve the anchor through
/// this one function. A seed-time target and a refine-time target that
/// disagree let the refiner silently undo the seed — the ADR-14
/// single-source lesson.
pub(crate) fn anchored_column_x(
    placement: &Placement,
    checked: &CheckedNetlist,
    anchor: RailStubAnchor,
    members: &[usize],
) -> f64 {
    if anchor.outward == 0 {
        return anchor.x;
    }
    let mut reach_mm = 0.0_f64;
    for &el in members {
        let ext = world_extent(
            &checked.elements[el].symbol,
            placement.elements[el].orientation,
            None,
        );
        let toward_pin = if anchor.outward > 0 {
            -ext.min_x
        } else {
            ext.max_x
        };
        reach_mm = reach_mm.max(toward_pin);
    }
    let cells = crate::mm_up_to_cells(reach_mm + crate::MIN_CLEARANCE_MM);
    anchor.x + f64::from(anchor.outward * cells) * GridPoint::STEP_MM
}

/// Move every unpinned rail stub into the column of the node it
/// terminates, so the stub hangs straight off that node instead of
/// jogging sideways to reach it.
///
/// Stubs are grouped by `(signal net, side)`. A group is spread
/// symmetrically about the anchor column at a geometry-derived
/// horizontal stride, so two stubs on the same node and the same side
/// (an emitter resistor and its bypass capacitor) sit side by side at a
/// legal spacing instead of stacking into each other. Stubs on the same
/// node but opposite sides (a bias divider's top and bottom resistor)
/// each take the anchor column outright — the conventional single-column
/// divider — because they are separated vertically by
/// [`crate::cost::rail_direction`].
///
/// Only X changes: which side of the device a stub falls on is
/// `cost::rail_direction`'s concern, and the stub's Y is left exactly as
/// the band seeder placed it. Elements
/// already `pinned` by a user `align` / `place` directive, by V7
/// symmetry, or by an earlier idiom are skipped, so an explicit
/// annotation always wins. Stubs are *not* pinned by this pass — unlike
/// the divider and shared-centre idioms it emits a better starting
/// column and then lets the SA refine, because a stub column is a
/// preference rather than a structural invariant.
///
/// `[crate::cost::rail_stub_alignment]` scores the same property, so the
/// seed and the objective agree on where a stub belongs (the ADR-14
/// single-source lesson: a seed-time placement and a refine-time score
/// that disagree let the refiner silently undo the seed).
pub(crate) fn apply_rail_stub_columns(
    placement: &mut Placement,
    pinned: &[bool],
    sym_released: &[bool],
    checked: &CheckedNetlist,
    stubs: &[RailStub],
) {
    // Group by (signal net, side), preserving element-index order.
    let mut groups: HashMap<(&str, VertPref), Vec<&RailStub>> = HashMap::new();
    for s in stubs {
        groups
            .entry((s.signal_net.as_str(), s.side))
            .or_default()
            .push(s);
    }
    let mut keys: Vec<(&str, VertPref)> = groups.keys().copied().collect();
    keys.sort_unstable_by_key(|(net, side)| (*net, matches!(side, VertPref::Down)));

    for key in keys {
        let members = &groups[&key];
        let Some(anchor) = rail_stub_anchor_x(placement, checked, stubs, key.0) else {
            continue;
        };
        // A group whose V7 symmetry pin was released for this pass moves
        // ONLY on a strong (active-device) anchor.
        //
        // V7 owns the mirror relation, and the caller released the pin
        // because a collector-load column is a better opinion than the
        // seeded one. The weak `any`-pin fallback is not a better
        // opinion: measured on `multivibrator`, `RB1`'s only vertical
        // anchor on net `b1` is the cross-coupling capacitor `C2`, which
        // lives above the OTHER transistor — the fallback dragged `RB1`
        // 15 mm across the sheet into `Q2`'s column and stretched `b1`
        // into a full-width diagonal. So a released group keeps the
        // symmetric column V7 gave it unless the active device it
        // terminates actually presents a vertical pin.
        //
        // Scoped to released groups on purpose: the fallback stays live
        // everywhere it was live before, so no non-symmetric fixture
        // changes (`common_emitter`'s `R1`/`R2` bias divider still snaps
        // to `CIN`'s column).
        let member_idx: Vec<usize> = members.iter().map(|s| s.element).collect();
        let anchor_x = anchored_column_x(placement, checked, anchor, &member_idx);
        if !anchor.strong && members.iter().any(|s| sym_released[s.element]) {
            continue;
        }
        // A group containing ANY pinned member is left entirely alone.
        //
        // The pin means something stronger than this heuristic already
        // decided that column — a position-cache hint (ADR-4), V7
        // symmetry, or an earlier idiom. Re-spreading the group around
        // the anchor would move the *pinned* member's neighbours out
        // from under it and, worse, silently break position stability:
        // measured on `tests/layout_cache.rs`, adding one resistor to
        // the `rc_lowpass` netlist made the new `R2` land in the exact
        // column of the already-cached `C1` (both stubs on `out`,
        // both on the ground side), because the pinned `C1` was skipped
        // without consuming its slot. Skipping the whole group keeps the
        // cached frame reproducible; the newcomer simply keeps the
        // column the layer seeder gave it, which is no worse than before
        // this idiom existed.
        if members.iter().any(|s| pinned[s.element]) {
            continue;
        }
        let movable: Vec<&RailStub> = members.clone();
        if movable.is_empty() {
            continue;
        }

        // Geometry-derived horizontal stride: the widest pair of
        // adjacent extents in the group, so no two members clip.
        let mut stride = CELL_W;
        for w in movable.windows(2) {
            let a = world_extent(
                &checked.elements[w[0].element].symbol,
                placement.elements[w[0].element].orientation,
                None,
            );
            let b = world_extent(
                &checked.elements[w[1].element].symbol,
                placement.elements[w[1].element].orientation,
                None,
            );
            let gap_mm = a.max_x + (-b.min_x) + crate::MIN_CLEARANCE_MM;
            stride = stride.max(crate::mm_up_to_cells(gap_mm));
        }

        // Spread symmetrically about the anchor column.
        let count = i32::try_from(movable.len()).unwrap_or(1);
        for (slot, s) in movable.iter().enumerate() {
            let el = s.element;
            let Some(cur_x) = world_pin_x_of(
                &placement.elements[el],
                &checked.elements[el].symbol,
                &s.signal_net,
            ) else {
                continue;
            };
            let slot_i = i32::try_from(slot).unwrap_or(0);
            // Offset in cells of this slot from the group centre.
            let offset_cells = slot_i * stride - (count - 1) * stride / 2;
            let target_x = anchor_x + f64::from(offset_cells) * GridPoint::STEP_MM;
            #[allow(clippy::cast_possible_truncation)]
            let dx_cells = ((target_x - cur_x) / GridPoint::STEP_MM).round() as i32;
            placement.elements[el].origin = GridPoint::new(
                placement.elements[el].origin.x + dx_cells,
                placement.elements[el].origin.y,
            );
        }
    }
}

/// World-frame offset (mm) of `element`'s pin on `net` from the element's
/// own origin, at orientation `orient`. `None` when the element has no
/// terminal on `net`, or the symbol has no such pin.
///
/// The symbol-frame pin `y` is **negated**: world/screen Y grows downward
/// and `pins_in` yields the KiCad symbol frame, which is the same eeschema
/// flip [`crate::world_extent`] applies (`grow(p.x, -p.y)`). Note that
/// `PlacedElement::world_pin_mm` adds it instead — a known upstream defect
/// being corrected on its own track; do not mirror it here.
fn pin_offset_world(e: &ResolvedElement, orient: Orientation, net: &str) -> Option<(f64, f64)> {
    let ti = e.nodes.iter().position(|n| n == net)?;
    let want = e.pin_mapping.get(ti)?;
    e.symbol
        .pins_in(orient)
        .into_iter()
        .find(|p| &p.number == want)
        .map(|p| (p.x, -p.y))
}

/// World `(x, y)` mm of the pin of placed element `pe` (resolved as `e`)
/// that connects to `net`, in the screen frame (Y grows downward).
fn world_pin_xy_of(
    pe: &crate::PlacedElement,
    e: &ResolvedElement,
    net: &str,
) -> Option<(f64, f64)> {
    let (dx, dy) = pin_offset_world(e, pe.orientation, net)?;
    let (ox, oy) = pe.origin.to_mm();
    Some((ox + dx, oy + dy))
}

/// The grid origin that puts `e`'s pin on `net` at world `target` (mm)
/// when the element is drawn at `orient`, snapped to the schematic grid
/// (CLAUDE.md "everything lands on the KiCad schematic grid").
fn origin_placing_pin_at(
    e: &ResolvedElement,
    orient: Orientation,
    net: &str,
    target: (f64, f64),
) -> Option<GridPoint> {
    let (dx, dy) = pin_offset_world(e, orient, net)?;
    #[allow(clippy::cast_possible_truncation)]
    Some(GridPoint::new(
        ((target.0 - dx) / GridPoint::STEP_MM).round() as i32,
        ((target.1 - dy) / GridPoint::STEP_MM).round() as i32,
    ))
}

/// The X column `net`'s rail stubs occupy **as placed** — the mean world X
/// of the stubs' own pins on `net`, read after
/// [`apply_rail_stub_columns`] has had its say. `None` when the net
/// carries no stub.
///
/// This reads the placement rather than re-deriving a column from
/// [`rail_stub_anchor_x`] on purpose: the divider members are the divider
/// idiom's property, so the only column that is *true* is the one they
/// actually stand in.
fn stub_column_x(
    placement: &Placement,
    checked: &CheckedNetlist,
    stubs: &[RailStub],
    net: &str,
) -> Option<f64> {
    let mut sum = 0.0_f64;
    let mut n = 0_u32;
    for s in stubs.iter().filter(|s| s.signal_net == net) {
        if let Some((x, _)) = world_pin_xy_of(
            &placement.elements[s.element],
            &checked.elements[s.element],
            net,
        ) {
            sum += x;
            n += 1;
        }
    }
    (n > 0).then(|| sum / f64::from(n))
}

/// The world Y of the wire `net` sends to the device it drives — the Y of
/// `net`'s pin on the element with the most terminals that is neither
/// `series` (the element under construction) nor one of the node's rail
/// stubs.
///
/// For a bias divider that is the transistor base / FET gate the tap
/// feeds, i.e. the height at which the node's horizontal run is already
/// drawn. Landing the series element's downstream pin there makes that run
/// straight. `None` when the node drives nothing with three or more pins,
/// in which case the caller has no Y to honour and declines.
fn node_outgoing_wire_y(
    placement: &Placement,
    checked: &CheckedNetlist,
    stubs: &[RailStub],
    net: &str,
    series: usize,
) -> Option<f64> {
    let mut best: Option<(usize, f64)> = None;
    for (j, e) in checked.elements.iter().enumerate() {
        if j == series || e.nodes.len() < 3 || !e.nodes.iter().any(|n| n == net) {
            continue;
        }
        if stubs.iter().any(|s| s.element == j) {
            continue;
        }
        let Some((_, y)) = world_pin_xy_of(&placement.elements[j], e, net) else {
            continue;
        };
        if best.is_none_or(|(pins, _)| e.nodes.len() > pins) {
            best = Some((e.nodes.len(), y));
        }
    }
    best.map(|(_, y)| y)
}

/// Which construction [`apply_series_horizontal`] applies to an accepted
/// series element. The three variants are the case the pass has always
/// accepted, plus one for each of its two historical *declines* — F1
/// replaces each decline with a narrower construction rather than widening
/// the accepting one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Construction {
    /// The shipping case: the downstream node carries rail stub(s) on
    /// exactly ONE rail side, which are re-columned to drop straight off
    /// the output node.
    Recolumn,
    /// F1 case (a), behind [`Placer::terminal_net_series`] — an endpoint
    /// net is *terminal*, so there is nothing on it to re-column and
    /// nothing to collide with. The position half is a pin-anchored
    /// re-seat of the element itself.
    TerminalNet,
    /// F1 case (b), behind [`Placer::divider_node_series`] — the
    /// downstream node is a bias divider *through* the node. Orient onto
    /// the divider's column at the node's outgoing-wire Y, and never touch
    /// the divider itself.
    DividerNode,
}

/// Draw series signal elements horizontally on the flow lane, upstream
/// pin left, with their downstream shunts re-columned onto the output node
/// — dropping **beneath** it for a ground / negative-rail stub and rising
/// **above** it for a positive-supply stub (MEMORY "flow-orientation wall";
/// ADR-15 Stage-5 post-mortem; the ADR-15 §1.3 joint position+orientation
/// hypothesis).
///
/// The stub *side* ([`RailStub::side`]) governs both halves of the
/// re-column — the direction of the Y step and the shunt's rail-pin facing
/// ([`rail_facing_orientation`]). They must agree: a `+12V` bias resistor
/// dropped below its node with its rail pin facing down draws the supply
/// glyph *under* the body, which is both wrong-looking and a V14 violation
/// this pass then **pins** past every stage that enforces V14 (see
/// [`v14_permits`]).
///
/// A **series signal element** is the ADR-15 role-model "series" role,
/// derived structurally (pin count + net class, principle 9): 2-terminal,
/// both nodes Signal-class (neither a rail, neither ground). Such an
/// element lies on the signal path and reads best drawn *horizontally*,
/// upstream pin at the lower X so the signal runs left→right.
///
/// The three prior flow-orientation attempts changed **orientation
/// against an independently-chosen position** and regressed Tier-1: on
/// `rc_lowpass_ports` the horizontal element's `out` label collided with a
/// shunt capacitor still sitting to its *left* (ADR-15 Stage-5 "axis is
/// only half the constraint"). This pass changes **position and
/// orientation together**: the element is oriented horizontal AND every
/// downstream shunt is re-columned onto the element's downstream pin, so
/// the shunt hangs straight below the output node instead of beside it.
/// That is the layout the real router draws cleanly (verified end to end
/// on `rc_lowpass_ports`: a single straight `out` wire, the `out` global
/// label clear of C1's body).
///
/// Pins the element and every shunt it re-columns, so the SA
/// ([`crate::solver`]) and phase 4.5 ([`kicad-emitter`], via the same
/// `pinned` mask recomputed in [`crate::refinement_meta`]) cannot revert
/// the choice — the mechanism ADR-18's channel-row work relies on.
///
/// Runs AFTER [`apply_rail_stub_columns`] (so it overrides the stub's weak
/// `any`-pin column with the true downstream-pin column) and BEFORE
/// [`crate::pick_orientations`] (which skips pinned elements). Skips any
/// element already pinned by a stronger opinion (user `align`/`place`, a
/// cache hint, V7 symmetry, a divider).
// One loop over the elements deciding, for each, WHICH construction it
// gets and then applying it. The three constructions share the role test,
// the flow direction and the V14 gate, so splitting them into separate
// passes would duplicate all three and let them drift.
#[allow(clippy::too_many_lines)]
pub(crate) fn apply_series_horizontal(
    placement: &mut Placement,
    pinned: &mut [bool],
    checked: &CheckedNetlist,
    allowed: &[Vec<Orientation>],
    variant: Placer,
) {
    // Extra cells beyond a body-clean vertical stride, so a downstream
    // shunt's pin sits far enough from the series pin that the shared-node
    // port label prefers the series pin (V13 pin-text).
    //
    // Down side (a ground / negative-rail shunt dropping below the node):
    // measured on `rc_lowpass_ports` — the body-clean stride alone leaves a
    // 1-cell pin gap (label lands on the shunt, colliding with its pin
    // number); +2 cells clears it.
    const SHUNT_LABEL_MARGIN_DOWN_CELLS: i32 = 2;
    // Up side (a positive-supply shunt rising above the node): measured
    // separately, on `rc_phase_shift` / `shunt_feedback_amp` — the two
    // fixtures with an Up-side re-column. Carrying the Down figure over
    // untested would have been an assumption, and the Down figure was
    // itself a measurement, not a principle. It is NOT the same number:
    // swept over 0..=5 with the whole verifier suite, +2 (the Down value)
    // is the ONE value that collides the shared-node label with a
    // neighbouring pin's number text — V13 pin-text AND rendered ink, both
    // Tier 1. +3 clears both and is also the only V16-non-increasing
    // choice besides +4 (ADR-16 protocol), with fewer Tier-2 rises than
    // +4. The sweep table is in the commit message.
    const SHUNT_LABEL_MARGIN_UP_CELLS: i32 = 3;
    let classes = classify_nets(checked);
    let depth = signal_net_depth(checked, &classes, variant);
    let stubs = detect_rail_stubs(checked);

    let is_signal = |n: &str| -> bool {
        n != "0" && !matches!(classes.get(n), Some(NetClass::Power | NetClass::Ground))
    };

    // A **terminal** net is one the drawing has nothing else to hang off,
    // by either of two structural tests (CLAUDE.md principle 9 — no refdes
    // and no element-kind matching):
    //
    //  * it carries a declared `*@port`, the user's own statement that this
    //    net is a circuit boundary (`layers.rs` reads the same field for
    //    the left/right X bias); or
    //  * exactly one element touches it, so once that element is placed the
    //    net is a bare wire out to a label. `;@ ignore`d loads are already
    //    absent from `checked.elements`, which is what makes `out` a leaf
    //    on the amplifier fixtures.
    let declared_ports: HashSet<&str> = checked.ports.iter().map(|p| p.net.as_str()).collect();
    let mut net_degree: HashMap<&str, usize> = HashMap::new();
    for el in &checked.elements {
        let mut seen: HashSet<&str> = HashSet::new();
        for n in &el.nodes {
            if seen.insert(n.as_str()) {
                *net_degree.entry(n.as_str()).or_insert(0) += 1;
            }
        }
    }
    let is_terminal = |n: &str| -> bool {
        declared_ports.contains(n) || net_degree.get(n).copied().unwrap_or(0) <= 1
    };

    for i in 0..checked.elements.len() {
        if pinned[i] {
            continue;
        }
        let e = &checked.elements[i];
        // Series role: 2-terminal passive, both nodes on the signal path.
        // A power source and a rail stub (one node on a rail) are excluded
        // by construction here — they are handled by their own idioms and
        // stay vertical.
        if !is_two_terminal_passive(e) || matches!(e.role, ElementRole::Power(_)) {
            continue;
        }
        let (na, nb) = (e.nodes[0].as_str(), e.nodes[1].as_str());
        if na == nb || !is_signal(na) || !is_signal(nb) {
            continue;
        }
        // Upstream = the node of lower flow-depth (closer to an input).
        // Require BOTH nodes reachable from an input boundary with a
        // strict order: a tie, or either node off the input-rooted flow
        // graph, gives no direction to honour, so leave the element to the
        // general orientation chooser (conservative — avoids forcing a
        // horizontal facing on an element whose flow direction is unknown).
        let (up, down) = match (depth.get(na).copied(), depth.get(nb).copied()) {
            (Some(x), Some(y)) if x < y => (na, nb),
            (Some(x), Some(y)) if x > y => (nb, na),
            _ => continue,
        };
        let mut down_up_side = false;
        let mut down_down_side = false;
        for s in stubs.iter().filter(|s| s.signal_net == down) {
            match s.side {
                VertPref::Up => down_up_side = true,
                VertPref::Down => down_down_side = true,
            }
        }
        let construction = if down_up_side && down_down_side {
            // BOTH-SIDES GUARD — a downstream node carrying rail stubs on
            // BOTH the up and down rail sides is a divider THROUGH the node
            // (e.g. `common_emitter`'s R1/R2 bias divider on the base, or
            // `named_rails`'s pull-up/pull-down pair on `out`), not a shunt
            // to drop beneath an output. Re-columning it perturbs geometry
            // the divider idiom owns, so the shipping pass declines —
            // mirrors `rail_stub_anchor_x`'s `node_has_both_sides`.
            //
            // F1 case (b) keeps the hands-off half and drops the rest: the
            // divider is still read-only, but the series element is oriented
            // ONTO its column instead of being abandoned to the general
            // chooser.
            if variant.divider_node_series() {
                Construction::DividerNode
            } else {
                continue;
            }
        } else if down_up_side || down_down_side {
            Construction::Recolumn
        } else if variant.terminal_net_series() && (is_terminal(up) || is_terminal(down)) {
            // SHUNT-BEARING GUARD, relaxed by F1 case (a). Re-orienting a
            // series element horizontal with nothing to jointly place
            // re-basins fixtures that have nothing to drop (measured:
            // `common_emitter` COUT forced, B 4→7; `opamp_inverting`
            // regressed), which is why the shipping pass requires a
            // downstream shunt to anchor the re-column.
            //
            // A *terminal* net removes that hazard instead of widening past
            // it: nothing else touches the net, so there is nothing to
            // re-column and nothing for the element to swing into. The
            // position half survives as a pin-anchored re-seat below.
            Construction::TerminalNet
        } else {
            // No shunt to re-column and no terminal net to swing into — no
            // anchor, so leave the element to the general chooser
            // (conservative, and the shipping behaviour).
            continue;
        };
        // Series elements are pure-signal 2-pin passives → V14 allows all
        // eight orientations, so any horizontal one is V14-legal. Checked,
        // not assumed: see `v14_permits`.
        let Some(orient) = horizontal_flow_orientation(&placement.elements[i], &e.symbol, up, down)
        else {
            continue;
        };
        if !v14_permits(allowed, i, orient, &e.refdes) {
            continue;
        }
        // Both F1 constructions also need a POSITION, and it must be
        // derived BEFORE anything is mutated: orienting and pinning an
        // element whose target then turns out to be underivable would
        // freeze a bare rotation at a position chosen for the old pose —
        // exactly the orientation-only change ADR-15 Stage 5 measured the
        // cost of.
        let origin = match construction {
            Construction::Recolumn => placement.elements[i].origin,
            Construction::TerminalNet => {
                // Hold the pin on the element's INTERIOR side at its current
                // world position, so the body swings out into the empty
                // half-plane the terminal net is, rather than rotating about
                // its own origin into whatever sits beside it. The
                // terminal-side pin is deliberately free: it reaches a
                // label, not a neighbour. When BOTH ends are terminal there
                // is no interior side, and upstream is the conventional
                // anchor (signal runs left→right from it).
                let anchor = if is_terminal(up) && !is_terminal(down) {
                    down
                } else {
                    up
                };
                let Some(at) = world_pin_xy_of(&placement.elements[i], e, anchor) else {
                    continue;
                };
                let Some(o) = origin_placing_pin_at(e, orient, anchor, at) else {
                    continue;
                };
                o
            }
            Construction::DividerNode => {
                // Land the downstream pin ON the divider's own column, at
                // the Y of the wire the node already sends to the device it
                // drives. The divider members are read here and never
                // written — their column and their stack belong to the
                // divider / rail-stub idioms.
                let Some(x) = stub_column_x(placement, checked, &stubs, down) else {
                    continue;
                };
                let Some(y) = node_outgoing_wire_y(placement, checked, &stubs, down, i) else {
                    continue;
                };
                let Some(o) = origin_placing_pin_at(e, orient, down, (x, y)) else {
                    continue;
                };
                o
            }
        };

        placement.elements[i].origin = origin;
        placement.elements[i].orientation = orient;
        pinned[i] = true;

        if construction != Construction::Recolumn {
            // Neither F1 construction re-columns anything: a terminal net
            // has no members to move, and a divider node's members are the
            // divider idiom's property.
            continue;
        }

        // Re-column every downstream shunt onto this element's downstream
        // pin so it drops straight beneath the output node, AND drop it far
        // enough below that the shared-node port label attaches to THIS
        // element's downstream pin rather than to the shunt's own top pin.
        // The latter is load-bearing: with the shunt crammed one cell under
        // the node, the emitter anchors the `out` global label on the
        // shunt's top pin, whose pin-number text the label then overlaps
        // (V13 pin-text, Tier 1). A clean drop moves the label up onto this
        // element's pin, clear of the shunt's pin number.
        let mut probe = placement.elements[i].clone();
        probe.orientation = orient;
        let Some(down_x_mm) = world_pin_x_of(&probe, &e.symbol, down) else {
            continue;
        };
        #[allow(clippy::cast_possible_truncation)]
        let down_x = (down_x_mm / GridPoint::STEP_MM).round() as i32;
        let series_ext = world_extent(&e.symbol, orient, None);
        for s in stubs.iter().filter(|s| s.signal_net == down) {
            if pinned[s.element] {
                continue;
            }
            let se = &checked.elements[s.element];
            // Orient the shunt V14-correct: its rail pin faces the band its
            // rail lives in — screen-down for ground / a negative rail (the
            // glyph hangs below), screen-**up** for a positive supply (the
            // glyph sits above). Pinning skips `pick_orientations`, which
            // would otherwise choose this, so we must set it here.
            let s_orient = rail_facing_orientation(se, down, s.side)
                .unwrap_or(placement.elements[s.element].orientation);
            if !v14_permits(allowed, s.element, s_orient, &se.refdes) {
                continue;
            }
            placement.elements[s.element].orientation = s_orient;
            let shunt_ext = world_extent(&se.symbol, s_orient, None);
            // World Y grows *downward* (`world_extent` applies the eeschema
            // y-flip; `vertical_stride_cells(upper, lower)` takes the
            // smaller-world-Y element first). A Down-side stub drops BELOW
            // the series element — series is the upper, `+stride`. An
            // Up-side stub rises ABOVE it — the stub is the upper, so the
            // stride is measured the other way round and applied as
            // `-stride`. Getting the argument order wrong here would silently
            // under-space the pair whenever the two extents differ.
            let (stride, sign) = match s.side {
                VertPref::Down => (
                    vertical_stride_cells(&series_ext, &shunt_ext) + SHUNT_LABEL_MARGIN_DOWN_CELLS,
                    1,
                ),
                VertPref::Up => (
                    vertical_stride_cells(&shunt_ext, &series_ext) + SHUNT_LABEL_MARGIN_UP_CELLS,
                    -1,
                ),
            };
            let new_y = placement.elements[i].origin.y + sign * stride;
            placement.elements[s.element].origin = GridPoint::new(down_x, new_y);
            pinned[s.element] = true;
        }
    }
}

/// The V14 consistency gate for a pass that **pins** what it orients.
///
/// CLAUDE.md's *consistency requirement*: a property enforced as a hard
/// constraint at one stage must be hard at **every** stage that can move
/// the element. [`apply_series_horizontal`] pins, so its poses survive
/// `pick_orientations`, the SA rotate move and phase 4.5 untouched — the
/// three stages that would otherwise filter on
/// [`crate::orient::allowed_orientations`]. Pinning an orientation outside
/// that set therefore freezes a V14 violation past every enforcer, which
/// is exactly how a `+12V` bias resistor once shipped upside-down.
///
/// So the same filter binds here: `false` means the caller declines to
/// pin and leaves the element to the general chooser (which *will* apply
/// V14) rather than freezing a forbidden pose. The `debug_assert!` makes
/// the same condition loud in tests and CI, where a decline is a defect in
/// this pass's own reasoning, not a legitimate outcome.
fn v14_permits(allowed: &[Vec<Orientation>], i: usize, orient: Orientation, refdes: &str) -> bool {
    let ok = allowed.get(i).is_some_and(|set| set.contains(&orient));
    debug_assert!(
        ok,
        "apply_series_horizontal would pin {refdes} at {orient:?}, which V14 \
         forbids (allowed: {:?}). A pinned pose bypasses every stage that \
         enforces V14 — see CLAUDE.md 'consistency requirement'.",
        allowed.get(i)
    );
    ok
}

/// Pick the horizontal orientation of a vertical-native 2-pin passive that
/// places the `up` (upstream) pin at the lower world X. `None` if the
/// element has no terminal on both nets or no horizontal orientation
/// separates them.
fn horizontal_flow_orientation(
    pe: &crate::PlacedElement,
    symbol: &Symbol,
    up: &str,
    down: &str,
) -> Option<Orientation> {
    let mut best: Option<(Orientation, f64)> = None;
    for &o in &Orientation::ALL {
        // A vertical-native 2-pin passive is horizontal exactly at R90 /
        // R270. (R0 / R180 keep it vertical.)
        if !matches!(o.rotation, Rotation::R90 | Rotation::R270) {
            continue;
        }
        let mut probe = pe.clone();
        probe.orientation = o;
        let up_x = world_pin_x_of(&probe, symbol, up)?;
        let down_x = world_pin_x_of(&probe, symbol, down)?;
        if up_x < down_x && best.is_none_or(|(_, bx)| up_x < bx) {
            best = Some((o, up_x));
        }
    }
    best.map(|(o, _)| o)
}

/// Vertical orientation (R0 / R180, no mirror preferred) of a 2-pin shunt
/// that puts its **rail** pin (the pin on the non-signal node) facing the
/// screen direction its rail band lives in — the V14-correct facing
/// [`crate::pick_orientations`] would otherwise choose (it is skipped here
/// because the shunt is pinned).
///
/// `side` is the stub's [`RailStub::side`]: [`VertPref::Down`] (ground or
/// a negative rail) faces the rail pin screen-**down** so the glyph hangs
/// beneath the body; [`VertPref::Up`] (a positive supply) faces it
/// screen-**up** so the glyph sits above it. Parameterising this is
/// load-bearing — an earlier revision hard-coded the ground case, which
/// pinned a `+12V` bias resistor upside-down (glyph below the body) past
/// every stage that enforces V14. `None` if no vertical orientation faces
/// the rail pin the wanted way.
fn rail_facing_orientation(
    se: &ResolvedElement,
    signal_net: &str,
    side: VertPref,
) -> Option<Orientation> {
    let rail_ti = se.nodes.iter().position(|n| n != signal_net)?;
    let rail_pin = se.pin_mapping.get(rail_ti)?;
    // `pins_in` yields transformed (screen-frame) angles: 90 = down,
    // 270 = up (see `crate::orient::screen_facing`).
    let want_angle = match side {
        VertPref::Up => 270,
        VertPref::Down => 90,
    };
    for &o in &Orientation::ALL {
        if !matches!(o.rotation, Rotation::R0 | Rotation::R180) {
            continue;
        }
        if se
            .symbol
            .pins_in(o)
            .iter()
            .find(|p| &p.number == rail_pin)
            .is_some_and(|p| p.angle % 360 == want_angle)
        {
            return Some(o);
        }
    }
    None
}

/// Flow-depth of each signal net, as hop count from the input boundary.
///
/// BFS walks element net adjacency, never crossing a rail
/// (Power/Ground/`0`), so the depth orders signal nets from input to
/// output. Nets unreachable from a root are absent from the map (depth
/// unknown), and an **empty** map is the honest answer for a circuit
/// with no signal-flow root at all — `apply_series_horizontal` then
/// declines, which is correct for a rootless cycle.
///
/// `variant` selects the **root policy**, not the traversal:
///
/// * [`Placer::unified_roots`] — the whole tier ladder below is
///   replaced by [`crate::roots::signal_flow_roots`], the one policy the
///   X layering reads too. That is the point of the variant: the three
///   root divergences this function has had with `layers.rs` were all
///   *which roots exist*, never how they are walked.
/// * [`Placer::unified_depth_roots`] — the pre-unification half-step: a
///   third tier that roots at drawn sources when the first two seed
///   nothing. Retained as the scoreboard's control arm for the
///   unification.
pub(crate) fn signal_net_depth(
    checked: &CheckedNetlist,
    classes: &NetClassMap,
    variant: Placer,
) -> HashMap<String, u32> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &checked.elements {
        for a in &e.nodes {
            for b in &e.nodes {
                if a != b {
                    adj.entry(a.as_str()).or_default().push(b.as_str());
                }
            }
        }
    }
    // Under a unified-roots placer the three tiers below are replaced
    // wholesale by the one policy `layers.rs` reads as well. The tier
    // that fires (and the fact that exactly one does) is decided there;
    // here we only consume the depth-0 nets it names.
    let unified = variant
        .unified_roots()
        .then(|| crate::roots::signal_flow_roots(checked, classes, variant));
    let mut depth: HashMap<String, u32> = HashMap::new();
    let mut frontier: Vec<&str> = Vec::new();
    if let Some(r) = &unified {
        for net in &r.nets {
            if depth.insert(net.clone(), 0).is_none() {
                frontier.push(net.as_str());
            }
        }
    } else {
        for p in &checked.ports {
            if matches!(p.dir, PortDir::Input) && depth.insert(p.net.clone(), 0).is_none() {
                frontier.push(p.net.as_str());
            }
        }
        // Fallback root: no `*@port …=input` was declared, so the flow graph
        // has no seed and every series element would be left directionless
        // (an un-ported RC filter draws differently from its `*@port`-annotated
        // twin). Seed instead from *leaf input nets* recognised by NAME
        // convention — a Signal-class net whose name matches
        // `layers::boundary_net_role` (`in`/`vin`/`input`, channel digits
        // stripped) and which is touched by exactly one element, i.e. a
        // boundary of the signal chain. This mirrors the identical backstop
        // `layers::no_source_fallback` already applies for X-layer ordering, so
        // depth and layer agree. Only fires when the port loop seeded nothing,
        // so a fixture that DOES declare an input port is byte-unchanged.
        if frontier.is_empty() {
            let mut net_members: HashMap<&str, usize> = HashMap::new();
            for e in &checked.elements {
                let mut seen: Vec<&str> = Vec::new();
                for n in &e.nodes {
                    if !seen.contains(&n.as_str()) {
                        seen.push(n.as_str());
                        *net_members.entry(n.as_str()).or_default() += 1;
                    }
                }
            }
            for e in &checked.elements {
                for n in &e.nodes {
                    let net = n.as_str();
                    if net == "0"
                        || matches!(classes.get(net), Some(NetClass::Power | NetClass::Ground))
                    {
                        continue;
                    }
                    if net_members.get(net).copied() == Some(1)
                        && matches!(crate::roots::boundary_net_role(net), Some(PortDir::Input))
                        && depth.insert(net.to_string(), 0).is_none()
                    {
                        frontier.push(net);
                    }
                }
            }
        }
        // Third root tier (`--placer=flow-seed-v2` / `-v3`) — **drawn
        // sources**. The two tiers above are the whole reason this function
        // and `layers.rs` can disagree: the layering's *principled* path
        // roots at `layers::is_signal_source` (a `VoltageSrc`/`CurrentSrc`
        // that is not `;@ power`-tagged), and only its no-source *fallback*
        // is what the leaf-name backstop above mirrors. A netlist whose
        // stimulus is DRAWN therefore takes the layering's rooted-DAG path
        // while this map comes back empty — and an empty map makes
        // `apply_series_horizontal` decline every element, since it requires
        // a strict depth order across a series element's two nodes.
        //
        // `lc_ladder_lpf` is exactly that netlist: `VIN src 0` is drawn, `in`
        // is touched by three elements so the leaf backstop rejects it, and
        // the ladder's four series elements are left unpinned for the SA to
        // rotate apart. Rooting at the source's Signal-class nets gives
        // `src=0 → in=1 → n2=2 → n3=3 → out=4`.
        //
        // Last tier, deliberately: it fires only when the port loop AND the
        // leaf backstop both seeded nothing, so every fixture that declares
        // an input port or owns a leaf input net is byte-unchanged.
        if frontier.is_empty() && variant.unified_depth_roots() {
            for e in &checked.elements {
                if !matches!(e.kind, ElementKind::VoltageSrc | ElementKind::CurrentSrc)
                    || matches!(e.role, ElementRole::Power(_))
                {
                    continue;
                }
                for n in &e.nodes {
                    let net = n.as_str();
                    if net == "0"
                        || matches!(classes.get(net), Some(NetClass::Power | NetClass::Ground))
                    {
                        continue;
                    }
                    if depth.insert(net.to_string(), 0).is_none() {
                        frontier.push(net);
                    }
                }
            }
        }
    }
    let mut d = 0_u32;
    while !frontier.is_empty() {
        let mut next: Vec<&str> = Vec::new();
        for u in &frontier {
            let Some(vs) = adj.get(*u) else { continue };
            for v in vs {
                if *v == "0" || matches!(classes.get(*v), Some(NetClass::Power | NetClass::Ground))
                {
                    continue;
                }
                if !depth.contains_key(*v) {
                    depth.insert((*v).to_string(), d + 1);
                    next.push(*v);
                }
            }
        }
        frontier = next;
        d += 1;
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use kicad_symbols::Library;
    use spice_diagnostics::FileId;
    use spice_policy::check;

    fn fixture_library() -> &'static Library {
        static LIB: OnceLock<Library> = OnceLock::new();
        LIB.get_or_init(|| {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let dir = manifest
                .parent()
                .and_then(std::path::Path::parent)
                .expect("workspace root")
                .join("crates/kicad-symbols/tests/fixtures");
            let device = Library::from_file(dir.join("Device.kicad_sym"))
                .expect("load Device fixture library");
            let spice = Library::from_file(dir.join("Simulation_SPICE.kicad_sym"))
                .expect("load Simulation_SPICE fixture library");
            device.merge(spice)
        })
    }

    fn checked_of(src: &str) -> CheckedNetlist {
        let parsed = spice_parser::parse(src, FileId(0))
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let (checked, _warns) = check(resolved).expect("policy check failed");
        checked
    }

    /// Map detected pairs back to sorted refdes pairs for index-order
    /// independent assertions.
    fn refdes_pairs(checked: &CheckedNetlist, pairs: &[DividerPair]) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = pairs
            .iter()
            .map(|p| {
                let a = checked.elements[p.upper].refdes.clone();
                let b = checked.elements[p.lower].refdes.clone();
                if a <= b { (a, b) } else { (b, a) }
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn detects_simple_divider() {
        let src = "\
resistor divider
*@symbol Device:R for=R*
V1 in 0 DC 5 ;@ power=+5V
R1 in mid 10k
R2 mid 0 10k
.end
";
        let checked = checked_of(src);
        let pairs = detect_dividers(&checked, Placer::default());
        assert_eq!(
            refdes_pairs(&checked, &pairs),
            vec![("R1".to_string(), "R2".to_string())]
        );
    }

    /// A tap node with a third consumer is NOT a clean divider midpoint:
    /// the load on `mid` raises its degree above 2, so we decline.
    #[test]
    fn loaded_tap_is_not_a_divider() {
        let src = "\
loaded divider tap
*@symbol Device:R for=R*
*@symbol Device:C for=C*
V1 in 0 DC 5 ;@ power=+5V
R1 in mid 10k
R2 mid 0 10k
C1 mid 0 100n
.end
";
        let checked = checked_of(src);
        let pairs = detect_dividers(&checked, Placer::default());
        assert!(
            pairs.is_empty(),
            "tap with a third consumer must not be a divider, got {pairs:?}"
        );
    }

    /// Two resistors in *parallel* (sharing both nets) are not a series
    /// divider — the shared-tap test must reject them.
    #[test]
    fn parallel_resistors_not_a_divider() {
        let src = "\
parallel resistors
*@symbol Device:R for=R*
R1 a b 1k
R2 a b 1k
.end
";
        let checked = checked_of(src);
        // `a` and `b` both have degree 2, but each is shared by the SAME
        // two resistors, and the outer nets collapse — declined.
        let pairs = detect_dividers(&checked, Placer::default());
        assert!(
            pairs.is_empty(),
            "parallel resistors must not be a divider, got {pairs:?}"
        );
    }

    /// A three-resistor chain yields one non-overlapping pair, not two
    /// pairs sharing the middle resistor.
    #[test]
    fn three_resistor_chain_one_pair() {
        let src = "\
three in series
*@symbol Device:R for=R*
R1 in a 1k
R2 a b 1k
R3 b 0 1k
.end
";
        let checked = checked_of(src);
        let pairs = detect_dividers(&checked, Placer::default());
        // Greedy lowest-index: tap `a` pairs (R1,R2); R2 is then used,
        // so tap `b` cannot reuse it and (R2,R3) is declined.
        assert_eq!(pairs.len(), 1, "expected exactly one pair, got {pairs:?}");
        assert_eq!(
            refdes_pairs(&checked, &pairs),
            vec![("R1".to_string(), "R2".to_string())]
        );
    }

    /// A tap node wired into a hierarchical-sheet (`.subckt`) instance
    /// port has degree > 2 even though only two resistors appear in
    /// `elements` — the sheet port is the third consumer. The detector
    /// must count sheet-instance ports and decline. This is the real
    /// `opamp_inverting` false positive: RIN/RF meet at `inv`, which
    /// also feeds the opamp subckt's inverting input.
    #[test]
    fn tap_into_sheet_instance_is_not_a_divider() {
        let src = "\
opamp inverting (hierarchical sheet)
*@symbol Device:R for=R*
.subckt OPAMP inp inn out vcc vee
E1 out 0 inp inn 1e5
.ends
VCC vcc 0 DC 15 ;@ power=+15V
VEE vee 0 DC -15 ;@ power=-15V
RIN in inv 1k
RF inv out 10k
X1 0 inv out vcc vee OPAMP
.end
";
        let checked = checked_of(src);
        // X1 is lowered to a sheet instance; `inv` is touched by RIN,
        // RF, and X1's `inn` port -> degree 3, not a divider.
        let pairs = detect_dividers(&checked, Placer::default());
        assert!(
            pairs.is_empty(),
            "tap feeding a sheet-instance port must not be a divider, got {pairs:?}"
        );
    }

    /// A non-resistor (capacitor) in series with a resistor is not a
    /// resistor divider.
    #[test]
    fn rc_series_not_a_divider() {
        let src = "\
rc series
*@symbol Device:R for=R*
*@symbol Device:C for=C*
R1 in mid 1k
C1 mid 0 100n
.end
";
        let checked = checked_of(src);
        let pairs = detect_dividers(&checked, Placer::default());
        assert!(
            pairs.is_empty(),
            "R-C series must not be a resistor divider, got {pairs:?}"
        );
    }

    // ===================================================================
    // Rail-gated divider predicate (`--placer=divider-rails`). The
    // shipping predicate's tap-degree gate matches the wrong thing in
    // BOTH directions; these tests pin down the corrected one.
    // ===================================================================

    /// Named refdes pairs preserving the detector's `(upper, lower)`
    /// order — the rail-gated path derives that order from rail
    /// polarity, so an order-insensitive assertion would not see it.
    fn ordered_pairs(checked: &CheckedNetlist, pairs: &[DividerPair]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|p| {
                (
                    checked.elements[p.upper].refdes.clone(),
                    checked.elements[p.lower].refdes.clone(),
                )
            })
            .collect()
    }

    /// **The over-match.** `port_shapes`' shape: a four-resistor series
    /// chain between two Signal nets. Every interior net has degree 2,
    /// so the shipping predicate claims two "dividers" and pins them as
    /// two vertical stacks of two — before the series-chain pass can see
    /// the chain. The rail-gated predicate declines the whole chain.
    #[test]
    fn rail_gate_declines_a_plain_series_chain() {
        let src = "\
four in series, no rail at either end
*@symbol Device:R for=R*
R1 src ni 1k
R2 ni  no 1k
R3 no  nb 1k
R4 nb  nz 1k
.end
";
        let checked = checked_of(src);
        assert_eq!(
            detect_dividers(&checked, Placer::default()).len(),
            2,
            "the shipping predicate over-matches the chain (this is the defect)"
        );
        assert!(
            detect_dividers(&checked, Placer::DividerRails).is_empty(),
            "a chain between two Signal nets is not a divider"
        );
    }

    /// **The under-match.** A real bias divider: the tap drives a
    /// transistor base, so its degree is 3 and the shipping predicate
    /// never fires on the one topology the idiom was written for.
    #[test]
    fn rail_gate_accepts_a_loaded_bias_divider() {
        let src = "\
common-emitter bias divider
*@symbol Device:R for=R*
*@symbol Device:Q_NPN_BCE for=Q*
VCC vcc 0 DC 12 ;@ power=+12V
RB1 vcc b 100k
RB2 b   0 22k
Q1 c b 0 QMOD
.model QMOD NPN
.end
";
        let checked = checked_of(src);
        assert!(
            detect_dividers(&checked, Placer::default()).is_empty(),
            "the shipping predicate under-matches a loaded divider (this is the defect)"
        );
        assert_eq!(
            ordered_pairs(&checked, &detect_dividers(&checked, Placer::DividerRails)),
            vec![("RB1".to_string(), "RB2".to_string())],
            "supply-side resistor on top, return-side beneath"
        );
    }

    /// The stack order comes from rail polarity, not element index: a
    /// netlist that writes the ground-side resistor first still stacks
    /// the supply-side one on top.
    #[test]
    fn rail_gate_orders_the_stack_by_polarity_not_index() {
        let src = "\
ground-side resistor written first
*@symbol Device:R for=R*
VCC vcc 0 DC 12 ;@ power=+12V
RLO b   0 22k
RHI vcc b 100k
.end
";
        let checked = checked_of(src);
        assert_eq!(
            ordered_pairs(&checked, &detect_dividers(&checked, Placer::DividerRails)),
            vec![("RHI".to_string(), "RLO".to_string())]
        );
    }

    /// Two resistors from the SAME rail to a tap are not a divider —
    /// opposite `VertPref` is required, not merely "both rails".
    #[test]
    fn rail_gate_requires_opposite_rails() {
        let src = "\
both ends on ground
*@symbol Device:R for=R*
VCC vcc 0 DC 12 ;@ power=+12V
RA 0 mid 1k
RB mid gnd 1k
RL vcc 0 1k
.end
";
        let checked = checked_of(src);
        assert!(
            detect_dividers(&checked, Placer::DividerRails).is_empty(),
            "ground -> tap -> ground is not a divider"
        );
    }

    /// A tap feeding a hierarchical-sheet port is still declined when
    /// its outer nets are not both rails — the rail gate subsumes the
    /// sheet-degree guard for the `opamp_inverting` false positive
    /// (`in` and `out` are Signal), so relaxing the degree test does not
    /// re-open it.
    #[test]
    fn rail_gate_still_declines_the_opamp_feedback_pair() {
        let src = "\
opamp inverting (hierarchical sheet)
*@symbol Device:R for=R*
.subckt OPAMP inp inn out vcc vee
E1 out 0 inp inn 1e5
.ends
VCC vcc 0 DC 15 ;@ power=+15V
VEE vee 0 DC -15 ;@ power=-15V
RIN in inv 1k
RF inv out 10k
X1 0 inv out vcc vee OPAMP
.end
";
        let checked = checked_of(src);
        assert!(
            detect_dividers(&checked, Placer::DividerRails).is_empty(),
            "RIN/RF span two Signal nets, not two rails"
        );
    }

    /// A three-resistor rail-to-rail ladder is declined outright rather
    /// than half-claimed: each interior tap has one Signal outer net, so
    /// no pair passes. Pinning two of three would misdraw the ladder.
    #[test]
    fn rail_gate_declines_a_three_resistor_ladder() {
        let src = "\
three-resistor bias ladder
*@symbol Device:R for=R*
VCC vcc 0 DC 12 ;@ power=+12V
RB1 vcc b1 10k
RB2 b1  b2 10k
RB3 b2  0  10k
.end
";
        let checked = checked_of(src);
        assert!(
            detect_dividers(&checked, Placer::DividerRails).is_empty(),
            "a 3-element ladder is a chain for the series pass, not a 2-element divider"
        );
    }

    /// The **strict** arm keeps the shipping tap-degree gate, so it
    /// declines the loaded bias divider its sibling accepts — it is a
    /// pure narrowing of the shipping predicate and can only ever move
    /// a fixture the shipping detector wrongly claimed.
    #[test]
    fn the_strict_arm_narrows_only() {
        let loaded = "\
common-emitter bias divider
*@symbol Device:R for=R*
*@symbol Device:Q_NPN_BCE for=Q*
VCC vcc 0 DC 12 ;@ power=+12V
RB1 vcc b 100k
RB2 b   0 22k
Q1 c b 0 QMOD
.model QMOD NPN
.end
";
        let checked = checked_of(loaded);
        assert!(
            detect_dividers(&checked, Placer::DividerRailsStrict).is_empty(),
            "the strict arm declines a LOADED divider, exactly as the shipping one does"
        );
        assert_eq!(
            detect_dividers(&checked, Placer::DividerRails).len(),
            1,
            "the permissive arm accepts it (this is the difference under test)"
        );

        // An UNLOADED rail-to-rail divider passes both rail-gated arms.
        let unloaded = "\
unloaded divider
*@symbol Device:R for=R*
VCC vcc 0 DC 12 ;@ power=+12V
RA vcc mid 10k
RB mid 0   10k
.end
";
        let checked = checked_of(unloaded);
        assert_eq!(
            detect_dividers(&checked, Placer::DividerRailsStrict).len(),
            1
        );
        assert_eq!(detect_dividers(&checked, Placer::DividerRails).len(), 1);

        // And the over-match is removed by BOTH: a plain series chain
        // has degree-2 taps but no rails.
        let chain = "\
four in series, no rail at either end
*@symbol Device:R for=R*
R1 src ni 1k
R2 ni  no 1k
R3 no  nb 1k
R4 nb  nz 1k
.end
";
        let checked = checked_of(chain);
        assert!(detect_dividers(&checked, Placer::DividerRailsStrict).is_empty());
        assert!(detect_dividers(&checked, Placer::DividerRails).is_empty());
    }

    /// A split-supply divider (VCC -> tap -> VEE) is accepted: `VertPref`
    /// puts a `*@power=-…` rail `Down`, so the polarity test spans it.
    #[test]
    fn rail_gate_accepts_a_split_supply_divider() {
        let src = "\
split-supply divider
*@symbol Device:R for=R*
VCC vcc 0 DC 15 ;@ power=+15V
VEE vee 0 DC -15 ;@ power=-15V
RA vcc mid 10k
RB mid vee 10k
.end
";
        let checked = checked_of(src);
        assert_eq!(
            ordered_pairs(&checked, &detect_dividers(&checked, Placer::DividerRails)),
            vec![("RA".to_string(), "RB".to_string())]
        );
    }

    // ===================================================================
    // Canonical-placement idiom detectors (Tier-2 V6/V7). These tests are
    // RED until the following detectors land in this module, mirroring the
    // `detect_dividers` shape (pure netlist inspection, deterministic
    // output, strict / low-false-positive). The expected public-in-crate
    // API each test binds to (implementer must match these names):
    //
    //   pub(crate) struct ParallelPair { pub a: usize, pub b: usize }
    //   pub(crate) fn detect_parallel_pairs(&CheckedNetlist)
    //       -> Vec<ParallelPair>;
    //     Two two-terminal passives sharing BOTH of their (distinct) nets.
    //
    //   pub(crate) struct CollectorLoad { pub resistor: usize,
    //                                     pub transistor: usize }
    //   pub(crate) fn detect_collector_loads(&CheckedNetlist)
    //       -> Vec<CollectorLoad>;
    //     A two-terminal resistor whose non-rail pin shares a net with a
    //     BJT COLLECTOR terminal (SPICE terminal 0 of a `Bjt`).
    //
    //   pub(crate) struct SharedNodeCenter { pub element: usize,
    //                                        pub transistors: Vec<usize> }
    //   pub(crate) fn detect_shared_node_centers(&CheckedNetlist)
    //       -> Vec<SharedNodeCenter>;
    //     An element on a net whose OTHER members are >= 2 transistors (a
    //     shared tail/emitter node).
    // ===================================================================

    /// Refdes of the element at `idx`.
    fn refdes_at(checked: &CheckedNetlist, idx: usize) -> String {
        checked.elements[idx].refdes.clone()
    }

    /// Parallel pairs mapped to sorted refdes pairs, order-independent.
    fn parallel_refdes(checked: &CheckedNetlist, pairs: &[ParallelPair]) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = pairs
            .iter()
            .map(|p| {
                let a = refdes_at(checked, p.a);
                let b = refdes_at(checked, p.b);
                if a <= b { (a, b) } else { (b, a) }
            })
            .collect();
        out.sort();
        out
    }

    /// Collector-load hits mapped to `(resistor, transistor)` refdes.
    fn collector_refdes(checked: &CheckedNetlist, hits: &[CollectorLoad]) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = hits
            .iter()
            .map(|h| {
                (
                    refdes_at(checked, h.resistor),
                    refdes_at(checked, h.transistor),
                )
            })
            .collect();
        out.sort();
        out
    }

    /// Shared-node hits mapped to `(element, sorted transistor refdes)`.
    fn shared_refdes(
        checked: &CheckedNetlist,
        hits: &[SharedNodeCenter],
    ) -> Vec<(String, Vec<String>)> {
        let mut out: Vec<(String, Vec<String>)> = hits
            .iter()
            .map(|h| {
                let mut ts: Vec<String> = h
                    .transistors
                    .iter()
                    .map(|&i| refdes_at(checked, i))
                    .collect();
                ts.sort();
                (refdes_at(checked, h.element), ts)
            })
            .collect();
        out.sort();
        out
    }

    // ---- Idiom 1: PARALLEL two-terminal pair --------------------------

    /// R ‖ C sharing BOTH nets (the `common_emitter` RE‖CE case: both on
    /// nets `e` and `0`) is a parallel pair.
    #[test]
    fn detects_parallel_r_and_c() {
        let src = "\
parallel r and c
*@symbol Device:R for=R*
*@symbol Device:C for=C*
RE e 0 1k
CE e 0 100u
.end
";
        let checked = checked_of(src);
        let pairs = detect_parallel_pairs(&checked);
        assert_eq!(
            parallel_refdes(&checked, &pairs),
            vec![("CE".to_string(), "RE".to_string())],
            "R‖C sharing both nets must be detected as a parallel pair"
        );
    }

    /// Two resistors sharing BOTH nets are a parallel pair too.
    #[test]
    fn detects_parallel_r_and_r() {
        let src = "\
parallel resistors
*@symbol Device:R for=R*
R1 a b 1k
R2 a b 2k
.end
";
        let checked = checked_of(src);
        let pairs = detect_parallel_pairs(&checked);
        assert_eq!(
            parallel_refdes(&checked, &pairs),
            vec![("R1".to_string(), "R2".to_string())]
        );
    }

    /// NEAR-MISS: two elements sharing exactly ONE net (a series / divider
    /// topology) are NOT parallel — the both-nets test must reject them.
    #[test]
    fn series_pair_is_not_parallel() {
        let src = "\
series not parallel
*@symbol Device:R for=R*
*@symbol Device:C for=C*
R1 in mid 1k
C1 mid 0 100n
.end
";
        let checked = checked_of(src);
        let pairs = detect_parallel_pairs(&checked);
        assert!(
            pairs.is_empty(),
            "elements sharing only one net must not be a parallel pair, got {pairs:?}"
        );
    }

    // ---- Idiom 2: COLLECTOR-LOAD above transistor ---------------------

    /// A resistor whose non-rail pin is on a BJT collector net is a
    /// collector load. `RE` (on the emitter net) and `RB` (on the base
    /// net) are near-misses that must NOT be reported.
    #[test]
    fn detects_collector_load_only() {
        let src = "\
collector load with emitter/base near-misses
*@symbol Device:R for=R*
*@symbol Device:Q_NPN_BCE for=Q*
Q1 c b e QMOD
RC vcc c 3k3
RB vcc b 47k
RE e 0 1k
.model QMOD NPN (BF=100 IS=1e-15)
.end
";
        let checked = checked_of(src);
        let hits = detect_collector_loads(&checked);
        assert_eq!(
            collector_refdes(&checked, &hits),
            vec![("RC".to_string(), "Q1".to_string())],
            "only the collector-net resistor RC is a collector load; RB (base) \
             and RE (emitter) must be rejected, got {:?}",
            collector_refdes(&checked, &hits)
        );
    }

    /// NEAR-MISS: a resistor on the emitter net (no terminal on any
    /// collector net) must not be reported as a collector load.
    #[test]
    fn emitter_resistor_is_not_a_collector_load() {
        let src = "\
emitter resistor only
*@symbol Device:R for=R*
*@symbol Device:Q_NPN_BCE for=Q*
Q1 c b e QMOD
RE e 0 1k
.model QMOD NPN (BF=100 IS=1e-15)
.end
";
        let checked = checked_of(src);
        let hits = detect_collector_loads(&checked);
        assert!(
            hits.is_empty(),
            "an emitter-net resistor must not be a collector load, got {:?}",
            collector_refdes(&checked, &hits)
        );
    }

    // ---- Idiom 3: SHARED-NODE centering -------------------------------

    /// Two BJTs sharing an emitter (tail) node, with a resistor also on
    /// that node, is the differential-pair tail: RTAIL centers under
    /// {Q1, Q2}.
    #[test]
    fn detects_shared_tail_node() {
        let src = "\
diff-pair tail
*@symbol Device:R for=R*
*@symbol Device:Q_NPN_BCE for=Q*
Q1 c1 in1 tail QMOD
Q2 c2 in2 tail QMOD
RTAIL tail vee 2k2
.model QMOD NPN (BF=100 IS=1e-15)
.end
";
        let checked = checked_of(src);
        let hits = detect_shared_node_centers(&checked);
        assert_eq!(
            shared_refdes(&checked, &hits),
            vec![(
                "RTAIL".to_string(),
                vec!["Q1".to_string(), "Q2".to_string()]
            )],
            "RTAIL on a node shared by two transistors must center under them"
        );
    }

    /// NEAR-MISS: a node touched by only ONE transistor is not a shared
    /// tail — the >= 2 transistor requirement must reject it.
    #[test]
    fn single_transistor_node_is_not_shared() {
        let src = "\
single transistor emitter
*@symbol Device:R for=R*
*@symbol Device:C for=C*
*@symbol Device:Q_NPN_BCE for=Q*
Q1 c b e QMOD
RE e 0 1k
CE e 0 100u
.model QMOD NPN (BF=100 IS=1e-15)
.end
";
        let checked = checked_of(src);
        let hits = detect_shared_node_centers(&checked);
        assert!(
            hits.is_empty(),
            "a node with only one transistor must not be a shared-tail center, got {:?}",
            shared_refdes(&checked, &hits)
        );
    }

    // -----------------------------------------------------------------
    // Rail-stub SIDE: the shunt re-column is not "always down"
    // -----------------------------------------------------------------

    /// `rail_facing_orientation` must face the rail pin the way the
    /// stub's own `side` says, not the way ground alone would want.
    ///
    /// The pre-fix helper hard-coded screen-down ("so a ground glyph
    /// hangs below"), which pinned a positive-supply bias resistor
    /// upside-down — with its `+12V` glyph *under* the body.
    #[test]
    fn rail_facing_orientation_follows_stub_side() {
        let src = "\
up and down stubs
*@symbol Device:R for=R*
VCC vcc 0 DC 12 ;@ power=+12V
RUP vcc n 100k
RDN n 0 10k
.end
";
        let checked = checked_of(src);
        let by = |r: &str| {
            checked
                .elements
                .iter()
                .find(|e| e.refdes == r)
                .expect("element present")
        };
        let up = rail_facing_orientation(by("RUP"), "n", VertPref::Up).expect("up orientation");
        let dn = rail_facing_orientation(by("RDN"), "n", VertPref::Down).expect("down orientation");
        // The parameterisation itself: asking the SAME element for the
        // two sides must give two different poses. (Comparing RUP to RDN
        // instead proves nothing — their rail pins are opposite terminals,
        // so both legitimately resolve to R0.)
        let up_flipped = rail_facing_orientation(by("RUP"), "n", VertPref::Down)
            .expect("down orientation of the up-stub");
        assert_ne!(
            up, up_flipped,
            "`side` must change the chosen pose; the pre-fix helper ignored it"
        );
        // Screen-frame angles from `pins_in`: 270 = up, 90 = down.
        let facing = |e: &ResolvedElement, o: Orientation, signal: &str| {
            let ti = e.nodes.iter().position(|n| n != signal).expect("rail term");
            let pin = e.pin_mapping.get(ti).expect("rail pin");
            e.symbol
                .pins_in(o)
                .iter()
                .find(|p| &p.number == pin)
                .expect("pin present")
                .angle
                % 360
        };
        assert_eq!(
            facing(by("RUP"), up, "n"),
            270,
            "positive-rail pin must face screen-up"
        );
        assert_eq!(
            facing(by("RDN"), dn, "n"),
            90,
            "ground pin must face screen-down"
        );
    }

    /// End to end through the seed placer: a series element whose
    /// downstream node carries a **positive-supply** stub re-columns that
    /// stub ABOVE the node (smaller world Y — world Y grows downward),
    /// with a V14-allowed orientation. The pre-fix pass dropped it below
    /// and pinned the forbidden pose past every V14 enforcer.
    #[test]
    fn up_side_shunt_re_columns_above_the_series_element() {
        let src = "\
up-side shunt re-column
*@symbol Device:R for=R*
*@symbol Device:C for=C*
*@port in=input
VCC vcc 0 DC 12 ;@ power=+12V
R1 in mid 10k
CIN mid out 1u
RB vcc out 100k
.end
";
        let checked = checked_of(src);
        let allowed = crate::orient::allowed_orientations(&checked, Placer::default());
        let idx = |r: &str| {
            checked
                .elements
                .iter()
                .position(|e| e.refdes == r)
                .expect("element present")
        };
        let (rb, cin) = (idx("RB"), idx("CIN"));
        let placement = crate::place(checked.clone(), fixture_library()).expect("place");
        assert!(
            placement.elements[rb].origin.y < placement.elements[cin].origin.y,
            "the +12V stub RB must sit ABOVE its node (RB.y={}, CIN.y={})",
            placement.elements[rb].origin.y,
            placement.elements[cin].origin.y
        );
        assert!(
            allowed[rb].contains(&placement.elements[rb].orientation),
            "RB was placed at a V14-forbidden orientation {:?} (allowed {:?})",
            placement.elements[rb].orientation,
            allowed[rb]
        );
    }

    /// Orientation-churn stage 1 (`--placer=flow-seed-v2`).
    ///
    /// A netlist with a **drawn** stimulus and no `*@port … =input`
    /// seeds NEITHER of the two default root tiers: there is no declared
    /// input port, and the leaf-name backstop requires the net be
    /// touched by exactly one element (here `n1` is touched by three).
    /// So the depth map comes back empty on the default path — and an
    /// empty map makes `apply_series_horizontal` decline every element,
    /// because it needs a strict depth order across a series element's
    /// two nodes.
    ///
    /// This is the `lc_ladder_lpf` shape in miniature. Stage 1's third
    /// tier roots at the drawn source, mirroring
    /// `layers::is_signal_source`.
    #[test]
    fn drawn_source_roots_the_depth_map_only_under_stage_one() {
        let src = "\
drawn-source depth roots
*@symbol Device:R for=R*
*@symbol Device:C for=C*
*@symbol Simulation_SPICE:VDC for=VIN
VIN src 0  DC 0 AC 1
RS  src n1 1k
C1  n1  0  1n
R2  n1  n2 1k
C2  n2  0  1n
.end
";
        let checked = checked_of(src);
        let classes = classify_nets(&checked);
        assert!(
            signal_net_depth(&checked, &classes, Placer::FlowSeed).is_empty(),
            "the shipping default has no drawn-source root tier — if this \
             map is non-empty the change leaked onto the default path"
        );
        let depth = signal_net_depth(&checked, &classes, Placer::FlowSeedV2);
        assert_eq!(depth.get("src"), Some(&0), "{depth:?}");
        assert_eq!(depth.get("n1"), Some(&1), "{depth:?}");
        assert_eq!(depth.get("n2"), Some(&2), "{depth:?}");
        assert!(
            !depth.contains_key("0"),
            "the BFS must never cross a rail: {depth:?}"
        );
    }

    /// The third tier is **last**, so a fixture whose port loop already
    /// seeds a root is byte-unchanged by it: same map on both placers,
    /// and the drawn source's own net does NOT become a second root.
    #[test]
    fn a_declared_input_port_outranks_the_drawn_source_tier() {
        let src = "\
declared port wins
*@symbol Device:R for=R*
*@symbol Device:C for=C*
*@symbol Simulation_SPICE:VDC for=VIN
*@port n1=input
VIN src 0  DC 0 AC 1
RS  src n1 1k
C1  n1  0  1n
R2  n1  n2 1k
C2  n2  0  1n
.end
";
        let checked = checked_of(src);
        let classes = classify_nets(&checked);
        let base = signal_net_depth(&checked, &classes, Placer::FlowSeed);
        let stage1 = signal_net_depth(&checked, &classes, Placer::FlowSeedV2);
        assert_eq!(base, stage1, "stage 1 must be inert once a tier fires");
        assert_eq!(base.get("n1"), Some(&0), "{base:?}");
        assert_eq!(base.get("src"), Some(&1), "{base:?}");
    }

    /// Seed-only placement under one named placer, so a test can A/B the
    /// F1 constructions against the shipping default without the SA in
    /// the way. `refine: false` is load-bearing, not a speed-up: the
    /// annealer is globally coupled, so with it on, "did this element
    /// move?" cannot be attributed to the pass under test.
    fn seed_with(checked: &CheckedNetlist, placer: Placer) -> Placement {
        crate::place_with(
            checked.clone(),
            fixture_library(),
            &crate::LayoutOptions {
                placer,
                refine: false,
                ..crate::LayoutOptions::default()
            },
        )
        .expect("place")
    }

    /// Screen-frame world `(x, y)` of a placed element's pin on `net`.
    fn pin_at(
        placement: &Placement,
        checked: &CheckedNetlist,
        refdes: &str,
        net: &str,
    ) -> (f64, f64) {
        let i = checked
            .elements
            .iter()
            .position(|e| e.refdes == refdes)
            .expect("element present");
        world_pin_xy_of(&placement.elements[i], &checked.elements[i], net).expect("pin on net")
    }

    fn orientation_of(
        placement: &Placement,
        checked: &CheckedNetlist,
        refdes: &str,
    ) -> Orientation {
        let i = checked
            .elements
            .iter()
            .position(|e| e.refdes == refdes)
            .expect("element present");
        placement.elements[i].orientation
    }

    fn is_horizontal(o: Orientation) -> bool {
        matches!(o.rotation, Rotation::R90 | Rotation::R270)
    }

    /// F1 case (a) — the shunt-bearing guard's terminal-net relaxation.
    ///
    /// `COUT` couples the collector node to a declared `*@port …=output`
    /// that nothing else touches. The shipping pass declines it (no
    /// downstream shunt to re-column) and it is left standing on end, so
    /// the emitter attaches the `out` label vertically. Under
    /// `--placer=terminal-series` it is drawn horizontal, upstream pin at
    /// the lower X, and the interior-side pin does not move.
    #[test]
    fn a_terminal_net_series_element_goes_horizontal_only_under_f1() {
        let src = "\
terminal-net series
*@symbol Device:R for=R*
*@symbol Device:C for=C*
*@port in=input
*@port out=output
VCC vcc 0 DC 12 ;@ power=+12V
R1  in  mid 10k
RL  vcc mid 47k
COUT mid out 1u
.end
";
        let checked = checked_of(src);
        let f1 = seed_with(&checked, Placer::TerminalSeries);
        let i_cout = checked
            .elements
            .iter()
            .position(|e| e.refdes == "COUT")
            .expect("COUT present");

        // The mechanism claim, stated where it is unambiguous: under the
        // shipping default the pass DECLINES this element, so it is left
        // to `pick_orientations` unpinned — a choice the SA rotate move
        // and phase 4.5 may both undo. Under F1 the pass constructs the
        // pose and **pins** it. (Asserting "the default draws it vertical"
        // would be the weaker claim and is specimen-dependent: the V5 seed
        // chooser sometimes stumbles onto a horizontal pose on its own.)
        let pinned_under = |placer: Placer| -> bool {
            crate::refinement_meta(&checked, &crate::Hint::default(), placer)
                .expect("refinement meta")
                .pinned[i_cout]
        };
        assert!(
            !pinned_under(Placer::FlowSeedV4),
            "the shipping default must leave COUT unpinned here; if it does \
             not, the shunt-bearing guard no longer declines and this test \
             proves nothing"
        );
        assert!(
            pinned_under(Placer::TerminalSeries),
            "F1 must PIN the constructed pose — an unpinned orientation is \
             one `pick_orientations`, the SA and phase 4.5 can each undo"
        );

        let o = orientation_of(&f1, &checked, "COUT");
        assert!(is_horizontal(o), "COUT must be drawn horizontal, got {o:?}");

        // Direction: the upstream (interior) pin is at the lower X, so the
        // signal runs left -> right out to the port label.
        let (up_x, _) = pin_at(&f1, &checked, "COUT", "mid");
        let (down_x, _) = pin_at(&f1, &checked, "COUT", "out");
        assert!(
            up_x < down_x,
            "upstream pin must sit left: {up_x} !< {down_x}"
        );
    }

    /// The joint half of case (a), tested on the mechanism rather than
    /// through the whole pipeline: `origin_placing_pin_at` is the inverse
    /// of `world_pin_xy_of`, so re-seating an element at a new orientation
    /// leaves the anchored pin exactly where it was.
    ///
    /// This is what makes the construction *joint* (position AND
    /// orientation) instead of the bare rotation ADR-15 Stage 5 measured
    /// the cost of — and it is checked here because the end-to-end
    /// placement runs `pick_orientations` and `legalize` afterwards, which
    /// would make a pipeline-level before/after ambiguous.
    #[test]
    fn re_seating_holds_the_anchored_pin_still() {
        let src = "\
pin-anchored re-seat
*@symbol Device:C for=C*
*@port in=input
*@port out=output
CIN in out 1u
.end
";
        let checked = checked_of(src);
        let placement = seed_with(&checked, Placer::FlowSeedV4);
        let e = &checked.elements[0];
        let pe = &placement.elements[0];
        let before = world_pin_xy_of(pe, e, "in").expect("pin on `in`");
        for &o in &Orientation::ALL {
            let origin = origin_placing_pin_at(e, o, "in", before).expect("re-seat");
            let mut moved = pe.clone();
            moved.origin = origin;
            moved.orientation = o;
            let after = world_pin_xy_of(&moved, e, "in").expect("pin on `in`");
            assert!(
                (before.0 - after.0).abs() < 1e-9 && (before.1 - after.1).abs() < 1e-9,
                "{o:?}: the anchored pin moved {before:?} -> {after:?}"
            );
        }
    }

    /// F1 case (b) — the both-sides guard becomes orient-but-don't-re-column.
    ///
    /// `b` carries `RB1` (to `+12V`) and `RB2` (to ground): a bias divider
    /// THROUGH the node, which the shipping pass declines outright. Under
    /// `--placer=terminal-series-divider` the coupling cap is drawn
    /// horizontal with its downstream pin ON the divider's own column and
    /// at the Y of the wire the node sends to `Q1`'s base — and the
    /// divider members do not move a single cell.
    #[test]
    fn the_divider_node_case_orients_onto_the_column_without_moving_the_divider() {
        let src = "\
divider-node series
*@symbol Device:R for=R*
*@symbol Device:C for=C*
*@symbol Device:Q_NPN_BCE for=Q*
*@port in=input
VCC vcc 0 DC 12 ;@ power=+12V
CIN in  b   1u
RB1 vcc b   100k
RB2 b   0   22k
RC  vcc c   4k7
RE  e   0   1k
Q1  c b e   QGENERIC
.model QGENERIC NPN (BF=200 IS=1e-15)
.end
";
        let checked = checked_of(src);
        let terminal_only = seed_with(&checked, Placer::TerminalSeries);
        let f1 = seed_with(&checked, Placer::TerminalSeriesDivider);

        // Case (a) alone must NOT reach this element: `in` is a declared
        // port, but the both-sides guard is consulted FIRST and case (a)
        // only fires when the downstream node carries no stub at all.
        // That ordering is what makes the two challenger arms attributable
        // — if the (a)-only arm already produced this pose, the (b) arm's
        // aggregate could not be read.
        let i_cin = checked
            .elements
            .iter()
            .position(|e| e.refdes == "CIN")
            .expect("CIN present");
        assert_ne!(
            (
                terminal_only.elements[i_cin].origin,
                terminal_only.elements[i_cin].orientation
            ),
            (f1.elements[i_cin].origin, f1.elements[i_cin].orientation),
            "the (a)-only arm already produced the (b) pose; the two arms \
             are no longer separable"
        );
        let o = orientation_of(&f1, &checked, "CIN");
        assert!(is_horizontal(o), "CIN must be drawn horizontal, got {o:?}");

        // The downstream pin lands ON the divider column ...
        let (cin_x, cin_y) = pin_at(&f1, &checked, "CIN", "b");
        let (rb1_x, _) = pin_at(&f1, &checked, "RB1", "b");
        let (rb2_x, _) = pin_at(&f1, &checked, "RB2", "b");
        let column = f64::midpoint(rb1_x, rb2_x);
        assert!(
            (cin_x - column).abs() <= GridPoint::STEP_MM / 2.0,
            "CIN's `b` pin must sit on the divider column {column}, got {cin_x}"
        );
        // ... at the Y of the wire the node sends to Q1's base.
        let (_, base_y) = pin_at(&f1, &checked, "Q1", "b");
        assert!(
            (cin_y - base_y).abs() <= GridPoint::STEP_MM / 2.0,
            "CIN's `b` pin must sit at the base-wire Y {base_y}, got {cin_y}"
        );

        // The divider members are READ, never written: same origin and
        // same orientation as the arm that never looked at them.
        for r in ["RB1", "RB2"] {
            let i = checked
                .elements
                .iter()
                .position(|e| e.refdes == r)
                .expect("element present");
            assert_eq!(
                terminal_only.elements[i].origin, f1.elements[i].origin,
                "{r} moved; the divider members belong to the divider idiom"
            );
            assert_eq!(
                terminal_only.elements[i].orientation, f1.elements[i].orientation,
                "{r} was re-oriented; the divider members belong to the divider idiom"
            );
        }
    }
}
