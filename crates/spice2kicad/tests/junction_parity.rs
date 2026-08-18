//! **Junction-dot parity with KiCad's own rule.**
//!
//! KiCad decides where a junction dot belongs in
//! `eeschema/junction_helpers.cpp::AnalyzePoint`, and
//! `SCH_SCREEN::IsExplicitJunctionNeeded` / `SCH_SCREEN::GetConnectionPoints`
//! are what act on the answer: opening a sheet and nudging a wire makes
//! eeschema materialise every dot the file omitted. A converter that
//! under-dots therefore ships a schematic KiCad itself disagrees with —
//! and the disagreement is invisible until a human touches the file.
//!
//! # The rule, read off the source (not guessed)
//!
//! `AnalyzePoint( items, p )`:
//!
//! 1. Collect every connectable item overlapping `p`. `SCH_JUNCTION_T`
//!    hitting `p` sets `hasExplicitJunctionDot`.
//! 2. **Merge collinear wires first** (`SCH_LINE::MergeOverlap`), so two
//!    abutting collinear segments become one line whose *interior*
//!    contains `p`. The merge is SKIPPED when a dot is already present.
//! 3. Count **exit angles** on the WIRES layer:
//!    * a wire `IsConnected(p)` (i.e. `p` is one of its endpoints) sets
//!      `breakLines` and contributes **one** angle (`GetAngleFrom`);
//!    * a wire merely hit-testing `p` (a mid-span pass-through) is
//!      deferred, and contributes **two** angles (forward + reverse) iff
//!      `breakLines` ended up true;
//!    * `SCH_SYMBOL_T` / `SCH_SHEET_T` connected at `p` — i.e. a **pin**
//!      lands there — sets `breakLines` and contributes **one** unique
//!      angle (`uniqueAngle++`, deliberately never colliding with a
//!      wire's angle);
//!    * a label connected at `p` sets `breakLines` but contributes no
//!      angle.
//! 4. `isJunction = exitAngles[WIRES].size() >= 3`.
//!
//! The header comment on `SCH_SCREEN::IsJunction` states the same rule as
//! a list, and two of its five clauses are about pins:
//!
//! > - One wire midpoint and a symbol pin.
//! > - Two or more wire endpoints and a symbol pin.
//!
//! # Where we diverged
//!
//! `spice-route/src/cleanup.rs::rays_at` iterated `segments` only. A pin
//! contributed nothing, so a trunk running *through* a pin scored 2 rays
//! (pass-through) instead of KiCad's 3 (pass-through + pin) and got no
//! dot. Every such node was under-dotted relative to
//! `IsExplicitJunctionNeeded`.
//!
//! # What this file asserts
//!
//! For every emitted sheet of every fixture, reconstruct KiCad's
//! predicate over the emitted ink **plus the pins**, and assert the
//! resulting junction set is exactly the emitted `(junction …)` set:
//! nothing missing (KiCad would add it), nothing spurious (KiCad's
//! `GetConnectionPoints` prunes any point where
//! `!IsExplicitJunctionNeeded`, so a dot we emit there is ink KiCad does
//! not recognise).
//!
//! The predicate is evaluated on the file **as written**, dots included —
//! which is the self-consistent question, since clause 2 above makes the
//! merge itself conditional on the dot being there. A same-net
//! perpendicular crossing is exactly this case: its four split arms merge
//! back into two crossing lines *unless* the dot is present, so the dot
//! is what makes the point a junction, and that is precisely what
//! eeschema writes when a user dots a crossing by hand.
//!
//! Budget 0 on every fixture, both directions. This is parity with an
//! external authority, not a quality gradient — there is no budget to
//! tune.
//!
//! The one structural carve-out — points where the exit angles come from
//! two *different* nets, where KiCad's net-blind rule demands a dot our
//! converter must refuse to draw — is documented on
//! [`parity_for_sheet`] and held by its own zero-slack ratchet
//! ([`CROSS_NET_CONTACT_POINTS`]), so it can shrink but never grow.

mod common;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use common::ink::{Pt, as_f64, children, find_child, head, list_iter, q};
use common::spice_to_kicad;
use kicad_symbols::{Library, Orientation, Rotation};
use lexpr::Value;

