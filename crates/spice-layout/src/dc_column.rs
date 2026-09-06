//! Idiom 6 — the **DC-series column**: Y-ordering, not Y-spacing.
//!
//! # The mismodelling this addresses
//!
//! X was promoted a generation (ADR-23's second promotion: X means
//! *depth along the DC signal path*). Y was left behind — [`crate::bands`]
//! still assigns it from a five-value net-class-membership table, while
//! the convention it is meant to serve is *Y ∝ DC potential along the
//! current path*. The tree already contains the correct functional,
//! [`crate::dc_rank`], but only phase 4.5's facing trigger reads it;
//! placement never did.
//!
//! The owner's `cascode_amp` complaint lives exactly here: `Q1` and `Q2`
//! are one current, the drawing puts them side by side.
//!
//! # What this is NOT
//!
//! It is **not** "Y = potential" as a coordinate model. ADR-19's M4
//! post-mortem is unambiguous that Y-*spacing* changes land in chaotic,
//! unattributable basins (`MID_SUBROW_GAP` swept over five values, "a
//! chaotic response that has no monotone structure"), and M4 was
//! landed-then-reverted leaving master red for six commits. ADR-17's
//! retirement adds that global re-basing is intrinsic to any
//! spacing-derived placement — "determinism is not locality". **Y
//! spacing is out of scope here and stays out.**
//!
//! What this *is* is a **constructive column idiom**, in the same family
//! as [`crate::idioms::detect_dividers`] (the `divider-rails` arm) and
//! [`crate::idioms::apply_series_horizontal`] (the `terminal-series` /
//! `terminal-series-divider` arms): a local, netlist-derived
//! construction that writes *relative* geometry for the elements it
//! matches and leaves every other element alone.
//!
//! # The construction
//!
//! A **DC-series pair** is ADR-28 metric B's own discriminator, reused
//! rather than re-derived — see [`dc_series_pairs`]. Two elements that
//! carry the same current, chained transitively, form a **column**: one
//! shared X, ordered top-to-bottom by DC potential.
//!
//! Because a DC edge has exactly two endpoints and a series net has
//! exactly two DC conductors on it, every element sits in **at most two**
//! pairs, so a connected component of the pair graph is a simple path or
//! a cycle. A cycle has no top and is declined.
//!
//! The Y **order** comes from [`crate::dc_rank::higher_net`] — the same
//! rank, the same absorbing-rail BFS, and the same *decline on
//! ambiguity*. For consecutive members `u`, `v` sharing net `N`, the two
//! *outer* nets are compared with **both of the pair's edges removed**:
//! ranking either side by a path that runs through the pair itself is
//! the circularity `dc_rank` removes a device's own edge for. If any
//! consecutive comparison declines, or disagrees with the walk
//! direction, the **whole column is declined**. A construction that
//! guesses where the rank abstains is worse than one that does nothing.
//!
//! # Two arms, for attribution
//!
//! [`Placer::DcSeriesColumn`] writes the column's geometry and stops:
//! nothing is pinned, so `pick_orientations`, the SA and phase 4.5 all
//! keep every degree of freedom they have today. It removes no pose from
//! phase 4.5's Tier-0 repair, so it owes no Tier-0 escape.
//!
//! [`Placer::DcColumnNodeStubs`] additionally **carries the rail stubs of
//! the column's own shared nets** — each seated beside the column at its
//! shared pin's Y, so a tap's bypass capacitor sits level with the leg it
//! parallels instead of wherever [`crate::bands`]'s sheet-height fraction
//! left it. See [`plan_carried_stubs`] and ADR-41; it is an arm because it
//! raises eight Tier-2 per-fixture ratchets, two of them at the geometric
//! floor of the drawing.
//!
//! [`Placer::DcSeriesColumnPinned`] additionally **pins** the column, so
//! the SA and phase 4.5 leave it put. Pinning skips `pick_orientations`,
//! and CLAUDE.md's *consistency requirement* says a pass that freezes a
//! pose must own it — so this arm also chooses each member's
//! orientation, from the **same** [`crate::orient::allowed_orientations`]
//! set every other stage reads (V14 ∩ V17), declining the column when
//! that set admits nothing. It is a construction and not a filter, but
//! it does take freedom away from the Tier-0 repair in the way ADR-37
//! warns about; the seed sweep is the measurement that says what that
//! costs.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use kicad_symbols::{Orientation, Rotation};
use spice_policy::CheckedNetlist;
use spice_resolve::{ElementKind, ElementRole, ResolvedElement};

use crate::idioms::{RailStub, RowAnchor};
use crate::net_class::{NetClass, VertPref, classify_nets, vertical_prefs};
use crate::placer::Placer;
use crate::{GridPoint, Placement, WorldExtent, vertical_stride_cells, world_extent};

// ---------------------------------------------------------------------------
// Detection — ADR-28 metric B's discriminator, on the netlist
// ---------------------------------------------------------------------------

/// The nets at an element's **DC-relevant terminals** — every terminal a
/// DC current can enter or leave by.
///
/// A *stronger* notion than "has a DC edge", and deliberately so: an
/// op-amp symbol or a hierarchical `(sheet …)` instance conducts DC at
/// its pins without having any single current path *through* it, so it
/// has no DC edge — but a net it sits on is emphatically not a
/// two-element series node. Counting its terminals here is what stops
/// `opamp_inverting`'s virtual ground from reading as `RIN` in series
/// with `RF`.
///
/// Byte-for-byte the same rule as `readability_metrics::dc_terminals`.
fn dc_terminal_nets(el: &ResolvedElement) -> Vec<&str> {
    if matches!(el.role, ElementRole::Power(_)) {
        return Vec::new();
    }
    match el.kind {
        // No DC through a capacitor, at either end.
        ElementKind::Capacitor => Vec::new(),
        // `c b e` / `d g s [b]`: index 1 is the control terminal.
        ElementKind::Bjt | ElementKind::Mosfet | ElementKind::Jfet => el
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .map(|(_, n)| n.as_str())
            .collect(),
        _ => el.nodes.iter().map(String::as_str).collect(),
    }
}

/// Is `net` a rail — a supply, a ground, or a negative rail?
///
/// [`classify_nets`] answers this for every net any *element* touches
/// (its rule 4 marks every net a `;@ power=` source sits on, its rule 1
/// marks `"0"`, its rule 3 the canonical names). A net reached only
/// through a hierarchical-sheet port is absent from that map, which is
/// why the canonical-name fallback is here: metric B's `rail_nets` has
/// the same fallback for the same reason.
fn is_rail_net(classes: &HashMap<String, NetClass>, net: &str) -> bool {
    match classes.get(net) {
        Some(NetClass::Power | NetClass::Ground) => true,
        Some(NetClass::Signal) => false,
        None => {
            let lo = net.to_ascii_lowercase();
            net == "0"
                || matches!(
                    lo.as_str(),
                    "gnd" | "vss" | "vee" | "v-" | "vminus" | "vcc" | "vdd" | "v+" | "vplus"
                )
        }
    }
}

/// Rail nets reachable from `from` in the DC graph with `skip`'s edge
/// removed. A rail is a **terminus**: current entering it leaves through
/// the supply, not through the next signal net.
fn rails_reachable<'a>(
    classes: &HashMap<String, NetClass>,
    adj: &HashMap<&'a str, Vec<(&'a str, usize)>>,
    from: &'a str,
    skip: usize,
) -> BTreeSet<&'a str> {
    let mut rails: BTreeSet<&str> = BTreeSet::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut q: VecDeque<&str> = VecDeque::new();
    seen.insert(from);
    q.push_back(from);
    while let Some(n) = q.pop_front() {
        if is_rail_net(classes, n) {
            rails.insert(n);
            continue;
        }
        for (other, via) in adj.get(n).into_iter().flatten() {
            if *via == skip || !seen.insert(*other) {
                continue;
            }
            q.push_back(other);
        }
    }
    rails
}

