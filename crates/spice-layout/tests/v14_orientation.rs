//! V14 power-glyph-orientation hard constraint, end-to-end through the
//! placer (seed + SA refiner).
//!
//! These tests prove the constraint holds by *construction*, not by
//! luck: they run `place_with` across many SA seeds and assert that no
//! element with a vertical supply pin is ever left with that pin facing
//! the wrong screen direction. If the SA gate (rotate / mirror-Y
//! accept-reject against the allowed set) regressed, a seed would
//! eventually rotate a power-bearing element out of its feasible set
//! and trip the assertion.

use std::path::PathBuf;
use std::sync::OnceLock;

use kicad_symbols::{Library, Orientation};
use spice_diagnostics::FileId;
use spice_layout::net_class::{VertPref, vertical_prefs};
use spice_layout::orient::allowed_orientations;
use spice_layout::{LayoutOptions, place_with};
use spice_policy::{CheckedNetlist, check};

/// A library that, unlike [`common::fixture_library`], also carries the
/// `Amplifier_Operational:OPAMP` symbol — needed so the many-seed
/// fixture can include a genuine *multi-pin* power-bearing device whose
/// V14 allowed-orientation set is actually *restricted* (the 2-pin rail
/// sources are unconstrained by design, see `orient.rs`).
fn opamp_library() -> &'static Library {
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
        lib = lib.merge(
            Library::from_file(dir.join("Simulation_SPICE.kicad_sym"))
                .expect("load Simulation_SPICE fixture library"),
        );
        lib.merge(
            Library::from_file(dir.join("Amplifier_Operational.kicad_sym"))
                .expect("load Amplifier_Operational fixture library"),
        )
    })
}

/// Parse + resolve + policy-check a SPICE source against the opamp
/// library. Used to build a multi-pin power-bearing device (the opamp
/// X1) the way the real pipeline does — its V14 allowed set is genuinely
/// restricted, so the SA gate is exercised rather than skipped.
fn checked_from_src(src: &str) -> CheckedNetlist {
    let parsed = spice_parser::parse(src, FileId(0))
        .expect("parse failed")
        .netlist;
    let resolved = spice_resolve::resolve(&parsed, opamp_library()).expect("resolve failed");
    check(resolved).expect("policy check failed").0
}

/// Outcome of a V14 check over a placement: the human-readable
/// violations plus how many elements were *actually* governed (had a
/// restricted allowed set). The latter guards against a vacuous test —
/// a fixture of only 2-pin rail sources restricts nothing, so an empty
/// `violations` would prove nothing about the SA gate.
struct V14Check {
    violations: Vec<String>,
    governed: usize,
}

/// For each element, return its V14-violating vertical supply pins
/// after placement: `(refdes, term_idx, screen_dir, want)`. Empty means
/// V14 holds. Screen facing is computed exactly as the emitter does:
/// the library-frame pin angle is passed straight through (270 → up,
/// 90 → down), while the pin's world Y is negated — so the angle's
/// vertical sense already matches the screen.
fn v14_violations(checked: &CheckedNetlist, library: &Library, opts: &LayoutOptions) -> V14Check {
    let placement = place_with(checked.clone(), library, opts).expect("placement");
    let prefs = vertical_prefs(checked);
    let allowed = allowed_orientations(checked);
    let mut out = Vec::new();
    let mut governed = 0_usize;
    for (idx, (el, placed)) in checked.elements.iter().zip(&placement.elements).enumerate() {
        // Elements whose V14 filter is genuinely infeasible (the
        // full-8 fallback, e.g. a negative-rail source whose ground and
        // vee pins both want screen-down) are unconstrained by design —
        // the rails decoration stub covers them. Only assert V14 where
        // the filter actually restricts the orientation set.
        if allowed[idx].len() == Orientation::ALL.len() {
            continue;
        }
        governed += 1;
        let pins = el.symbol.pins_in(placed.orientation);
        let ident = el.symbol.pins_in(Orientation::IDENTITY);
        for (ti, node) in el.nodes.iter().enumerate() {
            let Some(pref) = prefs.get(node) else {
                continue;
            };
            let Some(kpin) = el.pin_mapping.get(ti) else {
                continue;
            };
            // Only supply-style (natively vertical) pins are governed.
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
                out.push(format!(
                    "{}.{kpin} (net {node}) faces {facing}, want {want}",
                    el.refdes,
                ));
            }
        }
    }
    V14Check {
        violations: out,
        governed,
    }
}

/// A real multi-pin power-bearing device (the inverting-opamp X1 from
/// the `opamp_inverting_real` family): V+ on pin 8 (lib-up), V- on
/// pin 4 (lib-down). Its V14 allowed set is *restricted* (R90/R270 and
/// R180 are excluded), so it — unlike the 2-pin rail sources — is
/// genuinely governed by the SA rotate / mirror-Y gate. Without it the
/// many-seed loop would assert nothing (the blocking-item vacuity bug).
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

#[test]
fn v14_holds_across_many_sa_seeds() {
    // A genuine multi-pin power-bearing device (the opamp X1) whose V14
    // allowed set is *restricted* — this is what actually exercises the
    // SA rotate / mirror-Y accept-reject gate. The surrounding rail
    // sources + feedback resistors give the SA freedom to move.
    let checked = checked_from_src(OPAMP_SRC);
    let lib = opamp_library();

    let mut total_governed = 0_usize;
    for seed in 0..64_u64 {
        let opts = LayoutOptions {
            refine: true,
            seed,
            fr_iters: 0,
            // A healthy budget so the rotate / mirror-Y moves fire many
            // times — the gate must hold every single one.
            refine_iterations: 1500,
        };
        let check = v14_violations(&checked, lib, &opts);
        assert!(
            check.violations.is_empty(),
            "seed {seed}: V14 violated after SA:\n  {}",
            check.violations.join("\n  "),
        );
        total_governed += check.governed;
    }
    // Guard against silent vacuity: every seed must have actually
    // checked the restricted opamp. If a future change loosens the
    // opamp's allowed set to the full eight (e.g. the orient filter
    // regresses), `governed` drops to 0 and the empty-violations
    // assertion above would pass for the wrong reason — this trips it.
    assert!(
        total_governed >= 64,
        "vacuous test: no element was V14-governed across the seeds \
         (expected the opamp X1 to be restricted on every seed, got \
         {total_governed} governed-element checks over 64 seeds)",
    );
}

#[test]
fn v14_holds_with_refine_disabled() {
    // Seed-only path (pick_orientations) must also satisfy V14 — and on
    // a genuinely restricted multi-pin device, not just 2-pin sources.
    let checked = checked_from_src(OPAMP_SRC);
    let opts = LayoutOptions {
        refine: false,
        ..LayoutOptions::default()
    };
    let check = v14_violations(&checked, opamp_library(), &opts);
    assert!(check.violations.is_empty());
    assert!(
        check.governed >= 1,
        "vacuous test: seed-only path governed no element"
    );
}
