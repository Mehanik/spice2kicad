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

/// Fixed positive page margin (mm) at which the top-left corner of the
/// emitted content bounding box is parked (V15). A multiple of the KiCad
/// schematic grid step (1.27 mm): 25.4 mm = 20 cells.
pub const PAGE_MARGIN_MM: f64 = 25.4;

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
    /// World origin `(x_mm, y_mm)` of the sheet's top-left `(at …)`,
    /// computed by the structural placer (`spice_layout::place_sheets`).
    /// When `None` the emitter falls back to a fixed off-circuit
    /// coordinate (used by callers that don't run the placer, e.g. the
    /// in-crate unit tests).
    pub origin: Option<(f64, f64)>,
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
    // Nets exposed only through a hierarchical sheet pin (the parent
    // side of each `X<n>` port) — they carry glyphs / flags too.
    let sheet_port_nets: Vec<String> = sheets
        .iter()
        .flat_map(|b| b.ports.iter().map(|p| p.net.clone()))
        .collect();
    let extra_power_lib_ids =
        power_lib_ids_for_placement(placement, library, &BTreeSet::new(), &sheet_port_nets, true);
    let extra_refs: Vec<&str> = extra_power_lib_ids.iter().map(String::as_str).collect();
    items.push(lib_symbols_with_extra(placement, library, &extra_refs));

    for el in &placement.elements {
        // V10 / annotation-spec §4.5: a `*@power` / `;@ power=` source
        // is a power *rail*, not a drawn component. Suppress its
        // `(symbol …)` instance; the consuming components' `power:*`
        // glyphs carry the rail connectivity.
        if el.is_power_source {
            continue;
        }
        items.push(symbol_instance(el));
    }

    // Hierarchical-sheet instances. Each block lives at a unique
    // location on the parent canvas; pin coordinates are derived from
    // the block's origin.
    let mut extra_pins: Vec<(String, f64, f64)> = Vec::new();
    // Coordinates of every hierarchical-sheet port pin. A `power:*`
    // glyph landing on one of these would overprint the sheet's port
    // label and overlap the sheet body, so the router offsets it
    // outward with a stub (V12/V13/V14 detached-glyph fallback).
    let mut sheet_edge_pins: Vec<(f64, f64)> = Vec::new();
    for (idx, block) in sheets.iter().enumerate() {
        let (sheet_node, pin_labels, sheet_pin_pos) = sheet_block(block, idx);
        items.push(sheet_node);
        for label in pin_labels {
            items.push(label);
        }
        // Sheet pin positions become extra "pins" on the parent net so
        // wire routing connects body pins to the sheet block.
        for (_, px, py) in &sheet_pin_pos {
            sheet_edge_pins.push((*px, *py));
        }
        extra_pins.extend(sheet_pin_pos);
    }

    let net_pins = collect_net_pins(placement, library, &extra_pins);
    let obstacles = placement_obstacles(placement, library);
    let driven = collect_driven_nets(placement, library);
    let requires_driver = collect_driver_required_nets(placement, library);
    let negative_rails = spice_layout::net_class::negative_rail_nets(placement);
    for routed in route_nets(
        &net_pins,
        "root",
        library,
        &obstacles,
        &driven,
        &requires_driver,
        &negative_rails,
        &sheet_edge_pins,
    )? {
        items.push(routed);
    }
    let property_bboxes = placement_property_bboxes(placement);
    for label in dangling_pin_labels(&net_pins, "root", &extra_pins, &property_bboxes) {
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

    // DECORATION-phase text-nudge: move colliding Reference / Value
    // property text off mutual / power-glyph collisions (V13 part 4).
    // Runs after routing + labels (so it sees the final geometry) and
    // before page translation. Moves TEXT only — never a symbol pose.
    nudge_property_text(&mut items, placement, library);
    nudge_power_glyph_value_text(&mut items, placement, library);

    let mut root = Sexpr::List(items);
    translate_into_page(&mut root);
    Ok(root.to_pretty())
}

/// Emit a hierarchical-sheet child schematic. The child carries a
/// `(hierarchical_label …)` per port at the same world-coordinate as
/// a body-element pin connected to the same SPICE net (so the port and
/// the body net resolve to one connectivity class).
pub fn emit_child_sheet(child: &ChildSheet<'_>, library: &Library) -> Result<String, EmitError> {
    let port_driven: BTreeSet<String> = child.ports.iter().cloned().collect();
    let extra_power_lib_ids =
        power_lib_ids_for_placement(child.placement, library, &port_driven, &[], false);
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
        // V10 / annotation-spec §4.5: power-rail sources are not drawn.
        if el.is_power_source {
            continue;
        }
        items.push(child_symbol_instance(el, &child.instance_refdeses));
    }

    let net_pins = collect_net_pins(child.placement, library, &extra_pins);
    let obstacles = placement_obstacles(child.placement, library);
    let mut driven = collect_driven_nets(child.placement, library);
    // A subckt *port* net is exposed to the parent; its driver status
    // (real driver or a parent-side PWR_FLAG) is decided on the parent
    // sheet. Marking ports as "driven" here suppresses a child-sheet
    // PWR_FLAG that would double-drive the global parent net through the
    // sheet port (`pin_to_pin`: two power_out pins). Only genuinely
    // sheet-local child nets receive a child PWR_FLAG.
    driven.extend(port_driven.iter().cloned());
    let requires_driver = collect_driver_required_nets(child.placement, library);
    let negative_rails = spice_layout::net_class::negative_rail_nets(child.placement);
    for routed in route_nets(
        &net_pins,
        &child.name,
        library,
        &obstacles,
        &driven,
        &requires_driver,
        &negative_rails,
        &[],
    )? {
        items.push(routed);
    }
    let child_props = placement_property_bboxes(child.placement);
    for label in dangling_pin_labels(&net_pins, &child.name, &extra_pins, &child_props) {
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

    // DECORATION-phase text-nudge (V13 part 4) — see `emit_root`.
    nudge_property_text(&mut items, child.placement, library);
    nudge_power_glyph_value_text(&mut items, child.placement, library);

    let mut root = Sexpr::List(items);
    translate_into_page(&mut root);
    Ok(root.to_pretty())
}

/// Render a `(sheet …)` block plus the `(global_label …)` pieces that
/// pin its port symbols to the parent net coordinates.
fn sheet_block(block: &SheetBlock, idx: usize) -> (Sexpr, Vec<Sexpr>, Vec<(String, f64, f64)>) {
    // Origin is supplied by the structural placer
    // (`spice_layout::place_sheets`) so the sheet lands adjacent to the
    // circuitry it shares nets with (V6). Without a placer-supplied
    // origin (e.g. callers that bypass layout), fall back to a fixed
    // off-circuit column stacked by index.
    #[allow(clippy::cast_precision_loss)]
    let (origin_x, origin_y): (f64, f64) =
        block.origin.unwrap_or((200.0, 50.0 + (idx as f64) * 60.0));
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
fn power_lib_ids_for_placement(
    placement: &Placement,
    library: &Library,
    extra_driven: &BTreeSet<String>,
    extra_pin_nets: &[String],
    is_root: bool,
) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    // Negative-rail nets render with `power:VEE` (not `power:GND`),
    // derived generally from `*@power` polarity / canonical names. Must
    // match the router's per-net glyph choice so the right lib_symbol
    // (and only that one) inlines (V3).
    let negative_rails = spice_layout::net_class::negative_rail_nets(placement);
    for el in &placement.elements {
        for node in &el.nodes {
            if let Some(id) = power_lib_id_for_net(node, &negative_rails) {
                out.insert(id.to_string());
            }
        }
    }
    // Sheet-port nets carry a glyph too (a parent power/ground net
    // exposed only through a hierarchical sheet pin still gets a
    // `power:*` glyph). Reflect those lib_ids so they inline as well.
    for net in extra_pin_nets {
        if let Some(id) = power_lib_id_for_net(net, &negative_rails) {
            out.insert(id.to_string());
        }
    }
    // `power:PWR_FLAG` is referenced by `spice_route::pwrflag` whenever
    // a net has pins but no driving pin — inline it so the instance the
    // router emits resolves (V3 verbatim passthrough). Use the same
    // net-pin / driver derivation the router runs so the lib-symbol set
    // exactly matches what gets emitted (no dangling entry, no missing
    // one).
    if placement_has_undriven_net(placement, library, extra_driven, extra_pin_nets, is_root) {
        out.insert("power:PWR_FLAG".to_string());
    }
    out.into_iter().collect()
}

