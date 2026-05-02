//! V6: topology-aware placement.
//!
//! Recognises canonical analog topologies in the resolved netlist and
//! seeds element coordinates per a built-in template that mirrors how
//! the topology is traditionally drawn (power rails horizontal, signal
//! left-to-right, bias clusters on the input side, …).
//!
//! Each archetype produces a map `refdes -> GridPoint` of *synthetic*
//! seed coordinates. The driver overlays those on the stage-1
//! placement and pins the matched elements so phase-4 auto-fill and
//! stage-3 refinement leave them alone.
//!
//! Archetypes are tried in registration order. The first one whose
//! `match_and_seed` returns `Some` wins; the others are skipped. This
//! keeps the matcher simple while we have only one archetype — V7 and
//! beyond will need a richer composition story (see CLAUDE.md
//! § Visual quality invariants).

use std::collections::HashMap;

use spice_policy::CheckedNetlist;

use crate::{GridPoint, Placement};

mod common_emitter;

/// A topology archetype: pattern-matches a subgraph of the resolved
/// netlist and proposes seed coordinates for its members.
trait Archetype {
    /// Return `Some(seeds)` if this archetype matches the netlist, or
    /// `None` if it does not. `seeds` maps refdes to a grid coordinate.
    fn match_and_seed(&self, checked: &CheckedNetlist) -> Option<HashMap<String, GridPoint>>;
}

/// Run every registered archetype against `checked` and return the
/// first match. Returns an empty map if nothing matches — callers
/// then fall through to the generic placer untouched.
pub(crate) fn detect_and_seed(checked: &CheckedNetlist) -> HashMap<String, GridPoint> {
    let archetypes: Vec<Box<dyn Archetype>> = vec![Box::new(common_emitter::CommonEmitter)];
    for arch in &archetypes {
        if let Some(seeds) = arch.match_and_seed(checked) {
            return seeds;
        }
    }
    HashMap::new()
}

/// Overlay `seeds` onto `placement`. For each refdes in `seeds`, set
/// the element's origin and mark it pinned, but only if the user
/// hasn't already pinned it via `align`/`place`.
pub(crate) fn apply_seeds(
    placement: &mut Placement,
    pinned: &mut [bool],
    seeds: &HashMap<String, GridPoint>,
) {
    for (i, elem) in placement.elements.iter_mut().enumerate() {
        if pinned[i] {
            continue;
        }
        if let Some(&seed) = seeds.get(&elem.refdes) {
            elem.origin = seed;
            pinned[i] = true;
        }
    }
}
