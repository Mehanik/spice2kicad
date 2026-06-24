//! PWR_FLAG placement — driver markers for otherwise-undriven nets.
//!
//! KiCad's ERC reports `power_pin_not_driven` for any net whose
//! `power_in` pin(s) are not fed by a `power_out` pin, and
//! `pin_not_driven` for an `input` pin not fed by an `output` pin.
//! Both are *correctness*-tier (Tier-0 V2) errors. The standard KiCad
//! remedy is a `PWR_FLAG` symbol: it exposes a single `power_out` pin
//! that marks the net as externally driven, silencing both checks.
//!
//! The rule here is **general and structural**: place exactly one
//! `PWR_FLAG` on every net that (a) has at least one pin, (b) has at
//! least one pin that *requires* a driver (a `power_in` or `input`
//! pin — `PinRef::requires_driver`), and (c) has no *driving* pin
//! (`PinRef::drives == false` for all its pins). This single predicate
//! covers both ERC classes — a rail net whose only pins are
//! `power_in`, and a signal net whose only pins are `input` (e.g. a
//! transistor base fed solely by an input global label whose stimulus
//! source is `;@ ignore`d) — while leaving passive-only nets (R–C
//! junctions) untouched, since they impose no driver requirement. No
//! fixture or refdes names are consulted.
//!
//! Placement is V11-safe: the flag's anchor pin sits exactly on an
//! existing pin coordinate of the *same* net, so it joins that net by
//! geometric coincidence and shorts nothing. The flag body extends in
//! the host pin's outward direction (away from the symbol body), so it
//! does not overlap the host body (V12/V13).

use lexpr::Value as Sexpr;
use spice_layout::net_class::NetClass;

use crate::types::{NetSpec, PinRef, RouteResult};

/// Scope name the root sheet is routed under (see
/// `kicad_emitter::schematic::emit_root`). Power/Ground nets are global
/// in KiCad (connected by name across every sheet), so their single
/// `PWR_FLAG` driver belongs on the root sheet only — emitting one on a
/// child sheet too would double-drive the net (`pin_to_pin`: two
/// `power_out` pins).
const ROOT_SCOPE: &str = "root";

/// Library id of the PWR_FLAG symbol. Inlined verbatim from the loaded
/// `power.kicad_sym` (V3).
const PWR_FLAG_LIB_ID: &str = "power:PWR_FLAG";

/// Append a `PWR_FLAG` symbol for every net in `req` that has pins but
/// no driving pin. Returns nothing; pushes onto `out`/`warnings`.
///
/// `library`-resolution mirrors [`crate::rails::emit`]: when the
/// `PWR_FLAG` symbol is missing from the loaded library the marker is
/// skipped and a warning recorded (ERC then still reports the
/// not-driven error, surfaced by the V2 verifier — we never silently
/// fake a driver).
pub fn emit(
    nets: &[NetSpec],
    library: Option<&kicad_symbols::Library>,
    scope: &str,
    sheet_uuid: &str,
    project_name: &str,
    flg_counter: &mut usize,
    out: &mut RouteResult,
) {
    let resolved = library.is_none_or(|lib| lib.lookup(PWR_FLAG_LIB_ID).is_some());
    let is_root = scope == ROOT_SCOPE;
    for net in nets {
        if net.pins.is_empty() {
            continue;
        }
        // Only nets that ERC *requires* to be driven need a flag.
        // Two sources of that requirement:
        //   * A Power/Ground net always gets a `power:*` glyph (whose
        //     anchor pin is `power_in`) from `rails::emit`, so it
        //     unconditionally requires a `power_out` driver.
        //   * A Signal net requires one only if a placement pin on it is
        //     itself `input`/`power_in` (`PinRef::requires_driver`).
        // A purely passive Signal net (e.g. an R–C junction) imposes no
        // driver requirement, so a PWR_FLAG there would be spurious
        // visual noise.
        let requires = matches!(net.class, NetClass::Power | NetClass::Ground)
            || net.pins.iter().any(|p| p.requires_driver);
        if !requires {
            continue;
        }
        if net.pins.iter().any(|p| p.drives) {
            continue;
        }
        // Power/Ground nets are global (one electrical net across all
        // sheets). Drive them with a single root-sheet PWR_FLAG; a
        // child-sheet copy would double-drive the net. Signal nets are
        // sheet-local, so a child PWR_FLAG is correct and necessary.
        if matches!(net.class, NetClass::Power | NetClass::Ground) && !is_root {
            continue;
        }
        // Net has no driver — it would trip ERC. Pick a deterministic
        // anchor pin (lexicographically smallest world coordinate) and
        // attach one PWR_FLAG there.
        let Some(anchor) = pick_anchor(&net.pins) else {
            continue;
        };
        if !resolved {
            out.warnings.push(format!(
                "pwrflag: lib_id '{PWR_FLAG_LIB_ID}' not found in library; net '{}' left undriven (ERC will flag it)",
                net.name
            ));
            continue;
        }
        *flg_counter += 1;
        let refdes = format!("#FLG{flg_counter}");
        // The rail glyph at this anchor draws its body on one vertical
        // side: a `power:GND` triangle hangs *down* (world +Y); every
        // other rail glyph (VCC / VDD / +NV chevron, VEE marker) rises
        // *up* (world −Y). The flag is co-located on the same pin and
        // points the *opposite* way, so its chevron clears the glyph
        // body (V13 — issue [2]) without a separating stub wire (which
        // would read as a non-outward first segment at the host pin, V5).
        let glyph_down = matches!(net.class, NetClass::Ground) && !net.negative_rail;
        out.sexprs.push(pwr_flag_sexpr(
            anchor,
            glyph_down,
            &refdes,
            sheet_uuid,
            project_name,
        ));
    }
}