/// True when some net in `placement` will receive a `PWR_FLAG`: it has
/// at least one pin, no driving pin, is not in `extra_driven` (subckt
/// ports owned by the parent), and — for Power/Ground class nets —
/// only on the root sheet (global nets are driven once, at root).
/// Mirrors the predicate in `spice_route::pwrflag::emit`.
fn placement_has_undriven_net(
    placement: &Placement,
    library: &Library,
    extra_driven: &BTreeSet<String>,
    extra_pin_nets: &[String],
    is_root: bool,
) -> bool {
    // Feed the same sheet-port "extra pins" the router sees, so a
    // parent power/ground net present only through a hierarchical sheet
    // pin still counts as having a pin (and thus gets a PWR_FLAG).
    let extra: Vec<(String, f64, f64)> = extra_pin_nets
        .iter()
        .map(|n| (n.clone(), 0.0, 0.0))
        .collect();
    let net_pins = collect_net_pins(placement, library, &extra);
    let driven = collect_driven_nets(placement, library);
    let requires_driver = collect_driver_required_nets(placement, library);
    net_pins.iter().any(|(name, pins)| {
        if pins.is_empty() || driven.contains(name) || extra_driven.contains(name) {
            return false;
        }
        let class = classify_net_by_name(name);
        let is_power_ground = !matches!(class, spice_layout::net_class::NetClass::Signal);
        // Mirror `spice_route::pwrflag::emit`: a Power/Ground net always
        // requires a driver (it gets a `power_in` glyph); a Signal net
        // requires one only if a placement pin on it is input/power_in.
        if !is_power_ground && !requires_driver.contains(name) {
            return false;
        }
        if is_power_ground && !is_root {
            // Power/Ground on a child sheet: root owns the driver.
            return false;
        }
        true
    })
}

