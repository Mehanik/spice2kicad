//! **Q5 — mutual-alignment near-miss** (v0.2 roadmap A3, Tier 2).
//!
//! A human snaps components that are *almost* on a shared axis onto it, so
//! the connecting wire runs straight instead of jogging. Q5 counts the
//! avoidable near-misses left on the emitted sheet — a whole-sheet
//! OUTPUT-geometry count, measured on the emitted `.kicad_sch` and
//! ratcheted per fixture at the value measured on `master`.
//!
//! # Two metrics live here: `q5` (origin frame) and `q5.pin` (pin frame)
//!
//! `q5` is the original, kept verbatim below. It compares symbol
//! **origins**, and ADR-40 measured what that costs: it and `q3` were the
//! only two metrics in the suite that read origins, and the pin-anchored
//! DC-series column — which puts its members' SHARED PINS on one x,
//! exactly as CLAUDE.md § "Layout invariants" requires — offsets their
//! origins by each symbol's own pin offset. A `Device:Q_NPN_BCE`
//! collector sits 2 cells off its origin, `NEAR_CELLS` is 2, so a
//! correctly pin-collinear column read as an origin near-miss BY
//! CONSTRUCTION. Every spurious pair involved a transistor.
//!
//! `q5.pin` asks the same question between **the pins that share the
//! net** — the two ends of the wire whose straightness the metric's own
//! first paragraph is about. The spurious column pairs vanish there
//! (`common_emitter` loses `Q1↔RC` and `Q1↔RE`; `rc_phase_shift` loses
//! `Q1↔RC`, `Q1↔RE` and `CE↔Q1`), and real near-misses the origin frame
//! could not see appear (`common_emitter` `COUT↔Q1`: 1.27 mm of vertical
//! jog across a 13.97 mm run, while the two origins are far apart on
//! both axes). Neither frame dominates: see the two budget tables.
//!
//! # NOT a leading indicator of V16 — measured, in BOTH frames
//!
//! This module used to claim Q5 was "a **pre-routing leading indicator**
//! of V16 bends". That claim is **falsified**, and re-framing does not
//! rescue it. ADR-40 recorded `q5` moving **+14 while `v16.bends` moved
//! −4** on one construction; measured again here over six registered
//! placer arms × 18 common fixtures, fixture-centred so circuit size
//! cannot carry the correlation:
//!
//! | metric | r vs `v16.bends` | r vs `v16.branches` |
//! | --- | ---: | ---: |
//! | `q5` (origins) | +0.47 | +0.26 |
//! | `q5.pin` (pins) | **+0.01** | **+0.03** |
//!
//! The reason is in the definition, not the frame: a **near**-miss is an
//! INTERMEDIATE state. A placer that pulls connected parts together turns
//! "far apart on both axes" (not counted) into "close on one axis"
//! (counted) on its way to "aligned" (not counted), so the count is
//! non-monotone in quality either way. ADR-23's own `flow-seed` promotion
//! note said as much — "a placer that packs columns tighter naturally
//! produces more of them".
//!
//! Q5 therefore stands or falls on its FIRST claim only — an avoidable
//! jog is a legibility defect whatever the router then does with it —
//! and must never be read as a bend forecast. Like V16 it is Tier-2
//! aesthetic and a **counted quantity**, never a coefficient (CLAUDE.md
//! § constraints-vs-costs / V16 doctrine).
//!
//! # The metric
//!
//! Consider unordered pairs of DRAWN, non-power components `(u, v)` that
//!
//!   * share at least one **Signal-class** net (so a wire runs between
//!     them — an unaligned pair forces a jog), and
//!   * are exactly aligned on **neither** axis.
//!
//! Let `dx`, `dy` be the absolute differences of the two symbols'
//! `(at x y)` origins. The pair is a **near-miss** when
//!
//!   * `dx > 0` and `dy > 0` (aligned on neither axis — a pair already
//!     snapped on one axis is not a near-miss, it is done), AND
//!   * `dx <= NEAR` or `dy <= NEAR` (within a small snap threshold on at
//!     least one axis).
//!
//! `NEAR = NEAR_CELLS` grid cells (1 cell = 1.27 mm). `NEAR_CELLS = 2`: a
//! human would snap a ≤2-cell offset onto the axis; a larger gap is
//! intentional separation, not a near-miss. The per-fixture metric is the
//! integer count of such pairs.
//!
//! Power sources / rail glyphs (refdes starting `#`, `lib_id` `power:*`)
//! and `;@ ignore`d elements take no part: they are decoration hung off a
//! rail pin, never placed flow bodies snapped to a neighbour.
//!
//! # Distinct from what already exists
//!
//! * **V16** (`wire_geometry.rs`) — bends on *routed* wires; Q5 is
//!   pre-routing and never looks at a wire.
//! * **Q3** (`flow_monotonicity.rs`) — left→right *ordering* against the
//!   placer's layer model; Q5 is order-agnostic and measures *offset*.
//!   Q3 has the same origin/pin split, for the same reason.
//! * **V5** (`placement_quality.rs`) — pin-*facing* orientation; Q5 is
//!   about co-alignment, orientation-agnostic.
//! * **F7** (`flow_geometry.rs`, ADR-42) — how FAR apart two elements on
//!   the identical net set are drawn; Q5 is about the last two cells.
//!
//! # Ratchet
//!
//! Zero-slack per-fixture high-water marks (CLAUDE.md § "Budgets are
//! ratchets, not knobs"): each literal equals the count measured on
//! `master` and only ever goes **down**. A commit that removes near-misses
//! SHOULD lower the literal in the same commit; a rise is a placement
//! regression to diagnose, never a budget to bump.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use common::pin_frame::{CELL_UM as PIN_CELL_UM, PinFrame};
use common::spice_to_kicad;
use lexpr::Value;
use spice_diagnostics::FileId;
use spice_layout::net_class::{NetClass, classify_nets};
use spice_resolve::ElementRole;

