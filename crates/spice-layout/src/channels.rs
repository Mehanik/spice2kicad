//! Signal-component decomposition → Y **rows** (V6, Y-axis).
//!
//! [`bands`](crate::bands) answers "is this element on a rail row or in
//! the middle?". It deliberately says nothing about *which* middle row,
//! because its input — the set of [`NetClass`]es an element touches — is
//! blind to the one structural fact that separates independent
//! sub-circuits: whether any **signal** path connects them at all.
//!
//! A dual op-amp deck, a stereo pair, two cascaded-but-separate filter
//! chains: every element in each is Signal-only, so `assign_y_bands`
//! correctly returns `Mid / 0.50` for all of them and the Y axis is left
//! completely unconstrained. The SA's `soft_y_residual` then pulls every
//! element toward one shared midline, and the channels interleave
//! diagonally instead of stacking as clean rows.
//!
//! The fix is the decomposition [`symmetry::retain_coupled_pairs`] already
//! relies on: union-find over **non-rail** nets. Two elements in distinct
//! components are, by construction, independent sub-circuits — they share
//! nothing but the supply — so they belong in distinct horizontal rows.
//! That is a structural derivation from graph topology alone: no refdes,
//! no element kind, no named topology (CLAUDE.md principle 9).
//!
//! **No-op by construction on single-component circuits.** When fewer
//! than two multi-element components exist, [`assign_rows`] returns
//! `count == 1` and every row `None`, and [`row_adjusted_frac`] is then
//! the identity on `soft_y_target_frac`. Every consumer therefore behaves
//! exactly as it did before rows existed.

use std::collections::HashMap;

use spice_policy::CheckedNetlist;

use crate::bands::Band;
use crate::net_class::{NetClass, NetClassMap};

/// Per-element row assignment plus the number of rows in play.
#[derive(Debug, Clone)]
pub struct Rows {
    /// Row index per element, in `checked.elements` order. `None` means
    /// the element belongs to no channel — it touches only rail nets (a
    /// `*@power` source) or stands alone — and is left at the vertical
    /// centre rather than dragged into some channel's row.
    pub row: Vec<Option<usize>>,
    /// Number of distinct rows. `1` means "no row structure detected";
    /// every consumer must then be a no-op.
    pub count: usize,
}

impl Rows {
    /// True when no row structure was detected, i.e. every consumer must
    /// behave exactly as it did before rows existed.
    #[must_use]
    pub fn is_trivial(&self) -> bool {
        self.count <= 1
    }
}