fn power_lib_id_for_net(
    net_name: &str,
    negative_rails: &std::collections::BTreeSet<String>,
) -> Option<&'static str> {
    use spice_layout::net_class::{NetClass, matches_negative_rail_name};
    // A negative supply rail renders with the distinct `power:VEE`
    // glyph, regardless of NetClass (it is Ground-class for layout).
    // Honour both the upstream-derived set (which captures `*@power`
    // negative-voltage polarity) and a canonical-name fallback.
    if negative_rails.contains(net_name)
        || matches_negative_rail_name(&net_name.to_ascii_lowercase())
    {
        return Some("power:VEE");
    }
    let class = match () {
        () if net_name == "0" => NetClass::Ground,
        () => {
            let lower = net_name.to_ascii_lowercase();
            match lower.as_str() {
                "vcc" | "vdd" | "v+" | "vplus" | "+5v" | "5v" | "+12v" | "12v" | "+3v3" | "3v3" => {
                    NetClass::Power
                }
                "gnd" | "vss" => NetClass::Ground,
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
        // A suppressed power-rail source emits no instance, so its
        // lib symbol would be a dangling, unreferenced entry.
        if el.is_power_source {
            continue;
        }
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
pub(crate) fn collect_net_pins(
    placement: &Placement,
    library: &Library,
    extra_pins: &[(String, f64, f64)],
) -> std::collections::BTreeMap<String, Vec<(f64, f64, u16)>> {
    let mut nets: std::collections::BTreeMap<String, Vec<(f64, f64, u16)>> =
        std::collections::BTreeMap::new();
    for el in &placement.elements {
        // V10 / annotation-spec §4.5: a power-rail source contributes
        // no pins of its own — dropping them drops only ITS two
        // `power:*` glyphs. Every circuit component's pin on the rail
        // net still emits a glyph, so the rail stays connected.
        if el.is_power_source {
            continue;
        }
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

/// Set of net names that have at least one *driving* pin — a pin whose
/// KiCad electrical type drives connectivity (Output, Power-output,
/// bidirectional, tri-state, open-collector / open-emitter). Used by
/// the router to decide which nets need a `PWR_FLAG` driver marker so
/// ERC stops reporting `power_pin_not_driven` / `pin_not_driven`.
///
/// Power-rail *sources* (`is_power_source`) contribute no symbol and
/// no pins (V10), so their nets are driven only if a real circuit
/// element on the net carries a driving pin — exactly the rail case
/// that needs a `PWR_FLAG`. Hierarchical `extra_pins` (sheet ports /
/// labels) are intentionally NOT counted as drivers: they are label
/// anchors, and on a child sheet the body still needs its own
/// `PWR_FLAG` if nothing inside drives the net.
pub(crate) fn collect_driven_nets(
    placement: &Placement,
    library: &Library,
) -> std::collections::BTreeSet<String> {
    net_set_where(placement, library, |pin| pin.electrical.drives())
}

/// Set of net names with at least one pin that *requires* a driver
/// (a `power_in` or `input` pin). A net absent from this set imposes no
/// ERC driver requirement (e.g. a purely `passive` R–C junction) and
/// must not receive a `PWR_FLAG`.
pub(crate) fn collect_driver_required_nets(
    placement: &Placement,
    library: &Library,
) -> std::collections::BTreeSet<String> {
    net_set_where(placement, library, |pin| pin.electrical.requires_driver())
}

/// Collect net names having ≥1 pin satisfying `pred`. Shared backbone
/// of [`collect_driven_nets`] and [`collect_driver_required_nets`].
fn net_set_where(
    placement: &Placement,
    library: &Library,
    pred: impl Fn(&kicad_symbols::TransformedPin) -> bool,
) -> std::collections::BTreeSet<String> {
    let mut set = std::collections::BTreeSet::new();
    for el in &placement.elements {
        if el.is_power_source {
            continue;
        }
        let Some(symbol) = library.lookup(&el.lib_id) else {
            continue;
        };
        let pins = symbol.pins_in(el.orientation);
        for (node, kicad_pin) in el.nodes.iter().zip(el.pin_mapping.iter()) {
            let Some(pin) = pins.iter().find(|p| &p.number == kicad_pin) else {
                continue;
            };
            if pred(pin) {
                set.insert(node.clone());
            }
        }
    }
    set
}

/// Route every net with ≥ 2 pin positions.
///
/// Thin adapter over `spice_route::route`. Power/Ground nets become
/// `power:*` symbol glyphs (no wires); Signal nets are routed as
/// per-net rectilinear Steiner trees with junctions at branch points.
/// `library` is consulted by Stage 1 so a missing `power:*` lib_id
/// gracefully falls back to a `(global_label …)` instead of emitting
/// an unresolvable instance.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn route_nets(
    nets: &std::collections::BTreeMap<String, Vec<(f64, f64, u16)>>,
    scope: &str,
    library: &Library,
    obstacles: &[spice_route::Bbox],
    driven: &std::collections::BTreeSet<String>,
    requires_driver: &std::collections::BTreeSet<String>,
    negative_rails: &std::collections::BTreeSet<String>,
    sheet_edge_pins: &[(f64, f64)],
) -> Result<Vec<Sexpr>, EmitError> {
    use spice_route::{NetSpec, PinRef, RouteRequest};

    let is_sheet_edge = |x: f64, y: f64| {
        sheet_edge_pins
            .iter()
            .any(|&(sx, sy)| approx_eq(sx, x) && approx_eq(sy, y))
    };

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
        let net_driven = driven.contains(name);
        let net_requires = requires_driver.contains(name);
        let pin_refs: Vec<PinRef> = uniq
            .into_iter()
            .map(|(x, y, angle)| {
                let on_sheet_edge = is_sheet_edge(x, y);
                // A sheet port pin's glyph must hang *outward* — away from
                // the sheet body, which lies to the right of its left-edge
                // port column. `collect_net_pins` stamps `extra_pins` with
                // a default rightward angle; override it to Left so the
                // offset+stub escapes the sheet body rather than diving
                // into it.
                let outward = if on_sheet_edge {
                    spice_route::Direction::Left
                } else {
                    angle_to_direction(angle)
                };
                PinRef {
                    element_idx: 0,
                    pin_number: 0,
                    x_mm: x,
                    y_mm: y,
                    outward,
                    drives: net_driven,
                    requires_driver: net_requires,
                    on_sheet_edge,
                }
            })
            .collect();
        specs.push(NetSpec {
            name: name.clone(),
            class,
            pins: pin_refs,
            negative_rail: negative_rails.contains(name),
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

/// Outcome of trial-routing a placement: the world-frame wire segments
/// the *real* router emitted (after every conflict-resolution and
/// cleanup pass — the stages where V5 violations are born), plus the
/// count of unresolved `v11:` foreign-pin coincidences. Used by the
/// routing-aware orientation-refinement phase ([`crate::refine`]) to
/// measure the actual V5 / V11 consequence of a candidate orientation.
pub(crate) struct TrialRoute {
    /// Each wire's two endpoints in world mm: `((x1, y1), (x2, y2))`.
    pub segments: Vec<crate::v5::WireSegment>,
    /// Number of `v11:` warnings (router could not detour off a foreign
    /// pin). Must not increase under a candidate orientation.
    pub v11_count: usize,
}

/// Run the *real* router over `placement` and return its wire segments
/// plus V11-warning count. This is the same routing path
/// [`emit_root`] runs (`collect_net_pins` → `placement_obstacles` →
/// `spice_route::route`), minus hierarchical-sheet `extra_pins` (the
/// refinement targets body-pin orientation, which sheet labels do not
/// affect). Routing errors collapse to an empty result so the caller
/// simply declines the candidate.
pub(crate) fn trial_route(placement: &Placement, library: &Library) -> TrialRoute {
    use spice_route::{NetSpec, PinRef, RouteRequest};

    let net_pins = collect_net_pins(placement, library, &[]);
    let obstacles = placement_obstacles(placement, library);

    let mut specs: Vec<NetSpec> = Vec::with_capacity(net_pins.len());
    for (name, pins) in &net_pins {
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
        // trial_route only measures wire-segment geometry for V5/V11
        // refinement; PWR_FLAG markers are not emitted as wires, so the
        // driver flag is irrelevant here.
        let pin_refs: Vec<PinRef> = uniq
            .into_iter()
            .map(|(x, y, angle)| PinRef {
                element_idx: 0,
                pin_number: 0,
                x_mm: x,
                y_mm: y,
                outward: angle_to_direction(angle),
                drives: false,
                requires_driver: false,
                on_sheet_edge: false,
            })
            .collect();
        specs.push(NetSpec {
            name: name.clone(),
            class,
            pins: pin_refs,
            // Negative-rail VEE-vs-GND glyph selection is a *decoration*
            // concern (glyph identity), not a wire-geometry one. The
            // refinement phase measures only V5/V11 wire consequences,
            // so the flag would only perturb orientation choice without
            // changing any wire it measures — keep it `false` so glyph
            // selection never feeds back into placement (CLAUDE.md:
            // "Decoration is a strict consumer of placement output").
            negative_rail: false,
        });
    }

    let suuid = sheet_uuid();
    let result = spice_route::route(RouteRequest {
        nets: &specs,
        scope: "refine",
        library: Some(library),
        sheet_uuid: &suuid,
        project_name: GENERATOR,
        obstacles: &obstacles,
        bounds: None,
    });
    let v11_count = result
        .warnings
        .iter()
        .filter(|w| w.starts_with("v11:"))
        .count();
    let segments = result
        .sexprs
        .iter()
        .filter_map(wire_segment_from_lexpr)
        .collect();
    TrialRoute {
        segments,
        v11_count,
    }
}

/// Extract `((x1,y1),(x2,y2))` from a `(wire (pts (xy …) (xy …)))`
/// lexpr value emitted by `spice_route`. Returns `None` for any other
/// node kind (junctions, power glyphs, labels) or a malformed wire.
fn wire_segment_from_lexpr(v: &lexpr::Value) -> Option<crate::v5::WireSegment> {
    // lexpr renders `(wire (pts (xy a b) (xy c d)))` as a proper list.
    let items: Vec<&lexpr::Value> = v.list_iter()?.collect();
    if items.first().map(|h| h.as_symbol()) != Some(Some("wire")) {
        return None;
    }
    let pts = items.iter().skip(1).find_map(|node| {
        let inner: Vec<&lexpr::Value> = node.list_iter()?.collect();
        (inner.first().map(|h| h.as_symbol()) == Some(Some("pts"))).then_some(inner)
    })?;
    let mut coords: Vec<(f64, f64)> = Vec::new();
    for xy in pts.iter().skip(1) {
        let inner: Vec<&lexpr::Value> = xy.list_iter()?.collect();
        if inner.first().map(|h| h.as_symbol()) != Some(Some("xy")) {
            continue;
        }
        let x = inner.get(1)?.as_f64()?;
        let y = inner.get(2)?.as_f64()?;
        coords.push((x, y));
    }
    if coords.len() < 2 {
        return None;
    }
    Some((coords[0], coords[1]))
}

/// Build the set of symbol-body bounding boxes the router should
/// avoid for V12 (wires do not cross foreign symbol bodies).
///
/// For each placed element we look up its library symbol and use
/// [`Symbol::body_bbox`] to obtain the real graphical extent in
/// symbol-local coordinates, then transform to world frame using the
/// same convention as pin coordinates (rotate/mirror via
/// [`Orientation::apply_point`], then apply the eeschema y-flip
/// `world_y = origin_y - local_y`). A 0.5 mm margin is added so
/// wires routed on the adjacent grid line clear the body cleanly.
///
/// Elements that resolve to a library symbol without graphics (V8
/// hierarchical-sheet stubs, `power:*` glyphs) fall back to the
/// uniform 2.54 mm half-extent box used previously — they are
/// either not visible obstacles (sheets are drawn separately and
/// don't carry V12-relevant graphics) or correctly skipped as
/// router-managed (power glyphs are placed by Stage 1, not present
/// in `placement.elements`).
///
/// Power-rail glyphs are filtered out explicitly by `lib_id` prefix
/// just in case a caller has injected one into the placement.
pub(crate) fn placement_obstacles(
    placement: &Placement,
    library: &Library,
) -> Vec<spice_route::Bbox> {
    /// Half-extent (mm) fallback for symbols whose body bbox is
    /// unavailable (sheet stubs, missing libraries).
    const SYM_HALF_MM: f64 = 2.54;
    placement
        .elements
        .iter()
        .filter_map(|el| {
            if el.lib_id.starts_with("power:") {
                return None;
            }
            // A suppressed power-rail source draws nothing, so it is
            // not an obstacle (V10 / annotation-spec §4.5).
            if el.is_power_source {
                return None;
            }
            let (ox, oy) = el.origin.to_mm();
            let bbox = library
                .lookup(&el.lib_id)
                .and_then(Symbol::body_bbox)
                .map_or(
                    spice_route::Bbox {
                        x0: ox - SYM_HALF_MM,
                        y0: oy - SYM_HALF_MM,
                        x1: ox + SYM_HALF_MM,
                        y1: oy + SYM_HALF_MM,
                    },
                    |local| body_bbox_to_world(local, ox, oy, el.orientation),
                );
            Some(bbox)
        })
        .collect()
}

/// Transform a symbol-local [`LocalBbox`] into world-frame
/// [`spice_route::Bbox`] using the same convention as pin
/// coordinates: rotate / mirror via [`Orientation::apply_point`],
/// then apply the eeschema y-flip
/// `world_y = origin_y - local_y` and take the AABB of the four
/// transformed corners. The output bbox is axis-aligned in world
/// space even after a 90° rotation.
fn body_bbox_to_world(
    local: kicad_symbols::LocalBbox,
    origin_x: f64,
    origin_y: f64,
    orient: Orientation,
) -> spice_route::Bbox {
    let corners = [
        (local.x0, local.y0),
        (local.x0, local.y1),
        (local.x1, local.y0),
        (local.x1, local.y1),
    ];
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (lx, ly) in corners {
        let (rx, ry) = orient.apply_point(lx, ly);
        let wx = origin_x + rx;
        let wy = origin_y - ry;
        if wx < min_x {
            min_x = wx;
        }
        if wx > max_x {
            max_x = wx;
        }
        if wy < min_y {
            min_y = wy;
        }
        if wy > max_y {
            max_y = wy;
        }
    }
    spice_route::Bbox {
        x0: min_x,
        y0: min_y,
        x1: max_x,
        y1: max_y,
    }
}

/// Heuristic Power/Ground classification from the net name alone.
/// Mirrors rules 1 and 3 of `spice_layout::net_class::classify_nets`.
pub(crate) fn classify_net_by_name(name: &str) -> spice_layout::net_class::NetClass {
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
pub(crate) fn angle_to_direction(angle: u16) -> spice_route::Direction {
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

/// One label the emitter will plant: its net name, world anchor,
/// rotation (CCW degrees, world frame), and whether it is a
/// `(global_label …)` (vs a plain `(label …)`). Factored out of
/// [`dangling_pin_labels`] so the routing-aware refinement phase can
/// measure the exact same label geometry (V13) the emitter writes —
/// shared, never re-derived.
#[derive(Debug, Clone)]
pub(crate) struct LabelSpec {
    pub net: String,
    pub x: f64,
    pub y: f64,
    pub rot: u16,
    pub is_global: bool,
}

/// Build the structured [`LabelSpec`] list naming each signal net. The
/// label carries the SPICE net name (e.g. `b`, `in`, `out`); KiCad's
/// SPICE netlist exporter preserves the original net name only if at
/// least one label of that name appears on the schematic. The Sexpr
/// emitter ([`dangling_pin_labels`]) and the refinement V13 metric both
/// consume this, so their label geometry can never drift.
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
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn label_specs(
    nets: &std::collections::BTreeMap<String, Vec<(f64, f64, u16)>>,
    extra_pins: &[(String, f64, f64)],
    property_bboxes: &[TextBbox],
) -> Vec<LabelSpec> {
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
        let _ = idx;
        let (fx, fy, fang) = uniq[0];
        if uniq.len() == 1 && !net_touches_port {
            // Global labels carry a chevron; their bbox differs from a
            // plain label, so we keep the body-clearing rotation as-is
            // (the property-text avoidance below targets plain labels,
            // where the regression appears).
            out.push(LabelSpec {
                net: net.clone(),
                x: fx,
                y: fy,
                rot: label_rot(fang),
                is_global: true,
            });
        } else {
            // V13: prefer the body-clearing outward rotation, but if that
            // makes the label text overlap a Reference/Value bbox (e.g.
            // the inverting-amp `out` label landing on the feedback
            // resistor's Value), rotate the label to a clear direction.
            let rot = label_rotation_avoiding(net, (fx, fy), label_rot(fang), property_bboxes);
            out.push(LabelSpec {
                net: net.clone(),
                x: fx,
                y: fy,
                rot,
                is_global: false,
            });
            if net_touches_port && uniq.len() >= 2 {
                let (lx, ly, lang) = uniq[uniq.len() - 1];
                let rot2 = label_rotation_avoiding(net, (lx, ly), label_rot(lang), property_bboxes);
                out.push(LabelSpec {
                    net: net.clone(),
                    x: lx,
                    y: ly,
                    rot: rot2,
                    is_global: false,
                });
            }
        }
    }
    out
}

/// Emit the `(label …)` / `(global_label …)` Sexpr nodes for a sheet,
/// thin wrapper over [`label_specs`] that assigns each spec a stable
/// per-net UUID seed. Used by [`emit_root`] / [`emit_child_sheet`].
fn dangling_pin_labels(
    nets: &std::collections::BTreeMap<String, Vec<(f64, f64, u16)>>,
    scope: &str,
    extra_pins: &[(String, f64, f64)],
    property_bboxes: &[TextBbox],
) -> Vec<Sexpr> {
    let specs = label_specs(nets, extra_pins, property_bboxes);
    // Reproduce the previous per-net UUID-seed scheme: globals seeded by
    // net order index; plain labels by `idx*2` (+1 for the second of a
    // name-jump pair). Net order matches `label_specs` since both walk
    // `nets` in BTreeMap order; we re-derive the index per net.
    let mut out = Vec::with_capacity(specs.len());
    let mut net_idx: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for (i, n) in nets.keys().enumerate() {
        net_idx.insert(n.as_str(), i);
    }
    // Track how many plain labels we've emitted per net (0 → first /
    // leftmost, 1 → second / rightmost) for the name-jump seed offset.
    let mut plain_seen: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for spec in &specs {
        let idx = net_idx.get(spec.net.as_str()).copied().unwrap_or(0);
        if spec.is_global {
            out.push(global_label_simple(
                &spec.net, spec.x, spec.y, spec.rot, scope, idx,
            ));
        } else {
            let nth = plain_seen.entry(spec.net.as_str()).or_insert(0);
            let seed = idx * 2 + *nth;
            *nth += 1;
            out.push(label_simple(
                &spec.net, spec.x, spec.y, spec.rot, scope, seed,
            ));
        }
    }
    out
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

/// Axis-aligned text bounding box (world mm). Mirrors the geometry the
/// V13 verifier uses so the emitter can pre-empt a label↔property-text
/// overlap before it is written.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TextBbox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl TextBbox {
    pub(crate) fn intersects(self, o: TextBbox) -> bool {
        let eps = 1e-3;
        self.x0 + eps < o.x1 && o.x0 + eps < self.x1 && self.y0 + eps < o.y1 && o.y0 + eps < self.y1
    }
}

/// World-frame AABB of left-justified text drawn at `anchor`, rotated
/// `rot_deg` CCW on screen. Matches the V13 verifier's `text_bbox`
/// (size 1.27 mm, width = 0.6·n·size + 0.8·size, height = 1.4·size) so
/// the emitter's collision check agrees with the test that grades it.
pub(crate) fn text_bbox(text: &str, anchor: (f64, f64), rot_deg: u16) -> TextBbox {
    let size = 1.27_f64;
    #[allow(clippy::cast_precision_loss)]
    let chars = text.chars().count() as f64;
    let width = chars * 0.6 * size + 0.8 * size;
    let height = 1.4 * size;
    let (lx, rx, ty, by) = (0.0, width, -height / 2.0, height / 2.0);
    let theta = f64::from(rot_deg).to_radians();
    let (s, c) = (theta.sin(), theta.cos());
    let corners = [(lx, ty), (rx, ty), (rx, by), (lx, by)];
    let (mut x0, mut y0, mut x1, mut y1) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for (px, py) in corners {
        let wx = anchor.0 + c * px + s * py;
        let wy = anchor.1 - s * px + c * py;
        x0 = x0.min(wx);
        y0 = y0.min(wy);
        x1 = x1.max(wx);
        y1 = y1.max(wy);
    }
    TextBbox { x0, y0, x1, y1 }
}

/// Reference / Value property-text bboxes for every placed element, in
/// the same world frame and offsets the emitter writes them at
/// (Reference at local `(2.54, -2.54)`, Value at `(2.54, 2.54)`, both
/// left-justified, rot 0). Hidden properties are excluded — the
/// resistor/cap/opamp Reference & Value are the only visible ones.
pub(crate) fn placement_property_bboxes(placement: &Placement) -> Vec<TextBbox> {
    let mut out = Vec::new();
    for el in &placement.elements {
        // A suppressed power-rail source draws no Reference/Value text
        // (V10 / annotation-spec §4.5), so it reserves no bbox.
        if el.is_power_source {
            continue;
        }
        let (ox, oy) = el.origin.to_mm();
        let (rx, ry) = property_anchor(ox, oy, el.orientation, 2.54, -2.54);
        out.push(text_bbox(&el.refdes, (rx, ry), 0));
        let value_text = el.value.as_deref().unwrap_or(&el.refdes);
        let (vx, vy) = property_anchor(ox, oy, el.orientation, 2.54, 2.54);
        out.push(text_bbox(value_text, (vx, vy), 0));
    }
    out
}

/// Convert a [`spice_route::Bbox`] (world AABB) into a [`TextBbox`] so
/// the nudge pass can intersection-test symbol bodies against text.
fn bbox_as_text(b: spice_route::Bbox) -> TextBbox {
    TextBbox {
        x0: b.x0,
        y0: b.y0,
        x1: b.x1,
        y1: b.y1,
    }
}

/// True if `t`'s text bbox strictly intersects the axis-aligned wire
/// segment `(a, b)` (interior overlap). Mirrors the V13(3) verifier's
/// strict-interior test so a nudged property anchor is never dropped
/// onto a wire's interior.
fn text_crosses_segment(t: TextBbox, a: (f64, f64), b: (f64, f64)) -> bool {
    let eps = 1e-3;
    let (xlo, xhi, ylo, yhi) = (t.x0 + eps, t.x1 - eps, t.y0 + eps, t.y1 - eps);
    if xlo >= xhi || ylo >= yhi {
        return false;
    }
    let (x1, y1) = a;
    let (x2, y2) = b;
    if x1.max(x2) <= xlo || x1.min(x2) >= xhi || y1.max(y2) <= ylo || y1.min(y2) >= yhi {
        return false;
    }
    if (x1 - x2).abs() < f64::EPSILON {
        x1 > xlo && x1 < xhi && y1.min(y2) < yhi && y1.max(y2) > ylo
    } else if (y1 - y2).abs() < f64::EPSILON {
        y1 > ylo && y1 < yhi && x1.min(x2) < xhi && x1.max(x2) > xlo
    } else {
        false
    }
}

/// Candidate local property-text offsets, default first. The default
/// `(2.54, base_dy)` (Reference `base_dy = -2.54`, Value `+2.54`) is
/// emitted byte-for-byte whenever it does not collide, so every clean
/// fixture is unchanged. Fallbacks keep the property on the same
/// vertical side of the origin (Reference stays above, Value below) so
/// Reference and Value never swap places; they widen the horizontal
/// offset and push further away from the body along the same axis.
/// Purely geometric — no fixture constants.
fn property_offset_candidates(base_dy: f64) -> Vec<(f64, f64)> {
    // Vertical distances on the default side (sign of base_dy), and
    // horizontal offsets sweeping both sides. The default `(2.54,
    // base_dy)` is first so a clean fixture is byte-identical; the rest
    // widen monotonically so the chosen anchor stays "least
    // surprising". Within each vertical row the horizontal offset
    // sweeps near→far, both sides.
    let vertical = [base_dy, base_dy * 2.0, base_dy * 3.0, base_dy * 4.0];
    let horizontal = [2.54_f64, -2.54, 5.08, -5.08, 7.62, -7.62];
    let mut out = vec![(2.54_f64, base_dy)];
    for &vy in &vertical {
        for &hx in &horizontal {
            if (hx, vy) != (2.54, base_dy) {
                out.push((hx, vy));
            }
        }
    }
    out
}

/// World-frame bboxes of every VISIBLE symbol-internal pin-name and
/// pin-number text for every host (non-power) placed symbol (V13 part
/// 5). [`Symbol::pin_text_local_bboxes`] yields one local box per
/// visible label; each is transformed through the placed pose exactly
/// like the symbol body bbox.
fn host_pin_text_bboxes(placement: &Placement, library: &Library) -> Vec<TextBbox> {
    placement
        .elements
        .iter()
        .filter(|el| !el.is_power_source && !el.lib_id.starts_with("power:"))
        .flat_map(|el| {
            let (ox, oy) = el.origin.to_mm();
            let orient = el.orientation;
            library
                .lookup(&el.lib_id)
                .map(Symbol::pin_text_local_bboxes)
                .unwrap_or_default()
                .into_iter()
                .map(move |local| bbox_as_text(body_bbox_to_world(local, ox, oy, orient)))
        })
        .collect()
}

/// Obstacle classes already serialised into `items`: power-glyph
/// net-name `Value` text bboxes (returned as `occupied`), label text
/// bboxes, and wire segments. Used by [`nudge_property_text`].
type EmittedObstacles = (Vec<TextBbox>, Vec<TextBbox>, Vec<((f64, f64), (f64, f64))>);
fn emitted_text_obstacles(items: &[Sexpr]) -> EmittedObstacles {
    let mut occupied: Vec<TextBbox> = Vec::new();
    let mut labels: Vec<TextBbox> = Vec::new();
    let mut wires: Vec<((f64, f64), (f64, f64))> = Vec::new();
    for item in items {
        let Sexpr::List(parts) = item else { continue };
        match head_of(item) {
            Some("symbol") => {
                if sexpr_symbol_refdes(item).is_some_and(|r| r.starts_with("#PWR")) {
                    if let Some(b) = power_glyph_value_bbox(item) {
                        occupied.push(b);
                    }
                }
            }
            Some("label" | "global_label") => {
                if let Some(b) = label_text_bbox(item) {
                    labels.push(b);
                }
            }
            Some("wire") => {
                if let Some(seg) = wire_seg_from_sexpr(parts) {
                    wires.push(seg);
                }
            }
            _ => {}
        }
    }
    (occupied, labels, wires)
}

/// DECORATION-phase pass: nudge visible Reference / Value property text
/// off mutual collisions (V13 parts 4 & 5 — host text ↔ host text, host
/// text ↔ power-glyph net-name text, and host text ↔ symbol-internal
/// pin-name/number text). Reads the already-emitted power
/// glyphs, labels and wires from `items`; computes host-symbol body
/// bboxes from `placement` + `library`. For each host Reference/Value
/// it keeps the default anchor when clean, else picks the first
/// candidate offset (see [`property_offset_candidates`]) that collides
/// with no occupied text bbox, no symbol body, no label, and no wire
/// interior. Rewrites only the property `(at …)` token — never the
/// symbol's own `(at …)` (the decoration contract: text may move,
/// symbols may not).
///
/// General by construction: drives entirely off the measured
/// `text_bbox` model and the candidate grid; zero fixture/refdes
/// special-casing.
fn nudge_property_text(items: &mut [Sexpr], placement: &Placement, library: &Library) {
    // ---- Build the fixed obstacle sets from already-emitted items. ----
    // Power-glyph net-name Value text (visible) seeds `occupied`; labels
    // and wires are their own classes.
    let (mut occupied, labels, wires) = emitted_text_obstacles(items);

    // Symbol body bboxes (world) for every visible host symbol — a
    // nudged property must not land on any body (V13.1 analogue).
    let bodies: Vec<TextBbox> = placement
        .elements
        .iter()
        .filter(|el| !el.is_power_source && !el.lib_id.starts_with("power:"))
        .map(|el| {
            let (ox, oy) = el.origin.to_mm();
            let world = library
                .lookup(&el.lib_id)
                .and_then(Symbol::body_bbox)
                .map_or(
                    spice_route::Bbox {
                        x0: ox - 2.54,
                        y0: oy - 2.54,
                        x1: ox + 2.54,
                        y1: oy + 2.54,
                    },
                    |local| body_bbox_to_world(local, ox, oy, el.orientation),
                );
            bbox_as_text(world)
        })
        .collect();

    // Visible symbol-internal pin-name / pin-number text bboxes (world)
    // for every host symbol — a nudged property must also clear these
    // (V13 part 5).
    let pin_texts = host_pin_text_bboxes(placement, library);

    // ---- Decide and rewrite each host symbol's Reference & Value. ----
    // Greedy, deterministic: iterate placement order; each chosen text
    // bbox becomes occupied for subsequent decisions.
    for el in &placement.elements {
        if el.is_power_source || el.lib_id.starts_with("power:") {
            continue;
        }
        let (ox, oy) = el.origin.to_mm();
        let value_text = el.value.as_deref().unwrap_or(&el.refdes);
        // (property key, text, default base_dy)
        for (key, text, base_dy) in [
            ("Reference", el.refdes.as_str(), -2.54_f64),
            ("Value", value_text, 2.54_f64),
        ] {
            let candidates = property_offset_candidates(base_dy);
            // Overlap *area* of a text bbox against every obstacle class
            // (counting wire-interior crossings as a unit penalty). Used
            // both as the accept test (== 0 → clear) and, when no
            // candidate is clear, as the least-overlap tie-breaker so a
            // dense symbol still gets the *best* available anchor rather
            // than silently keeping the colliding default.
            let overlap_cost = |b: TextBbox| -> f64 {
                let area = |o: &TextBbox| -> f64 {
                    let w = (b.x1.min(o.x1) - b.x0.max(o.x0)).max(0.0);
                    let h = (b.y1.min(o.y1) - b.y0.max(o.y0)).max(0.0);
                    w * h
                };
                let mut c: f64 = occupied.iter().map(area).sum();
                c += labels.iter().map(area).sum::<f64>();
                c += bodies.iter().map(area).sum::<f64>();
                c += pin_texts.iter().map(area).sum::<f64>();
                #[allow(clippy::cast_precision_loss)]
                let wire_hits = wires
                    .iter()
                    .filter(|&&(a, w)| text_crosses_segment(b, a, w))
                    .count() as f64;
                c += wire_hits * 100.0;
                c
            };
            let mut chosen = candidates[0];
            let mut chosen_bbox = {
                let (ax, ay) = property_anchor(ox, oy, el.orientation, chosen.0, chosen.1);
                text_bbox(text, (ax, ay), 0)
            };
            let mut best_cost = f64::INFINITY;
            for cand in &candidates {
                let (ax, ay) = property_anchor(ox, oy, el.orientation, cand.0, cand.1);
                let b = text_bbox(text, (ax, ay), 0);
                let cost = overlap_cost(b);
                if cost == 0.0 {
                    chosen = *cand;
                    chosen_bbox = b;
                    break;
                }
                if cost < best_cost {
                    best_cost = cost;
                    chosen = *cand;
                    chosen_bbox = b;
                }
            }
            occupied.push(chosen_bbox);
            // Rewrite the matching property's `(at …)` in `items`.
            let (ax, ay) = property_anchor(ox, oy, el.orientation, chosen.0, chosen.1);
            set_property_anchor(items, &el.refdes, key, ax, ay);
        }
    }
}

/// Centred text bbox (no `(justify …)` → KiCad centres the field
/// horizontally about its anchor). Power-glyph net-name `Value` text is
/// emitted without a justify, so it renders centred; modelling it
/// left-anchored would over-estimate its rightward reach. Height and
/// per-char advance match [`text_bbox`].
fn centered_text_bbox(text: &str, anchor: (f64, f64)) -> TextBbox {
    let size = 1.27_f64;
    #[allow(clippy::cast_precision_loss)]
    let chars = text.chars().count() as f64;
    let width = chars * 0.6 * size + 0.8 * size;
    let height = 1.4 * size;
    TextBbox {
        x0: anchor.0 - width / 2.0,
        y0: anchor.1 - height / 2.0,
        x1: anchor.0 + width / 2.0,
        y1: anchor.1 + height / 2.0,
    }
}

/// World-frame bboxes of every hierarchical-sheet port-NAME text already
/// serialised into `items` (a `(sheet … (pin "name" … (at x y rot)))`).
/// KiCad draws the port label reading outward from the pin; we model it
/// with the same left-anchored [`text_bbox`] used elsewhere (a
/// conservative over-estimate of its reach). Used as an obstacle class
/// the power-glyph value-text nudge must clear (V13 — issue [4]).
fn sheet_port_name_bboxes(items: &[Sexpr]) -> Vec<TextBbox> {
    let mut out = Vec::new();
    for item in items {
        if head_of(item) != Some("sheet") {
            continue;
        }
        let Sexpr::List(parts) = item else { continue };
        for p in parts {
            if head_of(p) != Some("pin") {
                continue;
            }
            let Sexpr::List(pin) = p else { continue };
            let name = match pin.get(1) {
                Some(Sexpr::QString(s) | Sexpr::Atom(s)) => s.as_str(),
                _ => continue,
            };
            if let Some((x, y, rot)) = sexpr_at(p) {
                out.push(text_bbox(name, (x, y), rot));
            }
        }
    }
    out
}

/// DECORATION-phase pass: nudge each `power:*` glyph's visible net-name
/// `Value` text off collisions with host symbol bodies, host
/// pin-name/number text, and hierarchical-sheet port-name text (V13 —
/// the power-glyph-text-vs-body class, issue [1]/[4] residuals).
///
/// The default anchor (set by the router on the glyph's *outward* side,
/// see `spice_route::rails::value_text_anchor`) is kept whenever clean —
/// so every glyph not crowded against a neighbour is byte-identical.
/// When it collides, the pass sweeps cardinal offsets about the glyph
/// anchor and picks the first clear one (least-overlap as a tie-break),
/// rewriting only the glyph's Value `(at …)`. The glyph body, its anchor
/// pin, and the symbol pose are never touched — strictly a text move
/// (decoration contract). General by construction: drives off the
/// measured `centered_text_bbox` model and a fixed candidate grid; no
/// fixture or refdes constants.
///
/// PWR_FLAG glyphs are skipped (their Value text is hidden).
fn nudge_power_glyph_value_text(items: &mut [Sexpr], placement: &Placement, library: &Library) {
    // Candidate offsets from the glyph anchor (default first → byte
    // identical when clean). The default keeps the value at whatever
    // outward offset the router chose; fallbacks sweep the four cardinal
    // directions at one and two glyph-clearing distances. All centred
    // horizontally on the offset point.
    const OFFSETS: &[(f64, f64)] = &[
        (0.0, 3.81),
        (0.0, -3.81),
        (-3.81, 0.0),
        (3.81, 0.0),
        (-5.08, 0.0),
        (5.08, 0.0),
        (0.0, 5.08),
        (0.0, -5.08),
    ];

    // Obstacle sets (fixed for the whole pass): host bodies, host
    // pin-text, sheet-port names. Power-glyph bodies are NOT obstacles
    // for each other's text (they sit on their own pins by design).
    let bodies: Vec<TextBbox> = placement
        .elements
        .iter()
        .filter(|el| !el.is_power_source && !el.lib_id.starts_with("power:"))
        .filter_map(|el| {
            let (ox, oy) = el.origin.to_mm();
            library
                .lookup(&el.lib_id)
                .and_then(Symbol::body_bbox)
                .map(|local| bbox_as_text(body_bbox_to_world(local, ox, oy, el.orientation)))
        })
        .collect();
    let pin_texts = host_pin_text_bboxes(placement, library);
    let port_names = sheet_port_name_bboxes(items);

    // Anchors already chosen by this pass become obstacles for later
    // glyphs so two glyph labels never stack.
    let mut chosen_text: Vec<TextBbox> = Vec::new();

    for item in items.iter_mut() {
        if head_of(item) != Some("symbol") {
            continue;
        }
        let Some(refdes) = sexpr_symbol_refdes(item) else {
            continue;
        };
        if !refdes.starts_with("#PWR") {
            continue;
        }
        // Glyph anchor + current Value text.
        let Some((gx, gy, _)) = sexpr_at(item) else {
            continue;
        };
        let Some(value) = power_glyph_value_text(item) else {
            continue; // hidden / absent → nothing to place
        };
        // Current default Value anchor (relative to the glyph anchor).
        let Some((val_x, val_y, _)) = power_glyph_value_at(item) else {
            continue;
        };
        let default_off = (val_x - gx, val_y - gy);

        let overlap_cost = |b: &TextBbox| -> f64 {
            let area = |o: &TextBbox| -> f64 {
                let w = (b.x1.min(o.x1) - b.x0.max(o.x0)).max(0.0);
                let h = (b.y1.min(o.y1) - b.y0.max(o.y0)).max(0.0);
                w * h
            };
            bodies.iter().map(area).sum::<f64>()
                + pin_texts.iter().map(area).sum::<f64>()
                + port_names.iter().map(area).sum::<f64>()
                + chosen_text.iter().map(area).sum::<f64>()
        };

        // Default first, then the candidate sweep.
        let mut best_off = default_off;
        let mut best_bbox = centered_text_bbox(&value, (gx + default_off.0, gy + default_off.1));
        let mut best_cost = overlap_cost(&best_bbox);
        if best_cost > 0.0 {
            for &(dx, dy) in OFFSETS {
                let bb = centered_text_bbox(&value, (gx + dx, gy + dy));
                let cost = overlap_cost(&bb);
                if cost < best_cost {
                    best_cost = cost;
                    best_off = (dx, dy);
                    best_bbox = bb;
                    if cost == 0.0 {
                        break;
                    }
                }
            }
        }
        chosen_text.push(best_bbox);
        let (ax, ay) = (gx + best_off.0, gy + best_off.1);
        set_property_anchor_in(item, "Value", ax, ay);
    }
}

/// `(at x y rot)` of a power-glyph's visible `Value` property, or `None`
/// when hidden / absent.
fn power_glyph_value_at(sym: &Sexpr) -> Option<(f64, f64, u16)> {
    let Sexpr::List(items) = sym else {
        return None;
    };
    for it in items {
        if let Sexpr::List(p) = it
            && matches!(p.first(), Some(Sexpr::Atom(a)) if a == "property")
            && matches!(p.get(1), Some(Sexpr::QString(k)) if k == "Value")
        {
            if sexpr_property_hidden(it) {
                return None;
            }
            return sexpr_at(it);
        }
    }
    None
}

/// The visible `Value` property string of a power-glyph `(symbol …)`.
fn power_glyph_value_text(sym: &Sexpr) -> Option<String> {
    let Sexpr::List(items) = sym else {
        return None;
    };
    for it in items {
        if let Sexpr::List(p) = it
            && matches!(p.first(), Some(Sexpr::Atom(a)) if a == "property")
            && matches!(p.get(1), Some(Sexpr::QString(k)) if k == "Value")
        {
            if sexpr_property_hidden(it) {
                return None;
            }
            if let Some(Sexpr::QString(v)) = p.get(2) {
                return Some(v.clone());
            }
        }
    }
    None
}

/// Rewrite the `(at x y …)` of the named property within a single
/// `(symbol …)` sexpr, preserving the rotation token. Used by the
/// power-glyph value-text nudge (the glyph is addressed by `&mut Sexpr`,
/// not by refdes scan, since `#PWR` refdes are not globally unique under
/// the scan ordering this pass relies on).
fn set_property_anchor_in(sym: &mut Sexpr, key: &str, x: f64, y: f64) {
    let Sexpr::List(parts) = sym else { return };
    for it in parts.iter_mut() {
        let Sexpr::List(p) = it else { continue };
        let is_target = matches!(p.first(), Some(Sexpr::Atom(a)) if a == "property")
            && matches!(p.get(1), Some(Sexpr::QString(k)) if k == key);
        if !is_target {
            continue;
        }
        for sub in p.iter_mut() {
            if head_of(sub) == Some("at") {
                if let Sexpr::List(a) = sub {
                    let rot = a.get(3).cloned();
                    let mut new_at = vec![
                        atom("at"),
                        atom(&format!("{x:.2}")),
                        atom(&format!("{y:.2}")),
                    ];
                    if let Some(r) = rot {
                        new_at.push(r);
                    }
                    *sub = Sexpr::List(new_at);
                }
            }
        }
    }
}

/// Head symbol of a `Sexpr::List`, if any.
fn head_of(s: &Sexpr) -> Option<&str> {
    match s {
        Sexpr::List(items) => sexpr_head(items),
        _ => None,
    }
}

/// The `Reference` property string of a `(symbol …)` sexpr.
fn sexpr_symbol_refdes(sym: &Sexpr) -> Option<&str> {
    let Sexpr::List(items) = sym else {
        return None;
    };
    for it in items {
        if let Sexpr::List(p) = it {
            if matches!(p.first(), Some(Sexpr::Atom(a)) if a == "property")
                && matches!(p.get(1), Some(Sexpr::QString(k)) if k == "Reference")
            {
                if let Some(Sexpr::QString(v)) = p.get(2) {
                    return Some(v.as_str());
                }
            }
        }
    }
    None
}

/// Visible net-name `Value` text bbox of a power-glyph `(symbol …)`.
/// Returns `None` if the Value property is hidden.
fn power_glyph_value_bbox(sym: &Sexpr) -> Option<TextBbox> {
    let Sexpr::List(items) = sym else {
        return None;
    };
    for it in items {
        if let Sexpr::List(p) = it {
            if matches!(p.first(), Some(Sexpr::Atom(a)) if a == "property")
                && matches!(p.get(1), Some(Sexpr::QString(k)) if k == "Value")
            {
                if sexpr_property_hidden(it) {
                    return None;
                }
                let Some(Sexpr::QString(text)) = p.get(2) else {
                    return None;
                };
                let (x, y, rot) = sexpr_at(it)?;
                return Some(text_bbox(text, (x, y), rot));
            }
        }
    }
    None
}

/// True if a `(property …)` sexpr is hidden via `(effects … (hide yes))`.
fn sexpr_property_hidden(prop: &Sexpr) -> bool {
    let Sexpr::List(items) = prop else {
        return false;
    };
    for it in items {
        if head_of(it) == Some("effects") {
            if let Sexpr::List(eff) = it {
                for e in eff {
                    if head_of(e) == Some("hide") {
                        if let Sexpr::List(h) = e {
                            return !matches!(h.get(1), Some(Sexpr::Atom(a)) if a == "no");
                        }
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Extract `(at x y rot)` from a sexpr that has one as a direct child.
fn sexpr_at(node: &Sexpr) -> Option<(f64, f64, u16)> {
    let Sexpr::List(items) = node else {
        return None;
    };
    for it in items {
        if head_of(it) == Some("at") {
            if let Sexpr::List(a) = it {
                let x = sexpr_num(a.get(1)?)?;
                let y = sexpr_num(a.get(2)?)?;
                let rot = a.get(3).and_then(sexpr_num).unwrap_or(0.0);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let r = ((rot.round() as i64).rem_euclid(360)) as u16;
                return Some((x, y, r));
            }
        }
    }
    None
}

/// Parse a numeric atom (the emitter writes coords as `Atom`).
fn sexpr_num(s: &Sexpr) -> Option<f64> {
    match s {
        Sexpr::Atom(a) => a.parse().ok(),
        _ => None,
    }
}

/// Text bbox of a `(label …)` / `(global_label …)` sexpr (name at idx 1).
fn label_text_bbox(node: &Sexpr) -> Option<TextBbox> {
    let Sexpr::List(items) = node else {
        return None;
    };
    let name = match items.get(1) {
        Some(Sexpr::QString(s) | Sexpr::Atom(s)) => s.as_str(),
        _ => return None,
    };
    let (x, y, rot) = sexpr_at(node)?;
    Some(text_bbox(name, (x, y), rot))
}

/// Extract `((x1,y1),(x2,y2))` from a `(wire (pts (xy …) (xy …)))`
/// emitter `Sexpr`.
fn wire_seg_from_sexpr(parts: &[Sexpr]) -> Option<((f64, f64), (f64, f64))> {
    let pts = parts.iter().find_map(|node| match node {
        Sexpr::List(inner) if matches!(inner.first(), Some(Sexpr::Atom(a)) if a == "pts") => {
            Some(inner)
        }
        _ => None,
    })?;
    let mut coords: Vec<(f64, f64)> = Vec::new();
    for xy in pts.iter().skip(1) {
        if let Sexpr::List(inner) = xy {
            if matches!(inner.first(), Some(Sexpr::Atom(a)) if a == "xy") {
                let x = sexpr_num(inner.get(1)?)?;
                let y = sexpr_num(inner.get(2)?)?;
                coords.push((x, y));
            }
        }
    }
    (coords.len() >= 2).then(|| (coords[0], coords[1]))
}

/// Rewrite the `(at x y rot)` of the named property (`Reference` /
/// `Value`) on the host symbol whose Reference equals `refdes`. Only
/// touches the property's anchor — never the symbol's own `(at …)`.
fn set_property_anchor(items: &mut [Sexpr], refdes: &str, key: &str, x: f64, y: f64) {
    for item in items.iter_mut() {
        if head_of(item) != Some("symbol") {
            continue;
        }
        if sexpr_symbol_refdes(item) != Some(refdes) {
            continue;
        }
        let Sexpr::List(parts) = item else { continue };
        for it in parts.iter_mut() {
            let Sexpr::List(p) = it else { continue };
            let is_target = matches!(p.first(), Some(Sexpr::Atom(a)) if a == "property")
                && matches!(p.get(1), Some(Sexpr::QString(k)) if k == key);
            if !is_target {
                continue;
            }
            for sub in p.iter_mut() {
                if head_of(sub) == Some("at") {
                    let rot = match sub {
                        Sexpr::List(a) => a.get(3).cloned(),
                        _ => None,
                    };
                    let mut new_at =
                        vec![atom("at"), atom(&format_coord(x)), atom(&format_coord(y))];
                    new_at.push(rot.unwrap_or_else(|| atom("0")));
                    *sub = Sexpr::List(new_at);
                }
            }
        }
        return;
    }
}

/// Pick a label rotation that does not collide with any property-text
/// bbox, preferring the body-clearing `preferred` rotation. Falls back
/// through the perpendicular rotations (±90) and finally 180° before
/// giving up and returning `preferred` (a property overlap is a
/// quality defect, never a correctness one, so we never fail to label).
fn label_rotation_avoiding(
    text: &str,
    anchor: (f64, f64),
    preferred: u16,
    props: &[TextBbox],
) -> u16 {
    let collides = |rot: u16| {
        let b = text_bbox(text, anchor, rot);
        props.iter().any(|p| b.intersects(*p))
    };
    // Order: preferred first (keeps the existing body-clearing choice
    // and every non-colliding fixture byte-identical), then the two
    // perpendiculars, then the opposite.
    for cand in [
        preferred,
        (preferred + 90) % 360,
        (preferred + 270) % 360,
        (preferred + 180) % 360,
    ] {
        if !collides(cand) {
            return cand;
        }
    }
    preferred
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

/// V15 — translate the entire emitted sheet so its content bounding box
/// top-left corner lands at [`PAGE_MARGIN_MM`].
///
/// This is the *single* place the placed layout is shifted into the
/// page's usable area. It is a uniform, grid-snapped affine translation
/// of every instance-section coordinate — symbol/property `(at …)`, wire
/// `(xy …)`, power-glyph `(at …)`, junctions, labels, hierarchical
/// labels, no_connects, and `(sheet …)` blocks (their `(at …)` and pin
/// `(at …)`, but **not** `(size …)`). Because it operates on the final
/// `Sexpr` tree it cannot miss a category that other passes generate from
/// constants (hierarchical labels at `-25.4`, sheet blocks, …).
///
/// `(lib_symbols …)` is deliberately excluded from BOTH passes: its
/// `(at …)`/`(xy …)` are symbol-DEFINITION-local geometry that must not
/// move with the instance layout.
///
/// Hidden `(property … (hide yes))` instance-section nodes are handled
/// asymmetrically: they are EXCLUDED from the min-bbox collection (a
/// hidden Sim/Footprint/Datasheet prop parked at `(0 0 0)` must not drag
/// the content bbox toward the origin and skew the margin) but are still
/// TRANSLATED, so a hidden prop carrying a real page coordinate — e.g. a
/// power glyph's `#PWRn` Reference, emitted glyph-relative in
/// `spice-route`'s `rails.rs` — rides the same uniform shift as its
/// symbol and stays co-located with it. Net rule: hidden instance props
/// are translated but do not vote on the min.
///
/// Uniform translation only: no scaling, no per-element moves, so every
/// relative-geometry invariant (V5–V7, V10–V14) is preserved by
/// construction. The offset is an integer number of grid cells, so all
/// coordinates remain grid-snapped.
fn translate_into_page(root: &mut Sexpr) {
    let mut min = (f64::INFINITY, f64::INFINITY);
    collect_translatable_min(root, &mut min);
    if !min.0.is_finite() || !min.1.is_finite() {
        // No content coordinates (e.g. an empty sheet) — nothing to do.
        return;
    }
    // Snap the offset to an integer number of grid cells so the result
    // stays on the KiCad grid. Round the per-axis shift to the nearest
    // cell; the content top-left then lands within one cell of the
    // margin.
    let step = 1.27_f64;
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let off_cells_x = ((PAGE_MARGIN_MM - min.0) / step).round() as i64;
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let off_cells_y = ((PAGE_MARGIN_MM - min.1) / step).round() as i64;
    #[allow(clippy::cast_precision_loss)]
    let dx = off_cells_x as f64 * step;
    #[allow(clippy::cast_precision_loss)]
    let dy = off_cells_y as f64 * step;
    apply_translation(root, dx, dy);
}

/// Recurse, folding the minimum X/Y over every translatable coordinate
/// node (see [`translate_into_page`] for the exclusion rules).
fn collect_translatable_min(node: &Sexpr, min: &mut (f64, f64)) {
    let Sexpr::List(items) = node else {
        return;
    };
    match sexpr_head(items) {
        Some("lib_symbols") => return,
        // Hidden instance props (e.g. a prop parked at `(0 0 0)`) must not
        // vote on the content min — they are still translated by
        // `apply_translation`, just excluded from the bbox here.
        Some("property") if property_node_hidden(items) => return,
        Some("at" | "xy") => {
            if let Some((x, y)) = coord_pair(items) {
                if x < min.0 {
                    min.0 = x;
                }
                if y < min.1 {
                    min.1 = y;
                }
            }
            return;
        }
        _ => {}
    }
    for child in items {
        collect_translatable_min(child, min);
    }
}

/// Recurse, adding `(dx, dy)` to every translatable coordinate node.
/// `(lib_symbols …)` is excluded — its geometry is definition-local. A
/// hidden property anchored at exactly `(0, 0)` is also skipped: that is
/// KiCad's "unplaced placeholder" anchor (Sim/Footprint/Datasheet instance
/// props the emitter parks at the origin), not a page coordinate;
/// translating it would strand it at `(dx, dy)`, possibly off the top/left
/// margin. Every OTHER hidden instance prop — notably a power glyph's
/// `#PWRn` Reference, emitted glyph-relative at a real coordinate — IS
/// translated, so it keeps the same offset as its symbol rather than being
/// stranded at its pre-translation coord.
fn apply_translation(node: &mut Sexpr, dx: f64, dy: f64) {
    let Sexpr::List(items) = node else {
        return;
    };
    match sexpr_head(items) {
        Some("lib_symbols") => return,
        Some("property") if property_node_hidden(items) && property_anchor_at_origin(items) => {
            return;
        }
        Some("at" | "xy") => {
            // items[0] = head, items[1] = x, items[2] = y, [3..] = rot etc.
            if let Some(Sexpr::Atom(s)) = items.get(1) {
                if let Ok(x) = s.parse::<f64>() {
                    items[1] = Sexpr::Atom(format_coord(x + dx));
                }
            }
            if let Some(Sexpr::Atom(s)) = items.get(2) {
                if let Ok(y) = s.parse::<f64>() {
                    items[2] = Sexpr::Atom(format_coord(y + dy));
                }
            }
            return;
        }
        _ => {}
    }
    for child in items.iter_mut() {
        apply_translation(child, dx, dy);
    }
}

/// Head symbol of an s-expr list, if its first element is an atom.
fn sexpr_head(items: &[Sexpr]) -> Option<&str> {
    match items.first() {
        Some(Sexpr::Atom(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// The first two scalar children of an `(at …)` / `(xy …)` node parsed as
/// `(x, y)` millimetre coordinates.
fn coord_pair(items: &[Sexpr]) -> Option<(f64, f64)> {
    let x = match items.get(1)? {
        Sexpr::Atom(s) => s.parse::<f64>().ok()?,
        _ => return None,
    };
    let y = match items.get(2)? {
        Sexpr::Atom(s) => s.parse::<f64>().ok()?,
        _ => return None,
    };
    Some((x, y))
}

/// True when a `(property …)` list carries `(effects … (hide yes))`.
/// True when a `(property …)` node's own `(at x y …)` anchor is the
/// origin `(0, 0)` — KiCad's "unplaced placeholder" convention. Such
/// anchors carry no meaningful page coordinate and must not be translated
/// (doing so would move them to `(dx, dy)`, off the content area).
fn property_anchor_at_origin(items: &[Sexpr]) -> bool {
    items.iter().any(|child| {
        let Sexpr::List(at) = child else {
            return false;
        };
        if sexpr_head(at) != Some("at") {
            return false;
        }
        let zero = |i: usize| {
            matches!(at.get(i), Some(Sexpr::Atom(s)) if s.parse::<f64>().is_ok_and(|v| v == 0.0))
        };
        zero(1) && zero(2)
    })
}

fn property_node_hidden(items: &[Sexpr]) -> bool {
    items.iter().any(|child| {
        let Sexpr::List(effects) = child else {
            return false;
        };
        if sexpr_head(effects) != Some("effects") {
            return false;
        }
        effects.iter().any(|e| {
            let Sexpr::List(hide) = e else {
                return false;
            };
            sexpr_head(hide) == Some("hide")
                && matches!(hide.get(1), Some(Sexpr::Atom(v)) if v == "yes")
        })
    })
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
                is_power_source: false,
                power_rail: None,
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
        // V15 translates the placement into the page's usable area, so
        // the origin no longer sits at (0 0 0). The single resistor's
        // symbol `(at …)` lands at the page margin (rotation 0 kept).
        assert!(
            out.contains(&format!(
                "(at {} {} 0)",
                format_coord(PAGE_MARGIN_MM),
                format_coord(PAGE_MARGIN_MM + 2.54)
            )),
            "missing margin-translated origin in output:\n{out}"
        );
        // No coordinate may be negative after the V15 translation.
        assert!(
            !out.contains("(at -"),
            "negative origin survived V15 translation:\n{out}"
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
                    is_power_source: false,
                    power_rail: None,
                },
                PlacedElement {
                    refdes: "R2".into(),
                    lib_id: "Device:R".into(),
                    origin: GridPoint::new(10, 0),
                    orientation: Orientation::IDENTITY,
                    nodes: Vec::new(),
                    pin_mapping: Vec::new(),
                    value: None,
                    is_power_source: false,
                    power_rail: None,
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
                is_power_source: false,
                power_rail: None,
            }],
        };
        let out = emit(&placement, &fixture_library()).expect("emit");
        // V15 translates absolute coordinates into the page area, but the
        // rotation token (and the relative geometry) is preserved: the
        // symbol's `(at …)` still carries the 90° rotation, and no
        // coordinate is negative.
        let sym_at = out
            .split("(symbol")
            .nth(1)
            .and_then(|s| s.split("(at ").nth(1))
            .and_then(|s| s.split(')').next())
            .expect("symbol (at …)");
        assert!(
            sym_at.trim_end().ends_with(" 90"),
            "rotation 90 not preserved through V15 translation; got `(at {sym_at})`:\n{out}"
        );
        assert!(
            !out.contains("(at -"),
            "negative origin survived V15 translation:\n{out}"
        );
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
                is_power_source: false,
                power_rail: None,
            }],
        };
        let out = emit(&placement, &fixture_library()).expect("emit");
        assert!(out.contains("(mirror y)"), "got:\n{out}");
    }
}
