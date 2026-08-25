//! DC-potential rank over nets, and the device facing it implies (F2).
//!
//! # The defect this exists for
//!
//! Phase 4.5's at-risk sweep (`kicad_emitter::refine`) is
//! **offender-gated**: an element becomes a candidate only when it
//! currently carries a V5 first-segment violation or a V12 wire speared
//! through its body. That is a cost bound, and it has a blind spot the
//! owner reported on `two_stage_amp`: the seed emits both `Q1` and `Q2`
//! upside down (rot 180 + mirror), phase 4.5 repaired `Q1`, and never so
//! much as *looked* at `Q2` — because at its post-SA position the flipped
//! `Q2` is violation-free. Both its first segments leave outward. The
//! cost is a 35 mm bypass wire and a transistor drawn emitter-up, and no
//! trigger in the phase can see either.
//!
//! Proof that acceptance was not the blocker: with the SA disabled
//! (`--refine-iterations 0`) phase 4.5 flips *both* transistors to rot 0.
//! **Reach, not acceptance, is what saved `Q2`.** So the repair is a third
//! at-risk trigger, and this module computes it.
//!
//! # The convention, and how it is derived without a device library
//!
//! A reader expects a transistor's **higher-DC-potential terminal drawn
//! screen-up**: an NPN's collector above its emitter, current running
//! down the page from supply to ground. Everything needed to decide that
//! is in the SPICE source — terminal identity from the element syntax
//! (`Q c b e`, `M d g s b`), and the potential order from where those
//! nets sit relative to the rails. **No `.model` card and no KiCad symbol
//! name is consulted**, which is what makes the rule polarity-agnostic:
//! a PNP's emitter is the terminal nearer the positive supply, so the
//! same comparison puts *it* up with no special case.
//!
//! ## The rank
//!
//! Build an undirected **DC graph** over nets. An edge is a path a DC
//! current can actually take through one drawn element:
//!
//! * two-terminal conductors — R, L, D, and a *drawn* V/I source — join
//!   their two nodes;
//! * a BJT joins collector–emitter, a FET drain–source. The base / gate
//!   is deliberately not an edge: it is a control terminal, and treating
//!   it as a conductor would fuse every bias node into the current path.
//!   (Same choice, for the same reason, as ADR-28 metric B's `dc_edge`.)
//! * a **capacitor has no edge at all** — it blocks DC, which is exactly
//!   what keeps one RC-coupled stage's ranks independent of the next's;
//! * a `;@ power=` source is a rail glyph rather than a drawn body, and a
//!   hierarchical `(sheet …)` instance has no single current path
//!   through it, so neither contributes an edge.
//!
//! Then run two multi-source BFS's: `up(n)` = hops from `n` to the
//! nearest **`VertPref::Up`** rail (a positive supply), `dn(n)` = hops to
//! the nearest **`VertPref::Down`** rail (ground, or a negative supply).
//! Reading the polarity off [`crate::net_class::VertPref`] rather than
//! off a net *name* is what makes `named_rails`-style circuits work, and
//! it is the same classification V14 keys off.
//!
//! A rail is **absorbing**: reaching one records the distance and stops.
//! Current entering a rail leaves through the supply, not out the far
//! side into the next signal net, and letting the walk continue would
//! manufacture paths that run *through* the power supply.
//!
//! ## The comparison, and when it declines
//!
//! For a device with conduction terminals `a` (SPICE index 0 — collector
//! / drain) and `b` (index 2 — emitter / source), the walk is run with
//! **the device's own edge removed**. Ranking a device by a path through
//! itself is circular: every transistor would read as "collector one hop
//! further from ground than emitter" no matter how it is wired.
//!
//! `a` is the up-terminal iff `up(a) < up(b)` **and** `dn(a) > dn(b)` —
//! both axes agreeing, strictly. Otherwise the device **declines** and
//! this module reports `None`. Declining is the correct answer, not a
//! failure, and it is what the three hard cases reduce to:
//!
//! * both terminals unreachable from any rail through DC conductors
//!   (a floating pass transistor, an analog switch) — nothing to compare;
//! * a tie on either axis — the two terminals are the same distance from
//!   the rails, so the drawing has no preferred way up;
//! * the two axes disagreeing — a bidirectional or symmetric use, where
//!   "which side is more positive" is not a property of the topology.
//!
//! **Never guess.** A declined device falls back to the phase's existing
//! behaviour exactly as if this module did not exist.
//!
//! # What this is, and what it must never become
//!
//! This is an **input to phase 4.5's at-risk sweep** — a reason to *look*
//! at an element — and an ADR-28-style informational metric. It is
//! deliberately **not**:
//!
//! * a hard candidate filter. ADR-15's Stage-5 post-mortem measured that
//!   exact move (`allowed`-set filtering on a flow proxy) and it caused
//!   Tier-1 damage: "making the orientation choice hard does not make it
//!   *good* — it makes it *permanent*."
//! * a `cost.rs` weight. CLAUDE.md's constraints-vs-costs rule, and the
//!   V14 Attempt-A failure it records, both apply.
//!
//! The acceptance predicate in `kicad_emitter::refine` is untouched: a
//! candidate pose still has to strictly improve
//! `(severed, coincident, v11, v13, v12, v5, bends)`. The worst case of
//! a wrong answer here is therefore "tried a pose and refused it".

