//! Position-stability sidecar (ADR-4).
//!
//! This module defines `<basename>.layout.json`: a stable map from
//! SPICE refdes → final grid position + orientation. On every run the
//! tool writes the freshly-computed placement here; on the *next* run
//! it reads the file back as a [`crate::Hint`] so untouched elements
//! keep their position instead of re-annealing from system entropy.
//!
//! **This is a position-CACHE the tool owns and rewrites every run —
//! NOT a user-annotation carrier.** ADR-4 (docs/layout-adr.md) is
//! explicit on this distinction: the no-config-sidecar rule in
//! CLAUDE.md ("Don't introduce a YAML/TOML/JSON sidecar file") bans
//! encoding *annotations* (user intent) outside the SPICE file. This
//! sidecar encodes no intent — it is derived geometry the converter
//! computes for itself and may delete or overwrite at will. Users who
//! want to pin a position use the SPICE-embedded `*@place` / `*@align`
//! directives, never this file.
//!
//! The format is JSON via `serde` for human-readability and git
//! diffability (ADR-4 "Implications": "versioned, documented, diffable
//! in git").

use std::collections::BTreeMap;
use std::path::Path;

use kicad_symbols::{Orientation, Rotation};
use serde::{Deserialize, Serialize};

use crate::{GridPoint, Placement};

/// Schema version of the sidecar. Bumped if the on-disk shape changes
/// in a way an older reader could misinterpret; readers ignore files
/// whose `version` they do not understand (treated as "no hint").
pub const SIDECAR_VERSION: u32 = 1;

/// One element's cached placement: grid coordinates plus orientation.
///
/// Orientation is stored as the rotation in degrees (0/90/180/270) and
/// a mirror flag, matching the user-facing KiCad notion rather than the
/// internal [`Rotation`] enum — this keeps the JSON self-describing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarEntry {
    /// Grid X (integer multiple of the 1.27 mm schematic grid).
    pub x: i32,
    /// Grid Y.
    pub y: i32,
    /// Rotation in degrees: one of 0, 90, 180, 270.
    pub rotation: u16,
    /// Mirror across the Y axis (horizontal flip).
    pub mirror: bool,
}

impl SidecarEntry {
    /// Convert to the internal [`GridPoint`] + [`Orientation`] pair.
    /// An out-of-range rotation degrades to `R0` (the file is a cache;
    /// a corrupt entry is recovered from, not fatal).
    #[must_use]
    pub fn to_placement(self) -> (GridPoint, Orientation) {
        let rotation = match self.rotation {
            90 => Rotation::R90,
            180 => Rotation::R180,
            270 => Rotation::R270,
            _ => Rotation::R0,
        };
        (
            GridPoint::new(self.x, self.y),
            Orientation {
                rotation,
                mirror_y: self.mirror,
            },
        )
    }

    /// Build a sidecar entry from an internal position + orientation.
    #[must_use]
    pub fn from_placement(origin: GridPoint, orient: Orientation) -> Self {
        Self {
            x: origin.x,
            y: origin.y,
            rotation: orient.rotation.degrees(),
            mirror: orient.mirror_y,
        }
    }
}

/// The whole sidecar file: a version tag plus a refdes→entry map.
///
/// The map is a `BTreeMap` so serialisation is deterministic (sorted
/// by refdes), keeping git diffs minimal across runs that only move a
/// few parts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sidecar {
    pub version: u32,
    pub positions: BTreeMap<String, SidecarEntry>,
}

impl Sidecar {
    /// Build a sidecar snapshot from a finished [`Placement`].
    #[must_use]
    pub fn from_placement(placement: &Placement) -> Self {
        let positions = placement
            .elements
            .iter()
            .map(|e| {
                (
                    e.refdes.clone(),
                    SidecarEntry::from_placement(e.origin, e.orientation),
                )
            })
            .collect();
        Self {
            version: SIDECAR_VERSION,
            positions,
        }
    }

    /// Serialise to pretty JSON.
    ///
    /// # Panics
    /// Never in practice: `Sidecar` is plain data that always
    /// serialises. The `expect` guards an impossible `serde_json` error.
    #[must_use]
    pub fn to_json(&self) -> String {
        // `Sidecar` is always serialisable (plain data); unwrap is safe.
        serde_json::to_string_pretty(self).expect("Sidecar serialises to JSON")
    }

