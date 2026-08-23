//! Signal-flow **root** detection — the one policy every consumer that
//! needs to know where the signal enters the circuit reads.
//!
//! # What is unified here, and what deliberately is not
//!
//! Two functions have to agree on which nets and which elements are
//! signal-flow roots:
//!
//! * [`crate::layers::assign_x_layers_with`] needs **element** roots for
//!   a longest-path DAG layering (X = depth along the signal path);
//! * `crate::idioms::signal_net_depth` needs **net** depths from a
//!   shortest-hop BFS (which way a series element's signal runs, which
//!   is the only thing that *pins* a series chain horizontal).
//!
//! Those two traversals legitimately differ, and unifying them would be
//! wrong. Every divergence that has actually cost us a defect was a
//! divergence in *which roots exist*, never in the traversal:
//!
//! 1. `lc_ladder_lpf` — a **drawn source** and no `*@port`. The layering
//!    rooted at the source; the depth map did not know drawn sources
//!    existed, came back empty, and `apply_series_horizontal` declined
//!    the whole ladder, leaving a textbook LC filter for the SA and
//!    phase 4.5 to take apart (ADR-23 D10, ADR-24 D4).
//! 2. `port_shapes` — `*@port ni=input` declared on an **interior** net
//!    of a four-resistor chain, trusted blindly, rooting mid-chain.
//! 3. `signal_net_depth`'s own comment claimed it mirrored
//!    `layers::no_source_fallback` "so depth and layer agree". Only the
//!    *fallback* was ever mirrored — never the principled source-rooted
//!    path.
//!
//! So: **one root set, two traversals.** This module owns the root set.
//!
//! # Tiers
//!
//! [`RootTier::DeclaredPorts`] ≻ [`RootTier::DrawnSources`] ≻
//! [`RootTier::LeafNames`] ≻ [`RootTier::None`]. Exactly one tier fires
//! and the winner is recorded as provenance on [`SignalRoots::tier`], so
//! a consumer (or a test) can ask *why* the circuit is rooted where it
//! is.
//!
//! Ports lead because the project's direction is
//! annotations-over-heuristics: an explicit user declaration outranks
//! any inference. Note the two pre-unification policies ordered the
//! tiers **oppositely** — the layering ran sources-first (declared ports
//! are invisible on its principled path), the depth map ran ports-first
//! — so unification necessarily changes one of them. No current fixture
//! declares an input port *and* draws its source, so the choice is
//! settled by the scoreboard rather than by argument.
//!
//! ## Why ports-first is safe: the boundary check
//!
//! Trusting a declaration blindly is what produced defect (2) above. A
//! declared input net earns depth 0 only if it is a **boundary** of the
//! signal chain: at most one *non-source* element touches it. A net with
//! two ordinary elements on it is interior — the signal arrives there
//! from somewhere, so rooting it means the flow runs backwards out of
//! the root.
//!
//! * If **any** declared input port is boundary, the interior ones are
//!   dropped and listed in [`SignalRoots::demoted_ports`], with a
//!   `log::warn`.
//! * If the user declared **only** interior ports, they are **kept**,
//!   also with a `log::warn`. A mid-chain root beats an empty map: the
//!   alternative on `port_shapes` (no drawn source, no boundary net
//!   name, no power rail) is [`RootTier::None`] and a single collapsed
//!   column. `demoted_ports` stays empty in that case — nothing was
//!   demoted — and the warning text is what distinguishes it.
//!
//! # The one designed asymmetry
//!
//! `layers::no_source_fallback`'s **rail-rooted** policy (root at every
//! power-touching element, so X measures hops from the nearest rail) is
//! **layers-only, forever**. Rail-hop depth is not a signal direction.
//!
//! When [`RootTier::None`] fires, the layering may still fall back to it
//! — it needs *some* X ordering — but `signal_net_depth` must return an
//! **empty map**, so `apply_series_horizontal` declines. Declining is
//! the correct answer for a rootless cycle; fabricating a flow direction
//! out of rail hops is not. This is why `diff_pair`, `multivibrator` and
//! `wien_bridge_osc` are byte-identical across the unification, and they
//! are the cheapest check that it stayed that way.
//!
//! # Known accepted false negative
//!
//! A genuine circuit input that feeds **two parallel loads** fails the
//! "at most one non-source element" boundary test and would be demoted
//! if a boundary port existed alongside it. That is a real false
//! negative, recorded rather than fixed: no current fixture has that
//! shape, and every candidate refinement (degree thresholds, driver
//! detection) is a heuristic with its own failure set. Revisit it when a
//! fixture demonstrates the shape.
//!
//! # Accepted residual — where the next divergence hunt starts
//!
//! Identical roots do **not** guarantee identical *order*. On a
//! reconvergent or feedback path, the layering's longest-path rank and
//! the depth map's shortest-hop BFS can disagree about a pair even from
//! the same root set: longest-path pushes a node past its deepest
//! predecessor, BFS records its shallowest. Harmless today — the Q3
//! flow-inversion verifier gates X only, and a pinned element has no X
//! to invert — but it is the remaining structural gap between the two
//! consumers, and the place to look first when the next "these two
//! disagree" defect appears.