/// Snap threshold, in grid cells. 1 cell = 1.27 mm. A ≤2-cell offset off
/// a shared axis is what a human would snap flush; beyond it the gap reads
/// as intentional separation.
const NEAR_CELLS: i64 = 2;

/// The KiCad schematic grid pitch, in micrometres (1.27 mm = 50 mil).
const CELL_UM: i64 = 1270;

/// Snap threshold in micrometres. Coordinates are quantised to µm so the
/// `dx == 0` / `dx <= NEAR` comparisons are exact integer tests on a grid
/// far coarser than µm.
const NEAR_UM: i64 = NEAR_CELLS * CELL_UM;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> common::TempDir {
    common::TempDir::new("q5", name)
}

// --- lexpr helpers (mirrors flow_monotonicity.rs) ------------------------

fn list_iter(v: &Value) -> Box<dyn Iterator<Item = &Value> + '_> {
    v.list_iter().map_or_else(
        || Box::new(std::iter::empty()) as Box<dyn Iterator<Item = &Value>>,
        |it| Box::new(it),
    )
}

fn head(v: &Value) -> Option<&str> {
    list_iter(v).next().and_then(as_str)
}

fn as_str(v: &Value) -> Option<&str> {
    v.as_symbol()
        .or_else(|| v.as_str())
        .or_else(|| v.as_keyword())
}

fn as_f64(v: &Value) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))
}

fn find_child<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    list_iter(v).find(|c| c.is_list() && head(c) == Some(name))
}

fn children<'a>(v: &'a Value, name: &str) -> Vec<&'a Value> {
    list_iter(v)
        .filter(|c| c.is_list() && head(c) == Some(name))
        .collect()
}

/// Quantise a millimetre coordinate to micrometres for exact comparison.
#[allow(clippy::cast_possible_truncation)]
fn q(mm: f64) -> i64 {
    (mm * 1000.0).round() as i64
}

/// `refdes -> emitted symbol origin (x, y) in µm` for every DRAWN flow
/// body: every top-level `(symbol …)` whose refdes is not a `#`-glyph and
/// whose `lib_id` is not `power:*`. Power/ground glyphs are decoration
/// hung off a rail pin, never placement participants.
fn drawn_symbol_origin(root: &Value) -> HashMap<String, (i64, i64)> {
    let mut out = HashMap::new();
    for sym in children(root, "symbol") {
        let lib_id = find_child(sym, "lib_id")
            .and_then(|l| list_iter(l).nth(1).and_then(as_str))
            .unwrap_or_default();
        if lib_id.starts_with("power:") {
            continue;
        }
        let mut refdes = None;
        for prop in children(sym, "property") {
            let mut it = list_iter(prop);
            it.next();
            if it.next().and_then(as_str) == Some("Reference") {
                refdes = it.next().and_then(as_str).map(str::to_owned);
                break;
            }
        }
        let Some(refdes) = refdes else { continue };
        if refdes.starts_with('#') {
            continue;
        }
        let Some(at) = find_child(sym, "at") else {
            continue;
        };
        let mut it = list_iter(at);
        it.next(); // head "at"
        let (Some(x), Some(y)) = (it.next().and_then(as_f64), it.next().and_then(as_f64)) else {
            continue;
        };
        out.insert(refdes, (q(x), q(y)));
    }
    out
}