/// Deterministically choose the anchor pin: smallest (x, y) world coord.
fn pick_anchor(pins: &[PinRef]) -> Option<&PinRef> {
    pins.iter().min_by(|a, b| {
        a.x_mm
            .partial_cmp(&b.x_mm)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.y_mm
                    .partial_cmp(&b.y_mm)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    })
}

/// PWR_FLAG rotation (degrees) that points the chevron *away* from the
/// co-located rail glyph's body, so the two graphics never overlap (V13 —
/// issue [2]) without any separating stub wire.
///
/// The flag and the rail glyph share the host pin (their connection pins
/// both sit at the symbol origin). The glyph body occupies one vertical
/// side of that origin — `glyph_down` (a `power:GND` triangle) hangs down
/// (screen, world +Y); every other rail glyph rises up (world −Y). The
/// PWR_FLAG body is drawn local-+Y (screen up at rot 0). So:
///   * glyph hangs down → flag points up (rot 0), chevron above the pin,
///     clear of the triangle below;
///   * glyph rises up → flag points down (rot 180), chevron below the
///     pin, clear of the chevron above.
///
/// Co-locating with no stub keeps the host pin's wiring identical to the
/// pre-flag layout: no spurious non-outward first segment (V5) and no new
/// wire to cross a body (V12).
fn flag_rotation(glyph_down: bool) -> u16 {
    if glyph_down { 0 } else { 180 }
}

fn pwr_flag_sexpr(
    pin: &PinRef,
    glyph_down: bool,
    refdes: &str,
    sheet_uuid: &str,
    project_name: &str,
) -> Sexpr {
    // A flag anchored on a hierarchical-sheet port pin rides the same
    // outward offset as the `power:*` glyph it drives (see
    // `rails::sheet_edge_offset`), so it stays co-located with the offset
    // glyph on the same net (V11). For a non-sheet pin the offset is zero.
    let (ox, oy) = crate::rails::sheet_edge_offset(pin);
    let (x, y) = (pin.x_mm + ox, pin.y_mm + oy);
    let rot = flag_rotation(glyph_down);
    // The PWR_FLAG anchor pin sits at the symbol origin, so the pin tip
    // stays at (x, y) for any rotation — the connection point is stable
    // and coincident with the host net pin (V11). Reference and Value are
    // both hidden (a drawn `#FLGn` / "PWR_FLAG" would collide with
    // neighbouring text, V13). The `(instances …)` block is mandatory for
    // kicad-cli netlist export.
    //
    // The hidden Reference/Value anchors track the flag's own rotation so
    // they never reserve text geometry on the host side; both are hidden,
    // so their exact `(at)` is cosmetic, but we keep them on the chevron
    // side for tidiness.
    let txt = format!(
        "(symbol \
            (lib_id \"{PWR_FLAG_LIB_ID}\") \
            (at {x:.2} {y:.2} {rot}) \
            (unit 1) \
            (in_bom no) (on_board no) \
            (property \"Reference\" \"{refdes}\" (at {x:.2} {ry:.2} 0) \
                (effects (font (size 1.27 1.27)) (hide yes))) \
            (property \"Value\" \"PWR_FLAG\" (at {x:.2} {vy:.2} 0) \
                (effects (font (size 1.27 1.27)) (hide yes))) \
            (instances (project \"{project_name}\" \
                (path \"/{sheet_uuid}\" \
                    (reference \"{refdes}\") (unit 1)))))",
        ry = y - 1.27,
        vy = y + 3.81,
    );
    lexpr::from_str(&txt).expect("pwr_flag s-expr parses")
}