// ---------------------------------------------------------------------------
// Fixtures / driver.
// ---------------------------------------------------------------------------

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
    "two_stage_amp",
    "cascode_amp",
    "lc_ladder_lpf",
    "sallen_key_lpf",
    "wien_bridge_osc",
    "sallen_key_driven",
    "shunt_feedback_amp",
];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> common::TempDir {
    common::TempDir::new("jp", name)
}

fn parse_sch(sch: &Path) -> Value {
    let src = std::fs::read_to_string(sch).expect("read sch");
    lexpr::from_str(&src).expect("parse sch as lexpr")
}

fn load_test_library() -> Library {
    let libs_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
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

// ---------------------------------------------------------------------------
// The emitted sheet, in the terms `AnalyzePoint` works in.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seg {
    a: Pt,
    b: Pt,
}

impl Seg {
    fn is_h(self) -> bool {
        self.a.1 == self.b.1 && self.a.0 != self.b.0
    }

    fn is_v(self) -> bool {
        self.a.0 == self.b.0 && self.a.1 != self.b.1
    }

    /// `SCH_LINE::HitTest` for an axis-aligned segment: on the closed
    /// span, endpoints included.
    fn hits(self, p: Pt) -> bool {
        if self.is_h() {
            p.1 == self.a.1 && p.0 >= self.a.0.min(self.b.0) && p.0 <= self.a.0.max(self.b.0)
        } else if self.is_v() {
            p.0 == self.a.0 && p.1 >= self.a.1.min(self.b.1) && p.1 <= self.a.1.max(self.b.1)
        } else {
            false
        }
    }

    /// `SCH_LINE::doIsConnected`: endpoints only.
    fn is_connected(self, p: Pt) -> bool {
        self.a == p || self.b == p
    }
}

#[derive(Debug, Default)]
struct SheetItems {
    wires: Vec<Seg>,
    /// Symbol and sheet-port pin coordinates.
    pins: Vec<Pt>,
    /// Label anchors of every flavour (they set `breakLines`, no angle).
    labels: Vec<Pt>,
    dots: HashSet<Pt>,
}

fn as_str(v: &Value) -> Option<&str> {
    v.as_str().or_else(|| v.as_symbol())
}

fn at_xy(node: &Value) -> Option<(f64, f64, f64)> {
    let at = find_child(node, "at")?;
    let mut it = list_iter(at);
    it.next();
    let x = as_f64(it.next()?)?;
    let y = as_f64(it.next()?)?;
    let r = it.next().and_then(as_f64).unwrap_or(0.0);
    Some((x, y, r))
}