/// Every **DC-series pair** in `checked`, as `(u, v, shared net)` with
/// `u < v` by element index, sorted.
///
/// This is ADR-28 metric B's definition
/// (`crates/spice2kicad/tests/readability_metrics.rs::dc_series_pairs`),
/// restated over a [`CheckedNetlist`] instead of over emitted geometry.
/// Two drawn elements `u`, `v` qualify when
///
/// 1. each conducts DC between two **distinct** rail nets by a path that
///    does not re-use its own edge, and
/// 2. they share a **non-rail** net whose DC degree is exactly 2 — `u`
///    and `v` are the only DC conductors on it, so all of `u`'s current
///    flows into `v`.
///
/// Clause 2 is what keeps this from demanding nonsense: a differential
/// pair's two transistors share `tail` with `RTAIL`, DC degree 3, and
/// are *correctly* drawn side by side.
///
/// It is public because
/// `readability_metrics::the_placer_and_metric_b_agree_on_every_fixture`
/// asserts the two implementations return the same set on all 22
/// fixtures. The metric and the construction have to agree on what a
/// DC-series pair *is*, or the construction is optimising against a
/// different predicate than the one grading it.
#[must_use]
pub fn dc_series_pairs(checked: &CheckedNetlist) -> Vec<(usize, usize, String)> {
    let classes = classify_nets(checked);
    let adj = crate::dc_rank::dc_adjacency(checked);

    // Elements with a DC edge, and the nets that edge joins.
    let mut edges: HashMap<usize, (&str, &str)> = HashMap::new();
    for (i, el) in checked.elements.iter().enumerate() {
        if let Some((ta, tb)) = crate::dc_rank::conduction_terminals(el) {
            edges.insert(i, (el.nodes[ta].as_str(), el.nodes[tb].as_str()));
        }
    }

    // Which elements sit on a path between two DISTINCT rail nets?
    let mut conducts: HashSet<usize> = HashSet::new();
    for (&i, (a, b)) in &edges {
        let ra = rails_reachable(&classes, &adj, a, i);
        let rb = rails_reachable(&classes, &adj, b, i);
        if ra.iter().any(|x| rb.iter().any(|y| x != y)) {
            conducts.insert(i);
        }
    }

    // DC degree over EVERY element's DC terminals — not just the ones
    // with an edge — plus every hierarchical-sheet instance's ports.
    let mut degree: BTreeMap<&str, BTreeSet<usize>> = BTreeMap::new();
    // Sheet instances are not elements, so they need index space of
    // their own; offsetting past the element count keeps the sets
    // disjoint without a second map.
    let sheet_base = checked.elements.len();
    for (i, el) in checked.elements.iter().enumerate() {
        for net in dc_terminal_nets(el) {
            degree.entry(net).or_default().insert(i);
        }
    }
    for (k, si) in checked.sheet_instances.iter().enumerate() {
        for net in &si.nodes {
            degree
                .entry(net.as_str())
                .or_default()
                .insert(sheet_base + k);
        }
    }

    let mut out = Vec::new();
    for (net, on) in &degree {
        if is_rail_net(&classes, net) || on.len() != 2 {
            continue;
        }
        let mut it = on.iter().copied();
        let (Some(u), Some(v)) = (it.next(), it.next()) else {
            continue;
        };
        // Both must carry the current *through* themselves…
        if !edges.contains_key(&u) || !edges.contains_key(&v) {
            continue;
        }
        // …and both must sit on a supply-to-ground path.
        if !conducts.contains(&u) || !conducts.contains(&v) {
            continue;
        }
        out.push((u, v, (*net).to_string()));
    }
    out.sort();
    out
}

/// A detected DC-series column: element indices ordered **top to
/// bottom** by DC potential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DcColumn {
    /// Members, highest DC potential first. Always at least 2 long.
    pub members: Vec<usize>,
    /// The net each consecutive pair of members shares: `shared[k]` is
    /// the DC-series node joining `members[k]` to `members[k + 1]`, so
    /// this is always exactly one shorter than [`Self::members`].
    ///
    /// Carried because the column is **pin-anchored**, not
    /// centre-anchored (CLAUDE.md, "Constraints are pin-anchored"): the
    /// thing that has to be collinear is the shared *pin* on each
    /// member, and naming that pin needs the net it sits on.
    pub shared: Vec<String>,
}

/// Every DC-series column in `checked`, in a deterministic order.
///
/// Components of the pair graph are simple paths or cycles (an element
/// has at most one pair per DC endpoint). Cycles are declined — a ring
/// of series elements has no top. A path whose consecutive potential
/// comparisons do not all resolve, or do not all agree with the walk
/// direction, is declined too: see the module docs on never guessing.
#[must_use]
pub fn detect_dc_columns(checked: &CheckedNetlist) -> Vec<DcColumn> {
    let pairs = dc_series_pairs(checked);
    if pairs.is_empty() {
        return Vec::new();
    }
    let prefs = vertical_prefs(checked);
    let adj = crate::dc_rank::dc_adjacency(checked);

    // The shared net of each pair, and the pair graph.
    let mut shared: HashMap<(usize, usize), String> = HashMap::new();
    let mut nbr: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for (u, v, net) in &pairs {
        shared.insert((*u, *v), net.clone());
        shared.insert((*v, *u), net.clone());
        nbr.entry(*u).or_default().insert(*v);
        nbr.entry(*v).or_default().insert(*u);
    }

    // The far net of `e` relative to `net` — the DC-edge endpoint that
    // is not the shared node.
    let far = |e: usize, net: &str| -> Option<&str> {
        let el = &checked.elements[e];
        let (ta, tb) = crate::dc_rank::conduction_terminals(el)?;
        let (a, b) = (el.nodes[ta].as_str(), el.nodes[tb].as_str());
        if a == net {
            Some(b)
        } else if b == net {
            Some(a)
        } else {
            None
        }
    };

    let mut seen: HashSet<usize> = HashSet::new();
    let mut columns: Vec<DcColumn> = Vec::new();
    // Walk from every degree-1 endpoint. Anything left unvisited
    // afterwards is a cycle and is declined.
    let ends: Vec<usize> = nbr
        .iter()
        .filter(|(_, n)| n.len() == 1)
        .map(|(e, _)| *e)
        .collect();
    for start in ends {
        if seen.contains(&start) {
            continue;
        }
        // Trace the path.
        let mut path = vec![start];
        seen.insert(start);
        let mut cur = start;
        loop {
            let Some(next) = nbr[&cur].iter().copied().find(|n| !seen.contains(n)) else {
                break;
            };
            seen.insert(next);
            path.push(next);
            cur = next;
        }
        if path.len() < 2 {
            continue;
        }
        // Direction: the first consecutive comparison decides which end
        // is up; every later one has to agree. A decline anywhere kills
        // the whole column.
        let mut order: Option<bool> = None; // Some(true) => `path` runs top→bottom
        let mut ok = true;
        let mut shared_nets: Vec<String> = Vec::with_capacity(path.len() - 1);
        for w in path.windows(2) {
            let (u, v) = (w[0], w[1]);
            let Some(net) = shared.get(&(u, v)) else {
                ok = false;
                break;
            };
            shared_nets.push(net.clone());
            let (Some(fu), Some(fv)) = (far(u, net), far(v, net)) else {
                ok = false;
                break;
            };
            let Some(u_is_higher) = crate::dc_rank::higher_net(&adj, &prefs, fu, fv, &[u, v])
            else {
                ok = false;
                break;
            };
            match order {
                None => order = Some(u_is_higher),
                Some(d) if d == u_is_higher => {}
                Some(_) => {
                    ok = false;
                    break;
                }
            }
        }
        let Some(top_first) = order.filter(|_| ok) else {
            continue;
        };
        let mut members = path;
        if !top_first {
            members.reverse();
            shared_nets.reverse();
        }
        columns.push(DcColumn {
            members,
            shared: shared_nets,
        });
    }
    columns.sort_by(|a, b| a.members.cmp(&b.members));
    columns
}

// ---------------------------------------------------------------------------
// Application
// ---------------------------------------------------------------------------

