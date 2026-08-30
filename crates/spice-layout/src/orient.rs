//! The per-element allowed-orientation set: the V14 power-glyph
//! constraint and the V17 signal-direction constraint, one hard filter.
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
//!
//! **V17 (`--placer=signal-direction`) is the second filter here, and it
//! is orthogonal to V14 by construction.** V14 constrains the *vertical*
//! axis; a KiCad `(mirror y)` flips only `x`, so a mirrored opamp still
//! has V+ up and V− down and is V14-legal. Nothing in the tree governed a
//! directional device's *horizontal* axis, so the SA mirrored one freely
//! whenever it shortened a wire — `opamp_inverting_real` and
//! `sallen_key_lpf` both ship a `rot 0` + `mirror y` opamp on the default
//! placer. [`satisfies_signal_direction`] is that missing filter: a symbol
//! carrying at least one `Output` pin *and* at least one `Input` pin must
//! be posed with its outputs right of its inputs. See `docs/invariants.md`
//! V17 and ADR-33.

use kicad_symbols::{Orientation, PinElectrical};
use spice_policy::CheckedNetlist;

use crate::net_class::{VertPref, vertical_prefs};
use crate::placer::Placer;

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

/// Mean symbol-frame `x` of an element's `Input` pins and of its
/// `Output` pins, in `orient` — or `None` when the element is **exempt**
/// from V17 because one of the two groups is empty.
///
/// Symbol-frame `x` is the right quantity even though V17 is stated in
/// world coordinates: the element's origin is the same for every
/// candidate orientation, so `origin.x + p.x` and `p.x` order the
/// candidates identically.
///
/// **Why the mean and not an extreme.** "The input side" of an amplifier
/// is a *cluster* — an opamp has two inputs at one `x`, a gate has `n` —
/// so the group statistic must be insensitive to how many pins each group
/// has. A strict `max(input x) < min(output x)` separation test agrees
/// with the mean on a clean symbol, but becomes **unsatisfiable** for any
/// symbol carrying one off-side pin (an enable input drawn on the bottom
/// edge, whose `x` sits mid-body); an unsatisfiable hard filter degrades
/// to the empty-set fallback below and loses the constraint entirely
/// rather than merely relaxing it.
fn directional_pin_means(
    elem: &spice_resolve::ResolvedElement,
    orient: Orientation,
) -> Option<(f64, f64)> {
    let pins = elem.symbol.pins_in(orient);
    let mut ins: Vec<f64> = Vec::new();
    let mut outs: Vec<f64> = Vec::new();
    for p in &pins {
        match p.electrical {
            PinElectrical::Input => ins.push(p.x),
            PinElectrical::Output => outs.push(p.x),
            _ => {}
        }
    }
    if ins.is_empty() || outs.is_empty() {
        // Exempt: the symbol carries no left-to-right reading direction.
        // `Device:Q_NPN_BCE` is exactly this case — one `input` base and
        // two `passive` pins — and mirroring a BJT is a legitimate and
        // common drawing choice, so V17 must not touch it.
        return None;
    }
    #[allow(clippy::cast_precision_loss)] // pin counts are tiny
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    Some((mean(&ins), mean(&outs)))
}

/// True when `orient` satisfies V17: the element's output pins lie to the
/// **right** of its input pins. Elements lacking either pin group are
/// exempt and trivially satisfy it.
///
/// The inequality is **strict**, so a pose whose two group means coincide
/// — a 90°/270° rotation of a horizontally-drawn amplifier, whose output
/// then points down — is a violation. Such a pose establishes no
/// left-to-right reading at all, which is exactly what V17 asserts.
fn satisfies_signal_direction(elem: &spice_resolve::ResolvedElement, orient: Orientation) -> bool {
    match directional_pin_means(elem, orient) {
        None => true,
        Some((mean_in, mean_out)) => mean_out > mean_in + f64::EPSILON,
    }
}

