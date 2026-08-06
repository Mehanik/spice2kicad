//! **A2 — round-trip connectivity certificate** (Milestone A / Weave
//! §verification).
//!
//! A whole-file, self-contained companion to the V11 electrical-safety
//! suite. Where the V11 tests
//! (`electrical_safety.rs::v11_no_foreign_pin_coincidence`) ask a *local*
//! question — "does any wire/label touch a FOREIGN pin?" — A2 asks the
//! *global* one: reconstruct the ENTIRE net partition from the emitted
//! geometry and compare it, net-for-net, against the source netlist's
//! partition. That catches two failure modes a foreign-pin scan can miss
//! by construction:
//!
//!  * a **silent split** — one source net drawn as two geometric islands
//!    that KiCad would import as two distinct nets (an open), even though
//!    no foreign pin is ever touched;
//!  * a **silent merge** — two source nets fused into one geometric
//!    component (a short). V11 catches the wire-on-foreign-pin flavour of
//!    this; A2 also catches a merge produced purely by label/glyph naming
//!    or by a shared coordinate that the local scan does not enumerate.
//!
//! This is deliberately NOT the CLI's `kicad-cli`-driven post-emit
//! connectivity check (see `placement_stability.rs::convert`): it needs
//! no external tool, and it grades the emitted `(wire …)` / `(symbol …)`
//! / `(label …)` geometry directly against the resolved SPICE netlist, so
//! it runs everywhere the suite runs.
//!
//! # Reconstruction model (mirrors KiCad connectivity)
//!
//! Nets are rebuilt by union-find over *geometric coincidence*, plus the
//! by-name rule KiCad uses for power rails and labels:
//!
//!  1. The two endpoints of every `(wire …)` are unioned. The router
//!     splits every same-net attachment into an endpoint-to-endpoint join
//!     (`spice-route/src/cleanup.rs::split_at_interior_attachments`), so
//!     endpoint coincidence alone reconstructs the Steiner trees; the
//!     interior-through-pin case below covers the one exception.
//!  2. Any component pin, power-glyph anchor, or label anchor that lands
//!     on a wire — at an endpoint OR strictly inside it (V11's
//!     interior-through-pin electrical rule) — is unioned into that wire.
//!  3. Same-coordinate identity is implicit: pins / glyphs / labels /
//!     wire ends that share a quantised coordinate are the same union
//!     node.
//!  4. **By name**: every rail glyph (`power:*`, excluding `PWR_FLAG`) and
//!     every label (plain or global) carrying the SAME canonical net name
//!     is unioned — KiCad connects power nets and same-name labels by
//!     name, not by wire.
//!
//! # The certificate
//!
//! Component terminals — real `(refdes, kicad-pin)` pairs of the placed
//! non-glyph symbols — are the shared vertex set of both partitions. The
//! source partition groups them by `ResolvedElement::nodes`; the emitted
//! partition groups them by reconstructed union-find component. The two
//! must agree exactly:
//!
//!  * **no merge** — two terminals on DIFFERENT source nets never share a
//!    reconstructed component;
//!  * **no split** — two terminals on the SAME source net always share a
//!    reconstructed component.
//!
//! This is a categorical Tier-0 correctness gate, like V11: budget 0 on
//! every fixture, no per-fixture table — a hard assert that the
//! partitions match. `;@ ignore`d elements are undrawn and excluded
//! (they never reach `resolved.elements`).

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use common::spice_to_kicad;
use kicad_symbols::{Library, Orientation, Rotation};
use lexpr::Value;
use spice_diagnostics::FileId;

// ---------------------------------------------------------------------------
// Fixtures / driver (mirrors electrical_safety.rs).
// ---------------------------------------------------------------------------

