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
pub const SIDECAR_VERSION: u32 = 3;

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

/// The uniform page shift the emitter applied to one sheet, in whole
/// grid cells (1.27 mm).
///
/// **Why this is cached (V15 / ADR-4).** The emitter's final V15 pass
/// shifts a sheet so its content bounding box clears the page margin.
/// Recomputing that shift from the bbox on every run makes the page
/// frame depend on the content: add one element whose decoration
/// extends the bbox leftward and the whole sheet re-anchors, panning
/// every *existing* element uniformly — measured at
/// `Δ = (+5.08, −1.27) mm` on a 2-element circuit gaining a third,
/// with placer grid coordinates bit-identical across both runs. No
/// placer change can fix that (any uniform pre-translation cancels in
/// the normalisation), so the shift itself is cached and replayed.
/// V15 is `min ≥ margin`, not `min == margin`, so replaying a shift is
/// fully conformant; the emitter re-normalises whenever the replayed
/// shift would push content off the page, which bounds any drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageShiftEntry {
    /// Horizontal shift in grid cells.
    pub cells_x: i64,
    /// Vertical shift in grid cells.
    pub cells_y: i64,
}

/// Sidecar key under which the root sheet's page shift is stored. Child
/// sheets are keyed by their subckt name, which can never collide with
/// this (SPICE identifiers do not contain `<`/`>`).
pub const ROOT_SHEET_KEY: &str = "<root>";

/// The whole sidecar file: a version tag plus a refdes→entry map.
///
/// The map is a `BTreeMap` so serialisation is deterministic (sorted
/// by refdes), keeping git diffs minimal across runs that only move a
/// few parts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sidecar {
    pub version: u32,
    /// Source netlist these positions were computed from.
    ///
    /// The cache is keyed by *output path*, so without this a second
    /// netlist written to the same path inherits the first one's
    /// placement. That is not merely untidy: the hint drags shared
    /// refdes to coordinates chosen for a different circuit, and the
    /// router can then fail to connect a net at all — measured on
    /// `opamp_definition_level`, whose net `out1` came out disconnected
    /// after `opamp_inverting` had written the same output path, which
    /// KiCad's own netlist export confirmed as `unconnected-`.
    ///
    /// Identity is the *source path*, deliberately, not a digest of the
    /// element set. The whole point of this cache (ADR-4) is that
    /// **editing** a netlist leaves untouched elements where they were,
    /// so anything that changes when the circuit is edited would defeat
    /// it — a refdes-set fingerprint invalidates the cache the moment a
    /// part is added, which is precisely when position stability matters
    /// most. Same file re-converted → hit; a different file written to
    /// the same output → miss.
    #[serde(default)]
    pub circuit: String,
    pub positions: BTreeMap<String, SidecarEntry>,
    /// Sheet name → the V15 page shift the emitter applied last run.
    /// The root sheet is keyed by [`ROOT_SHEET_KEY`]; each hierarchical
    /// child sheet by its subckt name. Absent (or absent for a given
    /// sheet) → the emitter normalises that sheet's bbox onto the page
    /// margin, exactly as before this field existed.
    #[serde(default)]
    pub page_shifts: BTreeMap<String, PageShiftEntry>,
}

/// Canonical identity string for a source netlist path.
///
/// Canonicalised where possible so `./x.cir` and `x.cir` agree; falls
/// back to the path as given when the file cannot be resolved.
#[must_use]
pub fn source_id(source: &Path) -> String {
    std::fs::canonicalize(source)
        .unwrap_or_else(|_| source.to_path_buf())
        .to_string_lossy()
        .into_owned()
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
            circuit: String::new(),
            positions,
            page_shifts: BTreeMap::new(),
        }
    }

    /// Stamp the source netlist this placement came from. Separate from
    /// [`Sidecar::from_placement`] so callers that only need the
    /// positions are unaffected.
    #[must_use]
    pub fn with_source(mut self, source: &Path) -> Self {
        self.circuit = source_id(source);
        self
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
                    power_rail: None,
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
                    power_rail: None,
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
    fn page_shifts_round_trip() {
        let mut s = Sidecar::from_placement(&placement_fixture());
        s.page_shifts.insert(
            ROOT_SHEET_KEY.to_string(),
            PageShiftEntry {
                cells_x: 24,
                cells_y: -8,
            },
        );
        let back = Sidecar::from_json(&s.to_json()).expect("parse");
        assert_eq!(back.page_shifts[ROOT_SHEET_KEY].cells_x, 24);
        assert_eq!(back.page_shifts[ROOT_SHEET_KEY].cells_y, -8);
    }

    #[test]
    fn missing_page_shifts_field_still_parses() {
        // A v3 file written before a sheet's shift was known (or by a
        // reader that omits the field) is a hit with no preferred shift,
        // not a parse failure — the emitter then normalises.
        let json = format!(r#"{{"version":{SIDECAR_VERSION},"circuit":"x.cir","positions":{{}}}}"#);
        let parsed = Sidecar::from_json(&json).expect("parse");
        assert!(parsed.page_shifts.is_empty());
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
