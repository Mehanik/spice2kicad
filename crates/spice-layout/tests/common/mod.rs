//! Shared helpers for `spice-layout` tests.
//!
//! Each test crate owns its own helpers — we deliberately do not
//! import test code from sibling crates (per stage-1 instructions).

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::OnceLock;

use kicad_symbols::Library;
use spice_resolve::{
    AlignSpec, Axis, ElementKind, ElementRole, PlaceSpec, Relation, ResolvedElement,
    ResolvedNetlist,
};

/// Workspace-relative path to the kicad-symbols fixture directory.
fn fixture_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent() // crates/
        .and_then(std::path::Path::parent) // workspace root
        .expect("workspace root")
        .join("crates/kicad-symbols/tests/fixtures")
}

pub fn fixture_library() -> &'static Library {
    static LIB: OnceLock<Library> = OnceLock::new();
    LIB.get_or_init(|| {
        let dir = fixture_dir();
        let device =
            Library::from_file(dir.join("Device.kicad_sym")).expect("load Device fixture library");
        let spice = Library::from_file(dir.join("Simulation_SPICE.kicad_sym"))
            .expect("load Simulation_SPICE fixture library");
        device.merge(spice)
    })
}

/// Build a `ResolvedElement` for a resistor `R<n>` bound to
/// `Device:R` in the fixture library.
pub fn make_r(refdes: &str) -> ResolvedElement {
    let lib = fixture_library();
    let symbol = lib.lookup("Device:R").expect("Device:R fixture").clone();
    ResolvedElement {
        refdes: refdes.to_owned(),
        kind: ElementKind::Resistor,
        lib_id: "Device:R".to_owned(),
        symbol,
        pin_mapping: vec!["1".into(), "2".into()],
        nodes: vec!["a".into(), "b".into()],
        value: None,
        role: ElementRole::Normal,
    }
}

pub fn mk_resolved(
    refdeses: &[&str],
    align: &[(Axis, &[&str])],
    place: &[(&str, Relation, &str)],
) -> ResolvedNetlist {
    ResolvedNetlist {
        elements: refdeses.iter().map(|r| make_r(r)).collect(),
        align: align
            .iter()
            .map(|(axis, refs)| AlignSpec {
                axis: *axis,
                refdes: refs.iter().map(|s| (*s).to_owned()).collect(),
                span: None,
            })
            .collect(),
        place: place
            .iter()
            .map(|(refdes, rel, anchor)| PlaceSpec {
                refdes: (*refdes).to_owned(),
                relation: *rel,
                anchor: (*anchor).to_owned(),
                span: None,
            })
            .collect(),
        subckts: vec![],
        sheet_instances: vec![],
    }
}