/// The pose a column member is drawn in, when the pinned arm has to own
/// one (CLAUDE.md's consistency requirement: pinning skips
/// `pick_orientations`, so this pass must choose what it freezes).
///
/// Candidates are scored by convention and the best one **inside
/// `allowed`** wins; `None` means `allowed` admits nothing usable and
/// the caller declines the whole column.
///
/// The conventions, in order of strength:
///
/// * a Q / M / J device with a resolved [`crate::dc_rank`] facing is
///   posed with its higher-DC-potential terminal screen-**up** — the
///   same convention `facing-trigger` repairs to, applied at the seed;
/// * a two-terminal element with exactly one rail node is posed with
///   that node's pin facing the rail's own band (up for a supply, down
///   for a ground / negative rail) — V14's convention, restated;
/// * anything else prefers [`Orientation::IDENTITY`], the vertical pose
///   a column wants.
///
/// Horizontal poses are never preferred for a two-terminal member: a
/// column of sideways resistors is not the drawing this construction is
/// for. They stay *available* (a V14 ∩ V17 set that admits nothing else
/// still yields a pose rather than a decline).
fn column_pose(
    el: &ResolvedElement,
    facing: Option<(usize, usize)>,
    prefs: &HashMap<String, VertPref>,
    allowed: &[Orientation],
) -> Option<Orientation> {
    // Screen Y grows downward, so "up" is the SMALLER y.
    let pin_y = |o: Orientation, ti: usize| -> Option<f64> {
        let want = el.pin_mapping.get(ti)?;
        el.symbol
            .pins_in(o)
            .into_iter()
            .find(|p| &p.number == want)
            .map(|p| -p.y)
    };
    let rail_side = || -> Option<(usize, VertPref)> {
        if el.nodes.len() != 2 {
            return None;
        }
        let mut hit = None;
        for (ti, n) in el.nodes.iter().enumerate() {
            if let Some(&p) = prefs.get(n.as_str()) {
                if hit.is_some() {
                    return None; // both nodes are rails — no side to face
                }
                hit = Some((ti, p));
            }
        }
        hit
    };
    let side = rail_side();

    // Lower score is better. Ties break on `Orientation::ALL` order, so
    // the result is deterministic.
    let score = |o: Orientation| -> (u8, u8, u8) {
        let convention = if let Some((hi, lo)) = facing {
            match (pin_y(o, hi), pin_y(o, lo)) {
                (Some(yh), Some(yl)) if yh < yl => 0,
                _ => 1,
            }
        } else if let Some((ti, pref)) = side {
            let other = usize::from(ti == 0);
            match (pin_y(o, ti), pin_y(o, other)) {
                (Some(yr), Some(yo)) => {
                    let rail_is_up = yr < yo;
                    u8::from(rail_is_up != (pref == VertPref::Up))
                }
                _ => 1,
            }
        } else {
            1 // no convention to satisfy; everything ties here
        };
        // A column member is drawn along the column: prefer a pose whose
        // rotation keeps the pin axis vertical.
        let upright = u8::from(!matches!(o.rotation, Rotation::R0 | Rotation::R180));
        let identity = u8::from(o != Orientation::IDENTITY);
        (convention, upright, identity)
    };

    allowed
        .iter()
        .copied()
        .min_by_key(|&o| (score(o), Orientation::ALL.iter().position(|x| *x == o)))
}

/// The x offset, in whole grid cells, from `el`'s origin to the pin it
/// presents on `net` when posed at `o`.
///
/// `None` — which the caller turns into a decline — when the element has
/// no terminal on `net`, when that terminal has no mapped KiCad pin, or
/// when the pin does not sit an **integral** number of cells from the
/// origin. The last clause is not fussiness: every origin is grid-snapped
/// by construction, so a pin at a fractional cell offset cannot be put on
/// a shared column x at all without taking some other member off it.
#[allow(clippy::cast_possible_truncation)] // pin coords are bounded; KiCad symbols fit in i32 grid units.
fn shared_pin_x_cells(el: &ResolvedElement, o: Orientation, net: &str) -> Option<i32> {
    let ti = el.nodes.iter().position(|n| n == net)?;
    let want = el.pin_mapping.get(ti)?;
    let px = el
        .symbol
        .pins_in(o)
        .into_iter()
        .find(|p| &p.number == want)
        .map(|p| p.x)?;
    let cells = px / GridPoint::STEP_MM;
    let snapped = cells.round();
    ((cells - snapped).abs() < 1e-6 && snapped.abs() < f64::from(i32::MAX / 2))
        .then_some(snapped as i32)
}

/// The per-member x offset that makes a column **pin-anchored**: the
/// number of cells to subtract from the column's x to get each member's
/// origin, so that the member's own shared-net pin lands ON the column.
///
/// # Why this is not the origin
///
/// CLAUDE.md's layout invariant is explicit — "`place` and `align`
/// describe relationships between *connecting pins*, not symbol
/// centers … the constraint resolver therefore consumes resolved symbol
/// pin geometry". A two-pin resistor's pins sit on its origin's x, but
/// `Device:Q_NPN_BCE`'s collector is 2.54 mm to the right of it, so a
/// column anchored on origins aligns the *bodies* and leaves a 2-cell jog
/// in the very wire the column exists to straighten. The offset is read
/// from [`kicad_symbols::Symbol::pins_in`] at the pose the column draws,
/// because it differs by symbol AND by orientation.
///
/// # The two-shared-pins case
///
/// An **interior** member has two shared nets — one to the neighbour
/// above, one below — and therefore two pins that both have to be on the
/// column. If their x offsets differ, no single x satisfies both and the
/// column's own geometry is inconsistent: this returns `None` and the
/// caller **declines the whole column**.
///
/// Declining is the deliberate choice over splitting or picking a side.
/// Splitting is ill-defined (the offending member belongs to both halves,
/// and dropping it breaks the very series relation the column asserts),
/// and picking one pin silently re-introduces the jog this function
/// exists to remove — on the *other* neighbour, where nothing measures
/// it. It is the same rule the construction already applies to `dc_rank`
/// ambiguity and to cycles: "a construction that guesses where the rank
/// abstains is worse than one that does nothing."
///
/// In practice the case is reachable but rare, and only through a
/// sideways pose: a vertical two-terminal element has both pins on x = 0,
/// and a vertical BJT has C and E on the same x (±2.54), so both agree.
/// Rotate that BJT 90° and its C and E land on opposite sides — which is
/// exactly a member that is not being drawn as part of a column, so
/// declining is also the right *drawing*. [`column_pose`] prefers upright
/// poses, so the pinned arm reaches the decline only when V14 ∩ V17
/// admits nothing else.
///
/// **End** members have exactly one shared net; that single offset is
/// used unconditionally.
fn column_pin_offsets(
    checked: &CheckedNetlist,
    col: &DcColumn,
    poses: &[Orientation],
) -> Option<Vec<i32>> {
    if col.shared.len() + 1 != col.members.len() || poses.len() != col.members.len() {
        return None;
    }
    let mut out = Vec::with_capacity(col.members.len());
    for (k, &i) in col.members.iter().enumerate() {
        let el = &checked.elements[i];
        let above = k.checked_sub(1).and_then(|j| col.shared.get(j));
        let below = col.shared.get(k);
        let mut off: Option<i32> = None;
        for net in [above, below].into_iter().flatten() {
            let this = shared_pin_x_cells(el, poses[k], net)?;
            match off {
                None => off = Some(this),
                // The pins to the neighbour above and to the neighbour
                // below want different columns; decline the column.
                Some(prev) if prev != this => return None,
                Some(_) => {}
            }
        }
        out.push(off?);
    }
    Some(out)
}

/// Extra cells beyond a body-clean stride between two column members.
///
/// **Measured, not chosen.** The shared node between two column members
/// is a signal net, so decoration puts a plain label on it, and a stride
/// that only clears body-union-pin leaves the label nowhere to go: on
/// `cascode_amp` the `c1` label lands on `Q2`'s body AND on its
/// pin-number text (`v13.1_label_body` and `v13.7_label_pintext`, both
/// Tier 1, both zero-budget). That is precisely the gap CLAUDE.md's "pin
/// text is modelled by NO placement stage" note predicts every
/// repositioning pass will hit — and the lawful remedy is the one
/// [`crate::idioms::apply_series_horizontal`] already uses twice
/// (`SHUNT_LABEL_MARGIN_DOWN_CELLS` / `_UP_CELLS`): a margin on the
/// stride THIS construction owns. It is emphatically not the
/// decoration-reservation programme, measured dead four times (ADR-14
/// post-mortem, ADR-19 M3): nothing here reserves a general text class or
/// widens a band; it widens one stack's own pitch.
///
/// **The sweep**, 0..=5, each a full workspace suite run collected into
/// its own scoreboard sink and graded against `readable-v1`:
///
/// | margin | Tier-1 Δ | Tier-2 Δ | B | f5 | v5 | bends | branches | crossings |
/// | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
/// | *champion* | — | — | 4 | 11 | 42 | 164 | 35 | 14 |
/// | 0 | **+2.00** | −129.74 | 0 | 13 | 50 | 140 | 25 | 11 |
/// | **1** | **+0.00** | **−129.25** | 0 | 13 | 48 | 141 | 31 | 10 |
/// | 2 | +0.00 | −111.00 | 0 | 13 | 49 | 144 | 30 | 9 |
/// | 3 | +0.00 | −117.82 | 0 | 13 | 48 | 142 | 30 | 9 |
/// | 4 | +0.00 | −127.34 | 0 | 13 | 47 | 142 | 32 | 10 |
/// | 5 | **+1.00** | −113.70 | 0 | 13 | 48 | 145 | 31 | 10 |
///
/// **1** is the Tier-1-clean value with the best Tier-2 sum. It is a
/// **plateau, not a knife edge** — 1, 2, 3 and 4 are all Tier-1 clean,
/// which is the property ADR-19 M4's `MID_SUBROW_GAP` table lacked
/// ("passes by one cell of Manhattan tie-break margin… a knife-edge, not
/// a fix"). Only the two ends fail: 0 leaves the `cascode_amp` label
/// nowhere to go, and 5 spreads the column far enough to cost a Tier-1
/// point elsewhere.
const DC_COLUMN_LABEL_MARGIN_CELLS: i32 = 1;

