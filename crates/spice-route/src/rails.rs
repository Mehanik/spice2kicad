//! Stage 1 — power-symbol placement.
//!
//! Power and Ground nets emit no `(wire …)`. Each pin on such a net
//! gets a `power:*` library symbol placed on the pin's outward side;
//! KiCad treats matching `power:*` symbol instances as electrically
//! connected globally (by their Value property), so no wire or label
//! is needed to express connectivity.
//!
//! Fallback: when the symbol library does not contain the chosen
//! `lib_id` (e.g. the user did not load `power.kicad_sym`), a
//! `(global_label …)` is emitted instead — same electrical semantics
//! at the cost of visual prettiness — and a warning is recorded.

use kicad_symbols::Library;
use lexpr::Value as Sexpr;
use spice_layout::net_class::NetClass;

use crate::types::{Direction, NetSpec, PinRef};

/// One grid cell (1.27 mm). Power glyphs sit one cell along the pin's
/// outward direction, so the pin connects to the symbol's anchor pin
/// without a stem wire.
const GRID_MM: f64 = 1.27;

/// Append power-symbol (or fallback global-label) S-exprs for every
/// pin on a Power/Ground net. Signal nets are ignored.
///
/// `pwr_counter` is incremented once per emitted power symbol so each
/// glyph carries a unique `#PWR<n>` reference designator across the
/// whole sheet.
pub fn emit(
    net: &NetSpec,
    library: Option<&Library>,
    sheet_uuid: &str,
    project_name: &str,
    pwr_counter: &mut usize,
    out: &mut Vec<Sexpr>,
    warnings: &mut Vec<String>,
) {
    let lib_id = match net.class {
        NetClass::Power => power_lib_id(&net.name),
        NetClass::Ground => ground_lib_id(&net.name),
        NetClass::Signal => return,
    };
    let resolved = library.is_none_or(|lib| lib.lookup(lib_id).is_some());
    for pin in &net.pins {
        if resolved {
            *pwr_counter += 1;
            let refdes = format!("#PWR{pwr_counter}");
            out.push(power_symbol_sexpr(
                lib_id,
                &net.name,
                pin,
                net.class,
                &refdes,
                sheet_uuid,
                project_name,
            ));
            // V14 forced-sideways fallback: when the host pin does not
            // face the glyph's canonical axis (power → up, ground →
            // down), the glyph — locked at rot 0 — would extend into
            // the host body. Offset it one grid cell along the
            // canonical axis and bridge the gap with a one-cell stub
            // wire so the glyph body never overlaps the host body while
            // the rotation stays the conventional 0.
            if let Some(stub) = stub_wire(pin, net.class) {
                out.push(stub);
            }
        } else {
            out.push(global_label_sexpr(&net.name, pin));
        }
    }
    if !resolved {
        warnings.push(format!(
            "rails: lib_id '{lib_id}' for net '{}' not found in library; emitted (global_label) fallback",
            net.name
        ));
    }
}

fn power_lib_id(net_name: &str) -> &'static str {
    match net_name.to_ascii_lowercase().as_str() {
        "vdd" => "power:VDD",
        "+5v" | "v5" | "5v" => "power:+5V",
        "+12v" | "v12" | "12v" => "power:+12V",
        "+3v3" | "3v3" => "power:+3V3",
        // Default (incl. "vcc", "v+", "vplus", and any unrecognised
        // positive-rail name) maps to VCC.
        _ => "power:VCC",
    }
}

fn ground_lib_id(_net_name: &str) -> &'static str {
    // v0.1: GNDA / GNDPWR variants collapse to plain GND. See plan.
    "power:GND"
}

/// The canonical screen-vertical direction a rot-0 glyph of `class`
/// extends its body away from its anchor pin.
///
/// * Power → up (chevron above the anchor).
/// * Ground → down (triangle below the anchor).
fn canonical_axis(class: NetClass) -> Direction {
    match class {
        NetClass::Power => Direction::Up,
        // Ground (the only other class that reaches here; Signal is
        // filtered out before `symbol_pose` is called).
        _ => Direction::Down,
    }
}

/// True only in the V14 *forced-sideways* case the offset+stub fallback
/// exists for: the host pin points in the exact *opposite* of the
/// glyph's canonical body direction, so a rot-0 glyph placed at the pin
/// coordinate would extend its body back through the host symbol body
/// (a GND pin pointing screen-up, or a VCC pin pointing screen-down).
///
/// A horizontal pin (Left/Right) is *not* forced-sideways: the rot-0
/// glyph hangs vertically off to the side and does not enter the host
/// body, so it keeps the on-pin placement with no stub (matching the
/// pre-V14 behaviour and avoiding a spurious vertical stub that would
/// otherwise read as a V5 non-outward first segment).
fn is_forced_sideways(pin: &PinRef, class: NetClass) -> bool {
    let canon = canonical_axis(class);
    let opposite = match canon {
        Direction::Up => Direction::Down,
        Direction::Down => Direction::Up,
        Direction::Left => Direction::Right,
        Direction::Right => Direction::Left,
    };
    pin.outward == opposite
}

