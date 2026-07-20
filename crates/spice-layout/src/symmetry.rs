//! V7: symmetry-aware placement.
//!
//! Detects pairwise structural symmetry in the resolved netlist and
//! mirrors paired elements about a common vertical axis. Builds on V6
//! (archetype seeds): we pin paired elements at mirrored coordinates
//! before V5's orientation pass runs, so the orientation chooser
//! treats them as fixed. Q-style active devices in the pair receive
//! mirrored orientations (one carries `(mirror y)`) so their pins
//! point inward.
//!
//! Detection is **structural**, not name-driven. Candidate pairs are
//! proposed by grouping elements on a local graph signature —
//! `(lib_id, terminal count, per-terminal net-class, and the sorted
//! multiset of neighbour `(lib_id, kicad-pin)` touched at each
//! terminal)` — and pairing any signature class that contains exactly
//! two elements. Refdes names play *no* role in proposal except as a
//! tie-breaker when a single class holds more than two structurally
//! interchangeable elements (e.g. four identical resistors): there we
//! pair by a shared refdes stem if and only if doing so yields a clean
//! 2-grouping, and otherwise decline (specificity over recall — a
//! false-positive idiom is worse than none, roadmap §6).
//!
//! This finds pairs the old `Q1/Q2` digit heuristic missed —
//! `Q1/Q3`, `M1/MA`, `QL/QR` — because the signature is computed from
//! topology, not from the trailing character of the refdes.
//!
//! Each candidate set is then validated strictly by the involution
//! machinery: build a net permutation σ from corresponding terminals,
//! require fixed nets (ground, `*@power` rails) to self-map, signal
//! nets to form 2-cycles, σ to be an involution, and every unpaired
//! element to be σ-invariant. A candidate set that fails any check is
//! rejected wholesale (`None`).
//!
//! Geometry: axis_x is the centre of the stage-1/V6 placement's
//! bounding box (or, if the user pinned a pair via `align`/`place`,
//! that pair's midpoint). For each pair `(L, R)` with L being the
//! lower-indexed element, the right element's origin becomes
//! `(axis_sum - L.origin.x, L.origin.y)` and its orientation is
//! `IDENTITY.flip()` (mirror_y = true). The left element keeps
//! IDENTITY.

use std::collections::{HashMap, HashSet};

use kicad_symbols::Orientation;
use spice_policy::CheckedNetlist;
use spice_resolve::{ElementRole, ResolvedElement};

use crate::Placement;
use crate::net_class::{NetClass, classify_nets};

/// A validated symmetry plan: which element indices in `Placement`
/// are paired, and where the mirror axis sits (in grid units).
#[derive(Debug, Clone)]
pub(crate) struct SymmetryPlan {
    /// Pairs of element indices `(left, right)` into `Placement.elements`.
    /// `left.refdes` always carries the lower-numbered index ("…1");
    /// `right.refdes` carries "…2".
    pub pairs: Vec<(usize, usize)>,
}

/// Detect a symmetric pairing in the netlist. Returns `None` if no
/// non-trivial symmetry is found, or if any candidate pair fails
/// validation.
pub(crate) fn detect_pairs(checked: &CheckedNetlist) -> Option<SymmetryPlan> {
    let elems = &checked.elements;
    let candidates = collect_candidate_pairs(checked)?;
    let fixed = fixed_nets(elems);
    let candidates = retain_coupled_pairs(elems, &fixed, candidates)?;
    let sigma = build_sigma(elems, &candidates, &fixed)?;
    if !sigma_is_involution(&sigma) {
        return None;
    }
    // Reject the trivial case where σ is the identity on every net —
    // that means the candidate "pairs" share all their nets, so
    // there is no actual mirror to apply. Without this guard, three
    // identical elements (R1, R2, R3 all on the same nets) would
    // pair (R1, R2) and stomp R2's x onto R3's.
    if sigma.iter().all(|(k, v)| k == v) {
        return None;
    }
    if !unpaired_invariant_under_sigma(elems, &candidates, &sigma) {
        return None;
    }
    Some(SymmetryPlan { pairs: candidates })
}

