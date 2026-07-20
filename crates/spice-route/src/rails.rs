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
use spice_layout::glyph_geom::{self, GlyphAxis};
use spice_layout::net_class::NetClass;

use crate::types::{Direction, NetSpec, PinRef};

/// One grid cell (1.27 mm). Power glyphs sit one cell along the pin's
/// outward direction, so the pin connects to the symbol's anchor pin
/// without a stem wire. Single-sourced from `spice-layout` so the
/// placer's reserved glyph zone and this drawn glyph cannot drift
/// (ADR-14, Phase 1).
const GRID_MM: f64 = glyph_geom::GRID_MM;

/// Grid-cell offset applied to a power glyph anchored on a
/// hierarchical-sheet port pin. The glyph (and its net-name label) are
/// pushed this many cells *outward* (away from the sheet body) so the
/// glyph body and label clear both the sheet body and the sheet's port
/// label, which KiCad draws at the port-pin coordinate. Two cells: the
/// glyph body extends ±1 cell about its anchor, so a 2-cell offset puts
/// the inner glyph edge one full cell clear of the sheet edge. A stub
/// wire bridges the port pin to the offset anchor. Single-sourced from
/// `spice-layout` (ADR-14, Phase 1).
const SHEET_EDGE_GLYPH_OFFSET_CELLS: f64 = glyph_geom::SHEET_EDGE_GLYPH_OFFSET_CELLS;

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
    let Some(lib_id) = lib_id_for(net) else {
        return;
    };
    // The glyph's canonical attachment axis (the host-pin direction that
    // needs no offset). Computed once per net; a negative rail attaches
    // like ground (Down) regardless of its `power:VEE` body geometry.
    let canon = canonical_axis(net.class, net.negative_rail);
    let resolved = library.is_none_or(|lib| lib.lookup(lib_id).is_some());
    for pin in &net.pins {
        if resolved {
            *pwr_counter += 1;
            let refdes = format!("#PWR{pwr_counter}");
            out.push(power_symbol_sexpr(
                lib_id,
                &net.name,
                pin,
                canon,
                &refdes,
                sheet_uuid,
                project_name,
            ));
            // V14 forced-sideways fallback (host-body case) and the
            // sheet-edge fallback (sheet-port case): offset the glyph
            // outward and bridge the gap with a stub wire so the glyph
            // body never overlaps the host / sheet body while the
            // rotation stays the conventional rot-0.
            if let Some(stub) = stub_wire(pin, canon) {
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

/// The `power:*` library symbol that renders `net`'s rail glyph, or
/// `None` for a Signal net (which gets wires, not glyphs).
///
/// Shared by Stage 1 (the per-pin rail glyphs) and the PWR_FLAG stage,
/// which draws one more instance of the *same* glyph in the corner
/// driver block. Both must resolve to the identical `lib_id` and Value,
/// since KiCad ties `power:*` instances together by Value — that shared
/// name is exactly what makes the corner flag drive the whole rail.
pub(crate) fn lib_id_for(net: &NetSpec) -> Option<&'static str> {
    Some(match net.class {
        // A negative supply rail is classed Ground for layout but must
        // render with the distinct `power:VEE` glyph (V10), regardless
        // of its NetClass. The negative-rail flag is the authoritative
        // signal here.
        _ if net.negative_rail => "power:VEE",
        // The declared tag outranks the net's spelling: an unrecognised
        // but correctly-declared rail name must not silently collapse to
        // the generic VCC glyph.
        NetClass::Power => power_lib_id(net.rail_tag.as_deref().unwrap_or(&net.name)),
        NetClass::Ground => ground_lib_id(&net.name),
        NetClass::Signal => return None,
    })
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

/// The canonical screen-vertical direction a rot-0 glyph's *attachment
/// side* faces — i.e. the host-pin outward direction for which the glyph
/// needs no forced-sideways offset.
///
/// * Positive supply (`power:VCC`/`VDD`/`+NV`) → up (chevron above the
///   anchor; attaches to an up-facing pin).
/// * True ground (`power:GND`) → down (triangle below the anchor;
///   attaches to a down-facing pin).
/// * Negative rail (`power:VEE`) → **down**, like ground. A negative
///   rail sits in the bottom band and its host pins face screen-down
///   (its `VertPref` is `Down`), so the glyph attaches to a down-facing
///   pin exactly as a GND glyph does. Treating it as `Down` here keeps
///   the glyph at the host pin with no offset and no stub — identical
///   geometry to the GND glyph it replaces, only the `lib_id` (and thus
///   the drawn symbol: a `V-` marker, not a ground triangle) differs.
///   Using `Up` (the VEE symbol's body geometry) would force a sideways
///   offset whose stub wire dives back through the host circuitry (a
///   V12 foreign-body crossing); the body-direction mismatch is a
///   pre-existing V13 quality concern, not a wiring defect to create.
fn canonical_axis(class: NetClass, negative_rail: bool) -> Direction {
    // Delegate the rule to the single-sourced placement-side mapping
    // (ADR-14, Phase 1), then lift the screen-vertical axis into this
    // crate's `Direction`. Signal nets never reach here (they are
    // filtered out before `symbol_pose` is called).
    match glyph_geom::canonical_axis(class, negative_rail) {
        GlyphAxis::Up => Direction::Up,
        GlyphAxis::Down => Direction::Down,
    }
}

/// Rotation that makes a rail glyph's *drawn body* point along the axis
/// it *attaches* to.
///
/// Read off the KiCad library graphics: only `power:GND`'s triangle
/// hangs DOWN from its anchor (local −Y); every other rail glyph — the
/// VCC / VDD / +NV chevrons and the VEE arrow — rises UP. Match that
/// against [`canonical_axis`] and rotate 180° when they disagree.
///
/// Today the only disagreement is the negative rail. `power:VEE`
/// attaches to a down-facing pin like a ground glyph, but its arrow is
/// drawn upward, so at rot 0 it points back INTO the host body — see
/// the `diff_pair` render, where VEE's arrowhead sits inside `RTAIL`.
/// V14 reads "GND down, VCC up", and a negative supply belongs with
/// GND; rot 180 is what actually delivers that on the page.
///
/// This supersedes the "pre-existing V13 quality concern" that
/// [`canonical_axis`]'s doc comment records as accepted: the mismatch
/// it describes is exactly what this function removes, and it does so
/// without touching the attachment axis, so the offset/stub geometry
/// that comment protects is unchanged.
pub(crate) fn glyph_rotation(lib_id: &str, canon: Direction) -> u16 {
    let body = if lib_id == "power:GND" {
        Direction::Down
    } else {
        Direction::Up
    };
    if body == canon { 0 } else { 180 }
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
fn is_forced_sideways(pin: &PinRef, canon: Direction) -> bool {
    let opposite = match canon {
        Direction::Up => Direction::Down,
        Direction::Down => Direction::Up,
        Direction::Left => Direction::Right,
        Direction::Right => Direction::Left,
    };
    pin.outward == opposite
}

/// Outward offset (mm) for a glyph / driver marker anchored on a
/// hierarchical-sheet port pin, or `(0, 0)` when the pin is not on a
/// sheet edge. Shared by Stage 1 (`power:*` glyphs) and the PWR_FLAG
/// stage so the driver marker rides the same offset as the glyph it
/// drives, keeping both off the sheet port label.
pub(crate) fn sheet_edge_offset(pin: &PinRef) -> (f64, f64) {
    if !pin.on_sheet_edge {
        return (0.0, 0.0);
    }
    let (ux, uy) = outward_delta(pin.outward);
    (
        ux * SHEET_EDGE_GLYPH_OFFSET_CELLS,
        uy * SHEET_EDGE_GLYPH_OFFSET_CELLS,
    )
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
fn symbol_pose(pin: &PinRef, canon: Direction) -> (f64, f64, u16) {
    if let Some((dx, dy)) = glyph_offset(pin, canon) {
        (pin.x_mm + dx, pin.y_mm + dy, 0)
    } else {
        (pin.x_mm, pin.y_mm, 0)
    }
}

/// Outward offset (mm) applied to a glyph anchor, or `None` when the
/// glyph sits exactly on the pin. Two cases need an offset:
///
/// * **V14 forced-sideways** — the host pin points opposite the glyph's
///   canonical body direction; offset one cell outward so the rot-0
///   glyph body clears the host symbol body.
/// * **Sheet-edge** — the pin is a hierarchical-sheet port pin (see
///   [`PinRef::on_sheet_edge`]); offset
///   [`SHEET_EDGE_GLYPH_OFFSET_CELLS`] cells outward (away from the
///   sheet body) so the glyph body and net-name label clear the sheet
///   body and its port label.
///
/// Both offsets run along the pin's outward direction, so the bridging
/// stub doubles as the pin's outward first segment (V5). The sheet-edge
/// case takes precedence (its larger offset subsumes the V14 one cell).
fn glyph_offset(pin: &PinRef, canon: Direction) -> Option<(f64, f64)> {
    if pin.on_sheet_edge {
        return Some(sheet_edge_offset(pin));
    }
    if is_forced_sideways(pin, canon) {
        let (ux, uy) = outward_delta(pin.outward);
        return Some((ux, uy));
    }
    None
}

/// Stub wire from the host pin to the offset glyph anchor, emitted
/// whenever the glyph is offset (V14 forced-sideways or sheet-edge). The
/// stub extends along the pin's outward direction, so it doubles as the
/// pin's outward first segment (V5). Returns `None` when the glyph sits
/// on the pin.
fn stub_wire(pin: &PinRef, canon: Direction) -> Option<Sexpr> {
    let (dx, dy) = glyph_offset(pin, canon)?;
    let (x0, y0) = (pin.x_mm, pin.y_mm);
    let (x1, y1) = (pin.x_mm + dx, pin.y_mm + dy);
    let txt = format!(
        "(wire (pts (xy {x0:.2} {y0:.2}) (xy {x1:.2} {y1:.2})) \
         (stroke (width 0) (type default)))",
    );
    Some(lexpr::from_str(&txt).expect("stub wire s-expr parses"))
}

/// Offset (mm) past the glyph tip for the net-name Value text, one grid
/// cell beyond the ≈2.54 mm glyph body extent. Single-sourced from
/// `spice-layout` (ADR-14, Phase 1) so the placer reserves the same
/// value-text reach it draws here.
const VALUE_TEXT_OFFSET_MM: f64 = glyph_geom::VALUE_TEXT_OFFSET_MM;

/// World anchor `(x, y)` for a power glyph's Value (net-name) text,
/// placed on the *outward* side of the glyph — the side the host pin
/// points, which is also the side the glyph body and any forced-sideways
/// / sheet-edge offset run along (see [`glyph_offset`]). This is the side
/// *away* from the host symbol body the anchor pin attaches to, so the
/// label never hangs into the host (V13 — issue [1]).
///
/// Keying on the host pin's outward direction (not the glyph's fixed
/// rot-0 graphic direction) is what makes this correct in every case:
///   * a VCC chevron on an up-facing pin → name above (outward);
///   * a GND triangle on a down-facing pin → name below (outward);
///   * a *forced-sideways* GND glyph (pin faces up, glyph offset up away
///     from the host below) → name above, clear of the host (the
///     graphic-direction rule would put it below, back into the host);
///   * a sheet-edge glyph (pin faces left, glyph offset left) → name
///     further left, clear of both the sheet body and the sheet's
///     port-name text to the right (V13 — issue [4]).
fn value_text_anchor(x: f64, y: f64, outward: Direction) -> (f64, f64) {
    let (dx, dy) = outward_delta(outward);
    // `outward_delta` returns one grid cell; scale to the glyph-clearing
    // offset.
    let scale = VALUE_TEXT_OFFSET_MM / GRID_MM;
    (x + dx * scale, y + dy * scale)
}

fn power_symbol_sexpr(
    lib_id: &str,
    net_name: &str,
    pin: &PinRef,
    canon: Direction,
    refdes: &str,
    sheet_uuid: &str,
    project_name: &str,
) -> Sexpr {
    let (x, y, _rot) = symbol_pose(pin, canon);
    glyph_sexpr_at(
        lib_id,
        net_name,
        x,
        y,
        pin.outward,
        glyph_rotation(lib_id, canon),
        refdes,
        sheet_uuid,
        project_name,
    )
}

/// Emit one rot-0 `power:*` glyph whose anchor pin sits at `(x, y)`,
/// with its Value (net-name) text placed one glyph-clearing offset along
/// `outward`.
///
/// Split out of [`power_symbol_sexpr`] so the PWR_FLAG corner driver
/// block can draw a rail glyph at a *synthesised* coordinate — one with
/// no host pin to derive a pose from. Keeping both callers on this one
/// body guarantees the corner glyph is byte-identical in `lib_id`,
/// Value, and property layout to the in-circuit glyphs it stands for,
/// which is what makes KiCad connect them by name.
#[allow(clippy::too_many_arguments)]
pub(crate) fn glyph_sexpr_at(
    lib_id: &str,
    net_name: &str,
    x: f64,
    y: f64,
    outward: Direction,
    rot: u16,
    refdes: &str,
    sheet_uuid: &str,
    project_name: &str,
) -> Sexpr {
    // Single-sourced with the placer: `glyph_reach` reserves the box
    // this exact string occupies, so the two must not drift.
    let value = glyph_geom::rail_value_text(net_name);
    // Anchor the Value (net-name) text on the *outward* side of the
    // glyph — the side the glyph graphic points, away from the host
    // body the glyph attaches to. A GND triangle hangs *below* its
    // anchor pin (body local y < 0), so its name reads below; a VCC /
    // VEE chevron rises *above* its anchor pin (body local y > 0), so
    // its name reads above. Placing the name on the body side (the old
    // fixed `y + 1.27`) made a VCC label hang *down* into the host
    // resistor it sits atop (V13 — issue [1]). The offset clears the
    // glyph graphic itself: each glyph body extends ≈2.54 mm from the
    // anchor, so a 3.81 mm offset places the text one cell beyond the
    // tip.
    let (vx, vy) = value_text_anchor(x, y, outward);
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
            (property \"Value\" \"{value}\" (at {vx:.2} {vy:.2} 0)) \
            (instances (project \"{project_name}\" \
                (path \"/{sheet_uuid}\" \
                    (reference \"{refdes}\") (unit 1)))))",
        rx = x,
        ry = y - 1.27,
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
