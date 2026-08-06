//! V11 / V12 / V13 — electrical-safety invariants.
//!
//! Per CLAUDE.md:
//!  * **V11 (correctness)** — wire endpoints, wire interiors, and
//!    label anchors must not coincide with pins owned by a different
//!    net. KiCad's connectivity engine merges geometric coincidence
//!    into electrical connection without any junction marker, so a
//!    V11 violation is a *silent short* of two nets on export.
//!  * **V12** — wires must not cross foreign symbol bodies. Today's
//!    `avoid_obstacles` pass already tries to keep wires clear; V12
//!    promotes the warning to a measured quality defect. Four
//!    fixtures expect zero crossings; `common_emitter` is held to a
//!    fixture-specific cap (residual placer-level issue tracked as a
//!    v0.2 router improvement).
//!  * **V13** — labels must not overlap symbol bodies, property text,
//!    or foreign-net wire interiors. Body-overlap and foreign-wire
//!    coincidence are correctness defects; property-overlap is a
//!    quality one (current placer routinely overlaps Reference /
//!    Value text and that's tracked separately).
//!
//! Symbol-body bboxes approximate as a 5.08 × 5.08 mm square centred
//! on the placed instance's origin — same approximation used in
//! `placement_quality::no_symbol_symbol_overlap_across_fixtures`.

mod common;

use std::path::PathBuf;

use common::spice_to_kicad;
use common::text_model::{Bbox, Pt, TextKind, text_bbox};
use kicad_symbols::{Orientation, PinElectrical, Rotation};
use lexpr::Value;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spice2kicad-elec-{pid}-{seq}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn parse(path: &std::path::Path) -> Value {
    let src = std::fs::read_to_string(path).expect("read sch");
    lexpr::from_str(&src).expect("parse sch as lexpr")
}

fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    match v.list_iter() {
        Some(it) => Box::new(it),
        None => Box::new(std::iter::empty()),
    }
}

fn head(v: &Value) -> Option<&str> {
    list_iter(v).next().and_then(|h| h.as_symbol())
}

fn as_str(v: &Value) -> Option<&str> {
    v.as_str().or_else(|| v.as_symbol())
}

fn as_f64(v: &Value) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    {
        v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))
    }
}

fn find_child<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    list_iter(v).find(|c| head(c) == Some(name))
}

fn children<'a>(v: &'a Value, name: &str) -> Vec<&'a Value> {
    list_iter(v).filter(|c| head(c) == Some(name)).collect()
}

const SYM_HALF_MM: f64 = 2.54;

fn placed_symbol_bboxes(root: &Value) -> Vec<(String, Bbox)> {
    let library = load_test_library();
    let mut out = Vec::new();
    for sym in children(root, "symbol") {
        let Some(at) = find_child(sym, "at") else {
            continue;
        };
        let mut it = list_iter(at);
        it.next();
        let Some(x) = it.next().and_then(as_f64) else {
            continue;
        };
        let Some(y) = it.next().and_then(as_f64) else {
            continue;
        };
        let rot_deg = it.next().and_then(as_f64).unwrap_or(0.0);
        let mirror_y = find_child(sym, "mirror")
            .and_then(|m| list_iter(m).nth(1).and_then(as_str))
            .is_some_and(|t| t.eq_ignore_ascii_case("y"));
        let mut refdes = String::new();
        let mut lib_id = String::new();
        if let Some(lid_node) = find_child(sym, "lib_id") {
            if let Some(s) = list_iter(lid_node).nth(1).and_then(as_str) {
                s.clone_into(&mut lib_id);
            }
        }
        for prop in children(sym, "property") {
            let mut pit = list_iter(prop);
            pit.next();
            let key = pit.next().and_then(as_str);
            let val = pit.next().and_then(as_str);
            if key == Some("Reference") {
                val.unwrap_or_default().clone_into(&mut refdes);
                break;
            }
        }
        if refdes.starts_with("#PWR") || lib_id.starts_with("power:") {
            // Power glyphs sit ON a host pin by design (V10). Skip —
            // they are not obstacles for wire routing or label placement.
            continue;
        }
        let bbox = library
            .lookup(&lib_id)
            .and_then(kicad_symbols::Symbol::body_bbox)
            .map_or(
                Bbox {
                    x0: x - SYM_HALF_MM,
                    y0: y - SYM_HALF_MM,
                    x1: x + SYM_HALF_MM,
                    y1: y + SYM_HALF_MM,
                },
                |local| body_bbox_to_world(local, x, y, rot_deg, mirror_y),
            );
        out.push((refdes, bbox));
    }
    out
}

/// Transform a symbol-local `LocalBbox` into world-frame `Bbox` using
/// the same convention as pin coordinates: rotate / mirror via
/// orientation, then eeschema y-flip `world_y = origin_y - local_y`,
/// take AABB of the four transformed corners.
fn body_bbox_to_world(
    local: kicad_symbols::LocalBbox,
    origin_x: f64,
    origin_y: f64,
    rot_degrees: f64,
    mirror_y: bool,
) -> Bbox {
    let rot_norm = rot_degrees.rem_euclid(360.0).round();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rot = rot_norm as u16;
    let rotation = match rot {
        90 => Rotation::R90,
        180 => Rotation::R180,
        270 => Rotation::R270,
        _ => Rotation::R0,
    };
    let orient = Orientation { rotation, mirror_y };
    let corners = [
        (local.x0, local.y0),
        (local.x0, local.y1),
        (local.x1, local.y0),
        (local.x1, local.y1),
    ];
    let mut x0 = f64::INFINITY;
    let mut y0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for (lx, ly) in corners {
        let (rx, ry) = orient.apply_point(lx, ly);
        let wx = origin_x + rx;
        let wy = origin_y - ry;
        if wx < x0 {
            x0 = wx;
        }
        if wx > x1 {
            x1 = wx;
        }
        if wy < y0 {
            y0 = wy;
        }
        if wy > y1 {
            y1 = wy;
        }
    }
    Bbox { x0, y0, x1, y1 }
}

fn wire_segments(root: &Value) -> Vec<(Pt, Pt)> {
    let mut out = Vec::new();
    for w in children(root, "wire") {
        let Some(pts) = find_child(w, "pts") else {
            continue;
        };
        let xys: Vec<&Value> = list_iter(pts).filter(|c| head(c) == Some("xy")).collect();
        if xys.len() < 2 {
            continue;
        }
        let a = xy(xys[0]);
        let b = xy(xys[1]);
        if let (Some(a), Some(b)) = (a, b) {
            out.push((a, b));
        }
    }
    out
}

fn xy(v: &Value) -> Option<Pt> {
    let mut it = list_iter(v);
    it.next();
    let x = it.next().and_then(as_f64)?;
    let y = it.next().and_then(as_f64)?;
    Some((x, y))
}

fn label_positions(root: &Value) -> Vec<(String, Pt)> {
    let mut out = Vec::new();
    for kind in ["label", "global_label"] {
        for node in children(root, kind) {
            let Some(name) = list_iter(node).nth(1).and_then(as_str) else {
                continue;
            };
            let Some(at) = find_child(node, "at") else {
                continue;
            };
            let mut it = list_iter(at);
            it.next();
            let Some(x) = it.next().and_then(as_f64) else {
                continue;
            };
            let Some(y) = it.next().and_then(as_f64) else {
                continue;
            };
            out.push((name.to_owned(), (x, y)));
        }
    }
    out
}

/// Every fixture the invariant suite grades.
///
/// This deliberately covers *all* emitted sheets, not just the classic
/// five: four fixtures (`opamp_inverting`, `opamp_definition_level`,
/// `port_shapes`, `rc_lowpass_ports`) were converted by the CLI but
/// graded by nothing, so defects in the port and hierarchical-sheet paths
/// — exercised by the newest features — were invisible to the suite.
const SHEETS: &[&str] = &[
    "rc_lowpass",
    "common_emitter",
    "multivibrator",
    "diff_pair",
    "opamp_inverting_real",
    "opamp_inverting",
    "port_shapes",
    "rc_lowpass_ports",
    "opamp_definition_level",
    "named_rails",
    "rc_phase_shift",
];

/// Per-fixture crossing budget. After the V11/V12 cascade + Steiner-
/// junction-move step + maze fallback, every router-fixable case is
/// gone across all five v0.1 fixtures. A non-zero budget here would
/// be a regression: every fixture should route clean.
fn v12_crossing_budget(_name: &str) -> usize {
    // `opamp_definition_level` PAID ITS DEBT. It carried an "OWED,
    // NOT ACCEPTED" budget of 4, whose comment named the exact
    // precondition for retiring it: "MUST ratchet down to 0 when the
    // seed defect is fixed". That seed defect — `place_seed`'s
    // within-bucket Y stride being a hardcoded 5 cells regardless of
    // body geometry, so two 10.16 mm opamp triangles seeded 6.35 mm
    // apart and RF1/RF2 ended up inside a foreign body — is fixed,
    // and the router no longer logs `skipping V12 enforcement` here.
    //
    // Every fixture routes clean, and must stay that way.
    0
}

#[test]
fn v12_wires_do_not_cross_foreign_symbol_bodies() {
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let bodies = placed_symbol_bboxes(&root);
        let wires = wire_segments(&root);
        let mut crossings = 0;
        for (refdes, bbox) in &bodies {
            for (a, b) in &wires {
                if bbox.intersects_segment(*a, *b) {
                    eprintln!(
                        "{name}: wire ({:.2},{:.2})→({:.2},{:.2}) crosses {refdes}'s body",
                        a.0, a.1, b.0, b.1,
                    );
                    crossings += 1;
                }
            }
        }
        let budget = v12_crossing_budget(name);
        assert!(
            crossings <= budget,
            "{name}: {crossings} foreign-body wire crossings > V12 budget {budget}",
        );
    }
}

// ---------------------------------------------------------------------------
// V11 — Wire/label–pin coincidence is electrical.
// ---------------------------------------------------------------------------

use kicad_symbols::Library;
use spice_diagnostics::FileId;
use std::collections::{HashMap, HashSet};

/// Quantise mm coords to integer micrometres for hash-keying. Inputs
/// sit on the 1.27 mm KiCad grid, so 1 µm resolution is comfortably
/// inside f64 precision and matches `spice-route` quantisation.
#[allow(clippy::cast_possible_truncation)]
fn qkey(x: f64, y: f64) -> (i64, i64) {
    ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64)
}

/// World pin position descriptor produced by [`world_pins_for_sheet`].
struct WorldPin {
    refdes: String,
    pin_number: String,
    x_mm: f64,
    y_mm: f64,
    net: String,
    /// Pin's outward direction in degrees (0=Right, 90=Down, 180=Left,
    /// 270=Up, file-y semantics). Mirrors `angle_to_direction` in
    /// `kicad-emitter::schematic`. Power glyphs synthesise a sentinel
    /// `u16::MAX` since they don't have a meaningful body-relative
    /// outward direction.
    angle: u16,
}

/// Load the standard fixture libraries used by every test fixture.
fn load_test_library() -> Library {
    let libs_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .join("crates/kicad-symbols/tests/fixtures");
    let device =
        Library::from_file(libs_dir.join("Device.kicad_sym")).expect("parse Device.kicad_sym");
    let sim = Library::from_file(libs_dir.join("Simulation_SPICE.kicad_sym"))
        .expect("parse Simulation_SPICE.kicad_sym");
    let amp = Library::from_file(libs_dir.join("Amplifier_Operational.kicad_sym"))
        .expect("parse Amplifier_Operational.kicad_sym");
    let power =
        Library::from_file(libs_dir.join("power.kicad_sym")).expect("parse power.kicad_sym");
    device.merge(sim).merge(amp).merge(power)
}

/// Decode a placed `(symbol …)` instance's `(at x y rot)` plus
/// optional `(mirror x|y)` token into an [`Orientation`] and translation.
fn placed_symbol_pose(sym: &Value) -> Option<(f64, f64, Orientation)> {
    let at = find_child(sym, "at")?;
    let mut it = list_iter(at);
    it.next();
    let x = it.next().and_then(as_f64)?;
    let y = it.next().and_then(as_f64)?;
    let rot_deg = it.next().and_then(as_f64).unwrap_or(0.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rot_u = ((rot_deg.round() as i64).rem_euclid(360)) as u16;
    let rotation = match rot_u {
        0 => Rotation::R0,
        90 => Rotation::R90,
        180 => Rotation::R180,
        270 => Rotation::R270,
        _ => return None,
    };
    let mirror_y = find_child(sym, "mirror")
        .and_then(|m| list_iter(m).nth(1).and_then(as_str))
        .is_some_and(|s| s == "y");
    Some((x, y, Orientation { rotation, mirror_y }))
}

fn placed_symbol_refdes_and_lib_id(sym: &Value) -> Option<(String, String)> {
    let mut lib_id = None;
    if let Some(lid) = find_child(sym, "lib_id")
        && let Some(s) = list_iter(lid).nth(1).and_then(as_str)
    {
        lib_id = Some(s.to_string());
    }
    let mut refdes = None;
    for prop in children(sym, "property") {
        let mut it = list_iter(prop);
        it.next();
        let key = it.next().and_then(as_str);
        let val = it.next().and_then(as_str);
        if key == Some("Reference") {
            refdes = val.map(str::to_owned);
            break;
        }
    }
    Some((refdes?, lib_id?))
}

/// Canonical net identity for V11 coincidence comparison.
///
/// A KiCad `power:*` glyph connects globally by its `Value` text, and the
/// emitter renders that Value as the *canonical* rail name (R-6): the
/// SPICE ground net `"0"` → `GND`, every other rail uppercased
/// (`vcc`→`VCC`, `vee`→`VEE`). The resolved SPICE netlist still carries
/// the raw lowercase token, so the verifier must apply the same
/// canonicalization to *both* sides before comparing net identity — a
/// pure case difference (`vcc` vs `VCC`) is the same net, not a foreign
/// short. This is the single normalization point that keeps V11's
/// string-equality model aligned with KiCad's by-Value power-net
/// connectivity.
fn canonical_net(net: &str) -> String {
    if net == "0" {
        "GND".to_string()
    } else {
        net.to_ascii_uppercase()
    }
}

/// Build the world-pin → net map for one fixture by:
///  1. Re-running `spice_resolve::resolve` on the SPICE source to
///     recover `(refdes, kicad_pin_number) → spice_net` for every
///     placed element.
///  2. Walking the emitted `.kicad_sch` placed symbols, transforming
///     each library pin's local coordinate through the placed
///     `(at … rot)` + `(mirror …)` pose via [`Orientation::apply_point`]
///     and the eeschema Y-flip-on-load quirk (world Y = origin Y - pin Y).
///
/// Power glyph instances (`power:*`) are intentionally included: a
/// signal-net wire that touches a `power:GND` pin would also be a V11
/// short, and the placer/router need to honour that constraint.
fn world_pins_for_sheet(spice_path: &std::path::Path, root: &Value) -> Vec<WorldPin> {
    let library = load_test_library();
    let source = std::fs::read_to_string(spice_path).expect("read spice fixture");
    let parsed = spice_parser::parse(&source, FileId(0)).expect("parse spice fixture");
    let resolved =
        spice_resolve::resolve(&parsed.netlist, &library).expect("resolve spice fixture");

    // Map (refdes -> Vec<(kicad_pin_number, spice_net)>) so we can join
    // against placed-instance pin lists. Refdes is the stable key.
    let mut by_refdes: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for el in &resolved.elements {
        let mut pairs = Vec::with_capacity(el.pin_mapping.len());
        for (i, kicad_pin) in el.pin_mapping.iter().enumerate() {
            if let Some(net) = el.nodes.get(i) {
                pairs.push((kicad_pin.clone(), net.clone()));
            }
        }
        by_refdes.insert(el.refdes.clone(), pairs);
    }

    let mut out: Vec<WorldPin> = Vec::new();
    for sym in children(root, "symbol") {
        let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
            continue;
        };
        // Skip power glyphs in the SPICE-driven map: their refdes
        // (`#PWR…`) isn't a SPICE element. They're handled below as
        // synthetic ground/power pins.
        if refdes.starts_with("#PWR") {
            continue;
        }
        let Some((ox, oy, orient)) = placed_symbol_pose(sym) else {
            continue;
        };
        let Some(lib_sym) = library.lookup(&lib_id) else {
            continue;
        };
        let pin_to_net: HashMap<&str, &str> = by_refdes
            .get(&refdes)
            .map(|v| v.iter().map(|(p, n)| (p.as_str(), n.as_str())).collect())
            .unwrap_or_default();
        for tp in lib_sym.pins_in(orient) {
            let wx = ox + tp.x;
            let wy = oy - tp.y;
            let net = match pin_to_net.get(tp.number.as_str()) {
                Some(n) => canonical_net(n),
                None => continue,
            };
            out.push(WorldPin {
                refdes: refdes.clone(),
                pin_number: tp.number.clone(),
                x_mm: wx,
                y_mm: wy,
                net,
                angle: tp.angle,
            });
        }
    }

    // Power glyphs: synthesise a single pin at the placement origin
    // carrying the glyph's net. The library's `power:GND` / `power:VCC`
    // pins sit at local (0, 0) by convention, so the world position is
    // simply the placement's `(at …)`. Net comes from the Value property
    // (`GND` glyphs have Value="0", `VCC`/`+5V`/… glyphs have Value=net).
    for sym in children(root, "symbol") {
        let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
            continue;
        };
        if !refdes.starts_with("#PWR") {
            continue;
        }
        let Some((ox, oy, _)) = placed_symbol_pose(sym) else {
            continue;
        };
        // The `Value` property carries the net the glyph anchors to.
        let mut net: Option<String> = None;
        for prop in children(sym, "property") {
            let mut it = list_iter(prop);
            it.next();
            let key = it.next().and_then(as_str);
            let val = it.next().and_then(as_str);
            if key == Some("Value") {
                net = val.map(str::to_owned);
                break;
            }
        }
        let Some(net) = net else { continue };
        let net = canonical_net(&net);
        let _ = lib_id;
        out.push(WorldPin {
            refdes,
            pin_number: "1".to_string(),
            x_mm: ox,
            y_mm: oy,
            net,
            angle: u16::MAX,
        });
    }

    out
}

