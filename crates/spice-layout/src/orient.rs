//! V14 power-glyph-orientation hard constraint: the per-element
//! allowed-orientation set.
//!
//! V14 says a VCC/positive-rail pin must face screen-**up** and a
//! GND/negative-rail pin must face screen-**down**. This is a
//! *categorical, Tier-1* geometric fact, so per CLAUDE.md it is a
//! **hard candidate-space filter**, never a soft cost. The same filter
//! must bind every stage that can move an element:
//!
//! * the V5 seed orientation chooser ([`crate::pick_orientations`]),
//!   which scores only over the allowed set; and
//! * the SA refiner ([`crate::solver`]), whose rotate / mirror-Y moves
//!   accept-reject against the allowed set.
//!
//! [`allowed_orientations`] computes, for each element, the subset of
//! [`Orientation::ALL`] that satisfies V14. Elements with no
//! power/ground pin allow all eight. Elements whose power pin is forced
//! sideways by every orientation (an empty filtered set) fall back to
//! all eight — the rails decoration stub then covers the glyph (V14's
//! documented detached-glyph fallback).

use kicad_symbols::Orientation;
use spice_policy::CheckedNetlist;

use crate::net_class::{VertPref, vertical_prefs};

/// Screen-vertical facing of a transformed pin angle.
///
/// The emitter passes the library-frame (`Y`-up) pin angle straight
/// through to the router, then negates the pin's world `Y`. Net result
/// (see `kicad-emitter::angle_to_direction`): library angle 270 renders
/// screen-**up**, 90 renders screen-**down**. 0/180 are horizontal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenFacing {
    Up,
    Down,
    Horizontal,
}

fn screen_facing(transformed_angle: u16) -> ScreenFacing {
    match transformed_angle % 360 {
        270 => ScreenFacing::Up,
        90 => ScreenFacing::Down,
        _ => ScreenFacing::Horizontal,
    }
}

/// True when `orient` satisfies V14 for the given element: every pin on
/// a positive rail faces up and every pin on a negative/ground rail
/// faces down. Elements with no rail pins trivially satisfy it.
fn satisfies_v14(
    elem: &spice_resolve::ResolvedElement,
    prefs: &std::collections::HashMap<String, VertPref>,
    orient: Orientation,
) -> bool {
    let pins = elem.symbol.pins_in(orient);
    let ident_pins = elem.symbol.pins_in(Orientation::IDENTITY);
    for (term_idx, node) in elem.nodes.iter().enumerate() {
        let Some(pref) = prefs.get(node) else {
            continue; // signal pin: no orientation constraint
        };
        let Some(kicad_pin) = elem.pin_mapping.get(term_idx) else {
            continue;
        };
        // V14 governs *supply-style* pins only: pins that point
        // vertically in the symbol's native (identity) frame. A pin
        // drawn horizontally at identity (e.g. an opamp's `+`/`-`
        // input that happens to be wired to ground in a particular
        // circuit) is a signal/input pin, not a rail-supply pin —
        // rotating the whole part to make it vertical would scramble
        // the layout. Such a rail pin is a don't-care for orientation;
        // its glyph is handled by the rails decoration stub instead.
        let native_vertical = ident_pins
            .iter()
            .find(|p| &p.number == kicad_pin)
            .is_some_and(|p| matches!(p.angle % 360, 90 | 270));
        if !native_vertical {
            continue;
        }
        let Some(p) = pins.iter().find(|p| &p.number == kicad_pin) else {
            continue;
        };
        let want = match pref {
            VertPref::Up => ScreenFacing::Up,
            VertPref::Down => ScreenFacing::Down,
        };
        if screen_facing(p.angle) != want {
            return false;
        }
    }
    true
}