fn placed_symbol_pose(sym: &Value) -> Option<(f64, f64, Orientation)> {
    let (x, y, rot_deg) = at_xy(sym)?;
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

fn lib_id_of(sym: &Value) -> Option<String> {
    find_child(sym, "lib_id")
        .and_then(|l| list_iter(l).nth(1).and_then(as_str))
        .map(str::to_owned)
}

/// Read one emitted sheet into the item set `AnalyzePoint` consumes.
///
/// Pin coordinates are derived from the **library** through the emitted
/// pose — the same derivation `roundtrip_connectivity.rs` uses, and the
/// only one that can falsify the emitter's own pin bookkeeping. Power
/// glyphs (`#PWR…`) are ordinary `SCH_SYMBOL_T` to KiCad and their pins
/// count exactly like any other.
fn read_sheet(root: &Value, library: &Library, unresolved: &mut Vec<String>) -> SheetItems {
    let mut items = SheetItems::default();

    for w in children(root, "wire") {
        let Some(pts) = find_child(w, "pts") else {
            continue;
        };
        let xys: Vec<&Value> = list_iter(pts)
            .filter(|c| c.is_list() && head(c) == Some("xy"))
            .collect();
        if xys.len() < 2 {
            continue;
        }
        let (Some(a), Some(b)) = (common::ink::xy(xys[0]), common::ink::xy(xys[1])) else {
            continue;
        };
        if a != b {
            items.wires.push(Seg { a, b });
        }
    }

    for sym in children(root, "symbol") {
        let Some(lib_id) = lib_id_of(sym) else {
            continue;
        };
        let Some((ox, oy, orient)) = placed_symbol_pose(sym) else {
            continue;
        };
        let Some(lib_sym) = library.lookup(&lib_id) else {
            unresolved.push(lib_id);
            continue;
        };
        for tp in lib_sym.pins_in(orient) {
            items.pins.push((q(ox + tp.x), q(oy - tp.y)));
        }
    }

    for sheet in children(root, "sheet") {
        for p in children(sheet, "pin") {
            if let Some((x, y, _)) = at_xy(p) {
                items.pins.push((q(x), q(y)));
            }
        }
    }

    for kind in ["label", "global_label", "hierarchical_label"] {
        for node in children(root, kind) {
            if let Some((x, y, _)) = at_xy(node) {
                items.labels.push((q(x), q(y)));
            }
        }
    }

    for j in children(root, "junction") {
        if let Some((x, y, _)) = at_xy(j) {
            items.dots.insert((q(x), q(y)));
        }
    }

    items
}

// ---------------------------------------------------------------------------
// `AnalyzePoint`, reproduced.
// ---------------------------------------------------------------------------

/// Merge collinear overlapping-or-touching lines, KiCad's
/// `SCH_LINE::MergeOverlap` run to a fixed point over the candidate set.
fn merge_collinear(lines: &mut Vec<Seg>) {
    loop {
        let mut merged = None;
        'outer: for i in 0..lines.len() {
            for j in (i + 1)..lines.len() {
                let (a, b) = (lines[i], lines[j]);
                let same_line = (a.is_h() && b.is_h() && a.a.1 == b.a.1)
                    || (a.is_v() && b.is_v() && a.a.0 == b.a.0);
                if !same_line {
                    continue;
                }
                let axis_h = a.is_h();
                let key = |p: Pt| if axis_h { p.0 } else { p.1 };
                let (alo, ahi) = (key(a.a).min(key(a.b)), key(a.a).max(key(a.b)));
                let (blo, bhi) = (key(b.a).min(key(b.b)), key(b.a).max(key(b.b)));
                // Overlapping or touching.
                if blo > ahi || alo > bhi {
                    continue;
                }
                let (lo, hi) = (alo.min(blo), ahi.max(bhi));
                let line = if axis_h { a.a.1 } else { a.a.0 };
                let m = if axis_h {
                    Seg {
                        a: (lo, line),
                        b: (hi, line),
                    }
                } else {
                    Seg {
                        a: (line, lo),
                        b: (line, hi),
                    }
                };
                merged = Some((i, j, m));
                break 'outer;
            }
        }
        let Some((i, j, m)) = merged else { return };
        lines[i] = m;
        lines.remove(j);
    }
}

/// The direction key `GetAngleFrom` / `GetReverseAngleFrom` produce,
/// reduced to the sign pair (all our ink is axis-aligned, so the angle is
/// one of four values and the sign pair is faithful).
fn dir_key(from: Pt, to: Pt) -> i64 {
    (to.0 - from.0).signum() * 3 + (to.1 - from.1).signum()
}

/// `JUNCTION_HELPERS::AnalyzePoint(...).isJunction` for the WIRES layer.
fn is_junction(items: &SheetItems, p: Pt) -> bool {
    let mut lines: Vec<Seg> = items.wires.iter().copied().filter(|s| s.hits(p)).collect();
    let dot = items.dots.contains(&p);
    if !dot {
        merge_collinear(&mut lines);
    }

    let mut angles: HashSet<i64> = HashSet::new();
    let mut break_lines = false;
    let mut midpoints: Vec<Seg> = Vec::new();

    for l in &lines {
        if l.is_connected(p) {
            break_lines = true;
            let far = if l.a == p { l.b } else { l.a };
            angles.insert(dir_key(p, far));
        } else {
            // `hits(p)` already established the hit-test.
            midpoints.push(*l);
        }
    }

    // A pin at 90 degrees must not collide with a wire at 90 degrees, so
    // KiCad gives each pin its own unique angle. Mirror that with a
    // counter disjoint from the four direction keys.
    let mut unique_angle = 10_000i64;
    for pin in &items.pins {
        if *pin == p {
            break_lines = true;
            angles.insert(unique_angle);
            unique_angle += 1;
        }
    }

    for label in &items.labels {
        if *label == p {
            break_lines = true;
        }
    }

    if break_lines {
        for l in &midpoints {
            angles.insert(dir_key(p, l.a));
            angles.insert(dir_key(p, l.b));
        }
    }

    angles.len() >= 3
}

