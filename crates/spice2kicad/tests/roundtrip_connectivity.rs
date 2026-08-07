//! **A2 — round-trip connectivity certificate** (Milestone A / Weave
//! §verification).
//!
//! Where the V11 electrical-safety tests
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
//!    component (a short), including a merge produced purely by
//!    label/glyph naming rather than by a wire.
//!
//! # What changed when this check moved into production (ADR-22)
//!
//! A2 used to be the *only* implementation of this reconstruction. It is
//! now production code — `kicad_emitter::connectivity::check_partition`,
//! run inside `emit_root` / `emit_child_sheet` before any bytes reach
//! disk, refusing with `EmitError::NetPartition`. So the obvious move
//! would be to delete A2, or to reduce it to "call the production check
//! and assert it passed". **Both would destroy its value**, and the
//! reason is worth stating plainly because this project has already been
//! burned by it once (the V13 suite was a byte-copy of the emitter's own
//! text model, so only real `kicad-cli` SVG ink could falsify it).
//!
//! The production check grades the router's output against the router's
//! own input: `collect_net_pins` supplies both the wires' targets and the
//! oracle's pin→net attribution. If that attribution or the pose maths
//! behind it is wrong, the router draws the wrong wire *and the oracle
//! blesses it* — shared fate. It is equally blind to everything that
//! happens after it: page translation and S-expression serialisation.
//!
//! A2 therefore keeps **independent inputs** and shares only the engine:
//!
//!  * **Terminals** are re-derived from scratch — re-parse the `.cir`
//!    through `spice-parser` + `spice-resolve`, and push library pin
//!    geometry through the *emitted* symbol pose. This is the falsifier
//!    for `collect_net_pins`, which production structurally cannot check.
//!  * **Geometry** is read back off the `.kicad_sch` **file on disk**,
//!    after page translation and serialisation, with an independent
//!    parser (`lexpr`). This is the falsifier for the emit tail.
//!  * **The union-find engine** is shared, deliberately. A second
//!    hand-written union-find would encode the *same* beliefs about
//!    KiCad's connectivity semantics, so it could only ever agree and be
//!    wrong in company. The model's true falsifier is KiCad itself — the
//!    CLI's post-emit `kicad-cli` netlist comparison, which every graded
//!    conversion in this suite runs.
//!
//! # The certificate
//!
//! Component terminals — real `(refdes, kicad-pin)` pairs of the placed
//! non-glyph symbols — are the shared vertex set of both partitions. The
//! source partition groups them by `ResolvedElement::nodes`; the emitted
//! partition groups them by reconstructed component. The two must agree
//! exactly: no merge, no split.
//!
//! This is a categorical Tier-0 correctness gate, like V11: budget 0 on
//! every fixture, no per-fixture table. `;@ ignore`d elements are undrawn
//! and excluded (they never reach `resolved.elements`).
//!
//! Two guards keep the gate from passing vacuously — see
//! [`emitted_geometry_round_trips_to_source_netlist_across_fixtures`]'s
//! terminal-count assertion and
//! [`the_reconstruction_is_sensitive_on_real_fixtures`]'s mutations.

mod common;

use std::path::PathBuf;

use common::spice_to_kicad;
use kicad_emitter::connectivity::{PartitionFinding, SheetGeometry, Terminal, check_partition};
use kicad_symbols::{Library, Orientation, Rotation};
use lexpr::Value;
use spice_diagnostics::FileId;

// ---------------------------------------------------------------------------
// Fixtures / driver (mirrors electrical_safety.rs).
// ---------------------------------------------------------------------------

/// The eleven fixtures every invariant verifier drives.
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

/// Canonical net identity: SPICE ground `"0"` → `GND`, every other net
/// uppercased. Applied to the *source* net names only — SPICE node names
/// are case-insensitive, so `vcc` and `VCC` must group as one net when
/// the two partitions are compared. Emitted anchor *names* are compared
/// verbatim, exactly as KiCad compares them.
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
// Independent geometry extraction — off the FILE, with `lexpr`.
// ---------------------------------------------------------------------------

/// Every axis-aligned `(wire …)` segment, as endpoint pairs.
fn wire_segments(root: &Value) -> Vec<((f64, f64), (f64, f64))> {
    let mut out = Vec::new();
    for w in children(root, "wire") {
        let Some(pts) = find_child(w, "pts") else {
            continue;
        };
        let xys: Vec<&Value> = list_iter(pts).filter(|c| head(c) == Some("xy")).collect();
        if xys.len() < 2 {
            continue;
        }
        let pt = |v: &Value| -> Option<(f64, f64)> {
            let mut it = list_iter(v);
            it.next();
            let x = it.next().and_then(as_f64)?;
            let y = it.next().and_then(as_f64)?;
            Some((x, y))
        };
        if let (Some(a), Some(b)) = (pt(xys[0]), pt(xys[1])) {
            out.push((a, b));
        }
    }
    out
}

/// Label anchors as `(text, coord)` — every flavour connects to a
/// same-named label on its sheet.
fn label_nodes(root: &Value) -> Vec<(String, (f64, f64))> {
    let mut out = Vec::new();
    for kind in ["label", "global_label", "hierarchical_label"] {
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
            out.push((name.to_string(), (x, y)));
        }
    }
    out
}

/// Rail power-glyph anchors as `(value, coord)`. `PWR_FLAG` carries no
/// rail net in its Value (its Value is literally `PWR_FLAG`), so it is
/// excluded from the by-name union — it still participates geometrically
/// via coordinate coincidence with the rail pin it sits on.
fn rail_glyph_nodes(root: &Value) -> Vec<(String, (f64, f64))> {
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
        out.push((net, (ox, oy)));
    }
    out
}