/// Per-element allowed-orientation set for the V14 hard constraint.
///
/// `result[i]` is the subset of [`Orientation::ALL`] permitted for
/// `checked.elements[i]`. Guarantees:
///
/// * Every returned set is **non-empty** so callers can treat it as an
///   unconditional filter. Resolution order:
///   1. orientations satisfying V14 outright (every rail pin faces its
///      ideal screen direction); else
///   2. the full eight (the conflicting ±rail / source case — e.g. a
///      negative-supply source whose vee and ground pins both want
///      screen-down has no ideal orientation; the rails decoration stub
///      then offsets the glyph one cell out).
/// * Order within each set follows [`Orientation::ALL`], so callers'
///   deterministic tie-breaks are preserved.
#[must_use]
pub fn allowed_orientations(checked: &CheckedNetlist) -> Vec<Vec<Orientation>> {
    let prefs = vertical_prefs(checked);
    checked
        .elements
        .iter()
        .map(|elem| {
            // The ≤2-terminal exemption is scoped to elements for which
            // V14 carries no orientation information:
            //
            //   * A 2-pin *power source* (`VCC vcc 0`, `VEE vee 0`): its
            //     body is replaced by a rail glyph entirely placed and
            //     oriented by the rails decoration stub (V14's documented
            //     detached-glyph fallback). Locking the source body's
            //     orientation reshuffles the layout for zero V14 benefit.
            //   * A 2-pin element with *no rail pin at all* (a pure
            //     signal element like `CIN in b`): nothing to orient
            //     against a rail, so all eight survive trivially anyway —
            //     `satisfies_v14` would return `true` for every
            //     orientation, but short-circuiting keeps the seed
            //     candidate set the full eight (matching prior behaviour
            //     exactly, so signal-only placement is byte-identical).
            //
            // A 2-pin *rail consumer* (`RC vcc c`, `R1 vcc b`) is NOT
            // exempt: one pin is a real rail pin whose V14 facing applies
            // (rail pin out toward its band → glyph on the body exterior),
            // and its signal pin is then forced opposite, toward the Mid
            // band where its neighbour lives. It must flow into the
            // `satisfies_v14` filter below so the rail pin faces its band.
            let is_power_source = matches!(elem.role, spice_resolve::ElementRole::Power(_));
            let has_rail_pin = elem.nodes.iter().any(|n| prefs.contains_key(n));
            if elem.nodes.len() <= 2 && (is_power_source || !has_rail_pin) {
                return Orientation::ALL.to_vec();
            }
            let filtered: Vec<Orientation> = Orientation::ALL
                .iter()
                .copied()
                .filter(|&o| satisfies_v14(elem, &prefs, o))
                .collect();
            if filtered.is_empty() {
                // No V14-ideal orientation (e.g. a negative-supply
                // source whose vee and ground pins both want
                // screen-down). Fall back to the full eight — the rails
                // decoration stub offsets the glyph.
                Orientation::ALL.to_vec()
            } else {
                filtered
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kicad_symbols::{Library, Rotation};
    use spice_diagnostics::FileId;
    use spice_policy::check;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    fn fixture_library() -> &'static Library {
        static LIB: OnceLock<Library> = OnceLock::new();
        LIB.get_or_init(|| {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let fixture_dir = manifest
                .parent()
                .and_then(std::path::Path::parent)
                .expect("workspace root")
                .join("crates/kicad-symbols/tests/fixtures");
            let mut lib = Library::from_file(fixture_dir.join("Device.kicad_sym"))
                .expect("load Device fixture library");
            lib = lib.merge(
                Library::from_file(fixture_dir.join("Simulation_SPICE.kicad_sym"))
                    .expect("load Simulation_SPICE fixture library"),
            );
            lib.merge(
                Library::from_file(fixture_dir.join("Amplifier_Operational.kicad_sym"))
                    .expect("load Amplifier_Operational fixture library"),
            )
        })
    }

    fn allowed_str(src: &str) -> (Vec<String>, Vec<Vec<Orientation>>) {
        let file_id = FileId(0);
        let parsed = spice_parser::parse(src, file_id)
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let (checked, _warns) = check(resolved).expect("policy check failed");
        let refdes = checked.elements.iter().map(|e| e.refdes.clone()).collect();
        (refdes, allowed_orientations(&checked))
    }

    fn idx_of(refdes: &[String], r: &str) -> usize {
        refdes.iter().position(|x| x == r).expect("refdes present")
    }

    #[test]
    fn signal_only_element_allows_all_eight() {
        let (refdes, allowed) = allowed_str("test\nV1 in 0 AC 1\nR1 in out 1k\n.end\n");
        let i = idx_of(&refdes, "R1");
        assert_eq!(allowed[i].len(), 8);
    }

    #[test]
    fn two_pin_rail_consumer_is_orientation_filtered() {
        // A 2-pin rail *consumer* (R1 with a vcc rail pin + a signal pin)
        // IS orientation-locked by V14 (R-5 fix): its real rail pin must
        // face its band (vcc → screen-up) so the VCC glyph lands on the
        // body *exterior*, not buried under the resistor. The filtered set
        // is therefore a strict subset of the eight, and every survivor
        // satisfies V14.
        let (refdes, allowed) =
            allowed_str("test\nV1 vcc 0 12 ;@ power=vcc\nR1 vcc out 1k\n.end\n");
        let prefs = {
            let file_id = FileId(0);
            let parsed = spice_parser::parse(
                "test\nV1 vcc 0 12 ;@ power=vcc\nR1 vcc out 1k\n.end\n",
                file_id,
            )
            .expect("parse")
            .netlist;
            let resolved =
                spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
            let (checked, _w) = check(resolved).expect("policy check failed");
            vertical_prefs(&checked)
        };
        let i = idx_of(&refdes, "R1");
        assert!(
            allowed[i].len() < 8 && !allowed[i].is_empty(),
            "a 2-pin rail consumer must be V14-filtered to a non-empty subset, got {}",
            allowed[i].len()
        );
        // Reconstruct R1 to assert every survivor satisfies V14.
        let file_id = FileId(0);
        let parsed = spice_parser::parse(
            "test\nV1 vcc 0 12 ;@ power=vcc\nR1 vcc out 1k\n.end\n",
            file_id,
        )
        .expect("parse")
        .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let (checked, _w) = check(resolved).expect("policy check failed");
        let r1 = checked
            .elements
            .iter()
            .find(|e| e.refdes == "R1")
            .expect("R1");
        for &o in &allowed[i] {
            assert!(
                satisfies_v14(r1, &prefs, o),
                "filtered orientation {o:?} does not satisfy V14"
            );
        }
    }

    #[test]
    fn two_pin_signal_only_element_keeps_full_set() {
        // A 2-pin element with NO rail pin (pure signal) keeps all eight
        // orientations — V14 carries no information for it, and the seed
        // candidate set must stay byte-identical to prior behaviour.
        let (refdes, allowed) =
            allowed_str("test\nV1 in 0 AC 1\nR1 in out 1k\nC1 out mid 1u\n.end\n");
        let i = idx_of(&refdes, "C1");
        assert_eq!(allowed[i].len(), 8);
    }

    #[test]
    fn two_pin_power_source_keeps_full_set() {
        // A 2-pin power SOURCE stays exempt: its body is replaced by a
        // glyph oriented by the rails decoration stub.
        let (refdes, allowed) =
            allowed_str("test\nV1 vcc 0 12 ;@ power=vcc\nR1 vcc out 1k\n.end\n");
        let i = idx_of(&refdes, "V1");
        assert_eq!(allowed[i].len(), 8);
    }

    #[test]
    fn opamp_identity_is_v14_feasible() {
        // X1: vcc on pin 8 (lib-up), vee (negative rail) on pin 4
        // (lib-down). Identity satisfies both; rot 90 makes both
        // sideways and must be excluded.
        let src = "test\n\
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
        let (refdes, allowed) = allowed_str(src);
        let i = idx_of(&refdes, "X1");
        assert!(allowed[i].contains(&Orientation::IDENTITY));
        // No allowed orientation may be R90/R270 (those rotate the
        // vertical power pins to horizontal).
        assert!(
            allowed[i]
                .iter()
                .all(|o| matches!(o.rotation, Rotation::R0 | Rotation::R180)),
            "allowed opamp orientations had a 90/270 rotation: {:?}",
            allowed[i]
        );
        // And R180 is excluded (would put V+ down, V- up).
        assert!(allowed[i].iter().all(|o| o.rotation != Rotation::R180));
    }
}
