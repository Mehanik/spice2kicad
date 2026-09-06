//! **Placer-agnostic structural layout checks**, in one place so that
//! the default-placer suites and the cross-arm sweep assert the same
//! bytes.
//!
//! # Why this module exists (ADR-40, "Challenger blindness")
//!
//! Every `spice-layout` integration test used to build its
//! `LayoutOptions` with `..LayoutOptions::default()`, so it only ever
//! exercised [`Placer::default`]. A registered `--placer` challenger was
//! therefore graded end-to-end on the scoreboard — 22 fixtures, ~60
//! verifiers, a k = 9 multi-seed replay — while no test in the tree ever
//! ran it against a *structural* placement property. That is how
//! `dc-series-column-pinned` shipped a column anchored on the barycenter
//! of its members' **origins** instead of their **shared pins**,
//! violating CLAUDE.md § "Layout invariants" ("constraints are
//! pin-anchored"), and was caught only by hand at promotion time.
//!
//! # What belongs here, and what does not
//!
//! A check belongs here when it states a **structural** property in
//! CLAUDE.md's constraints-vs-costs sense: a yes/no geometric fact with
//! one correct answer, true of *any* placement engine. "A ground-
//! returning stub is below the device it serves", "a column's members
//! share their shared pin's X", "an `align vertical` group shares a
//! column", "every origin is on the grid".
//!
//! A check does **not** belong here when it is a continuous quality
//! gradient (wire length, crossings, bend counts) — those are the
//! scoreboard's job, in aggregate, and asserting them per-fixture across
//! arms would re-measure the scoreboard in a frame ADR-23 established is
//! satisfiable essentially only by a no-op.
//!
//! Every check is a `fn(Placer) -> Result<(), String>` so a sweep can
//! collect all of them; the failure message always names the geometry
//! that was measured, and the sweep prepends the arm.

use std::path::PathBuf;
use std::sync::OnceLock;

use kicad_symbols::{Library, Orientation};
use spice_diagnostics::FileId;
use spice_layout::net_class::{VertPref, vertical_prefs};
use spice_layout::orient::allowed_orientations;
use spice_layout::{LayoutOptions, PlacedElement, Placement, Placer, place_with};
use spice_policy::{CheckedNetlist, check};
use spice_resolve::{Axis, Relation};

/// One grid step (50 mil) in millimetres.
pub const STEP_MM: f64 = 1.27;

/// Two grid-snapped pins are "the same column" when their X differ by
/// less than half a grid step — i.e. they are on the identical grid
/// line. (Half-grid, not zero, only to absorb float round-trip noise;
/// an origin-anchored-instead-of-pin-anchored mistake is a full 2 grid
/// steps off here and still fails.)
pub const SAME_COLUMN_EPS_MM: f64 = STEP_MM / 2.0;

const TOL_MM: f64 = 1e-9;

// ---------------------------------------------------------------------------
// Fixture plumbing
// ---------------------------------------------------------------------------

/// The fixture library the structural checks resolve against: `Device`
/// plus the three libraries the amplifier fixtures need.
pub fn library() -> &'static Library {
    static LIB: OnceLock<Library> = OnceLock::new();
    LIB.get_or_init(|| {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = manifest
            .parent()
            .and_then(std::path::Path::parent)
            .expect("workspace root")
            .join("crates/kicad-symbols/tests/fixtures");
        let mut lib =
            Library::from_file(dir.join("Device.kicad_sym")).expect("load Device fixture library");
        for f in [
            "power.kicad_sym",
            "Simulation_SPICE.kicad_sym",
            "Amplifier_Operational.kicad_sym",
        ] {
            lib = lib.merge(
                Library::from_file(dir.join(f))
                    .unwrap_or_else(|e| panic!("load fixture library {f}: {e:?}")),
            );
        }
        lib
    })
}

fn checked_from_src(src: &str) -> CheckedNetlist {
    let parsed = spice_parser::parse(src, FileId(0))
        .expect("parse failed")
        .netlist;
    let resolved = spice_resolve::resolve(&parsed, library()).expect("resolve failed");
    check(resolved).expect("policy check failed").0
}