/// Per-element allowed-orientation set for the V14 and V17 hard
/// constraints.
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
///
///   Then, under `--placer=signal-direction` only, that set is narrowed
///   again to the poses satisfying V17 ([`satisfies_signal_direction`]),
///   and **the narrowing is dropped if it would empty the set**.
/// * Order within each set follows [`Orientation::ALL`], so callers'
///   deterministic tie-breaks are preserved.
///
/// **Precedence when the two conflict.** V14 is applied first and V17
/// relaxes into it, never the other way round: V14 carries a documented
/// escape for an infeasible element (the detached-glyph stub) where V17
/// has none, so relaxing V17 loses less. Neither fallback branch is
/// reached by any fixture today — the suite's one directional symbol
/// (`Amplifier_Operational:OPAMP`) has a V14 ∩ V17 intersection of
/// exactly one pose, [`Orientation::IDENTITY`].
#[must_use]
pub fn allowed_orientations(checked: &CheckedNetlist, placer: Placer) -> Vec<Vec<Orientation>> {
    allowed_and_repair_orientations(checked, placer).0
}

/// [`allowed_orientations`] together with the **Tier-0 repair set** it was
/// narrowed from.
///
/// Returns `(allowed, repair_allowed)`, both indexed by element:
///
/// * `allowed[i]` is exactly what [`allowed_orientations`] returns — the
///   V14 set, then narrowed by V17 when the placer arms that filter. It
///   binds every ordinary stage: the V5 seed chooser, the SA rotate /
///   mirror moves, and phase 4.5's normal search.
/// * `repair_allowed[i]` is the **V14 set alone**, before any V17
///   narrowing. It is `allowed[i]` itself on every placer that does not
///   arm the V17 filter, which is every shipping path today.
///
/// **Why the wider set exists (ADR-37).** V17 is a hard *candidate
/// filter*, and every pose a hard filter removes is a pose phase 4.5's
/// Tier-0 repair cannot use. On `sallen_key_lpf` at SA seed 1 under
/// `--placer=readable-v1` that composed with the divider construction's
/// extra pinning to make the repair space empty: the placement arrived
/// with one severed net, the pose that reconnects it is `X1` mirrored,
/// V17 had removed exactly that pose, and the CLI refused the conversion
/// on ADR-22's net-partition certificate.
///
/// So phase 4.5 — and *only* phase 4.5, and *only* while it is repairing
/// an already-Tier-0-broken placement — may select from `repair_allowed`,
/// accepting a pose outside `allowed` on a strict improvement of the
/// Tier-0 prefix and on nothing else. This is the same precedence this
/// module already declares for the static conflict ("V14 wins and V17
/// relaxes, because V14 carries a documented escape and V17 has none"),
/// extended from *static* infeasibility (an empty V14 ∩ V17) to *dynamic*
/// infeasibility (no V17-legal pose reconnects the net) — a fact only the
/// router can establish, which is why it lives at phase 4.5.
///
/// **V14 is not in the wider set and never becomes liftable**: it keeps
/// its own detached-glyph escape, so nothing here needs to relax it.
#[must_use]
pub fn allowed_and_repair_orientations(
    checked: &CheckedNetlist,
    placer: Placer,
) -> (Vec<Vec<Orientation>>, Vec<Vec<Orientation>>) {
    let prefs = vertical_prefs(checked);
    checked
        .elements
        .iter()
        .map(|elem| {
            let v14_set = v14_allowed(elem, &prefs);
            if !placer.signal_direction_filter() {
                return (v14_set.clone(), v14_set);
            }
            let directed: Vec<Orientation> = v14_set
                .iter()
                .copied()
                .filter(|&o| satisfies_signal_direction(elem, o))
                .collect();
            if directed.is_empty() {
                (v14_set.clone(), v14_set)
            } else {
                (directed, v14_set)
            }
        })
        .unzip()
}

/// The V14 half of [`allowed_orientations`], for one element.
///
/// Split out of the closure so the V17 narrowing composes onto the
/// *whole* V14 result — including its `<= 2`-terminal exemption. Folding
/// V17 in beside that early return instead would let a two-pin
/// directional part (a buffer with one `input` and one `output` and no
/// rail pin) take the exemption and escape V17 entirely.
fn v14_allowed(
    elem: &spice_resolve::ResolvedElement,
    prefs: &std::collections::HashMap<String, VertPref>,
) -> Vec<Orientation> {
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
        .filter(|&o| satisfies_v14(elem, prefs, o))
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
}