use std::collections::{HashMap, VecDeque};

use spice_policy::CheckedNetlist;
use spice_resolve::{ElementKind, ElementRole, ResolvedElement};

use crate::net_class::{VertPref, vertical_prefs};

/// The two SPICE terminal indices of a device whose DC facing resolved:
/// `(hi, lo)`, where `hi` is the terminal that must be drawn screen-UP.
///
/// Indices, not names, because the caller (`kicad_emitter::refine`) maps
/// them through `PlacedElement::pin_mapping` to reach world geometry.
pub type Facing = (usize, usize);

/// Hop distance for a net no BFS reached. Compared with `<` / `>` like
/// any other distance, so an unreachable terminal is simply "infinitely
/// far", and two unreachable terminals tie and decline.
const UNREACHED: usize = usize::MAX;

/// The two SPICE terminal indices a DC current flows between, for the
/// elements that conduct DC at all. See the module docs for why a base /
/// gate is excluded and a capacitor contributes nothing.
fn conduction_terminals(el: &ResolvedElement) -> Option<Facing> {
    if matches!(el.role, ElementRole::Power(_)) {
        return None;
    }
    let pair = match el.kind {
        ElementKind::Resistor
        | ElementKind::Inductor
        | ElementKind::Diode
        | ElementKind::VoltageSrc
        | ElementKind::CurrentSrc => (0, 1),
        // `c b e` / `d g s [b]` — index 1 is the control terminal.
        ElementKind::Bjt | ElementKind::Mosfet | ElementKind::Jfet => (0, 2),
        _ => return None,
    };
    let (a, b) = (el.nodes.get(pair.0)?, el.nodes.get(pair.1)?);
    if a == b {
        return None;
    }
    Some(pair)
}

/// Is this element one the facing convention has an opinion about? A
/// three-or-more-terminal device with a conduction path and a distinct
/// control terminal — a BJT, a MOSFET or a JFET.
fn is_facing_device(el: &ResolvedElement) -> bool {
    matches!(
        el.kind,
        ElementKind::Bjt | ElementKind::Mosfet | ElementKind::Jfet
    ) && conduction_terminals(el).is_some()
}

/// The DC graph: net → `(other net, element index)` for every DC edge.
fn dc_adjacency(checked: &CheckedNetlist) -> HashMap<&str, Vec<(&str, usize)>> {
    let mut adj: HashMap<&str, Vec<(&str, usize)>> = HashMap::new();
    for (i, el) in checked.elements.iter().enumerate() {
        let Some((ta, tb)) = conduction_terminals(el) else {
            continue;
        };
        let (a, b) = (el.nodes[ta].as_str(), el.nodes[tb].as_str());
        adj.entry(a).or_default().push((b, i));
        adj.entry(b).or_default().push((a, i));
    }
    for v in adj.values_mut() {
        v.sort_unstable();
    }
    adj
}

