//! **The net-partition certificate** — Tier-0, geometric, in-process.
//!
//! `kicad-emitter` has always been able to answer "is a net *severed*?"
//! geometrically (union-find over the emitted wires). It could not answer
//! the mirror question — "are two nets *merged*?" — geometrically at all.
//! Every merge refusal was a **string match on a router warning**
//! (`v11:`), which meant each new way to fuse two nets needed its own
//! warning string and its own escalation, and the ways that had no
//! escalation (`conflict:`) reached exit 0. See ADR-22.
//!
//! This module answers both questions at once, from one model: rebuild
//! the entire net partition out of the ink about to be written and
//! compare it, class for class, against the pin→net attribution the
//! source netlist supplies. A mismatch is a Tier-0 refusal.
//!
//! # What it models
//!
//! KiCad's connectivity engine joins schematic items by two rules, and
//! this reconstruction implements exactly those two:
//!
//! 1. **Geometric.** Two wires sharing an endpoint are one net; a pin,
//!    power-glyph anchor or label anchor lying on a wire — at an endpoint
//!    OR strictly inside its span — is on that wire's net. (KiCad connects
//!    both; the router relies on it, splitting same-net attachments into
//!    endpoint joins in `spice_route::cleanup`.) Items sharing a
//!    coordinate are the same connection point.
//! 2. **By name.** Power-symbol instances connect to each other by their
//!    `Value` field, and labels connect to each other by their text —
//!    with no wire between them. `power:PWR_FLAG` is excluded: its Value
//!    is literally `PWR_FLAG`, not a net name, so it carries no by-name
//!    connectivity (it still participates geometrically, through the
//!    coordinate it shares with the rail pin it drives).
//!
//! `(junction …)` items add no edges: a junction is only ever emitted at
//! a coordinate where three or more wire *endpoints* already meet, which
//! rule 1 has joined already.
//!
//! # Why the engine is shared and the inputs are not
//!
//! [`check_partition`] is deliberately input-agnostic — it takes a
//! [`SheetGeometry`] and a slice of [`Terminal`]s, never an emitter type.
//! Production builds both from the in-memory `Sexpr` items and
//! `collect_net_pins`; the A2 verifier
//! (`spice2kicad/tests/roundtrip_connectivity.rs`) builds both by an
//! independent route — it re-parses the `.cir` through
//! `spice-parser`/`spice-resolve`, re-derives each terminal's world
//! coordinate from the library through the emitted pose, and reads the
//! geometry back off the `.kicad_sch` *file on disk*.
//!
//! That split is the point. The production check grades the router's
//! output against the router's own input (`collect_net_pins` feeds both),
//! so it is structurally blind to an attribution or pose bug — it would
//! bless a wire drawn to the wrong pin because both sides agree on where
//! that pin is. It is equally blind to anything that happens after it:
//! page translation and S-expression serialisation. A2 covers both axes
//! precisely because its inputs are derived independently and read back
//! from the written bytes. Sharing the union-find engine costs nothing
//! there (a second hand-written union-find would encode the *same*
//! beliefs about KiCad's semantics, so it could only agree and be wrong
//! together); sharing the inputs would cost everything.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// One component terminal — a pin whose source net is known.
///
/// The terminal set is the shared vertex set of the two partitions being
/// compared: the source netlist groups them by net name, the emitted
/// geometry groups them by reconstructed component.
#[derive(Debug, Clone)]
pub struct Terminal {
    /// Identity for diagnostics only (`"R1.2"`, `"vcc#0"`). Never used
    /// for grouping.
    pub id: String,
    /// The net this pin belongs to per the source netlist.
    pub net: String,
    /// World coordinate, millimetres, in the same frame as
    /// [`SheetGeometry`].
    pub at: (f64, f64),
}

