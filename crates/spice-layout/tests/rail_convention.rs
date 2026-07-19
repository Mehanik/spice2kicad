//! Property tests for the **rail-direction drawing convention**, the
//! rule a reader uses to orient themselves on any analog schematic:
//!
//! > Signals connected to a positive supply go **up**; signals connected
//! > to ground go **down**; a negative rail goes lower still.
//!
//! These are position-only assertions over the public `Placement` (the
//! ADR-7 strategy), read from live pin geometry via `world_pin_mm`, so
//! they hold wherever the anchor devices finally land.
//!
//! # Why these exist
//!
//! The convention was encoded in `cost::rail_direction`, but **inverted**:
//! the term read `pin_extents_y`'s `(y_min, y_max)` return as
//! `(y_top, y_bot)`, calling the *largest* screen Y "top". Screen Y
//! increases downward, so the term pulled positive rails to the bottom of
//! the sheet and ground to the top — and at weight 200 over squared
//! millimetres it was ~97% of the whole objective, so it comfortably
//! overpowered the band terms that were pulling the right way. Every
//! part with a single rail connection ended up on the wrong side of the
//! device it serves. Nothing caught it, because the one test that
//! covered the term (`tests/cost.rs::rail_direction_power_above_zero_below`)
//! asserted the inverted expectation too.
//!
//! The properties below are the falsifiable form of the convention, so a
//! sign error cannot come back silently.

use std::path::PathBuf;
use std::sync::OnceLock;

use kicad_symbols::Library;
use spice_diagnostics::FileId;
use spice_layout::{LayoutOptions, Placement, place_with};
use spice_policy::check;

fn fixture_library() -> &'static Library {
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

fn place_fixture(name: &str, refine: bool) -> Placement {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = manifest.join("../spice2kicad/tests/fixtures").join(name);
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
    let parsed = spice_parser::parse(&src, FileId(0))
        .expect("parse failed")
        .netlist;
    let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
    let (checked, _warns) = check(resolved).expect("policy check failed");
    let opts = LayoutOptions {
        refine,
        ..LayoutOptions::default()
    };
    place_with(checked, fixture_library(), &opts).expect("placement")
}

/// World `(x, y)` mm of the pin of `refdes` that sits on SPICE `node`.
fn pin_xy(p: &Placement, refdes: &str, node: &str) -> (f64, f64) {
    let el = p
        .elements
        .iter()
        .find(|e| e.refdes == refdes)
        .unwrap_or_else(|| panic!("element {refdes} not placed"));
    let ti = el
        .nodes
        .iter()
        .position(|n| n == node)
        .unwrap_or_else(|| panic!("{refdes} has no terminal on {node}"));
    let want = &el.pin_mapping[ti];
    // The resolved symbol is not on `PlacedElement`; look it up by lib_id.
    let sym = fixture_library()
        .lookup(&el.lib_id)
        .unwrap_or_else(|| panic!("symbol {} not in fixture library", el.lib_id));
    el.world_pin_mm(sym)
        .into_iter()
        .find(|(num, _, _)| num == want)
        .map_or_else(
            || panic!("{refdes} pin {want} missing"),
            |(_, x, y)| (x, y),
        )
}

/// Screen Y grows downward, so "A is above B" is `A.y < B.y`. A full
/// grid step of separation is required, not merely a tie-break, so an
/// accidental near-coincidence cannot pass.
const STEP_MM: f64 = 1.27;

fn assert_above(p: &Placement, upper: (&str, &str), lower: (&str, &str), why: &str) {
    let (_, uy) = pin_xy(p, upper.0, upper.1);
    let (_, ly) = pin_xy(p, lower.0, lower.1);
    assert!(
        uy < ly - STEP_MM,
        "{why}: expected {}.{} ABOVE {}.{} (smaller screen Y), got y={uy:.2} vs y={ly:.2}",
        upper.0,
        upper.1,
        lower.0,
        lower.1
    );
}

// ---------------------------------------------------------------------------
// common_emitter — the fixture the defects were reported on
// ---------------------------------------------------------------------------

