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
//! Detection is name-driven: refdes pairs like `(Q1, Q2)`,
//! `(RC1, RC2)`, `(C1, C2)` — same stem, indices `1` and `2`. Each
//! candidate pair is validated by attempting to construct a net
//! permutation σ from corresponding terminals. Fixed nets (ground,
//! `*@power` rails) must map to themselves; signal nets must form
//! 2-cycles; σ must be an involution; and every unpaired element must
//! be σ-invariant (its node set is fixed under σ).
//!
//! Geometry: axis_x is the centre of the stage-1/V6 placement's
//! bounding box. For each pair `(L, R)` with L being the lower-indexed
//! refdes, the right element's origin becomes
//! `(2 * axis_x - L.origin.x, L.origin.y)` and its orientation is
//! `IDENTITY.flip()` (mirror_y = true). The left element keeps
//! IDENTITY.

use std::collections::{HashMap, HashSet};

use kicad_symbols::Orientation;
use spice_policy::CheckedNetlist;
use spice_resolve::{ElementRole, ResolvedElement};

use crate::Placement;

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
    let candidates = collect_candidate_pairs(elems)?;
    let fixed = fixed_nets(elems);
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

/// Group refdes by stem (everything before a trailing `1` or `2`) and
/// return all stems for which both halves exist, with shared lib_id
/// and matching terminal counts.
fn collect_candidate_pairs(elems: &[ResolvedElement]) -> Option<Vec<(usize, usize)>> {
    let mut by_stem: HashMap<String, [Option<usize>; 2]> = HashMap::new();
    for (i, e) in elems.iter().enumerate() {
        if let Some((stem, idx)) = split_refdes(&e.refdes) {
            let slot = by_stem.entry(stem).or_insert([None, None]);
            // Ambiguity (two refdes ending in the same digit per stem)
            // disqualifies that stem.
            if slot[idx].is_some() {
                slot[idx] = None;
                slot[1 - idx] = None;
                continue;
            }
            slot[idx] = Some(i);
        }
    }

    let mut candidates: Vec<(usize, usize)> = by_stem
        .values()
        .filter_map(|slot| match (slot[0], slot[1]) {
            (Some(l), Some(r)) => Some((l, r)),
            _ => None,
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_unstable();

    for &(l, r) in &candidates {
        if elems[l].lib_id != elems[r].lib_id || elems[l].nodes.len() != elems[r].nodes.len() {
            return None;
        }
    }
    Some(candidates)
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

    // Determine the mirror reference. We avoid integer-dividing the
    // axis (which loses 0.5-cell precision on odd sums) and instead
    // store `axis_sum = L.x + R.x` for some chosen reference pair —
    // the mirror operation `R.x = axis_sum - L.x` preserves grid
    // alignment exactly. If a pair is already pinned by the user
    // (`align`/`place` bound both halves), it defines the reference;
    // otherwise we fall back to the placement bbox midpoint.
    let axis_sum = plan
        .pairs
        .iter()
        .find(|&&(l, r)| pinned[l] && pinned[r])
        .map_or_else(
            || {
                let min_x = placement.elements.iter().map(|e| e.origin.x).min().unwrap();
                let max_x = placement.elements.iter().map(|e| e.origin.x).max().unwrap();
                min_x + max_x
            },
            |&(l, r)| placement.elements[l].origin.x + placement.elements[r].origin.x,
        );

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