/// The connectivity-bearing ink of one sheet.
#[derive(Debug, Default, Clone)]
pub struct SheetGeometry {
    /// Every `(wire …)` segment as a world-mm endpoint pair.
    pub wires: Vec<((f64, f64), (f64, f64))>,
    /// Every by-name connection point: `(name, anchor)`. Power-glyph
    /// anchors carry the glyph's `Value`; labels carry their text.
    /// `PWR_FLAG` glyphs are not members.
    pub named_anchors: Vec<(String, (f64, f64))>,
}

/// A way in which the emitted geometry fails to reconstruct the source
/// net partition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartitionFinding {
    /// Two or more distinct source nets landed in one geometric
    /// component: KiCad will import them as a single net. A short.
    Merge {
        nets: Vec<String>,
        terminals: Vec<String>,
    },
    /// One source net reconstructs as two or more disjoint components:
    /// KiCad will import it as several nets. An open.
    Split {
        net: String,
        islands: Vec<Vec<String>>,
    },
}

impl fmt::Display for PartitionFinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Merge { nets, terminals } => write!(
                f,
                "MERGE: source nets {nets:?} share one geometric component \
                 (KiCad imports them as ONE net); terminals {terminals:?}"
            ),
            Self::Split { net, islands } => write!(
                f,
                "SPLIT: source net {:?} reconstructs as {} disconnected islands \
                 (KiCad imports it as {} nets): {islands:?}",
                net,
                islands.len(),
                islands.len(),
            ),
        }
    }
}

/// Quantise millimetres to integer micrometres. Every coordinate the
/// emitter writes is an exact multiple of the 1.27 mm grid divided by a
/// small integer, so 1 µm buckets give exact identity without an epsilon.
#[allow(clippy::cast_possible_truncation)]
fn qkey(x: f64, y: f64) -> (i64, i64) {
    ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64)
}

