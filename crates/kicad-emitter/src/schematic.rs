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
use kicad_symbols::{Library, Orientation, RawSexpr, Rotation, Symbol, TransformedPin};
use spice_layout::{PlacedElement, Placement};
use uuid::Uuid;

const SCHEMA_VERSION: &str = "20231120";
const GENERATOR: &str = "spice2kicad";

/// Stable namespace for v5 UUIDs emitted by spice2kicad. Picked once
/// and frozen so two runs over the same input produce byte-identical
/// output.
const UUID_NAMESPACE: Uuid = Uuid::from_u128(0x7363_6932_6b69_6361_6432_6b69_6361_6431);

pub fn emit(placement: &Placement, library: &Library) -> Result<String, EmitError> {
    emit_root(placement, library, &[])
}

/// One top-level `X<n>` SPICE instance lowered to a KiCad hierarchical
/// sheet on the parent schematic.
#[derive(Debug, Clone)]
pub struct SheetBlock {
    /// The instance refdes (e.g. `"X1"`).
    pub refdes: String,
    /// Child sheet filename, relative to the parent (e.g.
    /// `"OPAMP.kicad_sch"`).
    pub sheet_file: String,
    /// Port name → SPICE net name on the parent. Order matches the
    /// child sheet's port list.
    pub ports: Vec<SheetPort>,
}

/// One port of a [`SheetBlock`] — the port name visible on the sheet
/// symbol plus the parent-scope net it connects to.
#[derive(Debug, Clone)]
pub struct SheetPort {
    pub name: String,
    pub net: String,
}

/// A child schematic's body plus its port list. Used by
/// [`emit_child_sheet`].
#[derive(Debug, Clone)]
pub struct ChildSheet<'a> {
    pub name: String,
    pub placement: &'a Placement,
    pub ports: Vec<String>,
    /// Refdeses of every parent-level instance pointing at this child
    /// sheet file. Each one becomes a `(path …)` entry in the child's
    /// symbol-instance blocks so kicad-cli can resolve refdes
    /// annotations during netlist export.
    pub instance_refdeses: Vec<String>,
}