use std::collections::{BTreeMap, BTreeSet};

use spice_policy::CheckedNetlist;
use spice_resolve::{ElementKind, ElementRole, PortDir};

use crate::net_class::{NetClass, NetClassMap};
use crate::placer::Placer;

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
pub(crate) fn boundary_net_role(net: &str) -> Option<PortDir> {
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

/// A **drawn** stimulus: a voltage/current source that is not a
/// `;@ power`-tagged supply, so it appears on the sheet as a symbol and
/// genuinely roots the signal graph.
pub(crate) fn is_signal_source(checked: &CheckedNetlist, idx: usize) -> bool {
    let el = &checked.elements[idx];
    matches!(el.kind, ElementKind::VoltageSrc | ElementKind::CurrentSrc)
        && !matches!(el.role, ElementRole::Power(_))
}

/// Which tier supplied the roots. Exactly one fires.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum RootTier {
    /// `*@port <net>=input` — an explicit user declaration.
    DeclaredPorts,
    /// A drawn stimulus ([`is_signal_source`]).
    DrawnSources,
    /// The leaf-net **name** backstop ([`boundary_net_role`]).
    LeafNames,
    /// No signal-flow root exists: a pure cycle (`wien_bridge_osc`), or
    /// a circuit whose only sources are `;@ power` rails and whose nets
    /// carry no boundary name.
    #[default]
    None,
}

/// The signal-flow roots of a netlist, in both shapes its consumers need.
#[derive(Debug, Clone, Default)]
pub(crate) struct SignalRoots {
    /// Depth-0 nets for the net-space BFS.
    pub nets: BTreeSet<String>,
    /// Element roots for the layering, after ADR-18's "boundary, not
    /// interior" filter.
    pub elements: BTreeSet<usize>,
    /// Which tier fired.
    pub tier: RootTier,
    /// Declared `*@port …=input` nets **rejected** by the boundary check
    /// because a boundary port was available instead. Empty when the
    /// user declared only interior ports — those are kept, and warned
    /// about with different text.
    pub demoted_ports: Vec<String>,
}

/// Is `net` a Signal-class net? Unclassified nets default to Signal, as
/// everywhere else in the placer.
pub(crate) fn is_signal_net(classes: &NetClassMap, net: &str) -> bool {
    classes.get(net).copied().unwrap_or(NetClass::Signal) == NetClass::Signal
}