    /// Parse from JSON text. Returns `None` for unparseable input or a
    /// version this build does not understand — the caller then runs as
    /// if no sidecar existed (cache miss, never a hard error).
    #[must_use]
    pub fn from_json(text: &str) -> Option<Self> {
        let parsed: Sidecar = serde_json::from_str(text).ok()?;
        if parsed.version != SIDECAR_VERSION {
            return None;
        }
        Some(parsed)
    }

    /// Convert this cache into a [`crate::Hint`] for the placer.
    #[must_use]
    pub fn to_hint(&self) -> crate::Hint {
        let pins = self
            .positions
            .iter()
            .map(|(refdes, entry)| {
                let (origin, orient) = entry.to_placement();
                (refdes.clone(), (origin, orient))
            })
            .collect();
        crate::Hint { pins }
    }
}

/// Compute the sidecar path next to an emitted `.kicad_sch`.
///
/// `out.kicad_sch` → `out.layout.json`. The `.kicad_sch` extension is
/// replaced wholesale; a path with no extension just gains
/// `.layout.json`.
#[must_use]
pub fn sidecar_path_for(sch_path: &Path) -> std::path::PathBuf {
    sch_path.with_extension("layout.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PlacedElement, Placement};

    fn placement_fixture() -> Placement {
        Placement {
            elements: vec![
                PlacedElement {
                    refdes: "R1".to_string(),
                    lib_id: "Device:R".to_string(),
                    origin: GridPoint::new(3, 7),
                    orientation: Orientation {
                        rotation: Rotation::R90,
                        mirror_y: true,
                    },
                    nodes: vec!["in".to_string(), "out".to_string()],
                    pin_mapping: vec!["1".to_string(), "2".to_string()],
                    value: Some("1k".to_string()),
                    is_power_source: false,
                },
                PlacedElement {
                    refdes: "C1".to_string(),
                    lib_id: "Device:C".to_string(),
                    origin: GridPoint::new(-4, 12),
                    orientation: Orientation::IDENTITY,
                    nodes: vec!["out".to_string(), "0".to_string()],
                    pin_mapping: vec!["1".to_string(), "2".to_string()],
                    value: Some("100n".to_string()),
                    is_power_source: false,
                },
            ],
        }
    }

    #[test]
    fn round_trips_through_json() {
        let placement = placement_fixture();
        let sidecar = Sidecar::from_placement(&placement);
        let json = sidecar.to_json();
        let back = Sidecar::from_json(&json).expect("parse");
        assert_eq!(sidecar, back);
        // Spot-check that orientation degrees + mirror survived.
        let r1 = &back.positions["R1"];
        assert_eq!(r1.rotation, 90);
        assert!(r1.mirror);
        assert_eq!((r1.x, r1.y), (3, 7));
    }

    #[test]
    fn entry_placement_round_trip() {
        for &orient in &Orientation::ALL {
            let origin = GridPoint::new(5, -9);
            let e = SidecarEntry::from_placement(origin, orient);
            let (o2, or2) = e.to_placement();
            assert_eq!(origin, o2);
            assert_eq!(orient, or2);
        }
    }

    #[test]
    fn unknown_version_is_cache_miss() {
        let mut s = Sidecar::from_placement(&placement_fixture());
        s.version = 999;
        let json = serde_json::to_string(&s).unwrap();
        assert!(Sidecar::from_json(&json).is_none());
    }

    #[test]
    fn garbage_is_cache_miss() {
        assert!(Sidecar::from_json("not json").is_none());
        assert!(Sidecar::from_json("{}").is_none()); // missing fields
    }

    #[test]
    fn to_hint_maps_every_entry() {
        let placement = placement_fixture();
        let hint = Sidecar::from_placement(&placement).to_hint();
        assert_eq!(hint.pins.len(), 2);
        let (origin, orient) = hint.pins["R1"];
        assert_eq!(origin, GridPoint::new(3, 7));
        assert_eq!(orient.rotation, Rotation::R90);
        assert!(orient.mirror_y);
    }

    #[test]
    fn sidecar_path_replaces_extension() {
        let p = Path::new("/tmp/out/rc_lowpass.kicad_sch");
        assert_eq!(
            sidecar_path_for(p),
            Path::new("/tmp/out/rc_lowpass.layout.json")
        );
    }
}
