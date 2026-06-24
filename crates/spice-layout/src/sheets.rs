//! Structural placement of hierarchical-sheet instances (V6).
//!
//! A default-path `.subckt` instance (`X<n>` with no `*@symbol`
//! override) is lowered by the emitter to a KiCad `(sheet …)` block.
//! Historically the emitter pinned that block at a fixed off-circuit
//! page coordinate (`origin_x = 200`, stacked by index), which left
//! ~180 mm trunk wires running from the circuit to the sheet pins.
//!
//! This module makes the sheet a first-class *placeable unit*: it is
//! positioned adjacent to the real symbols it shares signal nets with,
//! so its port trunk wires are bounded like any other net. Power and
//! ground ports carry no trunk wire (they become `power:*` glyphs at the
//! sheet pin, V10), so only Signal-class ports drive the attachment
//! target.
//!
//! The sheet does *not* flow through the orientation / SA passes (those
//! index real symbol pin geometry); it has no rotation and a fixed
//! rectangular body. Instead its world origin is computed here directly
//! from the *final* placement of its neighbours, then de-overlapped
//! against every real symbol body and every other sheet rectangle.

use kicad_symbols::Library;
use spice_policy::CheckedNetlist;
use spice_resolve::SheetInstance;

use crate::Placement;
use crate::net_class::{NetClass, classify_nets};

/// Grid step in millimetres (KiCad schematic grid, 50 mil).
const STEP_MM: f64 = 1.27;

/// Sheet body width in millimetres (matches the emitter's `(size)` X).
const SHEET_W_MM: f64 = 30.48;

/// Vertical pitch between sheet port pins (matches the emitter).
const SHEET_PIN_PITCH_MM: f64 = 5.08;

/// Top/bottom padding of the sheet body above the first / below the last
/// port pin (matches the emitter's `height` computation).
const SHEET_PIN_PAD_MM: f64 = 5.08;

/// Outward reach (mm) of a `power:*` glyph that the emitter hangs off a
/// sheet *port pin* (left edge): the glyph anchor is offset
/// `SHEET_EDGE_GLYPH_OFFSET_CELLS` (= 2) cells outward and its body
/// extends a further cell, so the glyph zone reaches three grid cells to
/// the LEFT of the sheet's left edge. The sheet's de-overlap footprint
/// must include this zone so a power/ground port glyph never lands on a
/// neighbouring symbol body (the RF-vs-glyph overlap the V6/V13 fix
/// targets). Mirrors `spice_route::rails::SHEET_EDGE_GLYPH_OFFSET_CELLS`
/// plus one body cell; kept in lock-step here (the two crates do not
/// share a constant, but the geometry is the same grid).
const SHEET_GLYPH_REACH_MM: f64 = 3.0 * STEP_MM;

/// Trailing margin (mm) the emitter's text renderer adds past the bare
/// per-character advance of a value string (~0.8 × the 1.27 mm text
/// size). Folded into the value-text obstacle reach so the sheet
/// de-overlap clears a glyph from the *rendered* text box (the V13
/// verifier's `text_bbox` model), not merely a tighter advance estimate.
const VALUE_TEXT_TRAIL_MM: f64 = 0.8 * STEP_MM;

/// A sheet's computed world rectangle, in millimetres.
#[derive(Debug, Clone, Copy)]
struct Rect {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Rect {
    fn overlaps(&self, other: &Rect) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }
}

/// Snap a millimetre value to the nearest grid line.
fn snap(v: f64) -> f64 {
    (v / STEP_MM).round() * STEP_MM
}

/// World height of a sheet body with `port_count` ports (matches the
/// emitter's `height`).
fn sheet_height(port_count: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let pins = (port_count as f64).max(2.0);
    pins * SHEET_PIN_PITCH_MM + SHEET_PIN_PAD_MM
}