/// Drop candidate pairs whose two halves are not **signal-coupled**.
///
/// A σ-involution proves the two halves are structurally interchangeable.
/// It does NOT prove they belong on opposite sides of a shared axis, and
/// that distinction is what separates the two circuit families the
/// signature groups together:
///
/// * A **coupled** pair — `diff_pair`'s `Q1`/`Q2` about their tail node,
///   `multivibrator`'s cross-coupled halves — genuinely nests about a
///   common axis. The shared node sits ON the axis, and mirroring puts
///   each half's pins facing inward toward it. This is V7's whole point.
///
/// * **Uncoupled repeated channels** — a dual opamp, a quad comparator,
///   a stereo stage — share nothing but the supply rails. There is no
///   node on the axis to nest about. Mirroring channel 2 onto
///   `axis_sum - L.x` maps it onto the SAME x-span channel 1 occupies
///   instead of placing it beside channel 1, and flips its orientation
///   so its inputs face away from its own resistors. Such channels
///   belong side by side, which is what the ordinary
///   classify → bands → layers seed produces once V7 declines.
///
/// The predicate: build a graph on elements using only **non-rail** nets
/// (excluding ground, `*@power` rails and conventional supply names —
/// the same `fixed` set σ requires to self-map, which is exactly the set
/// that carries no signal), and require both halves of a pair to land in
/// the same connected component. Direct sharing is the common case;
/// connectivity through intermediate elements covers a tail node reached
/// via a shared resistor.
///
/// Rejected pairs are dropped individually rather than failing the whole
/// plan, so a circuit mixing a coupled pair with an uncoupled repeat
/// keeps the symmetry it really has. Returns `None` if nothing survives.
fn retain_coupled_pairs(
    elems: &[ResolvedElement],
    fixed: &HashSet<String>,
    candidates: Vec<(usize, usize)>,
) -> Option<Vec<(usize, usize)>> {
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }

    // Union-find over elements, joined by shared non-rail nets.
    let mut parent: Vec<usize> = (0..elems.len()).collect();
    let mut by_net: HashMap<&str, usize> = HashMap::new();
    for (i, e) in elems.iter().enumerate() {
        for node in &e.nodes {
            if fixed.contains(node) {
                continue;
            }
            match by_net.get(node.as_str()) {
                Some(&first) => {
                    let (a, b) = (find(&mut parent, first), find(&mut parent, i));
                    if a != b {
                        parent[a] = b;
                    }
                }
                None => {
                    by_net.insert(node.as_str(), i);
                }
            }
        }
    }

    let coupled: Vec<(usize, usize)> = candidates
        .into_iter()
        .filter(|&(l, r)| find(&mut parent, l) == find(&mut parent, r))
        .collect();
    if coupled.is_empty() {
        return None;
    }
    Some(coupled)
}

/// A purely structural fingerprint of an element's local neighbourhood.
///
/// Two elements with the same signature are *candidates* for being a
/// mirrored pair: they bind the same kind of symbol, with the same arity,
/// onto nets of the same classes, and each of those nets touches the same
/// multiset of neighbour `(lib_id, kicad-pin)` endpoints. The signature is
/// computed from the netlist graph alone — no refdes characters enter it —
/// so it pairs `Q1/Q3`, `M1/MA`, `QL/QR` just as readily as `Q1/Q2`.
///
/// It is deliberately *coarse*: false groupings are filtered out later by
/// the strict involution validation in [`detect_pairs`]. Its one job is to
/// distinguish structurally-distinct elements that the involution check
/// alone would otherwise mis-pair (e.g. the multivibrator's collector
/// resistors `RC*` vs. base resistors `RB*`, identical in lib_id and net
/// class but attached to different transistor pins).
type Signature = (String, usize, Vec<(NetClass, Vec<(String, String)>)>);