// --- measurement ---------------------------------------------------------

/// One drawn, non-power flow body: its Signal-class nets and its emitted
/// symbol-origin coordinates (µm).
struct AlignElem {
    refdes: String,
    signal_nets: Vec<String>,
    x_um: i64,
    y_um: i64,
}

/// Everything one fixture contributes: the origin-frame bodies (Q5) and
/// the pin frame both `q5` and `q5.pin` are measured over (`q5.pin`).
struct AlignFixture {
    elems: Vec<AlignElem>,
    frame: PinFrame,
}

/// Build the Q5 alignment bodies for a fixture: convert it, then join the
/// resolved netlist's Signal-class nets to the emitted symbol origins.
fn align_elems(name: &str) -> AlignFixture {
    let dir = tempdir(name);
    let sch = spice_to_kicad(&fixtures_dir().join(format!("{name}.cir")), &dir)
        .unwrap_or_else(|e| panic!("convert {name}: {e}"));
    let root = lexpr::from_str(&std::fs::read_to_string(&sch).expect("read sch"))
        .expect("parse sch as lexpr");
    let origins = drawn_symbol_origin(&root);

    // Re-derive the net classification from the same source, exactly as the
    // seed placer does: parse → resolve → check → classify.
    let spice_src =
        std::fs::read_to_string(fixtures_dir().join(format!("{name}.cir"))).expect("read cir");
    let library = load_test_library();
    let parsed = spice_parser::parse(&spice_src, FileId(0)).expect("parse spice");
    let resolved = spice_resolve::resolve(&parsed.netlist, &library).expect("resolve spice");
    let (checked, _diags) = spice_policy::check(resolved).expect("policy check");

    let classes = classify_nets(&checked);

    let mut out = Vec::new();
    for el in &checked.elements {
        // Power sources are lowered to rail glyphs, not flow bodies.
        if matches!(el.role, ElementRole::Power(_)) {
            continue;
        }
        // Only elements actually DRAWN as a (non-glyph) symbol participate;
        // this transparently drops `;@ ignore`d elements and any element
        // lowered to a sheet rather than a body.
        let Some(&(x_um, y_um)) = origins.get(&el.refdes) else {
            continue;
        };
        let signal_nets: Vec<String> = el
            .nodes
            .iter()
            .filter(|n| {
                classes.get(n.as_str()).copied().unwrap_or(NetClass::Signal) == NetClass::Signal
            })
            .cloned()
            .collect();
        out.push(AlignElem {
            refdes: el.refdes.clone(),
            signal_nets,
            x_um,
            y_um,
        });
    }
    let frame = PinFrame::build(&root, &checked.elements);
    AlignFixture { elems: out, frame }
}