/// Defect 1: `R2` connects the base to ground, so it belongs UNDER `Q1`,
/// not above it. Measured on the base net `b`, which both share.
fn assert_r2_below_q1(refine: bool) {
    let p = place_fixture("common_emitter.cir", refine);
    assert_above(
        &p,
        ("Q1", "b"),
        ("R2", "b"),
        "a ground-returning stub belongs below the device it serves",
    );
}

#[test]
fn common_emitter_r2_below_q1_seed() {
    assert_r2_below_q1(false);
}

#[test]
fn common_emitter_r2_below_q1_refined() {
    assert_r2_below_q1(true);
}

/// Defect 2 (part 1): `RE` and `CE` both return the emitter to ground, so
/// both belong UNDER `Q1`.
fn assert_emitter_loads_below_q1(refine: bool) {
    let p = place_fixture("common_emitter.cir", refine);
    for r in ["RE", "CE"] {
        assert_above(
            &p,
            ("Q1", "e"),
            (r, "e"),
            "an emitter-to-ground stub belongs below the transistor",
        );
    }
}

#[test]
fn common_emitter_emitter_loads_below_q1_seed() {
    assert_emitter_loads_below_q1(false);
}

#[test]
fn common_emitter_emitter_loads_below_q1_refined() {
    assert_emitter_loads_below_q1(true);
}

/// Defect 2 (part 2): `RE` and `CE` are in parallel across the same two
/// nets, so they read as a pair only if they sit at the SAME level.
///
/// Note this is the *horizontal* reading of "aligned" — side by side on a
/// shared row. The vertical single-column stack that
/// `idiom_placement.rs`'s deferred Idiom 1 asserts is deliberately NOT
/// what this expects: a same-column stack shorts one shared net past the
/// other's pin (V11, Tier 0) under the orientation wall, which is exactly
/// why that idiom is still deferred.
fn assert_emitter_loads_share_a_row(refine: bool) {
    let p = place_fixture("common_emitter.cir", refine);
    let (_, re_y) = pin_xy(&p, "RE", "e");
    let (_, ce_y) = pin_xy(&p, "CE", "e");
    assert!(
        (re_y - ce_y).abs() < STEP_MM,
        "parallel RE/CE must sit at the same level: RE.e.y={re_y:.2} CE.e.y={ce_y:.2}"
    );
}

// RED. The refined case passes; the SEED does not level the pair,
// because `apply_rail_stub_columns` adjusts X only. Levelling Y within a
// stub group was implemented and MEASURED: on its own it is fine, but
// together with the `bands.rs` VertPref switch it regressed Tier-1
// `v13_labels_no_mutual_overlap`, `v13_labels_dont_overlap_symbol_body`
// and `no_symbol_symbol_overlap_across_fixtures`. Both were reverted
// under the tier rule. Re-land the levelling alone and re-measure.
#[test]
#[ignore = "seed does not level parallel stubs; Y-levelling reverted after a Tier-1 V13 regression"]
fn common_emitter_emitter_loads_share_a_row_seed() {
    assert_emitter_loads_share_a_row(false);
}

#[test]
fn common_emitter_emitter_loads_share_a_row_refined() {
    assert_emitter_loads_share_a_row(true);
}

/// Defect 3: `RC` pulls the collector UP to VCC, so it belongs above
/// `Q1`, in the collector's own column.
fn assert_rc_above_q1_collector(refine: bool) {
    let p = place_fixture("common_emitter.cir", refine);
    assert_above(
        &p,
        ("RC", "c"),
        ("Q1", "c"),
        "a supply-returning collector load belongs above the transistor",
    );
    let (rc_x, _) = pin_xy(&p, "RC", "c");
    let (q_x, _) = pin_xy(&p, "Q1", "c");
    assert!(
        (rc_x - q_x).abs() < STEP_MM / 2.0,
        "RC must share Q1's collector column: RC.c.x={rc_x:.3} Q1.c.x={q_x:.3}"
    );
}

#[test]
fn common_emitter_rc_above_q1_collector_seed() {
    assert_rc_above_q1_collector(false);
}

