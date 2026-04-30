//! Emit a KiCad schematic (`.kicad_sch`) from a [`Placement`].
//!
//! For each [`PlacedElement`] the emitter renders one `(symbol …)`
//! instance plus one `(global_label …)` per terminal at the pin's
//! world position. KiCad's connectivity engine nets pins together by
//! shared label name, so this produces a netlist-export-correct
//! schematic without needing a wire router. Wires, junctions and
//! aesthetic improvements are a later pass — this layer's contract is
//! purely "kicad-cli sch export netlist round-trips the topology".
//!
//! The schematic also carries a minimal `(lib_symbols)` block: every
//! `lib_id` referenced by a placed instance gets a stub entry that
//! lists pin numbers and positions, which is what kicad-cli needs to
//! resolve pin coordinates during netlist extraction.
//!
//! UUIDs are derived deterministically (uuid v5) from a fixed
//! namespace plus a per-item seed, so emitted output is stable across
//! runs and useful in golden tests.
//!
//! # Coordinate convention
//!
//! KiCad symbol-library pin coordinates are Y-up; KiCad schematic file
//! coordinates are Y-down. Placing a symbol at `(ox, oy)` therefore
//! renders a local pin at `(px, py)` at the world position
//! `(ox + px, oy − py)`. The label emitter applies that flip; the
//! placer's internal coordinates remain Y-up to match the rest of
//! `spice-layout`.

use std::collections::BTreeSet;

use crate::EmitError;
use crate::sexpr::Sexpr;
use kicad_symbols::{Library, Orientation, Rotation, Symbol, TransformedPin};
use spice_layout::{PlacedElement, Placement};
use uuid::Uuid;

const SCHEMA_VERSION: &str = "20231120";
const GENERATOR: &str = "spice2kicad";

/// Stable namespace for v5 UUIDs emitted by spice2kicad. Picked once
/// and frozen so two runs over the same input produce byte-identical
/// output.
const UUID_NAMESPACE: Uuid = Uuid::from_u128(0x7363_6932_6b69_6361_6432_6b69_6361_6431);

pub fn emit(placement: &Placement, library: &Library) -> Result<String, EmitError> {
    let mut items: Vec<Sexpr> = Vec::with_capacity(placement.elements.len() * 4 + 8);
    items.push(atom("kicad_sch"));
    items.push(list(vec![atom("version"), atom(SCHEMA_VERSION)]));
    items.push(list(vec![atom("generator"), qstring(GENERATOR)]));
    items.push(list(vec![atom("uuid"), qstring(&sheet_uuid())]));
    items.push(list(vec![atom("paper"), qstring("A4")]));
    items.push(lib_symbols(placement, library));

    for el in &placement.elements {
        items.push(symbol_instance(el));
        for label in pin_labels(el, library) {
            items.push(label);
        }
    }

    items.push(list(vec![
        atom("sheet_instances"),
        list(vec![
            atom("path"),
            qstring("/"),
            list(vec![atom("page"), qstring("1")]),
        ]),
    ]));

    Ok(Sexpr::List(items).to_pretty())
}

/// Emit a `(lib_symbols …)` block listing every `lib_id` referenced
/// by the placement. Each entry is a stub that contains only the
/// information kicad-cli needs to resolve pin geometry: pin numbers
/// and their local positions/angles. Symbols missing from `library`
/// are skipped silently — kicad-cli will then drop their nets, which
/// is the same outcome as a stub-less file. Future work: surface a
/// diagnostic when this happens.
fn lib_symbols(placement: &Placement, library: &Library) -> Sexpr {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut entries: Vec<Sexpr> = vec![atom("lib_symbols")];
    for el in &placement.elements {
        if !seen.insert(el.lib_id.as_str()) {
            continue;
        }
        if let Some(symbol) = library.lookup(&el.lib_id) {
            entries.push(lib_symbol_stub(symbol));
        }
    }
    Sexpr::List(entries)
}

fn lib_symbol_stub(symbol: &Symbol) -> Sexpr {
    let mut pin_unit: Vec<Sexpr> = Vec::with_capacity(symbol.pins.len() + 2);
    pin_unit.push(atom("symbol"));
    pin_unit.push(qstring(&format!("{}_1_1", symbol.name)));
    for pin in &symbol.pins {
        pin_unit.push(pin_def(pin));
    }
    Sexpr::List(vec![
        atom("symbol"),
        qstring(&symbol.lib_id),
        list(vec![atom("exclude_from_sim"), atom("no")]),
        list(vec![atom("in_bom"), atom("yes")]),
        list(vec![atom("on_board"), atom("yes")]),
        property_field("Reference", "U", 0.0, 0.0, true),
        property_field("Value", &symbol.name, 0.0, 0.0, false),
        property_field("Footprint", "", 0.0, 0.0, true),
        property_field("Datasheet", "", 0.0, 0.0, true),
        Sexpr::List(pin_unit),
    ])
}