/// Build a connected-components partition over wires and pins. Two
/// wire endpoints (or a wire endpoint and a pin coordinate) that
/// share an exact coordinate are unioned; thereafter every wire
/// segment is labelled with the connected component of either of its
/// endpoints.
struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            let p = self.parent[x];
            self.parent[x] = self.parent[p];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}

/// Pin coord-key → set of refdes/pin/net for diagnostic messages and
/// foreign-net checks.
type PinIndex = HashMap<(i64, i64), Vec<(String, String, String)>>;

fn build_pin_index(pins: &[WorldPin]) -> PinIndex {
    let mut out: PinIndex = HashMap::new();
    for p in pins {
        out.entry(qkey(p.x_mm, p.y_mm)).or_default().push((
            p.refdes.clone(),
            p.pin_number.clone(),
            p.net.clone(),
        ));
    }
    out
}

/// Quantised interior coords of an axis-aligned segment (exclusive of
/// the two endpoints). Steps along the 1.27 mm grid; pin coords always
/// align so this enumeration is exact for V11's coord-equality model.
fn interior_grid_coords(seg: &(Pt, Pt)) -> Vec<(i64, i64)> {
    const GRID_UM: i64 = 1270;
    let (a, b) = *seg;
    let ka = qkey(a.0, a.1);
    let kb = qkey(b.0, b.1);
    if ka == kb {
        return Vec::new();
    }
    let dx = kb.0 - ka.0;
    let dy = kb.1 - ka.1;
    if dx != 0 && dy != 0 {
        // Router emits axis-aligned segments only; a diagonal here is
        // already a defect, but bail out rather than enumerating
        // off-grid interior points.
        return Vec::new();
    }
    let mut out = Vec::new();
    if dx == 0 {
        let step: i64 = if dy > 0 { GRID_UM } else { -GRID_UM };
        let mut y = ka.1 + step;
        while (step > 0 && y < kb.1) || (step < 0 && y > kb.1) {
            out.push((ka.0, y));
            y += step;
        }
    } else {
        let step: i64 = if dx > 0 { GRID_UM } else { -GRID_UM };
        let mut x = ka.0 + step;
        while (step > 0 && x < kb.0) || (step < 0 && x > kb.0) {
            out.push((x, ka.1));
            x += step;
        }
    }
    out
}

#[test]
fn v11_pin_overlap_is_a_placer_bug() {
    // Companion to [`v11_no_foreign_pin_coincidence`]: surfaces any
    // *placer*-level pin-on-pin overlap (two distinct nets at the same
    // world coord before the router runs) explicitly. The V14
    // power-pin-orientation fix removed the last such overlap
    // (`opamp_inverting_real`'s X1 output vs VEE `-` pin), so the budget
    // is now **zero on every fixture** — a non-zero count is a
    // regression, never a budget to bump.
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let pins = world_pins_for_sheet(&src, &root);
        let pin_index = build_pin_index(&pins);
        let mut overlaps = 0usize;
        for list in pin_index.values() {
            let nets: HashSet<&str> = list.iter().map(|(_, _, n)| n.as_str()).collect();
            if nets.len() > 1 {
                overlaps += 1;
            }
        }
        assert_eq!(
            overlaps, 0,
            "{name}: expected 0 placer-level pin overlap(s), found {overlaps}"
        );
    }
}

/// V11 is a correctness invariant — KiCad merges any wire endpoint or
/// wire-interior coincidence with a foreign pin into an electrical
/// connection, which is a silent net short on schematic load. The
/// per-fixture budget is therefore **zero** across the board, with no
/// exceptions: the V14 power-pin-orientation fix removed the last
/// placer-level pin overlap (`opamp_inverting_real`), so every fixture
/// is now fully V11-enforced. A budget for a correctness invariant is a
/// contradiction in terms — if we cannot fix it, it is not a "budget"
/// but an `#[ignore]` test.
fn v11_violation_budget(_name: &str) -> usize {
    0
}

/// Build wire-only union-find over wire-endpoint coordinates. Returns
/// `(coord_idx, uf, coords)` so callers can map any coordinate back to
/// its connected component. Pin coords are NOT unioned here — see the
/// "phase B" assignment below.
#[allow(clippy::type_complexity)]
fn build_wire_uf(wires: &[(Pt, Pt)]) -> (HashMap<(i64, i64), usize>, UnionFind, Vec<(i64, i64)>) {
    let mut coord_idx: HashMap<(i64, i64), usize> = HashMap::new();
    let mut coords: Vec<(i64, i64)> = Vec::new();
    let mut intern = |k: (i64, i64), coords: &mut Vec<(i64, i64)>| -> usize {
        if let Some(&i) = coord_idx.get(&k) {
            i
        } else {
            let i = coords.len();
            coord_idx.insert(k, i);
            coords.push(k);
            i
        }
    };
    for (a, b) in wires {
        intern(qkey(a.0, a.1), &mut coords);
        intern(qkey(b.0, b.1), &mut coords);
    }
    let mut uf = UnionFind::new(coords.len());
    for (a, b) in wires {
        let ia = coord_idx[&qkey(a.0, a.1)];
        let ib = coord_idx[&qkey(b.0, b.1)];
        uf.union(ia, ib);
    }
    (coord_idx, uf, coords)
}

/// For each wire-island (connected component of the wire-only UF),
/// determine its single owning net by surveying pin coords coincident
/// with the island's wire endpoints AND interior grid points. Returns
/// `(island_root -> nominal_net, extra_violations)` where
/// `extra_violations` reports any pins on a multi-net island that
/// disagree with the lexicographically-smallest nominal net (a silent
/// short).
#[allow(clippy::cast_precision_loss, clippy::type_complexity)]
fn assign_island_nets(
    wires: &[(Pt, Pt)],
    coord_idx: &HashMap<(i64, i64), usize>,
    uf: &mut UnionFind,
    pin_index: &PinIndex,
    name: &str,
) -> (HashMap<usize, String>, Vec<String>) {
    // island root -> set of (coord, refdes, pin_no, net) touching it.
    let mut island_pins: HashMap<usize, Vec<((i64, i64), String, String, String)>> = HashMap::new();
    // Endpoint coincidences.
    for (a, b) in wires {
        for k in [qkey(a.0, a.1), qkey(b.0, b.1)] {
            let r = uf.find(coord_idx[&k]);
            if let Some(list) = pin_index.get(&k) {
                for (refdes, pin_no, net) in list {
                    island_pins.entry(r).or_default().push((
                        k,
                        refdes.clone(),
                        pin_no.clone(),
                        net.clone(),
                    ));
                }
            }
        }
    }
    // Interior coincidences also contribute (a wire whose interior
    // passes through a pin is electrically connected per V11).
    for (a, b) in wires {
        let ka = qkey(a.0, a.1);
        let r = uf.find(coord_idx[&ka]);
        for k in interior_grid_coords(&(*a, *b)) {
            if let Some(list) = pin_index.get(&k) {
                for (refdes, pin_no, net) in list {
                    island_pins.entry(r).or_default().push((
                        k,
                        refdes.clone(),
                        pin_no.clone(),
                        net.clone(),
                    ));
                }
            }
        }
    }

    let mut comp_net: HashMap<usize, String> = HashMap::new();
    let mut extras: Vec<String> = Vec::new();
    for (root, pins) in &island_pins {
        let mut nets: Vec<&str> = pins.iter().map(|(_, _, _, n)| n.as_str()).collect();
        nets.sort_unstable();
        nets.dedup();
        if nets.len() == 1 {
            comp_net.insert(*root, nets[0].to_string());
        } else {
            // Silent short — multiple distinct nets on the same wire
            // island. Pick the lex-smallest as nominal so subsequent
            // foreign-pin checks have a deterministic owner.
            let nominal = nets[0].to_string();
            for (coord, refdes, pin_no, net) in pins {
                if net != &nominal {
                    extras.push(format!(
                        "{name}: silent short — wire island carries pins from nets {nets:?}; \
                         pin {refdes}.{pin_no} at ({:.3},{:.3}) on net {:?} differs from \
                         nominal {:?}",
                        coord.0 as f64 / 1000.0,
                        coord.1 as f64 / 1000.0,
                        net,
                        nominal,
                    ));
                }
            }
            comp_net.insert(*root, nominal);
        }
    }
    (comp_net, extras)
}

#[allow(
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap
)]
#[test]
fn v11_no_foreign_pin_coincidence() {
    let mut hard_failures: Vec<String> = Vec::new();
    for name in SHEETS {
        let mut failures: Vec<String> = Vec::new();
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let pins = world_pins_for_sheet(&src, &root);
        let pin_index = build_pin_index(&pins);

        // Sanity: a placer-level bug would put two distinct nets on
        // the same coord. Surface that explicitly rather than letting
        // it silently dictate the wire net below.
        for (coord, list) in &pin_index {
            let nets: HashSet<&str> = list.iter().map(|(_, _, n)| n.as_str()).collect();
            if nets.len() > 1 {
                failures.push(format!(
                    "{name}: pin overlap — coord ({:.3}, {:.3}) hosts pins from nets {:?} \
                     (placer bug, not a router bug)",
                    coord.0 as f64 / 1000.0,
                    coord.1 as f64 / 1000.0,
                    nets,
                ));
            }
        }

        // Phase A — wire-only union-find: connected components over
        // wire-endpoint coords ONLY. Pin coords are intentionally NOT
        // unioned with wire endpoints — that's the bug the previous
        // verifier had (a foreign-pin endpoint coincidence got
        // silently absorbed into the wire's net by union-find).
        let wires = wire_segments(&root);
        let (coord_idx, mut uf, _coords) = build_wire_uf(&wires);

        // Phase B — assign each wire-island a single owning net by
        // surveying every pin coord that touches the island (endpoint
        // or interior). Multi-net islands are silent shorts; record
        // every non-nominal pin as a violation.
        let (comp_net, extras) = assign_island_nets(&wires, &coord_idx, &mut uf, &pin_index, name);
        failures.extend(extras);

        // Phase C — for every wire segment, check endpoint and interior
        // pin coincidences against the island's nominal net.
        for (a, b) in &wires {
            let ka = qkey(a.0, a.1);
            let kb = qkey(b.0, b.1);
            let ia = coord_idx[&ka];
            let ra = uf.find(ia);
            let net = match comp_net.get(&ra) {
                Some(n) => n.clone(),
                // Unlabelled component (a wire island with zero pin
                // contact). Not a V11 violation per se — skip.
                None => continue,
            };
            for k in [ka, kb] {
                if let Some(pins_at) = pin_index.get(&k) {
                    for (refdes, pin_no, pin_net) in pins_at {
                        if pin_net != &net {
                            failures.push(format!(
                                "{name}: wire ({:.3},{:.3})→({:.3},{:.3}) on net {:?} \
                                 touches pin {refdes}.{pin_no} at ({:.3},{:.3}) on \
                                 foreign net {:?}",
                                a.0,
                                a.1,
                                b.0,
                                b.1,
                                net,
                                k.0 as f64 / 1000.0,
                                k.1 as f64 / 1000.0,
                                pin_net,
                            ));
                        }
                    }
                }
            }
            for k in interior_grid_coords(&(*a, *b)) {
                if let Some(pins_at) = pin_index.get(&k) {
                    for (refdes, pin_no, pin_net) in pins_at {
                        if pin_net != &net {
                            failures.push(format!(
                                "{name}: wire ({:.3},{:.3})→({:.3},{:.3}) on net {:?} \
                                 passes through pin {refdes}.{pin_no} at \
                                 ({:.3},{:.3}) on foreign net {:?}",
                                a.0,
                                a.1,
                                b.0,
                                b.1,
                                net,
                                k.0 as f64 / 1000.0,
                                k.1 as f64 / 1000.0,
                                pin_net,
                            ));
                        }
                    }
                }
            }
        }

        // Phase D — label anchors coincident with a pin must agree on net.
        for (lname, pos) in label_positions(&root) {
            let lname = canonical_net(&lname);
            let k = qkey(pos.0, pos.1);
            if let Some(pins_at) = pin_index.get(&k) {
                for (refdes, pin_no, pin_net) in pins_at {
                    if pin_net != &lname {
                        failures.push(format!(
                            "{name}: label {lname:?} at ({:.3},{:.3}) coincides with pin \
                             {refdes}.{pin_no} on foreign net {pin_net:?}",
                            pos.0, pos.1,
                        ));
                    }
                }
            }
        }

        let budget = v11_violation_budget(name);
        if failures.len() > budget {
            hard_failures.push(format!(
                "{name}: {} V11 violations > budget {budget}:\n    {}",
                failures.len(),
                failures.join("\n    "),
            ));
        }
    }
    assert!(
        hard_failures.is_empty(),
        "V11 foreign-pin coincidence regressions:\n  {}",
        hard_failures.join("\n  "),
    );
}

