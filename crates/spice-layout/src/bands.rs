//! Y-band assignment from net classification. See spec §4.
//!
//! For each element, examines the set of [`NetClass`]es of its connected
//! nets to decide which vertical band it belongs to and a soft Y target
//! fraction within the schematic sheet (0.0 = top, 1.0 = bottom).

use spice_policy::CheckedNetlist;

use crate::net_class::{NetClass, NetClassMap};

/// Vertical band for an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// Power rail row at the top of the sheet.
    Top,
    /// Signal / intermediate rows in the middle.
    Mid,
    /// Ground rail row at the bottom of the sheet.
    Bot,
}

/// Band assignment for a single element.
#[derive(Debug, Clone)]
pub struct BandAssignment {
    pub band: Band,
    /// Fractional Y position within the sheet `[0.0, 1.0]`.
    /// 0.0 = top, 1.0 = bottom. Used as a soft placement target by the
    /// force-directed auto-fill pass (phase 4 of spec §5).
    pub soft_y_target_frac: f64,
}

/// Assign a [`BandAssignment`] to every element in `checked`, in the
/// same index order as `checked.elements`.
///
/// Each element's connected net classes are collected, then one of the
/// six cases in spec §4 is matched:
///
/// | Classes on element nets           | Band | frac |
/// |-----------------------------------|------|------|
/// | Power only                        | Top  | 0.0  |
/// | Ground only                       | Bot  | 1.0  |
/// | Power + Ground (± Signal)         | Mid  | 0.5  |
/// | Power + Signal (no Ground)        | Mid  | 1/3  |
/// | Ground + Signal (no Power)        | Mid  | 2/3  |
/// | Signal only **or** no connection  | Mid  | 0.5  |
pub fn assign_y_bands(checked: &CheckedNetlist, classes: &NetClassMap) -> Vec<BandAssignment> {
    checked
        .elements
        .iter()
        .map(|el| {
            let mut has_power = false;
            let mut has_ground = false;
            let mut has_signal = false;

            for net in &el.nodes {
                match classes.get(net.as_str()) {
                    Some(NetClass::Power) => has_power = true,
                    Some(NetClass::Ground) => has_ground = true,
                    Some(NetClass::Signal) => has_signal = true,
                    None => {}
                }
            }

            let (band, soft_y_target_frac) = match (has_power, has_ground, has_signal) {
                // Power only → Top
                (true, false, false) => (Band::Top, 0.0),
                // Ground only → Bot
                (false, true, false) => (Band::Bot, 1.0),
                // Power + Signal (no Ground) → upper-mid
                (true, false, true) => (Band::Mid, 1.0 / 3.0),
                // Ground + Signal (no Power) → lower-mid
                (false, true, true) => (Band::Mid, 2.0 / 3.0),
                // Power + Ground (± Signal), Signal only, or no connection → Mid centre
                _ => (Band::Mid, 0.5),
            };

            BandAssignment {
                band,
                soft_y_target_frac,
            }
        })
        .collect()
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

    /// Parse a SPICE source string, resolve, check, classify, and assign bands.
    /// Returns `(checked_elements_refdeses, band_assignments)` parallel vectors.
    fn assign_str(src: &str) -> Vec<(String, BandAssignment)> {
        let file_id = FileId(0);
        let parsed = spice_parser::parse(src, file_id)
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let (checked, _warns) = check(resolved).expect("policy check failed");
        let classes = classify_nets(&checked);
        let bands = assign_y_bands(&checked, &classes);
        checked
            .elements
            .iter()
            .map(|e| e.refdes.clone())
            .zip(bands)
            .collect()
    }

    /// Helper: find the band assignment for a given refdes.
    fn find(assignments: &[(String, BandAssignment)], refdes: &str) -> BandAssignment {
        assignments.iter().find(|(r, _)| r == refdes).map_or_else(
            || panic!("refdes {refdes} not found in assignments"),
            |(_, b)| b.clone(),
        )
    }

    /// R1 connects vcc (Power) and out (Signal) → Power + Signal → Mid, frac < 0.5.
    #[test]
    fn power_to_signal_resistor_is_mid_top_biased() {
        let assignments = assign_str("test\nV1 vcc 0 12 ;@ power=vcc\nR1 vcc out 1k\n.end\n");
        let r1 = find(&assignments, "R1");
        assert_eq!(r1.band, Band::Mid, "R1 should be in Mid band");
        assert!(
            r1.soft_y_target_frac < 0.5,
            "R1 frac should be < 0.5, got {}",
            r1.soft_y_target_frac
        );
    }

    /// R1 connects emit (Signal) and 0 (Ground) → Ground + Signal → Mid, frac > 0.5.
    #[test]
    fn signal_to_ground_resistor_is_mid_bot_biased() {
        let assignments = assign_str("test\nV1 vcc 0 12 ;@ power=vcc\nR1 emit 0 1k\n.end\n");
        let r1 = find(&assignments, "R1");
        assert_eq!(r1.band, Band::Mid, "R1 should be in Mid band");
        assert!(
            r1.soft_y_target_frac > 0.5,
            "R1 frac should be > 0.5, got {}",
            r1.soft_y_target_frac
        );
    }

    /// R1 connects in (Signal) and mid (Signal) → Signal only → Mid, frac ≈ 0.5.
    #[test]
    fn signal_only_resistor_has_no_bias() {
        let assignments = assign_str("test\nV1 in 0 AC 1\nR1 in mid 1k\nC1 mid 0 1u\n.end\n");
        let r1 = find(&assignments, "R1");
        assert_eq!(r1.band, Band::Mid, "R1 should be in Mid band");
        assert!(
            (r1.soft_y_target_frac - 0.5).abs() < 1e-6,
            "R1 frac should be ≈ 0.5, got {}",
            r1.soft_y_target_frac
        );
    }
}