/// World-pin position of one real (non-power-rail) placed element on a
/// given net, in the emitter's coordinate convention
/// (`wy = origin_y - pin_y`, matching `collect_net_pins`).
fn element_pins_on_nets<'a>(
    placement: &'a Placement,
    library: &'a Library,
) -> impl Iterator<Item = (&'a str, f64, f64)> + 'a {
    placement.elements.iter().flat_map(move |el| {
        let mut out: Vec<(&str, f64, f64)> = Vec::new();
        if el.is_power_source {
            return out.into_iter();
        }
        let Some(symbol) = library.lookup(&el.lib_id) else {
            return out.into_iter();
        };
        let pins = symbol.pins_in(el.orientation);
        let (ox, oy) = el.origin.to_mm();
        for (node, kicad_pin) in el.nodes.iter().zip(el.pin_mapping.iter()) {
            if let Some(pin) = pins.iter().find(|p| &p.number == kicad_pin) {
                out.push((node.as_str(), ox + pin.x, oy - pin.y));
            }
        }
        out.into_iter()
    })
}

/// World-frame right reach (mm) of the value text the emitter renders on
/// a symbol's +X side: the text is left-justified at `VALUE_TEXT_OFFSET_MM`
/// from the origin and advances `VALUE_CHAR_MM` per character. Returns the
/// absolute world X the text reaches (origin-relative reach added to `ox`),
/// or `ox` when the element has no value text. Mirrors the +X value-text
/// pad `world_extent` uses for align-cluster spacing (`lib.rs`).
fn value_text_right_x(el: &crate::PlacedElement, ox: f64) -> f64 {
    let Some(v) = el.value.as_deref() else {
        return ox;
    };
    let chars = v.chars().count();
    if chars == 0 {
        return ox;
    }
    // Match the emitter's rendered text right reach: the text is
    // left-justified `VALUE_TEXT_OFFSET_MM` from the origin, advances
    // `VALUE_CHAR_MM` per character, plus the renderer's trailing margin.
    #[allow(clippy::cast_precision_loss)]
    let reach =
        crate::VALUE_TEXT_OFFSET_MM + (chars as f64) * crate::VALUE_CHAR_MM + VALUE_TEXT_TRAIL_MM;
    ox + reach
}

/// Body bounding boxes (world, mm) of every real placed symbol, each
/// widened on its +X side to cover the value text the emitter renders
/// there. Used as overlap obstacles for sheet placement. Symbols without
/// a drawable body contribute a small placeholder box around their
/// origin.
///
/// The +X value-text extension is load-bearing for sheet placement: the
/// sheet's left-edge port pins hang `power:*` glyphs into the strip to
/// their left, and the de-overlap loop only nudges the sheet RIGHT until
/// that glyph zone clears every obstacle. Modelling each neighbour's
/// body alone (ignoring its value text, which reaches right toward the
/// sheet) let the sheet stop while a glyph still speared the neighbour's
/// rendered value (e.g. RF's "10k", V13). Folding the text reach into the
/// obstacle's `x1` pushes the sheet right until both clear. The loop only
/// ever moves the sheet, never a real symbol, so this is purely additive
/// clearance.
fn symbol_obstacles(placement: &Placement, library: &Library) -> Vec<Rect> {
    let mut out = Vec::new();
    for el in &placement.elements {
        if el.is_power_source {
            continue;
        }
        let (ox, oy) = el.origin.to_mm();
        let mut rect = library
            .lookup(&el.lib_id)
            .and_then(kicad_symbols::Symbol::body_bbox)
            .map_or_else(
                || Rect {
                    x0: ox - STEP_MM,
                    y0: oy - STEP_MM,
                    x1: ox + STEP_MM,
                    y1: oy + STEP_MM,
                },
                |b| {
                    // Local bbox in symbol frame; KiCad negates Y on load
                    // (same convention as pins). Take the min/max after
                    // applying origin and the Y flip.
                    let ys = [oy - b.y0, oy - b.y1];
                    let xs = [ox + b.x0, ox + b.x1];
                    Rect {
                        x0: xs[0].min(xs[1]),
                        y0: ys[0].min(ys[1]),
                        x1: xs[0].max(xs[1]),
                        y1: ys[0].max(ys[1]),
                    }
                },
            );
        // Widen the +X edge to cover the value text rendered there.
        rect.x1 = rect.x1.max(value_text_right_x(el, ox));
        out.push(rect);
    }
    out
}

/// Compute a world-origin (millimetres) for every sheet instance such
/// that each sheet sits adjacent to the real symbols it shares Signal
/// nets with, with no overlap against any real symbol body or another
/// already-placed sheet.
///
/// Returns `(refdes, (origin_x_mm, origin_y_mm))` in `sheets` order. The
/// origin is the sheet's top-left `(at …)` — the same anchor the emitter
/// derives its pin / property coordinates from. Coordinates are
/// grid-snapped and pre-page-translation (the emitter's
/// `translate_into_page` shifts the whole sheet uniformly afterwards).
#[must_use]
pub fn place_sheets(
    checked: &CheckedNetlist,
    placement: &Placement,
    library: &Library,
    sheets: &[SheetInstance],
) -> Vec<(String, (f64, f64))> {
    let classes = classify_nets(checked);

    // World pins of real elements, grouped by net. Used to compute each
    // sheet's attachment centroid.
    let mut net_pins: std::collections::HashMap<String, Vec<(f64, f64)>> =
        std::collections::HashMap::new();
    for (net, x, y) in element_pins_on_nets(placement, library) {
        net_pins.entry(net.to_string()).or_default().push((x, y));
    }

    // Circuit bounding box (real symbol origins) — the fallback anchor
    // when a sheet shares no Signal net with any real element (e.g. a
    // sheet wired only to power rails). We drop it just to the right of
    // the circuit rather than at a fixed page coordinate.
    let mut circuit_top = f64::INFINITY;
    let mut circuit_right = f64::NEG_INFINITY;
    let mut circuit_bot = f64::NEG_INFINITY;
    let mut any_element = false;
    for el in &placement.elements {
        let (ox, oy) = el.origin.to_mm();
        circuit_top = circuit_top.min(oy);
        circuit_right = circuit_right.max(ox);
        circuit_bot = circuit_bot.max(oy);
        any_element = true;
    }
    if !any_element {
        // No real elements at all; anchor at the origin.
        circuit_top = 0.0;
        circuit_right = 0.0;
        circuit_bot = 0.0;
    }

    let mut occupied: Vec<Rect> = symbol_obstacles(placement, library);
    let mut out: Vec<(String, (f64, f64))> = Vec::with_capacity(sheets.len());

    for sheet in sheets {
        let port_count = sheet.nodes.len();
        let height = sheet_height(port_count);

        // Attachment target: centroid of every real pin on a Signal net
        // this sheet touches. Power/Ground ports carry no trunk wire
        // (they become glyphs), so they don't pull the sheet.
        let mut tx = 0.0_f64;
        let mut ty = 0.0_f64;
        let mut count = 0usize;
        for net in &sheet.nodes {
            if classes
                .get(net.as_str())
                .copied()
                .unwrap_or(NetClass::Signal)
                != NetClass::Signal
            {
                continue;
            }
            if let Some(pins) = net_pins.get(net) {
                for &(px, py) in pins {
                    tx += px;
                    ty += py;
                    count += 1;
                }
            }
        }

        // The sheet's port pins run down its LEFT edge, so the natural
        // anchor places that left edge at (or just right of) the
        // attachment centroid. The sheet origin is its top-left corner;
        // the first port pin sits at `origin_y + SHEET_PIN_PAD_MM`, so
        // we centre the pin column on the target Y.
        #[allow(clippy::cast_precision_loss)]
        let pin_span = (port_count.max(1) - 1) as f64 * SHEET_PIN_PITCH_MM;
        let (target_x, target_y) = if count > 0 {
            #[allow(clippy::cast_precision_loss)]
            let c = count as f64;
            // Place the sheet to the right of the centroid so its
            // left-edge pins face back toward the circuit.
            (tx / c + SHEET_PIN_PITCH_MM, ty / c)
        } else {
            // No signal neighbours: drop to the right of the circuit,
            // vertically centred on it.
            (
                circuit_right + SHEET_W_MM,
                f64::midpoint(circuit_top, circuit_bot),
            )
        };

        let mut origin_x = snap(target_x);
        let base_origin_y = snap(target_y - SHEET_PIN_PAD_MM - pin_span / 2.0);

        // De-overlap: nudge rightward by a grid step until the sheet
        // rectangle clears every real symbol body and previously-placed
        // sheet. Rightward keeps the left-edge pins as close to the
        // attachment target as the obstacle field allows (sheets are
        // downstream sinks in the usual left-to-right flow).
        let mut origin_y = base_origin_y;
        let mut guard = 0;
        // De-overlap against a *footprint* rectangle that extends the
        // sheet body leftward by the power-glyph reach. The sheet's
        // left-edge port pins hang `power:*` glyphs that far outward
        // (see `SHEET_GLYPH_REACH_MM`); folding that zone into the
        // obstacle test pushes the sheet right until both the body and
        // its glyphs clear every neighbouring symbol — without it a
        // glyph spears an adjacent body (RF, V6/V13).
        loop {
            let footprint = Rect {
                x0: origin_x - SHEET_GLYPH_REACH_MM,
                y0: origin_y,
                x1: origin_x + SHEET_W_MM,
                y1: origin_y + height,
            };
            if !occupied.iter().any(|o| o.overlaps(&footprint)) {
                occupied.push(footprint);
                break;
            }
            origin_x = snap(origin_x + STEP_MM);
            guard += 1;
            if guard > 4096 {
                // Pathological obstacle field — give up nudging and
                // stack below the circuit so we never loop forever.
                #[allow(clippy::cast_precision_loss)]
                let stack = out.len() as f64 + 1.0;
                origin_x = snap(circuit_right + SHEET_W_MM);
                origin_y = snap(circuit_bot + height * stack);
                let footprint = Rect {
                    x0: origin_x - SHEET_GLYPH_REACH_MM,
                    y0: origin_y,
                    x1: origin_x + SHEET_W_MM,
                    y1: origin_y + height,
                };
                occupied.push(footprint);
                break;
            }
        }

        out.push((sheet.refdes.clone(), (origin_x, origin_y)));
    }

    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use kicad_symbols::Library;
    use spice_diagnostics::FileId;
    use spice_policy::check;

    use super::*;
    use crate::{LayoutOptions, place_with};

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

    /// `(refdes, (origin_x_mm, origin_y_mm))` of each placed sheet.
    type SheetOrigin = (String, (f64, f64));
    /// Circuit bounding box `(x0, y0, x1, y1)` in mm.
    type Bbox = (f64, f64, f64, f64);

    /// Parse → resolve → check, then place real elements and the sheets.
    /// Returns `(refdes → sheet origin mm)` plus the real-element circuit
    /// bounding box for proximity assertions.
    fn place(src: &str) -> (Vec<SheetOrigin>, Bbox) {
        let parsed = spice_parser::parse(src, FileId(0))
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let sheet_instances = resolved.sheet_instances.clone();
        // Mirror main.rs: strip the sheets out before placing real elements.
        let top = spice_resolve::ResolvedNetlist {
            elements: resolved.elements,
            align: resolved.align,
            place: resolved.place,
            subckts: resolved.subckts,
            sheet_instances: Vec::new(),
        };
        let (checked, _w) = check(top).expect("policy check failed");
        let placement = place_with(
            checked.clone(),
            fixture_library(),
            &LayoutOptions::default(),
        )
        .expect("place");
        let origins = place_sheets(&checked, &placement, fixture_library(), &sheet_instances);

        let mut bbox = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        for el in &placement.elements {
            let (ox, oy) = el.origin.to_mm();
            bbox.0 = bbox.0.min(ox);
            bbox.1 = bbox.1.min(oy);
            bbox.2 = bbox.2.max(ox);
            bbox.3 = bbox.3.max(oy);
        }
        (origins, bbox)
    }

    /// Proximity slack (mm) around the circuit bbox for the "near"
    /// assertions — a sheet flung to the legacy x≈200 mm fails by far.
    const NEAR_M: f64 = 40.0;

    /// A single-sheet subckt instance lands adjacent to the circuit, not
    /// at the legacy off-circuit x≈200 mm coordinate.
    #[test]
    fn single_sheet_lands_near_circuit() {
        let src = "\
test
.subckt SUB a b
R1 a b 1k
.ends
R0 in n1 1k
X1 n1 out SUB
RL out 0 1k
.end
";
        let (origins, (x0, y0, x1, y1)) = place(src);
        assert_eq!(origins.len(), 1, "expected one sheet");
        let (_, (sx, sy)) = &origins[0];
        assert!(
            *sx >= x0 - NEAR_M && *sx <= x1 + NEAR_M && *sy >= y0 - NEAR_M && *sy <= y1 + NEAR_M,
            "sheet at ({sx:.2},{sy:.2}) not near circuit bbox [{x0:.2}..{x1:.2}]x[{y0:.2}..{y1:.2}]",
        );
        assert!(*sx < 150.0, "sheet x={sx:.2} still flung far right");
    }

    /// Multiple sheets in one file get distinct, non-overlapping
    /// rectangles (replacing the old `idx*60` stacking).
    #[test]
    fn multiple_sheets_do_not_overlap() {
        let src = "\
test
.subckt SUB a b
R1 a b 1k
.ends
R0 in n1 1k
X1 n1 m1 SUB
X2 m1 out SUB
RL out 0 1k
.end
";
        let (origins, _) = place(src);
        assert_eq!(origins.len(), 2, "expected two sheets");
        let rect = |o: &(String, (f64, f64))| Rect {
            x0: o.1.0,
            y0: o.1.1,
            x1: o.1.0 + SHEET_W_MM,
            y1: o.1.1 + sheet_height(2),
        };
        let a = rect(&origins[0]);
        let b = rect(&origins[1]);
        assert!(!a.overlaps(&b), "sheets overlap: {a:?} vs {b:?}");
    }

    /// Every sheet origin is grid-snapped.
    #[test]
    fn sheet_origins_are_grid_snapped() {
        let src = "\
test
.subckt SUB a b
R1 a b 1k
.ends
R0 in n1 1k
X1 n1 out SUB
.end
";
        let (origins, _) = place(src);
        for (refdes, (x, y)) in &origins {
            assert!(
                (x - snap(*x)).abs() < 1e-9 && (y - snap(*y)).abs() < 1e-9,
                "sheet {refdes} origin ({x},{y}) not grid-snapped",
            );
        }
    }
}
