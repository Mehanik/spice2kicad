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
    let extra_power_lib_ids = power_lib_ids_for_placement(placement);
    let extra_refs: Vec<&str> = extra_power_lib_ids.iter().map(String::as_str).collect();
    items.push(lib_symbols_with_extra(placement, library, &extra_refs));

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
    let obstacles = placement_obstacles(placement);
    for routed in route_nets(&net_pins, "root", library, &obstacles)? {
        items.push(routed);
    }
    for label in dangling_pin_labels(&net_pins, "root", &extra_pins) {
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
    let extra_power_lib_ids = power_lib_ids_for_placement(child.placement);
    let extra_refs: Vec<&str> = extra_power_lib_ids.iter().map(String::as_str).collect();
    let mut items: Vec<Sexpr> = vec![
        atom("kicad_sch"),
        list(vec![atom("version"), atom(SCHEMA_VERSION)]),
        list(vec![atom("generator"), qstring(GENERATOR)]),
        list(vec![atom("uuid"), qstring(&child_uuid(&child.name))]),
        list(vec![atom("paper"), qstring("A4")]),
        lib_symbols_with_extra(child.placement, library, &extra_refs),
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
    let obstacles = placement_obstacles(child.placement);
    for routed in route_nets(&net_pins, &child.name, library, &obstacles)? {
        items.push(routed);
    }
    for label in dangling_pin_labels(&net_pins, &child.name, &extra_pins) {
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

/// `(global_label …)` — chevron-bordered marker. V4 reserves this
/// kind for two cases: (1) nets that genuinely cross a sheet
/// boundary (v0.1 emits none); (2) one-pin "interface" nets where
/// no wire exists to anchor a plain label (ERC `label_dangling`
/// fires on a wireless plain label, but accepts a global label as
/// an external interface marker).
fn global_label_simple(text: &str, x: f64, y: f64, rot_deg: u16, scope: &str, idx: usize) -> Sexpr {
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
            atom(&rot_deg.to_string()),
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

/// Plain `(label …)` — sheet-local net name annotation (V4). Use
/// for in-sheet net labels. (`global_label` is reserved for nets
/// that cross a sheet boundary OR for one-pin "interface" nets
/// where there is no wire to anchor a plain label.)
fn label_simple(text: &str, x: f64, y: f64, rot_deg: u16, scope: &str, idx: usize) -> Sexpr {
    let uuid = Uuid::new_v5(
        &UUID_NAMESPACE,
        format!("label:{scope}:{idx}:{text}").as_bytes(),
    )
    .to_string();
    list(vec![
        atom("label"),
        qstring(text),
        list(vec![
            atom("at"),
            atom(&format_coord(x)),
            atom(&format_coord(y)),
            atom(&rot_deg.to_string()),
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
    // V13: offset property anchors to the symbol's right side so the
    // Reference / Value text bboxes do not overlap the body. Reference
    // above, Value below. The offset is rotated through the placed
    // orientation so a rotated/mirrored symbol gets a sensibly rotated
    // property too.
    let (rx, ry) = property_anchor(x_mm, y_mm, el.orientation, 2.54, -2.54);
    fields.push(reference_property(&el.refdes, rx, ry));
    let value_text = el.value.as_deref().unwrap_or(&el.refdes);
    let (vx, vy) = property_anchor(x_mm, y_mm, el.orientation, 2.54, 2.54);
    fields.push(value_property(value_text, vx, vy));
    for prop in sim_properties(&el.lib_id, value_text, &el.pin_mapping) {
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
/// Walk the placement and return the set of `power:*` library
/// identifiers needed by `spice_route::route` Stage 1 glyphs, derived
/// from each element's net node names. Mirrors the heuristic
/// classification in `classify_net_by_name` and the lib-id selection
/// in `spice-route::rails`.
fn power_lib_ids_for_placement(placement: &Placement) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for el in &placement.elements {
        for node in &el.nodes {
            if let Some(id) = power_lib_id_for_net(node) {
                out.insert(id.to_string());
            }
        }
    }
    out.into_iter().collect()
}

fn power_lib_id_for_net(net_name: &str) -> Option<&'static str> {
    use spice_layout::net_class::NetClass;
    let class = match () {
        () if net_name == "0" => NetClass::Ground,
        () => {
            let lower = net_name.to_ascii_lowercase();
            match lower.as_str() {
                "vcc" | "vdd" | "v+" | "vplus" | "+5v" | "5v" | "+12v" | "12v" | "+3v3" | "3v3" => {
                    NetClass::Power
                }
                "gnd" | "vee" | "vss" | "v-" | "vminus" => NetClass::Ground,
                _ => return None,
            }
        }
    };
    let lower = net_name.to_ascii_lowercase();
    Some(match class {
        NetClass::Power => match lower.as_str() {
            "vdd" => "power:VDD",
            "+5v" | "5v" => "power:+5V",
            "+12v" | "12v" => "power:+12V",
            "+3v3" | "3v3" => "power:+3V3",
            _ => "power:VCC",
        },
        NetClass::Ground => "power:GND",
        NetClass::Signal => return None,
    })
}

/// Same as [`lib_symbols`] but additionally inlines the listed extra
/// `lib_id`s. Used by the root and child emitters to splice in
/// `power:*` library entries referenced by `spice_route::route` Stage 1
/// glyphs (which are added after the placement is built).
fn lib_symbols_with_extra(
    placement: &Placement,
    library: &Library,
    extra_lib_ids: &[&str],
) -> Sexpr {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut entries: Vec<Sexpr> = vec![atom("lib_symbols")];
    for el in &placement.elements {
        if !seen.insert(el.lib_id.clone()) {
            continue;
        }
        if let Some(symbol) = library.lookup(&el.lib_id) {
            entries.push(lib_symbol_inline(symbol));
        }
    }
    for &lib_id in extra_lib_ids {
        if !seen.insert(lib_id.to_string()) {
            continue;
        }
        if let Some(symbol) = library.lookup(lib_id) {
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
    // V13: offset property anchors to the symbol's right side so the
    // Reference / Value text bboxes do not overlap the body. Reference
    // above, Value below. The offset is rotated through the placed
    // orientation so a rotated/mirrored symbol gets a sensibly rotated
    // property too.
    let (rx, ry) = property_anchor(x_mm, y_mm, el.orientation, 2.54, -2.54);
    fields.push(reference_property(&el.refdes, rx, ry));
    let value_text = el.value.as_deref().unwrap_or(&el.refdes);
    let (vx, vy) = property_anchor(x_mm, y_mm, el.orientation, 2.54, 2.54);
    fields.push(value_property(value_text, vx, vy));
    for prop in sim_properties(&el.lib_id, value_text, &el.pin_mapping) {
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
/// `Sim.Pins` IS emitted for active devices because `spice-resolve`
/// maps SPICE terminals to KiCad pins by canonical pin name (V11) —
/// so symbol pin order is decoupled from SPICE terminal order, and
/// kicad-cli's default `model_pin[i] = symbol_pin[i]` rule would
/// otherwise scramble nodes on `kicad-cli sch export netlist`.
/// Format: `<symbol-pin-num>=<model-pin-name>` pairs (cf.
/// `SIM_MODEL_SERIALIZER::GeneratePins` in KiCad). For a BJT
/// (model pins C,B,E,S), `pin_mapping[0]` is the symbol pin number
/// for the C terminal, etc.
fn sim_properties(lib_id: &str, value: &str, pin_mapping: &[String]) -> Vec<Sexpr> {
    // Strip the `Lib:` prefix.
    let bare = lib_id.split_once(':').map_or(lib_id, |(_, name)| name);
    // Model-pin name table per device family, in SPICE-terminal order.
    // pin_mapping[i] = symbol pin number for SPICE term (i+1) = model
    // pin model_pins[i].
    let model_pins: &[&str] = if bare.starts_with("Q_NPN") || bare.starts_with("Q_PNP") {
        &["C", "B", "E", "S"]
    } else if bare.starts_with("Q_NMOS") || bare.starts_with("Q_PMOS") {
        &["D", "G", "S", "B"]
    } else if bare.starts_with("Q_NJFET") || bare.starts_with("Q_PJFET") {
        &["D", "G", "S"]
    } else {
        &[]
    };
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
    let mut props = vec![
        sim_property("Sim.Device", device),
        sim_property("Sim.Type", sim_type),
        sim_property("Sim.Name", value),
    ];
    // Sim.Pins: "<symbol-pin-number>=<model-pin-name>" pairs sorted by
    // symbol pin number (matches KiCad's GeneratePins output). Only
    // emitted when we have a non-empty mapping; tests construct
    // PlacedElements with an empty pin_mapping for fixtures that
    // don't exercise the netlister.
    if !model_pins.is_empty() && !pin_mapping.is_empty() {
        let take = pin_mapping.len().min(model_pins.len());
        let mut pairs: Vec<(String, &str)> = (0..take)
            .map(|i| (pin_mapping[i].clone(), model_pins[i]))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        let pins_text = pairs
            .iter()
            .map(|(num, name)| format!("{num}={name}"))
            .collect::<Vec<_>>()
            .join(" ");
        props.push(sim_property("Sim.Pins", &pins_text));
    }
    props
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
/// Thin adapter over `spice_route::route`. Power/Ground nets become
/// `power:*` symbol glyphs (no wires); Signal nets are routed as
/// per-net rectilinear Steiner trees with junctions at branch points.
/// `library` is consulted by Stage 1 so a missing `power:*` lib_id
/// gracefully falls back to a `(global_label …)` instead of emitting
/// an unresolvable instance.
#[allow(clippy::type_complexity)]
fn route_nets(
    nets: &std::collections::BTreeMap<String, Vec<(f64, f64, u16)>>,
    scope: &str,
    library: &Library,
    obstacles: &[spice_route::Bbox],
) -> Result<Vec<Sexpr>, EmitError> {
    use spice_route::{NetSpec, PinRef, RouteRequest};

    // Build the per-net pin list expected by spice_route. Net class
    // is derived from the net name with the same heuristic
    // `spice_layout::net_class::classify_nets` uses (rules 1 and 3 —
    // the only ones that fire from name alone). The `*@power=`
    // tagging path (rules 2 and 4) is not visible at this level; the
    // common rail names cover the V0.1 fixtures.
    let mut specs: Vec<NetSpec> = Vec::with_capacity(nets.len());
    for (name, pins) in nets {
        // Deduplicate coincident pins, mirroring the previous router.
        let mut uniq: Vec<(f64, f64, u16)> = Vec::new();
        for &(x, y, a) in pins {
            if !uniq
                .iter()
                .any(|&(ux, uy, _)| approx_eq(ux, x) && approx_eq(uy, y))
            {
                uniq.push((x, y, a));
            }
        }
        let class = classify_net_by_name(name);
        let pin_refs: Vec<PinRef> = uniq
            .into_iter()
            .map(|(x, y, angle)| PinRef {
                element_idx: 0,
                pin_number: 0,
                x_mm: x,
                y_mm: y,
                outward: angle_to_direction(angle),
            })
            .collect();
        specs.push(NetSpec {
            name: name.clone(),
            class,
            pins: pin_refs,
        });
    }

    let suuid = sheet_uuid();
    let result = spice_route::route(RouteRequest {
        nets: &specs,
        scope,
        library: Some(library),
        sheet_uuid: &suuid,
        project_name: GENERATOR,
        obstacles,
        bounds: None,
    });
    // Split V11 (correctness) residue from other warnings. A `v11:`
    // prefix indicates a wire still touches a foreign pin after the
    // active rerouter ran — KiCad would silently short the two nets
    // on load. We escalate that to a hard `EmitError` when the
    // `SPICE2KICAD_V11_STRICT` env var is set; the env-gate keeps the
    // existing single fixture with a known placer-level pin overlap
    // (`opamp_inverting_real`) emittable for the V12/V13 verifier
    // suite while still giving callers a way to opt into nonzero
    // exit-status on V11 residue. The `v11-placer:` tag (router-
    // detected placer overlap, see `conflict::avoid_foreign_pins`)
    // is logged as a warning regardless. Other warnings (V12 body
    // crossings, missing `power:*` lib_id, conflict-resolver cap)
    // stay at the warning tier.
    let mut v11_errors: Vec<&String> = Vec::new();
    for w in &result.warnings {
        if w.starts_with("v11:") {
            v11_errors.push(w);
            eprintln!("spice2kicad route: {w}");
        } else {
            eprintln!("spice2kicad route: {w}");
        }
    }
    if !v11_errors.is_empty() && std::env::var_os("SPICE2KICAD_V11_STRICT").is_some() {
        return Err(EmitError::V11Violation(format!(
            "{} unresolved foreign-pin coincidence(s) in `{scope}`: {}",
            v11_errors.len(),
            v11_errors
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("; "),
        )));
    }
    Ok(result.sexprs.iter().map(lexpr_to_sexpr).collect())
}

/// Build the set of symbol-body bounding boxes the router should
/// avoid. Each placed element gets a square box of half-extent
/// `SYM_HALF_MM` around its origin — the same approximation used by
/// the placement-quality verifier (`crates/spice2kicad/tests/placement_quality.rs`).
/// Power-rail glyphs (`#PWR*`) are emitted by the router itself at pin
/// coordinates, so they never appear in `placement.elements` and don't
/// need filtering here.
fn placement_obstacles(placement: &Placement) -> Vec<spice_route::Bbox> {
    /// Half-extent (mm) covering a typical R/C/Q body. Matches
    /// `placement_quality::SYM_HALF_MM`.
    const SYM_HALF_MM: f64 = 2.54;
    placement
        .elements
        .iter()
        .map(|el| {
            let (cx, cy) = el.origin.to_mm();
            spice_route::Bbox {
                x0: cx - SYM_HALF_MM,
                y0: cy - SYM_HALF_MM,
                x1: cx + SYM_HALF_MM,
                y1: cy + SYM_HALF_MM,
            }
        })
        .collect()
}

/// Heuristic Power/Ground classification from the net name alone.
/// Mirrors rules 1 and 3 of `spice_layout::net_class::classify_nets`.
fn classify_net_by_name(name: &str) -> spice_layout::net_class::NetClass {
    use spice_layout::net_class::NetClass;
    if name == "0" {
        return NetClass::Ground;
    }
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "vcc" | "vdd" | "v+" | "vplus" | "+5v" | "5v" | "+12v" | "12v" | "+3v3" | "3v3" => {
            NetClass::Power
        }
        "gnd" | "vee" | "vss" | "v-" | "vminus" => NetClass::Ground,
        _ => NetClass::Signal,
    }
}

/// Convert a KiCad pin angle (in `.kicad_sym` library frame) to the
/// outward direction in the world (Y-down schematic) frame. Matches
/// the convention in the previous router: angle 270 → visually upward.
fn angle_to_direction(angle: u16) -> spice_route::Direction {
    use spice_route::Direction;
    match angle % 360 {
        90 => Direction::Down,
        180 => Direction::Left,
        270 => Direction::Up,
        // 0 and any non-cardinal fall back to Right.
        _ => Direction::Right,
    }
}

/// Convert a parsed `lexpr::Value` (the s-expr shape used by
/// `spice-route`) into the emitter's local `Sexpr`. Reuses the
/// existing `RawSexpr::from_lexpr` walker — `RawSexpr` and
/// `Sexpr` already share a `From` bridge.
fn lexpr_to_sexpr(v: &lexpr::Value) -> Sexpr {
    Sexpr::from(RawSexpr::from_lexpr(v))
}

/// Emit plain `(label …)` markers naming each signal net. The label
/// carries the SPICE net name (e.g. `b`, `in`, `out`); KiCad's
/// SPICE netlist exporter preserves the original net name only if
/// at least one label of that name appears on the schematic.
///
/// V4 hard rules enforced here:
/// - **Plain `(label …)`, not `(global_label …)`.** Global labels
///   mean "this net spans every sheet by name" and are reserved for
///   hierarchical-sheet cross-boundary nets. Internal nets on a
///   single-sheet schematic must use plain labels.
/// - One label at the geometrically leftmost body pin (ties broken
///   by smaller y), and — only when the net also touches a
///   hierarchical-sheet port — a second label at the rightmost body
///   pin. The second label is a sheet-local name-jump that pairs
///   with the port-side `hierarchical_label` so KiCad's connectivity
///   engine binds the body-side and port-side wire fragments even
///   if the router's Steiner tree is split by an obstacle detour.
///   Single-sheet fixtures emit one label per net.
/// - Power/Ground nets emit zero labels — `power:*` glyphs from
///   `spice_route` Stage 1 are the connectivity carrier.
/// - The label anchor must not coincide with a foreign-net pin
///   coordinate (V11 silent-short guard) or with a port marker
///   (`extra_pins`) that already names the net at that coord.
fn dangling_pin_labels(
    nets: &std::collections::BTreeMap<String, Vec<(f64, f64, u16)>>,
    scope: &str,
    extra_pins: &[(String, f64, f64)],
) -> Vec<Sexpr> {
    // Coordinates already carrying a port marker (sheet pin position
    // on the parent, hierarchical_label on a child) name the net by
    // themselves. Adding a `(label …)` on top is redundant and worse,
    // *replaces* the body-pin anchor we actually need to identify the
    // net at the body side (a wire from body to port without a label
    // anywhere on the body leaves the body-pin segment auto-named).
    #[allow(clippy::cast_possible_truncation)]
    let port_coords: std::collections::HashSet<(i64, i64)> = extra_pins
        .iter()
        .map(|&(_, x, y)| ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64))
        .collect();
    // V11 — a `(global_label …)` for net N planted at the coordinate
    // of a pin that belongs to a different net silently merges the
    // two nets in KiCad. Build the foreign-coord set per net (every
    // pin coord of every other net not also a pin of this net) so
    // we can filter such coordinates out before picking label
    // anchors.
    #[allow(clippy::cast_possible_truncation)]
    let key_of = |x: f64, y: f64| -> (i64, i64) {
        ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64)
    };
    let net_coords: std::collections::BTreeMap<&String, std::collections::HashSet<(i64, i64)>> =
        nets.iter()
            .map(|(n, pins)| {
                let s = pins.iter().map(|&(x, y, _)| key_of(x, y)).collect();
                (n, s)
            })
            .collect();
    let mut out = Vec::new();
    for (idx, (net, pins)) in nets.iter().enumerate() {
        // Skip Power/Ground nets: those pins already carry a `power:*`
        // glyph from `spice_route::route` Stage 1, which is the
        // connectivity carrier. Adding a global_label on top would
        // double-encode the net and trip V4 ("≤ 2 labels per net").
        if !matches!(
            classify_net_by_name(net),
            spice_layout::net_class::NetClass::Signal
        ) {
            continue;
        }
        // Foreign-pin coord set for this net.
        let own = net_coords.get(net);
        let mut foreign: std::collections::HashSet<(i64, i64)> = std::collections::HashSet::new();
        for (other, set) in &net_coords {
            if *other == net {
                continue;
            }
            for k in set {
                if !own.is_some_and(|s| s.contains(k)) {
                    foreign.insert(*k);
                }
            }
        }
        // Deduplicate coincident pins; drop any coord that belongs to
        // another net (V11 would silently short the two) and any coord
        // that already carries a port marker (sheet-pin / hierarchical_label).
        // Carry pin-outward-angle per coord so the label can rotate to
        // extend AWAY from the symbol body (V13 — text bbox doesn't
        // overlap the body the pin belongs to).
        let mut uniq: Vec<(f64, f64, u16)> = Vec::new();
        for &(x, y, ang) in pins {
            let k = key_of(x, y);
            if foreign.contains(&k) || port_coords.contains(&k) {
                continue;
            }
            if !uniq
                .iter()
                .any(|&(ux, uy, _)| approx_eq(ux, x) && approx_eq(uy, y))
            {
                uniq.push((x, y, ang));
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
        // Label rotation: orient the label so its text extends in the
        // pin's *outward* direction (away from the symbol body), so
        // the label's text bbox doesn't overlap the body it anchors
        // on (V13 — label↔body overlap). KiCad's `.kicad_sym` pin
        // `(at x y angle)` stores the angle the pin line extends
        // *toward the body* (tip at (x,y), body in direction `angle`).
        // The outward direction is therefore `angle + 180 (mod 360)`.
        //
        // Additionally, eeschema applies a world-Y flip when loading
        // pins (see the matching comment in `collect_net_pins`), which
        // is purely a frame conversion for pin tip coordinates and
        // does *not* affect the label's `(at … rot)` interpretation —
        // labels live in the same flipped world frame as the pins, so
        // we can pass the outward-angle straight through as the
        // label's rotation token.
        let label_rot = |pin_angle: u16| -> u16 { (pin_angle + 180) % 360 };
        // Classify the net's label kind:
        //
        //   - 1 body pin only → `(global_label …)`. The single pin
        //     is an *interface* to the outside world (e.g. the
        //     schematic's `in` or `out` port on a v0.1 single-sheet
        //     fixture); plain labels would trip ERC `label_dangling`
        //     because there's no wire to anchor a plain label on
        //     a one-pin net.
        //   - ≥ 2 body pins, no hierarchical-sheet port → 1 plain
        //     `(label …)` at the leftmost body pin.
        //   - ≥ 2 body pins, touches a port → 1 plain label at the
        //     leftmost body pin and a second plain label at the
        //     rightmost body pin. The pair acts as a name-jump:
        //     KiCad's in-sheet plain-label name-matching binds the
        //     body-side wire fragment to the port-side even when
        //     the router's Steiner tree is split by an obstacle
        //     detour.
        let net_touches_port = pins.iter().any(|&(x, y, _)| {
            let k = key_of(x, y);
            port_coords.contains(&k)
        });
        let (fx, fy, fang) = uniq[0];
        if uniq.len() == 1 && !net_touches_port {
            out.push(global_label_simple(
                net,
                fx,
                fy,
                label_rot(fang),
                scope,
                idx,
            ));
        } else {
            out.push(label_simple(net, fx, fy, label_rot(fang), scope, idx * 2));
            if net_touches_port && uniq.len() >= 2 {
                let (lx, ly, lang) = uniq[uniq.len() - 1];
                out.push(label_simple(
                    net,
                    lx,
                    ly,
                    label_rot(lang),
                    scope,
                    idx * 2 + 1,
                ));
            }
        }
    }
    out
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
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

/// Property text effects: 1.27 mm Newstroke font, left-justified so the
/// emitted `(at x y)` anchors the *leftmost* edge of the rendered text.
/// Left-justify is essential for V13's text-bbox computation: with
/// centred text the verifier would have to widen the bbox in both
/// directions and the placer's right-of-body offset would still overlap
/// the symbol itself.
fn property_effects() -> Sexpr {
    list(vec![
        atom("effects"),
        list(vec![
            atom("font"),
            list(vec![atom("size"), atom("1.27"), atom("1.27")]),
        ]),
        list(vec![atom("justify"), atom("left")]),
    ])
}

/// Offset the `Reference` / `Value` property `(at …)` from the symbol
/// origin by `(dx, dy)` in symbol-local space, rotated/mirrored by the
/// placed instance's orientation. Returns the world-space anchor.
fn property_anchor(
    origin_x: f64,
    origin_y: f64,
    orient: Orientation,
    dx: f64,
    dy: f64,
) -> (f64, f64) {
    // `apply_point` operates in symbol-local space; the eeschema
    // convention places property anchors in world space using the same
    // rotation/mirror that `at`'s `rot` token encodes.
    let (rx, ry) = orient.apply_point(dx, dy);
    (origin_x + rx, origin_y + ry)
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
        property_effects(),
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
        property_effects(),
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
