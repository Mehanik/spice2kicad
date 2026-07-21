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
//!   divider midpoint and not an arbitrary shared node),
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

use crate::net_class::{NetClass, NetClassMap, VertPref, classify_nets};
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
pub(crate) fn detect_dividers(checked: &CheckedNetlist) -> Vec<DividerPair> {
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
        // A divider midpoint connects exactly two terminals, both of
        // which are the two resistors meeting here.
        if net_degree.get(tap).copied() != Some(2) {
            continue;
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

        used[a] = true;
        used[b] = true;
        pairs.push(DividerPair { upper: a, lower: b });
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

/// Draw series signal elements horizontally on the flow lane, upstream
/// pin left, with their downstream shunts dropping straight beneath the
/// output node (MEMORY "flow-orientation wall"; ADR-15 Stage-5 post-mortem;
/// the ADR-15 §1.3 joint position+orientation hypothesis).
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
pub(crate) fn apply_series_horizontal(
    placement: &mut Placement,
    pinned: &mut [bool],
    checked: &CheckedNetlist,
) {
    // Extra cells beyond a body-clean vertical stride, so a downstream
    // shunt's top pin sits far enough below the series pin that the
    // shared-node port label prefers the series pin (V13 pin-text).
    // Measured on `rc_lowpass_ports`: the body-clean stride alone leaves a
    // 1-cell pin gap (label lands on the shunt, colliding with its pin
    // number); +2 cells clears it.
    const SHUNT_LABEL_MARGIN_CELLS: i32 = 2;
    let classes = classify_nets(checked);
    let depth = signal_net_depth(checked, &classes);
    let stubs = detect_rail_stubs(checked);

    let is_signal = |n: &str| -> bool {
        n != "0" && !matches!(classes.get(n), Some(NetClass::Power | NetClass::Ground))
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
        // Series elements are pure-signal 2-pin passives → V14 allows all
        // eight orientations, so any horizontal one is V14-legal.
        let Some(orient) = horizontal_flow_orientation(&placement.elements[i], &e.symbol, up, down)
        else {
            continue;
        };
        placement.elements[i].orientation = orient;
        pinned[i] = true;

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
            // Orient the shunt V14-correct: its rail pin faces screen-down
            // (so a ground glyph hangs below). Pinning skips
            // `pick_orientations`, which would otherwise choose this, so we
            // must set it here.
            let s_orient = rail_down_orientation(se, down)
                .unwrap_or(placement.elements[s.element].orientation);
            placement.elements[s.element].orientation = s_orient;
            let shunt_ext = world_extent(&se.symbol, s_orient, None);
            let stride = vertical_stride_cells(&series_ext, &shunt_ext) + SHUNT_LABEL_MARGIN_CELLS;
            let new_y = placement.elements[i].origin.y + stride;
            placement.elements[s.element].origin = GridPoint::new(down_x, new_y);
            pinned[s.element] = true;
        }
    }
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
/// that puts its **rail** pin (the pin on the non-signal node) facing
/// screen-down, so a ground/negative-rail glyph hangs beneath it — the
/// V14-correct facing [`crate::pick_orientations`] would otherwise choose
/// (it is skipped here because the shunt is pinned). `None` if no vertical
/// orientation faces the rail pin down.
fn rail_down_orientation(se: &ResolvedElement, signal_net: &str) -> Option<Orientation> {
    let rail_ti = se.nodes.iter().position(|n| n != signal_net)?;
    let rail_pin = se.pin_mapping.get(rail_ti)?;
    for &o in &Orientation::ALL {
        if !matches!(o.rotation, Rotation::R0 | Rotation::R180) {
            continue;
        }
        // `pins_in` yields transformed (screen-frame) angles: 90 = down.
        if se
            .symbol
            .pins_in(o)
            .iter()
            .find(|p| &p.number == rail_pin)
            .is_some_and(|p| p.angle % 360 == 90)
        {
            return Some(o);
        }
    }
    None
}

/// Flow-depth of each signal net, as hop count from the input boundary.
///
/// Roots are declared `*@port … =input` nets; BFS walks element net
/// adjacency, never crossing a rail (Power/Ground/`0`), so the depth
/// orders signal nets from input to output. Nets unreachable from an
/// input root are absent from the map (depth unknown).
fn signal_net_depth(checked: &CheckedNetlist, classes: &NetClassMap) -> HashMap<String, u32> {
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
    let mut depth: HashMap<String, u32> = HashMap::new();
    let mut frontier: Vec<&str> = Vec::new();
    for p in &checked.ports {
        if matches!(p.dir, PortDir::Input)
            && depth.insert(p.net.clone(), 0).is_none()
        {
            frontier.push(p.net.as_str());
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
        let pairs = detect_dividers(&checked);
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
        let pairs = detect_dividers(&checked);
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
        let pairs = detect_dividers(&checked);
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
        let pairs = detect_dividers(&checked);
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
        let pairs = detect_dividers(&checked);
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
        let pairs = detect_dividers(&checked);
        assert!(
            pairs.is_empty(),
            "R-C series must not be a resistor divider, got {pairs:?}"
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
}
