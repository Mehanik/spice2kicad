//! Signal-flow **root** detection — the one policy shared by every
//! consumer that needs to know where the signal enters the circuit.
//!
//! Two functions in this crate have to agree on which nets and which
//! elements are signal-flow roots:
//!
//! * [`crate::layers::assign_x_layers_with`] needs *element* roots for a
//!   longest-path DAG layering (X = depth along the signal path);
//! * [`crate::idioms::signal_net_depth`] needs *net* depths from a
//!   shortest-hop BFS (which way a series element's signal runs).
//!
//! The traversals legitimately differ. The **root set** must not — and
//! historically it did, in three independent places, each of which
//! produced a visible defect (ADR-24 D4 records two of them). This
//! module owns the root set; the traversals stay where they are.
//!
//! For now it holds only the two shared predicates, moved here verbatim
//! from `layers.rs`.

use spice_policy::CheckedNetlist;
use spice_resolve::{ElementKind, ElementRole, PortDir};

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
