//! Emit a KiCad schematic (`.kicad_sch`) from a [`Placement`].
//!
//! For each [`PlacedElement`] the emitter renders one `(symbol …)`
//! instance. Connectivity between pins on the same SPICE net is
//! expressed via orthogonal `(wire …)` segments emitted by a
//! Manhattan dog-leg router (KISS approach: chain pins sorted by
//! `(x, y)`, connecting consecutive pairs with an L-shape).
//! `(junction …)` is dropped at any T-intersection (3+ wire endpoints
//! coincident) so KiCad sees a single connectivity class.
//!
//! Per-pin `(global_label …)` for internal connectivity is *not*
//! emitted — that would violate V4 (≤ 2 labels per net per sheet).
//! Labels remain only at hierarchical-sheet boundaries (parent-side
//! sheet pins and child-side hierarchical port labels), each at most
//! once per net per sheet.
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
use kicad_symbols::{Library, Orientation, RawSexpr, Rotation, Symbol};
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
    }

    // Hierarchical-sheet instances. Each block lives at a unique
    // location on the parent canvas; pin coordinates are derived from
    // the block's origin.
    let mut extra_pins: Vec<(String, f64, f64)> = Vec::new();
    for (idx, block) in sheets.iter().enumerate() {
        let (sheet_node, pin_labels, sheet_pin_pos) = sheet_block(block, idx);
        items.push(sheet_node);
        for label in pin_labels {
            items.push(label);
        }
        // Sheet pin positions become extra "pins" on the parent net so
        // wire routing connects body pins to the sheet block.
        extra_pins.extend(sheet_pin_pos);
    }

    let net_pins = collect_net_pins(placement, library, &extra_pins);
    for routed in route_nets(&net_pins, "root") {
        items.push(routed);
    }
    for label in dangling_pin_labels(&net_pins, "root") {
        items.push(label);
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

    // Determine which subckt ports are actually consumed by a body
    // element. A port is "used" if any body element has a node whose
    // name matches the port name — in that case the body's
    // pin-emitted global_label of the same name carries the
    // connectivity, and a colocated global_label by the hierarchical
    // label keeps the port-side endpoint on the same net. An unused
    // port (e.g. a power rail wired straight through the sheet)
    // would otherwise leave the hierarchical_label dangling, so we
    // attach a `(no_connect …)` to mark the non-connection
    // deliberate and keep ERC clean.
    let used_ports: BTreeSet<&str> = child
        .placement
        .elements
        .iter()
        .flat_map(|el| el.nodes.iter().map(String::as_str))
        .collect();

    // Place hierarchical labels off to the left of the body, on grid,
    // one row per port. Distinct positions stop KiCad from collapsing
    // them into one symbol.
    let mut extra_pins: Vec<(String, f64, f64)> = Vec::new();
    for (i, port) in child.ports.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let y = -(i as f64) * 5.08;
        items.push(hierarchical_label(port, -25.4, y));
        if used_ports.contains(port.as_str()) {
            // The hierarchical label position becomes an extra pin on
            // the port net so the wire router connects it to the
            // body's pins on that same net.
            extra_pins.push((port.clone(), -25.4, y));
        } else {
            // Port is exposed by the parent but unused by the body.
            // Mark the hierarchical_label endpoint as a deliberate
            // no-connect so ERC doesn't flag it as dangling.
            items.push(no_connect(-25.4, y, &child.name, i));
        }
    }

    for el in &child.placement.elements {
        items.push(child_symbol_instance(el, &child.instance_refdeses));
    }

    let net_pins = collect_net_pins(child.placement, library, &extra_pins);
    for routed in route_nets(&net_pins, &child.name) {
        items.push(routed);
    }
    for label in dangling_pin_labels(&net_pins, &child.name) {
        items.push(label);
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
fn sheet_block(block: &SheetBlock, idx: usize) -> (Sexpr, Vec<Sexpr>, Vec<(String, f64, f64)>) {
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
    let mut pin_positions: Vec<(String, f64, f64)> = Vec::with_capacity(pin_count);
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
        // Note: the sheet pin's connectivity to the parent net is
        // expressed via wires from `pin_positions` (collected
        // below). No colocated global_label is emitted — that would
        // bring the per-net label count above the V4 budget when
        // combined with dangling_pin_labels' two-marker policy.
        let _ = (i, &mut pin_labels);
        pin_positions.push((port.net.clone(), px, py));
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

    (Sexpr::List(sheet_items), pin_labels, pin_positions)
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

fn no_connect(x: f64, y: f64, scope: &str, idx: usize) -> Sexpr {
    let uuid = Uuid::new_v5(&UUID_NAMESPACE, format!("nc:{scope}:{idx}").as_bytes()).to_string();
    list(vec![
        atom("no_connect"),
        list(vec![
            atom("at"),
            atom(&format_coord(x)),
            atom(&format_coord(y)),
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

/// World-space pin info: `(net, x, y, angle_deg)`. Angle is the pin's
/// outward direction in `.kicad_sym` (Y-up) convention, after the
/// placement orientation has been applied.
type PinPos = (String, f64, f64, u16);

/// Collect the world-space pin positions per SPICE net for a
/// `Placement` plus any `extra_pins` (hierarchical port labels or
/// sheet-block pin coordinates). Each entry includes the pin's
/// outward angle so the router can pick a non-colliding escape
/// direction; `extra_pins` are given a default angle of 0
/// (right-pointing) since they sit at hierarchical-label positions
/// where the label itself extends rightward.
fn collect_net_pins(
    placement: &Placement,
    library: &Library,
    extra_pins: &[(String, f64, f64)],
) -> std::collections::BTreeMap<String, Vec<(f64, f64, u16)>> {
    let mut nets: std::collections::BTreeMap<String, Vec<(f64, f64, u16)>> =
        std::collections::BTreeMap::new();
    for el in &placement.elements {
        let Some(symbol) = library.lookup(&el.lib_id) else {
            continue;
        };
        let pins = symbol.pins_in(el.orientation);
        let (ox, oy) = el.origin.to_mm();
        for (node, kicad_pin) in el.nodes.iter().zip(el.pin_mapping.iter()) {
            let Some(pin) = pins.iter().find(|p| &p.number == kicad_pin) else {
                continue;
            };
            // KiCad's .kicad_sym parser negates pin Y on load
            // (`parseXY(true)` in eeschema/sch_io_kicad_sexpr_parser.h),
            // and applies an identity transform plus the symbol
            // origin to get the world position. Net result: the
            // schematic-file world Y is `symbol_origin_y - file_pin_y`.
            let wx = ox + pin.x;
            let wy = oy - pin.y;
            nets.entry(node.clone())
                .or_default()
                .push((wx, wy, pin.angle));
        }
    }
    for (net, x, y) in extra_pins {
        nets.entry(net.clone()).or_default().push((*x, *y, 0));
    }
    let _ = std::marker::PhantomData::<PinPos>;
    nets
}

/// Route every net with ≥ 2 pin positions.
///
/// Strategy: **per-pin escape + per-net trunk**. Every pin in the
/// design is given a globally-unique escape distance `esc_p` (one
/// grid step per pin, in scan order). From pin `(px, py)`:
///
///   1. Vertical escape `(px, py) → (px, py + esc_p)` — moves the
///      route off the pin's own Y row by a per-pin amount, so two
///      pins on different nets that share `(px, *)` (like a
///      resistor's two terminals) emit non-overlapping verticals.
///   2. Horizontal lead-in `(px, py + esc_p) → (trunk_x[N], py + esc_p)`
///      at a Y unique per pin, so no two horizontals share a Y row.
///   3. Vertical down to the trunk `(trunk_x[N], py + esc_p) → (trunk_x[N], trunk_y[N])`.
///
/// Trunks live in a per-net column-and-row pair `(trunk_x[N], trunk_y[N])`
/// far below and to the right of the placement. A single horizontal
/// trunk joins the per-pin endpoints at `trunk_y[N]`.
///
/// Because escape lengths are globally unique and trunk coordinates
/// are per-net unique, no two parallel wires from different nets
/// overlap. Perpendicular crossings are fine — KiCad only merges
/// nets at collinear overlaps or explicit junctions.
///
/// `(junction …)` is emitted where ≥ 3 wire endpoints coincide.
#[allow(clippy::too_many_lines, clippy::type_complexity)]
fn route_nets(
    nets: &std::collections::BTreeMap<String, Vec<(f64, f64, u16)>>,
    scope: &str,
) -> Vec<Sexpr> {
    let mut out: Vec<Sexpr> = Vec::new();
    let mut endpoint_counts: std::collections::HashMap<(i64, i64), usize> =
        std::collections::HashMap::new();
    let mut wire_seq: usize = 0;

    // Compute extents of the placement.
    let mut max_y = 0.0_f64;
    let mut max_x = 0.0_f64;
    let mut min_x = f64::INFINITY;
    for pins in nets.values() {
        for &(x, y, _) in pins {
            if y > max_y {
                max_y = y;
            }
            if x > max_x {
                max_x = x;
            }
            if x < min_x {
                min_x = x;
            }
        }
    }
    // Compute min_y too so the upper channel sits above every pin.
    let mut min_y = f64::INFINITY;
    for pins in nets.values() {
        for &(_, y, _) in pins {
            if y < min_y {
                min_y = y;
            }
        }
    }
    if !min_y.is_finite() {
        min_y = 0.0;
    }
    // Channel base coordinates. Pins whose outward direction is
    // *upward* (angle 270 in .kicad_sym; visually toward smaller Y
    // in our Y-down schematic) route to the **upper** channel
    // above the placement. All other pins route to the **lower**
    // channel below. Two pins of one component (top + bottom)
    // therefore escape on opposite sides and never share a vertical
    // segment.
    let escape_y_lower = max_y + 5.08;
    let escape_y_upper = min_y - 5.08;
    let trunk_x_base = max_x + 5.08;

    // Filter and order multi-pin nets deterministically.
    let mut multi_nets: Vec<(&String, Vec<(f64, f64, u16)>)> = Vec::new();
    for (net, pins) in nets {
        let mut uniq: Vec<(f64, f64, u16)> = Vec::new();
        for &(x, y, a) in pins {
            if !uniq
                .iter()
                .any(|&(ux, uy, _)| approx_eq(ux, x) && approx_eq(uy, y))
            {
                uniq.push((x, y, a));
            }
        }
        if uniq.len() >= 2 {
            uniq.sort_by(|a, b| {
                a.0.partial_cmp(&b.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            });
            multi_nets.push((net, uniq));
        }
    }

    // Globally-unique escape-row counters per channel. Each routed
    // pin gets its own horizontal escape row (`epy_row`), so no two
    // pin lead-ins share a Y row in the same channel. Lower channel
    // rows grow downward; upper channel rows grow upward.
    let mut lower_idx: usize = 0;
    let mut upper_idx: usize = 0;

    for (net_idx, (net, pins)) in multi_nets.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let trunk_x = trunk_x_base + (net_idx as f64) * 1.27;

        let mut trunk_ys: Vec<f64> = Vec::with_capacity(pins.len());
        for &(px, py, angle) in pins {
            // Pick a channel based on the pin's outward direction.
            // Pins pointing up in .kicad_sym (angle 270; visually
            // upward in our Y-down schematic) route via the upper
            // channel; everything else uses the lower channel. This
            // splits a component's top + bottom pins onto opposite
            // sides so their escape verticals never collide.
            let (epy_row, going_up) = if angle == 270 {
                upper_idx += 1;
                #[allow(clippy::cast_precision_loss)]
                let row = escape_y_upper - (upper_idx as f64) * 1.27;
                (row, true)
            } else {
                lower_idx += 1;
                #[allow(clippy::cast_precision_loss)]
                let row = escape_y_lower + (lower_idx as f64) * 1.27;
                (row, false)
            };
            trunk_ys.push(epy_row);

            // Segment 1: vertical from the pin to the escape row.
            // Direction follows the chosen channel.
            let _ = going_up;
            push_segment(
                &mut out,
                &mut endpoint_counts,
                &mut wire_seq,
                px,
                py,
                px,
                epy_row,
                scope,
                net,
            );
            // Segment 2: horizontal from (px, epy_row) to
            // (trunk_x, epy_row). Y is unique per pin so no two
            // horizontals share a row.
            push_segment(
                &mut out,
                &mut endpoint_counts,
                &mut wire_seq,
                px,
                epy_row,
                trunk_x,
                epy_row,
                scope,
                net,
            );
        }
        // Trunk: vertical segments at trunk_x between consecutive
        // (sorted) lead-in row endpoints. All lead-ins for this net
        // sit at (trunk_x, epy_row) and get tied together into one
        // connectivity class. Interior endpoints become
        // T-junctions (1 lead-in + 2 trunk halves = 3 endpoints)
        // and get a (junction …) emitted below.
        trunk_ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for pair in trunk_ys.windows(2) {
            push_segment(
                &mut out,
                &mut endpoint_counts,
                &mut wire_seq,
                trunk_x,
                pair[0],
                trunk_x,
                pair[1],
                scope,
                net,
            );
        }
    }
    let _ = min_x;

    // Emit junctions at any point where 3+ wire endpoints coincide.
    let mut junction_pts: Vec<(i64, i64)> = endpoint_counts
        .iter()
        .filter(|&(_, &n)| n >= 3)
        .map(|(&k, _)| k)
        .collect();
    junction_pts.sort_unstable();
    for (kx, ky) in junction_pts {
        let x = key_to_coord(kx);
        let y = key_to_coord(ky);
        out.push(junction(x, y, scope));
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn push_segment(
    out: &mut Vec<Sexpr>,
    endpoint_counts: &mut std::collections::HashMap<(i64, i64), usize>,
    wire_seq: &mut usize,
    sx: f64,
    sy: f64,
    ex: f64,
    ey: f64,
    scope: &str,
    net: &str,
) {
    if approx_eq(sx, ex) && approx_eq(sy, ey) {
        return;
    }
    out.push(wire_segment(sx, sy, ex, ey, scope, net, *wire_seq));
    *wire_seq += 1;
    *endpoint_counts.entry(coord_key(sx, sy)).or_default() += 1;
    *endpoint_counts.entry(coord_key(ex, ey)).or_default() += 1;
}

/// Emit `(global_label "<net>" …)` markers at the pin positions of
/// each net. The user-supplied SPICE net name (e.g. `0`, `vcc`,
/// `in`) is preserved in `kicad-cli`'s SPICE export only if at
/// least one label of that name appears on the schematic; otherwise
/// kicad-cli synthesises a generic `Net-(...)` name and the
/// round-trip topology comparator can't recover the original
/// ground class.
///
/// To satisfy V4 (≤ 2 labels per net per sheet) we emit at most two
/// labels per net: one at the first pin and one at the last (in the
/// same sorted order the router used). Single-pin nets get one
/// label, satisfying ERC's "no dangling pin" expectation.
fn dangling_pin_labels(
    nets: &std::collections::BTreeMap<String, Vec<(f64, f64, u16)>>,
    scope: &str,
) -> Vec<Sexpr> {
    let mut out = Vec::new();
    for (idx, (net, pins)) in nets.iter().enumerate() {
        // Deduplicate coincident pins.
        let mut uniq: Vec<(f64, f64)> = Vec::new();
        for &(x, y, _) in pins {
            if !uniq
                .iter()
                .any(|&(ux, uy)| approx_eq(ux, x) && approx_eq(uy, y))
            {
                uniq.push((x, y));
            }
        }
        if uniq.is_empty() {
            continue;
        }
        uniq.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        let (fx, fy) = uniq[0];
        out.push(global_label_simple(net, fx, fy, scope, idx * 2));
        if uniq.len() >= 2 {
            let (lx, ly) = uniq[uniq.len() - 1];
            out.push(global_label_simple(net, lx, ly, scope, idx * 2 + 1));
        }
    }
    out
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[allow(clippy::cast_possible_truncation)]
fn coord_key(x: f64, y: f64) -> (i64, i64) {
    (
        (x * 1_000_000.0).round() as i64,
        (y * 1_000_000.0).round() as i64,
    )
}

#[allow(clippy::cast_precision_loss)]
fn key_to_coord(k: i64) -> f64 {
    (k as f64) / 1_000_000.0
}

fn wire_segment(x1: f64, y1: f64, x2: f64, y2: f64, scope: &str, net: &str, seq: usize) -> Sexpr {
    let uuid = Uuid::new_v5(
        &UUID_NAMESPACE,
        format!("wire:{scope}:{net}:{seq}").as_bytes(),
    )
    .to_string();
    list(vec![
        atom("wire"),
        list(vec![
            atom("pts"),
            list(vec![
                atom("xy"),
                atom(&format_coord(x1)),
                atom(&format_coord(y1)),
            ]),
            list(vec![
                atom("xy"),
                atom(&format_coord(x2)),
                atom(&format_coord(y2)),
            ]),
        ]),
        list(vec![
            atom("stroke"),
            list(vec![atom("width"), atom("0")]),
            list(vec![atom("type"), atom("default")]),
        ]),
        list(vec![atom("uuid"), qstring(&uuid)]),
    ])
}

fn junction(x: f64, y: f64, scope: &str) -> Sexpr {
    let uuid = Uuid::new_v5(
        &UUID_NAMESPACE,
        format!("junction:{scope}:{x}:{y}").as_bytes(),
    )
    .to_string();
    list(vec![
        atom("junction"),
        list(vec![
            atom("at"),
            atom(&format_coord(x)),
            atom(&format_coord(y)),
        ]),
        list(vec![atom("diameter"), atom("0")]),
        list(vec![
            atom("color"),
            atom("0"),
            atom("0"),
            atom("0"),
            atom("0"),
        ]),
        list(vec![atom("uuid"), qstring(&uuid)]),
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