/// Multi-source BFS from every rail of preference `polarity`, with
/// `skip`'s edge removed. Rails are **absorbing**: a non-root rail is
/// recorded at its distance and never expanded, so no walk runs through
/// a power supply and out the other side.
fn rank(
    adj: &HashMap<&str, Vec<(&str, usize)>>,
    prefs: &HashMap<String, VertPref>,
    polarity: VertPref,
    skip: usize,
) -> HashMap<String, usize> {
    let mut dist: HashMap<String, usize> = HashMap::new();
    let mut q: VecDeque<(&str, usize)> = VecDeque::new();
    // Deterministic seeding order (a `HashMap` iterates arbitrarily);
    // BFS distances do not depend on it, but reproducibility is cheap.
    let mut roots: Vec<&str> = prefs
        .iter()
        .filter(|(_, p)| **p == polarity)
        .map(|(n, _)| n.as_str())
        .collect();
    roots.sort_unstable();
    for r in roots {
        dist.insert(r.to_string(), 0);
        q.push_back((r, 0));
    }
    while let Some((net, d)) = q.pop_front() {
        // A rail that is not a root of THIS walk is a terminus.
        if d > 0 && prefs.contains_key(net) {
            continue;
        }
        for (other, via) in adj.get(net).into_iter().flatten() {
            if *via == skip || dist.contains_key(*other) {
                continue;
            }
            dist.insert((*other).to_string(), d + 1);
            q.push_back((other, d + 1));
        }
    }
    dist
}

