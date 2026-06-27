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

use std::collections::HashMap;

use spice_policy::CheckedNetlist;
use spice_resolve::ElementKind;

use kicad_symbols::Orientation;

use crate::{GridPoint, Placement, WorldExtent, vertical_stride_cells, world_extent};

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
        // Respect any element already pinned by a stronger (user or V7)
        // constraint — never override it.
        if pinned[upper] || pinned[lower] {
            continue;
        }

        // Both members keep identity orientation (matching the align
        // path, which pins members before `pick_orientations`). The
        // vertical stride covers both resolved extents plus clearance,
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
}