/// Audit the V14 **consistency requirement** over the seed idioms.
///
/// CLAUDE.md: *"A property enforced as a hard constraint at the
/// seeding/placement stage MUST be hard at every stage that can move the
/// element."* Three stages filter orientations against
/// [`allowed_orientations`] — [`crate::pick_orientations`], the SA rotate
/// move, and phase 4.5 — and **all three skip pinned elements**. So a seed
/// pass that pins an element also freezes its orientation past every V14
/// enforcer: whatever it chose is what gets emitted.
///
/// This asserts the closing half of that contract: every element a *seed
/// pass* pinned must already hold a V14-allowed orientation. It caught a
/// real defect — `apply_series_horizontal` re-columning a positive-supply
/// bias resistor below its node with the rail pin facing down, pinning a
/// pose `allowed_orientations` would have excluded.
///
/// `externally_pinned` is the mask *before* the seed idioms ran: user
/// `*@place` / `*@align` and cache-hint pins are exempt, since their
/// orientation is caller data rather than a seed pass's choice.
///
/// Debug-only. In release this is a no-op; the passes themselves carry the
/// always-on filter.
pub(crate) fn debug_assert_seed_pins_satisfy_v14(
    placement: &crate::Placement,
    pinned: &[bool],
    externally_pinned: &[bool],
    allowed: &[Vec<Orientation>],
    checked: &CheckedNetlist,
) {
    if !cfg!(debug_assertions) {
        return;
    }
    let offenders: Vec<String> = placement
        .elements
        .iter()
        .enumerate()
        .filter(|(i, _)| pinned.get(*i) == Some(&true) && externally_pinned.get(*i) != Some(&true))
        .filter(|(i, el)| {
            allowed
                .get(*i)
                .is_some_and(|set| !set.is_empty() && !set.contains(&el.orientation))
        })
        .map(|(i, el)| {
            let refdes = checked
                .elements
                .get(i)
                .map_or_else(|| format!("#{i}"), |e| e.refdes.clone());
            format!(
                "{refdes} pinned at {:?} (allowed: {:?})",
                el.orientation, allowed[i]
            )
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "seed idioms pinned a V14-forbidden orientation, which no later \
         stage can repair (pinned elements are skipped by \
         `pick_orientations`, the SA rotate move and phase 4.5): {}",
        offenders.join("; ")
    );
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
        allowed_str_with(src, Placer::default())
    }

    fn allowed_str_with(src: &str, placer: Placer) -> (Vec<String>, Vec<Vec<Orientation>>) {
        let file_id = FileId(0);
        let parsed = spice_parser::parse(src, file_id)
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let (checked, _warns) = check(resolved).expect("policy check failed");
        let refdes = checked.elements.iter().map(|e| e.refdes.clone()).collect();
        (refdes, allowed_orientations(&checked, placer))
    }

    /// As [`allowed_str_with`], returning BOTH sets
    /// (`allowed`, `repair_allowed`).
    fn repair_str_with(
        src: &str,
        placer: Placer,
    ) -> (Vec<Vec<Orientation>>, Vec<Vec<Orientation>>) {
        let parsed = spice_parser::parse(src, FileId(0)).expect("parse").netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let (checked, _w) = check(resolved).expect("policy check failed");
        allowed_and_repair_orientations(&checked, placer)
    }

    /// The opamp source used by the V14 and V17 orientation tests.
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

    /// V14 is blind to the mirror: the pose the owner objected to —
    /// `rot 0` + `(mirror y)` — survives the default placer's filter,
    /// because a `(mirror y)` flips only `x` and leaves V+ up / V− down.
    /// This is the defect V17 exists to close, asserted rather than
    /// assumed.
    #[test]
    fn the_default_filter_still_admits_a_mirrored_opamp() {
        let (refdes, allowed) = allowed_str(OPAMP_SRC);
        let i = idx_of(&refdes, "X1");
        assert!(
            allowed[i].iter().any(|o| o.mirror_y),
            "V14 alone should still admit a mirrored opamp: {:?}",
            allowed[i]
        );
    }

    /// Under `--placer=signal-direction` the V17 narrowing removes every
    /// mirrored pose, leaving the opamp with exactly one candidate.
    ///
    /// This is also the evidence that the SA's **MirrorY** proposal is
    /// gated, not just its rotate: `solver::anneal` force-rejects any
    /// proposal whose result is outside `allowed[idx]`, and
    /// `Proposal::reorients()` returns the index for `MirrorY` as well as
    /// `Rotate`. With no mirrored pose in the set, no mirror can survive.
    #[test]
    fn signal_direction_excludes_every_mirrored_opamp_pose() {
        let (refdes, allowed) = allowed_str_with(OPAMP_SRC, Placer::SignalDirection);
        let i = idx_of(&refdes, "X1");
        assert_eq!(
            allowed[i],
            vec![Orientation::IDENTITY],
            "V14 ∩ V17 should leave the opamp exactly one pose"
        );
        assert!(allowed[i].iter().all(|o| !o.mirror_y));
    }

    /// **ADR-37 — the Tier-0 repair set is exactly V14, and it equals
    /// `allowed` on every placer that does not arm the V17 filter.**
    ///
    /// Both halves are the safety argument for the phase-4.5 widening:
    /// the second is why the shipping path is *structurally* inert (no
    /// widened set exists to select from), and the first is why the
    /// widening cannot relax V14 — the repair set is the V14 survivors,
    /// not the full eight.
    #[test]
    fn the_repair_set_is_v14_and_matches_allowed_off_the_v17_filter() {
        for placer in [
            Placer::FlowSeedV4,
            Placer::FlowSeed,
            Placer::Champion,
            Placer::TerminalSeriesDivider,
            Placer::FacingTrigger,
        ] {
            let (refdes, _) = allowed_str_with(OPAMP_SRC, placer);
            let (allowed, repair) = repair_str_with(OPAMP_SRC, placer);
            assert_eq!(
                allowed,
                repair,
                "{}: `repair_allowed` must equal `allowed` when V17 is off",
                placer.name()
            );
            // And on this fixture V14 is a strict filter, so equality is
            // not the trivial "both are all eight".
            let i = idx_of(&refdes, "X1");
            assert!(allowed[i].len() < 8, "{:?}", allowed[i]);
        }

        // Under the V17 filter the two diverge on exactly the mirrored
        // poses V17 removes — and the repair set is still V14-filtered.
        let (refdes, _) = allowed_str_with(OPAMP_SRC, Placer::SignalDirection);
        let (allowed, repair) = repair_str_with(OPAMP_SRC, Placer::SignalDirection);
        let i = idx_of(&refdes, "X1");
        assert_eq!(allowed[i], vec![Orientation::IDENTITY]);
        assert!(
            repair[i].len() > allowed[i].len(),
            "the repair set must be strictly wider here: {:?}",
            repair[i]
        );
        assert!(
            repair[i].iter().any(|o| o.mirror_y),
            "and it must contain the mirrored pose V17 removed: {:?}",
            repair[i]
        );
        // V14 is NOT lifted: no 90/270 rotation and no R180 survives,
        // exactly as `opamp_identity_is_v14_feasible` asserts for
        // `allowed`.
        assert!(
            repair[i].iter().all(|o| o.rotation == Rotation::R0),
            "the repair set must still be V14-filtered: {:?}",
            repair[i]
        );
    }

    /// V17 exempts a symbol lacking one of the two pin groups.
    /// `Device:Q_NPN_BCE` carries one `input` (the base) and two
    /// `passive` pins, so it has no left-to-right reading direction and
    /// mirroring it is a legitimate drawing choice. Its allowed set must
    /// be **identical** under both placers.
    #[test]
    fn signal_direction_is_inert_for_a_symbol_with_no_output_pin() {
        let src = "test\n\
            *@symbol Device:Q_NPN_BCE for=Q1\n\
            VCC vcc 0 DC 12 ;@ power=vcc\n\
            RC vcc c 4k7\n\
            RB vcc b 100k\n\
            RE e 0 1k\n\
            Q1 c b e QGENERIC\n\
            .end\n";
        let (refdes, base) = allowed_str_with(src, Placer::default());
        let (_, narrowed) = allowed_str_with(src, Placer::SignalDirection);
        let i = idx_of(&refdes, "Q1");
        assert_eq!(base[i], narrowed[i]);
        assert!(base[i].iter().any(|o| o.mirror_y), "{:?}", base[i]);
    }
}