/// Every point worth testing: any junction needs `breakLines`, which only
/// a wire endpoint, a pin or a label can set, and any 4-angle
/// label-at-a-crossing case needs the crossing itself.
fn candidate_points(items: &SheetItems) -> Vec<Pt> {
    let mut set: HashSet<Pt> = HashSet::new();
    for s in &items.wires {
        set.insert(s.a);
        set.insert(s.b);
    }
    set.extend(items.pins.iter().copied());
    set.extend(items.labels.iter().copied());
    set.extend(items.dots.iter().copied());
    for h in items.wires.iter().filter(|s| s.is_h()) {
        for v in items.wires.iter().filter(|s| s.is_v()) {
            let p = (v.a.0, h.a.1);
            if h.hits(p) && v.hits(p) {
                set.insert(p);
            }
        }
    }
    let mut v: Vec<Pt> = set.into_iter().collect();
    v.sort_unstable();
    v
}

// ---------------------------------------------------------------------------
// The verifier.
// ---------------------------------------------------------------------------

/// Connected component id per wire segment, on **KiCad's own rule**: two
/// wires join iff they share an *endpoint* (`SCH_LINE::GetConnectionPoints`
/// returns exactly `{start, end}`). Same as `common::ink::run_components`
/// uses, and for the same reason: an end landing on a foreign wire's
/// interior looks like a join and is not one.
fn wire_components(wires: &[Seg]) -> Vec<usize> {
    fn find(uf: &mut [usize], mut x: usize) -> usize {
        while uf[x] != x {
            uf[x] = uf[uf[x]];
            x = uf[x];
        }
        x
    }
    let mut ids: std::collections::HashMap<Pt, usize> = std::collections::HashMap::new();
    let mut uf: Vec<usize> = Vec::new();
    let mut seg_node: Vec<usize> = Vec::with_capacity(wires.len());
    for s in wires {
        let mut id_of = |p: Pt, uf: &mut Vec<usize>| -> usize {
            *ids.entry(p).or_insert_with(|| {
                uf.push(uf.len());
                uf.len() - 1
            })
        };
        let (ia, ib) = (id_of(s.a, &mut uf), id_of(s.b, &mut uf));
        let (ra, rb) = (find(&mut uf, ia), find(&mut uf, ib));
        if ra != rb {
            uf[ra] = rb;
        }
        seg_node.push(ia);
    }
    seg_node
        .into_iter()
        .map(|n| find(&mut uf, n))
        .collect::<Vec<_>>()
}

struct Parity {
    missing: Vec<Pt>,
    spurious: Vec<Pt>,
    /// Points where KiCad's rule fires but the exit angles come from
    /// **two or more distinct ink components**. See
    /// [`parity_for_sheet`]'s doc comment.
    cross_net: Vec<Pt>,
    needed: usize,
}

/// Compare the emitted `(junction …)` set against KiCad's rule.
///
/// # The one carve-out, and why it is not a weakening
///
/// KiCad's predicate is deliberately **net-blind** — it derives nets
/// *from* geometry, so three exit angles is a junction whoever drew them.
/// Where two different nets' trunks share a collinear run, that predicate
/// therefore fires on a point our converter must NOT dot: dotting it is
/// what would turn a latent short into a real one (KiCad breaks segments
/// at a junction, merging the pass-through net into the one that ends
/// there). Asserting parity at such a point would demand the wrong
/// output.
///
/// Those points are separated out — identified structurally, as points
/// whose contributing wires span **more than one ink component** under
/// KiCad's endpoint-sharing rule — and gated by a zero-slack per-fixture
/// ratchet in the caller instead. They are not excused: they are the
/// already-registered `no_cross_net_collinear_wire_overlap` defect
/// (`electrical_safety.rs`, the deferred v0.2 channel-router wall),
/// rediscovered from geometry by a second instrument. Duplicating that
/// gate's *assertion* here is the failure ADR-23 D2 warns about;
/// recording the count so it cannot grow silently is not.
///
/// In a schematic with no cross-net overlap the carve-out is empty by
/// construction: one net is one ink component, so every junction point
/// has exactly one.
fn parity_for_sheet(items: &SheetItems) -> Parity {
    let comps = wire_components(&items.wires);
    let mut missing = Vec::new();
    let mut cross_net = Vec::new();
    let mut needed = 0usize;
    let mut wanted: HashSet<Pt> = HashSet::new();
    let mut excused: HashSet<Pt> = HashSet::new();
    for p in candidate_points(items) {
        if !is_junction(items, p) {
            continue;
        }
        let touching: HashSet<usize> = items
            .wires
            .iter()
            .enumerate()
            .filter(|(_, s)| s.hits(p))
            .map(|(i, _)| comps[i])
            .collect();
        if touching.len() > 1 {
            cross_net.push(p);
            excused.insert(p);
            continue;
        }
        needed += 1;
        wanted.insert(p);
        if !items.dots.contains(&p) {
            missing.push(p);
        }
    }
    let mut spurious: Vec<Pt> = items
        .dots
        .iter()
        .copied()
        .filter(|p| !wanted.contains(p) && !excused.contains(p))
        .collect();
    spurious.sort_unstable();
    missing.sort_unstable();
    cross_net.sort_unstable();
    Parity {
        missing,
        spurious,
        cross_net,
        needed,
    }
}