fn property_field(name: &str, value: &str, x: f64, y: f64, hidden: bool) -> Sexpr {
    let mut items = vec![
        atom("property"),
        qstring(name),
        qstring(value),
        list(vec![
            atom("at"),
            atom(&format_coord(x)),
            atom(&format_coord(y)),
            atom("0"),
        ]),
    ];
    if hidden {
        items.push(list(vec![
            atom("effects"),
            list(vec![
                atom("font"),
                list(vec![atom("size"), atom("1.27"), atom("1.27")]),
            ]),
            list(vec![atom("hide"), atom("yes")]),
        ]));
    } else {
        items.push(list(vec![
            atom("effects"),
            list(vec![
                atom("font"),
                list(vec![atom("size"), atom("1.27"), atom("1.27")]),
            ]),
        ]));
    }
    list(items)
}

fn pin_def(pin: &kicad_symbols::Pin) -> Sexpr {
    list(vec![
        atom("pin"),
        atom("passive"),
        atom("line"),
        list(vec![
            atom("at"),
            atom(&format_coord(pin.x)),
            atom(&format_coord(pin.y)),
            atom(&pin.angle.to_string()),
        ]),
        list(vec![atom("length"), atom(&format_coord(0.0))]),
        list(vec![
            atom("name"),
            qstring(&pin.name),
            list(vec![
                atom("effects"),
                list(vec![
                    atom("font"),
                    list(vec![atom("size"), atom("1.27"), atom("1.27")]),
                ]),
            ]),
        ]),
        list(vec![
            atom("number"),
            qstring(&pin.number),
            list(vec![
                atom("effects"),
                list(vec![
                    atom("font"),
                    list(vec![atom("size"), atom("1.27"), atom("1.27")]),
                ]),
            ]),
        ]),
    ])
}

fn symbol_instance(el: &PlacedElement) -> Sexpr {
    let (x_mm, y_mm) = el.origin.to_mm();
    let angle = rotation_degrees(el.orientation);
    let mirror = mirror_token(el.orientation);

    let mut fields = vec![
        atom("symbol"),
        list(vec![atom("lib_id"), qstring(&el.lib_id)]),
        list(vec![
            atom("at"),
            atom(&format_coord(x_mm)),
            atom(&format_coord(y_mm)),
            atom(&angle.to_string()),
        ]),
        list(vec![atom("unit"), atom("1")]),
    ];
    if let Some(m) = mirror {
        fields.push(list(vec![atom("mirror"), atom(m)]));
    }
    fields.push(list(vec![atom("uuid"), qstring(&instance_uuid(el))]));
    fields.push(reference_property(&el.refdes, x_mm, y_mm));
    let value_text = el.value.as_deref().unwrap_or(&el.refdes);
    fields.push(value_property(value_text, x_mm, y_mm));
    for prop in sim_properties(&el.lib_id, value_text) {
        fields.push(prop);
    }
    fields.push(instances_block(&el.refdes));
    Sexpr::List(fields)
}

/// Emit the per-instance `Sim.*` properties needed by kicad-cli's
/// SPICE netlister for active devices. Two-terminal passives (R, C,
/// L, D, V, I) are recognised by kicad-cli from the refdes prefix
/// alone and need no annotation. Active devices (Q, M, J) are emitted
/// as `__Q1`-style placeholders unless `Sim.Device` and `Sim.Type`
/// are set, so we add minimal stubs derived from the symbol family.
///
/// `Sim.Pins` is intentionally omitted: the default model-pin ↔
/// symbol-pin mapping treats `model_pin[i] = symbol_pin[i]` (see
/// `SIM_MODEL::createPins` in the KiCad source), which combined with
/// the SPICE-order pin numbering used by `spice-resolve` produces
/// the right SPICE terminal order on the round-trip.
fn sim_properties(lib_id: &str, value: &str) -> Vec<Sexpr> {
    // Strip the `Lib:` prefix.
    let bare = lib_id.split_once(':').map_or(lib_id, |(_, name)| name);
    let (device, sim_type) = if bare.starts_with("Q_NPN") {
        ("NPN", "GUMMELPOON")
    } else if bare.starts_with("Q_PNP") {
        ("PNP", "GUMMELPOON")
    } else if bare.starts_with("Q_NMOS") {
        ("NMOS", "MOS1")
    } else if bare.starts_with("Q_PMOS") {
        ("PMOS", "MOS1")
    } else if bare.starts_with("Q_NJFET") {
        ("NJFET", "SHICHMANHODGES")
    } else if bare.starts_with("Q_PJFET") {
        ("PJFET", "SHICHMANHODGES")
    } else {
        return Vec::new();
    };
    vec![
        sim_property("Sim.Device", device),
        sim_property("Sim.Type", sim_type),
        sim_property("Sim.Name", value),
    ]
}