/// The DC facing of every element in `checked`, parallel to
/// `checked.elements` (and therefore to `Placement::elements`).
///
/// `Some((hi, lo))` names the SPICE terminal indices whose drawn order
/// must be *`hi` above `lo`*; `None` means the element is not a facing
/// device, or its rank did not resolve — see the module docs for the
/// three ways a device declines.
#[must_use]
pub fn device_facings(checked: &CheckedNetlist) -> Vec<Option<Facing>> {
    let prefs = vertical_prefs(checked);
    let adj = dc_adjacency(checked);
    checked
        .elements
        .iter()
        .enumerate()
        .map(|(i, el)| {
            if !is_facing_device(el) {
                return None;
            }
            let (ta, tb) = conduction_terminals(el)?;
            let (a, b) = (el.nodes[ta].as_str(), el.nodes[tb].as_str());
            // The device's own edge is removed: ranking a device by a
            // path through itself is circular.
            let up = rank(&adj, &prefs, VertPref::Up, i);
            let dn = rank(&adj, &prefs, VertPref::Down, i);
            let d = |m: &HashMap<String, usize>, n: &str| m.get(n).copied().unwrap_or(UNREACHED);
            let (ua, ub) = (d(&up, a), d(&up, b));
            let (da, db) = (d(&dn, a), d(&dn, b));
            if ua < ub && da > db {
                Some((ta, tb))
            } else if ub < ua && db > da {
                Some((tb, ta))
            } else {
                // Tie, disagreement, or both terminals unreachable.
                // Declining is correct; never guess.
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use kicad_symbols::Library;
    use spice_diagnostics::FileId;
    use spice_policy::check;

    use super::device_facings;

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

    /// `refdes → facing`, so a test can name the device it means.
    fn facings(src: &str) -> Vec<(String, Option<(usize, usize)>)> {
        let parsed = spice_parser::parse(src, FileId(0))
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let (checked, _w) = check(resolved).expect("policy check failed");
        let f = device_facings(&checked);
        checked
            .elements
            .iter()
            .zip(f)
            .map(|(el, x)| (el.refdes.clone(), x))
            .collect()
    }

    fn facing_of(src: &str, refdes: &str) -> Option<(usize, usize)> {
        facings(src)
            .into_iter()
            .find(|(r, _)| r == refdes)
            .unwrap_or_else(|| panic!("no element {refdes}"))
            .1
    }

    const HDR: &str = "test\n*@symbol Device:R_US for=R*\n*@symbol Device:C for=C*\n\
                       *@symbol Device:Q_NPN_BCE for=Q*\n";

    /// The base case: a textbook common-emitter stage. The collector
    /// reaches the supply through `RC`, the emitter reaches ground
    /// through `RE`, both axes agree, and `c` (SPICE terminal 0) is up.
    #[test]
    fn common_emitter_resolves_collector_up() {
        let src = format!(
            "{HDR}VCC vcc 0 DC 12 ;@ power=+12V\n\
             R1 vcc b 47k\nR2 b 0 10k\nRC vcc c 3k3\nRE e 0 1k\n\
             Q1 c b e QGENERIC\n.model QGENERIC NPN\n.end\n"
        );
        assert_eq!(facing_of(&src, "Q1"), Some((0, 2)));
    }

    /// A cascode's shared middle net is the interesting case: it is the
    /// lower device's collector AND the upper device's emitter, so it
    /// must rank strictly between the two rail-adjacent nets. Both
    /// devices resolve collector-up.
    #[test]
    fn cascode_resolves_both_devices_collector_up() {
        let src = format!(
            "{HDR}VCC vcc 0 DC 12 ;@ power=+12V\n\
             RB1 vcc b2 47k\nRB2 b2 b1 22k\nRB3 b1 0 10k\n\
             RC vcc c2 4k7\nQ2 c2 b2 c1 QGENERIC\nQ1 c1 b1 e1 QGENERIC\nRE e1 0 470\n\
             .model QGENERIC NPN\n.end\n"
        );
        assert_eq!(facing_of(&src, "Q2"), Some((0, 2)));
        assert_eq!(facing_of(&src, "Q1"), Some((0, 2)));
    }

    /// A differential pair. The collectors reach the supply through
    /// their loads; the shared tail reaches the negative rail through
    /// `RTAIL`. Both halves resolve collector-up even though the tail is
    /// shared — and the rank is read off `VertPref`, so the **negative
    /// supply** `vee` counts as a down-rail exactly as ground does.
    #[test]
    fn diff_pair_resolves_against_a_negative_rail() {
        let src = format!(
            "{HDR}VCC vcc 0 DC 12 ;@ power=+12V\nVEE vee 0 DC -12 ;@ power=-12V\n\
             RC1 vcc c1 4k7\nRC2 vcc c2 4k7\nRTAIL tail vee 2k2\n\
             Q1 c1 in1 tail QGENERIC\nQ2 c2 in2 tail QGENERIC\n.model QGENERIC NPN\n.end\n"
        );
        assert_eq!(facing_of(&src, "Q1"), Some((0, 2)));
        assert_eq!(facing_of(&src, "Q2"), Some((0, 2)));
    }

    /// **Polarity-free by construction.** A PNP wired the conventional
    /// way — emitter on the supply side, collector toward ground — comes
    /// back `(2, 0)`: the *emitter* is the up terminal. Nothing here
    /// read the `.model` card; the topology said it.
    #[test]
    fn pnp_topology_puts_the_emitter_up_with_no_model_lookup() {
        let src = format!(
            "{HDR}VCC vcc 0 DC 12 ;@ power=+12V\n\
             RE vcc e 1k\nRC c 0 4k7\nRB1 vcc b 47k\nRB2 b 0 22k\n\
             Q1 c b e QGENERIC\n.model QGENERIC PNP\n.end\n"
        );
        assert_eq!(facing_of(&src, "Q1"), Some((2, 0)));
    }

    /// A folded-cascode-style PNP branch: emitter to the supply through
    /// a degeneration resistor, collector down into an NPN's collector
    /// node. Emitter up, collector down.
    #[test]
    fn folded_pnp_branch_resolves_emitter_up() {
        let src = format!(
            "{HDR}VCC vcc 0 DC 12 ;@ power=+12V\n\
             REP vcc ep 1k\nRB1 vcc bp 47k\nRB2 bp 0 22k\n\
             QP cp bp ep QGENERIC\nQN cp bn en QGENERIC\nRE en 0 470\n\
             RBN vcc bn 100k\nRBN2 bn 0 22k\n.model QGENERIC PNP\n.end\n"
        );
        assert_eq!(facing_of(&src, "QP"), Some((2, 0)));
        assert_eq!(facing_of(&src, "QN"), Some((0, 2)));
    }

    /// **Decline 1 — floating.** A pass transistor between two signal
    /// nets that reach no rail through any DC conductor. Both terminals
    /// are unreachable, so they tie at infinity and the device declines.
    #[test]
    fn a_floating_pass_transistor_declines() {
        let src = format!(
            "{HDR}VCC vcc 0 DC 12 ;@ power=+12V\n\
             C1 na 0 1u\nC2 nb 0 1u\nRG vcc g 100k\n\
             Q1 na g nb QGENERIC\n.model QGENERIC NPN\n.end\n"
        );
        assert_eq!(facing_of(&src, "Q1"), None);
    }

    /// **Decline 2 — a tie.** Symmetric wiring: both conduction
    /// terminals sit one resistor from the supply and one from ground,
    /// so neither axis separates them. There is no preferred way up and
    /// the module says so rather than guessing.
    #[test]
    fn a_symmetric_tie_declines() {
        let src = format!(
            "{HDR}VCC vcc 0 DC 12 ;@ power=+12V\n\
             RA vcc na 1k\nRB na 0 1k\nRC vcc nb 1k\nRD nb 0 1k\nRG vcc g 100k\n\
             Q1 na g nb QGENERIC\n.model QGENERIC NPN\n.end\n"
        );
        assert_eq!(facing_of(&src, "Q1"), None);
    }

    /// A capacitor is not a DC conductor, so an RC-coupled second stage
    /// cannot borrow the first stage's rank through the coupling cap.
    /// Both stages resolve, and they resolve *independently*.
    #[test]
    fn coupling_capacitors_keep_stage_ranks_independent() {
        let src = format!(
            "{HDR}VCC vcc 0 DC 12 ;@ power=+12V\n\
             RB1 vcc b1 100k\nRB2 b1 0 22k\nRC1 vcc c1 4k7\nRE1 e1 0 1k\n\
             Q1 c1 b1 e1 QGENERIC\nCC c1 b2 1u\n\
             RB3 vcc b2 100k\nRB4 b2 0 22k\nRC2 vcc c2 4k7\nRE2 e2 0 1k\n\
             Q2 c2 b2 e2 QGENERIC\n.model QGENERIC NPN\n.end\n"
        );
        assert_eq!(facing_of(&src, "Q1"), Some((0, 2)));
        assert_eq!(facing_of(&src, "Q2"), Some((0, 2)));
    }

    /// Only Q / M / J devices have a facing at all. Every passive, every
    /// source and every capacitor reports `None` — the convention has no
    /// opinion about a two-terminal part, whose pose V14 and the series
    /// idioms already own.
    #[test]
    fn only_three_terminal_devices_get_a_facing() {
        let src = format!(
            "{HDR}VCC vcc 0 DC 12 ;@ power=+12V\n\
             RC vcc c 3k3\nRE e 0 1k\nCE e 0 100u\nRB vcc b 47k\nRB2 b 0 10k\n\
             Q1 c b e QGENERIC\n.model QGENERIC NPN\n.end\n"
        );
        for (refdes, f) in facings(&src) {
            if refdes == "Q1" {
                assert!(f.is_some(), "Q1 must resolve");
            } else {
                assert_eq!(f, None, "{refdes} must have no facing");
            }
        }
    }

    /// The result is parallel to `checked.elements`, which is the whole
    /// contract the caller relies on to index it alongside
    /// `Placement::elements` / `RefinementMeta::pinned`.
    #[test]
    fn the_result_is_parallel_to_the_element_list() {
        let src = format!(
            "{HDR}VCC vcc 0 DC 12 ;@ power=+12V\nRC vcc c 3k3\nRE e 0 1k\n\
             RB vcc b 47k\nQ1 c b e QGENERIC\n.model QGENERIC NPN\n.end\n"
        );
        let parsed = spice_parser::parse(&src, FileId(0))
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let (checked, _w) = check(resolved).expect("policy check failed");
        assert_eq!(device_facings(&checked).len(), checked.elements.len());
    }
}
