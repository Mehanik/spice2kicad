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
                &refdes,
                sheet_uuid,
                project_name,
            ));
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

/// Compute (x, y, rotation_degrees) for the symbol body so its anchor
/// pin sits at `pin`'s coordinate AND the glyph body extends *away*
/// from the host pin (not toward it).
///
/// Rule: power-symbol body must extend in the same world-direction the
/// host pin is "outward" pointing. Empirically (verified by rendering
/// `power:GND` at all four rotations under kicad-cli — see
/// `tests/symbol_pose_orientation` in this crate):
///
/// * rotation 0   → glyph body extends visually DOWN (+Y in schematic).
///   Use when host pin's outward direction is Down.
/// * rotation 90  → body extends visually LEFT  (-X). Use for outward Left.
/// * rotation 180 → body extends visually UP    (-Y). Use for outward Up.
/// * rotation 270 → body extends visually RIGHT (+X). Use for outward Right.
///
/// Equivalently: `rot = (host_outward_angle + 180) mod 360`, where the
/// host-outward angle in KiCad pin convention is 0=right, 90=up,
/// 180=left, 270=down.
///
/// The previous mapping (Up→0, Right→90, Down→180, Left→270) was off
/// by 180° on every axis: GND glyphs at the bottom of BJT emitters
/// rendered with the triangle apex pointing UP toward the host instead
/// of DOWN away from it. Fixed in the commit accompanying this comment.
fn symbol_pose(pin: &PinRef) -> (f64, f64, u16) {
    // Anchor pin is at lib origin (0, 0); placing the symbol at the
    // host pin's world coord makes the two pins coincide so KiCad
    // treats them as connected without an explicit wire.
    let (sx, sy) = (pin.x_mm, pin.y_mm);
    let rot = match pin.outward {
        Direction::Down => 0,
        Direction::Left => 90,
        Direction::Up => 180,
        Direction::Right => 270,
    };
    let _ = GRID_MM;
    (sx, sy, rot)
}

fn power_symbol_sexpr(
    lib_id: &str,
    net_name: &str,
    pin: &PinRef,
    refdes: &str,
    sheet_uuid: &str,
    project_name: &str,
) -> Sexpr {
    let (x, y, rot) = symbol_pose(pin);
    // Use the same pattern as the existing emitter: nested `(symbol …)`
    // with `lib_id`, `at`, `unit`, properties. Reference is a unique
    // `#PWR<n>`, Value is the net name (which is what wires the global
    // power net together). The sibling `(instances …)` block is
    // mandatory: kicad-cli's netlist export skips any symbol instance
    // that doesn't appear in such a block.
    let txt = format!(
        "(symbol \
            (lib_id \"{lib_id}\") \
            (at {x:.2} {y:.2} {rot}) \
            (unit 1) \
            (in_bom no) (on_board no) \
            (property \"Reference\" \"{refdes}\" (at {rx:.2} {ry:.2} 0)) \
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