fn opts(placer: Placer, refine: bool) -> LayoutOptions {
    LayoutOptions {
        refine,
        placer,
        ..LayoutOptions::default()
    }
}

/// Memoised placements, keyed by `(source key, placer, refine)`.
///
/// Several checks read different properties of the *same* placement —
/// four of them measure `common_emitter` alone — and a placement is a
/// pure function of that key. Without the memo the cross-arm sweep runs
/// ~20 placements per arm instead of ~8, which is most of its wall
/// clock. `Placement` is `Clone`, so callers still get an owned value
/// and cannot mutate the cached one.
type PlacementCache = std::collections::HashMap<(String, Placer, bool), Placement>;

static PLACEMENTS: std::sync::Mutex<Option<PlacementCache>> = std::sync::Mutex::new(None);

fn memoised(
    key: &str,
    placer: Placer,
    refine: bool,
    build: impl FnOnce() -> Placement,
) -> Placement {
    let k = (key.to_string(), placer, refine);
    if let Some(hit) = PLACEMENTS
        .lock()
        .expect("placement cache")
        .get_or_insert_with(PlacementCache::new)
        .get(&k)
    {
        return hit.clone();
    }
    // Built outside the lock: a placement takes milliseconds and the
    // sweep runs arms concurrently, so holding the mutex across it would
    // serialise the whole file.
    let fresh = build();
    PLACEMENTS
        .lock()
        .expect("placement cache")
        .get_or_insert_with(PlacementCache::new)
        .insert(k, fresh.clone());
    fresh
}

/// Place one of `spice2kicad`'s `.cir` fixtures under `placer`.
pub fn place_fixture(name: &str, placer: Placer, refine: bool) -> Placement {
    memoised(name, placer, refine, || {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let path = manifest.join("../spice2kicad/tests/fixtures").join(name);
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
        place_with(checked_from_src(&src), library(), &opts(placer, refine)).expect("placement")
    })
}

fn place_src(key: &str, src: &str, placer: Placer, refine: bool) -> Placement {
    memoised(key, placer, refine, || {
        place_with(checked_from_src(src), library(), &opts(placer, refine)).expect("placement")
    })
}

pub fn elem<'a>(p: &'a Placement, refdes: &str) -> &'a PlacedElement {
    p.elements
        .iter()
        .find(|e| e.refdes == refdes)
        .unwrap_or_else(|| panic!("no such refdes {refdes}"))
}

/// World `(x, y)` mm of the pin of `refdes` that sits on SPICE `node`.
///
/// Pin-anchored on purpose: it resolves the KiCad pin number through
/// `pin_mapping` and reads the orientation-transformed pin set, so an
/// assertion written with it cannot be satisfied by aligning *bodies*.
pub fn pin_xy(p: &Placement, refdes: &str, node: &str) -> (f64, f64) {
    let e = elem(p, refdes);
    let ti = e.nodes.iter().position(|n| n == node).unwrap_or_else(|| {
        panic!(
            "{refdes} has no terminal on net {node}, nodes={:?}",
            e.nodes
        )
    });
    let want = &e.pin_mapping[ti];
    let sym = library()
        .lookup(&e.lib_id)
        .unwrap_or_else(|| panic!("no symbol for {}", e.lib_id));
    e.world_pin_mm(sym)
        .into_iter()
        .find(|(num, _, _)| num == want)
        .map_or_else(|| panic!("{refdes} has no pin #{want}"), |(_, x, y)| (x, y))
}

/// Screen Y grows downward, so "A is above B" is `A.y < B.y`. A full
/// grid step of separation is required, not merely a tie-break, so an
/// accidental near-coincidence cannot pass.
fn check_above(
    p: &Placement,
    upper: (&str, &str),
    lower: (&str, &str),
    why: &str,
) -> Result<(), String> {
    let (_, uy) = pin_xy(p, upper.0, upper.1);
    let (_, ly) = pin_xy(p, lower.0, lower.1);
    if uy < ly - STEP_MM {
        return Ok(());
    }
    Err(format!(
        "{why}: expected {}.{} ABOVE {}.{} (smaller screen Y), got y={uy:.2} vs y={ly:.2}",
        upper.0, upper.1, lower.0, lower.1
    ))
}