/// One grid-cell file-coordinate delta along a pin's outward direction.
/// File Y increases downward, so `Up` is a negative Y delta.
fn outward_delta(dir: Direction) -> (f64, f64) {
    match dir {
        Direction::Up => (0.0, -GRID_MM),
        Direction::Down => (0.0, GRID_MM),
        Direction::Left => (-GRID_MM, 0.0),
        Direction::Right => (GRID_MM, 0.0),
    }
}

/// Compute (x, y, rotation_degrees) for the power symbol's anchor pin.
///
/// V14 locks the glyph rotation to its conventional orientation (rot 0
/// always: GND triangle down, VCC chevron up) regardless of the host
/// pin's outward direction.
///
/// Forced-sideways fallback: when the host pin points opposite the
/// glyph's canonical body direction (see [`is_forced_sideways`]),
/// placing the rot-0 glyph at the pin coordinate would extend its body
/// back through the host symbol body. The anchor is then offset one
/// grid cell *along the pin's outward direction* so the glyph body
/// clears the host; [`stub_wire`] bridges the host pin to the offset
/// anchor along that same outward direction (so the first segment from
/// the pin extends outward, satisfying V5). Otherwise the anchor sits
/// exactly on the pin (no stub).
fn symbol_pose(pin: &PinRef, class: NetClass) -> (f64, f64, u16) {
    if is_forced_sideways(pin, class) {
        let (dx, dy) = outward_delta(pin.outward);
        (pin.x_mm + dx, pin.y_mm + dy, 0)
    } else {
        (pin.x_mm, pin.y_mm, 0)
    }
}

/// One-cell stub wire from the host pin to the offset glyph anchor,
/// emitted only in the V14 forced-sideways case. The stub extends along
/// the pin's outward direction, so it doubles as the pin's outward first
/// segment (V5). Returns `None` when the glyph sits on the pin.
fn stub_wire(pin: &PinRef, class: NetClass) -> Option<Sexpr> {
    if !is_forced_sideways(pin, class) {
        return None;
    }
    let (dx, dy) = outward_delta(pin.outward);
    let (x0, y0) = (pin.x_mm, pin.y_mm);
    let (x1, y1) = (pin.x_mm + dx, pin.y_mm + dy);
    let txt = format!(
        "(wire (pts (xy {x0:.2} {y0:.2}) (xy {x1:.2} {y1:.2})) \
         (stroke (width 0) (type default)))",
    );
    Some(lexpr::from_str(&txt).expect("stub wire s-expr parses"))
}

fn power_symbol_sexpr(
    lib_id: &str,
    net_name: &str,
    pin: &PinRef,
    class: NetClass,
    refdes: &str,
    sheet_uuid: &str,
    project_name: &str,
) -> Sexpr {
    let (x, y, rot) = symbol_pose(pin, class);
    // Use the same pattern as the existing emitter: nested `(symbol …)`
    // with `lib_id`, `at`, `unit`, properties. Reference is a unique
    // `#PWR<n>` and is *hidden* (KiCad convention for power symbols:
    // the bookkeeping refdes is never drawn — the glyph and net-name
    // Value carry all reader-visible information; a drawn `#PWRn`
    // merely collides with neighbouring property text, V13(4)). Value
    // is the net name (which is what wires the global power net
    // together). The sibling `(instances …)` block is
    // mandatory: kicad-cli's netlist export skips any symbol instance
    // that doesn't appear in such a block.
    let txt = format!(
        "(symbol \
            (lib_id \"{lib_id}\") \
            (at {x:.2} {y:.2} {rot}) \
            (unit 1) \
            (in_bom no) (on_board no) \
            (property \"Reference\" \"{refdes}\" (at {rx:.2} {ry:.2} 0) \
                (effects (font (size 1.27 1.27)) (hide yes))) \
            (property \"Value\" \"{net_name}\" (at {vx:.2} {vy:.2} 0)) \
            (instances (project \"{project_name}\" \
                (path \"/{sheet_uuid}\" \
                    (reference \"{refdes}\") (unit 1)))))",
        rx = x,
        ry = y - 1.27,
        vx = x,
        vy = y + 1.27,
    );
    lexpr::from_str(&txt).expect("power symbol s-expr parses")
}

fn global_label_sexpr(net_name: &str, pin: &PinRef) -> Sexpr {
    let (x, y) = (pin.x_mm, pin.y_mm);
    let shape = match pin.outward {
        Direction::Up | Direction::Left => "input",
        Direction::Down | Direction::Right => "output",
    };
    let rot = match pin.outward {
        Direction::Up => 90,
        Direction::Right => 0,
        Direction::Down => 270,
        Direction::Left => 180,
    };
    let txt = format!("(global_label \"{net_name}\" (shape {shape}) (at {x:.2} {y:.2} {rot}))",);
    lexpr::from_str(&txt).expect("global_label s-expr parses")
}