// ---------------------------------------------------------------------------
// Carried rail stubs — a column's own nodes, pin-anchored in BOTH axes
// ---------------------------------------------------------------------------

/// One rail stub carried beside a DC-series column, fully planned before
/// any geometry is written.
///
/// A stub on a column's *shared* net is the bypass capacitor, the
/// emitter bypass, the decoupling cap hanging off a tap. Before this
/// construction its X came from [`crate::idioms::apply_rail_stub_columns`]
/// — which runs BEFORE `apply_dc_columns` and therefore anchors on
/// positions the column then moves — and its Y came from
/// [`crate::bands`], a **sheet-height fraction** that knows nothing about
/// the node it hangs off. Two disagreeing Y authorities, and the stub
/// loses: on `resistor_ladder_ref` `CB2` ended 34 mm below the `t2` tap
/// it bypasses, dragging the `t2` port label down with it.
struct CarriedStub {
    /// Element index of the stub.
    element: usize,
    /// **Slot** (not element index) of the column member the stub is
    /// drawn level with — see [`plan_carried_stubs`] for which member
    /// that is and why.
    member: usize,
    /// The stub's own pin offset (mm, world frame) from its origin on the
    /// shared net, at [`Self::pose`].
    pin: (f64, f64),
    /// The anchor member's pin offset (mm) on the same net, at the pose
    /// the column draws it in. `anchor_dy - pin.1` is the whole Y
    /// construction: it puts the two pins on one horizontal line.
    anchor_dy: f64,
    /// The V14-correct rail facing, from
    /// [`crate::idioms::rail_facing_orientation`], intersected with the
    /// element's [`crate::orient::allowed_orientations`] set.
    pose: Orientation,
    /// Slot in its own `(net, side)` row; slot 0 is nearest the column.
    slot: usize,
    /// Size of that row.
    count: usize,
    /// The row's geometry-derived pitch, from
    /// [`crate::idioms::row_stride_cells`].
    row_stride: i32,
    /// Glyph-inclusive extent at [`Self::pose`] — the reach a `power:*`
    /// glyph and its net-name text add below a ground stub is exactly
    /// what makes two consecutive taps' capacitors collide, so the span
    /// this construction widens the column stride by has to include it.
    ext: WorldExtent,
}

/// Plan every rail stub `col` carries, without moving anything.
///
/// # Which member a stub is drawn level with
///
/// A stub *parallels* the column leg it drops alongside. A ground-side
/// stub on `shared[k]` and the member **below** that node both run down
/// toward the same rail, so they are drawn side by side — that is the
/// conventional emitter-resistor-and-bypass-capacitor drawing, and it is
/// exactly what the owner asked for on `two_stage_amp` ("CE1/RE1
/// previously was aligned horizontally … aligning it cost nothing and
/// looks visually better"). A supply-side stub parallels the member
/// **above** the node, symmetrically.
///
/// Levelling on the *pin* rather than the origin is CLAUDE.md's layout
/// invariant ("relationships between *connecting pins*, not symbol
/// centers") and it is also what makes the wire short: the anchor
/// member's own pin already sits ON the column trunk, so the stub reaches
/// it with one horizontal run and no bend.
///
/// # What declines
///
/// A `(net, side)` group is declined **whole** — never half-applied — when
///
/// * any member is already `pinned` (a user `*@place` / `*@align`, a V7
///   symmetry pin, the ADR-4 layout cache, or an earlier idiom).
///   `apply_rail_stub_columns` records what the other choice cost: a
///   member "skipped without consuming its slot" put a newcomer in a
///   cached element's exact column on `tests/layout_cache.rs`. Declining
///   the group avoids the question entirely;
/// * the anchor member presents no pin on the shared net, or a stub has
///   no V14-legal rail facing inside its `allowed` set. Pinning skips
///   `pick_orientations`, so CLAUDE.md's *consistency requirement* makes
///   choosing a pose this pass's own responsibility — and a pose outside
///   `allowed` would freeze a V14/V17 violation past every enforcer.
///
/// A stub that is itself a column member (a bottom-of-ladder resistor to
/// ground is both) is not carried: the column already owns its geometry.
// One read-only mask, the netlist, the two geometry tables the poses are
// filtered through, the column, its poses, the stub list and the member
// set. Bundling them into a struct would hide which are policy and which
// are geometry, for no benefit at a single call site.
#[allow(clippy::too_many_arguments)]
fn plan_carried_stubs(
    pinned: &[bool],
    checked: &CheckedNetlist,
    allowed: &[Vec<Orientation>],
    prefs: &HashMap<String, VertPref>,
    col: &DcColumn,
    poses: &[Orientation],
    stubs: &[RailStub],
    columned: &HashSet<usize>,
    variant: Placer,
) -> Vec<CarriedStub> {
    // The single gate. An empty plan leaves BOTH halves of the
    // construction inert — the stride widening reads `carried` and the
    // seating iterates it — so the shipping placer's output is unchanged
    // by construction (`baseline_lock` is the empirical half). See
    // [`Placer::DcColumnNodeStubs`] for what it costs, and why that makes
    // it an arm rather than a default-path fix.
    if !variant.dc_column_node_stubs() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (k, net) in col.shared.iter().enumerate() {
        for side in [VertPref::Up, VertPref::Down] {
            let group: Vec<&RailStub> = stubs
                .iter()
                .filter(|s| {
                    s.signal_net == *net && s.side == side && !columned.contains(&s.element)
                })
                .collect();
            if group.is_empty() || group.iter().any(|s| pinned[s.element]) {
                continue;
            }
            // Down-side stubs parallel the leg BELOW the node, up-side
            // ones the leg above it.
            let member = if side == VertPref::Down { k + 1 } else { k };
            let Some((_, anchor_dy)) = crate::idioms::pin_offset_world(
                &checked.elements[col.members[member]],
                poses[member],
                net,
            ) else {
                continue;
            };
            let mut planned: Vec<(usize, Orientation, (f64, f64), WorldExtent)> =
                Vec::with_capacity(group.len());
            let mut declined = false;
            for s in &group {
                let el = &checked.elements[s.element];
                let Some(pose) = crate::idioms::rail_facing_orientation(el, net, side)
                    .filter(|p| allowed.get(s.element).is_some_and(|a| a.contains(p)))
                else {
                    declined = true;
                    break;
                };
                let Some(pin) = crate::idioms::pin_offset_world(el, pose, net) else {
                    declined = true;
                    break;
                };
                planned.push((
                    s.element,
                    pose,
                    pin,
                    crate::world_extent_with_glyphs(el, pose, None, prefs),
                ));
            }
            if declined {
                continue;
            }
            let count = planned.len();
            let row_stride =
                crate::idioms::row_stride_cells(&planned.iter().map(|p| p.3).collect::<Vec<_>>());
            for (slot, (element, pose, pin, ext)) in planned.into_iter().enumerate() {
                out.push(CarriedStub {
                    element,
                    member,
                    pin,
                    anchor_dy,
                    pose,
                    slot,
                    count,
                    row_stride,
                    ext,
                });
            }
        }
    }
    out
}