// ---------------------------------------------------------------------------
// Group A — the rail-direction drawing convention (`rail_convention.rs`)
// ---------------------------------------------------------------------------

/// `R2` connects the base to ground, so it belongs UNDER `Q1`, not above
/// it. Measured on the base net `b`, which both share.
pub fn ce_r2_below_q1(placer: Placer, refine: bool) -> Result<(), String> {
    let p = place_fixture("common_emitter.cir", placer, refine);
    check_above(
        &p,
        ("Q1", "b"),
        ("R2", "b"),
        "a ground-returning stub belongs below the device it serves",
    )
}

/// `RE` and `CE` both return the emitter to ground, so both belong
/// UNDER `Q1`.
pub fn ce_emitter_loads_below_q1(placer: Placer, refine: bool) -> Result<(), String> {
    let p = place_fixture("common_emitter.cir", placer, refine);
    for r in ["RE", "CE"] {
        check_above(
            &p,
            ("Q1", "e"),
            (r, "e"),
            "an emitter-to-ground stub belongs below the transistor",
        )?;
    }
    Ok(())
}

/// `RE` and `CE` are in parallel across the same two nets, so they read
/// as a pair only if they sit at the SAME level.
pub fn ce_emitter_loads_share_a_row(placer: Placer, refine: bool) -> Result<(), String> {
    let p = place_fixture("common_emitter.cir", placer, refine);
    let (_, re_y) = pin_xy(&p, "RE", "e");
    let (_, ce_y) = pin_xy(&p, "CE", "e");
    if (re_y - ce_y).abs() < STEP_MM {
        return Ok(());
    }
    Err(format!(
        "parallel RE/CE must sit at the same level: RE.e.y={re_y:.2} CE.e.y={ce_y:.2}"
    ))
}

/// `RC` pulls the collector UP to VCC, so it belongs above `Q1`, in the
/// collector's own column.
///
/// The X half of this is the **pin-anchoring** assertion — it is what
/// caught ADR-40's origin-barycenter column (`RC.c.x = 15.240` against
/// `Q1.c.x = 17.780`, one BJT collector offset apart).
pub fn ce_rc_above_q1_collector(placer: Placer, refine: bool) -> Result<(), String> {
    let p = place_fixture("common_emitter.cir", placer, refine);
    check_above(
        &p,
        ("RC", "c"),
        ("Q1", "c"),
        "a supply-returning collector load belongs above the transistor",
    )?;
    let (rc_x, _) = pin_xy(&p, "RC", "c");
    let (q_x, _) = pin_xy(&p, "Q1", "c");
    if (rc_x - q_x).abs() < SAME_COLUMN_EPS_MM {
        return Ok(());
    }
    Err(format!(
        "RC must share Q1's collector column: RC.c.x={rc_x:.3} Q1.c.x={q_x:.3}"
    ))
}