/// Emit a top-level (root) schematic. Same as [`emit`] but additionally
/// embeds a `(sheet …)` block for each entry in `sheets`.
pub fn emit_root(
    placement: &Placement,
    library: &Library,
    sheets: &[SheetBlock],
) -> Result<String, EmitError> {
    let mut items: Vec<Sexpr> = Vec::with_capacity(placement.elements.len() * 4 + sheets.len() + 8);
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

    // Hierarchical-sheet instances. Each block lives at a unique
    // location on the parent canvas; pin coordinates are derived from
    // the block's origin.
    for (idx, block) in sheets.iter().enumerate() {
        let (sheet_node, pin_labels) = sheet_block(block, idx);
        items.push(sheet_node);
        for label in pin_labels {
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

/// Emit a hierarchical-sheet child schematic. The child carries a
/// `(hierarchical_label …)` per port at the same world-coordinate as
/// a body-element pin connected to the same SPICE net (so the port and
/// the body net resolve to one connectivity class).
pub fn emit_child_sheet(child: &ChildSheet<'_>, library: &Library) -> Result<String, EmitError> {
    let mut items: Vec<Sexpr> = vec![
        atom("kicad_sch"),
        list(vec![atom("version"), atom(SCHEMA_VERSION)]),
        list(vec![atom("generator"), qstring(GENERATOR)]),
        list(vec![atom("uuid"), qstring(&child_uuid(&child.name))]),
        list(vec![atom("paper"), qstring("A4")]),
        lib_symbols(child.placement, library),
    ];

    // Place hierarchical labels off to the left of the body, on grid,
    // one row per port. The exact location is irrelevant for
    // connectivity (the labels share their net via name with the body
    // global_label of the same port-name), but distinct positions stop
    // KiCad from collapsing them into one symbol.
    for (i, port) in child.ports.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let y = -(i as f64) * 5.08;
        items.push(hierarchical_label(port, -25.4, y));
        // A co-located global label so the port name is part of the
        // child's connectivity graph even when no body element happens
        // to use that exact net name (e.g. an unused port).
        items.push(global_label_simple(port, -25.4, y, &child.name, i));
    }

    for el in &child.placement.elements {
        items.push(child_symbol_instance(el, &child.instance_refdeses));
        for label in pin_labels(el, library) {
            items.push(label);
        }
    }

    // Child-sheet-instances: one path entry per parent instance,
    // rooted at the parent sheet uuid + the per-instance sheet uuid.
    let mut sheet_instances_items = vec![atom("sheet_instances")];
    for refdes in &child.instance_refdeses {
        sheet_instances_items.push(list(vec![
            atom("path"),
            qstring(&format!("/{}/{}", sheet_uuid(), child_sheet_uuid(refdes))),
            list(vec![atom("page"), qstring("2")]),
        ]));
    }
    if child.instance_refdeses.is_empty() {
        sheet_instances_items.push(list(vec![
            atom("path"),
            qstring("/"),
            list(vec![atom("page"), qstring("2")]),
        ]));
    }
    items.push(Sexpr::List(sheet_instances_items));

    Ok(Sexpr::List(items).to_pretty())
}

/// Render a `(sheet …)` block plus the `(global_label …)` pieces that
/// pin its port symbols to the parent net coordinates.
fn sheet_block(block: &SheetBlock, idx: usize) -> (Sexpr, Vec<Sexpr>) {
    // Lay out sheets one above the next, leftmost column. Coordinates
    // are arbitrary; KiCad's connectivity engine matches by sheet pin
    // name + colocated label, not by geometry.
    #[allow(clippy::cast_precision_loss)]
    let origin_x: f64 = 200.0;
    #[allow(clippy::cast_precision_loss)]
    let origin_y: f64 = 50.0 + (idx as f64) * 60.0;
    let pin_count = block.ports.len();
    #[allow(clippy::cast_precision_loss)]
    let height = (pin_count as f64).max(2.0) * 5.08 + 5.08;

    let mut sheet_items: Vec<Sexpr> = vec![
        atom("sheet"),
        list(vec![
            atom("at"),
            atom(&format_coord(origin_x)),
            atom(&format_coord(origin_y)),
        ]),
        list(vec![
            atom("size"),
            atom(&format_coord(30.48)),
            atom(&format_coord(height)),
        ]),
        list(vec![
            atom("uuid"),
            qstring(&child_sheet_uuid(&block.refdes)),
        ]),
        // Sheetname carries the SPICE refdes so the test wrapper sees X1.
        sheet_property("Sheetname", &block.refdes, origin_x, origin_y - 1.0),
        sheet_property(
            "Sheetfile",
            &block.sheet_file,
            origin_x,
            origin_y + height + 1.0,
        ),
    ];

    // One pin per port, plus a co-located global_label so the parent's
    // SPICE net joins the sheet pin.
    let mut pin_labels: Vec<Sexpr> = Vec::with_capacity(pin_count);
    for (i, port) in block.ports.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let py = origin_y + 5.08 + (i as f64) * 5.08;
        let px = origin_x; // left edge
        let pin_uuid = Uuid::new_v5(
            &UUID_NAMESPACE,
            format!("sheetpin:{}:{}", block.refdes, port.name).as_bytes(),
        )
        .to_string();
        sheet_items.push(list(vec![
            atom("pin"),
            qstring(&port.name),
            atom("input"),
            list(vec![
                atom("at"),
                atom(&format_coord(px)),
                atom(&format_coord(py)),
                atom("180"),
            ]),
            list(vec![atom("uuid"), qstring(&pin_uuid)]),
            list(vec![
                atom("effects"),
                list(vec![
                    atom("font"),
                    list(vec![atom("size"), atom("1.27"), atom("1.27")]),
                ]),
                list(vec![atom("justify"), atom("left")]),
            ]),
        ]));
        pin_labels.push(global_label_simple(&port.net, px, py, &block.refdes, i));
    }

    sheet_items.push(list(vec![
        atom("instances"),
        list(vec![
            atom("project"),
            qstring(GENERATOR),
            list(vec![
                atom("path"),
                qstring(&format!("/{}", sheet_uuid())),
                list(vec![atom("page"), qstring("2")]),
            ]),
        ]),
    ]));

    (Sexpr::List(sheet_items), pin_labels)
}

fn sheet_property(name: &str, value: &str, x: f64, y: f64) -> Sexpr {
    list(vec![
        atom("property"),
        qstring(name),
        qstring(value),
        list(vec![
            atom("at"),
            atom(&format_coord(x)),
            atom(&format_coord(y)),
            atom("0"),
        ]),
        list(vec![
            atom("effects"),
            list(vec![
                atom("font"),
                list(vec![atom("size"), atom("1.27"), atom("1.27")]),
            ]),
        ]),
    ])
}

fn hierarchical_label(text: &str, x: f64, y: f64) -> Sexpr {
    let uuid =
        Uuid::new_v5(&UUID_NAMESPACE, format!("hlabel:{text}:{x}:{y}").as_bytes()).to_string();
    list(vec![
        atom("hierarchical_label"),
        qstring(text),
        list(vec![atom("shape"), atom("input")]),
        list(vec![
            atom("at"),
            atom(&format_coord(x)),
            atom(&format_coord(y)),
            atom("0"),
        ]),
        list(vec![
            atom("effects"),
            list(vec![
                atom("font"),
                list(vec![atom("size"), atom("1.27"), atom("1.27")]),
            ]),
        ]),
        list(vec![atom("uuid"), qstring(&uuid)]),
    ])
}

fn global_label_simple(text: &str, x: f64, y: f64, scope: &str, idx: usize) -> Sexpr {
    let uuid = Uuid::new_v5(
        &UUID_NAMESPACE,
        format!("glabel:{scope}:{idx}:{text}").as_bytes(),
    )
    .to_string();
    list(vec![
        atom("global_label"),
        qstring(text),
        list(vec![atom("shape"), atom("input")]),
        list(vec![
            atom("at"),
            atom(&format_coord(x)),
            atom(&format_coord(y)),
            atom("0"),
        ]),
        list(vec![
            atom("effects"),
            list(vec![
                atom("font"),
                list(vec![atom("size"), atom("1.27"), atom("1.27")]),
            ]),
        ]),
        list(vec![atom("uuid"), qstring(&uuid)]),
    ])
}

fn child_sheet_uuid(refdes: &str) -> String {
    Uuid::new_v5(
        &UUID_NAMESPACE,
        format!("sheet-instance:{refdes}").as_bytes(),
    )
    .to_string()
}

fn child_uuid(subckt_name: &str) -> String {
    Uuid::new_v5(&UUID_NAMESPACE, format!("sheet:{subckt_name}").as_bytes()).to_string()
}

/// Per-symbol `(instances …)` block for a symbol that lives on a child
/// hierarchical sheet rather than the root. The path is
/// `/<root>/<sheet-instance>` and the reference is the body element's
/// refdes. One `(path …)` entry per parent instance pointing at this
/// sheet file (typically just one).
fn child_instances_block(refdes: &str, instance_refdeses: &[String]) -> Sexpr {
    let mut project = vec![atom("project"), qstring(GENERATOR)];
    if instance_refdeses.is_empty() {
        // Standalone child (no parent instance) — fall back to a
        // single-path block so kicad-cli has something to resolve.
        project.push(list(vec![
            atom("path"),
            qstring("/"),
            list(vec![atom("reference"), qstring(refdes)]),
            list(vec![atom("unit"), atom("1")]),
        ]));
    } else {
        for instance_refdes in instance_refdeses {
            project.push(list(vec![
                atom("path"),
                qstring(&format!(
                    "/{}/{}",
                    sheet_uuid(),
                    child_sheet_uuid(instance_refdes)
                )),
                list(vec![atom("reference"), qstring(refdes)]),
                list(vec![atom("unit"), atom("1")]),
            ]));
        }
    }
    list(vec![atom("instances"), Sexpr::List(project)])
}

fn child_symbol_instance(el: &PlacedElement, instance_refdeses: &[String]) -> Sexpr {
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
    fields.push(child_instances_block(&el.refdes, instance_refdeses));
    Sexpr::List(fields)
}

/// Emit a `(lib_symbols …)` block listing every `lib_id` referenced
/// by the placement.
///
/// Each entry is the raw `(symbol …)` body captured at library-parse
/// time (see [`kicad_symbols::Symbol::body`]) — copied verbatim, with
/// the bare symbol name in slot `[1]` rewritten to the full `Lib:Name`
/// form KiCad expects in instance-side `lib_id` references. This
/// preserves the source library's graphical primitives (rectangles,
/// polylines, etc.) and pin lengths, fulfilling V1 and V3 from
/// CLAUDE.md's Visual quality invariants.
///
/// Symbols missing from `library` are skipped silently — upstream
/// resolution (E003) is responsible for catching that case before the
/// emitter ever sees it.
fn lib_symbols(placement: &Placement, library: &Library) -> Sexpr {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut entries: Vec<Sexpr> = vec![atom("lib_symbols")];
    for el in &placement.elements {
        if !seen.insert(el.lib_id.as_str()) {
            continue;
        }
        if let Some(symbol) = library.lookup(&el.lib_id) {
            entries.push(lib_symbol_inline(symbol));
        }
    }
    Sexpr::List(entries)
}

/// Render a `Symbol` as a verbatim `(symbol …)` block.
///
/// The captured body has the structure
/// `(symbol "<bare>" …)`; KiCad requires the slot-1 name on the
/// library entry to match the `lib_id` referenced by instances, so we
/// rewrite that one slot before emitting. Everything else (graphics,
/// nested unit symbols, pins-with-length, properties) is forwarded
/// untouched.
///
/// TODO: a body that uses `(extends "Base")` is forwarded as-is. The
/// referenced base symbol is *not* automatically pulled in, so KiCad
/// may render incomplete graphics. Detect this and emit a diagnostic
/// when extended-symbol support lands.
fn lib_symbol_inline(symbol: &Symbol) -> Sexpr {
    let mut sx = Sexpr::from(symbol.body.clone());
    if let Sexpr::List(items) = &mut sx {
        if items.len() >= 2 {
            items[1] = qstring(&symbol.lib_id);
        }
    }
    sx
}

impl From<RawSexpr> for Sexpr {
    fn from(r: RawSexpr) -> Self {
        match r {
            RawSexpr::Atom(s) => Sexpr::Atom(s),
            RawSexpr::QString(s) => Sexpr::QString(s),
            RawSexpr::List(items) => Sexpr::List(items.into_iter().map(Sexpr::from).collect()),
        }
    }
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
    } else if bare == "ESOURCE" {
        // Voltage-controlled voltage source. KiCad's TYPE::V_VCL has
        // empty `Sim.Type`, so we emit an empty subtype field — that
        // empty-vs-empty match is enough for the SPICE exporter to
        // recognise the device. The gain rides in `Sim.Params` as
        // `gain=<value>` per
        // `eeschema/sim/sim_model_source.cpp:makeVcParamInfos`.
        return vec![
            sim_property("Sim.Device", "E"),
            sim_property("Sim.Type", ""),
            sim_property("Sim.Params", &format!("gain={value}")),
        ];
    } else if bare == "GSOURCE" {
        return vec![
            sim_property("Sim.Device", "G"),
            sim_property("Sim.Type", ""),
            sim_property("Sim.Params", &format!("gain={value}")),
        ];
    } else if bare == "FSOURCE" {
        return vec![
            sim_property("Sim.Device", "F"),
            sim_property("Sim.Type", ""),
            sim_property("Sim.Params", &format!("gain={value}")),
        ];
    } else if bare == "HSOURCE" {
        return vec![
            sim_property("Sim.Device", "H"),
            sim_property("Sim.Type", ""),
            sim_property("Sim.Params", &format!("gain={value}")),
        ];
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