fn sim_property(name: &str, value: &str) -> Sexpr {
    list(vec![
        atom("property"),
        qstring(name),
        qstring(value),
        list(vec![atom("at"), atom("0"), atom("0"), atom("0")]),
        list(vec![
            atom("effects"),
            list(vec![
                atom("font"),
                list(vec![atom("size"), atom("1.27"), atom("1.27")]),
            ]),
            list(vec![atom("hide"), atom("yes")]),
        ]),
    ])
}

/// Per-symbol `(instances …)` block. kicad-cli refuses to emit a
/// netlist entry for a symbol whose instance reference isn't recorded
/// here — even on a flat single-sheet design.
fn instances_block(refdes: &str) -> Sexpr {
    list(vec![
        atom("instances"),
        list(vec![
            atom("project"),
            qstring(GENERATOR),
            list(vec![
                atom("path"),
                qstring(&format!("/{}", sheet_uuid())),
                list(vec![atom("reference"), qstring(refdes)]),
                list(vec![atom("unit"), atom("1")]),
            ]),
        ]),
    ])
}

/// Emit a `(global_label "<net>" …)` per terminal of `el`, anchored
/// at the pin's world coordinates. Global labels (rather than local
/// `label`) are used so that ground (`"0"`) and other shared nets
/// retain their bare name in the exported netlist instead of being
/// prefixed with the sheet path. Terminals whose pin number isn't in
/// the library symbol are skipped silently — that means the symbol
/// resolution upstream is inconsistent, but the emitter has no good
/// way to report it here.
fn pin_labels(el: &PlacedElement, library: &Library) -> Vec<Sexpr> {
    let Some(symbol) = library.lookup(&el.lib_id) else {
        return Vec::new();
    };
    let pins = symbol.pins_in(el.orientation);
    let (ox, oy) = el.origin.to_mm();
    let mut out = Vec::with_capacity(el.nodes.len());
    for (term_index, (node, kicad_pin)) in el.nodes.iter().zip(el.pin_mapping.iter()).enumerate() {
        let Some(pin) = pins.iter().find(|p| &p.number == kicad_pin) else {
            continue;
        };
        // Symbol-local frame is Y-up; schematic file frame is Y-down.
        let wx = ox + pin.x;
        let wy = oy - pin.y;
        out.push(global_label(node, wx, wy, pin, el, term_index));
    }
    out
}

fn global_label(
    text: &str,
    x: f64,
    y: f64,
    pin: &TransformedPin,
    el: &PlacedElement,
    term_index: usize,
) -> Sexpr {
    // The label's text-rotation angle should match the pin's outward
    // direction so the label reads away from the symbol body. KiCad
    // accepts only 0 / 90 / 180 / 270 here.
    let angle = pin.angle;
    list(vec![
        atom("global_label"),
        qstring(text),
        list(vec![atom("shape"), atom("input")]),
        list(vec![
            atom("at"),
            atom(&format_coord(x)),
            atom(&format_coord(y)),
            atom(&angle.to_string()),
        ]),
        list(vec![
            atom("effects"),
            list(vec![
                atom("font"),
                list(vec![atom("size"), atom("1.27"), atom("1.27")]),
            ]),
        ]),
        list(vec![atom("uuid"), qstring(&label_uuid(el, term_index))]),
    ])
}

fn rotation_degrees(orient: Orientation) -> u16 {
    match orient.rotation {
        Rotation::R0 => 0,
        Rotation::R90 => 90,
        Rotation::R180 => 180,
        Rotation::R270 => 270,
    }
}

fn mirror_token(orient: Orientation) -> Option<&'static str> {
    if orient.mirror_y { Some("y") } else { None }
}

fn reference_property(refdes: &str, x: f64, y: f64) -> Sexpr {
    list(vec![
        atom("property"),
        qstring("Reference"),
        qstring(refdes),
        list(vec![
            atom("at"),
            atom(&format_coord(x)),
            atom(&format_coord(y)),
            atom("0"),
        ]),
    ])
}

fn value_property(value: &str, x: f64, y: f64) -> Sexpr {
    list(vec![
        atom("property"),
        qstring("Value"),
        qstring(value),
        list(vec![
            atom("at"),
            atom(&format_coord(x)),
            atom(&format_coord(y)),
            atom("0"),
        ]),
    ])
}

fn sheet_uuid() -> String {
    Uuid::new_v5(&UUID_NAMESPACE, b"sheet:root").to_string()
}

