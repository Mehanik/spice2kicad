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
use kicad_symbols::{Orientation, Rotation};
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

type Pt = (f64, f64);

#[derive(Debug, Clone, Copy)]
struct Bbox {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Bbox {
    /// AABB intersection. Inclusive on edges; coincident-edge cases
    /// (a label touching a body's edge at a pin coordinate) are
    /// *quality* defects, not correctness ones, so the verifier
    /// treats them as overlap when both bboxes have non-zero area.
    fn intersects(&self, other: &Bbox) -> bool {
        self.x0 < other.x1 && self.x1 > other.x0 && self.y0 < other.y1 && self.y1 > other.y0
    }

    fn intersects_segment(&self, a: Pt, b: Pt) -> bool {
        // Strict-interior test mirroring `spice_route::types::Bbox`.
        let eps = 0.1;
        let xlo = self.x0 + eps;
        let xhi = self.x1 - eps;
        let ylo = self.y0 + eps;
        let yhi = self.y1 - eps;
        if xlo >= xhi || ylo >= yhi {
            return false;
        }
        let (x1, y1) = a;
        let (x2, y2) = b;
        if x1.max(x2) <= xlo || x1.min(x2) >= xhi {
            return false;
        }
        if y1.max(y2) <= ylo || y1.min(y2) >= yhi {
            return false;
        }
        if (x1 - x2).abs() < f64::EPSILON {
            x1 > xlo && x1 < xhi && y1.min(y2) < yhi && y1.max(y2) > ylo
        } else if (y1 - y2).abs() < f64::EPSILON {
            y1 > ylo && y1 < yhi && x1.min(x2) < xhi && x1.max(x2) > xlo
        } else {
            // The router only emits axis-aligned segments; treat
            // diagonals (shouldn't exist) as non-intersecting.
            false
        }
    }

