//! PWR_FLAG placement — driver markers for otherwise-undriven nets.
//!
//! KiCad's ERC reports `power_pin_not_driven` for any net whose
//! `power_in` pin(s) are not fed by a `power_out` pin, and
//! `pin_not_driven` for an `input` pin not fed by an `output` pin.
//! Both are *correctness*-tier (Tier-0 V2) errors. The standard KiCad
//! remedy is a `PWR_FLAG` symbol: it exposes a single `power_out` pin
//! that marks the net as externally driven, silencing both checks.
//!
//! The rule here is **general, structural, and class-aware**. A net
//! gets exactly one `PWR_FLAG` iff:
//!   (a) it has at least one pin, AND
//!   (b) ERC *requires* it to be driven — a Power/Ground net always
//!       does (its `power:*` glyph exposes a `power_in` anchor), and a
//!       Signal net does only if some pin on it is `input`/`power_in`
//!       (`PinRef::requires_driver`), AND
//!   (c) it lacks a valid driver *for its class*:
//!         * any class → a true *driving* pin (`PinRef::drives`:
//!           Output/PowerOut/bidirectional/…);
//!         * Signal only → **also** any `Passive` pin. KiCad's
//!           `DrivingPinTypes` includes `PT_PASSIVE` for non-power
//!           nets, so a resistor/cap terminal is a valid signal-net
//!           driver — but *not* for a *power net*, which still demands
//!           a real `power_out`. "Power net" here follows KiCad's
//!           `ispowerNet` (erc.cpp:1033): a name-based Power/Ground
//!           rail OR any net carrying a component `power_in` pin
//!           (`NetSpec::has_power_in`), even under a signal-flavoured
//!           name.
//!
//! This covers both ERC classes — a rail net whose only pins are
//! `power_in` (rail flag preserved), and a signal net whose only pins
//! are `input` with no passive terminal (e.g. a transistor base fed
//! solely by an input global label whose stimulus source is `;@
//! ignore`d) — while leaving passive-bearing signal nets (R–C
//! junctions, a transistor base with a bias resistor) untouched, since
//! their passive pin is a valid driver. No fixture or refdes names are
//! consulted.
//!
//! # Where the flags go
//!
//! Flags are split into two placement strategies by how their net
//! achieves connectivity:
//!
//! * **Global rails** (name-based Power/Ground nets) connect by *name*:
//!   KiCad ties every `power:*` instance sharing a Value into one net,
//!   with no wire involved. So a rail's driver need not sit anywhere
//!   near the circuit. These flags are collected into a tidy **corner
//!   driver block** in the bottom-right of the drawing (see
//!   [`emit_corner_block`]): one row per rail, each row a `power:*`
//!   glyph for that rail with its `PWR_FLAG` co-located on the same
//!   anchor. The circuit keeps its ordinary rail glyphs and loses the
//!   flag clutter entirely.
//!
//! * **Sheet-local signal nets** connect by *wire*. A driver marker for
//!   one of these has to touch the net's own geometry, so it stays
//!   anchored on a pin of that net (the original behaviour).
//!
//! Both strategies are V11-safe. The corner block's glyph and flag share
//! one anchor coordinate, and both symbols carry their connection pin at
//! the symbol origin, so they join by geometric coincidence — the same
//! no-stub idiom the on-pin path uses, and the reason no wire is needed
//! between them. The on-pin path additionally lands on an existing pin
//! coordinate of its own net, shorting nothing.
//!
//! The corner block is placed outward of the circuit's own content bbox
//! (pins ∪ symbol bodies), so it cannot overlap any circuit geometry
//! (V12/V13). It rides the emitter's `translate_into_page` pass like all
//! other content, so it lands inside the page's usable area (V15).

use lexpr::Value as Sexpr;
use spice_layout::net_class::NetClass;