/// The ten fixtures every invariant verifier drives.
const FIXTURES: &[&str] = &[
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

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spice2kicad-rtc-{pid}-{seq}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn parse(path: &std::path::Path) -> Value {
    let src = std::fs::read_to_string(path).expect("read sch");
    lexpr::from_str(&src).expect("parse sch as lexpr")
}

// ---------------------------------------------------------------------------
// lexpr helpers (copied from electrical_safety.rs).
// ---------------------------------------------------------------------------

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

/// Quantise mm coords to integer micrometres for exact hash-key equality
/// on the 1.27 mm KiCad grid.
#[allow(clippy::cast_possible_truncation)]
fn qkey(x: f64, y: f64) -> (i64, i64) {
    ((x * 1000.0).round() as i64, (y * 1000.0).round() as i64)
}

/// Canonical net identity: SPICE ground `"0"` → `GND`, every other rail
/// uppercased. This is the emitter's rail-name convention (R-6); applying
/// it to both the reconstructed by-name union and the source grouping
/// keeps `vcc` and `VCC` the same net on both sides.
fn canonical_net(net: &str) -> String {
    if net == "0" {
        "GND".to_string()
    } else {
        net.to_ascii_uppercase()
    }
}

// ---------------------------------------------------------------------------
// Library + placed-symbol pose (copied from electrical_safety.rs).
// ---------------------------------------------------------------------------

fn load_test_library() -> Library {
    let libs_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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

fn symbol_value(sym: &Value) -> Option<String> {
    for prop in children(sym, "property") {
        let mut it = list_iter(prop);
        it.next();
        let key = it.next().and_then(as_str);
        let val = it.next().and_then(as_str);
        if key == Some("Value") {
            return val.map(str::to_owned);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Emitted-geometry extraction.
// ---------------------------------------------------------------------------

/// A component terminal — the shared vertex set of both partitions.
struct Terminal {
    refdes: String,
    pin: String,
    coord: (i64, i64),
    source_net: String,
}

/// Every axis-aligned `(wire …)` segment, as quantised endpoint pairs.
fn wire_segments(root: &Value) -> Vec<((i64, i64), (i64, i64))> {
    let mut out = Vec::new();
    for w in children(root, "wire") {
        let Some(pts) = find_child(w, "pts") else {
            continue;
        };
        let xys: Vec<&Value> = list_iter(pts).filter(|c| head(c) == Some("xy")).collect();
        if xys.len() < 2 {
            continue;
        }
        let pt = |v: &Value| -> Option<(i64, i64)> {
            let mut it = list_iter(v);
            it.next();
            let x = it.next().and_then(as_f64)?;
            let y = it.next().and_then(as_f64)?;
            Some(qkey(x, y))
        };
        if let (Some(a), Some(b)) = (pt(xys[0]), pt(xys[1])) {
            out.push((a, b));
        }
    }
    out
}

/// Quantised interior grid coords of an axis-aligned segment (endpoints
/// excluded). Steps the 1.27 mm grid; a diagonal (already a defect) is
/// skipped rather than enumerated off-grid.
fn interior_grid_coords(a: (i64, i64), b: (i64, i64)) -> Vec<(i64, i64)> {
    const GRID_UM: i64 = 1270;
    if a == b {
        return Vec::new();
    }
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    if dx != 0 && dy != 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    if dx == 0 {
        let step = if dy > 0 { GRID_UM } else { -GRID_UM };
        let mut y = a.1 + step;
        while (step > 0 && y < b.1) || (step < 0 && y > b.1) {
            out.push((a.0, y));
            y += step;
        }
    } else {
        let step = if dx > 0 { GRID_UM } else { -GRID_UM };
        let mut x = a.0 + step;
        while (step > 0 && x < b.0) || (step < 0 && x > b.0) {
            out.push((x, a.1));
            x += step;
        }
    }
    out
}

/// Plain- and global-label anchors as `(canonical_net, coord)`.
fn label_nodes(root: &Value) -> Vec<(String, (i64, i64))> {
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
            let (Some(x), Some(y)) = (it.next().and_then(as_f64), it.next().and_then(as_f64))
            else {
                continue;
            };
            out.push((canonical_net(name), qkey(x, y)));
        }
    }
    out
}

/// Rail power-glyph anchors as `(canonical_net, coord)`. `PWR_FLAG`
/// carries no rail net in its Value (its Value is literally `PWR_FLAG`),
/// so it is excluded from the by-name union — it still participates
/// geometrically via coordinate coincidence with the rail pin it sits on.
fn rail_glyph_nodes(root: &Value) -> Vec<(String, (i64, i64))> {
    let mut out = Vec::new();
    for sym in children(root, "symbol") {
        let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
            continue;
        };
        if !refdes.starts_with("#PWR") {
            continue;
        }
        if !lib_id.starts_with("power:") || lib_id == "power:PWR_FLAG" {
            continue;
        }
        let Some((ox, oy, _)) = placed_symbol_pose(sym) else {
            continue;
        };
        let Some(net) = symbol_value(sym) else {
            continue;
        };
        out.push((canonical_net(&net), qkey(ox, oy)));
    }
    out
}

/// Build the component terminals for a fixture: walk the resolved SPICE
/// netlist for the ground-truth `(refdes, kicad-pin) → net` map, then
/// place each library pin through the emitted symbol pose to recover its
/// world coordinate. Power-glyph (`#PWR…`) symbols are not SPICE
/// elements and contribute no terminals.
fn terminals_for_sheet(
    spice_path: &std::path::Path,
    root: &Value,
    library: &Library,
) -> Vec<Terminal> {
    let source = std::fs::read_to_string(spice_path).expect("read spice fixture");
    let parsed = spice_parser::parse(&source, FileId(0)).expect("parse spice fixture");
    let resolved = spice_resolve::resolve(&parsed.netlist, library).expect("resolve spice fixture");

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

    let mut out = Vec::new();
    for sym in children(root, "symbol") {
        let Some((refdes, lib_id)) = placed_symbol_refdes_and_lib_id(sym) else {
            continue;
        };
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
            let Some(net) = pin_to_net.get(tp.number.as_str()) else {
                continue;
            };
            out.push(Terminal {
                refdes: refdes.clone(),
                pin: tp.number.clone(),
                coord: qkey(ox + tp.x, oy - tp.y),
                source_net: canonical_net(net),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Union-find over geometric coincidence.
// ---------------------------------------------------------------------------

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
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}

/// Interns quantised coords and unions them into geometric components.
struct GeomPartition {
    idx: HashMap<(i64, i64), usize>,
    uf: UnionFind,
}

impl GeomPartition {
    /// Build the reconstructed net partition for one sheet.
    fn build(root: &Value, terminals: &[Terminal]) -> Self {
        let wires = wire_segments(root);
        let labels = label_nodes(root);
        let glyphs = rail_glyph_nodes(root);

        // Intern every coordinate the model can reference.
        let mut idx: HashMap<(i64, i64), usize> = HashMap::new();
        let mut coords: Vec<(i64, i64)> = Vec::new();
        let mut intern = |k: (i64, i64), coords: &mut Vec<(i64, i64)>| -> usize {
            *idx.entry(k).or_insert_with(|| {
                coords.push(k);
                coords.len() - 1
            })
        };
        for (a, b) in &wires {
            intern(*a, &mut coords);
            intern(*b, &mut coords);
        }
        for t in terminals {
            intern(t.coord, &mut coords);
        }
        for (_, c) in labels.iter().chain(glyphs.iter()) {
            intern(*c, &mut coords);
        }

        let mut uf = UnionFind::new(coords.len());

        // (1) wire endpoints.
        for (a, b) in &wires {
            uf.union(idx[a], idx[b]);
        }

        // (2) any pin / glyph / label anchor that lands on a wire —
        // endpoint or strict interior — joins that wire. Endpoint
        // coincidence is already covered by the shared coord key; the
        // interior case is the V11 interior-through-pin electrical rule.
        let attach: HashSet<(i64, i64)> = terminals
            .iter()
            .map(|t| t.coord)
            .chain(labels.iter().map(|(_, c)| *c))
            .chain(glyphs.iter().map(|(_, c)| *c))
            .collect();
        for (a, b) in &wires {
            for k in interior_grid_coords(*a, *b) {
                if attach.contains(&k) {
                    uf.union(idx[a], idx[&k]);
                }
            }
        }

        // (4) by-name: rail glyphs and labels sharing a canonical net
        // name connect by name, not by wire.
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (net, c) in labels.iter().chain(glyphs.iter()) {
            by_name.entry(net.clone()).or_default().push(idx[c]);
        }
        for members in by_name.values() {
            for w in members.windows(2) {
                uf.union(w[0], w[1]);
            }
        }

        Self { idx, uf }
    }

    fn component_of(&mut self, coord: (i64, i64)) -> usize {
        let i = self.idx[&coord];
        self.uf.find(i)
    }
}

// ---------------------------------------------------------------------------
// The certificate.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
#[test]
fn emitted_geometry_round_trips_to_source_netlist_across_fixtures() {
    let library = load_test_library();
    let mut failures: Vec<String> = Vec::new();

    for name in FIXTURES {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse(&sch);

        let terminals = terminals_for_sheet(&src, &root, &library);
        let mut geom = GeomPartition::build(&root, &terminals);

        // Reconstructed component of each terminal, plus its source net.
        // Terminal identity is `refdes.pin`.
        let mut term_comp: BTreeMap<String, usize> = BTreeMap::new();
        let mut term_src: BTreeMap<String, String> = BTreeMap::new();
        for t in &terminals {
            let id = format!("{}.{}", t.refdes, t.pin);
            term_comp.insert(id.clone(), geom.component_of(t.coord));
            term_src.insert(id, t.source_net.clone());
        }

        // --- no merge: each reconstructed component carries one source net.
        let mut comp_to_nets: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
        for (id, comp) in &term_comp {
            comp_to_nets
                .entry(*comp)
                .or_default()
                .insert(term_src[id].clone());
        }
        for (comp, nets) in &comp_to_nets {
            if nets.len() > 1 {
                let members: Vec<&String> = term_comp
                    .iter()
                    .filter(|(_, c)| *c == comp)
                    .map(|(id, _)| id)
                    .collect();
                failures.push(format!(
                    "{name}: SILENT MERGE — reconstructed component fuses distinct source \
                     nets {nets:?}; terminals {members:?}"
                ));
            }
        }

        // --- no split: each source net occupies one reconstructed component.
        let mut net_to_comps: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
        for (id, comp) in &term_comp {
            net_to_comps
                .entry(term_src[id].clone())
                .or_default()
                .insert(*comp);
        }
        for (net, comps) in &net_to_comps {
            if comps.len() > 1 {
                // Report the terminals grouped by their island so the
                // offending disconnection is legible.
                let mut islands: BTreeMap<usize, Vec<&String>> = BTreeMap::new();
                for (id, comp) in &term_comp {
                    if &term_src[id] == net {
                        islands.entry(*comp).or_default().push(id);
                    }
                }
                let island_list: Vec<Vec<&String>> = islands.into_values().collect();
                failures.push(format!(
                    "{name}: SILENT SPLIT — source net {net:?} reconstructs as \
                     {} disconnected islands: {island_list:?}",
                    comps.len()
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "A2 round-trip connectivity certificate failed \
         (emitted geometry does not reconstruct the source net partition):\n  {}",
        failures.join("\n  "),
    );
}