/// Build the structural signature for element `i`.
fn signature(
    i: usize,
    elems: &[ResolvedElement],
    classes: &HashMap<String, NetClass>,
    net_endpoints: &HashMap<&str, Vec<(usize, usize)>>,
) -> Signature {
    let e = &elems[i];
    let mut per_terminal: Vec<(NetClass, Vec<(String, String)>)> =
        Vec::with_capacity(e.nodes.len());
    for node in &e.nodes {
        let class = classes
            .get(node.as_str())
            .copied()
            .unwrap_or(NetClass::Signal);
        // Multiset of neighbour endpoints on this net: (neighbour lib_id,
        // neighbour kicad pin number). Excludes the element itself. The
        // kicad-pin component is what tells a collector-net resistor apart
        // from a base-net resistor (both Power+Signal, same lib_id).
        let mut endpoints: Vec<(String, String)> = Vec::new();
        if let Some(pins) = net_endpoints.get(node.as_str()) {
            for &(other_i, other_term) in pins {
                if other_i == i {
                    continue;
                }
                let pin = elems[other_i]
                    .pin_mapping
                    .get(other_term)
                    .cloned()
                    .unwrap_or_default();
                endpoints.push((elems[other_i].lib_id.clone(), pin));
            }
        }
        endpoints.sort_unstable();
        per_terminal.push((class, endpoints));
    }
    (e.lib_id.clone(), e.nodes.len(), per_terminal)
}

/// Propose candidate mirror pairs by structural signature.
///
/// Groups every element by its [`Signature`]; a group of exactly two
/// elements becomes a candidate pair (lower index = left). A group of more
/// than two structurally-interchangeable elements is *ambiguous*: we
/// attempt a refdes-stem tie-break (pairing `…1`/`…2` within the group) and
/// accept it only if it cleanly partitions the group into pairs; otherwise
/// the whole group is dropped. Groups of one are ignored.
fn collect_candidate_pairs(checked: &CheckedNetlist) -> Option<Vec<(usize, usize)>> {
    let elems = &checked.elements;
    let classes = classify_nets(checked);

    // net name -> [(element_idx, terminal_idx)] for neighbour lookup.
    let mut net_endpoints: HashMap<&str, Vec<(usize, usize)>> = HashMap::new();
    for (i, e) in elems.iter().enumerate() {
        for (t, node) in e.nodes.iter().enumerate() {
            net_endpoints.entry(node.as_str()).or_default().push((i, t));
        }
    }

    // Group element indices by signature. Use a BTreeMap-free approach:
    // signatures are not Ord-friendly (NetClass isn't), so collect into a
    // Vec keyed by signature via linear grouping on a stable order.
    // HashMap is safe here despite being iterated: every candidate pair is
    // normalised (min, max) and the final list is `sort_unstable`d, so the
    // group traversal order cannot reach the output.
    let mut groups: HashMap<Signature, Vec<usize>> = HashMap::new();
    for i in 0..elems.len() {
        let sig = signature(i, elems, &classes, &net_endpoints);
        groups.entry(sig).or_default().push(i);
    }

    let mut candidates: Vec<(usize, usize)> = Vec::new();
    for indices in groups.values() {
        match indices.len() {
            0 | 1 => {}
            2 => {
                let (a, b) = (indices[0].min(indices[1]), indices[0].max(indices[1]));
                candidates.push((a, b));
            }
            _ => {
                // Ambiguous structural class. Try a refdes-stem tie-break:
                // pair members sharing a stem with trailing 1/2. Accept
                // only a clean partition into pairs.
                if let Some(mut paired) = pair_by_stem(indices, elems) {
                    candidates.append(&mut paired);
                }
            }
        }
    }

    if candidates.is_empty() {
        return None;
    }
    candidates.sort_unstable();
    Some(candidates)
}