use crate::types::{Bbox, Direction, NetSpec, PinRef, RouteResult};

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
#[allow(clippy::too_many_arguments)]
pub fn emit(
    nets: &[NetSpec],
    obstacles: &[Bbox],
    sheet_bodies: &[Bbox],
    library: Option<&kicad_symbols::Library>,
    scope: &str,
    sheet_uuid: &str,
    project_name: &str,
    pwr_counter: &mut usize,
    flg_counter: &mut usize,
    out: &mut RouteResult,
) {
    let resolved = library.is_none_or(|lib| lib.lookup(PWR_FLAG_LIB_ID).is_some());
    let is_root = scope == ROOT_SCOPE;
    // Global rails deferred to the bottom-right corner driver block.
    let mut corner_rails: Vec<&NetSpec> = Vec::new();
    for net in nets {
        if net.pins.is_empty() {
            continue;
        }
        let is_power_ground = matches!(net.class, NetClass::Power | NetClass::Ground);
        // KiCad's ERC treats a net as a *power net* (`ispowerNet`,
        // erc.cpp:1033) iff it carries ≥1 `power_in` pin — purely
        // pin-based, not name-based. A power net accepts only a
        // `power_out` driver; a passive pin does NOT drive it. So the
        // passive exception below must be suppressed for any net with a
        // component `power_in` pin, even one with a signal-flavoured
        // name (e.g. an opamp `V+` on an RC-derived midrail). This is a
        // superset of the name-based Power/Ground class.
        let is_power_class = is_power_ground || net.has_power_in;
        // (1) Does ERC *require* this net to be driven?
        //   * A Power/Ground net always gets a `power:*` glyph (whose
        //     anchor pin is `power_in`) from `rails::emit`, so it
        //     unconditionally requires a `power_out` driver.
        //   * A Signal net requires one only if a placement pin on it is
        //     itself `input`/`power_in` (`PinRef::requires_driver`).
        let requires = is_power_ground || net.pins.iter().any(|p| p.requires_driver);
        if !requires {
            continue;
        }
        // (2) Does the net already carry a valid driver *for its class*?
        //   * Any class → a true driving pin (`PinRef::drives`).
        //   * Signal only → also a `Passive` pin (`NetSpec::has_passive`).
        //     KiCad's `DrivingPinTypes` counts `PT_PASSIVE` as a driver
        //     on a non-power net, so a Signal net with a resistor/cap
        //     terminal needs no flag. A *power* net (name-based rail OR
        //     any net with a component `power_in` pin, `is_power_class`)
        //     still requires a real `power_out`, so its passive pins do
        //     not qualify.
        // Latent divergence: for a *power-class* net, `p.drives` accepts
        // any driving pin (Output/TriState/Bidi), but KiCad silences
        // `power_pin_not_driven` **only** for a `POWER_OUT` pin — a power
        // net whose sole driver is a plain `Output` pin would skip the flag
        // here yet still trip ERC. No current fixture exercises it (same
        // untestable-divergence family as `drives()`'s OC/OE note,
        // kicad-symbols lib.rs); tighten this to a power_out-specific check
        // when a fixture that reproduces it lands.
        let has_driver = net.pins.iter().any(|p| p.drives) || (!is_power_class && net.has_passive);
        if has_driver {
            continue;
        }
        // Power/Ground nets are global (one electrical net across all
        // sheets). Drive them with a single root-sheet PWR_FLAG; a
        // child-sheet copy would double-drive the net. Signal nets are
        // sheet-local, so a child PWR_FLAG is correct and necessary.
        //
        // This gate keys on `is_power_ground` (name-based global rails),
        // NOT `is_power_class`. A signal-named net that is only a "power
        // net" by virtue of carrying a component `power_in` pin
        // (`has_power_in`) is sheet-*local*, so on a child sheet it
        // legitimately needs its own local flag — do not fold it into
        // the global root-only gate.
        if is_power_ground && !is_root {
            continue;
        }
        // Net has no driver — it would trip ERC.
        if !resolved {
            out.warnings.push(format!(
                "pwrflag: lib_id '{PWR_FLAG_LIB_ID}' not found in library; net '{}' left undriven (ERC will flag it)",
                net.name
            ));
            continue;
        }
        // A name-based Power/Ground rail is a *global* net: every
        // `power:*` instance sharing its Value is the same net, wire or
        // no wire. Its driver therefore does not have to sit on the
        // circuit at all — defer it to the corner block, which keeps the
        // flag glyphs out of the reader's way.
        if is_power_ground {
            corner_rails.push(net);
            continue;
        }
        // A sheet-local signal net connects by wire, so its driver must
        // touch the net's own geometry. Pick a deterministic anchor pin
        // (lexicographically smallest world coordinate) and attach the
        // PWR_FLAG there.
        let Some(anchor) = pick_anchor(&net.pins) else {
            continue;
        };
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
    emit_corner_block(
        &corner_rails,
        nets,
        obstacles,
        sheet_bodies,
        sheet_uuid,
        project_name,
        pwr_counter,
        flg_counter,
        out,
    );
}

/// Horizontal clearance (mm) between the circuit's content bbox and the
/// corner driver block's anchor column. Eight grid cells: enough for the
/// widest rail Value text (drawn centred on the anchor, so it reaches
/// roughly half its width to the left) to clear the circuit outright.
const BLOCK_GAP_MM: f64 = 8.0 * 1.27;

/// Vertical pitch (mm) between consecutive rows of the corner block. Ten
/// grid cells. Each row spans the glyph body (≈2.54 mm on the canonical
/// side) plus its Value text one cell beyond that, and the PWR_FLAG
/// chevron (≈2.54 mm on the opposite side) — about 9 mm all told, so a
/// 12.7 mm pitch leaves a clear cell between rows (V13).
const BLOCK_ROW_PITCH_MM: f64 = 10.0 * 1.27;

/// KiCad schematic grid (mm). The block anchors are snapped to it so
/// every emitted coordinate stays on-grid.
const GRID_MM: f64 = 1.27;

/// Draw the bottom-right **driver block**: one row per global rail, each
/// row a `power:*` glyph for that rail with its `PWR_FLAG` co-located on
/// the same anchor coordinate.
///
/// This is the whole point of the corner arrangement. KiCad connects
/// `power:*` symbols by Value, not by wire, so the `power:VCC` glyph
/// drawn here *is* the same net as every `power:VCC` in the circuit —
/// and the `PWR_FLAG` sharing its anchor pin therefore drives the entire
/// rail. ERC sees exactly one `power_out` per rail (unchanged), while
/// the reader sees an unflagged circuit and one tidy legend of drivers
/// off to the side.
///
/// The block sits `BLOCK_GAP_MM` to the right of the circuit's content
/// bbox, with its first row level with the content's bottom edge and
/// further rows descending — i.e. the bottom-right of the drawing. Rails
/// are emitted in the order they appear in `nets`, which is
/// deterministic, so the block is stable across runs.
#[allow(clippy::too_many_arguments)]
fn emit_corner_block(
    rails: &[&NetSpec],
    nets: &[NetSpec],
    obstacles: &[Bbox],
    sheet_bodies: &[Bbox],
    sheet_uuid: &str,
    project_name: &str,
    pwr_counter: &mut usize,
    flg_counter: &mut usize,
    out: &mut RouteResult,
) {
    if rails.is_empty() {
        return;
    }
    let Some(content) = content_bbox(nets, obstacles, sheet_bodies) else {
        return;
    };
    let x = snap(content.x1 + BLOCK_GAP_MM);
    let y0 = snap(content.y1);
    for (row, net) in rails.iter().enumerate() {
        let Some(lib_id) = crate::rails::lib_id_for(net) else {
            continue;
        };
        #[allow(clippy::cast_precision_loss)]
        let y = y0 + row as f64 * BLOCK_ROW_PITCH_MM;
        // Which way the glyph's *body* is drawn — NOT its attachment
        // axis. Only the `power:GND` triangle hangs below its anchor;
        // every other rail glyph (VCC / VDD / +NV chevron, and the VEE
        // marker) rises above it. `rails::canonical_axis` deliberately
        // reports `Down` for a negative rail because that is the pin
        // direction VEE *attaches* to, which is the opposite of where
        // its graphic is drawn — using it here would point the flag
        // straight into the VEE body (V13).
        let body_down = matches!(net.class, NetClass::Ground) && !net.negative_rail;
        // Value text goes on the body side, one cell past the glyph tip;
        // the flag goes on the other side. The two graphics therefore
        // occupy opposite halves of the row and cannot overlap.
        let text_outward = if body_down {
            Direction::Down
        } else {
            Direction::Up
        };
        *pwr_counter += 1;
        let pwr_refdes = format!("#PWR{pwr_counter}");
        out.sexprs.push(crate::rails::glyph_sexpr_at(
            lib_id,
            &net.name,
            x,
            y,
            text_outward,
            &pwr_refdes,
            sheet_uuid,
            project_name,
        ));
        // The flag points *opposite* the glyph body, so the two graphics
        // share the anchor pin without their chevrons overlapping (V13).
        let glyph_down = body_down;
        *flg_counter += 1;
        let flg_refdes = format!("#FLG{flg_counter}");
        out.sexprs.push(pwr_flag_sexpr_at(
            x,
            y,
            glyph_down,
            &flg_refdes,
            sheet_uuid,
            project_name,
        ));
    }
}

/// Bounding box of everything already on the sheet: every net pin plus
/// every symbol-body obstacle. `None` when the sheet has no geometry at
/// all (nothing to sit beside).
fn content_bbox(nets: &[NetSpec], obstacles: &[Bbox], sheet_bodies: &[Bbox]) -> Option<Bbox> {
    let mut acc: Option<Bbox> = None;
    let mut grow = |x0: f64, y0: f64, x1: f64, y1: f64| {
        acc = Some(match acc {
            None => Bbox { x0, y0, x1, y1 },
            Some(b) => Bbox {
                x0: b.x0.min(x0),
                y0: b.y0.min(y0),
                x1: b.x1.max(x1),
                y1: b.y1.max(y1),
            },
        });
    };
    for net in nets {
        for p in &net.pins {
            grow(p.x_mm, p.y_mm, p.x_mm, p.y_mm);
        }
    }
    for b in obstacles.iter().chain(sheet_bodies) {
        grow(
            b.x0.min(b.x1),
            b.y0.min(b.y1),
            b.x0.max(b.x1),
            b.y0.max(b.y1),
        );
    }
    acc
}

/// Snap a coordinate onto the KiCad schematic grid.
fn snap(v: f64) -> f64 {
    (v / GRID_MM).round() * GRID_MM
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
    pwr_flag_sexpr_at(x, y, glyph_down, refdes, sheet_uuid, project_name)
}

/// Emit a `PWR_FLAG` whose anchor pin sits at `(x, y)`, oriented away
/// from a co-located rail glyph's body per [`flag_rotation`].
///
/// Split out of [`pwr_flag_sexpr`] so the corner driver block can place
/// a flag at a synthesised coordinate rather than deriving one from a
/// host pin.
fn pwr_flag_sexpr_at(
    x: f64,
    y: f64,
    glyph_down: bool,
    refdes: &str,
    sheet_uuid: &str,
    project_name: &str,
) -> Sexpr {
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