// ---------------------------------------------------------------------------
// V12 / V13 verifiers (existing).
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// V13 text-bbox machinery shared by parts (1), (2), and (3).
// ---------------------------------------------------------------------------

/// True if a `(property …)` s-expression is marked hidden in either
/// the legacy form (`(hide)`) or the new `(effects (hide yes))` form.
fn property_hidden(prop: &Value) -> bool {
    for c in list_iter(prop) {
        if head(c) == Some("hide") {
            return true;
        }
        if head(c) == Some("effects") {
            for e in list_iter(c) {
                if head(e) == Some("hide") {
                    // (hide yes) — check the argument.
                    let v = list_iter(e).nth(1).and_then(as_str);
                    if v == Some("yes") || v.is_none() {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Extract `(at x y rot)` from any sexpr that has one as a child.
fn at_xy_rot(node: &Value) -> Option<(f64, f64, u16)> {
    let at = find_child(node, "at")?;
    let mut it = list_iter(at);
    it.next();
    let x = it.next().and_then(as_f64)?;
    let y = it.next().and_then(as_f64)?;
    let rot = it.next().and_then(as_f64).unwrap_or(0.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rot_u = ((rot.round() as i64).rem_euclid(360)) as u16;
    Some((x, y, rot_u))
}

/// Pull the font size (mm) out of an `(effects (font (size w h)) …)`.
fn effects_font_size(node: &Value) -> Option<f64> {
    let eff = find_child(node, "effects")?;
    let font = find_child(eff, "font")?;
    let size = find_child(font, "size")?;
    let mut it = list_iter(size);
    it.next();
    it.next().and_then(as_f64)
}

/// The `(shape …)` token of a label node, if any.
fn label_shape(node: &Value) -> Option<&str> {
    find_child(node, "shape").and_then(|s| list_iter(s).nth(1).and_then(as_str))
}

/// Collect every emitted plain-label and global-label as
/// (net_name, anchor, rot_deg, kind).
#[allow(clippy::similar_names)]
fn labels_with_kind(root: &Value) -> Vec<(String, Pt, u16, TextKind)> {
    let mut out = Vec::new();
    for sx_tag in ["label", "global_label"] {
        for node in children(root, sx_tag) {
            let Some(name) = list_iter(node).nth(1).and_then(as_str) else {
                continue;
            };
            let Some((x, y, rot)) = at_xy_rot(node) else {
                continue;
            };
            // A global label's chevron shape decides how far KiCad pushes
            // the text off the anchor — calibrated in `rendered_text.rs`.
            let lkind = if sx_tag == "label" {
                TextKind::PlainLabel
            } else {
                TextKind::global_label(label_shape(node))
            };
            out.push((name.to_owned(), (x, y), rot, lkind));
        }
    }
    out
}

/// Collect each placed `(symbol …)`'s visible Reference and Value
/// property bboxes. Power glyphs (`#PWR…`) are skipped — their text
/// is part of the standard library glyph and never collides with
/// other in-sheet text in practice.
/// True if a `(property …)` carries an explicit `(justify …)` inside its
/// `(effects …)`. KiCad centres a field horizontally when none is given,
/// so this selects between the left-anchored and centred bbox models —
/// verified against `kicad-cli sch export svg` (power-glyph net-name text
/// is emitted without a justify and renders centred on its anchor).
fn property_has_justify(prop: &Value) -> bool {
    children(prop, "effects")
        .iter()
        .any(|e| !children(e, "justify").is_empty())
}

/// Direction a symbol's field text actually reads on screen.
///
/// A field's own `(at … 0)` is not what KiCad draws: the parent symbol's
/// transform is applied on top of it. `SCH_FIELD::GetDrawRotation` swaps
/// horizontal ↔ vertical for a 90°/270° symbol and
/// `GetEffectiveHorizJustify` flips left ↔ right for a 180° rotation or a
/// Y mirror (`../kicad-source/eeschema/sch_field.cpp:396-415, 446-501`).
/// Verified against `kicad-cli sch export svg` across every orientation
/// the placer emits. Mirrors `kicad_emitter`'s `field_render_rotation` —
/// keep the two in step.
fn field_render_rotation(sym: &Value) -> u16 {
    let rot = at_xy_rot(sym).map_or(0, |(_, _, r)| r);
    let mirrored_y = children(sym, "mirror")
        .first()
        .and_then(|m| list_iter(m).nth(1).and_then(as_str))
        == Some("y");
    if mirrored_y { (540 - rot) % 360 } else { rot }
}

fn property_bboxes(root: &Value) -> Vec<(String, Bbox)> {
    let mut out = Vec::new();
    for sym in children(root, "symbol") {
        let mut refdes = String::new();
        for prop in children(sym, "property") {
            let mut it = list_iter(prop);
            it.next();
            let key = it.next().and_then(as_str);
            let val = it.next().and_then(as_str);
            if key == Some("Reference") {
                val.unwrap_or_default().clone_into(&mut refdes);
                break;
            }
        }
        if refdes.starts_with("#PWR") {
            continue;
        }
        for prop in children(sym, "property") {
            if property_hidden(prop) {
                continue;
            }
            let mut it = list_iter(prop);
            it.next();
            let key = it.next().and_then(as_str).unwrap_or("");
            let val = it.next().and_then(as_str).unwrap_or("");
            let tkind = match key {
                _ if !property_has_justify(prop) => TextKind::CenteredValue,
                "Reference" => TextKind::PropertyReference,
                "Value" => TextKind::PropertyValue,
                _ => continue,
            };
            if !matches!(key, "Reference" | "Value") {
                continue;
            }
            let Some((px, py, _)) = at_xy_rot(prop) else {
                continue;
            };
            let size = effects_font_size(prop).unwrap_or(1.27);
            let bbox = text_bbox(val, (px, py), size, field_render_rotation(sym), tkind);
            out.push((format!("{refdes}.{key}"), bbox));
        }
    }
    out
}

#[test]
fn v13_labels_dont_overlap_symbol_body() {
    // V13 part (1): label *text bbox* must not intersect a symbol's
    // body bbox. Stricter than the previous point-in-bbox check —
    // a label whose anchor sits just outside the body but whose
    // text rendering crosses into the body is still a defect.
    // Zero label↔body overlaps on every fixture. The routing-aware
    // orientation-refinement phase (Layout phase 4.5) re-oriented
    // opamp_inverting_real's X1/RIN/RF so the `out` label no longer
    // grazes RF's body (ratcheted 1 → 0); common_emitter was already 0.
    // A regression here is a defect, not a budget to bump.
    let body_overlap_budget = |_name: &str| -> usize { 0 };
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let bodies = placed_symbol_bboxes(&root);
        let labels = labels_with_kind(&root);
        let mut hits = 0;
        for (lname, anchor, rot, kind) in &labels {
            let lbbox = text_bbox(lname, *anchor, 1.27, *rot, *kind);
            for (refdes, body) in &bodies {
                if lbbox.intersects(body) {
                    eprintln!("{name}: label \"{lname}\" bbox overlaps {refdes}'s body",);
                    hits += 1;
                }
            }
        }
        let budget = body_overlap_budget(name);
        assert!(
            hits <= budget,
            "{name}: {hits} label↔body overlaps > V13(1) budget {budget}",
        );
    }
}

#[test]
fn v13_labels_dont_overlap_property_text() {
    // V13 part (2): a label's rendered text bbox must not overlap any
    // visible Reference / Value property's text bbox.
    // After Step 5 (property anchors offset right of body, left-justify)
    // and Step 6 (label rotation away from body), every v0.1 fixture
    // routes clean. Zero everywhere — a regression here is a defect.
    let budget = |_name: &str| -> usize { 0 };
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let props = property_bboxes(&root);
        let labels = labels_with_kind(&root);
        let mut hits = 0;
        for (lname, anchor, rot, kind) in &labels {
            let lbbox = text_bbox(lname, *anchor, 1.27, *rot, *kind);
            for (pname, pbbox) in &props {
                if lbbox.intersects(pbbox) {
                    eprintln!("{name}: label \"{lname}\" bbox overlaps property {pname}",);
                    hits += 1;
                }
            }
        }
        let b = budget(name);
        assert!(
            hits <= b,
            "{name}: {hits} label↔property text overlaps > V13(2) budget {b}",
        );
    }
}

#[allow(clippy::too_many_lines)]
#[test]
fn v13_label_anchor_not_on_foreign_wire_interior() {
    // V13 part (3): label anchor coordinate must not lie strictly
    // inside any wire segment whose net is different from the label's
    // own net. (V11 already covers the pin-coincidence subcase.)
    //
    // Net classification reuses the union-find construction from V11:
    // a wire's net is the connected component of its endpoints in the
    // pin-coord ∪ wire-endpoint graph, with each pin coord pulled
    // into its stated net.
    let budget = |_name: &str| -> usize { 0 };
    let mut hard_failures: Vec<String> = Vec::new();
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let pins = world_pins_for_sheet(&src, &root);
        let pin_index = build_pin_index(&pins);
        let wires = wire_segments(&root);
        let (coord_idx, mut uf, _coords) = build_wire_uf(&wires);
        let (comp_net, _extras) = assign_island_nets(&wires, &coord_idx, &mut uf, &pin_index, name);

        // For each label, walk every wire segment whose net != label's.
        // Test whether the label's anchor sits strictly between the
        // segment's endpoints (axis-aligned only).
        let labels = label_positions(&root);
        let mut hits = 0;
        for (lname, pos) in &labels {
            let lname = canonical_net(lname);
            let lk = qkey(pos.0, pos.1);
            for (a, b) in &wires {
                let ka = qkey(a.0, a.1);
                let ia = coord_idx[&ka];
                let ra = uf.find(ia);
                let Some(wnet) = comp_net.get(&ra) else {
                    continue;
                };
                if wnet == &lname {
                    continue;
                }
                let kb = qkey(b.0, b.1);
                if lk == ka || lk == kb {
                    // V11 covers the endpoint case; not our concern.
                    continue;
                }
                // Axis-aligned strict interior check.
                let on_interior = if ka.0 == kb.0 && ka.0 == lk.0 {
                    let lo = ka.1.min(kb.1);
                    let hi = ka.1.max(kb.1);
                    lk.1 > lo && lk.1 < hi
                } else if ka.1 == kb.1 && ka.1 == lk.1 {
                    let lo = ka.0.min(kb.0);
                    let hi = ka.0.max(kb.0);
                    lk.0 > lo && lk.0 < hi
                } else {
                    false
                };
                if on_interior {
                    eprintln!(
                        "{name}: label \"{lname}\" at ({:.3},{:.3}) on interior of foreign-net \
                         wire ({:.3},{:.3})→({:.3},{:.3}) (net {wnet:?})",
                        pos.0, pos.1, a.0, a.1, b.0, b.1,
                    );
                    hits += 1;
                }
            }
        }
        let b = budget(name);
        if hits > b {
            hard_failures.push(format!(
                "{name}: {hits} label↔foreign-wire-interior coincidences > V13(3) budget {b}"
            ));
        }
    }
    assert!(
        hard_failures.is_empty(),
        "V13(3) regressions:\n  {}",
        hard_failures.join("\n  "),
    );
}

/// Collect each placed `power:*` glyph's *body* bbox (world frame) plus
/// its visible `Value` net-name text bbox. These are the two pieces of
/// geometry a sheet-port glyph hangs into the strip beside the sheet's
/// left edge; the V13 sheet-glyph-clearance verifier asserts neither
/// touches a neighbouring component's value text.
fn power_glyph_bboxes(root: &Value) -> Vec<(String, Bbox)> {
    let library = load_test_library();
    let mut out = Vec::new();
    for sym in children(root, "symbol") {
        let mut lib_id = String::new();
        if let Some(lid_node) = find_child(sym, "lib_id")
            && let Some(s) = list_iter(lid_node).nth(1).and_then(as_str)
        {
            s.clone_into(&mut lib_id);
        }
        // PWR_FLAG carries no drawn body the reader confuses with a
        // glyph; the rail glyphs (GND/VCC/VEE/…) are the offenders.
        if !lib_id.starts_with("power:") || lib_id == "power:PWR_FLAG" {
            continue;
        }
        let mut refdes = String::new();
        for prop in children(sym, "property") {
            let mut it = list_iter(prop);
            it.next();
            if it.next().and_then(as_str) == Some("Reference") {
                it.next()
                    .and_then(as_str)
                    .unwrap_or_default()
                    .clone_into(&mut refdes);
                break;
            }
        }
        let Some((gx, gy, grot)) = at_xy_rot(sym) else {
            continue;
        };
        let mirror_y = find_child(sym, "mirror")
            .and_then(|m| list_iter(m).nth(1).and_then(as_str))
            .is_some_and(|t| t.eq_ignore_ascii_case("y"));
        // Glyph body bbox (world).
        if let Some(local) = library
            .lookup(&lib_id)
            .and_then(kicad_symbols::Symbol::body_bbox)
        {
            out.push((
                format!("{refdes}({lib_id}).body"),
                body_bbox_to_world(local, gx, gy, f64::from(grot), mirror_y),
            ));
        }
        // Glyph net-name text bbox (the visible `Value` property).
        for prop in children(sym, "property") {
            if property_hidden(prop) {
                continue;
            }
            let mut it = list_iter(prop);
            it.next();
            if it.next().and_then(as_str) != Some("Value") {
                continue;
            }
            let val = it.next().and_then(as_str).unwrap_or("");
            let Some((px, py, prot)) = at_xy_rot(prop) else {
                continue;
            };
            let size = effects_font_size(prop).unwrap_or(1.27);
            out.push((
                format!("{refdes}({lib_id}).Value"),
                // Power-glyph Value text carries no justify → KiCad
                // centres it horizontally (see `TextKind::CenteredValue`).
                text_bbox(val, (px, py), size, prot, TextKind::CenteredValue),
            ));
        }
    }
    out
}

/// Collect each non-power placed component's visible `(property "Value"
/// …)` text bbox — the "neighbour value text" a sheet-port glyph must
/// not crowd. Reuses [`property_bboxes`] (which already skips `#PWR`)
/// and keeps only the `.Value` entries.
fn neighbour_value_text_bboxes(root: &Value) -> Vec<(String, Bbox)> {
    property_bboxes(root)
        .into_iter()
        .filter(|(name, _)| name.ends_with(".Value"))
        .collect()
}

#[test]
fn sheet_port_glyphs_clear_neighbour_text() {
    // V13 — a hierarchical sheet's left-edge port pins hang `power:*`
    // glyphs (GND/VCC/VEE) into the strip beside the sheet. Those glyph
    // bodies AND their net-name labels must not crowd a neighbouring
    // component's rendered value text. The sheet de-overlap reserves the
    // glyph zone against neighbour *bodies* only; without folding the
    // neighbour's value-text width into the obstacle the sheet stops too
    // far left and a glyph (or its label) lands on e.g. RF's "10k".
    //
    // Budget 0 across every fixture that emits a sheet — this is a
    // ratchet, not a knob; a regression is a defect to diagnose, not a
    // budget to bump.
    let budget = |_name: &str| -> usize { 0 };
    // Every fixture that can emit a `(sheet …)` block. Fixtures without
    // one contribute zero glyph-on-sheet geometry and pass trivially.
    let with_sheets: Vec<&str> = {
        let mut v = vec!["opamp_inverting"];
        v.extend_from_slice(SHEETS);
        v
    };
    for name in with_sheets {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        // Only meaningful when a hierarchical sheet is present.
        if children(&root, "sheet").is_empty() {
            continue;
        }
        let glyphs = power_glyph_bboxes(&root);
        let neighbour_text = neighbour_value_text_bboxes(&root);
        let mut hits = 0;
        for (gname, gbbox) in &glyphs {
            for (tname, tbbox) in &neighbour_text {
                if gbbox.intersects(tbbox) {
                    eprintln!(
                        "{name}: sheet-port glyph {gname} overlaps neighbour value text {tname}",
                    );
                    hits += 1;
                }
            }
        }
        let b = budget(name);
        assert!(
            hits <= b,
            "{name}: {hits} sheet-glyph↔neighbour-value-text overlaps > budget {b}",
        );
    }
}

/// Collect every VISIBLE on-sheet text bbox that V13 part (4) governs:
///  * each placed component's visible `(property "Reference" …)` and
///    `(property "Value" …)` text, AND
///  * each `power:*` glyph's `(property "Value" …)` (the net-name text)
///    when it is visible.
///
/// Unlike [`property_bboxes`] this does NOT skip `#PWR` symbols — the
/// power-glyph net-name text is exactly the dominant collision class
/// (host Reference/Value ↔ power-glyph net name) ISSUE-5 targets. A
/// hidden property (`#PWR` Reference once hidden, or any `(hide yes)`)
/// reserves no bbox.
fn visible_text_bboxes(root: &Value) -> Vec<(String, Bbox)> {
    let mut out = Vec::new();
    for sym in children(root, "symbol") {
        let mut refdes = String::new();
        for prop in children(sym, "property") {
            let mut it = list_iter(prop);
            it.next();
            if it.next().and_then(as_str) == Some("Reference") {
                it.next()
                    .and_then(as_str)
                    .unwrap_or_default()
                    .clone_into(&mut refdes);
                break;
            }
        }
        for prop in children(sym, "property") {
            if property_hidden(prop) {
                continue;
            }
            let mut it = list_iter(prop);
            it.next();
            let key = it.next().and_then(as_str).unwrap_or("");
            let val = it.next().and_then(as_str).unwrap_or("");
            let tkind = match key {
                _ if !property_has_justify(prop) => TextKind::CenteredValue,
                "Reference" => TextKind::PropertyReference,
                "Value" => TextKind::PropertyValue,
                _ => continue,
            };
            if !matches!(key, "Reference" | "Value") {
                continue;
            }
            let Some((px, py, _)) = at_xy_rot(prop) else {
                continue;
            };
            let size = effects_font_size(prop).unwrap_or(1.27);
            let bbox = text_bbox(val, (px, py), size, field_render_rotation(sym), tkind);
            out.push((format!("{refdes}.{key}"), bbox));
        }
    }
    out
}

/// World-frame bboxes of every VISIBLE symbol-internal pin-name and
/// pin-number text, tagged `refdes.pin#`. Power glyphs (`#PWR…`) draw
/// no pin labels and are skipped. Each local pin-text bbox (from
/// [`kicad_symbols::Symbol::pin_text_local_bboxes`]) is transformed
/// through the placed pose with the same orientation + eeschema y-flip
/// used for symbol bodies, so the box agrees with the emitter's nudge
/// pass.
fn pin_text_bboxes(root: &Value) -> Vec<(String, Bbox)> {
    let library = load_test_library();
    let mut out = Vec::new();
    for sym in children(root, "symbol") {
        let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
            continue;
        };
        if refdes.starts_with("#PWR") || lib_id.starts_with("power:") {
            continue;
        }
        let Some((ox, oy, orient)) = placed_symbol_pose(sym) else {
            continue;
        };
        let Some(lib_sym) = library.lookup(&lib_id) else {
            continue;
        };
        #[allow(clippy::cast_lossless)]
        let rot_deg = f64::from(orient.rotation.degrees());
        for (i, local) in lib_sym.pin_text_local_bboxes().into_iter().enumerate() {
            let bbox = body_bbox_to_world(local, ox, oy, rot_deg, orient.mirror_y);
            out.push((format!("{refdes}.pintext{i}"), bbox));
        }
    }
    out
}

#[test]
fn v13_property_text_no_pin_text_overlap() {
    // V13 part (5): a symbol's VISIBLE Reference / Value property text
    // must not overprint VISIBLE symbol-internal pin-name / pin-number
    // text (its own or a neighbour's). The transistor `QGENERIC` Value
    // over the `B`/`C`/`E` pin names and `1`/`2`/`3` numbers is the
    // motivating defect (R-4). Budget is a ratchet: per-fixture
    // literals record the measured post-fix high-water mark and only
    // ever go down. After the pin-text-aware `nudge_property_text`
    // pass every fixture routes clean — 0 across the board. A
    // regression here is a defect, never a budget to bump.
    let budget = |_name: &str| -> usize { 0 };
    let mut failures: Vec<String> = Vec::new();
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let props = property_bboxes(&root);
        let pintexts = pin_text_bboxes(&root);
        let mut hits = 0;
        for (pname, pbox) in &props {
            for (tname, tbox) in &pintexts {
                if pbox.intersects(tbox) {
                    eprintln!("{name}: property {pname} overlaps pin-text {tname}");
                    hits += 1;
                }
            }
        }
        let b = budget(name);
        if hits > b {
            failures.push(format!(
                "{name}: {hits} property↔pin-text overlaps > V13(5) budget {b}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "V13(5) regressions:\n  {}",
        failures.join("\n  "),
    );
}

#[test]
fn v13_labels_clear_power_glyph_value_text() {
    // V13 part (6): a label's text must not overprint a power glyph's
    // visible net-name `Value` text. Labels are emitted on signal nets and
    // glyphs on rails, so every such pair is cross-net by construction.
    //
    // This pair class had no verifier until the renderer-measured audit
    // found `GND`x`in` on common_emitter and `VCC`x`c1` on multivibrator
    // surviving a suite whose every budget already read 0 — the classes
    // that ARE checked were all clean. Budget 0 from the outset: the
    // glyph-value nudge pass now takes labels as obstacles, so there is
    // nothing to ratchet down from.
    let mut failures: Vec<String> = Vec::new();
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let labels = labels_with_kind(&root);
        let glyph_values = power_glyph_value_text_bboxes(&root);
        let mut hits = 0;
        for (lname, anchor, rot, kind) in &labels {
            let lbox = text_bbox(lname, *anchor, 1.27, *rot, *kind);
            for (gname, gbox) in &glyph_values {
                if lbox.intersects(gbox) {
                    eprintln!("{name}: label {lname:?} overlaps glyph value {gname}");
                    hits += 1;
                }
            }
        }
        if hits > 0 {
            failures.push(format!(
                "{name}: {hits} label↔glyph-value overlaps > budget 0"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "V13(6) regressions:\n  {}",
        failures.join("\n  "),
    );
}

#[test]
fn v13_labels_no_mutual_overlap() {
    // V13 part (8): no two labels may overprint each other. Each net's
    // label is chosen independently, so nothing prevented two of them
    // landing on the same spot until `label_specs` started accumulating
    // the labels it had already placed as obstacles.
    //
    // Found by the renderer-measured audit (`out1`x`out2` on
    // opamp_definition_level) after multi-anchor placement moved one onto
    // the other — a good illustration of why this class needs its own
    // check rather than being assumed impossible. Budget 0.
    let mut failures: Vec<String> = Vec::new();
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let labels = labels_with_kind(&root);
        let boxes: Vec<(&String, Bbox)> = labels
            .iter()
            .map(|(n, anchor, rot, kind)| (n, text_bbox(n, *anchor, 1.27, *rot, *kind)))
            .collect();
        let mut hits = 0;
        for i in 0..boxes.len() {
            for j in (i + 1)..boxes.len() {
                if boxes[i].1.intersects(&boxes[j].1) {
                    eprintln!(
                        "{name}: label {:?} overlaps label {:?}",
                        boxes[i].0, boxes[j].0
                    );
                    hits += 1;
                }
            }
        }
        if hits > 0 {
            failures.push(format!("{name}: {hits} label↔label overlaps > budget 0"));
        }
    }
    assert!(
        failures.is_empty(),
        "V13(8) regressions:\n  {}",
        failures.join("\n  "),
    );
}

#[test]
fn v13_labels_clear_pin_text() {
    // V13 part (7): a label's text must not overprint visible
    // symbol-internal pin-name / pin-number text. Pin text is fixed
    // geometry belonging to the symbol body, so the label is the side that
    // must move — `label_rotation_obstacles` now includes it at the
    // emitter's call site.
    //
    // Also previously unverified; the renderer-measured audit found the
    // `1` pin number under the `out` label on opamp_inverting_real.
    // Budget 0.
    let mut failures: Vec<String> = Vec::new();
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let labels = labels_with_kind(&root);
        let pintexts = pin_text_bboxes(&root);
        let mut hits = 0;
        for (lname, anchor, rot, kind) in &labels {
            let lbox = text_bbox(lname, *anchor, 1.27, *rot, *kind);
            for (tname, tbox) in &pintexts {
                if lbox.intersects(tbox) {
                    eprintln!("{name}: label {lname:?} overlaps pin-text {tname}");
                    hits += 1;
                }
            }
        }
        if hits > 0 {
            failures.push(format!("{name}: {hits} label↔pin-text overlaps > budget 0"));
        }
    }
    assert!(
        failures.is_empty(),
        "V13(7) regressions:\n  {}",
        failures.join("\n  "),
    );
}

#[test]
fn v13_property_text_no_mutual_overlap() {
    // V13 part (4): no two VISIBLE on-sheet text bboxes may overlap —
    // host Reference/Value vs each other AND vs power-glyph net-name
    // Value text. (V13 parts 1–3 are label-anchored; this part closes
    // the property-text ↔ property-text / power-glyph gap, ISSUE-5.)
    //
    // Budget is a ratchet: per-fixture literals record the measured
    // post-fix high-water mark and only ever go down. After hiding the
    // `#PWRn` Reference and the decoration-phase text-nudge pass, every
    // fixture routes clean — 0 across the board. A regression here is a
    // defect, never a budget to bump.
    let budget = |_name: &str| -> usize { 0 };
    let mut failures: Vec<String> = Vec::new();
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let texts = visible_text_bboxes(&root);
        let mut hits = 0;
        for i in 0..texts.len() {
            for j in (i + 1)..texts.len() {
                if texts[i].1.intersects(&texts[j].1) {
                    eprintln!("{name}: text {} overlaps text {}", texts[i].0, texts[j].0,);
                    hits += 1;
                }
            }
        }
        let b = budget(name);
        if hits > b {
            failures.push(format!(
                "{name}: {hits} visible-text mutual overlaps > V13(4) budget {b}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "V13(4) regressions:\n  {}",
        failures.join("\n  "),
    );
}

// ---------------------------------------------------------------------------
// V13 part (6) — power-glyph Value text and PWR_FLAG/glyph graphic overlap.
//
// The label-anchored V13 parts (1)–(3) and the property-text parts (4)–(5)
// never measure a `power:*` glyph's *own* Value text against a host body /
// pin text, and never measure the PWR_FLAG chevron against the power-glyph
// triangle it sits on. These were verifier-blind overlaps:
//   [1] power-glyph Value text (e.g. "VCC") hangs into the host/neighbour
//       body or its pin-number/name text;
//   [2] each PWR_FLAG graphic is stacked at the identical coordinate as a
//       power glyph, so its chevron overprints the GND/VCC/VEE triangle;
//   [4] a sheet-port glyph's Value text overlaps the sheet's own port-NAME
//       text (`inp`/`vcc`/`vee`).
// ---------------------------------------------------------------------------

/// World-frame `(tag, bbox)` of every visible `power:*` glyph Value text
/// (the net-name label). `PWR_FLAG` is excluded — its Value text is
/// hidden — so this collects exactly the rail-name labels (`GND`/`VCC`/
/// `VEE`/…).
fn power_glyph_value_text_bboxes(root: &Value) -> Vec<(String, Bbox)> {
    power_glyph_bboxes(root)
        .into_iter()
        .filter(|(name, _)| name.ends_with(".Value"))
        .collect()
}

/// World-frame `(tag, bbox)` of every `power:*` glyph *graphic* body
/// (the drawn triangle / chevron / VEE marker), `PWR_FLAG` excluded.
#[allow(clippy::case_sensitive_file_extension_comparisons)] // `.body` is a tag suffix, not a file extension
fn power_glyph_graphic_bboxes(root: &Value) -> Vec<(String, Bbox)> {
    power_glyph_bboxes(root)
        .into_iter()
        .filter(|(name, _)| name.ends_with(".body"))
        .collect()
}

/// World-frame `(tag, bbox)` of every `power:PWR_FLAG` graphic body. The
/// chevron polyline is the only drawn graphic; transform it through the
/// placed pose exactly like [`power_glyph_bboxes`] does for rail glyphs.
fn pwr_flag_graphic_bboxes(root: &Value) -> Vec<(String, Bbox)> {
    let library = load_test_library();
    let mut out = Vec::new();
    for sym in children(root, "symbol") {
        let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
            continue;
        };
        if lib_id != "power:PWR_FLAG" {
            continue;
        }
        let Some((gx, gy, grot)) = at_xy_rot(sym) else {
            continue;
        };
        let mirror_y = find_child(sym, "mirror")
            .and_then(|m| list_iter(m).nth(1).and_then(as_str))
            .is_some_and(|t| t.eq_ignore_ascii_case("y"));
        if let Some(local) = library
            .lookup(&lib_id)
            .and_then(kicad_symbols::Symbol::body_bbox)
        {
            out.push((
                format!("{refdes}({lib_id}).body"),
                body_bbox_to_world(local, gx, gy, f64::from(grot), mirror_y),
            ));
        }
    }
    out
}

/// World-frame `(tag, bbox)` of every hierarchical-sheet port-NAME text.
/// KiCad draws the port label at the pin coordinate, justified away from
/// the sheet body (a left-edge pin, `(at … 180)`, draws its name to the
/// left). We model it as left-anchored text growing in the pin's outward
/// direction, matching the renderer's placement closely enough for a
/// collision check.
fn sheet_port_name_bboxes(root: &Value) -> Vec<(String, Bbox)> {
    let mut out = Vec::new();
    for sheet in children(root, "sheet") {
        for pin in children(sheet, "pin") {
            let Some(name) = list_iter(pin).nth(1).and_then(as_str) else {
                continue;
            };
            let Some((px, py, prot)) = at_xy_rot(pin) else {
                continue;
            };
            // The port text is anchored at the pin and reads outward
            // (away from the sheet body). A `(at … 180)` pin reads to
            // the left, so its text occupies x < px; model that by
            // rotating the left-anchored box 180° about the pin.
            let bbox = text_bbox(name, (px, py), 1.27, prot, TextKind::PlainLabel);
            out.push((format!("port.{name}"), bbox));
        }
    }
    out
}

/// Per-fixture budget for power-glyph Value text overlapping a foreign
/// symbol body / pin text / sheet-port-name text. Ratchet: the measured
/// post-fix high-water mark, driven toward 0. A regression is a defect
/// to diagnose, never a budget to bump.
fn v13_power_glyph_text_budget(_name: &str) -> usize {
    // 0 across every fixture. The decoration-phase power-glyph value-text
    // nudge (`nudge_power_glyph_value_text` in `kicad-emitter`) moves a
    // colliding net-name label off any host body / pin text / sheet-port
    // name, so even the glyph-adjacent-to-large-body cases (GND beside a
    // transistor; the issue-[3] VEE beside RIN) clear by relocating the
    // *text* — never the symbol. A regression is a defect to diagnose,
    // never a budget to bump.
    0
}

#[test]
fn v13_power_glyph_value_text_clear_of_bodies_and_pintext() {
    // V13 part (6a): a `power:*` glyph's visible Value text (its rail
    // name) must not overlap any non-power symbol body, any visible
    // pin-number/name text, or any hierarchical-sheet port-NAME text.
    // Issues [1] (text hanging into a host/neighbour body) and [4]
    // (sheet-port glyph text on the port name) live here.
    let mut xf =
        common::xfail::Guard::new("v13_power_glyph_value_text_clear_of_bodies_and_pintext");
    let with_sheets: Vec<&str> = {
        let mut v: Vec<&str> = SHEETS.to_vec();
        v.push("opamp_inverting");
        v
    };
    for name in with_sheets {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let glyph_text = power_glyph_value_text_bboxes(&root);
        let bodies = placed_symbol_bboxes(&root);
        let pintexts = pin_text_bboxes(&root);
        let port_names = sheet_port_name_bboxes(&root);
        let mut hits = 0;
        for (gname, gbbox) in &glyph_text {
            for (bname, bbox) in &bodies {
                if gbbox.intersects(bbox) {
                    eprintln!("{name}: glyph text {gname} overlaps body {bname}");
                    hits += 1;
                }
            }
            for (tname, tbbox) in &pintexts {
                if gbbox.intersects(tbbox) {
                    eprintln!("{name}: glyph text {gname} overlaps pin-text {tname}");
                    hits += 1;
                }
            }
            for (pname, pbbox) in &port_names {
                if gbbox.intersects(pbbox) {
                    eprintln!("{name}: glyph text {gname} overlaps sheet-port name {pname}");
                    hits += 1;
                }
            }
        }
        let b = v13_power_glyph_text_budget(name);
        // The budget stays a hard 0 for EVERY fixture (see
        // `v13_power_glyph_text_budget`); a fixture that re-exposes the
        // deferred decoration-phase nudge defect is excluded by name in
        // `tests/common/xfail.rs`, which fails the test the day that
        // fixture starts passing.
        xf.record(
            name,
            (hits > b)
                .then(|| format!("{name}: {hits} power-glyph-text overlaps > V13(6a) budget {b}")),
        );
    }
    xf.finish();
}

#[test]
fn v13_pwr_flag_graphic_clear_of_power_glyphs() {
    // V13 part (6b): a `power:PWR_FLAG` chevron graphic must not overlap
    // any `power:*` rail-glyph graphic. Issue [2] — PWR_FLAGs stacked at
    // the identical coordinate as the glyph they drive, so the chevron
    // overprints the GND/VCC/VEE triangle. Budget 0 across the board: a
    // flag need only be wire-coincident on the same net, not at the
    // identical point.
    let budget = |_name: &str| -> usize { 0 };
    let mut failures: Vec<String> = Vec::new();
    let with_sheets: Vec<&str> = {
        let mut v: Vec<&str> = SHEETS.to_vec();
        v.push("opamp_inverting");
        v
    };
    for name in with_sheets {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let flags = pwr_flag_graphic_bboxes(&root);
        let glyphs = power_glyph_graphic_bboxes(&root);
        let mut hits = 0;
        for (fname, fbbox) in &flags {
            for (gname, gbbox) in &glyphs {
                if fbbox.intersects(gbbox) {
                    eprintln!("{name}: PWR_FLAG {fname} overlaps power glyph {gname}");
                    hits += 1;
                }
            }
        }
        let b = budget(name);
        if hits > b {
            failures.push(format!(
                "{name}: {hits} PWR_FLAG↔power-glyph graphic overlaps > V13(6b) budget {b}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "V13(6b) regressions:\n  {}",
        failures.join("\n  "),
    );
}

/// Per-fixture V5 violation budget. The first wire segment at a pin
/// should extend in the pin's outward direction (V5). The router's
/// Steiner stage emits an outward stub at each pin whenever no L
/// corner satisfies the pin's outward constraint; the V11/V12 detour
/// passes prefer outward-clean corner placements when resolving
/// foreign-pin / symbol-body conflicts. Residual cases fall into two
/// buckets:
/// 1. Multi-pin nets where the Steiner tree places the pin on the
///    trunk axis and the outward direction is perpendicular to that
///    axis — splitting the trunk would create a V11/V12 conflict the
///    detour cascade can't resolve.
/// 2. V11/V12 detours that had to abandon the outward-clean option
///    because no foreign-pin- / obstacle-clean alternative existed.
///
/// Both buckets are tracked as v0.2 placer / channel-router work
/// items. The budgets here lock in the current high-water mark — a
/// regression trips the test.
// `named_rails` and `opamp_definition_level` both hold at 2 but for
// entirely different reasons (a rail-stub v0.2 work item vs. the
// owner-approved Option B channel-row escape), so their arms are kept
// separate with their own rationales rather than merged.
#[allow(clippy::match_same_arms)]
fn v5_violation_budget(name: &str) -> usize {
    match name {
        // Ratcheted high-water marks (current measured count on master).
        //
        // These were RE-BASELINED when the pin-angle inversion in
        // `Symbol::pins_in` was fixed. That fix corrected the *measure*
        // as well as the router: the old verifier read every horizontal
        // pin's outward direction backwards, so pre-fix literals are not
        // comparable to post-fix ones. Measured with the corrected
        // verifier, the TOTAL across all fixtures fell 16 → 8, then to 7
        // when the tail-trunk stub took `diff_pair` to zero.
        //
        // Two fixtures ratcheted DOWN to zero and stay there:
        // `opamp_inverting_real` (1 → 0) and `opamp_definition_level`
        // (3 → 0) — their residuals were all horizontal opamp pins, the
        // exact class the old measurement reported backwards, and the
        // router was steering their outward stubs the wrong way as a
        // result. `diff_pair` (1 → 0) followed from the shared-node-centre
        // trunk stub in `spice_layout::idioms`.
        //
        // The three below are newly VISIBLE, not newly introduced. Their
        // emitted geometry is bit-identical to the locked baseline
        // (`baseline_lock` shows zero differences on all three), so
        // nothing moved — only the measurement became correct. Every one
        // is a horizontal pin: `common_emitter`'s Q1.1 and
        // `multivibrator`'s Q1.1/Q2.1 are BJT bases, `opamp_inverting`'s
        // RF.1 sits on the feedback trunk. They are the same v0.2
        // placer / channel-router work items as the rest.
        // V5 5 -> 3: the rail-stub column idiom now fires on symmetric
        // fixtures, putting RC1/RC2 on their collector columns so both
        // collector trunks leave the pin outward. Ratchet DOWN.
        "multivibrator" => 3,
        "common_emitter" | "opamp_inverting" => 1,
        // PRE-EXISTING, newly VISIBLE: `named_rails` was absent from
        // `SHEETS` until the fixture lists were unified, so nothing ever
        // graded it. Nothing moved — the fixture is simply now measured.
        // Both residuals are the documented v0.2 placer work item, not a
        // routing bug: `RIN.2` and `RPU.2` are the two rail-stub pins on
        // the `out` node, which the router reaches with a run *along* the
        // shared column rather than an outward stub. V5 is Tier 2, and
        // MEMORY "flow-orientation wall" records that a seed/SA
        // orientation tie-break is the wrong lever here. Ratchets down
        // when the v0.2 placer redesign lands.
        "named_rails" => 2,
        // V5 0 → 2. Channel-row banding (Option B, OWNER SIGN-OFF
        // 2026-07-20) pins each inverting-amp channel to its textbook
        // seed facing (input-left, output-right) so the deck reads
        // left-to-right as two congruent rows. Both summing-node input
        // pins (X1.2 / X2.2, the opamp inverting inputs on nets
        // `inv1` / `inv2`) then face outward-left while their net
        // legitimately continues UP to the RIN/RF feedback junction —
        // the documented V5-vs-left-to-right-flow tension (MEMORY
        // "flow-orientation wall"), not a wire leaving a pin into open
        // space. Approved under the global-improvement escape: summed
        // across this fixture, TOTAL violations fall by 6 (B −9, F5 −1,
        // V5 +2, J +2). Ratchets down when the v0.2 placer redesign lands.
        "opamp_definition_level" => 2,
        // V5 0 -> 1. The series-horizontal idiom draws R1 horizontal
        // (in-left, out-right) with C1 dropped straight below the `out`
        // node, so `rc_lowpass_ports` reads left-to-right as a textbook
        // single-pole RC. R1's output pin then sends its wire DOWN to
        // C1 rather than outward along its own axis — the documented
        // V5-vs-left-to-right-flow tension (MEMORY "flow-orientation
        // wall", invariants.md V5: "some V5 violations are the correct
        // flow drawing"), not a wire into open space. Net on the
        // fixture: F5 1 -> 0, P5 1 -> 0, F6 5 -> 0, V5 0 -> 1 = -2 Tier-2
        // violations, zero Tier-0/Tier-1 cost.
        //
        // PROVENANCE: this rise was landed by the operating assistant
        // under the owner's standing instruction to proceed without
        // per-change confirmation; it was NOT an explicit owner decision
        // and the owner did not see this specific budget. The automatic
        // global-improvement escape does apply (net -2 on the fixture,
        // zero higher-tier cost). Re-examine rather than cite as owner
        // precedent. Ratchets down when the v0.2 placer redesign lands.
        "rc_lowpass_ports" => 1,
        // V5 0 -> 1. The series-horizontal idiom's name-based flow-root
        // fallback (`idioms::signal_net_depth`) now fires on `rc_lowpass`
        // too (its `in` leaf net roots the flow graph even with no `*@port`),
        // so the un-ported filter draws IDENTICALLY to `rc_lowpass_ports`:
        // R1 horizontal (in-left, out-right), C1 dropped straight below the
        // `out` node. R1's output pin then sends its wire DOWN to C1 rather
        // than outward along its own axis — the SAME documented
        // V5-vs-left-to-right-flow tension `rc_lowpass_ports` already carries
        // (MEMORY "flow-orientation wall"; invariants.md V5). Net on the
        // fixture: B 3 -> 0, F5 1 -> 0, F6 9 -> 0, V5 0 -> 1 = -12 Tier-2
        // violations, zero Tier-0/Tier-1 cost.
        //
        // PROVENANCE: landed on assistant judgement under the owner's
        // standing instruction to proceed; NOT an explicit owner decision,
        // and the owner did not see this specific budget. The automatic
        // global-improvement escape applies (net -12 on the fixture, zero
        // higher-tier cost). Re-examine rather than cite as owner precedent.
        "rc_lowpass" => 1,
        // F0 (v0.2 roadmap) NEW-GEOMETRY BASELINE, owner-approved.
        // `rc_phase_shift` is the F0 benchmark fixture: a three-section RC
        // ladder feeding a CE stage — a long rooted chain the current
        // placer sprawls. Its five residuals are exactly the Tier-2
        // headroom F0 exists to expose, and are the same class as every
        // arm above (a shared-net wire that leaves the pin sideways rather
        // than outward): `R1.2` / `R3.2` in the ladder, `CIN.2` on the
        // coupling cap, and `Q1.1` / `Q1.3` on the transistor. This is a
        // recorded high-water mark on a fixture that did not exist before,
        // NOT a loosened budget on an existing one — no v0.1 fixture's
        // count moved. Ratchet DOWN.
        "rc_phase_shift" => 5,
        // diff_pair, port_shapes, opamp_inverting_real: zero violations.
        _ => 0,
    }
}

/// V5 — first wire segment at every pin extends outward.
///
/// For each placed symbol's pin, find a wire endpoint coincident with
/// the pin coordinate and check that the segment's far end lies in the
/// pin's outward direction. Pins where the wire's segment passes
/// *through* the pin in its interior (T-on-trunk topology) are
/// reported as known limitations and not counted: the V5 stub-fallback
/// would have to split the trunk and likely create a V11/V12
/// violation. Other residual cases are counted against the per-fixture
/// budget in [`v5_violation_budget`].
#[test]
fn v5_first_segment_extends_outward() {
    let mut hard_failures: Vec<String> = Vec::new();
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let pins = world_pins_for_sheet(&src, &root);
        let wires = wire_segments(&root);

        // The V5 first-segment-outward rule lives in
        // `kicad_emitter::v5::count_outward_violations`, the SAME
        // function the routing-aware orientation-refinement phase
        // (Layout phase 4.5) uses as its router-in-the-loop oracle.
        // Calling it here binds verifier and refinement to one
        // measurement — they can never drift. (The interior-trunk
        // "report but don't fail" bucket is folded into that function:
        // pure interior-trunk pins are excluded from the returned
        // violations.)
        let probes: Vec<kicad_emitter::v5::PinProbe> = pins
            .iter()
            .map(|p| kicad_emitter::v5::PinProbe {
                refdes: p.refdes.clone(),
                pin_number: p.pin_number.clone(),
                x_mm: p.x_mm,
                y_mm: p.y_mm,
                angle: p.angle,
            })
            .collect();
        let segments: Vec<((f64, f64), (f64, f64))> = wires.iter().map(|&(a, b)| (a, b)).collect();
        let violations: Vec<String> =
            kicad_emitter::v5::count_outward_violations(&probes, &segments)
                .into_iter()
                .map(|v| {
                    format!(
                        "{}.{} at ({:.2}, {:.2}) angle={} has no outward-extending wire",
                        v.refdes, v.pin_number, v.x_mm, v.y_mm, v.angle,
                    )
                })
                .collect();
        let budget = v5_violation_budget(name);
        if violations.len() > budget {
            hard_failures.push(format!(
                "{name}: {} V5 outward-direction violation(s) > budget {budget}:\n    {}",
                violations.len(),
                violations.join("\n    "),
            ));
        } else if !violations.is_empty() {
            eprintln!(
                "{name}: {} V5 violation(s) within budget {budget}:\n    {}",
                violations.len(),
                violations.join("\n    "),
            );
        }
    }
    assert!(
        hard_failures.is_empty(),
        "V5 regressions:\n  {}",
        hard_failures.join("\n  "),
    );
}

// --- R-1 (V6/V10) — negative rails render as power:VEE, not power:GND ----

/// A negative supply rail (e.g. `VEE vee 0 DC -12 ;@ power=-12V`) must
/// render with a *negative-rail* glyph (`power:VEE`), never the
/// ground-triangle (`power:GND`). A reader who sees a ground symbol on a
/// -12 V rail is electrically misled.
///
/// Negative rails are derived **generally** from the SPICE source — never
/// from fixture or refdes names. Two independent signals (mirroring
/// `spice_layout::net_class`):
///   * a `;@ power=<rail>` / `*@power` tag whose rail string begins with
///     `-` (a negative voltage) — the strongest signal, and
///   * a canonical negative-rail net name (`vee` / `v-` / `vminus`).
///
/// `vss` is *not* treated as negative by name alone (commonly digital
/// ground at 0 V); it would only qualify via a negative `power=` tag.
///
/// True ground (net `0`, or canonical `gnd`) must stay `power:GND`.
///
/// Scans every fixture's `.cir`, builds the set of negative-rail and
/// true-ground net names, then asserts each emitted `power:*` glyph's
/// `lib_id` matches the class of the net in its `Value` property.
fn negative_and_ground_nets(
    cir_src: &str,
) -> (
    std::collections::BTreeSet<String>,
    std::collections::BTreeSet<String>,
) {
    let mut negative = std::collections::BTreeSet::new();
    let mut ground = std::collections::BTreeSet::new();
    ground.insert("0".to_string());
    for raw in cir_src.lines() {
        let line = raw.trim();
        // First whitespace-separated tokens are nodes; the `;@ power=`
        // tag (if any) carries the rail polarity.
        let code = line.split(";@").next().unwrap_or("").trim();
        let mut toks = code.split_whitespace();
        // Node names: positions 1.. up to the value; for a 2-terminal
        // voltage source `V<n> n+ n- …` the first two tokens after the
        // refdes are nets. We don't need to be precise — we just collect
        // every token that looks like a net and classify by name; the
        // power tag handles the source's own nets.
        let nodes: Vec<&str> = toks.by_ref().skip(1).collect();
        for n in &nodes {
            let lower = n.to_ascii_lowercase();
            match lower.as_str() {
                "vee" | "v-" | "vminus" => {
                    negative.insert((*n).to_string());
                }
                "gnd" => {
                    ground.insert((*n).to_string());
                }
                _ => {}
            }
        }
        // `;@ power=<rail>` with a negative voltage → the source's first
        // node (positive terminal) is a negative rail.
        if let Some(tag) = line.split(";@").nth(1) {
            let tag = tag.trim();
            if let Some(rest) = tag.strip_prefix("power=") {
                let rail = rest.split_whitespace().next().unwrap_or("");
                if rail.trim_start().starts_with('-') {
                    if let Some(first_node) = nodes.first() {
                        negative.insert((*first_node).to_string());
                        // A negative rail is not ground.
                        ground.remove(*first_node);
                    }
                }
            }
        }
    }
    // Canonical-name negatives are not ground.
    for n in &negative {
        ground.remove(n);
    }
    (negative, ground)
}

#[test]
fn negative_rails_render_as_vee_not_gnd() {
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let cir = std::fs::read_to_string(&src).expect("read .cir");
        let (negative, ground) = negative_and_ground_nets(&cir);
        // The emitted glyph `Value` is the *canonical* rail name (R-6),
        // so compare on canonical identity (`vee`→`VEE`, `0`→`GND`).
        let negative: std::collections::BTreeSet<String> =
            negative.iter().map(|n| canonical_net(n)).collect();
        let ground: std::collections::BTreeSet<String> =
            ground.iter().map(|n| canonical_net(n)).collect();
        if negative.is_empty() {
            continue; // fixture has no negative rail
        }
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let mut saw_negative = false;
        for sym in children(&root, "symbol") {
            let Some(lib_id) = find_child(sym, "lib_id")
                .and_then(|n| list_iter(n).nth(1))
                .and_then(as_str)
            else {
                continue;
            };
            if !lib_id.starts_with("power:") || lib_id == "power:PWR_FLAG" {
                continue;
            }
            // The glyph's `Value` property carries the net name.
            let mut net = String::new();
            for prop in children(sym, "property") {
                let mut pit = list_iter(prop);
                pit.next();
                if pit.next().and_then(as_str) == Some("Value") {
                    if let Some(v) = pit.next().and_then(as_str) {
                        net = v.to_string();
                    }
                }
            }
            if negative.contains(&net) {
                saw_negative = true;
                assert_eq!(
                    lib_id, "power:VEE",
                    "{name}: negative rail '{net}' rendered with glyph '{lib_id}'; \
                     must be 'power:VEE' (a ground triangle on a negative rail is \
                     electrically misleading)",
                );
            } else if ground.contains(&net) {
                assert_eq!(
                    lib_id, "power:GND",
                    "{name}: true-ground net '{net}' rendered with glyph '{lib_id}'; \
                     must be 'power:GND'",
                );
            }
        }
        assert!(
            saw_negative,
            "{name}: expected at least one power:VEE glyph for negative rail(s) {negative:?}, \
             but no negative-rail glyph was emitted",
        );
    }
}

// ---------------------------------------------------------------------------
// Phase 1 — no spurious PWR_FLAG on internal signal nets.
//
// A `power:PWR_FLAG` is only warranted on a net KiCad's ERC would
// otherwise flag as undriven:
//   * a Power/Ground *rail* net (its pins are all `power_in`; passives
//     do NOT count as drivers on a power net — a rail flag stays until
//     the deferred rail-elimination decision), or
//   * a *signal* net whose pins are ALL `input`/`power_in` with no
//     passive or driving pin (e.g. a transistor base fed solely by an
//     `;@ ignore`d stimulus — `diff_pair`'s `in1`/`in2`).
//
// The current generator uses the POWER-net driver rule (`drives()`,
// which excludes `Passive`) on *signal* nets too, so a signal net with
// only passive pins plus one `input` pin — e.g. `common_emitter`'s net
// `b` (R1/R2/CIN passive + Q1 base input) — wrongly gets a flag. But
// KiCad's real ERC counts a PASSIVE pin as a valid driver on a signal
// net, so such a net needs NO flag. These tests pin that down:
// `no_pwr_flag_on_signal_net_with_passive_pin` FAILS today (it catches
// the net-`b` flag) and passes once the generator is class-aware;
// `phase1_erc_stays_clean` is a Tier-0 (V2) regression guard that the
// flag removal must not reintroduce any ERC error.
// ---------------------------------------------------------------------------

/// World-frame pin electrical types for every *placed real symbol*
/// (power glyphs `power:*` and `#PWR`/`#FLG` markers excluded — they are
/// not the pins whose presence makes a flag redundant). Keyed by
/// quantised world coordinate so a co-located PWR_FLAG can be classified
/// by what actually sits under it.
#[allow(clippy::type_complexity)]
fn placed_pin_electricals(
    root: &Value,
) -> HashMap<(i64, i64), Vec<(String, String, PinElectrical)>> {
    let library = load_test_library();
    let mut out: HashMap<(i64, i64), Vec<(String, String, PinElectrical)>> = HashMap::new();
    for sym in children(root, "symbol") {
        let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
            continue;
        };
        // Rail glyphs and flags carry `power:*` lib_ids and #PWR/#FLG
        // refdes — never a real component pin we'd count as a driver.
        if lib_id.starts_with("power:") || refdes.starts_with("#PWR") || refdes.starts_with("#FLG")
        {
            continue;
        }
        let Some((ox, oy, orient)) = placed_symbol_pose(sym) else {
            continue;
        };
        let Some(lib_sym) = library.lookup(&lib_id) else {
            continue;
        };
        for tp in lib_sym.pins_in(orient) {
            let wx = ox + tp.x;
            let wy = oy - tp.y;
            out.entry(qkey(wx, wy)).or_default().push((
                refdes.clone(),
                tp.number.clone(),
                tp.electrical,
            ));
        }
    }
    out
}

/// Quantised world coordinates of every placed rail glyph
/// (`power:GND` / `power:VCC` / `power:VEE` / … — NOT `power:PWR_FLAG`).
/// A PWR_FLAG co-located with one of these is a *rail* flag, exempt from
/// the signal-net check (rail-flag elimination is a separate, deferred
/// decision — see MEMORY / annotation-spec §9).
fn rail_glyph_coords(root: &Value) -> HashSet<(i64, i64)> {
    let mut out = HashSet::new();
    for sym in children(root, "symbol") {
        let mut lib_id = String::new();
        if let Some(lid) = find_child(sym, "lib_id")
            && let Some(s) = list_iter(lid).nth(1).and_then(as_str)
        {
            s.clone_into(&mut lib_id);
        }
        if !lib_id.starts_with("power:") || lib_id == "power:PWR_FLAG" {
            continue;
        }
        if let Some((x, y, _)) = at_xy_rot(sym) {
            out.insert(qkey(x, y));
        }
    }
    out
}

/// Quantised world coordinates + refdes of every placed `power:PWR_FLAG`.
fn pwr_flag_coords(root: &Value) -> Vec<(String, (i64, i64))> {
    let mut out = Vec::new();
    for sym in children(root, "symbol") {
        let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
            continue;
        };
        if lib_id != "power:PWR_FLAG" {
            continue;
        }
        if let Some((x, y, _)) = at_xy_rot(sym) {
            out.push((refdes, qkey(x, y)));
        }
    }
    out
}

#[test]
#[allow(clippy::cast_precision_loss)]
fn no_pwr_flag_on_signal_net_with_passive_pin() {
    // For every fixture, every emitted `power:PWR_FLAG` that is NOT
    // co-located with a rail glyph must sit on a signal net whose pins
    // are *all* driver-requiring (`input`/`power_in`) — i.e. it must
    // land on a real component pin, and none of the pins co-located
    // there may be `Passive` or a driver (`drives()`). A flag whose
    // anchor pin is passive/driving is spurious: KiCad counts that pin
    // as a valid signal-net driver, so the flag is redundant noise.
    let mut violations: Vec<String> = Vec::new();
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);

        let rails = rail_glyph_coords(&root);
        let pins = placed_pin_electricals(&root);
        for (refdes, coord) in pwr_flag_coords(&root) {
            if rails.contains(&coord) {
                // Rail flag — exempt (deferred rail-flag decision).
                continue;
            }
            let here = pins.get(&coord);
            // A non-rail flag must anchor on a real component pin.
            let Some(here) = here else {
                violations.push(format!(
                    "{name}: {refdes} at ({:.2},{:.2}) is not co-located with any rail glyph \
                     or real component pin — cannot be a legitimate driver marker",
                    coord.0 as f64 / 1000.0,
                    coord.1 as f64 / 1000.0,
                ));
                continue;
            };
            let bad: Vec<String> = here
                .iter()
                .filter(|(_, _, e)| *e == PinElectrical::Passive || e.drives())
                .map(|(r, p, e)| format!("{r}.{p}({e:?})"))
                .collect();
            if !bad.is_empty() {
                violations.push(format!(
                    "{name}: {refdes} at ({:.2},{:.2}) sits on a signal net driven by \
                     passive/driving pin(s) [{}] — flag is spurious (KiCad counts a passive \
                     pin as a valid signal-net driver)",
                    coord.0 as f64 / 1000.0,
                    coord.1 as f64 / 1000.0,
                    bad.join(", "),
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "spurious PWR_FLAG(s) on signal nets that already have a passive/driving pin:\n  {}",
        violations.join("\n  "),
    );
}

/// Fixtures whose emitted schematic Phase 1 touches (flat root-sheet
/// fixtures — no hierarchical-ground ERC artifact). ERC must stay at
/// zero errors after the net-`b`-class flag is removed.
/// Every FLAT fixture. `opamp_inverting` is deliberately absent: it is
/// the one fixture that emits a hierarchical sheet, and it carries the
/// documented `power_pin_not_driven` artifact on its parent-side ground
/// glyph (see docs/invariants.md V2) — a genuine KiCad hierarchical
/// limitation, not a defect this guard should mask. Every other fixture
/// must be at zero ERC errors with no allowance at all.
const PHASE1_ERC_FIXTURES: &[&str] = &[
    "rc_lowpass",
    "rc_lowpass_ports",
    "common_emitter",
    "diff_pair",
    "multivibrator",
    "opamp_inverting_real",
    "port_shapes",
    "opamp_definition_level",
    "named_rails",
    "rc_phase_shift",
];

#[test]
fn phase1_erc_stays_clean() {
    // Tier-0 (V2) regression guard: removing the spurious net-`b` flag
    // (and any other class-aware flag pruning) must not reintroduce an
    // ERC error. Passes today (ERC is already clean) and must keep
    // passing after the fix. Skips cleanly when `kicad-cli` is absent.
    if std::process::Command::new("kicad-cli")
        .arg("version")
        .output()
        .ok()
        .is_none_or(|o| !o.status.success())
    {
        eprintln!("kicad-cli not on PATH — skipping phase1_erc_stays_clean");
        return;
    }
    let mut failures: Vec<String> = Vec::new();
    for name in PHASE1_ERC_FIXTURES {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let report = tmp.join(format!("{name}-erc.rpt"));
        let _ = std::process::Command::new("kicad-cli")
            .args(["sch", "erc", "--severity-error", "-o"])
            .arg(&report)
            .arg(&sch)
            .output()
            .expect("invoke kicad-cli sch erc");
        let body = std::fs::read_to_string(&report).unwrap_or_default();
        let lines: Vec<&str> = body.lines().collect();
        for i in 0..lines.len() {
            let trimmed = lines[i].trim_start();
            if !trimmed.starts_with('[') {
                continue;
            }
            let sev = lines
                .iter()
                .skip(i + 1)
                .take(3)
                .find_map(|l| l.trim_start().strip_prefix("; "))
                .unwrap_or("warning");
            if sev.starts_with("error") {
                failures.push(format!("{name}: {}", lines[i].trim()));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "Phase 1 ERC regression — error-severity violations after flag pruning:\n  {}",
        failures.join("\n  "),
    );
}

// ---------------------------------------------------------------------------
// Phase 2 — router / label READABILITY (decoration-only).
//
// Three defect classes, all Tier-1 (V12/V13), all fixable in the
// DECORATION phase without moving any placed symbol:
//
//  (1) redundant collinear same-net wire overlaps + missing mid-span
//      junction dots ("wires look disconnected"). The 3-pin Steiner +
//      conflict-detour passes emit co-directional segments that share a
//      start point and fully/partially overlap, and tee same-net wires
//      off another same-net wire's interior with NO junction dot.
//  (2) a FOREIGN (different-net) global-label / wire overlapping a
//      power-glyph BODY — power glyphs are excluded from both the
//      router/label obstacle set AND the label↔body V13 verifier, so
//      this was unmodeled and untested (common_emitter `in` over GND).
//  (3) an interface global-label overlapping a foreign symbol body
//      (diff_pair `in1` over Q1). See the note on the item-(3) guard
//      below — this class is already enforced by
//      `v13_labels_dont_overlap_symbol_body` at budget 0.
//
// Connectivity-inert guard for item (1): coalescing overlaps and adding
// junction dots is electrically inert, so the exported-netlist topology
// check in `tests/roundtrip.rs` (`common_emitter` / `diff_pair`, via
// `kicad-cli sch export netlist` → canonical topology match) must stay
// green. Those tests are the item-(1) connectivity guard; they run here
// only implicitly (kicad-cli present) but are the authoritative check
// that item (1) does not change connectivity.
// ---------------------------------------------------------------------------

const EPS_MM: f64 = 1e-6;

/// Every emitted `(junction (at x y) …)` position. A junction node has a
/// two-value `(at x y)` (no rotation); [`at_xy_rot`] tolerates that
/// (rot defaults to 0).
fn junction_positions(root: &Value) -> Vec<Pt> {
    let mut out = Vec::new();
    for j in children(root, "junction") {
        if let Some((x, y, _)) = at_xy_rot(j) {
            out.push((x, y));
        }
    }
    out
}

/// If `a` and `b` are collinear (both vertical at the same X, or both
/// horizontal at the same Y) and their spans overlap by a POSITIVE
/// length, return that overlap length. Sharing only an endpoint (overlap
/// length 0) returns `None` — that is a legal end-to-end chain, not a
/// redundant overlap. Two collinear segments overlapping by a positive
/// length are necessarily on the SAME net: a foreign collinear overlap
/// would already be a V11 short, forbidden elsewhere.
#[allow(clippy::similar_names)]
fn collinear_overlap(a: &(Pt, Pt), b: &(Pt, Pt)) -> Option<f64> {
    let (ax1, ay1) = a.0;
    let (ax2, ay2) = a.1;
    let (bx1, by1) = b.0;
    let (bx2, by2) = b.1;
    let a_vert = (ax1 - ax2).abs() < EPS_MM;
    let b_vert = (bx1 - bx2).abs() < EPS_MM;
    let a_horiz = (ay1 - ay2).abs() < EPS_MM;
    let b_horiz = (by1 - by2).abs() < EPS_MM;
    if a_vert && b_vert && (ax1 - bx1).abs() < EPS_MM {
        let (alo, ahi) = (ay1.min(ay2), ay1.max(ay2));
        let (blo, bhi) = (by1.min(by2), by1.max(by2));
        let ov = ahi.min(bhi) - alo.max(blo);
        if ov > EPS_MM {
            return Some(ov);
        }
    }
    if a_horiz && b_horiz && (ay1 - by1).abs() < EPS_MM {
        let (alo, ahi) = (ax1.min(ax2), ax1.max(ax2));
        let (blo, bhi) = (bx1.min(bx2), bx1.max(bx2));
        let ov = ahi.min(bhi) - alo.max(blo);
        if ov > EPS_MM {
            return Some(ov);
        }
    }
    None
}

#[test]
fn no_same_net_collinear_wire_overlap() {
    // Item (1a): no two emitted wire segments may overlap collinearly.
    // Concrete current defects this catches on `common_emitter`: three
    // IDENTICAL verticals at x=43.18 (43.18,40.64)->(43.18,39.37), and a
    // nested triple at x=52.07 (Q1-collector Steiner) where
    // [44.45,45.72] ⊂ [44.45,46.99] ⊂ [44.45,49.53] all share the start
    // y=44.45. Budget 0 — a ratchet, not a knob. Electrically inert
    // (same-net), so the roundtrip topology guard must stay green.
    let mut failures: Vec<String> = Vec::new();
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let wires = wire_segments(&root);
        // Restrict to SAME-net pairs via the wire-only union-find. A
        // connected net's Steiner tree is one island (its segments share
        // endpoints transitively), so same-island == same-net here. This
        // deliberately EXCLUDES collinear overlaps between DIFFERENT nets
        // (e.g. diff_pair `in1`/`in2`, multivibrator's cross-couple),
        // which are a distinct wire-on-wire coincidence class — item (1a)
        // is same-net redundancy only, and its coalescing fix must never
        // merge two different nets.
        let (coord_idx, mut uf, _c) = build_wire_uf(&wires);
        let start_root = |uf: &mut UnionFind, seg: &(Pt, Pt)| -> usize {
            let (sa, _) = *seg;
            uf.find(coord_idx[&qkey(sa.0, sa.1)])
        };
        let mut hits = 0usize;
        for i in 0..wires.len() {
            let ri = start_root(&mut uf, &wires[i]);
            for j in (i + 1)..wires.len() {
                let rj = start_root(&mut uf, &wires[j]);
                if ri != rj {
                    continue; // different net — not item (1a)
                }
                if let Some(ov) = collinear_overlap(&wires[i], &wires[j]) {
                    let (ai, bi) = wires[i];
                    let (aj, bj) = wires[j];
                    eprintln!(
                        "{name}: collinear same-net overlap ({ov:.4}mm) between \
                         ({:.2},{:.2})->({:.2},{:.2}) and ({:.2},{:.2})->({:.2},{:.2})",
                        ai.0, ai.1, bi.0, bi.1, aj.0, aj.1, bj.0, bj.1,
                    );
                    hits += 1;
                }
            }
        }
        if hits > 0 {
            failures.push(format!("{name}: {hits} collinear same-net wire overlap(s)"));
        }
    }
    assert!(
        failures.is_empty(),
        "item(1a) redundant collinear same-net wire overlaps (budget 0):\n  {}",
        failures.join("\n  "),
    );
}

#[test]
fn mid_span_same_net_t_has_junction() {
    // Item (1b): wherever a same-net wire endpoint lands on the strict
    // interior of another same-net wire segment (a mid-span T), KiCad
    // draws no automatic junction dot, so the schematic reads as
    // disconnected. Every such incidence must carry an explicit
    // `(junction …)`. "Same-net" is established via the wire-only
    // union-find (a foreign endpoint-on-interior is a V13 concern, not
    // a junction one). Concrete current defect on `common_emitter`:
    // (52.07,45.72) and (52.07,46.99) tee into the x=52.07 collector
    // trunk with no junction. Budget 0.
    let mut failures: Vec<String> = Vec::new();
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let wires = wire_segments(&root);
        let (coord_idx, mut uf, _c) = build_wire_uf(&wires);
        let juncs: HashSet<(i64, i64)> = junction_positions(&root)
            .iter()
            .map(|p| qkey(p.0, p.1))
            .collect();

        // Distinct wire-endpoint coords.
        let mut endpoints: Vec<(i64, i64)> = Vec::new();
        {
            let mut seen: HashSet<(i64, i64)> = HashSet::new();
            for (a, b) in &wires {
                for k in [qkey(a.0, a.1), qkey(b.0, b.1)] {
                    if seen.insert(k) {
                        endpoints.push(k);
                    }
                }
            }
        }

        let mut hits = 0usize;
        for (a, b) in &wires {
            let ka = qkey(a.0, a.1);
            let kb = qkey(b.0, b.1);
            let seg_root = uf.find(coord_idx[&ka]);
            for &p in &endpoints {
                if p == ka || p == kb {
                    continue; // endpoint of this very segment
                }
                let Some(&pi) = coord_idx.get(&p) else {
                    continue;
                };
                if uf.find(pi) != seg_root {
                    continue; // different net — not a same-net T
                }
                let interior = if ka.0 == kb.0 && p.0 == ka.0 {
                    let (lo, hi) = (ka.1.min(kb.1), ka.1.max(kb.1));
                    p.1 > lo && p.1 < hi
                } else if ka.1 == kb.1 && p.1 == ka.1 {
                    let (lo, hi) = (ka.0.min(kb.0), ka.0.max(kb.0));
                    p.0 > lo && p.0 < hi
                } else {
                    false
                };
                if interior && !juncs.contains(&p) {
                    #[allow(clippy::cast_precision_loss)]
                    {
                        eprintln!(
                            "{name}: mid-span same-net T at ({:.2},{:.2}) on segment \
                             ({:.2},{:.2})->({:.2},{:.2}) has no junction dot",
                            p.0 as f64 / 1000.0,
                            p.1 as f64 / 1000.0,
                            a.0,
                            a.1,
                            b.0,
                            b.1,
                        );
                    }
                    hits += 1;
                }
            }
        }
        if hits > 0 {
            failures.push(format!(
                "{name}: {hits} mid-span same-net T-branch(es) without a junction dot"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "item(1b) mid-span same-net T-branches missing junction dots (budget 0):\n  {}",
        failures.join("\n  "),
    );
}

/// Each rail glyph's (`power:*` except `PWR_FLAG`) canonical net and
/// world-frame FOOTPRINT bbox: the drawn triangle/chevron body UNIONED
/// with the glyph's connection point (its `(at …)`, which is the glyph
/// symbol's local origin = its power pin). Net comes from the glyph's
/// visible `Value` (canonicalised so `0`→`GND`, `vcc`→`VCC`).
///
/// The union with the connection point is load-bearing: `body_bbox` stops
/// at the drawn chevron/triangle, so a VCC/VDD/VEE glyph leaves a ~1.27 mm
/// stem between its open base and the pin it hangs on. A foreign wire
/// running along that base edge grazes the *body* boundary — below the
/// 0.1 mm interior epsilon — and slips through the empty triangle interior
/// undetected (the `opamp_inverting_real` feedback-wire class). Extending
/// the footprint down the stem to the pin makes such a wire strictly
/// interior, mirroring the router's own obstacle (`rail_glyph_body_bboxes`
/// in `kicad-emitter`). Ground glyphs already touch their pin at the body
/// edge, so this is a no-op there.
fn rail_glyph_bodies_with_net(root: &Value) -> Vec<(String, Bbox)> {
    let library = load_test_library();
    let mut out = Vec::new();
    for sym in children(root, "symbol") {
        let mut lib_id = String::new();
        if let Some(l) = find_child(sym, "lib_id")
            .and_then(|n| list_iter(n).nth(1))
            .and_then(as_str)
        {
            l.clone_into(&mut lib_id);
        }
        if !lib_id.starts_with("power:") || lib_id == "power:PWR_FLAG" {
            continue;
        }
        let mut net = String::new();
        for prop in children(sym, "property") {
            let mut it = list_iter(prop);
            it.next();
            if it.next().and_then(as_str) == Some("Value") {
                net = canonical_net(it.next().and_then(as_str).unwrap_or(""));
                break;
            }
        }
        let Some((gx, gy, grot)) = at_xy_rot(sym) else {
            continue;
        };
        let mirror_y = find_child(sym, "mirror")
            .and_then(|m| list_iter(m).nth(1).and_then(as_str))
            .is_some_and(|t| t.eq_ignore_ascii_case("y"));
        if let Some(local) = library
            .lookup(&lib_id)
            .and_then(kicad_symbols::Symbol::body_bbox)
        {
            let mut b = body_bbox_to_world(local, gx, gy, f64::from(grot), mirror_y);
            // Union with the connection point (the glyph's `at`, i.e. its
            // power pin) so the stem between the drawn body and the pin is
            // part of the footprint — see the doc comment.
            b.x0 = b.x0.min(gx);
            b.x1 = b.x1.max(gx);
            b.y0 = b.y0.min(gy);
            b.y1 = b.y1.max(gy);
            out.push((net, b));
        }
    }
    out
}

/// Per-fixture budget for [`no_foreign_label_or_wire_over_power_glyph_body`].
/// Budget 0 across the board: item (2) is a fully decoration-fixable
/// readability defect (add power glyphs to the label/wire obstacle set;
/// nudge the offending label clear). The tracked `opamp_inverting_real`
/// residual is glyph-body-vs-symbol-BODY (RIN), counted by
/// `placement_quality::power_glyph_foreign_body_overlap_budget=1` on a
/// DIFFERENT axis — this label/wire measure must stay 0 there too.
fn v13_foreign_over_glyph_budget(_name: &str) -> usize {
    0
}

#[test]
fn no_foreign_label_or_wire_over_power_glyph_body() {
    // Item (2): a FOREIGN (different-net) global-label / plain-label text
    // bbox, or a foreign-net wire, overlapping a power-glyph BODY.
    // common_emitter's `in` global-label sits over a GND glyph — invisible
    // to `v13_labels_dont_overlap_symbol_body` (which skips `power:*`) and
    // to the router obstacle set (glyphs early-return None there). This
    // models the glyph body as an obstacle for foreign labels/wires ONLY
    // (a glyph is never an obstacle for its OWN net's stub). Budget 0.
    let mut failures: Vec<String> = Vec::new();
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let glyphs = rail_glyph_bodies_with_net(&root);
        let labels = labels_with_kind(&root);
        let wires = wire_segments(&root);
        let pins = world_pins_for_sheet(&src, &root);
        let pin_index = build_pin_index(&pins);
        let (coord_idx, mut uf, _c) = build_wire_uf(&wires);
        let (comp_net, _extra) = assign_island_nets(&wires, &coord_idx, &mut uf, &pin_index, name);

        let mut hits = 0usize;
        for (gnet, gbody) in &glyphs {
            for (lname, anchor, rot, kind) in &labels {
                let lnet = canonical_net(lname);
                if &lnet == gnet {
                    continue; // a glyph is never an obstacle for its own net
                }
                let lbox = text_bbox(lname, *anchor, 1.27, *rot, *kind);
                if lbox.intersects(gbody) {
                    eprintln!("{name}: foreign label {lnet:?} overlaps {gnet:?} glyph body");
                    hits += 1;
                }
            }
            for (a, b) in &wires {
                let root_id = uf.find(coord_idx[&qkey(a.0, a.1)]);
                let Some(wnet) = comp_net.get(&root_id) else {
                    continue;
                };
                if wnet == gnet {
                    continue; // own-net stub, not foreign
                }
                if gbody.intersects_segment(*a, *b) {
                    eprintln!(
                        "{name}: foreign wire on net {wnet:?} ({:.2},{:.2})->({:.2},{:.2}) \
                         crosses {gnet:?} glyph body",
                        a.0, a.1, b.0, b.1,
                    );
                    hits += 1;
                }
            }
        }
        let b = v13_foreign_over_glyph_budget(name);
        if hits > b {
            failures.push(format!(
                "{name}: {hits} foreign label/wire over power-glyph body > budget {b}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "item(2) foreign label/wire over power-glyph body:\n  {}",
        failures.join("\n  "),
    );
}

#[test]
fn item3_interface_global_labels_clear_foreign_bodies() {
    // Item (3): an interface global-label must not overlap a foreign
    // symbol body (diff_pair `in1` over Q1). This is the SAME property
    // `v13_labels_dont_overlap_symbol_body` already enforces at budget 0
    // for every fixture — so this test is a focused, self-documenting
    // GUARD on the interface-label subset. It measures global-labels only
    // against non-host symbol bodies. Budget 0.
    //
    // NOTE: on current master this passes — the placer / routing-aware
    // orientation refinement positions `in1` reading leftward, away from
    // Q1's body, so the described symptom does not reproduce. It is kept
    // as a regression guard: if a future placement change re-introduces
    // the overlap, both this and `v13_labels_dont_overlap_symbol_body`
    // trip. (See the returned notes in the phase-2 test-author report.)
    let mut failures: Vec<String> = Vec::new();
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let bodies = placed_symbol_bboxes(&root);
        let mut hits = 0usize;
        for (lname, anchor, rot, kind) in &labels_with_kind(&root) {
            if !matches!(kind, TextKind::GlobalLabel { .. }) {
                continue;
            }
            let lbox = text_bbox(lname, *anchor, 1.27, *rot, *kind);
            for (refdes, body) in &bodies {
                if lbox.intersects(body) {
                    eprintln!("{name}: global-label {lname:?} overlaps {refdes} body");
                    hits += 1;
                }
            }
        }
        if hits > 0 {
            failures.push(format!(
                "{name}: {hits} interface global-label↔foreign-body overlap(s)"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "item(3) interface global-label over foreign body (budget 0):\n  {}",
        failures.join("\n  "),
    );
}

// ---------------------------------------------------------------------------
// V11 (correctness) — latent cross-net collinear wire overlap.
// ---------------------------------------------------------------------------

/// True when two axis-aligned wire segments overlap *collinearly* — i.e.
/// they lie on the same grid line (both horizontal at the same Y, or
/// both vertical at the same X) AND their extents share a run of
/// strictly positive length. A shared single endpoint (touch of length
/// zero) is NOT an overlap; a shared interval is.
///
/// All fixtures land on the 1.27 mm grid, so we compare the integer
/// micrometre keys from [`qkey`] for exactness (no float tolerance).
fn segments_collinearly_overlap(s1: &(Pt, Pt), s2: &(Pt, Pt)) -> bool {
    // Two closed integer intervals share a run of strictly positive
    // length (a shared single endpoint returns false).
    fn intervals_overlap(p: (i64, i64), q: (i64, i64)) -> bool {
        p.0.min(p.1).max(q.0.min(q.1)) < p.0.max(p.1).min(q.0.max(q.1))
    }

    // Endpoints as integer-micrometre (x, y) keys.
    let (a0, a1) = (qkey(s1.0.0, s1.0.1), qkey(s1.1.0, s1.1.1));
    let (b0, b1) = (qkey(s2.0.0, s2.0.1), qkey(s2.1.0, s2.1.1));

    // Both horizontal on a shared Y: overlap runs along X.
    if a0.1 == a1.1 && b0.1 == b1.1 && a0.1 == b0.1 {
        return intervals_overlap((a0.0, a1.0), (b0.0, b1.0));
    }
    // Both vertical on a shared X: overlap runs along Y.
    if a0.0 == a1.0 && b0.0 == b1.0 && a0.0 == b0.0 {
        return intervals_overlap((a0.1, a1.1), (b0.1, b1.1));
    }
    false
}

/// Per-fixture budget for cross-net collinear wire overlaps. This is a
/// **correctness** (Tier-0, V11) property, not a quality one: two
/// different-net wires sharing a collinear run are a *latent short* —
/// they stay electrically distinct today only because no junction dot
/// or nudge merges them, but a single added junction (or any router
/// tweak that snaps an endpoint onto the shared run) would merge the two
/// nets into one. ERC does not catch it because the segments carry no
/// junction. The budget is now **zero on every fixture**:
/// [`CROSS_NET_V02_ESCALATIONS`] is empty, the last entry having been
/// retired. Any non-zero count is a defect to fix in the router or the
/// placer, never a budget to raise.
fn cross_net_overlap_budget(name: &str) -> usize {
    usize::from(CROSS_NET_V02_ESCALATIONS.contains(&name))
}

/// Fixtures exercised by [`no_cross_net_collinear_wire_overlap`].
///
/// This is deliberately a *separate* list from the shared `SHEETS`
/// const: `SHEETS` gates ~20 unrelated budget tests (crossing,
/// wire-length, V12, min-gap) that the opamp fixtures are not tuned
/// for, so broadening `SHEETS` to reach the opamp cases would cascade
/// failures. This verifier therefore carries its own list.
///
/// Every fixture's overlap count is checked against
/// `cross_net_overlap_budget`, which is now a hard **0** everywhere —
/// three fixtures were escalations at various points (`diff_pair`,
/// `multivibrator`, `opamp_definition_level`) and all three have been
/// resolved. See [`CROSS_NET_V02_ESCALATIONS`] for what each wall was
/// and what removed it.
const ALL_FIXTURES_FOR_CROSS_NET: &[&str] = &[
    "rc_lowpass",
    "common_emitter",
    "opamp_inverting_real",
    "opamp_inverting",
    "diff_pair",
    "multivibrator",
    "opamp_definition_level",
    "port_shapes",
    "rc_lowpass_ports",
    "named_rails",
    "rc_phase_shift",
];

/// Symmetric fixtures whose two mirror-image sub-circuits force two
/// different nets' trunks onto one channel, producing a cross-net
/// collinear overlap (a latent V11 short) that the **minimal
/// single-grid-cell jog cannot clear without raising a per-fixture
/// budget** — the documented v0.2 channel-router boundary. The router's
/// deconfliction pass tries both the victim and (fallback) the winner
/// net here; on each of these two *neither* net can land in *either*
/// direction (both jog directions blocked). Each hits a distinct wall:
///
/// * `multivibrator` (b1/b2 at y=54.61): the up-track (y=53.34) runs the
///   trunk through a b1 pin at (57.15,53.34) — a **V11** short (G3
///   reject); the down-track (y=55.88) crosses b1's trunk — **+1
///   interior crossing** (budget 4→5).
/// * `opamp_definition_level` (out1/out2 at y=40.64): the down-track
///   (y=41.91) crosses an opamp-triangle body — **V12** (G2 reject); the
///   up-track (y=39.37) has no clean landing among the input stubs.
///
/// `multivibrator` has since been RESOLVED and is removed from this
/// list, ratcheting its budget 1 → 0. What resolved it was not the v0.2
/// channel router but the Tier-0 rollback in `spice_route::route`: when
/// the deconfliction pass cannot separate a pair, the lower-priority
/// net's V5 outward stub is dropped and the net re-routed, which vacates
/// the contested channel outright instead of shuffling one track
/// sideways within it. The b1/b2 wall above is real — it is why the
/// *jog* cannot land — but the jog is no longer the only remedy.
///
/// `opamp_definition_level` has since been RESOLVED too, and the list is
/// now EMPTY. Its wall was never really a router one: out1/out2 shared a
/// channel because the two channels were X-interleaved and drawn
/// backwards, which in turn came from `place_seed`'s hardcoded 5-cell
/// within-bucket Y stride seeding two oversized opamp bodies on top of
/// one another. With the channels laid out left-to-right and side by
/// side, out1 and out2 no longer contend for one track and the plain
/// trees do not overlap. Keep this list empty; a new entry needs the
/// same evidence these three carried.
const CROSS_NET_V02_ESCALATIONS: &[&str] = &[];

#[allow(clippy::too_many_lines)]
#[test]
fn no_cross_net_collinear_wire_overlap() {
    // Each emitted wire segment is mapped to its net by reusing the
    // V11 island machinery: `build_wire_uf` groups wire endpoints into
    // connected components (islands) and `assign_island_nets` labels
    // each island with the single net carried by the pins that touch
    // it. Two different-net segments that share a collinear run are the
    // defect. A shared *endpoint* between same-net segments is fine
    // (that's ordinary connectivity); we only flag a positive-length
    // overlap between segments whose islands resolve to two DISTINCT,
    // known nets.
    //
    // Two different-net wire segments that share a collinear run are a
    // latent V11 short (distinct today only for want of a junction dot).
    // This verifier checks each fixture in `ALL_FIXTURES_FOR_CROSS_NET`
    // (its own list, not the shared `SHEETS`) against
    // `cross_net_overlap_budget` — a hard 0 where the router's
    // victim-or-winner single-track jog resolves the channel share, or a
    // budget-1 high-water for the two `CROSS_NET_V02_ESCALATIONS`
    // (`multivibrator`, `opamp_definition_level`) where neither net can
    // land in either direction and the pass escalates with a warning
    // (deferred to the v0.2 channel router — see that const for the
    // per-fixture wall). `diff_pair` is now a hard 0 via the winner-jog
    // fallback. Resolving an escalation ratchets its budget down to 0.
    let mut failures: Vec<String> = Vec::new();
    for name in ALL_FIXTURES_FOR_CROSS_NET {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);

        let pins = world_pins_for_sheet(&src, &root);
        let pin_index = build_pin_index(&pins);
        let wires = wire_segments(&root);
        let (coord_idx, mut uf, _coords) = build_wire_uf(&wires);
        let (comp_net, _extras) = assign_island_nets(&wires, &coord_idx, &mut uf, &pin_index, name);

        // Resolve each segment to its island net (None = wire island
        // with no pin contact — net unknown, skip).
        let seg_net: Vec<Option<String>> = wires
            .iter()
            .map(|(a, _b)| {
                let r = uf.find(coord_idx[&qkey(a.0, a.1)]);
                comp_net.get(&r).cloned()
            })
            .collect();

        let mut overlaps = 0usize;
        for i in 0..wires.len() {
            for j in (i + 1)..wires.len() {
                let (Some(ni), Some(nj)) = (&seg_net[i], &seg_net[j]) else {
                    continue;
                };
                if ni == nj {
                    // Same net sharing a collinear run is ordinary
                    // connectivity, not a short.
                    continue;
                }
                if segments_collinearly_overlap(&wires[i], &wires[j]) {
                    let (a, b) = wires[i];
                    let (c, d) = wires[j];
                    eprintln!(
                        "{name}: cross-net collinear overlap — net {ni:?} wire \
                         ({:.2},{:.2})→({:.2},{:.2}) overlaps net {nj:?} wire \
                         ({:.2},{:.2})→({:.2},{:.2})",
                        a.0, a.1, b.0, b.1, c.0, c.1, d.0, d.1,
                    );
                    overlaps += 1;
                }
            }
        }

        let budget = cross_net_overlap_budget(name);
        if overlaps > budget {
            failures.push(format!(
                "{name}: {overlaps} cross-net collinear wire overlap(s) > budget {budget} \
                 (latent V11 short)"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "cross-net collinear wire overlap regressions:\n  {}",
        failures.join("\n  "),
    );
}

#[test]
fn conversion_is_deterministic() {
    // Converting the same netlist twice must produce byte-identical output.
    //
    // It did not: iterating `HashMap`/`HashSet` in the router's stub
    // coalescing, Steiner junction emission, conflict endpoint collection
    // and the force-directed edge build leaked hash order into the emitted
    // wire and junction ordering, so three runs of one binary on one input
    // gave three different files. That silently undermines every
    // position-comparing test (`baseline_lock` especially), makes diffing
    // two conversions meaningless, and hands users a schematic that
    // reshuffles for no reason.
    //
    // Those maps are now `BTreeMap`/`BTreeSet`. This test is the guard:
    // any future hash-ordered iteration that reaches the output trips it.
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let first = {
            let tmp = tempdir(name);
            std::fs::read_to_string(spice_to_kicad(&src, &tmp).expect("spice2kicad"))
                .expect("read first conversion")
        };
        for attempt in 2..=4 {
            let tmp = tempdir(name);
            let again = std::fs::read_to_string(spice_to_kicad(&src, &tmp).expect("spice2kicad"))
                .expect("read repeat conversion");
            assert!(
                again == first,
                "{name}: conversion #{attempt} differs from #1 \
                 ({} vs {} bytes) — output is not deterministic",
                again.len(),
                first.len(),
            );
        }
    }
}

/// World coordinates of every hierarchical `(sheet … (pin …))` anchor.
///
/// Sheet pins are connection points exactly as symbol pins are, but
/// `world_pins_for_sheet` derives its list from resolved SPICE elements
/// and so does not see them. Without this, every wire meeting a child
/// sheet reads as unattached.
fn sheet_pin_positions(root: &Value) -> Vec<Pt> {
    let mut out = Vec::new();
    for sheet in children(root, "sheet") {
        for pin in children(sheet, "pin") {
            if let Some(at) = find_child(pin, "at") {
                let mut it = list_iter(at);
                it.next();
                if let (Some(x), Some(y)) = (it.next().and_then(as_f64), it.next().and_then(as_f64))
                {
                    out.push((x, y));
                }
            }
        }
    }
    out
}

/// No dangling whiskers: every wire end attaches to something.
///
/// A **whisker** is a wire endpoint that touches nothing — no pin, no
/// sheet pin, no label anchor, no other wire. The reader sees a stray
/// stub hanging off the drawing and goes looking for the connection it
/// implies; there isn't one.
///
/// This class of defect was invisible to the suite for its entire
/// history, which is why it recurred. Restoring the V5 collinear
/// outward stub (`85b6469`) left three of them across the fixtures, and
/// nothing failed — worse, two of the three were *scored as V5
/// compliance*, because `count_outward_violations` credits any wire
/// leaving a pin outward without asking whether its far end goes
/// anywhere. A dangling stub is a Tier-1 readability defect and V5 is
/// Tier-2, so this test is the one that must hold; see
/// `cleanup::trim_whiskers` for the fix.
///
/// **Budget 0, and it should never rise.** Unlike the aesthetic
/// budgets, there is no such thing as an acceptable residual whisker:
/// a wire end either attaches or it is dead ink. The router can always
/// satisfy this by not emitting the segment.
#[test]
fn no_dangling_whiskers_across_fixtures() {
    #[allow(clippy::cast_possible_truncation)]
    let qk = |p: Pt| ((p.0 * 1000.0).round() as i64, (p.1 * 1000.0).round() as i64);
    #[allow(clippy::cast_precision_loss)]
    let unq = |v: i64| v as f64 / 1000.0;
    let mut failures: Vec<String> = Vec::new();
    for name in SHEETS {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);
        let wires = wire_segments(&root);

        let mut anchors: HashSet<(i64, i64)> = world_pins_for_sheet(&src, &root)
            .iter()
            .map(|p| qk((p.x_mm, p.y_mm)))
            .collect();
        anchors.extend(sheet_pin_positions(&root).into_iter().map(qk));
        anchors.extend(label_positions(&root).into_iter().map(|(_, p)| qk(p)));

        let mut degree: HashMap<(i64, i64), usize> = HashMap::new();
        for &(a, b) in &wires {
            *degree.entry(qk(a)).or_default() += 1;
            *degree.entry(qk(b)).or_default() += 1;
        }
        for (&pt, &deg) in &degree {
            if deg != 1 || anchors.contains(&pt) {
                continue;
            }
            // A lone endpoint landing on another wire's interior is a
            // T-connection, which reads (and connects) as attached.
            let on_interior = wires.iter().any(|&(a, b)| {
                let (a, b) = (qk(a), qk(b));
                if pt == a || pt == b {
                    return false;
                }
                let between = |v: i64, s: i64, e: i64| v > s.min(e) && v < s.max(e);
                (a.0 == b.0 && pt.0 == a.0 && between(pt.1, a.1, b.1))
                    || (a.1 == b.1 && pt.1 == a.1 && between(pt.0, a.0, b.0))
            });
            if !on_interior {
                failures.push(format!(
                    "{name}: wire end at ({:.2}, {:.2}) attaches to nothing",
                    unq(pt.0),
                    unq(pt.1),
                ));
            }
        }
    }
    failures.sort();
    assert!(
        failures.is_empty(),
        "dangling whiskers (budget 0):\n  {}",
        failures.join("\n  "),
    );
}