/// How many *distinct* Signal nets element `i` touches — the ADR-18
/// "boundary, not interior" measure. A rail stub is 1, a pass-through is
/// 2, an active block or a junction is 3 or more.
pub(crate) fn signal_degree(checked: &CheckedNetlist, classes: &NetClassMap, i: usize) -> usize {
    checked.elements[i]
        .nodes
        .iter()
        .filter(|net| is_signal_net(classes, net))
        .map(String::as_str)
        .collect::<BTreeSet<&str>>()
        .len()
}

/// ADR-18's "boundary, not interior" threshold for an element that owns
/// a root net: the signal may *pass through* it (a series input
/// resistor, an AC-coupling cap), but an element carrying three or more
/// Signal nets is a junction or an active block the input feeds *into*.
/// Rooting a `diff_pair` transistor whose base is `in1` collapses it
/// onto its own collector load.
const MAX_ROOT_SIGNAL_DEGREE: usize = 2;

/// The netlist's signal-flow roots, under the unified tier policy.
///
/// `variant` is the ADR-23 placer seam. The tier policy itself does not
/// branch on it today — it is one policy, which is the point — but the
/// warnings name the placer that produced them, so a scoreboard log is
/// attributable to the arm that wrote it.
#[allow(clippy::too_many_lines)] // the three tiers are conceptually one phase
pub(crate) fn signal_flow_roots(
    checked: &CheckedNetlist,
    classes: &NetClassMap,
    variant: Placer,
) -> SignalRoots {
    let n = checked.elements.len();
    let is_signal = |net: &str| is_signal_net(classes, net);

    // Signal-class net → the elements touching it. Rails are excluded
    // here for the same reason the layering excludes them: a path
    // through a supply is not a signal path.
    let mut members: BTreeMap<&str, BTreeSet<usize>> = BTreeMap::new();
    for (i, el) in checked.elements.iter().enumerate() {
        for net in &el.nodes {
            if is_signal(net) {
                members.entry(net.as_str()).or_default().insert(i);
            }
        }
    }
    let sources: BTreeSet<usize> = (0..n).filter(|&i| is_signal_source(checked, i)).collect();

    // Element roots derived from a set of root NETS: everything touching
    // one, filtered by the boundary threshold.
    let elements_of = |nets: &BTreeSet<String>| -> BTreeSet<usize> {
        let mut out = BTreeSet::new();
        for net in nets {
            let Some(m) = members.get(net.as_str()) else {
                continue;
            };
            for &i in m {
                if signal_degree(checked, classes, i) <= MAX_ROOT_SIGNAL_DEGREE {
                    out.insert(i);
                }
            }
        }
        out
    };

    // --- Tier 1: declared `*@port <net>=input` ---------------------------
    let declared: BTreeSet<&str> = checked
        .ports
        .iter()
        .filter(|p| matches!(p.dir, PortDir::Input))
        .map(|p| p.net.as_str())
        .filter(|net| members.contains_key(net))
        .collect();
    if !declared.is_empty() {
        // Boundary: at most one NON-SOURCE element touches the net. A
        // drawn source sitting on the input net does not make it
        // interior — that is exactly the shape `sallen_key_driven` has,
        // and counting the source is what made the old leaf test
        // (`members == 1`) fail on every drawn-source circuit.
        let boundary = |net: &str| -> bool {
            members
                .get(net)
                .is_some_and(|m| m.iter().filter(|i| !sources.contains(i)).count() <= 1)
        };
        let (bnd, interior): (BTreeSet<String>, BTreeSet<String>) = declared
            .iter()
            .map(|n| (*n).to_owned())
            .partition(|net| boundary(net));
        let (nets, demoted_ports) = if bnd.is_empty() {
            log::warn!(
                "[{}] every declared `*@port …=input` net ({}) is interior to the \
                 signal chain; keeping them as flow roots anyway — a mid-chain root \
                 beats no root, but the drawing may run backwards through them",
                variant.name(),
                join(&interior),
            );
            (interior, Vec::new())
        } else {
            (bnd, interior.into_iter().collect::<Vec<String>>())
        };
        let elements = elements_of(&nets);
        let out = SignalRoots {
            nets,
            elements,
            tier: RootTier::DeclaredPorts,
            demoted_ports,
        };
        // Surfaced by READING the field, not by a parallel local: the
        // structured provenance and the rendered line then cannot drift.
        if !out.demoted_ports.is_empty() {
            log::warn!(
                "[{}] declared `*@port …=input` net(s) {} are interior to the signal \
                 chain and were demoted; rooting at {} instead",
                variant.name(),
                out.demoted_ports.join(", "),
                join(&out.nets),
            );
        }
        return out;
    }

    // --- Tier 2: drawn sources -------------------------------------------
    // A source element IS a boundary by construction, so it takes no
    // degree filter: it is the thing the signal comes *out* of.
    if !sources.is_empty() {
        let nets: BTreeSet<String> = sources
            .iter()
            .flat_map(|&i| checked.elements[i].nodes.iter())
            .filter(|net| is_signal(net))
            .cloned()
            .collect();
        return SignalRoots {
            nets,
            elements: sources,
            tier: RootTier::DrawnSources,
            demoted_ports: Vec::new(),
        };
    }

    // --- Tier 3: the leaf-name backstop ----------------------------------
    let nets: BTreeSet<String> = members
        .iter()
        .filter(|(_, m)| m.len() == 1)
        .map(|(net, _)| (*net).to_owned())
        .filter(|net| matches!(boundary_net_role(net), Some(PortDir::Input)))
        .collect();
    if !nets.is_empty() {
        let elements = elements_of(&nets);
        return SignalRoots {
            nets,
            elements,
            tier: RootTier::LeafNames,
            demoted_ports: Vec::new(),
        };
    }

    SignalRoots::default()
}