/// Tie-break an ambiguous structural group by refdes stem. Returns a clean
/// partition of `indices` into `(left, right)` pairs (one per stem, with
/// trailing `1`/`2`) iff every member can be so paired; otherwise `None`.
fn pair_by_stem(indices: &[usize], elems: &[ResolvedElement]) -> Option<Vec<(usize, usize)>> {
    let mut by_stem: HashMap<String, [Option<usize>; 2]> = HashMap::new();
    for &i in indices {
        let (stem, slot) = split_refdes(&elems[i].refdes)?;
        let entry = by_stem.entry(stem).or_insert([None, None]);
        if entry[slot].is_some() {
            return None; // duplicate digit per stem: ambiguous, decline.
        }
        entry[slot] = Some(i);
    }
    let mut out = Vec::with_capacity(by_stem.len());
    for slots in by_stem.values() {
        match (slots[0], slots[1]) {
            (Some(l), Some(r)) => out.push((l.min(r), l.max(r))),
            _ => return None, // unpaired half: decline the whole group.
        }
    }
    Some(out)
}

/// Compute the set of "fixed" nets — those that must self-map under
/// σ. Includes ground (`"0"`), any net touched by a `*@power` element,
/// and conventional supply names (vcc/vdd/v+/vplus/vee/vss).
fn fixed_nets(elems: &[ResolvedElement]) -> HashSet<String> {
    let mut fixed: HashSet<String> = HashSet::new();
    fixed.insert("0".to_string());
    for e in elems {
        if matches!(e.role, ElementRole::Power(_)) {
            for n in &e.nodes {
                fixed.insert(n.clone());
            }
        }
        for n in &e.nodes {
            if matches!(
                n.to_ascii_lowercase().as_str(),
                "vcc" | "vdd" | "v+" | "vplus" | "vee" | "vss"
            ) {
                fixed.insert(n.clone());
            }
        }
    }
    fixed
}

/// Build σ from corresponding terminals across each candidate pair.
/// Returns `None` if any constraint conflicts (a fixed net mapping
/// to something other than itself; or an inconsistent binding).
fn build_sigma(
    elems: &[ResolvedElement],
    candidates: &[(usize, usize)],
    fixed: &HashSet<String>,
) -> Option<HashMap<String, String>> {
    let mut sigma: HashMap<String, String> = HashMap::new();
    for &(l, r) in candidates {
        for (na, nb) in elems[l].nodes.iter().zip(elems[r].nodes.iter()) {
            if fixed.contains(na) || fixed.contains(nb) {
                if na != nb {
                    return None;
                }
                if !bind(&mut sigma, na, nb) {
                    return None;
                }
                continue;
            }
            if !bind(&mut sigma, na, nb) || !bind(&mut sigma, nb, na) {
                return None;
            }
        }
    }
    Some(sigma)
}

/// Insert `a -> b` into σ. Returns false if `a` was previously bound
/// to something other than `b`.
fn bind(sigma: &mut HashMap<String, String>, a: &str, b: &str) -> bool {
    if let Some(prev) = sigma.get(a) {
        prev == b
    } else {
        sigma.insert(a.to_string(), b.to_string());
        true
    }
}

/// True iff σ(σ(x)) == x for every `x` in σ's domain.
fn sigma_is_involution(sigma: &HashMap<String, String>) -> bool {
    sigma
        .iter()
        .all(|(k, v)| sigma.get(v).is_some_and(|vv| vv == k))
}

/// Each unpaired element's node multiset must be invariant under σ —
/// i.e. rewriting its nodes through σ produces the same multiset.
fn unpaired_invariant_under_sigma(
    elems: &[ResolvedElement],
    candidates: &[(usize, usize)],
    sigma: &HashMap<String, String>,
) -> bool {
    let paired: HashSet<usize> = candidates.iter().flat_map(|&(a, b)| [a, b]).collect();
    for (i, e) in elems.iter().enumerate() {
        if paired.contains(&i) {
            continue;
        }
        let mut original: Vec<&str> = e.nodes.iter().map(String::as_str).collect();
        let mut mapped: Vec<String> = e
            .nodes
            .iter()
            .map(|n| sigma.get(n).cloned().unwrap_or_else(|| n.clone()))
            .collect();
        original.sort_unstable();
        mapped.sort_unstable();
        let mapped_refs: Vec<&str> = mapped.iter().map(String::as_str).collect();
        if original != mapped_refs {
            return false;
        }
    }
    true
}

