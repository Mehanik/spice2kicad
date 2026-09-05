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
        for w in path.windows(2) {
            let (u, v) = (w[0], w[1]);
            let Some(net) = shared.get(&(u, v)) else {
                ok = false;
                break;
            };
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
        }
        columns.push(DcColumn { members });
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

/// Apply every detected DC-series column: one shared X, members ordered
/// top-to-bottom by DC potential, separated by a geometry-derived
/// vertical stride.
///
/// A column with **any** already-pinned member is skipped whole — an
/// explicit `*@place` / `*@align`, a cache hint, a V7 symmetry pin or a
/// divider pin always wins, and half a column is worse than none.
///
/// Anchoring: X is the grid-snapped **mean** of the members' seed
/// columns and the stack is centred on their **mean seed Y**, so the
/// construction displaces the component as little as the shape allows.
/// It writes *relative* geometry (a stack), never a page coordinate.
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

    for col in detect_dc_columns(checked) {
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

        // Strides between consecutive members, at the poses above.
        let exts: Vec<WorldExtent> = col
            .members
            .iter()
            .zip(&poses)
            .map(|(&i, &o)| world_extent(&checked.elements[i].symbol, o, None))
            .collect();
        let strides: Vec<i32> = exts
            .windows(2)
            .map(|w| vertical_stride_cells(&w[0], &w[1]) + DC_COLUMN_LABEL_MARGIN_CELLS)
            .collect();

        // Anchor: the component's own seed barycenter, so the column
        // moves as little as its shape allows.
        let n = i32::try_from(col.members.len()).unwrap_or(i32::MAX);
        let sum_x: i32 = col
            .members
            .iter()
            .map(|&i| placement.elements[i].origin.x)
            .sum();
        let sum_y: i32 = col
            .members
            .iter()
            .map(|&i| placement.elements[i].origin.y)
            .sum();
        let total: i32 = strides.iter().sum();
        let x = sum_x.div_euclid(n);
        let mut y = sum_y.div_euclid(n) - total / 2;

        for (k, &i) in col.members.iter().enumerate() {
            placement.elements[i].origin = GridPoint::new(x, y);
            if pin_it {
                placement.elements[i].orientation = poses[k];
                pinned[i] = true;
            }
            if let Some(s) = strides.get(k) {
                y += *s;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use kicad_symbols::Library;
    use spice_diagnostics::FileId;
    use spice_policy::{CheckedNetlist, check};

    use super::{dc_series_pairs, detect_dc_columns};

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
}