fn join(nets: &BTreeSet<String>) -> String {
    nets.iter().cloned().collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    use kicad_symbols::Library;
    use spice_diagnostics::FileId;
    use spice_policy::check;

    use crate::net_class::classify_nets;

    fn library() -> &'static Library {
        static LIB: OnceLock<Library> = OnceLock::new();
        LIB.get_or_init(|| {
            let dir = workspace_root().join("crates/kicad-symbols/tests/fixtures");
            let mut lib = Library::default();
            for f in [
                "Device.kicad_sym",
                "Simulation_SPICE.kicad_sym",
                "Amplifier_Operational.kicad_sym",
                "power.kicad_sym",
            ] {
                lib = lib.merge(Library::from_file(dir.join(f)).expect("load fixture library"));
            }
            lib
        })
    }

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .to_path_buf()
    }

    fn checked_of(src: &str) -> CheckedNetlist {
        let parsed = spice_parser::parse(src, FileId(0)).expect("parse").netlist;
        let resolved = spice_resolve::resolve(&parsed, library()).expect("resolve");
        check(resolved).expect("policy check").0
    }

    fn roots_of(src: &str) -> SignalRoots {
        let checked = checked_of(src);
        let classes = classify_nets(&checked);
        signal_flow_roots(&checked, &classes, Placer::FlowSeedV4)
    }

    fn refdes(checked: &CheckedNetlist, elements: &BTreeSet<usize>) -> Vec<String> {
        elements
            .iter()
            .map(|&i| checked.elements[i].refdes.clone())
            .collect()
    }

    /// The `port_shapes` defect, in miniature and with a way out: `mid`
    /// is interior (two ordinary elements touch it), `in` is boundary
    /// (one does). Ports-first is only safe because the interior one
    /// loses.
    #[test]
    fn an_interior_declared_port_is_demoted_when_a_boundary_one_exists() {
        let r = roots_of(
            "* demotion\n\
             *@symbol Device:R_US for=R*\n\
             *@port in=input\n\
             *@port mid=input\n\
             R1 in  mid 1k\n\
             R2 mid n2  1k\n\
             R3 n2  0   1k\n\
             .end\n",
        );
        assert_eq!(r.tier, RootTier::DeclaredPorts);
        assert_eq!(r.nets, ["in".to_owned()].into_iter().collect());
        assert_eq!(r.demoted_ports, vec!["mid".to_owned()]);
    }

    /// `port_shapes` itself: `*@port ni=input` sits mid-chain and there
    /// is no boundary port to prefer. Dropping it would leave the
    /// fixture with no drawn source, no boundary net name and no power
    /// rail — `RootTier::None`, and a single collapsed column. A
    /// mid-chain root beats an empty map, so it is kept; the warning,
    /// not the root set, is what changes.
    #[test]
    fn only_interior_declared_ports_are_kept_rather_than_dropped() {
        let src = std::fs::read_to_string(
            workspace_root().join("crates/spice2kicad/tests/fixtures/port_shapes.cir"),
        )
        .expect("read port_shapes");
        let checked = checked_of(&src);
        let classes = classify_nets(&checked);
        let r = signal_flow_roots(&checked, &classes, Placer::FlowSeedV4);
        assert_eq!(r.tier, RootTier::DeclaredPorts);
        assert_eq!(r.nets, ["ni".to_owned()].into_iter().collect());
        assert!(r.demoted_ports.is_empty(), "kept, so nothing was demoted");
        assert_eq!(refdes(&checked, &r.elements), vec!["R1", "R2"]);
    }

    /// The `lc_ladder_lpf` shape: a drawn stimulus and no `*@port
    /// …=input`. The depth map used not to know drawn sources existed,
    /// so it came back empty while the layering rooted at the source.
    #[test]
    fn a_drawn_source_roots_when_no_input_port_is_declared() {
        let checked = checked_of(
            "* drawn source\n\
             *@symbol Device:R_US          for=R*\n\
             *@symbol Device:C             for=C*\n\
             *@symbol Simulation_SPICE:VDC for=VIN\n\
             *@port out=output\n\
             VIN src 0   DC 0 AC 1\n\
             RS  src in  50\n\
             C1  in  0   3n3\n\
             R2  in  out 1k\n\
             .end\n",
        );
        let classes = classify_nets(&checked);
        let r = signal_flow_roots(&checked, &classes, Placer::FlowSeedV4);
        assert_eq!(r.tier, RootTier::DrawnSources);
        assert_eq!(r.nets, ["src".to_owned()].into_iter().collect());
        assert_eq!(refdes(&checked, &r.elements), vec!["VIN"]);
        assert!(r.demoted_ports.is_empty());
        // `in` is touched by RS, C1 and R2, so the leaf-NAME backstop
        // rejects it — which is exactly why the drawn-source tier has to
        // exist rather than being subsumed by tier 3.
        assert!(!r.nets.contains("in"));
    }

    /// A pure cycle with `;@ power` supplies only: no declared port, no
    /// drawn source, no boundary net name. The tier is `None`, and the
    /// depth map must come back EMPTY — see the module doc's "one
    /// designed asymmetry".
    #[test]
    fn a_rootless_circuit_reports_no_tier_and_an_empty_depth_map() {
        let checked = checked_of(
            "* rootless\n\
             *@symbol Device:R_US for=R*\n\
             VCC vcc 0 DC 5 ;@ power=+5V\n\
             R1 vcc a 1k\n\
             R2 a   b 1k\n\
             R3 b vcc 1k\n\
             .end\n",
        );
        let classes = classify_nets(&checked);
        let r = signal_flow_roots(&checked, &classes, Placer::FlowSeedV4);
        assert_eq!(r.tier, RootTier::None);
        assert!(r.nets.is_empty() && r.elements.is_empty());
        assert!(
            crate::idioms::signal_net_depth(&checked, &classes, Placer::FlowSeedV4).is_empty(),
            "a rootless circuit must yield no flow direction at all"
        );
    }
}