fn instance_uuid(el: &PlacedElement) -> String {
    let seed = format!("symbol:{}:{}", el.lib_id, el.refdes);
    Uuid::new_v5(&UUID_NAMESPACE, seed.as_bytes()).to_string()
}

fn label_uuid(el: &PlacedElement, term_index: usize) -> String {
    let seed = format!("label:{}:{}:{}", el.lib_id, el.refdes, term_index);
    Uuid::new_v5(&UUID_NAMESPACE, seed.as_bytes()).to_string()
}

fn format_coord(v: f64) -> String {
    let rounded = (v * 1_000_000.0).round() / 1_000_000.0;
    if rounded == 0.0 {
        return "0".to_string();
    }
    let s = format!("{rounded}");
    if s.contains('.') { s } else { format!("{s}.0") }
}

fn atom(s: &str) -> Sexpr {
    Sexpr::Atom(s.to_string())
}

fn qstring(s: &str) -> Sexpr {
    Sexpr::QString(s.to_string())
}

fn list(items: Vec<Sexpr>) -> Sexpr {
    Sexpr::List(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kicad_symbols::Library;
    use spice_layout::{GridPoint, PlacedElement, Placement};

    fn fixture_library() -> Library {
        // The unit tests below don't exercise the label-emission path;
        // an empty Library is enough for the placed-symbol checks.
        // Tests that require resolved pin geometry live in
        // `tests/roundtrip.rs` (which round-trips through kicad-cli).
        Library::default()
    }

    fn one_resistor_at_origin() -> Placement {
        Placement {
            elements: vec![PlacedElement {
                refdes: "R1".to_string(),
                lib_id: "Device:R".to_string(),
                origin: GridPoint::new(0, 0),
                orientation: Orientation::IDENTITY,
                nodes: Vec::new(),
                pin_mapping: Vec::new(),
                value: None,
            }],
        }
    }

    #[test]
    fn emits_lib_id_and_origin_for_single_resistor() {
        let placement = one_resistor_at_origin();
        let library = fixture_library();
        let out = emit(&placement, &library).expect("emit");
        assert!(
            out.contains("(lib_id \"Device:R\")"),
            "missing lib_id in output:\n{out}"
        );
        assert!(
            out.contains("(at 0 0 0)"),
            "missing origin (at 0 0 0) in output:\n{out}"
        );
        assert!(out.contains("(kicad_sch"));
        assert!(out.contains("(sheet_instances"));
    }

    #[test]
    fn emits_two_symbols_with_distinct_uuids() {
        let placement = Placement {
            elements: vec![
                PlacedElement {
                    refdes: "R1".into(),
                    lib_id: "Device:R".into(),
                    origin: GridPoint::new(0, 0),
                    orientation: Orientation::IDENTITY,
                    nodes: Vec::new(),
                    pin_mapping: Vec::new(),
                    value: None,
                },
                PlacedElement {
                    refdes: "R2".into(),
                    lib_id: "Device:R".into(),
                    origin: GridPoint::new(10, 0),
                    orientation: Orientation::IDENTITY,
                    nodes: Vec::new(),
                    pin_mapping: Vec::new(),
                    value: None,
                },
            ],
        };
        let library = fixture_library();
        let out = emit(&placement, &library).expect("emit");
        let r1_uuid = instance_uuid(&placement.elements[0]);
        let r2_uuid = instance_uuid(&placement.elements[1]);
        assert_ne!(r1_uuid, r2_uuid);
        assert!(out.contains(&r1_uuid));
        assert!(out.contains(&r2_uuid));
    }

    #[test]
    fn rotation_is_emitted_in_degrees() {
        let placement = Placement {
            elements: vec![PlacedElement {
                refdes: "R1".into(),
                lib_id: "Device:R".into(),
                origin: GridPoint::new(2, 4),
                orientation: Orientation {
                    rotation: Rotation::R90,
                    mirror_y: false,
                },
                nodes: Vec::new(),
                pin_mapping: Vec::new(),
                value: None,
            }],
        };
        let out = emit(&placement, &fixture_library()).expect("emit");
        // 2 grid * 1.27mm = 2.54, 4 * 1.27 = 5.08
        assert!(out.contains("(at 2.54 5.08 90)"), "got:\n{out}");
    }

    #[test]
    fn mirror_y_emits_mirror_token() {
        let placement = Placement {
            elements: vec![PlacedElement {
                refdes: "R1".into(),
                lib_id: "Device:R".into(),
                origin: GridPoint::new(0, 0),
                orientation: Orientation {
                    rotation: Rotation::R0,
                    mirror_y: true,
                },
                nodes: Vec::new(),
                pin_mapping: Vec::new(),
                value: None,
            }],
        };
        let out = emit(&placement, &fixture_library()).expect("emit");
        assert!(out.contains("(mirror y)"), "got:\n{out}");
    }
}