/// Decompose `checked` into signal-connected components and turn the
/// components into row indices.
///
/// Components are built by union-find over nets that are **not** Power or
/// Ground: a rail is shared by construction and joins nothing. Only
/// components holding **two or more** elements count as channels — a lone
/// element joined to nothing has no channel identity to speak of, and
/// giving it a row of its own would invent structure the netlist does not
/// have.
///
/// Rows are numbered by each component's smallest element index, so the
/// assignment is deterministic and independent of `HashMap` iteration.
#[must_use]
pub fn assign_rows(checked: &CheckedNetlist, classes: &NetClassMap) -> Rows {
    let n = checked.elements.len();
    let trivial = Rows {
        row: vec![None; n],
        count: 1,
    };
    if n == 0 {
        return trivial;
    }

    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }

    let mut parent: Vec<usize> = (0..n).collect();
    let mut by_net: HashMap<&str, usize> = HashMap::new();
    for (i, e) in checked.elements.iter().enumerate() {
        for node in &e.nodes {
            // Rails carry no signal: joining through one would merge
            // every channel in the circuit into a single component.
            if matches!(
                classes.get(node.as_str()),
                Some(NetClass::Power | NetClass::Ground)
            ) {
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

    // Component root → (member count, smallest member index).
    let mut members: HashMap<usize, (usize, usize)> = HashMap::new();
    let roots: Vec<usize> = (0..n).map(|i| find(&mut parent, i)).collect();
    for (i, &r) in roots.iter().enumerate() {
        let e = members.entry(r).or_insert((0, i));
        e.0 += 1;
        e.1 = e.1.min(i);
    }

    // Channels: components with ≥ 2 elements, ordered by first member.
    let mut channels: Vec<(usize, usize)> = members
        .iter()
        .filter(|(_, (count, _))| *count >= 2)
        .map(|(&root, &(_, first))| (first, root))
        .collect();
    channels.sort_unstable();

    if channels.len() < 2 {
        return trivial;
    }

    let row_of_root: HashMap<usize, usize> = channels
        .iter()
        .enumerate()
        .map(|(row, &(_, root))| (root, row))
        .collect();

    Rows {
        row: roots.iter().map(|r| row_of_root.get(r).copied()).collect(),
        count: channels.len(),
    }
}

/// Fold an element's row into its `soft_y_target_frac`, producing the
/// fraction the SA's Y terms should actually aim at.
///
/// Each row owns a `1 / count` slice of the vertical span, and the
/// element's own `frac` positions it inside that slice. `Top` / `Bot`
/// keep their rail fractions untouched — the rails run across the whole
/// sheet and are not part of any one channel.
///
/// With `count == 1` (and therefore `row == None` everywhere) this is the
/// identity, which is what makes rows a no-op on single-component
/// circuits.
#[must_use]
pub fn row_adjusted_frac(band: Band, frac: f64, row: Option<usize>, count: usize) -> f64 {
    match (band, row) {
        (Band::Mid, Some(r)) if count > 1 => {
            #[allow(clippy::cast_precision_loss)]
            let (r, count) = (r as f64, count as f64);
            (r + frac) / count
        }
        _ => frac,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kicad_symbols::Library;
    use spice_diagnostics::FileId;
    use spice_policy::check;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use crate::net_class::classify_nets;

    fn fixture_library() -> &'static Library {
        static LIB: OnceLock<Library> = OnceLock::new();
        LIB.get_or_init(|| {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let fixture_dir = manifest
                .parent()
                .and_then(std::path::Path::parent)
                .expect("workspace root")
                .join("crates/kicad-symbols/tests/fixtures");
            let device = Library::from_file(fixture_dir.join("Device.kicad_sym"))
                .expect("load Device fixture library");
            let spice = Library::from_file(fixture_dir.join("Simulation_SPICE.kicad_sym"))
                .expect("load Simulation_SPICE fixture library");
            device.merge(spice)
        })
    }

    fn rows_of(src: &str) -> Vec<(String, Option<usize>)> {
        let parsed = spice_parser::parse(src, FileId(0))
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let (checked, _warns) = check(resolved).expect("policy check failed");
        let classes = classify_nets(&checked);
        let rows = assign_rows(&checked, &classes);
        checked
            .elements
            .iter()
            .map(|e| e.refdes.clone())
            .zip(rows.row)
            .collect()
    }

    fn row_count(src: &str) -> usize {
        let parsed = spice_parser::parse(src, FileId(0))
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let (checked, _warns) = check(resolved).expect("policy check failed");
        let classes = classify_nets(&checked);
        assign_rows(&checked, &classes).count
    }

    /// A single signal-connected circuit has no row structure: the pass
    /// must be a strict no-op.
    #[test]
    fn single_component_circuit_gets_no_rows() {
        let src = "test\nV1 in 0 AC 1\nR1 in mid 1k\nC1 mid 0 1u\n.end\n";
        assert_eq!(row_count(src), 1, "one component must yield one row");
        assert!(
            rows_of(src).iter().all(|(_, r)| r.is_none()),
            "no element may be assigned a row in a single-component circuit"
        );
    }

    /// Two RC chains sharing only ground are independent channels and
    /// must land in distinct rows.
    #[test]
    fn uncoupled_repeated_channels_get_distinct_rows() {
        let src = "test\n\
             R1 in1 mid1 1k\nC1 mid1 0 1u\n\
             R2 in2 mid2 1k\nC2 mid2 0 1u\n.end\n";
        assert_eq!(row_count(src), 2, "two uncoupled chains are two rows");
        let rows = rows_of(src);
        let get = |r: &str| {
            rows.iter()
                .find(|(x, _)| x == r)
                .and_then(|(_, v)| *v)
                .unwrap_or_else(|| panic!("{r} has no row"))
        };
        assert_eq!(get("R1"), get("C1"), "one chain is one row");
        assert_eq!(get("R2"), get("C2"), "one chain is one row");
        assert_ne!(get("R1"), get("R2"), "distinct chains are distinct rows");
    }

    /// Sharing a *rail* does not couple two channels — that is the whole
    /// reason the union-find skips Power/Ground nets.
    #[test]
    fn sharing_only_a_supply_rail_does_not_merge_rows() {
        let src = "test\nV1 vcc 0 12 ;@ power=vcc\n\
             R1 vcc mid1 1k\nC1 mid1 in1 1u\n\
             R2 vcc mid2 1k\nC2 mid2 in2 1u\n.end\n";
        assert_eq!(row_count(src), 2, "a shared rail must not merge channels");
    }

    /// A genuine signal path between the halves keeps them in one row —
    /// the predicate must not shatter a coupled circuit.
    #[test]
    fn a_signal_path_between_halves_keeps_one_row() {
        let src = "test\n\
             R1 in1 mid1 1k\nC1 mid1 0 1u\n\
             R2 mid1 mid2 1k\nC2 mid2 0 1u\n.end\n";
        assert_eq!(row_count(src), 1, "coupled halves are a single component");
    }

    /// A lone element joined to nothing invents no row of its own.
    #[test]
    fn an_isolated_element_is_not_a_channel() {
        let src = "test\n\
             R1 in1 mid1 1k\nC1 mid1 0 1u\n\
             R2 in2 0 1k\n.end\n";
        assert_eq!(row_count(src), 1, "one chain plus a stub is one channel");
    }

    /// The fold is the identity whenever no row structure was detected.
    #[test]
    fn row_adjusted_frac_is_identity_without_rows() {
        for band in [Band::Top, Band::Mid, Band::Bot] {
            for frac in [0.0, 1.0 / 3.0, 0.5, 2.0 / 3.0, 1.0] {
                assert!(
                    (row_adjusted_frac(band, frac, None, 1) - frac).abs() < 1e-12,
                    "must be the identity with no rows"
                );
            }
        }
    }

    /// Rails span the whole sheet and keep their fractions even when the
    /// middle is split into rows.
    #[test]
    fn rails_keep_their_fraction_under_rows() {
        assert!((row_adjusted_frac(Band::Top, 0.0, Some(1), 2) - 0.0).abs() < 1e-12);
        assert!((row_adjusted_frac(Band::Bot, 1.0, Some(0), 2) - 1.0).abs() < 1e-12);
    }

    /// Two rows split the span in half, and the element's own frac
    /// positions it inside its half.
    #[test]
    fn rows_slice_the_span_in_order() {
        assert!((row_adjusted_frac(Band::Mid, 0.5, Some(0), 2) - 0.25).abs() < 1e-12);
        assert!((row_adjusted_frac(Band::Mid, 0.5, Some(1), 2) - 0.75).abs() < 1e-12);
        assert!(
            row_adjusted_frac(Band::Mid, 1.0 / 3.0, Some(0), 2)
                < row_adjusted_frac(Band::Mid, 2.0 / 3.0, Some(0), 2),
            "frac still orders elements inside a row"
        );
        assert!(
            row_adjusted_frac(Band::Mid, 1.0, Some(0), 2)
                <= row_adjusted_frac(Band::Mid, 0.0, Some(1), 2),
            "rows never interleave"
        );
    }
}