/// `named_rails`' rails are called `p5` and `n5`, matching none of the
/// canonical supply names — only `;@ power=±5V` can classify them. If a
/// placer regresses to name matching, both fall through to
/// `NetClass::Signal`, every stub loses its vertical preference, and
/// this fails.
pub fn named_rails_convention(placer: Placer, refine: bool) -> Result<(), String> {
    let p = place_fixture("named_rails.cir", placer, refine);
    check_above(
        &p,
        ("RPU", "out"),
        ("RIN", "out"),
        "a stub to a *named* positive rail (+5V) still goes up",
    )?;
    for r in ["RPD", "CL"] {
        check_above(
            &p,
            ("RIN", "out"),
            (r, "out"),
            "a stub to a *named* negative rail (-5V) or to ground still goes down",
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Group B — the canonical-placement idioms (`idiom_placement.rs`)
// ---------------------------------------------------------------------------

/// PARALLEL two-terminal pair: `common_emitter`'s RE ‖ CE share one X
/// column and are adjacent.
pub fn ce_re_ce_parallel(placer: Placer, refine: bool) -> Result<(), String> {
    let p = place_fixture("common_emitter.cir", placer, refine);
    let (re_ex, _) = pin_xy(&p, "RE", "e");
    let (ce_ex, _) = pin_xy(&p, "CE", "e");
    if (re_ex - ce_ex).abs() >= SAME_COLUMN_EPS_MM {
        return Err(format!(
            "parallel RE‖CE must share an X column (vertical align): \
             RE.e-pin.x={re_ex:.3} CE.e-pin.x={ce_ex:.3}"
        ));
    }
    let re = elem(&p, "RE");
    let ce = elem(&p, "CE");
    let dy = (f64::from(re.origin.y) - f64::from(ce.origin.y)).abs();
    if dy <= 15.0 {
        return Ok(());
    }
    Err(format!(
        "parallel RE‖CE must be adjacent (close in Y), got |ΔY|={dy} cells"
    ))
}

/// COLLECTOR-LOAD above transistor, pin-anchored on the shared
/// collector net's X column.
pub fn diff_pair_collector_load_column(
    placer: Placer,
    refine: bool,
    rc: &str,
    q: &str,
    collector_net: &str,
) -> Result<(), String> {
    let p = place_fixture("diff_pair.cir", placer, refine);
    let (rc_x, _) = pin_xy(&p, rc, collector_net);
    let (q_x, _) = pin_xy(&p, q, collector_net);
    if (rc_x - q_x).abs() < SAME_COLUMN_EPS_MM {
        return Ok(());
    }
    Err(format!(
        "collector-load {rc} must share {q}'s collector ({collector_net}) X column: \
         {rc}.pin.x={rc_x:.3} {q}.collector.x={q_x:.3}"
    ))
}

/// SHARED-NODE centering: `diff_pair`'s RTAIL sits centred under the
/// shared tail node of Q1/Q2, one band below them.
pub fn diff_pair_rtail_centered(placer: Placer, refine: bool) -> Result<(), String> {
    let p = place_fixture("diff_pair.cir", placer, refine);
    let (rtail_x, _) = pin_xy(&p, "RTAIL", "tail");
    let (q1_x, _) = pin_xy(&p, "Q1", "tail");
    let (q2_x, _) = pin_xy(&p, "Q2", "tail");
    let mid = f64::midpoint(q1_x, q2_x);
    if (rtail_x - mid).abs() > STEP_MM {
        return Err(format!(
            "RTAIL must be centered under Q1/Q2's tail node: RTAIL.tail.x={rtail_x:.3} \
             midpoint={mid:.3} (Q1={q1_x:.3}, Q2={q2_x:.3})"
        ));
    }
    let rtail = elem(&p, "RTAIL");
    let q1 = elem(&p, "Q1");
    let q2 = elem(&p, "Q2");
    if rtail.origin.y > q1.origin.y && rtail.origin.y > q2.origin.y {
        return Ok(());
    }
    Err(format!(
        "RTAIL must sit below Q1/Q2: RTAIL.y={} Q1.y={} Q2.y={}",
        rtail.origin.y, q1.origin.y, q2.origin.y
    ))
}

// ---------------------------------------------------------------------------
// Group C — the inferred `align vertical` divider channel (`idioms.rs`)
// ---------------------------------------------------------------------------

const DIVIDER_SRC: &str = "\
resistor divider fixture
*@symbol Device:R for=R*
V1 in 0 DC 5 ;@ power=+5V
R1 in mid 10k
R2 mid 0 10k
.end
";

/// The divider pair must come out stacked in one vertical column — the
/// same outcome a user's `*@align vertical R1 R2` would have produced,
/// with no annotation written.
pub fn divider_co_aligns_vertically(placer: Placer, refine: bool) -> Result<(), String> {
    let p = place_src("<divider>", DIVIDER_SRC, placer, refine);
    let r1 = elem(&p, "R1");
    let r2 = elem(&p, "R2");
    if r1.origin.x != r2.origin.x {
        return Err(format!(
            "divider R1/R2 must share an X column (vertical align), got R1.x={} R2.x={}",
            r1.origin.x, r2.origin.x
        ));
    }
    if r1.origin.y != r2.origin.y {
        return Ok(());
    }
    Err("divider R1/R2 must be stacked (distinct Y)".to_string())
}

// ---------------------------------------------------------------------------
// Group D — the hard-constraint contract every placer owes
//           (`cases.rs`, `place_direction.rs`, `refine.rs`,
//            `v14_orientation.rs`)
// ---------------------------------------------------------------------------

fn solve_relation(placer: Placer, rel: Relation) -> ((f64, f64), (f64, f64)) {
    let resolved = super::mk_resolved(
        &["R1", "R2"],
        &[] as &[(Axis, &[&str])],
        &[("R2", rel, "R1")],
    );
    let (checked, _w) = check(resolved).expect("policy check");
    let p = place_with(checked, library(), &opts(placer, true)).expect("placement");
    (elem(&p, "R1").origin.to_mm(), elem(&p, "R2").origin.to_mm())
}

/// Annotation-spec §4.3: `place=above` / `below` / `right-of` /
/// `left-of` are hard constraints, so their direction is a structural
/// fact no placement engine may reinterpret.
pub fn place_relations_point_the_spec_way(placer: Placer) -> Result<(), String> {
    let cases: [(Relation, &str); 4] = [
        (Relation::Above, "above"),
        (Relation::Below, "below"),
        (Relation::RightOf, "right-of"),
        (Relation::LeftOf, "left-of"),
    ];
    let mut deltas = Vec::new();
    for (rel, label) in cases {
        let ((ax, ay), (tx, ty)) = solve_relation(placer, rel);
        let (ok, shared) = match rel {
            Relation::Above => (ty < ay, (tx - ax).abs() < TOL_MM),
            Relation::Below => (ty > ay, (tx - ax).abs() < TOL_MM),
            Relation::RightOf => (tx > ax, (ty - ay).abs() < TOL_MM),
            Relation::LeftOf => (tx < ax, (ty - ay).abs() < TOL_MM),
        };
        if !ok {
            return Err(format!(
                "spec §4.3: `R2 place={label} R1` put R2 the wrong side: \
                 R2=({tx:.3}, {ty:.3}) R1=({ax:.3}, {ay:.3})"
            ));
        }
        if !shared {
            return Err(format!(
                "spec §4.3: `{label}` must share the orthogonal coordinate: \
                 R2=({tx:.3}, {ty:.3}) R1=({ax:.3}, {ay:.3})"
            ));
        }
        deltas.push((ty - ay, tx - ax));
    }
    // `below` is the mirror of `above`; `left-of` the mirror of `right-of`.
    if (deltas[0].0 + deltas[1].0).abs() >= TOL_MM {
        return Err(format!(
            "spec §4.3 calls `below` the mirror of `above`: above Δy={}, below Δy={}",
            deltas[0].0, deltas[1].0
        ));
    }
    if (deltas[2].1 + deltas[3].1).abs() >= TOL_MM {
        return Err(format!(
            "spec §4.3 calls `left-of` the mirror of `right-of`: \
             right-of Δx={}, left-of Δx={}",
            deltas[2].1, deltas[3].1
        ));
    }
    Ok(())
}

/// `align` is a hard constraint: a horizontal group shares a row and a
/// vertical group shares a column, on every engine.
pub fn align_groups_share_their_axis(placer: Placer) -> Result<(), String> {
    for (axis, label) in [
        (Axis::Horizontal, "horizontal"),
        (Axis::Vertical, "vertical"),
    ] {
        let resolved = super::mk_resolved(&["R1", "R2"], &[(axis, &["R1", "R2"])], &[]);
        let (checked, _w) = check(resolved).expect("policy check");
        let p = place_with(checked, library(), &opts(placer, true)).expect("placement");
        let (r1, r2) = (elem(&p, "R1").origin, elem(&p, "R2").origin);
        let (same, distinct) = match axis {
            Axis::Horizontal => (r1.y == r2.y, r1.x != r2.x),
            Axis::Vertical => (r1.x == r2.x, r1.y != r2.y),
        };
        if !same {
            return Err(format!(
                "`align {label} R1 R2` must share the axis: R1={r1:?} R2={r2:?}"
            ));
        }
        if !distinct {
            return Err(format!(
                "`align {label} R1 R2` must keep the members distinct: R1={r1:?} R2={r2:?}"
            ));
        }
    }
    Ok(())
}

/// Pinned elements (here: `align`ed and `place`d ones) survive the SA
/// refiner untouched — the placer may not trade a hard constraint for a
/// soft one — and every refined origin is still on the KiCad grid
/// (CLAUDE.md § "Layout invariants").
pub fn pinned_stay_put_and_origins_stay_on_grid(placer: Placer) -> Result<(), String> {
    let resolved = super::mk_resolved(
        &["R1", "R2", "R3", "R4"],
        &[(Axis::Horizontal, &["R1", "R2"])],
        &[("R3", Relation::RightOf, "R1")],
    );
    let (checked, _w) = check(resolved).expect("policy check");
    let seed = place_with(checked.clone(), library(), &opts(placer, false)).expect("seed");
    let refined = LayoutOptions {
        refine: true,
        seed: 1,
        fr_iters: 30,
        refine_iterations: 500,
        placer,
    };
    let refined = place_with(checked, library(), &refined).expect("refined");

    for s in &seed.elements {
        if !matches!(s.refdes.as_str(), "R1" | "R2" | "R3") {
            continue;
        }
        let r = elem(&refined, &s.refdes);
        if r.origin != s.origin || r.orientation != s.orientation {
            return Err(format!(
                "constrained {} moved under refinement: seed {:?}/{:?} -> refined {:?}/{:?}",
                s.refdes, s.origin, s.orientation, r.origin, r.orientation
            ));
        }
    }
    for e in &refined.elements {
        let (x, y) = e.origin.to_mm();
        let step = spice_layout::GridPoint::STEP_MM;
        if (x / step - (x / step).round()).abs() > TOL_MM
            || (y / step - (y / step).round()).abs() > TOL_MM
        {
            return Err(format!("{} origin off-grid: ({x}, {y}) mm", e.refdes));
        }
    }
    Ok(())
}

/// Same placer + same SA seed → identical placement. The scoreboard,
/// `baseline_lock` and the ADR-4 position cache all assume this.
pub fn refinement_is_deterministic(placer: Placer) -> Result<(), String> {
    let resolved = super::mk_resolved(&["R1", "R2", "R3", "R4", "R5"], &[], &[]);
    let (checked, _w) = check(resolved).expect("policy check");
    let o = LayoutOptions {
        refine: true,
        seed: 42,
        fr_iters: 30,
        refine_iterations: 500,
        placer,
    };
    let a = place_with(checked.clone(), library(), &o).expect("run a");
    let b = place_with(checked, library(), &o).expect("run b");
    for (ea, eb) in a.elements.iter().zip(b.elements.iter()) {
        if ea.refdes != eb.refdes || ea.origin != eb.origin || ea.orientation != eb.orientation {
            return Err(format!(
                "non-deterministic: {} {:?}/{:?} vs {} {:?}/{:?}",
                ea.refdes, ea.origin, ea.orientation, eb.refdes, eb.origin, eb.orientation
            ));
        }
    }
    Ok(())
}

/// A real multi-pin power-bearing device (the inverting-opamp X1): V+ on
/// pin 8 (lib-up), V- on pin 4 (lib-down). Its V14 allowed set is
/// genuinely *restricted*, so it — unlike a 2-pin rail source — is
/// governed by the orientation filter rather than skipped.
const OPAMP_SRC: &str = "test\n\
    *@symbol Amplifier_Operational:OPAMP for=X1 pinmap=1:3,2:2,3:1,4:8,5:4\n\
    VCC vcc 0 DC 15 ;@ power=+15V\n\
    VEE vee 0 DC -15 ;@ power=-15V\n\
    .subckt OPAMP inp inn out vcc vee\n\
    E1 out 0 inp inn 1e5\n\
    .ends\n\
    RIN in inv 1k\n\
    RF inv out 10k\n\
    X1 0 inv out vcc vee OPAMP\n\
    .end\n";

/// V14 is a **hard constraint** at every stage of every placer
/// (CLAUDE.md § "Constraints vs. costs"): the orientation candidate set
/// is filtered, so no engine can emit a governed supply pin facing the
/// wrong way. Run on the seed path and on a short SA, so both the seed
/// chooser and the rotate / mirror-Y gate are exercised.
pub fn v14_holds_on_governed_devices(placer: Placer) -> Result<(), String> {
    let checked = checked_from_src(OPAMP_SRC);
    let prefs = vertical_prefs(&checked);
    let allowed = allowed_orientations(&checked, placer);
    let mut governed_total = 0_usize;

    for o in [
        opts(placer, false),
        LayoutOptions {
            refine: true,
            seed: 7,
            fr_iters: 0,
            refine_iterations: 1500,
            placer,
        },
    ] {
        let placement = place_with(checked.clone(), library(), &o).expect("placement");
        for (idx, (el, placed)) in checked.elements.iter().zip(&placement.elements).enumerate() {
            // The full-8 fallback (a genuinely infeasible filter) is
            // unconstrained by design — the decoration stub covers it.
            if allowed[idx].len() == Orientation::ALL.len() {
                continue;
            }
            governed_total += 1;
            let pins = el.symbol.pins_in(placed.orientation);
            let ident = el.symbol.pins_in(Orientation::IDENTITY);
            for (ti, node) in el.nodes.iter().enumerate() {
                let (Some(pref), Some(kpin)) = (prefs.get(node), el.pin_mapping.get(ti)) else {
                    continue;
                };
                let native_vertical = ident
                    .iter()
                    .find(|p| &p.number == kpin)
                    .is_some_and(|p| matches!(p.angle % 360, 90 | 270));
                if !native_vertical {
                    continue;
                }
                let Some(p) = pins.iter().find(|p| &p.number == kpin) else {
                    continue;
                };
                let facing = match p.angle % 360 {
                    270 => "up",
                    90 => "down",
                    _ => "horizontal",
                };
                let want = match pref {
                    VertPref::Up => "up",
                    VertPref::Down => "down",
                };
                if facing != want {
                    return Err(format!(
                        "V14: {}.{kpin} (net {node}) faces {facing}, want {want} (refine={})",
                        el.refdes, o.refine
                    ));
                }
            }
        }
    }
    if governed_total >= 2 {
        return Ok(());
    }
    Err(format!(
        "vacuous: no element was V14-governed (expected the opamp X1 restricted on \
         both paths, got {governed_total})"
    ))
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// One named structural check.
pub struct Check {
    /// Stable id, used by the cross-arm sweep's expected-failure
    /// registry. `<group>::<property>[<stage>]`.
    pub id: &'static str,
    pub run: fn(Placer) -> Result<(), String>,
}

/// Every placer-agnostic structural check, in one table.
///
/// `[seed]` runs the deterministic stage-1 placer alone; `[refined]`
/// runs it through the FR + SA refiner. Both stages are listed because
/// CLAUDE.md's consistency requirement is exactly that a property held
/// at seed time must still hold after every stage that can move the
/// element — a seed-only guarantee is the documented bug shape.
pub const CHECKS: &[Check] = &[
    Check {
        id: "rail::r2_below_q1[seed]",
        run: |p| ce_r2_below_q1(p, false),
    },
    Check {
        id: "rail::r2_below_q1[refined]",
        run: |p| ce_r2_below_q1(p, true),
    },
    Check {
        id: "rail::emitter_loads_below_q1[seed]",
        run: |p| ce_emitter_loads_below_q1(p, false),
    },
    Check {
        id: "rail::emitter_loads_below_q1[refined]",
        run: |p| ce_emitter_loads_below_q1(p, true),
    },
    Check {
        id: "rail::emitter_loads_share_a_row[seed]",
        run: |p| ce_emitter_loads_share_a_row(p, false),
    },
    Check {
        id: "rail::emitter_loads_share_a_row[refined]",
        run: |p| ce_emitter_loads_share_a_row(p, true),
    },
    Check {
        id: "rail::rc_above_q1_collector[seed]",
        run: |p| ce_rc_above_q1_collector(p, false),
    },
    Check {
        id: "rail::rc_above_q1_collector[refined]",
        run: |p| ce_rc_above_q1_collector(p, true),
    },
    Check {
        id: "rail::named_rails_convention[seed]",
        run: |p| named_rails_convention(p, false),
    },
    Check {
        id: "rail::named_rails_convention[refined]",
        run: |p| named_rails_convention(p, true),
    },
    Check {
        id: "idiom::re_ce_parallel[seed]",
        run: |p| ce_re_ce_parallel(p, false),
    },
    Check {
        id: "idiom::re_ce_parallel[refined]",
        run: |p| ce_re_ce_parallel(p, true),
    },
    Check {
        id: "idiom::rc1_over_q1_collector[seed]",
        run: |p| diff_pair_collector_load_column(p, false, "RC1", "Q1", "c1"),
    },
    Check {
        id: "idiom::rc1_over_q1_collector[refined]",
        run: |p| diff_pair_collector_load_column(p, true, "RC1", "Q1", "c1"),
    },
    Check {
        id: "idiom::rc2_over_q2_collector[seed]",
        run: |p| diff_pair_collector_load_column(p, false, "RC2", "Q2", "c2"),
    },
    Check {
        id: "idiom::rc2_over_q2_collector[refined]",
        run: |p| diff_pair_collector_load_column(p, true, "RC2", "Q2", "c2"),
    },
    Check {
        id: "idiom::rtail_centered[seed]",
        run: |p| diff_pair_rtail_centered(p, false),
    },
    Check {
        id: "idiom::rtail_centered[refined]",
        run: |p| diff_pair_rtail_centered(p, true),
    },
    Check {
        id: "idiom::divider_co_aligns[seed]",
        run: |p| divider_co_aligns_vertically(p, false),
    },
    Check {
        id: "idiom::divider_co_aligns[refined]",
        run: |p| divider_co_aligns_vertically(p, true),
    },
    Check {
        id: "contract::place_relations",
        run: place_relations_point_the_spec_way,
    },
    Check {
        id: "contract::align_groups",
        run: align_groups_share_their_axis,
    },
    Check {
        id: "contract::pinned_and_grid",
        run: pinned_stay_put_and_origins_stay_on_grid,
    },
    Check {
        id: "contract::determinism",
        run: refinement_is_deterministic,
    },
    Check {
        id: "contract::v14_governed",
        run: v14_holds_on_governed_devices,
    },
];

thread_local! {
    /// Set while a check body runs, so [`install_quiet_hook`] drops the
    /// default hook's "thread panicked at …" spew for a panic this
    /// module is about to convert into a failure string.
    static SILENCE_PANICS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Install a panic hook that honours [`SILENCE_PANICS`].
///
/// Deliberately installed **once per process, and thread-locally
/// scoped**. `set_hook` / `take_hook` are process-global, so the obvious
/// take-silence-restore dance around each check corrupts the hook of
/// every *other* test running concurrently — under `--test-threads=N`
/// that silently swallowed 12 of 25 arms' failure reports on the first
/// run of this sweep.
fn install_quiet_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !SILENCE_PANICS.with(std::cell::Cell::get) {
                previous(info);
            }
        }));
    });
}

/// Run one check, converting a panic (a missing refdes, an
/// unplaceable fixture) into a failure string rather than letting it
/// abort the whole sweep.
#[must_use]
pub fn run_check(check: &Check, placer: Placer) -> Option<String> {
    install_quiet_hook();
    SILENCE_PANICS.with(|c| c.set(true));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (check.run)(placer)));
    SILENCE_PANICS.with(|c| c.set(false));
    match outcome {
        Ok(Ok(())) => None,
        Ok(Err(msg)) => Some(msg),
        Err(payload) => {
            let what = payload
                .downcast_ref::<&str>()
                .map(std::string::ToString::to_string)
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic>".to_string());
            Some(format!("PANIC: {what}"))
        }
    }
}