fn geometry_for_sheet(root: &Value) -> SheetGeometry {
    let mut named_anchors = label_nodes(root);
    named_anchors.extend(rail_glyph_nodes(root));
    SheetGeometry {
        wires: wire_segments(root),
        named_anchors,
    }
}

// ---------------------------------------------------------------------------
// Independent terminal derivation — source → resolver → library → pose.
// ---------------------------------------------------------------------------

/// Build the component terminals for a fixture: walk the resolved SPICE
/// netlist for the ground-truth `(refdes, kicad-pin) → net` map, then
/// place each library pin through the emitted symbol pose to recover its
/// world coordinate. Power-glyph (`#PWR…`) symbols are not SPICE
/// elements and contribute no terminals.
///
/// **This deliberately does not call `collect_net_pins`.** It is the one
/// derivation of pin→net attribution and pin geometry that the production
/// check cannot perform on itself — see the module docs.
fn terminals_for_sheet(
    spice_path: &std::path::Path,
    root: &Value,
    library: &Library,
) -> Vec<Terminal> {
    use std::collections::HashMap;

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
                id: format!("{}.{}", refdes, tp.number),
                net: canonical_net(net),
                at: (ox + tp.x, oy - tp.y),
            });
        }
    }
    out
}

/// Convert a fixture and return `(terminals, geometry)` — both derived
/// independently of production, from the written file.
fn reconstruct(name: &str, library: &Library) -> (Vec<Terminal>, SheetGeometry) {
    let src = fixtures_dir().join(format!("{name}.cir"));
    let tmp = tempdir(name);
    let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
    let root = parse(&sch);
    (
        terminals_for_sheet(&src, &root, library),
        geometry_for_sheet(&root),
    )
}

// ---------------------------------------------------------------------------
// The certificate.
// ---------------------------------------------------------------------------

#[test]
fn emitted_geometry_round_trips_to_source_netlist_across_fixtures() {
    let library = load_test_library();
    let mut failures: Vec<String> = Vec::new();

    for name in FIXTURES {
        let (terminals, geometry) = reconstruct(name, &library);

        // Vacuity guard. A partition check over an empty terminal set
        // passes trivially, and a silently-empty derivation (a library
        // lookup that stopped matching, a pose that stopped parsing)
        // would turn this whole file green while measuring nothing.
        assert!(
            terminals.len() >= 2,
            "{name}: derived only {} terminal(s) — the certificate below would \
             pass vacuously. This is a defect in A2's own derivation, not in the \
             emitted schematic.",
            terminals.len(),
        );

        for f in check_partition(&terminals, &geometry) {
            failures.push(format!("{name}: {f}"));
        }
    }

    assert!(
        failures.is_empty(),
        "A2 round-trip connectivity certificate failed \
         (emitted geometry does not reconstruct the source net partition):\n  {}",
        failures.join("\n  "),
    );
}

/// **Mutation guard.** The certificate above only tells you something if
/// it *can* fail on the fixtures it grades. A reconstruction that quietly
/// unions everything (or nothing) would pass it forever.
///
/// So: take each real fixture's real reconstruction and inject each of
/// the three edge classes the model implements, one at a time, in the
/// direction that must break it. Every injection must be caught.
///
/// This runs on the same read-back geometry as the certificate — no
/// second conversion — so it costs nothing beyond the mutations.
#[test]
fn the_reconstruction_is_sensitive_on_real_fixtures() {
    let library = load_test_library();
    let mut failures: Vec<String> = Vec::new();

    for name in FIXTURES {
        let (terminals, geometry) = reconstruct(name, &library);

        // Two terminals on genuinely different source nets, needed by the
        // merge injections.
        let a = &terminals[0];
        let Some(b) = terminals.iter().find(|t| t.net != a.net) else {
            failures.push(format!("{name}: fixture has only one net; cannot mutate"));
            continue;
        };

        // (1) wire injection: a segment joining two foreign nets' pins.
        // This is the `v11:` / `conflict:` hazard in its purest form.
        let mut wired = geometry.clone();
        wired.wires.push((a.at, b.at));
        let found = check_partition(&terminals, &wired);
        if !found
            .iter()
            .any(|f| matches!(f, PartitionFinding::Merge { .. }))
        {
            failures.push(format!(
                "{name}: a wire drawn from {} to {} (different nets: {} vs {}) was \
                 NOT reported as a merge — the reconstruction is not reading wires",
                a.id, b.id, a.net, b.net,
            ));
        }

        // (2) name injection: two same-named anchors on foreign nets.
        // This is the merge a foreign-pin scan cannot see at all.
        let mut named = geometry.clone();
        named.named_anchors.push(("A2_INJECTED".to_string(), a.at));
        named.named_anchors.push(("A2_INJECTED".to_string(), b.at));
        let found = check_partition(&terminals, &named);
        if !found
            .iter()
            .any(|f| matches!(f, PartitionFinding::Merge { .. }))
        {
            failures.push(format!(
                "{name}: two same-named anchors on {} and {} (different nets) were \
                 NOT reported as a merge — the by-name rule is not being applied",
                a.id, b.id,
            ));
        }

        // (3) erasure: drop every wire and every anchor. Any multi-pin
        // net must now come apart. If nothing does, the fixture's
        // connectivity is not being measured from geometry at all.
        let found = check_partition(&terminals, &SheetGeometry::default());
        if !found
            .iter()
            .any(|f| matches!(f, PartitionFinding::Split { .. }))
        {
            failures.push(format!(
                "{name}: erasing ALL wires, glyphs and labels produced no split — \
                 the certificate is not measuring the emitted geometry"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "A2 mutation guard failed (the reconstruction is insensitive to defects it \
         must catch):\n  {}",
        failures.join("\n  "),
    );
}