/// Apply the plan to `placement`. The axis is recomputed from the
/// current (post-V6) bounding box of the placement, then for each
/// `(left, right)` pair the right element's origin is mirrored about
/// that axis and both endpoints are pinned.
pub(crate) fn apply(placement: &mut Placement, pinned: &mut [bool], plan: &SymmetryPlan) {
    if placement.elements.is_empty() || plan.pairs.is_empty() {
        return;
    }

    let axis_sum = axis_sum(placement, pinned, plan);

    // For each pair, mirror R's origin and align R's y to L's. If
    // both halves were user-pinned we trust the user and only adjust
    // orientation.
    for &(l, r) in &plan.pairs {
        let l_was_pinned = pinned[l];
        let r_was_pinned = pinned[r];
        if !(l_was_pinned && r_was_pinned) {
            let l_origin = placement.elements[l].origin;
            placement.elements[r].origin.x = axis_sum - l_origin.x;
            placement.elements[r].origin.y = l_origin.y;
        }
        placement.elements[l].orientation = Orientation::IDENTITY;
        placement.elements[r].orientation = Orientation::IDENTITY.flip();
        pinned[l] = true;
        pinned[r] = true;
    }
}

/// The mirror reference for `plan`, as `L.x + R.x`.
///
/// We avoid integer-dividing the axis (which loses 0.5-cell precision on
/// odd sums) and instead store the *sum* — the mirror operation
/// `R.x = axis_sum - L.x` then preserves grid alignment exactly. If a
/// pair is already pinned by the user (`align`/`place` bound both
/// halves), it defines the reference; otherwise we fall back to the
/// placement bbox midpoint.
pub(crate) fn axis_sum(placement: &Placement, pinned: &[bool], plan: &SymmetryPlan) -> i32 {
    plan.pairs
        .iter()
        .find(|&&(l, r)| pinned[l] && pinned[r])
        .map_or_else(
            || {
                let min_x = placement
                    .elements
                    .iter()
                    .map(|e| e.origin.x)
                    .min()
                    .unwrap_or(0);
                let max_x = placement
                    .elements
                    .iter()
                    .map(|e| e.origin.x)
                    .max()
                    .unwrap_or(0);
                min_x + max_x
            },
            |&(l, r)| placement.elements[l].origin.x + placement.elements[r].origin.x,
        )
}

/// Re-establish the mirror relation for every pair in `plan` about
/// `axis_sum`, taking the LEFT member's column as authoritative.
///
/// V7 owns the *relation* between a mirror pair, not either member's
/// absolute column — so a later seed idiom (rail-stub columns) is
/// allowed to move a symmetry-pinned element, provided the relation is
/// restored afterwards. Pairs whose halves the user pinned outright are
/// left alone: an explicit `align` / `place` outranks V7.
pub(crate) fn remirror(
    placement: &mut Placement,
    plan: &SymmetryPlan,
    user_pinned: &[bool],
    axis_sum: i32,
) {
    for &(l, r) in &plan.pairs {
        if user_pinned[l] && user_pinned[r] {
            continue;
        }
        let l_origin = placement.elements[l].origin;
        placement.elements[r].origin.x = axis_sum - l_origin.x;
        placement.elements[r].origin.y = l_origin.y;
    }
}