// RED. The seed places RC on the collector column correctly; the SA then
// drifts it ~3 cells off. Raising `CostWeights::rail_stub_alignment` from
// 50 to 300 holds the column and makes this pass, but MEASURED at 300 it
// regressed Tier-1 V13 and pushed `opamp_inverting_real` V5 1 -> 3. The
// column needs to be held by something that is not a bigger soft weight
// — most likely a hard candidate-space filter on the stub's X (CLAUDE.md
// "Constraints vs. costs"), which is a v0.2-shaped change.
#[test]
#[ignore = "SA drifts the stub column; holding it via a larger soft weight regressed Tier-1 V13"]
fn common_emitter_rc_above_q1_collector_refined() {
    assert_rc_above_q1_collector(true);
}

// ---------------------------------------------------------------------------
// named_rails — the convention must key off `*@power=`, not net NAMES
// ---------------------------------------------------------------------------

/// The whole point of the `named_rails` fixture: its rails are called
/// `p5` and `n5`, which match none of the canonical supply names
/// (`vcc` / `vdd` / `gnd` / `vee` / …). The only thing that can classify
/// them is `;@ power=+5V` / `;@ power=-5V`.
///
/// If the placer ever regresses to name matching, `p5` and `n5` both fall
/// through to `NetClass::Signal`, every stub here loses its vertical
/// preference, and these assertions fail.
fn assert_named_rail_convention(refine: bool) {
    let p = place_fixture("named_rails.cir", refine);
    // RPU returns `out` to the +5V rail → above the node it serves.
    assert_above(
        &p,
        ("RPU", "out"),
        ("RIN", "out"),
        "a stub to a *named* positive rail (+5V) still goes up",
    );
    // RPD returns `out` to the -5V rail, CL returns it to ground → below.
    for r in ["RPD", "CL"] {
        assert_above(
            &p,
            ("RIN", "out"),
            (r, "out"),
            "a stub to a *named* negative rail (-5V) or to ground still goes down",
        );
    }
}

// RED. The refined case passes. The SEED puts the `-5V` stub UP, because
// `bands::assign_y_bands` bands off `NetClass` (a `*@power=-5V` net is
// `NetClass::Power` -> Top band) while `cost::rail_direction` bands off
// `vertical_prefs` (negative rail -> Down). Switching `assign_y_bands` to
// `vertical_prefs` fixes exactly this and is the right generalisation,
// but MEASURED it regressed Tier-1 V13 + symbol-overlap, so it was
// reverted pending sign-off. See the report accompanying this branch.
#[test]
#[ignore = "bands.rs bands off NetClass, so a negative rail seeds UP; the VertPref switch regressed Tier-1"]
fn named_rails_convention_seed() {
    assert_named_rail_convention(false);
}

#[test]
fn named_rails_convention_refined() {
    assert_named_rail_convention(true);
}

/// The generalisation, stated directly on the classifier rather than on a
/// placement: a `*@power=`-tagged rail gets a vertical preference from its
/// TAG, and the sign of the tagged voltage decides the direction.
#[test]
fn named_rail_polarity_comes_from_the_power_tag() {
    use spice_layout::net_class::{VertPref, vertical_prefs};

    let src = "test\n\
               VPOS p5 0 DC  5 ;@ power=+5V\n\
               VNEG n5 0 DC -5 ;@ power=-5V\n\
               RPU p5 out 10k\n\
               RPD out n5 10k\n\
               .end\n";
    let parsed = spice_parser::parse(src, FileId(0))
        .expect("parse failed")
        .netlist;
    let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
    let (checked, _w) = check(resolved).expect("policy check failed");
    let prefs = vertical_prefs(&checked);

    assert_eq!(
        prefs.get("p5"),
        Some(&VertPref::Up),
        "`;@ power=+5V` must make `p5` an up-rail despite its non-canonical name"
    );
    assert_eq!(
        prefs.get("n5"),
        Some(&VertPref::Down),
        "`;@ power=-5V` must make `n5` a down-rail despite its non-canonical name"
    );
    assert_eq!(prefs.get("0"), Some(&VertPref::Down), "ground goes down");
    assert_eq!(prefs.get("out"), None, "a signal net has no preference");
}