/// Apply every detected DC-series column: one shared X, members ordered
/// top-to-bottom by DC potential, separated by a geometry-derived
/// vertical stride.
///
/// A column with **any** already-pinned member is skipped whole — an
/// explicit `*@place` / `*@align`, a cache hint, a V7 symmetry pin or a
/// divider pin always wins, and half a column is worse than none.
///
/// Anchoring is **pin-anchored, not centre-anchored** (CLAUDE.md,
/// "Constraints are pin-anchored"): X is the grid-snapped mean of the
/// members' *shared-pin* seed x, and each member's origin is then offset
/// so its own shared-net pin lands on that x — see
/// [`column_pin_offsets`], which also states when a column declines
/// because its two shared pins disagree. The stack is centred on the
/// members' **mean seed Y**, so the construction displaces the component
/// as little as the shape allows. It writes *relative* geometry (a
/// stack), never a page coordinate.
///
/// The column also **carries the rail stubs of its own shared nets** —
/// see [`plan_carried_stubs`]. Each is seated beside the column at its
/// shared pin's Y, so the tap's bypass capacitor sits level with the leg
/// it parallels instead of wherever [`crate::bands`]'s sheet-height
/// fraction left it, and the column's pitch widens on stubbed nodes to
/// clear their glyph-inclusive extents.
pub(crate) fn apply_dc_columns(
    placement: &mut Placement,
    pinned: &mut [bool],
    checked: &CheckedNetlist,
    allowed: &[Vec<Orientation>],
    variant: Placer,
) {
    if !variant.dc_series_columns() {
        return;
    }
    let pin_it = variant.dc_series_columns_pinned();
    let prefs = vertical_prefs(checked);
    let facings = crate::dc_rank::device_facings(checked);
    let stubs = crate::idioms::detect_rail_stubs(checked);
    let columns = detect_dc_columns(checked);
    // Every element the construction owns as a column MEMBER, across all
    // columns. A bottom-of-ladder resistor to ground is both a member and
    // a rail stub; the column already writes its geometry, so it must not
    // also be carried beside itself.
    let columned: HashSet<usize> = columns
        .iter()
        .flat_map(|c| c.members.iter().copied())
        .collect();

    for col in &columns {
        if col.members.iter().any(|&i| pinned[i]) {
            continue;
        }
        // Choose every member's pose BEFORE mutating anything: a column
        // that turns out to have an unposeable member must leave no
        // half-applied geometry behind.
        let mut poses: Vec<Orientation> = Vec::with_capacity(col.members.len());
        let mut declined = false;
        for &i in &col.members {
            let set = allowed.get(i).map_or(&[][..], Vec::as_slice);
            if let Some(o) = column_pose(&checked.elements[i], facings[i], &prefs, set) {
                poses.push(o);
            } else {
                declined = true;
                break;
            }
        }
        if declined {
            continue;
        }

        // Pin-anchoring (CLAUDE.md "Constraints are pin-anchored"): the
        // column's x is a line through the members' SHARED PINS, not
        // through their origins. `offsets[k]` is what member `k`'s origin
        // sits left of that line by, at the pose it is drawn in.
        // `None` = the column's own geometry cannot put every shared pin
        // on one x; decline it whole, before anything is mutated.
        let Some(offsets) = column_pin_offsets(checked, col, &poses) else {
            continue;
        };

        // The rail stubs this column carries. Planned here, before any
        // geometry is written, for the reason the poses are: a group that
        // turns out to be unposeable must leave nothing half-applied.
        let carried = plan_carried_stubs(
            pinned, checked, allowed, &prefs, col, &poses, &stubs, &columned, variant,
        );

        // Strides between consecutive members, at the poses above.
        let exts: Vec<WorldExtent> = col
            .members
            .iter()
            .zip(&poses)
            .map(|(&i, &o)| world_extent(&checked.elements[i].symbol, o, None))
            .collect();
        // The Y span each member's carried stubs occupy, in that member's
        // OWN origin frame — the stub is levelled on the member's pin, so
        // it travels with it and its reach is part of what the column's
        // pitch has to clear.
        let mut carried_span: Vec<Option<(f64, f64)>> = vec![None; col.members.len()];
        for c in &carried {
            let d = c.anchor_dy - c.pin.1;
            let (lo, hi) = (c.ext.min_y + d, c.ext.max_y + d);
            let slot = &mut carried_span[c.member];
            *slot = Some(match *slot {
                None => (lo, hi),
                Some((a, b)) => (a.min(lo), b.max(hi)),
            });
        }
        let strides: Vec<i32> = (0..exts.len() - 1)
            .map(|k| {
                let body = vertical_stride_cells(&exts[k], &exts[k + 1]);
                // Two consecutive taps' stubs share one X lane, so the
                // pitch that clears the BODIES does not clear them: a
                // `Device:C` spans 7.62 mm pin to pin and its ground glyph
                // and net-name text reach 3.81 mm further, against a
                // column pitch of 10.16 mm. That is the zero-budget
                // `v13.7_label_pintext` class CLAUDE.md predicts every
                // repositioning pass will hit, and the lawful remedy is
                // the one `DC_COLUMN_LABEL_MARGIN_CELLS` already uses:
                // widen the stride THIS construction owns, derived from
                // `glyph_geom` reach rather than tuned. (Alternating sides
                // was the other candidate and is rejected: it breaks the
                // moment one tap carries two stubs.)
                let carried_need = match (carried_span[k], carried_span[k + 1]) {
                    (Some(a), Some(b)) => {
                        crate::mm_up_to_cells(a.1 - b.0 + crate::MIN_CLEARANCE_MM)
                    }
                    _ => 0,
                };
                body.max(carried_need) + DC_COLUMN_LABEL_MARGIN_CELLS
            })
            .collect();

        // Anchor: the component's own seed barycenter, so the column
        // moves as little as its shape allows.
        let n = i32::try_from(col.members.len()).unwrap_or(i32::MAX);
        // The barycenter is taken over the SHARED PINS' seed x, so the
        // pins — the things that must end up collinear — move as little
        // as the shape allows.
        let sum_x: i32 = col
            .members
            .iter()
            .zip(&offsets)
            .map(|(&i, &d)| placement.elements[i].origin.x + d)
            .sum();
        let sum_y: i32 = col
            .members
            .iter()
            .map(|&i| placement.elements[i].origin.y)
            .sum();
        let total: i32 = strides.iter().sum();
        let x = sum_x.div_euclid(n);
        let mut y = sum_y.div_euclid(n) - total / 2;

        let mut member_y: Vec<i32> = Vec::with_capacity(col.members.len());
        for (k, &i) in col.members.iter().enumerate() {
            member_y.push(y);
            placement.elements[i].origin = GridPoint::new(x - offsets[k], y);
            if pin_it {
                placement.elements[i].orientation = poses[k];
                pinned[i] = true;
            }
            if let Some(s) = strides.get(k) {
                y += *s;
            }
        }

        seat_carried_stubs(
            placement, pinned, checked, &prefs, &carried, col, &exts, &member_y, x, pin_it,
        );
    }
}

/// Write the geometry of one column's carried rail stubs: beside the
/// column, level with each one's anchor member's shared pin.
///
/// # One side for the whole column
///
/// A ladder of taps must read as a second regular column, not a zigzag
/// (the owner: "circuit is pretty regular, but the way terminals is
/// connected are pretty non-regular"). So the side is chosen once, for
/// the column, by two keys in order:
///
/// 1. **How many foreign bodies the row would land on.** A side is not
///    free just because the column clears it: on `cascode_amp` the bias
///    ladder's right-hand side is where the device stack lives, and a
///    row seated there overlaps `Q2` — which `legalize` then repairs by
///    shoving `Q2` out of its own column, undoing the construction two
///    columns away. Counting the collision here is what keeps the repair
///    from being needed.
/// 2. **Which side the seed already leaned to**, so an unobstructed
///    choice displaces the group as little as the shape allows. Ties go
///    right.
///
/// # How far out
///
/// Geometry, never a tuned constant: the widest reach toward the stubs of
/// the column members the row can ACTUALLY clip — those whose Y span
/// overlaps a stub's — plus the widest stub's reach back toward the
/// column, plus [`crate::MIN_CLEARANCE_MM`], grid-snapped up.
///
/// The Y-overlap filter is the difference between a correct stride and a
/// merely safe one, and it is worth 2 cells on every fixture with a
/// transistor in its column: a bypass capacitor level with an *emitter
/// resistor* is nowhere near the BJT two rows above it, so the BJT's
/// width has no business setting the run. Taking the max over all members
/// unconditionally measured 6 cells where 4 clears everything — and F6
/// (rail-stub lateral run) prices exactly that difference, on five
/// fixtures at once. It is also the owner's other report: "don't make
/// this wires too long".
// The placement and pin mask it writes, the netlist and glyph prefs the
// collision test reads, the plan, the column (members, extents, Y and x)
// and whether to pin. A struct would only rename them.
#[allow(clippy::too_many_arguments)]
fn seat_carried_stubs(
    placement: &mut Placement,
    pinned: &mut [bool],
    checked: &CheckedNetlist,
    prefs: &HashMap<String, VertPref>,
    carried: &[CarriedStub],
    col: &DcColumn,
    exts: &[WorldExtent],
    member_y: &[i32],
    x: i32,
    pin_it: bool,
) {
    if carried.is_empty() {
        return;
    }
    let lean: i64 = carried
        .iter()
        .map(|c| i64::from(placement.elements[c.element].origin.x - x))
        .sum();
    let leaned = if lean < 0 { -1 } else { 1 };

    // Everything the row could land on: not a member of this column, and
    // not one of the stubs being seated.
    let own: HashSet<usize> = col
        .members
        .iter()
        .copied()
        .chain(carried.iter().map(|c| c.element))
        .collect();
    let foreign: Vec<(f64, f64, f64, f64)> = (0..checked.elements.len())
        .filter(|i| !own.contains(i))
        .map(|i| {
            let e = crate::world_extent_with_glyphs(
                &checked.elements[i],
                placement.elements[i].orientation,
                None,
                prefs,
            );
            let (ox, oy) = placement.elements[i].origin.to_mm();
            (ox + e.min_x, ox + e.max_x, oy + e.min_y, oy + e.max_y)
        })
        .collect();

    let mut best: Option<(usize, u8, i32, i32)> = None;
    for side in [1_i32, -1] {
        let gap = carried_gap_cells(carried, exts, member_y, side > 0);
        let mut hits = 0_usize;
        for c in carried {
            let (lo_x, hi_x, lo_y, hi_y) = stub_box(c, x, side, gap, member_y);
            hits += foreign
                .iter()
                .filter(|(a, b, p, q)| lo_x < *b && *a < hi_x && lo_y < *q && *p < hi_y)
                .count();
        }
        let lean_rank = u8::from(side != leaned);
        let key = (hits, lean_rank, side, gap);
        if best.is_none_or(|b| (key.0, key.1) < (b.0, b.1)) {
            best = Some(key);
        }
    }
    let Some((_, _, side, gap)) = best else {
        return;
    };

    for c in carried {
        let slot_off =
            crate::idioms::row_slot_offset_cells(c.slot, c.count, c.row_stride, RowAnchor::Outward);
        #[allow(clippy::cast_possible_truncation)]
        let dx = (c.pin.0 / GridPoint::STEP_MM).round() as i32;
        placement.elements[c.element].origin = GridPoint::new(
            x + side * (gap + slot_off) - dx,
            member_y[c.member] + stub_dy_cells(c),
        );
        if pin_it {
            placement.elements[c.element].orientation = c.pose;
            pinned[c.element] = true;
        }
    }
}