/// Split a refdes into `(stem, slot_index)` where slot_index is 0 for
/// trailing `1` and 1 for trailing `2`. Returns `None` if the refdes
/// does not end in `1` or `2`, or if the stem would be empty.
fn split_refdes(refdes: &str) -> Option<(String, usize)> {
    let last = refdes.chars().last()?;
    let slot = match last {
        '1' => 0usize,
        '2' => 1usize,
        _ => return None,
    };
    let stem: String = refdes[..refdes.len() - 1].to_string();
    if stem.is_empty() {
        return None;
    }
    // Stem must start with a letter (ruling out `12` → stem "1").
    if !stem.chars().next().unwrap().is_ascii_alphabetic() {
        return None;
    }
    Some((stem, slot))
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

    /// Map a detected `SymmetryPlan` back to refdes pairs (sorted) so
    /// assertions don't depend on element index order.
    fn refdes_pairs(checked: &CheckedNetlist, plan: &SymmetryPlan) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = plan
            .pairs
            .iter()
            .map(|&(l, r)| {
                let a = checked.elements[l].refdes.clone();
                let b = checked.elements[r].refdes.clone();
                if a <= b { (a, b) } else { (b, a) }
            })
            .collect();
        out.sort();
        out
    }

    /// Generality proof: a structurally-symmetric astable multivibrator
    /// whose refdes do NOT end in 1/2 (`Q1/Q3`, `RCA/RCB`, `RBA/RBB`,
    /// `CA/CB`) must still be detected. The old digit heuristic would
    /// have found zero of these pairs.
    #[test]
    fn detects_pairs_without_trailing_1_2() {
        let src = "\
multivibrator with non-1/2 refdes
*@symbol Device:R_US      for=R*
*@symbol Device:C         for=C*
*@symbol Device:Q_NPN_BCE for=Q*

VCC vcc 0 DC 5 ;@ power=+5V

RCA vcc c1 10k
RCB vcc c2 10k
RBA vcc b1 100k
RBB vcc b2 100k

CA c1 b2 10n
CB c2 b1 10n

Q1 c1 b1 0 QGENERIC
Q3 c2 b2 0 QGENERIC

.model QGENERIC NPN (BF=200 IS=1e-15)
.end
";
        let checked = checked_of(src);
        let plan = detect_pairs(&checked).expect("symmetry should be detected");
        let pairs = refdes_pairs(&checked, &plan);
        assert!(
            pairs.contains(&("Q1".to_string(), "Q3".to_string())),
            "expected Q1/Q3 pair, got {pairs:?}"
        );
        assert!(
            pairs.contains(&("CA".to_string(), "CB".to_string())),
            "expected CA/CB pair, got {pairs:?}"
        );
        assert!(
            pairs.contains(&("RCA".to_string(), "RCB".to_string())),
            "expected RCA/RCB pair, got {pairs:?}"
        );
        assert!(
            pairs.contains(&("RBA".to_string(), "RBB".to_string())),
            "expected RBA/RBB pair, got {pairs:?}"
        );
        // Exactly four pairs (the four cross-coupled element classes).
        assert_eq!(pairs.len(), 4, "expected 4 pairs, got {pairs:?}");
    }

    /// The signature must keep collector resistors (RC*) and base
    /// resistors (RB*) in distinct classes even though they share lib_id
    /// and net classes: they attach to different transistor pins. Without
    /// the neighbour-`(lib_id, pin)` term they'd form one 4-element class
    /// and (lacking a stem tie-break here) be dropped.
    #[test]
    fn collector_and_base_resistors_pair_separately() {
        let src = "\
multivibrator
*@symbol Device:R_US      for=R*
*@symbol Device:C         for=C*
*@symbol Device:Q_NPN_BCE for=Q*

VCC vcc 0 DC 5 ;@ power=+5V

RCA vcc c1 10k
RCB vcc c2 10k
RBA vcc b1 100k
RBB vcc b2 100k

CA c1 b2 10n
CB c2 b1 10n

Q1 c1 b1 0 QGENERIC
Q3 c2 b2 0 QGENERIC

.model QGENERIC NPN (BF=200 IS=1e-15)
.end
";
        let checked = checked_of(src);
        let plan = detect_pairs(&checked).expect("symmetry should be detected");
        let pairs = refdes_pairs(&checked, &plan);
        // The collector pair and the base pair are both present and
        // distinct — i.e. no RC was paired with an RB.
        assert!(pairs.contains(&("RCA".to_string(), "RCB".to_string())));
        assert!(pairs.contains(&("RBA".to_string(), "RBB".to_string())));
        for (a, b) in &pairs {
            let cross = (a.starts_with("RC") && b.starts_with("RB"))
                || (a.starts_with("RB") && b.starts_with("RC"));
            assert!(!cross, "collector/base cross-pairing leaked: {a}/{b}");
        }
    }

    /// An asymmetric circuit (RC low-pass) must NOT be spuriously
    /// mirrored: there is no structural automorphism, so `detect_pairs`
    /// returns `None`.
    #[test]
    fn asymmetric_rc_lowpass_not_mirrored() {
        let src = "\
rc lowpass
*@symbol Device:R for=R*
*@symbol Device:C for=C*

V1 in 0 DC 1 ;@ power=+5V
R1 in out 1k
C1 out 0 100n
.end
";
        let checked = checked_of(src);
        assert!(
            detect_pairs(&checked).is_none(),
            "asymmetric RC low-pass should not be detected as symmetric"
        );
    }

    /// Two electrically independent channels sharing only the supply
    /// rails are structurally interchangeable but NOT coupled: there is
    /// no node on the axis to nest about. Mirroring maps channel 2 onto
    /// channel 1's own x-span and flips its orientation so its inputs
    /// face away from its own resistors. V7 must decline and let the
    /// ordinary layered seed place them side by side.
    #[test]
    fn uncoupled_repeated_channels_are_not_mirrored() {
        let src = "\
dual independent inverting amplifier
*@symbol Device:R_US for=R*

VCC vcc 0 DC 15 ;@ power=+15V
VEE vee 0 DC -15 ;@ power=-15V

RIN1 in1  inv1 1k
RF1  inv1 out1 10k
RIN2 in2  inv2 1k
RF2  inv2 out2 10k
.end
";
        let checked = checked_of(src);
        assert!(
            detect_pairs(&checked).is_none(),
            "uncoupled channels sharing only rails must not be mirror-paired"
        );
    }

    /// The coupling predicate must not weaken genuine symmetry. A
    /// cross-coupled multivibrator's halves reach each other through the
    /// signal nets `c1`/`b2`, so every pair survives.
    #[test]
    fn coupled_halves_still_pair() {
        let src = "\
multivibrator
*@symbol Device:R_US      for=R*
*@symbol Device:C         for=C*
*@symbol Device:Q_NPN_BCE for=Q*

VCC vcc 0 DC 5 ;@ power=+5V

RCA vcc c1 10k
RCB vcc c2 10k
RBA vcc b1 100k
RBB vcc b2 100k

CA c1 b2 10n
CB c2 b1 10n

Q1 c1 b1 0 QGENERIC
Q3 c2 b2 0 QGENERIC

.model QGENERIC NPN (BF=200 IS=1e-15)
.end
";
        let checked = checked_of(src);
        let plan = detect_pairs(&checked).expect("cross-coupled symmetry must survive coupling");
        assert_eq!(refdes_pairs(&checked, &plan).len(), 4);
    }

    /// Three structurally-identical elements on the same nets must not be
    /// paired: the ambiguous group has no clean 2-partition and the σ it
    /// would build is the identity (no real mirror).
    #[test]
    fn three_identical_resistors_not_paired() {
        let src = "\
three parallel resistors
*@symbol Device:R for=R*
R1 a b 1k
R2 a b 1k
R3 a b 1k
.end
";
        let checked = checked_of(src);
        assert!(
            detect_pairs(&checked).is_none(),
            "three identical resistors must not be paired"
        );
    }
}