/// True when `p` lies on the axis-aligned span `a`–`b`, endpoints
/// included. A diagonal segment (already a defect elsewhere) covers
/// nothing.
fn on_span(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> bool {
    const EPS: f64 = 1e-6;
    if (a.1 - b.1).abs() < EPS && (p.1 - a.1).abs() < EPS {
        return p.0 >= a.0.min(b.0) - EPS && p.0 <= a.0.max(b.0) + EPS;
    }
    if (a.0 - b.0).abs() < EPS && (p.0 - a.0).abs() < EPS {
        return p.1 >= a.1.min(b.1) - EPS && p.1 <= a.1.max(b.1) + EPS;
    }
    false
}

/// Index-based union-find with path halving.
struct UnionFind(Vec<usize>);

impl UnionFind {
    fn new(n: usize) -> Self {
        Self((0..n).collect())
    }
    fn find(&mut self, mut x: usize) -> usize {
        while self.0[x] != x {
            self.0[x] = self.0[self.0[x]];
            x = self.0[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.0[ra] = rb;
        }
    }
}

/// Reconstruct the net partition from `geometry` and compare it against
/// the source partition implied by `terminals`.
///
/// Returns every disagreement; an empty result is the certificate that
/// the emitted ink draws exactly the source circuit's connectivity —
/// no net merged, no net severed.
///
/// A net with a single terminal cannot split, and a terminal set with no
/// two distinct nets cannot merge; both fall out of the algorithm rather
/// than being special-cased.
#[must_use]
pub fn check_partition(terminals: &[Terminal], geometry: &SheetGeometry) -> Vec<PartitionFinding> {
    // ---- intern every coordinate the model can reference -------------
    let mut idx: BTreeMap<(i64, i64), usize> = BTreeMap::new();
    let mut intern = |k: (i64, i64)| -> usize {
        let n = idx.len();
        *idx.entry(k).or_insert(n)
    };
    for (a, b) in &geometry.wires {
        intern(qkey(a.0, a.1));
        intern(qkey(b.0, b.1));
    }
    for t in terminals {
        intern(qkey(t.at.0, t.at.1));
    }
    for (_, c) in &geometry.named_anchors {
        intern(qkey(c.0, c.1));
    }
    let mut uf = UnionFind::new(idx.len());
    let node = |c: (f64, f64)| idx[&qkey(c.0, c.1)];

    // ---- rule 1a: a wire joins its own two endpoints ------------------
    for (a, b) in &geometry.wires {
        uf.union(node(*a), node(*b));
    }

    // ---- rule 1b: an anchor lying on a wire joins that wire -----------
    // Endpoint coincidence is already covered by the shared coordinate
    // key; this adds the strict-interior case, which KiCad connects too.
    let anchors = terminals
        .iter()
        .map(|t| t.at)
        .chain(geometry.named_anchors.iter().map(|(_, c)| *c));
    for p in anchors {
        for (a, b) in &geometry.wires {
            if on_span(p, *a, *b) {
                uf.union(node(p), node(*a));
            }
        }
    }

    // ---- rule 2: same-name power glyphs / labels join by name ---------
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (name, c) in &geometry.named_anchors {
        by_name.entry(name.as_str()).or_default().push(node(*c));
    }
    for members in by_name.values() {
        for w in members.windows(2) {
            uf.union(w[0], w[1]);
        }
    }

    // ---- compare the two partitions ----------------------------------
    let mut comp_of: Vec<usize> = Vec::with_capacity(terminals.len());
    for t in terminals {
        let c = uf.find(node(t.at));
        comp_of.push(c);
    }

    let mut out = Vec::new();

    // no merge: one component carries at most one source net.
    let mut comp_nets: BTreeMap<usize, BTreeSet<&str>> = BTreeMap::new();
    for (t, c) in terminals.iter().zip(&comp_of) {
        comp_nets.entry(*c).or_default().insert(t.net.as_str());
    }
    for (comp, nets) in &comp_nets {
        if nets.len() > 1 {
            let mut members: Vec<String> = terminals
                .iter()
                .zip(&comp_of)
                .filter(|(_, c)| *c == comp)
                .map(|(t, _)| format!("{} ({})", t.id, t.net))
                .collect();
            members.sort();
            out.push(PartitionFinding::Merge {
                nets: nets.iter().map(|s| (*s).to_string()).collect(),
                terminals: members,
            });
        }
    }

    // no split: one source net occupies exactly one component.
    let mut net_comps: BTreeMap<&str, BTreeMap<usize, Vec<&str>>> = BTreeMap::new();
    for (t, c) in terminals.iter().zip(&comp_of) {
        net_comps
            .entry(t.net.as_str())
            .or_default()
            .entry(*c)
            .or_default()
            .push(t.id.as_str());
    }
    for (net, islands) in &net_comps {
        if islands.len() > 1 {
            out.push(PartitionFinding::Split {
                net: (*net).to_string(),
                islands: islands
                    .values()
                    .map(|ids| ids.iter().map(|s| (*s).to_string()).collect())
                    .collect(),
            });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: &str, net: &str, x: f64, y: f64) -> Terminal {
        Terminal {
            id: id.into(),
            net: net.into(),
            at: (x, y),
        }
    }

    #[test]
    fn a_wired_two_pin_net_certifies() {
        let terms = vec![t("R1.1", "a", 0.0, 0.0), t("R2.1", "a", 10.0, 0.0)];
        let geom = SheetGeometry {
            wires: vec![((0.0, 0.0), (10.0, 0.0))],
            named_anchors: vec![],
        };
        assert!(check_partition(&terms, &geom).is_empty());
    }

    #[test]
    fn a_pin_on_a_wire_interior_is_connected() {
        let terms = vec![
            t("R1.1", "a", 0.0, 0.0),
            t("R2.1", "a", 10.0, 0.0),
            t("R3.1", "a", 5.0, 0.0),
        ];
        let geom = SheetGeometry {
            wires: vec![((0.0, 0.0), (10.0, 0.0))],
            named_anchors: vec![],
        };
        assert!(check_partition(&terms, &geom).is_empty());
    }

    #[test]
    fn a_missing_wire_is_a_split() {
        let terms = vec![t("R1.1", "a", 0.0, 0.0), t("R2.1", "a", 10.0, 0.0)];
        let geom = SheetGeometry::default();
        assert!(matches!(
            check_partition(&terms, &geom).as_slice(),
            [PartitionFinding::Split { net, .. }] if net == "a"
        ));
    }

    #[test]
    fn a_wire_ending_on_a_foreign_pin_is_a_merge() {
        // Net `a` is wired 0→10; net `b`'s pin sits at 10, i.e. on that
        // wire's endpoint. This is the `v11:` hazard, geometrically.
        let terms = vec![
            t("R1.1", "a", 0.0, 0.0),
            t("R2.1", "a", 10.0, 0.0),
            t("R3.1", "b", 10.0, 0.0),
            t("R4.1", "b", 20.0, 0.0),
        ];
        let geom = SheetGeometry {
            wires: vec![((0.0, 0.0), (10.0, 0.0)), ((10.0, 0.0), (20.0, 0.0))],
            named_anchors: vec![],
        };
        let found = check_partition(&terms, &geom);
        assert!(
            found
                .iter()
                .any(|f| matches!(f, PartitionFinding::Merge { .. })),
            "{found:?}"
        );
    }

    #[test]
    fn pin_on_pin_across_nets_is_a_merge_with_no_wires_at_all() {
        let terms = vec![t("R1.1", "a", 4.0, 4.0), t("R2.1", "b", 4.0, 4.0)];
        let found = check_partition(&terms, &SheetGeometry::default());
        assert!(matches!(found.as_slice(), [PartitionFinding::Merge { .. }]));
    }

    #[test]
    fn same_name_anchors_connect_without_a_wire() {
        // Two GND pins, no wire between them: connected by their glyphs.
        let terms = vec![t("R1.2", "0", 0.0, 0.0), t("R2.2", "0", 50.0, 50.0)];
        let geom = SheetGeometry {
            wires: vec![],
            named_anchors: vec![
                ("GND".to_string(), (0.0, 0.0)),
                ("GND".to_string(), (50.0, 50.0)),
            ],
        };
        assert!(check_partition(&terms, &geom).is_empty());
    }

    #[test]
    fn a_label_renamed_onto_a_foreign_net_is_a_merge() {
        let terms = vec![
            t("R1.1", "a", 0.0, 0.0),
            t("R2.1", "a", 0.0, 10.0),
            t("R3.1", "b", 50.0, 0.0),
            t("R4.1", "b", 50.0, 10.0),
        ];
        let geom = SheetGeometry {
            wires: vec![((0.0, 0.0), (0.0, 10.0)), ((50.0, 0.0), (50.0, 10.0))],
            // Both islands labelled `n1` — KiCad fuses them by name.
            named_anchors: vec![
                ("n1".to_string(), (0.0, 0.0)),
                ("n1".to_string(), (50.0, 0.0)),
            ],
        };
        assert!(
            check_partition(&terms, &geom)
                .iter()
                .any(|f| matches!(f, PartitionFinding::Merge { .. }))
        );
    }

    #[test]
    fn distinct_names_do_not_connect() {
        let terms = vec![
            t("R1.1", "a", 0.0, 0.0),
            t("R2.1", "a", 0.0, 10.0),
            t("R3.1", "b", 50.0, 0.0),
            t("R4.1", "b", 50.0, 10.0),
        ];
        let geom = SheetGeometry {
            wires: vec![((0.0, 0.0), (0.0, 10.0)), ((50.0, 0.0), (50.0, 10.0))],
            named_anchors: vec![
                ("a".to_string(), (0.0, 0.0)),
                ("b".to_string(), (50.0, 0.0)),
            ],
        };
        assert!(check_partition(&terms, &geom).is_empty());
    }

    #[test]
    fn a_single_pin_net_never_splits() {
        let terms = vec![t("R1.1", "dangling", 0.0, 0.0)];
        assert!(check_partition(&terms, &SheetGeometry::default()).is_empty());
    }
}