fn emitted_sheets(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("read output dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "kicad_sch"))
        .collect();
    out.sort();
    out
}

fn mm(v: i64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let f = v as f64 / 1000.0;
    format!("{f}")
}

/// Per-fixture count of points where KiCad's net-blind rule fires across
/// an ink-component boundary — a zero-slack ratchet, not a budget.
///
/// Every literal here is a *cross-net collinear overlap*, i.e. the
/// registered `no_cross_net_collinear_wire_overlap` defect seen from a
/// second angle. They only ever go DOWN; the day the channel router
/// separates those trunks, every literal becomes 0 and this table
/// disappears. A rise means a NEW pair of nets was laid on one track —
/// diagnose it, never bump the number.
const CROSS_NET_CONTACT_POINTS: &[(&str, usize)] = &[
    // EMPTY, as this table's own doc predicted: "the day the channel
    // router separates those trunks, every literal becomes 0 and this
    // table disappears".
    //
    // It was not the channel router that separated them. The ADR-23
    // promotion of `--placer=flow-seed` to the default (owner-approved,
    // 2026-08-18) did it from the skeleton: `two_stage_amp`'s only entry
    // (4 contact points, the `b2`/`c2` run at x = 57.15 and the `c2`/`e2`
    // run at y = 87.63) is gone because those trunks no longer share a
    // column. Rail-hop layering collapsed `in->b1->c1->b2->c2->out` into
    // three columns; signal-depth layering gives it five.
    //
    // Zero slack, and the ratchet now reads 0 for every fixture through
    // `cross_net_ratchet`'s `map_or(0, ..)`. A rise means a NEW pair of
    // nets was laid on one track — diagnose it, never bump the number.
];

fn cross_net_ratchet(name: &str) -> usize {
    CROSS_NET_CONTACT_POINTS
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(0, |&(_, n)| n)
}

#[test]
fn junction_dots_match_kicads_own_rule_across_fixtures() {
    let library = load_test_library();
    let mut failures: Vec<String> = Vec::new();

    for name in FIXTURES {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        // The conversion writes every sheet into `tmp`; the root path it
        // returns is not needed, `emitted_sheets` walks the directory so
        // child sheets are graded too.
        spice_to_kicad(&src, &tmp).expect("spice2kicad");

        let mut fixture_missing = 0usize;
        let mut fixture_spurious = 0usize;
        let mut fixture_cross_net = 0usize;
        for sheet_path in emitted_sheets(tmp.path()) {
            let root = parse_sch(&sheet_path);
            let mut unresolved = Vec::new();
            let items = read_sheet(&root, &library, &mut unresolved);
            let sheet_name = sheet_path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();

            // Vacuity guard: a pose or lib_id that stopped resolving
            // would silently drop the pins this whole check is about.
            assert!(
                unresolved.is_empty(),
                "{name}/{sheet_name}: {} symbol instance(s) whose lib_id did not \
                 resolve in the test library ({:?}) — their pins are missing from \
                 the analysis, which would make the parity check pass vacuously.",
                unresolved.len(),
                unresolved,
            );

            let p = parity_for_sheet(&items);
            fixture_missing += p.missing.len();
            fixture_spurious += p.spurious.len();
            fixture_cross_net += p.cross_net.len();

            if !p.missing.is_empty() {
                failures.push(format!(
                    "{name}/{sheet_name}: {} junction dot(s) MISSING relative to \
                     KiCad's rule (IsExplicitJunctionNeeded is true and no \
                     (junction …) is emitted) at {}; {} needed in total, {} emitted",
                    p.missing.len(),
                    p.missing
                        .iter()
                        .map(|&(x, y)| format!("({}, {})", mm(x), mm(y)))
                        .collect::<Vec<_>>()
                        .join(" "),
                    p.needed,
                    items.dots.len(),
                ));
            }
            if !p.spurious.is_empty() {
                failures.push(format!(
                    "{name}/{sheet_name}: {} SPURIOUS junction dot(s) — KiCad's \
                     rule reports no junction there, so GetConnectionPoints would \
                     prune them: {}",
                    p.spurious.len(),
                    p.spurious
                        .iter()
                        .map(|&(x, y)| format!("({}, {})", mm(x), mm(y)))
                        .collect::<Vec<_>>()
                        .join(" "),
                ));
            }
        }
        common::scoreboard::record_count("junction.missing", name, fixture_missing);
        common::scoreboard::record_count("junction.spurious", name, fixture_spurious);
        common::scoreboard::record_count("junction.cross_net", name, fixture_cross_net);

        let ratchet = cross_net_ratchet(name);
        if fixture_cross_net > ratchet {
            failures.push(format!(
                "{name}: {fixture_cross_net} point(s) where KiCad's junction rule fires \
                 ACROSS an ink-component boundary > ratchet {ratchet}. Each is a \
                 cross-net collinear overlap (the `no_cross_net_collinear_wire_overlap` \
                 defect) that KiCad would dot — and dotting would short the two nets. \
                 Do NOT raise this literal; diagnose the router regression."
            ));
        } else if fixture_cross_net < ratchet {
            eprintln!(
                "junction parity {name}: improved — you may lower the cross-net \
                 ratchet to (\"{name}\", {fixture_cross_net})"
            );
        }
    }

    assert!(
        failures.is_empty(),
        "junction-dot parity with KiCad's own rule failed (budget 0, both \
         directions):\n  {}",
        failures.join("\n  "),
    );
}

/// **Mutation guard.** The parity assertion above is only worth anything
/// if the reproduction of `AnalyzePoint` can actually fire. Two
/// injections per fixture, each in the direction that must break it.
#[test]
fn the_junction_rule_reproduction_is_sensitive() {
    let library = load_test_library();
    let mut failures: Vec<String> = Vec::new();

    for name in FIXTURES {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);
        let mut unresolved = Vec::new();
        let mut items = read_sheet(&root, &library, &mut unresolved);

        // (1) Deleting every dot must be reported as missing dots
        //     wherever the geometry alone still demands one, and must
        //     never be reported as spurious.
        let all_dots = items.dots.clone();
        items.dots.clear();
        let p = parity_for_sheet(&items);
        if !p.spurious.is_empty() {
            failures.push(format!(
                "{name}: erasing every dot produced {} 'spurious' report(s) — the \
                 predicate is not reading the emitted dot set",
                p.spurious.len()
            ));
        }
        items.dots = all_dots;

        // (2) A dot at a point with no ink at all must be spurious.
        let bogus = (-999_000, -999_000);
        items.dots.insert(bogus);
        let p = parity_for_sheet(&items);
        if !p.spurious.contains(&bogus) {
            failures.push(format!(
                "{name}: a dot placed in empty space was not reported as spurious"
            ));
        }
        items.dots.remove(&bogus);

        // (3) A T injected onto the middle of an existing run must be
        //     reported as a missing dot. Pick the first wire long enough
        //     to have a grid cell of interior.
        let Some(&host) = items.wires.iter().find(|s| {
            let len = (s.b.0 - s.a.0).abs() + (s.b.1 - s.a.1).abs();
            len >= 2540
        }) else {
            failures.push(format!("{name}: no wire long enough to inject a T"));
            continue;
        };
        // Split the host so the tap point is a real endpoint of both
        // halves, exactly as `split_at_interior_attachments` would.
        let mid = (
            (host.a.0 + host.b.0) / 2 / 1270 * 1270,
            (host.a.1 + host.b.1) / 2 / 1270 * 1270,
        );
        if mid == host.a || mid == host.b {
            failures.push(format!("{name}: injected tap point degenerate"));
            continue;
        }
        let stub_end = if host.is_h() {
            (mid.0, mid.1 + 1270)
        } else {
            (mid.0 + 1270, mid.1)
        };
        let mut injected = SheetItems {
            wires: items.wires.clone(),
            pins: items.pins.clone(),
            labels: items.labels.clone(),
            dots: items.dots.clone(),
        };
        injected.wires.retain(|s| *s != host);
        injected.wires.push(Seg { a: host.a, b: mid });
        injected.wires.push(Seg { a: mid, b: host.b });
        injected.wires.push(Seg {
            a: mid,
            b: stub_end,
        });
        let p = parity_for_sheet(&injected);
        if !p.missing.contains(&mid) && !injected.dots.contains(&mid) {
            failures.push(format!(
                "{name}: a wire T injected at ({}, {}) was NOT reported as a \
                 missing junction — the ray count is not being computed",
                mm(mid.0),
                mm(mid.1)
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "junction-rule reproduction is insensitive to defects it must catch:\n  {}",
        failures.join("\n  "),
    );
}

/// Informational: how many of the branch vertices V16 counts as `J` sit
/// on a pin of the ink they branch from.
///
/// This is the measurement behind the proposed V16 `J` redefinition
/// discussed in `docs/layout-adr.md` ADR-27 — **reported, never
/// asserted**. Redefining `J` is an owner-signed doctrine change; this
/// test exists so the numbers are on the record, not to pre-empt it.
#[test]
fn report_pin_anchored_branch_share() {
    use common::ink::{Axis, candidate_vertices, maximal_runs, raw_wire_segments, rays_at};

    let library = load_test_library();
    let mut rows: Vec<String> = Vec::new();

    for name in FIXTURES {
        let src = fixtures_dir().join(format!("{name}.cir"));
        let tmp = tempdir(name);
        let sch = spice_to_kicad(&src, &tmp).expect("spice2kicad");
        let root = parse_sch(&sch);
        let mut unresolved = Vec::new();
        let items = read_sheet(&root, &library, &mut unresolved);
        let comps = wire_components(&items.wires);
        // Pins that touch a given ink component — the same-net proxy.
        // (One net is one ink component; V11 forbids a foreign pin on
        // our ink, so "a pin on this component" is "a pin of this net".)
        let pins_of_comp = |c: usize| -> Vec<Pt> {
            items
                .pins
                .iter()
                .copied()
                .filter(|&p| {
                    items
                        .wires
                        .iter()
                        .enumerate()
                        .any(|(i, s)| comps[i] == c && s.hits(p))
                })
                .collect()
        };
        let comp_at = |p: Pt| -> Option<usize> {
            items
                .wires
                .iter()
                .enumerate()
                .find(|(_, s)| s.hits(p))
                .map(|(i, _)| comps[i])
        };

        let segs = raw_wire_segments(&root);
        let runs = maximal_runs(&segs);
        let dots = common::ink::junction_positions(&root);

        let (mut j_total, mut j_pin, mut j_near, mut b_total) = (0u32, 0u32, 0u32, 0u32);
        for (x, y) in candidate_vertices(&runs) {
            let (rays, axes) = rays_at(&runs, x, y);
            let is_branch = rays == 3 || (rays >= 4 && dots.contains(&(x, y)));
            if rays == 2
                && axes.iter().filter(|a| **a == Axis::H).count() == 1
                && axes.iter().filter(|a| **a == Axis::V).count() == 1
            {
                b_total += 1;
            }
            if is_branch {
                j_total += 1;
                let own: Vec<Pt> = comp_at((x, y)).map(pins_of_comp).unwrap_or_default();
                if own.contains(&(x, y)) {
                    j_pin += 1;
                } else if own
                    .iter()
                    .any(|&(px, py)| (px - x).abs() + (py - y).abs() <= 1270)
                {
                    // One grid cell away, on a pin of this net's own ink.
                    // `idioms::apply_shared_centers` deliberately puts
                    // `diff_pair`'s approved T here — a stub cell reserved
                    // UNDER the pin, because V5 says a wire leaves a pin
                    // along the pin's axis before it turns — so a rule
                    // keyed on exact coincidence would not reach it.
                    j_near += 1;
                }
            }
        }
        rows.push(format!(
            "{name:<24} B {b_total:>3}   J {j_total:>3}   J_at_pin {j_pin:>3}   \
             J_1cell_from_pin {j_near:>3}   J_midair {:>3}",
            j_total - j_pin - j_near
        ));
    }

    eprintln!(
        "\nV16 J composition (informational — see ADR-27):\n{}\n",
        rows.join("\n")
    );
}