/// The load helper mirrors `flow_monotonicity.rs`: the same four fixture
/// libraries the CLI is handed.
fn load_test_library() -> kicad_symbols::Library {
    use kicad_symbols::Library;
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

/// **Q5** — mutual-alignment near-misses. One entry per offending pair
/// `(a, b)` (refdes-sorted within the pair), sorted.
fn q5_near_misses(elems: &[AlignElem]) -> Vec<(String, String)> {
    // net → the drawn flow bodies that carry it as a Signal net.
    let mut net_members: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, e) in elems.iter().enumerate() {
        for net in &e.signal_nets {
            net_members.entry(net.as_str()).or_default().push(i);
        }
    }

    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut out = Vec::new();
    let mut nets: Vec<&&str> = net_members.keys().collect();
    nets.sort_unstable();
    for net in nets {
        let members = &net_members[*net];
        for &a in members {
            for &b in members {
                if a >= b || !seen.insert((a, b)) {
                    continue;
                }
                let (ea, eb) = (&elems[a], &elems[b]);
                let dx = (ea.x_um - eb.x_um).abs();
                let dy = (ea.y_um - eb.y_um).abs();
                // Exactly aligned on either axis → already snapped, not a
                // near-miss. Require aligned on NEITHER axis.
                if dx == 0 || dy == 0 {
                    continue;
                }
                // Near-miss on at least one axis (within the snap threshold).
                if dx <= NEAR_UM || dy <= NEAR_UM {
                    let (lo, hi) = if ea.refdes <= eb.refdes {
                        (ea.refdes.clone(), eb.refdes.clone())
                    } else {
                        (eb.refdes.clone(), ea.refdes.clone())
                    };
                    out.push((lo, hi));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

// --- Q5.pin — the same question, asked in the pin frame ------------------

/// Snap threshold for the pin-frame metric, in grid cells.
///
/// **Re-derived, not carried across.** In the origin frame `NEAR_CELLS = 2`
/// was justified as "what a human would snap flush", and ADR-40 showed the
/// number then collided with a `Q_NPN_BCE`'s own 2-cell collector/emitter
/// offset, so a correctly pin-collinear column read as an origin near-miss
/// by construction. That collision cannot happen here: the pins on the
/// shared net ARE the compared points, so a pin-collinear column measures
/// `dx == 0`.
///
/// The pin frame supplies its own scale. A KiCad symbol's pin pitch is
/// 100 mil = 2.54 mm = **2 grid cells** — the smallest offset that can
/// separate two pins of the same body, and therefore the smallest jog
/// that can be *intentional* rather than a placement residue. A jog of at
/// most one pin pitch is a jog a human snaps out; more than that and the
/// two pins are on genuinely different rows/columns. Numerically equal to
/// the origin-frame constant, from a different argument.
const PIN_NEAR_CELLS: i64 = 2;

/// Snap threshold in micrometres, pin frame.
const PIN_NEAR_UM: i64 = PIN_NEAR_CELLS * PIN_CELL_UM;

/// How one shared net reads between two bodies, in the pin frame.
#[derive(PartialEq, Eq)]
enum NetSnap {
    /// Some pin pair on this net is exactly collinear: the connecting
    /// wire can run straight. Q5's stated postcondition, met.
    Aligned,
    /// Not collinear, but within one pin pitch on an axis — the jog a
    /// human would snap out.
    NearMiss,
    /// Comfortably off-axis: intentional separation, not a near-miss.
    Apart,
}

/// Read one shared net between two bodies in the pin frame.
///
/// An element may present a net on more than one pin (a shorted terminal
/// pair). `Aligned` wins over `NearMiss` over `Apart`: if ANY pin pair on
/// the net can be joined by a straight wire, the net is drawn, and the
/// metric must not charge for the pins that were not used.
fn net_snap(frame: &PinFrame, a: &str, b: &str, net: &str) -> Option<NetSnap> {
    // `None` is unreachable in practice, and NOT a silent skip: both
    // bodies are drawn (that is how they entered `elems`) and both carry
    // `net`, so an absent pin is recorded in `PinFrame::unresolved`,
    // which the verifier asserts is empty before it reads this count.
    let (pa, pb) = (frame.pins(a, net)?, frame.pins(b, net)?);
    let mut best = NetSnap::Apart;
    for &(ax, ay) in pa {
        for &(bx, by) in pb {
            let (dx, dy) = ((ax - bx).abs(), (ay - by).abs());
            if dx == 0 || dy == 0 {
                return Some(NetSnap::Aligned);
            }
            if dx <= PIN_NEAR_UM || dy <= PIN_NEAR_UM {
                best = NetSnap::NearMiss;
            }
        }
    }
    Some(best)
}

/// The worst (smallest) near-miss offset between two bodies, in µm, over
/// every shared Signal net and every pin pair on it — the diagnostic
/// behind a `q5.pin` entry. Only used by the `S2K_Q5_DUMP` path.
fn q5_pin_detail(
    elems: &[AlignElem],
    frame: &PinFrame,
    a: &str,
    b: &str,
) -> Option<(String, i64, i64)> {
    let ea = elems.iter().find(|e| e.refdes == a)?;
    let eb = elems.iter().find(|e| e.refdes == b)?;
    let mut best: Option<(String, i64, i64)> = None;
    for net in &ea.signal_nets {
        if !eb.signal_nets.contains(net) || net_snap(frame, a, b, net) != Some(NetSnap::NearMiss) {
            continue;
        }
        let (Some(pa), Some(pb)) = (frame.pins(a, net), frame.pins(b, net)) else {
            continue;
        };
        for &(ax, ay) in pa {
            for &(bx, by) in pb {
                let (dx, dy) = ((ax - bx).abs(), (ay - by).abs());
                let score = dx.min(dy);
                if best
                    .as_ref()
                    .is_none_or(|&(_, bx2, by2)| score < bx2.min(by2))
                {
                    best = Some((net.clone(), dx, dy));
                }
            }
        }
    }
    best
}

/// **Q5.pin** — mutual-alignment near-misses measured between the pins
/// that share a net, rather than between symbol origins.
///
/// Same pair set, same threshold semantics, same counting unit (one entry
/// per unordered pair). The single difference is the frame: `dx`/`dy` are
/// taken between the two bodies' pins ON THE SHARED NET, which is the
/// geometry the metric's own postcondition is stated in ("so the
/// connecting wire runs straight instead of jogging") and the geometry
/// CLAUDE.md § "Layout invariants" says a placement constraint IS.
///
/// A pair counts once if ANY of its shared Signal nets reads `NearMiss`;
/// a net that reads `Aligned` exempts only itself, because a pair sharing
/// two nets can be straight on one and jog on the other, and the jog is
/// really there.
fn q5_pin_near_misses(elems: &[AlignElem], frame: &PinFrame) -> Vec<(String, String)> {
    let mut net_members: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, e) in elems.iter().enumerate() {
        for net in &e.signal_nets {
            net_members.entry(net.as_str()).or_default().push(i);
        }
    }

    let mut out = Vec::new();
    let mut nets: Vec<&&str> = net_members.keys().collect();
    nets.sort_unstable();
    for net in nets {
        let members = &net_members[*net];
        for &a in members {
            for &b in members {
                if a >= b {
                    continue;
                }
                let (ea, eb) = (&elems[a], &elems[b]);
                if net_snap(frame, &ea.refdes, &eb.refdes, net) != Some(NetSnap::NearMiss) {
                    continue;
                }
                let (lo, hi) = if ea.refdes <= eb.refdes {
                    (ea.refdes.clone(), eb.refdes.clone())
                } else {
                    (eb.refdes.clone(), ea.refdes.clone())
                };
                out.push((lo, hi));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

// --- ratchet -------------------------------------------------------------

/// Every fixture, with its zero-slack Q5 high-water mark measured on
/// `master`. CLAUDE.md § "Budgets are ratchets, not knobs": these literals
/// only ever go **down**.
const Q5_NEAR_MISS_BUDGET: &[(&str, u32)] = &[
    // --- ADR-23 PROMOTION of `--placer=flow-seed` to the default
    // (owner-approved, 2026-08-18): re-recorded at the new default's
    // measured counts. Q5 is the ONE Tier-2 aggregate the promotion
    // loses: +3 points (two_stage_amp 7 -> 5 and rc_phase_shift 3 -> 2
    // against RISES on opamp_inverting_real 0 -> 3, cascode_amp and
    // common_emitter 3 -> 4, sallen_key_lpf 2 -> 3). Recorded, not
    // argued away; Q5 counts NEAR-misses, so a placer that packs
    // columns tighter naturally produces more of them.
    ("rc_lowpass", 0),
    ("rc_lowpass_ports", 0),
    // R1/R2 (bias divider) and Q1/RC/RE/CIN sit a cell or two off their
    // shared-net neighbours' axes — each near-miss is a candidate straight
    // drop the router currently jogs.
    // 4 -> 3: reclaimed by the ADR-19 M4 revert (`docs/layout-adr.md`,
    // "M4 reverted"). The literal was first measured on the M4 tree; the
    // pre-M4 Y datum this restores costs one fewer near-miss here. Ratchets
    // down only.
    // ADR-40 PROMOTION re-record: literal 3 -> 2. The literal was STALE
    // (the promotion never re-recorded it); against the pre-fix control
    // sink the measured count rose 0 -> 2 — the same origin-frame effect
    // the RISE arms below document, here still inside the mark.
    ("common_emitter", 2),
    // C1/RC1, C2/RC2 (cross-coupling caps vs collector loads) and the
    // Q/RC collector columns land just off-axis on this symmetric fixture.
    ("multivibrator", 4),
    // Q1/RC1, Q2/RC2 collector columns a hair off their shared axis.
    ("diff_pair", 2),
    ("opamp_inverting", 0),
    ("opamp_inverting_real", 1),
    ("port_shapes", 1),
    // RF1/X1, RF2/X2 feedback resistors sit just off their opamp's axis.
    ("opamp_definition_level", 2),
    // CL/RIN and RIN/RPU near-miss on the shared `out`/input nets.
    ("named_rails", 2),
    // F0 (v0.2 roadmap) NEW-GEOMETRY BASELINE, owner-approved:
    // C3/CIN, CIN/Q1 and Q1/RB each miss a shared axis by a hair.
    // *** RISE, Tier 2, AWAITING OWNER SIGN-OFF (ADR-40 § "Q5 and Q3
    // measure symbol origins"). *** Caused by the PIN-ANCHORED DC-series
    // column: a column puts each member's SHARED PIN on one x, and a
    // BJT's collector/emitter sit exactly 2.54 mm (= NEAR_CELLS, this
    // metric's whole snap threshold) off its origin — so a correctly
    // pin-collinear column is GUARANTEED to read as an origin near-miss.
    // Every new pair involves a transistor. Measured against it on the
    // same trees: v16.bends 141 -> 137, v16.branches 31 -> 26, v5 48 ->
    // 43, f6 79 -> 77, crossings 10 -> 9 — i.e. the routed-ink metric
    // Q5 calls itself a "leading indicator" of moved the OTHER way.
    // Q5 2 -> 5.
    ("rc_phase_shift", 5),
    // F0 (v0.2 roadmap) NEW-GEOMETRY BASELINE: seven near-misses
    // (CC/RB3, CE1/RE1, CIN/Q1, CIN/RB1, CIN/RB2, RB1/RB2, RB3/RB4) —
    // the worst in the suite, roughly double `rc_phase_shift`'s 3. Each
    // is a candidate straight drop the router currently jogs.
    // ADR-40 PROMOTION re-record: literal 8 -> 4 (stale literal); against
    // the pre-fix control sink the measured count rose 0 -> 4.
    ("two_stage_amp", 4),
    // --- F2 (v0.2 roadmap, second benchmark wave) NEW-GEOMETRY BASELINES.
    // Recorded at their measured values with zero slack; they ratchet
    // DOWN only. No existing fixture's literal moved.
    //
    // Q1/RB3, Q2/RB2 and RC/RB1 each miss a shared axis by a hair — the
    // bias chain is a column the placer does not keep in one column.
    // *** RISE, Tier 2, AWAITING OWNER SIGN-OFF (ADR-40 § "Q5 and Q3
    // measure symbol origins"). *** Caused by the PIN-ANCHORED DC-series
    // column: a column puts each member's SHARED PIN on one x, and a
    // BJT's collector/emitter sit exactly 2.54 mm (= NEAR_CELLS, this
    // metric's whole snap threshold) off its origin — so a correctly
    // pin-collinear column is GUARANTEED to read as an origin near-miss.
    // Every new pair involves a transistor. Measured against it on the
    // same trees: v16.bends 141 -> 137, v16.branches 31 -> 26, v5 48 ->
    // 43, f6 79 -> 77, crossings 10 -> 9 — i.e. the routed-ink metric
    // Q5 calls itself a "leading indicator" of moved the OTHER way.
    // Q5 4 -> 5.
    ("cascode_amp", 5),
    // Five near-misses. A doubly-terminated ladder is nothing BUT
    // shared axes, and the placer snaps none of them: worst in the suite
    // after `two_stage_amp`.
    // --- SECOND ADR-23 PROMOTION: `--placer=flow-seed-v4` becomes the
    // default (owner-authorised, 2026-08-24). Re-recorded at the NEW
    // DEFAULT's measured value, read from the scoreboard sink. Only the
    // two drawn-stimulus fixtures move; a whole-placer swap is the ONLY
    // sanctioned way one of these RISES, and it is not available to an
    // ordinary change.
    //
    // Q5 5 -> 1. "A doubly-terminated ladder is nothing BUT shared axes,
    // and the placer snaps none of them" — it snaps them now. Ratchet
    // DOWN.
    // `Recolumn` shunt-row fix: `C4`/`RL` take deterministic
    // pin-anchored slots on `out`, and the last near-miss snaps. Q5
    // 1 -> 0, measured from the sink. Ratchet DOWN.
    ("lc_ladder_lpf", 0),
    ("sallen_key_lpf", 2),
    ("wien_bridge_osc", 2),
    // --- F3 (Tier-0 router fix, ADR-24): the two fixtures promoted out of
    // `tests/f0_defects.rs` once the Steiner-vertex-on-foreign-pin defect was
    // fixed. NEW-GEOMETRY BASELINES, zero slack, ratchet DOWN only. Adding
    // them moved no existing fixture's literal.
    ("sallen_key_driven", 5),
    // RISE 2 -> 4, rail-stub SIDE fix (Tier 2, global-improvement escape,
    // AWAITING OWNER SIGN-OFF): moving RB above its node re-bases the CE
    // stage, leaving Q1 a near-miss against CE/COUT/RE/RF. Paid for four
    // Tier-1 xfail expiries (V14 rail-pin on this fixture and
    // `rc_phase_shift`, plus that fixture's V14 [3] and rail ordering).
    // *** RISE, Tier 2, AWAITING OWNER SIGN-OFF (ADR-40 § "Q5 and Q3
    // measure symbol origins"). *** Caused by the PIN-ANCHORED DC-series
    // column: a column puts each member's SHARED PIN on one x, and a
    // BJT's collector/emitter sit exactly 2.54 mm (= NEAR_CELLS, this
    // metric's whole snap threshold) off its origin — so a correctly
    // pin-collinear column is GUARANTEED to read as an origin near-miss.
    // Every new pair involves a transistor. Measured against it on the
    // same trees: v16.bends 141 -> 137, v16.branches 31 -> 26, v5 48 ->
    // 43, f6 79 -> 77, crossings 10 -> 9 — i.e. the routed-ink metric
    // Q5 calls itself a "leading indicator" of moved the OTHER way.
    // Q5 0 -> 4: all four pairs are Q1 against RB / RC / RE / RF.
    ("shunt_feedback_amp", 4),
    ("stepped_attenuator", 0),
    ("opamp_transimpedance", 2),
    // ADR-40 PROMOTION re-record: literal 1 -> 0. Ratchet DOWN; the
    // control sink also reads 0, so nothing moved here.
    ("resistor_ladder_ref", 0),
    ("compensated_divider", 0),
];

/// Every fixture, with its zero-slack **pin-frame** Q5 high-water mark
/// measured on the shipping default (`dc-series-column-pinned`).
///
/// A separate table from `Q5_NEAR_MISS_BUDGET` on purpose: `q5.pin` is a
/// different metric wearing a related name, so its literals are its own
/// and neither table's history transfers to the other. Zero slack,
/// ratchets DOWN only, same policy as every other budget in the suite.
const Q5_PIN_NEAR_MISS_BUDGET: &[(&str, u32)] = &[
    ("rc_lowpass", 0),
    ("rc_lowpass_ports", 0),
    ("common_emitter", 6),
    ("multivibrator", 6),
    ("diff_pair", 2),
    ("opamp_inverting", 0),
    ("opamp_inverting_real", 1),
    ("port_shapes", 0),
    ("opamp_definition_level", 2),
    ("named_rails", 2),
    ("rc_phase_shift", 2),
    ("two_stage_amp", 9),
    ("cascode_amp", 4),
    ("lc_ladder_lpf", 1),
    ("sallen_key_lpf", 0),
    ("wien_bridge_osc", 4),
    ("sallen_key_driven", 3),
    ("shunt_feedback_amp", 4),
    ("stepped_attenuator", 0),
    ("opamp_transimpedance", 2),
    ("resistor_ladder_ref", 0),
    ("compensated_divider", 2),
];

#[test]
fn alignment_near_miss_within_budget_across_fixtures() {
    let mut failures = Vec::new();
    for &(name, budget) in Q5_NEAR_MISS_BUDGET {
        let elems = align_elems(name).elems;
        let viol = q5_near_misses(&elems);
        let count = u32::try_from(viol.len()).unwrap_or(u32::MAX);
        common::scoreboard::record_count("q5", name, viol.len());
        if std::env::var("S2K_Q5_DUMP").is_ok() {
            println!("(\"{name}\", {count}),");
            for (a, b) in &viol {
                println!("    Q5 near-miss: {a} and {b} share an axis by a hair");
            }
        }
        if count > budget {
            failures.push(format!(
                "{name}: Q5 mutual-alignment near-misses rose to {count} (budget {budget}): \
                 {viol:?}. Do NOT raise the budget — diagnose the placement regression."
            ));
        } else if count < budget {
            // Lower-is-better: advertise the reclaimable slack so a fix
            // ratchets the literal down in the same commit.
            eprintln!("Q5 {name}: improved — you may lower the ratchet to (\"{name}\", {count})");
        }
    }
    assert!(
        failures.is_empty(),
        "Q5 mutual-alignment near-miss ratchet regressions:\n{}",
        failures.join("\n")
    );
}

#[test]
fn alignment_near_miss_pin_frame_within_budget_across_fixtures() {
    let mut failures = Vec::new();
    for &(name, budget) in Q5_PIN_NEAR_MISS_BUDGET {
        let fx = align_elems(name);
        // Coverage, not a formality: a join that silently resolved no
        // pins would report 0 near-misses on every fixture and read as a
        // perfect score (ADR-23 D9, "a blind cell is not conservatively
        // blind"). Both halves are collected, not asserted in-loop, so a
        // failure still reports every fixture's number to the sink.
        if fx.frame.pin_count() == 0 {
            failures.push(format!(
                "{name}: the pin frame resolved NO pins — the metric measured nothing"
            ));
        }
        if !fx.frame.unresolved.is_empty() {
            failures.push(format!(
                "{name}: {} (refdes, net) pair(s) have no resolvable pin: {:?}. \
                 Q5.pin would silently skip them.",
                fx.frame.unresolved.len(),
                fx.frame.unresolved
            ));
        }
        let viol = q5_pin_near_misses(&fx.elems, &fx.frame);
        let count = u32::try_from(viol.len()).unwrap_or(u32::MAX);
        common::scoreboard::record_count("q5.pin", name, viol.len());
        if std::env::var("S2K_Q5_DUMP").is_ok() {
            println!("(\"{name}\", {count}),");
            for (a, b) in &viol {
                let d = q5_pin_detail(&fx.elems, &fx.frame, a, b);
                println!("    Q5.pin near-miss: {a} and {b} {d:?}");
            }
        }
        if count > budget {
            failures.push(format!(
                "{name}: Q5.pin pin-frame near-misses rose to {count} (budget {budget}): \
                 {viol:?}. Do NOT raise the budget — diagnose the placement regression."
            ));
        } else if count < budget {
            eprintln!(
                "Q5.pin {name}: improved — you may lower the ratchet to (\"{name}\", {count})"
            );
        }
    }
    assert!(
        failures.is_empty(),
        "Q5.pin pin-frame near-miss ratchet regressions:\n{}",
        failures.join("\n")
    );
}

/// The mechanism, pinned to one fixture with the origin frame as its
/// control arm.
///
/// ADR-40's diagnosis in executable form: on `common_emitter` the
/// origin-frame metric flags `Q1↔RC` and `Q1↔RE` — the two members of
/// the pin-anchored DC-series column — while their SHARED PINS are
/// exactly collinear, so no wire between them jogs at all. A future
/// change that quietly re-based `q5.pin` onto bodies would pass every
/// ratchet above (the literals would simply be re-recorded) and fail
/// here.
///
/// The control arm is deliberately the live `q5_near_misses`, not a
/// copy: the claim under test is that the two frames DISAGREE on these
/// pairs, which is only meaningful against the origin metric the suite
/// actually ships.
#[test]
fn the_dc_series_column_is_an_origin_near_miss_and_a_pin_frame_match() {
    let fx = align_elems("common_emitter");
    let origin: HashSet<(String, String)> = q5_near_misses(&fx.elems).into_iter().collect();
    let pin: HashSet<(String, String)> = q5_pin_near_misses(&fx.elems, &fx.frame)
        .into_iter()
        .collect();

    for (a, b, net) in [("Q1", "RC", "c"), ("Q1", "RE", "e")] {
        let pair = (a.to_owned(), b.to_owned());
        assert!(
            origin.contains(&pair),
            "control arm broken: the origin frame no longer flags {a}\u{2194}{b}, so this \
             test can no longer show the two frames disagree"
        );
        assert!(
            !pin.contains(&pair),
            "{a}\u{2194}{b} is still a pin-frame near-miss — `q5.pin` has been re-based \
             onto body geometry"
        );
        let (pa, pb) = (
            fx.frame.pins(a, net).expect("pins on the shared net"),
            fx.frame.pins(b, net).expect("pins on the shared net"),
        );
        assert!(
            pa.iter().any(|&(ax, _)| pb.iter().any(|&(bx, _)| ax == bx)),
            "{a} and {b} are column members, so their `{net}` pins must share an x: \
             {pa:?} vs {pb:?}"
        );
    }
}