    #[allow(dead_code)]
    fn contains(&self, p: Pt) -> bool {
        let eps = 0.1;
        p.0 > self.x0 + eps && p.0 < self.x1 - eps && p.1 > self.y0 + eps && p.1 < self.y1 - eps
    }
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

const SHEETS: &[&str] = &[
    "rc_lowpass",
    "common_emitter",
    "multivibrator",
    "diff_pair",
    "opamp_inverting_real",
];

/// Per-fixture crossing budget. After the V11/V12 cascade + Steiner-
/// junction-move step + maze fallback, every router-fixable case is
/// gone across all five v0.1 fixtures. A non-zero budget here would
/// be a regression: every fixture should route clean.
fn v12_crossing_budget(_name: &str) -> usize {
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
                Some(n) => (*n).to_string(),
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

/// Which kind of text we're sizing a bbox for. Determines anchor
/// semantics (left vs centred) and any flavour-specific padding
/// (chevron lead for global labels).
#[derive(Debug, Clone, Copy)]
enum TextKind {
    /// Plain `(label …)` — KiCad anchors the text at the left edge.
    PlainLabel,
    /// `(global_label …)` — chevron-bordered tag; the chevron adds an
    /// extra `~0.6 × size` of horizontal lead on the anchor side.
    GlobalLabel,
    /// `(property "Reference" …)` text — anchor centred or left
    /// depending on `(justify …)`. The emitter now writes `justify
    /// left` (V13 Step 5) so we model it as left-anchored.
    PropertyReference,
    /// `(property "Value" …)` text — same anchor rules as Reference.
    PropertyValue,
}

/// Approximate the rendered text bbox of a label or property string.
///
/// References: KiCad's Newstroke font has an average advance of
/// roughly 0.6 × glyph height (see `../kicad-source/eeschema/sch_field.cpp`
/// and `../kicad-source/eeschema/sch_label.cpp`); we add 0.8 × size of
/// slack to absorb hinting variance and the small lead/trail margins
/// KiCad's renderer applies. Height is taken as 1.4 × size to cover
/// ascender + descender + line spacing.
///
/// `orientation_deg` rotates the unrotated bbox about the anchor and
/// the function returns the axis-aligned bounding box of the rotated
/// shape (matches what eeschema considers the field's visible bbox
/// for collision purposes).
fn text_bbox(text: &str, anchor: Pt, size_mm: f64, orientation_deg: u16, kind: TextKind) -> Bbox {
    #[allow(clippy::cast_precision_loss)]
    let chars = text.chars().count() as f64;
    let width = chars * 0.6 * size_mm + 0.8 * size_mm;
    let height = 1.4 * size_mm;
    let chevron_lead = match kind {
        TextKind::GlobalLabel => 0.6 * size_mm,
        _ => 0.0,
    };
    // Unrotated bbox in the anchor's local frame. Anchor is the
    // *left edge* for left-justified text; the bbox extends to the
    // right by `width`, half above and half below the baseline.
    // Property text is also left-anchored (the emitter writes
    // `(justify left)`); plain/global labels are likewise anchored
    // on the leftmost edge for `orientation 0`.
    let (lx, rx, ty, by) = match kind {
        TextKind::PlainLabel | TextKind::PropertyReference | TextKind::PropertyValue => {
            (-0.0, width, -height / 2.0, height / 2.0)
        }
        TextKind::GlobalLabel => (
            -chevron_lead,
            width + chevron_lead,
            -height / 2.0,
            height / 2.0,
        ),
    };
    // Rotate the four corners about the anchor. KiCad's schematic
    // file Y axis points DOWN on screen (eeschema renders with the
    // Y-flip on load), and rotation tokens are CCW *on screen*. To
    // produce a file-frame AABB matching what KiCad draws, we negate
    // the sine component so that rot=90 maps right-extending text to
    // upward (i.e. decreasing file Y).
    let theta = f64::from(orientation_deg).to_radians();
    let (s, c) = (theta.sin(), theta.cos());
    let corners = [(lx, ty), (rx, ty), (rx, by), (lx, by)];
    let mut x0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y0 = f64::INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for (px, py) in corners {
        let wx = anchor.0 + c * px + s * py;
        let wy = anchor.1 - s * px + c * py;
        x0 = x0.min(wx);
        x1 = x1.max(wx);
        y0 = y0.min(wy);
        y1 = y1.max(wy);
    }
    Bbox { x0, y0, x1, y1 }
}

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

/// Collect every emitted plain-label and global-label as
/// (net_name, anchor, rot_deg, kind).
#[allow(clippy::similar_names)]
fn labels_with_kind(root: &Value) -> Vec<(String, Pt, u16, TextKind)> {
    let mut out = Vec::new();
    for (sx_tag, lkind) in [
        ("label", TextKind::PlainLabel),
        ("global_label", TextKind::GlobalLabel),
    ] {
        for node in children(root, sx_tag) {
            let Some(name) = list_iter(node).nth(1).and_then(as_str) else {
                continue;
            };
            let Some((x, y, rot)) = at_xy_rot(node) else {
                continue;
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
                "Reference" => TextKind::PropertyReference,
                "Value" => TextKind::PropertyValue,
                _ => continue,
            };
            let Some((px, py, prot)) = at_xy_rot(prop) else {
                continue;
            };
            let size = effects_font_size(prop).unwrap_or(1.27);
            let bbox = text_bbox(val, (px, py), size, prot, tkind);
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
            let lk = qkey(pos.0, pos.1);
            for (a, b) in &wires {
                let ka = qkey(a.0, a.1);
                let ia = coord_idx[&ka];
                let ra = uf.find(ia);
                let Some(wnet) = comp_net.get(&ra) else {
                    continue;
                };
                if wnet == lname {
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
                "Reference" => TextKind::PropertyReference,
                "Value" => TextKind::PropertyValue,
                _ => continue,
            };
            let Some((px, py, prot)) = at_xy_rot(prop) else {
                continue;
            };
            let size = effects_font_size(prop).unwrap_or(1.27);
            let bbox = text_bbox(val, (px, py), size, prot, tkind);
            out.push((format!("{refdes}.{key}"), bbox));
        }
    }
    out
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
fn v5_violation_budget(name: &str) -> usize {
    match name {
        // Ratcheted high-water marks (current measured count on master).
        // The routing-aware orientation-refinement phase (Layout phase
        // 4.5) drove common_emitter (3→0), diff_pair (2→0), and
        // opamp_inverting_real (3→1) down to these values; the budgets
        // follow the ratchet-down policy. A regression trips the test.
        "multivibrator" => 4,
        "opamp_inverting_real" => 1,
        // common_emitter, diff_pair, rc_lowpass, and any other fixture:
        // zero violations.
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