/// Cells from the anchor member's origin to the stub's, so the two pins
/// on the shared net land on one horizontal line.
#[allow(clippy::cast_possible_truncation)]
fn stub_dy_cells(c: &CarriedStub) -> i32 {
    ((c.anchor_dy - c.pin.1) / GridPoint::STEP_MM).round() as i32
}

/// World bbox (mm) a carried stub would occupy at `side` / `gap`.
fn stub_box(
    c: &CarriedStub,
    x: i32,
    side: i32,
    gap: i32,
    member_y: &[i32],
) -> (f64, f64, f64, f64) {
    let slot_off =
        crate::idioms::row_slot_offset_cells(c.slot, c.count, c.row_stride, RowAnchor::Outward);
    #[allow(clippy::cast_possible_truncation)]
    let dx = (c.pin.0 / GridPoint::STEP_MM).round() as i32;
    let ox = f64::from(x + side * (gap + slot_off) - dx) * GridPoint::STEP_MM;
    let oy = f64::from(member_y[c.member] + stub_dy_cells(c)) * GridPoint::STEP_MM;
    (
        ox + c.ext.min_x,
        ox + c.ext.max_x,
        oy + c.ext.min_y,
        oy + c.ext.max_y,
    )
}

/// Cells from the column's shared-pin line to the nearest carried-stub
/// slot, on the given side.
///
/// Only the members whose Y span the stub's own span actually reaches are
/// consulted — see [`seat_carried_stubs`] for why that filter is the
/// whole difference between a 4-cell run and a 6-cell one.
fn carried_gap_cells(
    carried: &[CarriedStub],
    exts: &[WorldExtent],
    member_y: &[i32],
    right: bool,
) -> i32 {
    let mut gap = 1;
    for c in carried {
        let oy = f64::from(member_y[c.member] + stub_dy_cells(c)) * GridPoint::STEP_MM;
        let (lo, hi) = (oy + c.ext.min_y, oy + c.ext.max_y);
        let mut member_reach = 0.0_f64;
        for (m, e) in exts.iter().enumerate() {
            let my = f64::from(member_y[m]) * GridPoint::STEP_MM;
            // Clearance-padded overlap: a member the stub merely grazes
            // still has to be cleared sideways.
            if my + e.max_y + crate::MIN_CLEARANCE_MM <= lo
                || hi + crate::MIN_CLEARANCE_MM <= my + e.min_y
            {
                continue;
            }
            member_reach = member_reach.max(if right { e.max_x } else { -e.min_x });
        }
        let stub_reach = if right { -c.ext.min_x } else { c.ext.max_x };
        gap = gap.max(crate::mm_up_to_cells(
            member_reach + stub_reach + crate::MIN_CLEARANCE_MM,
        ));
    }
    gap
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use kicad_symbols::Library;
    use spice_diagnostics::FileId;
    use spice_policy::{CheckedNetlist, check};

    use kicad_symbols::{Orientation, Rotation};

    use std::collections::HashSet;

    use super::{
        column_pin_offsets, dc_series_pairs, detect_dc_columns, plan_carried_stubs, vertical_prefs,
    };
    use crate::GridPoint;
    use crate::placer::Placer;

    fn fixture_library() -> &'static Library {
        static LIB: OnceLock<Library> = OnceLock::new();
        LIB.get_or_init(|| {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let dir = manifest
                .parent()
                .and_then(std::path::Path::parent)
                .expect("workspace root")
                .join("crates/kicad-symbols/tests/fixtures");
            Library::from_file(dir.join("Device.kicad_sym"))
                .expect("Device fixture library")
                .merge(
                    Library::from_file(dir.join("Simulation_SPICE.kicad_sym"))
                        .expect("Simulation_SPICE fixture library"),
                )
        })
    }

    fn checked_of(src: &str) -> CheckedNetlist {
        let parsed = spice_parser::parse(src, FileId(0))
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        check(resolved).expect("policy check failed").0
    }

    fn refdes(checked: &CheckedNetlist, i: usize) -> &str {
        &checked.elements[i].refdes
    }

    fn pair_names(src: &str) -> Vec<(String, String, String)> {
        let c = checked_of(src);
        dc_series_pairs(&c)
            .into_iter()
            .map(|(u, v, n)| {
                (
                    refdes(&c, u).to_string(),
                    refdes(&c, v).to_string(),
                    n.clone(),
                )
            })
            .collect()
    }

    fn column_names(src: &str) -> Vec<Vec<String>> {
        let c = checked_of(src);
        detect_dc_columns(&c)
            .into_iter()
            .map(|col| {
                col.members
                    .iter()
                    .map(|&i| refdes(&c, i).to_string())
                    .collect()
            })
            .collect()
    }

    const HDR: &str = "test\n*@symbol Device:R_US for=R*\n*@symbol Device:C for=C*\n\
                       *@symbol Device:Q_NPN_BCE for=Q*\n";

    /// The motivating specimen. `cascode_amp` presents two independent
    /// current paths — the bias ladder and the device stack — and the
    /// construction produces exactly two columns, each in textbook
    /// supply-to-ground order.
    #[test]
    fn cascode_yields_the_bias_ladder_and_the_device_stack() {
        let src = format!(
            "{HDR}VCC vcc 0 DC 12 ;@ power=+12V\n\
             RB1 vcc b2 47k\nRB2 b2 b1 22k\nRB3 b1 0 10k\nCB2 b2 0 10u\n\
             RC vcc c2 4k7\nQ2 c2 b2 c1 QGENERIC\nQ1 c1 b1 e1 QGENERIC\n\
             RE e1 0 470\nCE e1 0 100u\n.model QGENERIC NPN\n.end\n"
        );
        let cols = column_names(&src);
        assert!(
            cols.contains(&vec![
                "RC".to_string(),
                "Q2".to_string(),
                "Q1".to_string(),
                "RE".to_string()
            ]),
            "device stack missing or misordered: {cols:?}"
        );
        assert!(
            cols.contains(&vec![
                "RB1".to_string(),
                "RB2".to_string(),
                "RB3".to_string()
            ]),
            "bias ladder missing or misordered: {cols:?}"
        );
        assert_eq!(cols.len(), 2, "{cols:?}");
    }

    /// Metric B's own honesty check, restated on the construction: a
    /// differential pair shares `tail` with `RTAIL`, so that net has DC
    /// degree 3, the transistors are NOT a DC-series pair, and the
    /// construction must not column them. A diff pair is drawn side by
    /// side on purpose.
    #[test]
    fn a_differential_pair_is_not_a_column() {
        let src = format!(
            "{HDR}VCC vcc 0 DC 12 ;@ power=+12V\nVEE vee 0 DC -12 ;@ power=-12V\n\
             RC1 vcc c1 4k7\nRC2 vcc c2 4k7\nRTAIL tail vee 2k2\n\
             Q1 c1 in1 tail QGENERIC\nQ2 c2 in2 tail QGENERIC\n.model QGENERIC NPN\n.end\n"
        );
        let cols = column_names(&src);
        for c in &cols {
            assert!(
                !(c.contains(&"Q1".to_string()) && c.contains(&"Q2".to_string())),
                "the diff pair was columned: {cols:?}"
            );
        }
    }

    /// A six-resistor reference string is ONE column of six, in supply
    /// -to-ground order — the `resistor_ladder_ref` shape. The bypass
    /// capacitors conduct no DC, so they neither join the column nor
    /// raise the taps' DC degree.
    #[test]
    fn a_resistor_string_is_one_column_in_supply_to_ground_order() {
        let src = format!(
            "{HDR}VDD vdd 0 DC 12 ;@ power=+12V\n\
             R1 vdd t1 10k\nR2 t1 t2 10k\nR3 t2 t3 10k\nR4 t3 t4 10k\n\
             R5 t4 t5 10k\nR6 t5 0 10k\nCB2 t2 0 100n\nCB3 t3 0 100n\n.end\n"
        );
        assert_eq!(
            column_names(&src),
            vec![vec!["R1", "R2", "R3", "R4", "R5", "R6"]]
        );
    }

    /// A capacitor blocks DC, so an RC-coupled pair is not in series for
    /// this construction's purposes and no column forms.
    #[test]
    fn a_coupling_capacitor_breaks_the_column() {
        let src = format!("{HDR}VCC vcc 0 DC 12 ;@ power=+12V\nR1 in out 1k\nC1 out 0 1u\n.end\n");
        assert_eq!(pair_names(&src), Vec::new());
        assert_eq!(column_names(&src), Vec::<Vec<String>>::new());
    }

    /// **The decline.** A floating series pair reaches no rail through
    /// any DC conductor, so `dc_rank` cannot order it — and neither can
    /// this construction, which declines rather than guessing. (It is
    /// already excluded one step earlier by the rail-to-rail clause;
    /// the assertion is that nothing downstream invents an order.)
    #[test]
    fn a_floating_series_pair_declines() {
        let src = format!("{HDR}R1 na nb 1k\nR2 nb nc 1k\n.end\n");
        assert_eq!(column_names(&src), Vec::<Vec<String>>::new());
    }

    /// The netlist behind the pin-anchoring tests: one device stack,
    /// `RC` above `Q1` above `RE`, sharing `c` and `e`.
    const STACK: &str = "VCC vcc 0 DC 12 ;@ power=+12V\n\
                         RB vcc b 100k\nRC vcc c 4k7\nQ1 c b e QGENERIC\n\
                         RE e 0 470\n.model QGENERIC NPN\n.end\n";

    /// **The pin-anchoring property** (CLAUDE.md: "Constraints are
    /// pin-anchored … not symbol centers").
    ///
    /// `Device:R_US`'s pins sit on its origin's x, but
    /// `Device:Q_NPN_BCE`'s collector AND emitter are both 2.54 mm — two
    /// grid cells — to the right of the origin. Anchoring the column on
    /// origins therefore aligns the *bodies* and leaves a two-cell jog in
    /// the collector wire; the offsets below are what removes it.
    #[test]
    fn a_column_is_anchored_on_its_shared_pins_not_its_origins() {
        let src = format!("{HDR}{STACK}");
        let c = checked_of(&src);
        let cols = detect_dc_columns(&c);
        assert_eq!(cols.len(), 1, "{cols:?}");
        let col = &cols[0];
        assert_eq!(
            col.members
                .iter()
                .map(|&i| refdes(&c, i))
                .collect::<Vec<_>>(),
            vec!["RC", "Q1", "RE"]
        );
        assert_eq!(col.shared.len(), col.members.len() - 1);
        let poses = vec![Orientation::IDENTITY; col.members.len()];
        assert_eq!(
            column_pin_offsets(&c, col, &poses),
            Some(vec![0, 2, 0]),
            "the BJT's shared pins are 2 cells right of its origin; the resistors' are on it"
        );
    }

    /// **The two-shared-pins decline.** An interior member has a shared
    /// pin to the neighbour above AND one to the neighbour below. Posed
    /// sideways, `Q_NPN_BCE`'s collector and emitter land on OPPOSITE
    /// sides of the origin, so no single column x carries both — and the
    /// construction declines the whole column rather than picking a side
    /// and re-introducing the jog on the other neighbour.
    #[test]
    fn a_member_whose_two_shared_pins_disagree_declines_the_column() {
        let src = format!("{HDR}{STACK}");
        let c = checked_of(&src);
        let cols = detect_dc_columns(&c);
        let col = &cols[0];
        let sideways = Orientation {
            rotation: Rotation::R90,
            mirror_y: false,
        };
        // Q1 sideways: C lands at -5.08, E at +5.08.
        let poses = vec![Orientation::IDENTITY, sideways, Orientation::IDENTITY];
        assert_eq!(column_pin_offsets(&c, col, &poses), None);
    }

    /// An **end** member has only one shared pin, and that single offset
    /// is used unconditionally — a sideways *end* resistor still columns,
    /// on whichever of its pins the column actually shares.
    #[test]
    fn an_end_member_with_one_shared_pin_still_anchors() {
        let src = format!("{HDR}{STACK}");
        let c = checked_of(&src);
        let cols = detect_dc_columns(&c);
        let col = &cols[0];
        let sideways = Orientation {
            rotation: Rotation::R90,
            mirror_y: false,
        };
        let poses = vec![sideways, Orientation::IDENTITY, Orientation::IDENTITY];
        let offs = column_pin_offsets(&c, col, &poses).expect("an end member cannot disagree");
        assert_eq!(offs.len(), 3);
        assert_ne!(
            offs[0], 0,
            "a sideways resistor's pin is off its origin's x"
        );
        assert_eq!(&offs[1..], &[2, 0]);
    }

    /// Columns are emitted in a deterministic order and never share a
    /// member, so the caller can apply them independently.
    #[test]
    fn columns_are_disjoint_and_deterministic() {
        let src = format!(
            "{HDR}VCC vcc 0 DC 12 ;@ power=+12V\n\
             RB1 vcc b2 47k\nRB2 b2 b1 22k\nRB3 b1 0 10k\nCB2 b2 0 10u\n\
             RC vcc c2 4k7\nQ2 c2 b2 c1 QGENERIC\nQ1 c1 b1 e1 QGENERIC\n\
             RE e1 0 470\n.model QGENERIC NPN\n.end\n"
        );
        let c = checked_of(&src);
        let a = detect_dc_columns(&c);
        let b = detect_dc_columns(&c);
        assert_eq!(a, b);
        let mut seen = std::collections::HashSet::new();
        for col in &a {
            for m in &col.members {
                assert!(seen.insert(*m), "member {m} in two columns");
            }
        }
    }

    // -----------------------------------------------------------------
    // Carried rail stubs
    // -----------------------------------------------------------------

    /// A ladder with a bypass capacitor on two consecutive taps — the
    /// `resistor_ladder_ref` shape, cut to the two taps that make
    /// consecutive-stub spacing observable.
    const LADDER: &str = "VDD vdd 0 DC 12 ;@ power=+12V\n\
                          R1 vdd t1 10k\nR2 t1 t2 10k\nR3 t2 t3 10k\n\
                          R4 t3 t4 10k\nR5 t4 0 10k\n\
                          CB2 t2 0 100n\nCB3 t3 0 100n\n.end\n";

    /// Seed-only placement (no SA), so "did this move?" is attributable
    /// to the seed pass under test — the `idioms.rs` `seed_with` rule.
    fn seed(src: &str) -> (CheckedNetlist, crate::Placement) {
        let checked = checked_of(src);
        let placement = crate::place_with(
            checked.clone(),
            fixture_library(),
            &crate::LayoutOptions {
                refine: false,
                placer: Placer::DcColumnNodeStubs,
                ..crate::LayoutOptions::default()
            },
        )
        .expect("place");
        (checked, placement)
    }

    fn index_of(checked: &CheckedNetlist, r: &str) -> usize {
        checked
            .elements
            .iter()
            .position(|e| e.refdes == r)
            .unwrap_or_else(|| panic!("{r} present"))
    }

    /// World `(x, y)` mm of `refdes`'s pin on `net`, as placed.
    fn pin_at(
        checked: &CheckedNetlist,
        placement: &crate::Placement,
        refdes: &str,
        net: &str,
    ) -> (f64, f64) {
        let i = index_of(checked, refdes);
        let (dx, dy) = crate::idioms::pin_offset_world(
            &checked.elements[i],
            placement.elements[i].orientation,
            net,
        )
        .expect("pin on net");
        let (ox, oy) = placement.elements[i].origin.to_mm();
        (ox + dx, oy + dy)
    }

    /// **The Y half of the construction, and the negative control.**
    ///
    /// A ground-side bypass capacitor parallels the ladder leg *below*
    /// its tap, so its own tap pin lands on exactly the same horizontal
    /// line as that leg's — CLAUDE.md's "constraints are pin-anchored",
    /// now in both axes rather than X only.
    ///
    /// This is what fails on the pre-fix code. Before the carry,
    /// `apply_rail_stub_columns` moved a stub in **X only** ("the stub's
    /// Y is left exactly as the band seeder placed it") while
    /// `apply_dc_columns` re-seated its tap constructively from
    /// `dc_rank` — two disagreeing Y authorities, and the stub lost by a
    /// whole band. Measured on the shipping `resistor_ladder_ref`: `CB2`
    /// emitted at y = 86.36 for a `t2` tap at y = 52.07.
    #[test]
    fn a_carried_stub_lands_on_its_tap_pin_line() {
        let src = format!("{HDR}{LADDER}");
        let (checked, p) = seed(&src);
        for (cap, leg, net) in [("CB2", "R3", "t2"), ("CB3", "R4", "t3")] {
            let (cx, cy) = pin_at(&checked, &p, cap, net);
            let (lx, ly) = pin_at(&checked, &p, leg, net);
            assert!(
                (cy - ly).abs() < 1e-6,
                "{cap}'s {net} pin (y = {cy}) must sit on {leg}'s ({ly})"
            );
            assert!(
                (cx - lx).abs() > GridPoint::STEP_MM / 2.0,
                "{cap} must sit BESIDE the column, not in it (both at x = {cx})"
            );
        }
    }

    /// **The X half.** The run in is one horizontal segment whose length
    /// is the geometry: the widest column member's reach toward the
    /// stubs plus the widest stub's reach back, plus one clearance cell,
    /// rounded up to the grid. Nothing tuned, and nothing long — the
    /// owner's other report was that `readable-v1`'s stub wires were too
    /// long.
    #[test]
    fn a_carried_stub_sits_one_geometry_derived_stride_from_the_column() {
        let src = format!("{HDR}{LADDER}");
        let (checked, p) = seed(&src);
        let (cx, _) = pin_at(&checked, &p, "CB2", "t2");
        let (lx, _) = pin_at(&checked, &p, "R3", "t2");
        let widest = |r: &str| {
            let i = index_of(&checked, r);
            crate::world_extent(&checked.elements[i].symbol, p.elements[i].orientation, None)
        };
        let need = widest("R3").max_x - widest("CB2").min_x + crate::MIN_CLEARANCE_MM;
        let run = (cx - lx).abs();
        assert!(
            run >= need - 1e-6,
            "the run ({run}) must clear both bodies ({need})"
        );
        assert!(
            run <= need + GridPoint::STEP_MM,
            "the run ({run}) must be the SMALLEST such stride, not a page-scale detour \
             (grid-snapped {need})"
        );
        // Both taps' capacitors take the SAME side, so the taps read as a
        // second regular column rather than a zigzag. (The owner: "circuit
        // is pretty regular, but the way terminals is connected are pretty
        // non-regular".)
        let (c3x, _) = pin_at(&checked, &p, "CB3", "t3");
        let (l4x, _) = pin_at(&checked, &p, "R4", "t3");
        assert!(
            (cx - lx) * (c3x - l4x) > 0.0,
            "the two carried stubs took opposite sides of the column"
        );
        assert!((cx - c3x).abs() < 1e-6, "and they share one column x");
    }

    /// Consecutive taps' stubs share one X lane, so the column pitch has
    /// to clear their **glyph-inclusive** extents, not just their bodies:
    /// a `Device:C` spans 7.62 mm pin to pin and its GND glyph and
    /// net-name text reach 3.81 mm further, against a 10.16 mm body-clean
    /// pitch. This is the `v13.7_label_pintext` class CLAUDE.md predicts,
    /// and the remedy is the stride *this construction owns*.
    #[test]
    fn consecutive_carried_stubs_clear_each_other() {
        let src = format!("{HDR}{LADDER}");
        let (checked, p) = seed(&src);
        let prefs = vertical_prefs(&checked);
        let span = |r: &str| {
            let i = index_of(&checked, r);
            let e = crate::world_extent_with_glyphs(
                &checked.elements[i],
                p.elements[i].orientation,
                None,
                &prefs,
            );
            let oy = p.elements[i].origin.to_mm().1;
            (oy + e.min_y, oy + e.max_y)
        };
        let (a_lo, a_hi) = span("CB2");
        let (b_lo, b_hi) = span("CB3");
        assert!(
            a_hi + crate::MIN_CLEARANCE_MM <= b_lo || b_hi + crate::MIN_CLEARANCE_MM <= a_lo,
            "CB2 [{a_lo}, {a_hi}] and CB3 [{b_lo}, {b_hi}] overlap, glyphs included"
        );
    }

    /// A **member** of the column is never carried beside itself. `R5`
    /// here is both the ladder's bottom leg and a ground rail stub on
    /// `t4`; the column already owns its geometry.
    #[test]
    fn a_column_member_is_not_carried_beside_itself() {
        let src = format!("{HDR}{LADDER}");
        let (checked, p) = seed(&src);
        let col_x = pin_at(&checked, &p, "R4", "t3").0;
        let r5_x = pin_at(&checked, &p, "R5", "t4").0;
        assert!(
            (col_x - r5_x).abs() < 1e-6,
            "R5 is a column member and must stay on the column x ({col_x}), got {r5_x}"
        );
    }

    /// **The decline.** A `(net, side)` group with ANY already-pinned
    /// member is declined WHOLE — never half-applied.
    ///
    /// `apply_rail_stub_columns` records what the other choice costs: a
    /// member "skipped without consuming its slot" put a newcomer in a
    /// cached element's exact column on `tests/layout_cache.rs`. The pin
    /// can come from a user `*@place` / `*@align`, V7 symmetry, an
    /// earlier idiom or the ADR-4 layout cache, so this is asserted on the
    /// mask itself rather than through one directive that produces it.
    #[test]
    fn one_pinned_member_declines_the_whole_carried_group() {
        let src = format!(
            "{HDR}VCC vcc 0 DC 12 ;@ power=+12V\n\
             RB vcc b 100k\nRC vcc c 4k7\nQ1 c b e QGENERIC\n\
             RE e 0 470\nCE e 0 100u\nCE2 e 0 10u\n.model QGENERIC NPN\n.end\n"
        );
        let c = checked_of(&src);
        let cols = detect_dc_columns(&c);
        let col = cols
            .iter()
            .find(|col| col.shared.iter().any(|n| n == "e"))
            .expect("the RC/Q1/RE stack");
        let prefs = vertical_prefs(&c);
        let allowed = crate::orient::allowed_orientations(&c, Placer::default());
        let stubs = crate::idioms::detect_rail_stubs(&c);
        let columned: HashSet<usize> = cols
            .iter()
            .flat_map(|x| x.members.iter().copied())
            .collect();
        let poses = vec![Orientation::IDENTITY; col.members.len()];

        let free = vec![false; c.elements.len()];
        let carried = plan_carried_stubs(
            &free,
            &c,
            &allowed,
            &prefs,
            col,
            &poses,
            &stubs,
            &columned,
            Placer::DcColumnNodeStubs,
        );
        assert_eq!(
            carried.len(),
            2,
            "both bypass caps on `e` are carried when nothing is pinned"
        );

        let mut pinned = free.clone();
        pinned[c
            .elements
            .iter()
            .position(|e| e.refdes == "CE2")
            .expect("CE2")] = true;
        let carried = plan_carried_stubs(
            &pinned,
            &c,
            &allowed,
            &prefs,
            col,
            &poses,
            &stubs,
            &columned,
            Placer::DcColumnNodeStubs,
        );
        assert!(
            carried.is_empty(),
            "one pinned member must decline the whole group, not half-apply it: {:?}",
            carried.iter().map(|x| x.element).collect::<Vec<_>>()
        );
    }
}
