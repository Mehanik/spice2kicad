//! Baseline lock: snapshots every fixture's `(symbol …)` instances as
//! `(refdes, lib_id, x, y, rot, mirror)` tuples. Used as a safety net
//! for surgical layout changes: any unintended movement in any element
//! of any fixture trips the assertion. (V14 note: for `Device:R_US`
//! with the power net on terminal 0, rot 0 places the VCC pin
//! screen-up — the V14-correct orientation, as `common_emitter`'s `RC`
//! and the diff_pair / multivibrator collector resistors all show.)
//!
//! To intentionally update a single line, edit the BASELINE entry
//! below — do **not** widen the comparison or skip elements.
//!
//! # Regenerating the whole table
//!
//! `baseline_lock` is a **movement detector, not a ratchet**: it has no
//! notion of better or worse, so regenerating it wholesale after a
//! deliberate layout change is legitimate (unlike a budget literal,
//! which may only ever ratchet down — see CLAUDE.md § "Budgets are
//! ratchets, not knobs"). What it is *not* is safe to regenerate by
//! hand or by script: the table is 100+ rustfmt-wrapped multi-line
//! rows, and a scripted splice has already silently left nine stale
//! entries behind, producing a baffling "9 rows MISSING in actual"
//! failure that cost real debugging time.
//!
//! So there is a dump hook. Run:
//!
//! ```sh
//! S2K_BASELINE_DUMP=1 cargo test -p spice2kicad --test baseline_lock -- --nocapture
//! ```
//!
//! It measures every fixture and prints the `BASELINE` table to stdout
//! in exactly this file's source syntax (one row per line), then
//! **skips the comparison** so the test cannot fail on the very drift
//! you are recording. Replace the whole `const BASELINE … ];` block
//! with the printed text and run `cargo fmt` — rustfmt re-wraps the
//! long rows. Copy-paste, no scripted surgery. (Verified: the rows
//! round-trip byte-identically through dump → paste → `cargo fmt`.)
//!
//! One thing the dump does **not** carry: the `//` regeneration-history
//! comments *inside* the array literal. Keep them — paste the printed
//! rows and re-add that comment block, appending your own note for why
//! this regeneration happened.
//!
//! Always confirm *why* rows moved before pasting: an unexplained
//! movement is the regression this file exists to catch. Record the
//! reason in the `BASELINE` doc comment, as every prior regeneration
//! has.

// Pedantic lints relaxed for this S-expression-parsing test harness:
// `car`/`cdr` and `s`/`x` are the conventional cons-cell names;
// `as_str`'s two `Some(s)` arms are intentionally distinct match
// patterns; the final `if !empty { panic! }` reads clearer than a
// formatted `assert!`.
#![allow(clippy::similar_names, clippy::match_same_arms, clippy::manual_assert)]

mod common;

use std::path::PathBuf;

use common::spice_to_kicad;
use lexpr::Value;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> common::TempDir {
    common::TempDir::new("baseline", name)
}

fn list_iter(v: &Value) -> impl Iterator<Item = &Value> {
    let mut cur = v;
    std::iter::from_fn(move || match cur {
        Value::Cons(c) => {
            let (car, cdr) = c.as_pair();
            cur = cdr;
            Some(car)
        }
        _ => None,
    })
}

fn first_atom(v: &Value) -> Option<&str> {
    list_iter(v).next().and_then(|x| match x {
        Value::Symbol(s) => Some(&**s),
        _ => None,
    })
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn as_str(v: &Value) -> Option<&str> {
    match v {
        Value::String(s) => Some(s),
        Value::Symbol(s) => Some(s),
        _ => None,
    }
}

/// Returns `(refdes, lib_id, x, y, rot, mirror)` tuples for every
/// top-level `(symbol …)` instance in the schematic.
fn extract_symbols(path: &std::path::Path) -> Vec<(String, String, f64, f64, f64, String)> {
    let src = std::fs::read_to_string(path).expect("read sch");
    let root = lexpr::from_str(&src).expect("parse sch");
    let mut out = Vec::new();
    for child in list_iter(&root) {
        if first_atom(child) != Some("symbol") {
            continue;
        }
        let mut lib_id = String::new();
        let mut x = 0.0;
        let mut y = 0.0;
        let mut rot = 0.0;
        let mut mirror = String::new();
        let mut refdes = String::new();
        for sub in list_iter(child).skip(1) {
            match first_atom(sub) {
                Some("lib_id") => {
                    if let Some(s) = list_iter(sub).nth(1).and_then(as_str) {
                        lib_id = s.to_string();
                    }
                }
                Some("at") => {
                    let parts: Vec<&Value> = list_iter(sub).skip(1).collect();
                    if let Some(v) = parts.first().and_then(|v| as_f64(v)) {
                        x = v;
                    }
                    if let Some(v) = parts.get(1).and_then(|v| as_f64(v)) {
                        y = v;
                    }
                    if let Some(v) = parts.get(2).and_then(|v| as_f64(v)) {
                        rot = v;
                    }
                }
                Some("mirror") => {
                    if let Some(s) = list_iter(sub).nth(1).and_then(|v| match v {
                        Value::Symbol(s) => Some(&**s),
                        _ => None,
                    }) {
                        mirror = s.to_string();
                    }
                }
                Some("property") => {
                    let parts: Vec<&Value> = list_iter(sub).skip(1).collect();
                    if parts.first().and_then(|v| as_str(v)) == Some("Reference") {
                        if let Some(s) = parts.get(1).and_then(|v| as_str(v)) {
                            refdes = s.to_string();
                        }
                    }
                }
                _ => {}
            }
        }
        if !refdes.is_empty() {
            out.push((refdes, lib_id, x, y, rot, mirror));
        }
    }
    out.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    out
}

/// The recorded baseline of every emitted top-level `(symbol …)`
/// instance. Updating any tuple requires deliberation: it implies a
/// layout change. Add a comment when you change one.
///
/// All coordinates here reflect the V15 page-translation pass: the
/// emitter shifts every sheet's content bounding box so its top-left
/// corner lands at `PAGE_MARGIN_MM` (25.4 mm). The translation is a
/// single uniform grid-snapped offset, so every *relative* geometry
/// (rotation, mirror, inter-element spacing) is preserved — only the
/// absolute origins move, all to non-negative coordinates inside the
/// A4 drawable area. (Regenerated when V13(4) hid the `#PWRn`
/// Reference and nudged colliding property text, and again for V13(5)
/// when the nudge pass began clearing symbol-internal pin-name/number
/// text too: those decoration changes shifted some sheets' content
/// bbox, so the V15 offset moved by a single per-fixture delta — here
/// `diff_pair` shifted uniformly by +7.62 mm in X. Symbol poses
/// relative to one another are unchanged.)
///
/// Regenerated again for R-5 (V6/V14 rail-pin facing): 2-pin rail
/// *consumers* (`RC`/`R1` on `vcc`, `RE`/`R2`/`CE` on ground in
/// `common_emitter`; `C1` on ground in `rc_lowpass`; `RTAIL` on `vee`
/// in `diff_pair`) are now orientation-filtered so their rail pin faces
/// its band — flipping their `(mirror …)` / rotation and rippling the
/// SA-refined neighbour positions. No budget changed (this is a
/// snapshot, not a budget); every V5/V6/V11/V12/V13/V14 verifier stays
/// green.
///
/// Regenerated again for the V13 power-glyph-text / PWR_FLAG fix: (a) the
/// router now anchors each `power:*` glyph's net-name Value text on the
/// pin's *outward* side, whose upward reach (VCC/VEE) extends the content
/// bbox so the V15 page-translate offset shifts some sheets by one
/// per-fixture delta; (b) each `power:PWR_FLAG` is now co-located with
/// the rail glyph it drives but rotated 0/180 to point its chevron *away*
/// from the glyph body (rot follows GND-down vs VCC/VEE-up), so the
/// `#FLG*` rows move onto their `#PWR*` coordinate and flip rotation. All
/// relative poses are otherwise preserved; every verifier stays green.
///
/// Regenerated again for the Phase-1 class-aware PWR_FLAG pruning: a
/// PWR_FLAG is no longer emitted on a *signal* net that carries a
/// `Passive` (resistor/cap) terminal, because KiCad's `DrivingPinTypes`
/// counts `PT_PASSIVE` as a valid signal-net driver — so those flags
/// were redundant. This removed the spurious signal-net flags on
/// `common_emitter` (net `b`), `multivibrator` (two base nets), and
/// `opamp_inverting_real` (one feedback node), renumbering the surviving
/// `#FLG*` rows on those three sheets (fewer flag rows = less geometry;
/// ratchet direction DOWN). No symbol moved — the removed flags were
/// co-located with an existing rail glyph/pin, so the V15 content bbox
/// and every other pose are unchanged. Rail flags (one per global rail)
/// and genuinely input-only signal nets (`diff_pair` `in1`/`in2`,
/// base-only with no passive pin) are preserved; ERC stays 0 errors.
///
/// Regenerated again for the **ADR-23 promotion of `--placer=flow-seed`
/// to the default** (owner-approved, 2026-08-18). This is a
/// whole-placer swap, not a decoration change: the X layering now
/// measures depth along the DC signal path instead of hops from the
/// nearest power rail, so 157 of 282 rows moved across ten fixtures.
/// Eight fixtures are byte-identical, five of them load-bearingly so —
/// `diff_pair`, `multivibrator` and `wien_bridge_osc` are rootless and
/// take the old rail-rooted policy verbatim, and `lc_ladder_lpf` and
/// `sallen_key_driven` already had real signal sources — which is the
/// cheapest available check that the fallback survived the swap.
/// Regenerated again when rail `PWR_FLAG` markers moved back onto the
/// circuit. The bottom-right driver block is gone: each rail's flag now
/// stacks on an existing in-circuit `power:*` glyph anchor, chosen by
/// `spice_route::pwrflag::choose_rail_anchor`. 78 power rows changed (39
/// synthesised corner glyphs removed, 39 `#FLG*` relocated) and **zero
/// circuit rows** — no symbol moved and no page-fit pan occurred, because
/// the block hung off the bottom-right while `translate_into_page`
/// anchors the top-left.
const BASELINE: &[(&str, &str, &str, f64, f64, f64, &str)] = &[
    // (fixture, refdes, lib_id, x, y, rot, mirror), sorted by
    // (fixture, refdes) to match the verifier's own ordering.
    //
    // Regenerated wholesale rather than patched: the rail `PWR_FLAG`
    // markers moved off the circuit into a bottom-right driver block
    // (each paired with its own `power:*` glyph), so the element SET
    // changed, not merely coordinates. Absolute positions also shifted
    // when the page frame began reserving property-text room
    // symmetrically — see the V15 note in docs/invariants.md.
    //
    // Regenerated again when the collinear outward stub was restored to
    // the Steiner stage (V5). Wires changed on most fixtures, which
    // moved the content bbox and so the V15 page-translate offset;
    // phase 4.5 also re-picked a few orientations now that its
    // router-in-the-loop oracle sees the outward routes again (notably
    // `rc_lowpass_ports` R1 → rot 180). No element was repositioned by
    // hand; every verifier is green.
    //
    // This is a SNAPSHOT, not a ratchet: it catches *accidental*
    // movement. Regenerate it deliberately when geometry changes for a
    // reason you can name — never to make a quality budget pass.
    //
    // Regenerated again after: (a) the rail-stub column idiom + the
    // un-inverted `cost::rail_direction` moved most elements, and (b)
    // rail glyphs began keying off the declared `*@power=` tag rather
    // than the net's spelling (`common_emitter`'s VCC glyph is now
    // `power:+12V`). Wholesale, not patched: 107 of 107 rows changed.
    //
    // Regenerated again when `Symbol::pins_in` stopped reporting
    // horizontal pins' outward direction backwards. That angle feeds the
    // router's outward stubs AND phase 4.5's V5 oracle, so orientations
    // moved on the fixtures with horizontal pins (opamp inputs/outputs,
    // `rc_lowpass_ports` R1 back to rot 0). True V5 violations summed
    // across fixtures fell 16 → 8; V16 (B, J) per fixture is unchanged
    // on 7 of 9 — see the commit message for the two exceptions.
    //
    // Regenerated again for two layout changes landing together:
    //
    // (a) `spice_layout::idioms::apply_shared_centers` now seats the
    //     centred passive one grid cell BELOW the clearance stride, so
    //     its shared-net pin no longer lands on the trunk row the router
    //     picks. Only `diff_pair` has the idiom; its `RTAIL` and the five
    //     power glyphs / flags whose column follows it moved +1.27 mm in
    //     Y. Buys `diff_pair` V5 1 → 0 at V16 J 0 → 1 (a three-way node
    //     drawn as a proper Steiner T instead of the trunk ending
    //     sideways on a pin).
    //
    // (b) Phase 4.5's acceptance objective gained the V16 ink-graph bend
    //     count as its FINAL lexicographic key, after (V13, V12, V5), so
    //     the refiner now separates orientations that tie on every
    //     higher-tier count by how straight the resulting ink is. This
    //     re-picked orientations on `rc_lowpass_ports` (R1 → rot 180,
    //     B 4 → 2) and `common_emitter` (B 10 → 4). See ADR-16 "Accepted
    //     extension" and invariants.md V16 for why a last-place
    //     lexicographic key cannot trade against Tier 1.
    //
    // V5 is unchanged or lower on every fixture; no Tier-0/Tier-1 count
    // moved anywhere. Only `opamp_definition_level` B rose (10 → 12), on
    // the owner-approved global-improvement escape.
    //
    // Regenerated again for the rail-stub column idiom's symmetry unlock.
    // Idiom 4 was a total no-op on any circuit V7 symmetry pinned, because
    // a group containing a pinned member is skipped wholesale — so the
    // collector-load column fix that landed for `common_emitter` was
    // silently excluded from every symmetric fixture. It now releases a
    // V7-ONLY pin (V7 owns a pair's mirror RELATION, not either member's
    // absolute column) and re-mirrors afterwards, and a released group
    // moves only on a STRONG anchor — an active device's own vertical pin.
    // `multivibrator`'s `RC1`/`RC2` drop onto their transistors' collector
    // columns, removing 17.78 mm of dog-leg each; their VCC glyphs follow.
    // 4 of 107 rows moved and no other fixture changed at all.
    //
    // Regenerated again for the `no_source_fallback` root refinement in
    // `layers.rs`. A power-touching element was rooted at layer 0
    // unconditionally, so the opamp `X1` — which touches `vcc` only for
    // its supply — was seeded level with the circuit's true input `RIN`.
    // The router answered by MIRRORING X1 so its output faced back left.
    // Roots are now restricted to genuine rail stubs (power-touching AND
    // at most one Signal net); an element on two or more Signal nets is
    // an interior node and takes its layer from the BFS. 12 of 107 rows
    // moved, all on `opamp_inverting_real`; no other fixture changed.
    //
    // Regenerated again for the rail-stub OUTWARD anchor. The column
    // idiom declined outright when a stub's node presented only
    // sideways-facing pins (a bias resistor feeding a transistor BASE),
    // leaving the stub at whatever column the layer seeder gave it. It
    // now takes the column one geometry-derived stride along such a
    // pin's OUTWARD direction and reaches the pin with a short run in —
    // the conventional drawing, and a different proposal from the
    // measured-and-rejected "anchor AT the pin, offset zero". A node
    // carrying stubs on BOTH sides is a divider through the node and is
    // deliberately excluded (it already shares one column). 16 of 107
    // rows moved, all on `multivibrator` (RB1/RB2 in, everything else
    // following the V15 page re-anchor); no other fixture changed.
    //
    // Regenerated again for the ADR-14 completion: a rail glyph's
    // net-name Value text is CENTRED on its anchor (confirmed against
    // `kicad-cli sch export svg` ink — a "GND" label anchored at x=25.40
    // renders x[23.71, 27.09]), so on a HORIZONTALLY-facing rail pin
    // roughly half the string used to lie outside the reserved zone.
    // `glyph_reach` now reserves that text's full rendered box on
    // horizontal pins, in BOTH consumers (seed/align stride and the SA
    // overlap gate), so the two cannot disagree.
    //
    // Blast radius is one fixture: `opamp_inverting_real` shifts its
    // right-hand cluster +2.54 mm in X (one grid cell — the reserved
    // half-width, grid-snapped). 11 of 109 rows; the other eight
    // fixtures are byte-identical. Pure spacing: no rotation, no mirror,
    // no reordering. V16 (B, J) unchanged on every fixture, and every
    // other ratchet is unchanged — consistent with ADR-14's finding that
    // a faithful reservation buys no observable quality until something
    // removes the slack.
    //
    // Regenerated again for the multi-channel layout fix: numbered
    // channel ports (`in1`/`out2`) now read as circuit boundaries, the
    // well-formedness fallback no longer re-admits interior opamps as
    // layer-0 roots, V7 mirrors only genuinely COUPLED halves, and the
    // seed's within-bucket Y stride is geometry-derived for a bucket
    // stacking two oversized bodies. Blast radius is ONE fixture:
    // 18 of 109 rows, all `opamp_definition_level`; the other nine
    // fixtures are byte-identical. F5 4 → 1 (both channels now layer
    // left-to-right instead of backwards and X-interleaved); every
    // Tier-0 and Tier-1 verifier is green. V16 (B, J) is unchanged on
    // every fixture EXCEPT `opamp_definition_level`, whose B rises
    // 12 → 15 — an UNAPPROVED Tier-2 ratchet rise, which is why this
    // sits on a branch and not on master.
    //
    // Regenerated again for the channel-row banding (Option B; channels.rs
    // + spice-layout::lib.rs). The two independent inverting-amp channels
    // are laid out as two CONGRUENT rows and each channel's orientation is
    // pinned THROUGH phase 4.5 to the textbook seed facing (input-left,
    // output-right), so the deck reads left-to-right. Blast radius is again
    // ONE fixture: all 18 `opamp_definition_level` rows change (both opamps
    // now rot 0 with NO mirror — the old baseline mirrored X2 `y` and drew
    // RF at rot 270 in a diagonal sprawl); the other nine fixtures are
    // byte-identical. On this fixture, summed violations fall by 6:
    // B 15 → 6, F5 → 0, wire_detour 1.0984 → 1.0732, crossings 0; at the
    // owner-approved cost (OWNER SIGN-OFF 2026-07-20, global-improvement
    // escape) of V5 0 → 2 (the two summing-node input pins facing the RF
    // feedback junction) and V16 J 0 → 2 (proper Steiner branches on the
    // two 3-pin nets). This B 15 → 6 SUPERSEDES the earlier unratified
    // B = 15. Every Tier-0 and Tier-1 verifier is green.
    // --- F2 (v0.2 roadmap, second benchmark wave): four fixtures
    // INSERTED in the alphabetical order `baseline_lock_all_fixtures`
    // iterates. Every pre-existing row below is byte-identical — the
    // diff for this commit deletes nothing from this table.
    //
    // `cascode_amp`: a CE stage under a common-base stage, i.e. a
    // circuit whose structure is a COLUMN. The placer has no stack
    // model, so this is new geometry recorded at its measured values.
    // Extended (NOT regenerated) for the ADR-24 Tier-0 router fix: the
    // two fixtures promoted out of `tests/f0_defects.rs` — `sallen_key_driven`
    // (19 rows) and `shunt_feedback_amp` (16 rows) — are APPENDED in
    // alphabetical position. Every one of the 247 pre-existing rows is
    // byte-identical: the fix is confined to `spice-route`, and ADR-16's
    // protocol demands exactly that of a router change. Verified by set
    // comparison of the dump against the previous table (0 removed,
    // 35 added, all on the two new fixtures), not by eye.
    //
    // Regenerated again for the rail-stub SIDE fix in
    // `idioms::apply_series_horizontal`: the pass re-columned EVERY
    // downstream shunt below its node with the rail pin forced
    // screen-down, a helper written for ground and never parameterised on
    // `RailStub::side`. A positive-supply bias resistor was therefore
    // pinned upside-down, with its `+12V` glyph under the body — and
    // because the pass PINS, that pose bypassed `pick_orientations`, the
    // SA rotate move and phase 4.5, i.e. every stage that enforces the
    // V14 hard constraint. An Up-side stub now rises above the node with
    // the mirrored facing. Blast radius is exactly the two fixtures that
    // have an Up-side re-column: 24 `rc_phase_shift` rows and 16
    // `shunt_feedback_amp` rows; the other sixteen fixtures are
    // byte-identical. Four xfail entries expire (V14 rail-pin on both
    // fixtures, the V14 [3] glyph/body overlap and the rail-band ordering
    // on `rc_phase_shift`). V16 (B, J) is non-increasing on every fixture
    // (ADR-16 protocol); five Tier-2 ratchets rise and are flagged for
    // owner sign-off in the commit message.
    //
    // Regenerated again for the ADR-23 PROMOTION of `--placer=flow-seed`
    // to the default (owner-approved, 2026-08-18). X now measures depth
    // along the DC signal path instead of hops from the nearest power
    // rail, so this is a whole-placer swap and 157 of 282 rows moved on
    // ten fixtures; the other eight are untouched.
    // The five fixtures the new default does not reach — the three
    // rootless ones (`diff_pair`, `multivibrator`, `wien_bridge_osc`,
    // which take the old rail-rooted policy verbatim) and the two
    // already on the principled path (`lc_ladder_lpf`,
    // `sallen_key_driven`) — are byte-identical, verified by `cmp` on
    // the emitted sheets, not inferred. `champion` stays registered as
    // the scoreboard control arm; run it with `--placer champion`.
    // Regenerated again when the rail `PWR_FLAG` markers came back off
    // the bottom-right driver block and onto the circuit. 39 rows
    // vanished (one synthesised `power:*` glyph per rail per fixture,
    // which existed only to give a corner flag something to stack on)
    // and 39 `#FLG*` rows moved onto an existing in-circuit rail-glyph
    // anchor. **Not one non-power row changed, and there was no page-fit
    // pan**: the block sat outward of the BOTTOM-RIGHT of the content
    // bbox, and `translate_into_page` anchors the TOP-LEFT, so deleting
    // it moved no origin. That is the whole diff — 78 power rows, zero
    // circuit rows.
    //
    // Regenerated again for the **SECOND ADR-23 promotion — `--placer=
    // flow-seed-v4` becomes the default** (owner-authorised,
    // 2026-08-24). `layers::assign_x_layers_with` and
    // `idioms::signal_net_depth` now read ONE tiered signal-flow root
    // set (`roots::signal_flow_roots`: declared `*@port …=input` >- drawn
    // source >- leaf-input name >- none) instead of two independently
    // drifted policies.
    //
    // **29 of 243 rows moved, on exactly TWO fixtures** — `lc_ladder_lpf`
    // and `sallen_key_driven`, the only two with a drawn stimulus and
    // therefore the only two whose depth map the old policy left EMPTY.
    // The other sixteen are byte-identical, verified with `diff` on the
    // emitted sheets under `--placer flow-seed` vs the new default, not
    // inferred. That is the cheapest available check that the
    // rootless/rail-rooted fallback survived the swap, and it is a much
    // tighter blast radius than the first promotion's ten fixtures.
    //
    // The headline is `lc_ladder_lpf`, which the owner called
    // "completely mad": `RS`/`L1`/`L2`/`L3` were emitted at rotations
    // 180/90/0/270 on four different rows. They are now all rot 90 on
    // ONE line at y = 35.56 — the textbook doubly-terminated ladder —
    // with the shunt caps hanging below. `flow-seed` and `champion` both
    // stay registered as control arms; run them with `--placer`.
    //
    // --- Regenerated for the THIRD ADR-23 PROMOTION, of
    // `--placer=readable-v1` to the default (owner-authorised
    // 2026-09-04, "Yes, let's promote").
    //
    // A whole-placer swap, so the movement is expected and wholesale:
    // readable-v1 composes four readability arms (V17 signal-direction,
    // terminal-series-divider, divider-rails-strict, facing-trigger) on
    // top of flow-seed-v4, plus ADR-37's Tier-0 V17 escape in phase 4.5.
    //
    // What the motion BUYS, sink-measured on this tree: Tier 0 clean
    // with nothing regressed, Tier 1 -2.00, and nine owner-reported
    // defects repaired with none broken -- vertical VIN/VOUT terminals
    // 9 -> 0, mirrored amplifiers 2 -> 0, two_stage_amp's inverted Q2,
    // port_shapes' split chain.
    //
    // One caution for whoever regenerates this next. The FIRST attempt
    // at this promotion moved the enum's `#[default]` while `--placer`
    // still carried `default_value = "flow-seed-v4"`, so the new default
    // reached no conversion and THIS FILE PASSED UNCHANGED. A green
    // baseline_lock after a promotion is not evidence the promotion is
    // byte-identical; it is equally consistent with the promotion not
    // having happened. Check that the geometry moved before believing
    // that it did not.
    (
        "cascode_amp",
        "#FLG1",
        "power:PWR_FLAG",
        39.37,
        77.47,
        0.0,
        "",
    ),
    (
        "cascode_amp",
        "#FLG2",
        "power:PWR_FLAG",
        49.53,
        31.75,
        180.0,
        "",
    ),
    ("cascode_amp", "#PWR1", "power:GND", 39.37, 77.47, 0.0, ""),
    ("cascode_amp", "#PWR2", "power:GND", 43.18, 85.09, 0.0, ""),
    ("cascode_amp", "#PWR3", "power:GND", 49.53, 85.09, 0.0, ""),
    ("cascode_amp", "#PWR4", "power:GND", 57.15, 85.09, 0.0, ""),
    ("cascode_amp", "#PWR5", "power:+12V", 49.53, 31.75, 0.0, ""),
    ("cascode_amp", "#PWR6", "power:+12V", 72.39, 31.75, 0.0, ""),
    ("cascode_amp", "CB2", "Device:C", 43.18, 81.28, 0.0, ""),
    ("cascode_amp", "CE", "Device:C", 57.15, 81.28, 0.0, ""),
    ("cascode_amp", "CIN", "Device:C", 35.56, 63.5, 90.0, ""),
    ("cascode_amp", "COUT", "Device:C", 85.09, 53.34, 90.0, ""),
    (
        "cascode_amp",
        "Q1",
        "Device:Q_NPN_BCE",
        50.8,
        66.04,
        0.0,
        "",
    ),
    (
        "cascode_amp",
        "Q2",
        "Device:Q_NPN_BCE",
        73.66,
        57.15,
        270.0,
        "y",
    ),
    ("cascode_amp", "RB1", "Device:R_US", 49.53, 35.56, 0.0, "y"),
    ("cascode_amp", "RB2", "Device:R_US", 45.72, 57.15, 270.0, ""),
    ("cascode_amp", "RB3", "Device:R_US", 39.37, 73.66, 0.0, ""),
    ("cascode_amp", "RC", "Device:R_US", 72.39, 35.56, 0.0, ""),
    ("cascode_amp", "RE", "Device:R_US", 49.53, 81.28, 0.0, ""),
    (
        "common_emitter",
        "#FLG1",
        "power:PWR_FLAG",
        40.64,
        74.93,
        0.0,
        "",
    ),
    (
        "common_emitter",
        "#FLG2",
        "power:PWR_FLAG",
        44.45,
        31.75,
        180.0,
        "",
    ),
    (
        "common_emitter",
        "#PWR1",
        "power:GND",
        40.64,
        74.93,
        0.0,
        "",
    ),
    (
        "common_emitter",
        "#PWR2",
        "power:GND",
        53.34,
        74.93,
        0.0,
        "",
    ),
    (
        "common_emitter",
        "#PWR3",
        "power:GND",
        58.42,
        74.93,
        0.0,
        "",
    ),
    (
        "common_emitter",
        "#PWR4",
        "power:+12V",
        44.45,
        31.75,
        0.0,
        "",
    ),
    (
        "common_emitter",
        "#PWR5",
        "power:+12V",
        55.88,
        31.75,
        0.0,
        "",
    ),
    ("common_emitter", "CE", "Device:C", 58.42, 71.12, 0.0, ""),
    ("common_emitter", "CIN", "Device:C", 35.56, 52.07, 90.0, ""),
    ("common_emitter", "COUT", "Device:C", 72.39, 48.26, 90.0, ""),
    (
        "common_emitter",
        "Q1",
        "Device:Q_NPN_BCE",
        54.61,
        53.34,
        0.0,
        "y",
    ),
    (
        "common_emitter",
        "R1",
        "Device:R_US",
        44.45,
        35.56,
        0.0,
        "y",
    ),
    ("common_emitter", "R2", "Device:R_US", 40.64, 71.12, 0.0, ""),
    ("common_emitter", "RC", "Device:R_US", 55.88, 35.56, 0.0, ""),
    ("common_emitter", "RE", "Device:R_US", 53.34, 71.12, 0.0, ""),
    //
    // Rows APPENDED (not regenerated) for the benchmark-widening
    // fixture `compensated_divider`: the 243 rows above are
    // byte-identical to their previous values, verified by diffing the
    // `S2K_BASELINE_DUMP=1` output against them before the splice. A
    // new fixture adds geometry that did not exist; it moves none.
    (
        "compensated_divider",
        "#FLG1",
        "power:PWR_FLAG",
        46.99,
        48.26,
        0.0,
        "",
    ),
    (
        "compensated_divider",
        "#PWR1",
        "power:GND",
        46.99,
        48.26,
        0.0,
        "",
    ),
    (
        "compensated_divider",
        "#PWR2",
        "power:GND",
        43.18,
        48.26,
        0.0,
        "",
    ),
    (
        "compensated_divider",
        "#PWR3",
        "power:GND",
        35.56,
        48.26,
        0.0,
        "",
    ),
    (
        "compensated_divider",
        "C1",
        "Device:C",
        85.09,
        35.56,
        90.0,
        "",
    ),
    (
        "compensated_divider",
        "C2",
        "Device:C",
        35.56,
        44.45,
        0.0,
        "",
    ),
    (
        "compensated_divider",
        "R1",
        "Device:R_US",
        39.37,
        35.56,
        90.0,
        "",
    ),
    (
        "compensated_divider",
        "R2",
        "Device:R_US",
        43.18,
        44.45,
        0.0,
        "",
    ),
    (
        "compensated_divider",
        "VIN",
        "Simulation_SPICE:VDC",
        46.99,
        43.18,
        0.0,
        "",
    ),
    (
        "diff_pair",
        "#FLG1",
        "power:PWR_FLAG",
        30.48,
        49.53,
        180.0,
        "",
    ),
    (
        "diff_pair",
        "#FLG2",
        "power:PWR_FLAG",
        55.88,
        49.53,
        180.0,
        "",
    ),
    (
        "diff_pair",
        "#FLG3",
        "power:PWR_FLAG",
        38.1,
        31.75,
        180.0,
        "",
    ),
    (
        "diff_pair",
        "#FLG4",
        "power:PWR_FLAG",
        43.18,
        64.77,
        0.0,
        "",
    ),
    ("diff_pair", "#PWR1", "power:+12V", 38.1, 31.75, 0.0, ""),
    ("diff_pair", "#PWR2", "power:+12V", 48.26, 31.75, 0.0, ""),
    ("diff_pair", "#PWR3", "power:VEE", 43.18, 64.77, 180.0, ""),
    ("diff_pair", "Q1", "Device:Q_NPN_BCE", 35.56, 49.53, 0.0, ""),
    ("diff_pair", "Q2", "Device:Q_NPN_BCE", 50.8, 49.53, 0.0, "y"),
    ("diff_pair", "RC1", "Device:R_US", 38.1, 35.56, 0.0, ""),
    ("diff_pair", "RC2", "Device:R_US", 48.26, 35.56, 0.0, "y"),
    ("diff_pair", "RTAIL", "Device:R_US", 43.18, 60.96, 0.0, ""),
    (
        "lc_ladder_lpf",
        "#FLG1",
        "power:PWR_FLAG",
        35.56,
        48.26,
        0.0,
        "",
    ),
    ("lc_ladder_lpf", "#PWR1", "power:GND", 35.56, 48.26, 0.0, ""),
    ("lc_ladder_lpf", "#PWR2", "power:GND", 55.88, 48.26, 0.0, ""),
    ("lc_ladder_lpf", "#PWR3", "power:GND", 71.12, 46.99, 0.0, ""),
    ("lc_ladder_lpf", "#PWR4", "power:GND", 101.6, 46.99, 0.0, ""),
    (
        "lc_ladder_lpf",
        "#PWR5",
        "power:GND",
        132.08,
        46.99,
        0.0,
        "",
    ),
    (
        "lc_ladder_lpf",
        "#PWR6",
        "power:GND",
        125.73,
        46.99,
        0.0,
        "",
    ),
    ("lc_ladder_lpf", "C1", "Device:C", 55.88, 44.45, 0.0, ""),
    ("lc_ladder_lpf", "C2", "Device:C", 71.12, 43.18, 0.0, ""),
    ("lc_ladder_lpf", "C3", "Device:C", 101.6, 43.18, 0.0, ""),
    ("lc_ladder_lpf", "C4", "Device:C", 132.08, 43.18, 0.0, ""),
    ("lc_ladder_lpf", "L1", "Device:L", 67.31, 35.56, 90.0, ""),
    ("lc_ladder_lpf", "L2", "Device:L", 97.79, 35.56, 90.0, ""),
    ("lc_ladder_lpf", "L3", "Device:L", 128.27, 35.56, 90.0, ""),
    ("lc_ladder_lpf", "RL", "Device:R_US", 125.73, 43.18, 0.0, ""),
    ("lc_ladder_lpf", "RS", "Device:R_US", 52.07, 35.56, 90.0, ""),
    (
        "lc_ladder_lpf",
        "VIN",
        "Simulation_SPICE:VDC",
        35.56,
        43.18,
        0.0,
        "y",
    ),
    (
        "multivibrator",
        "#FLG1",
        "power:PWR_FLAG",
        45.72,
        73.66,
        0.0,
        "",
    ),
    (
        "multivibrator",
        "#FLG2",
        "power:PWR_FLAG",
        45.72,
        31.75,
        180.0,
        "",
    ),
    ("multivibrator", "#PWR1", "power:GND", 45.72, 73.66, 0.0, ""),
    ("multivibrator", "#PWR2", "power:GND", 55.88, 73.66, 0.0, ""),
    ("multivibrator", "#PWR3", "power:+5V", 45.72, 31.75, 0.0, ""),
    ("multivibrator", "#PWR4", "power:+5V", 55.88, 31.75, 0.0, ""),
    ("multivibrator", "#PWR5", "power:+5V", 35.56, 44.45, 0.0, ""),
    ("multivibrator", "#PWR6", "power:+5V", 66.04, 44.45, 0.0, ""),
    ("multivibrator", "C1", "Device:C", 43.18, 52.07, 0.0, ""),
    ("multivibrator", "C2", "Device:C", 58.42, 52.07, 0.0, "y"),
    (
        "multivibrator",
        "Q1",
        "Device:Q_NPN_BCE",
        43.18,
        68.58,
        0.0,
        "",
    ),
    (
        "multivibrator",
        "Q2",
        "Device:Q_NPN_BCE",
        58.42,
        68.58,
        0.0,
        "y",
    ),
    ("multivibrator", "RB1", "Device:R_US", 35.56, 48.26, 0.0, ""),
    (
        "multivibrator",
        "RB2",
        "Device:R_US",
        66.04,
        48.26,
        0.0,
        "y",
    ),
    ("multivibrator", "RC1", "Device:R_US", 45.72, 35.56, 0.0, ""),
    (
        "multivibrator",
        "RC2",
        "Device:R_US",
        55.88,
        35.56,
        0.0,
        "y",
    ),
    (
        "named_rails",
        "#FLG1",
        "power:PWR_FLAG",
        43.18,
        55.88,
        0.0,
        "",
    ),
    (
        "named_rails",
        "#FLG2",
        "power:PWR_FLAG",
        35.56,
        49.53,
        0.0,
        "",
    ),
    (
        "named_rails",
        "#FLG3",
        "power:PWR_FLAG",
        38.1,
        31.75,
        180.0,
        "",
    ),
    ("named_rails", "#PWR1", "power:GND", 43.18, 55.88, 0.0, ""),
    ("named_rails", "#PWR2", "power:VEE", 35.56, 49.53, 180.0, ""),
    ("named_rails", "#PWR3", "power:+5V", 38.1, 31.75, 0.0, ""),
    ("named_rails", "CL", "Device:C", 43.18, 52.07, 0.0, "y"),
    ("named_rails", "RIN", "Device:R_US", 38.1, 46.99, 180.0, ""),
    ("named_rails", "RPD", "Device:R_US", 35.56, 45.72, 0.0, ""),
    ("named_rails", "RPU", "Device:R_US", 38.1, 35.56, 0.0, "y"),
    (
        "opamp_definition_level",
        "#FLG1",
        "power:PWR_FLAG",
        54.61,
        48.26,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#FLG2",
        "power:PWR_FLAG",
        59.69,
        43.18,
        180.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#FLG3",
        "power:PWR_FLAG",
        59.69,
        58.42,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#PWR1",
        "power:GND",
        54.61,
        48.26,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#PWR2",
        "power:GND",
        54.61,
        81.28,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#PWR3",
        "power:VCC",
        59.69,
        43.18,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#PWR4",
        "power:VCC",
        59.69,
        76.2,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#PWR5",
        "power:VEE",
        59.69,
        58.42,
        180.0,
        "",
    ),
    (
        "opamp_definition_level",
        "#PWR6",
        "power:VEE",
        59.69,
        91.44,
        180.0,
        "",
    ),
    (
        "opamp_definition_level",
        "RF1",
        "Device:R_US",
        60.96,
        35.56,
        90.0,
        "",
    ),
    (
        "opamp_definition_level",
        "RF2",
        "Device:R_US",
        60.96,
        68.58,
        90.0,
        "",
    ),
    (
        "opamp_definition_level",
        "RIN1",
        "Device:R_US",
        35.56,
        39.37,
        90.0,
        "",
    ),
    (
        "opamp_definition_level",
        "RIN2",
        "Device:R_US",
        35.56,
        72.39,
        90.0,
        "",
    ),
    (
        "opamp_definition_level",
        "X1",
        "Amplifier_Operational:OPAMP",
        62.23,
        50.8,
        0.0,
        "",
    ),
    (
        "opamp_definition_level",
        "X2",
        "Amplifier_Operational:OPAMP",
        62.23,
        83.82,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#FLG1",
        "power:PWR_FLAG",
        64.77,
        31.75,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#FLG2",
        "power:PWR_FLAG",
        64.77,
        46.99,
        180.0,
        "",
    ),
    (
        "opamp_inverting",
        "#FLG3",
        "power:PWR_FLAG",
        64.77,
        52.07,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR1",
        "power:GND",
        64.77,
        31.75,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR2",
        "power:VCC",
        64.77,
        46.99,
        0.0,
        "",
    ),
    (
        "opamp_inverting",
        "#PWR3",
        "power:VEE",
        64.77,
        52.07,
        180.0,
        "",
    ),
    (
        "opamp_inverting",
        "RF",
        "Device:R_US",
        57.15,
        36.83,
        90.0,
        "",
    ),
    (
        "opamp_inverting",
        "RIN",
        "Device:R_US",
        35.56,
        50.8,
        90.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#FLG1",
        "power:PWR_FLAG",
        52.07,
        35.56,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#FLG2",
        "power:PWR_FLAG",
        57.15,
        30.48,
        180.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#FLG3",
        "power:PWR_FLAG",
        57.15,
        45.72,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR1",
        "power:GND",
        52.07,
        35.56,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR2",
        "power:VCC",
        57.15,
        30.48,
        0.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "#PWR3",
        "power:VEE",
        57.15,
        45.72,
        180.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "RF",
        "Device:R_US",
        48.26,
        35.56,
        180.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "RIN",
        "Device:R_US",
        35.56,
        44.45,
        90.0,
        "",
    ),
    (
        "opamp_inverting_real",
        "X1",
        "Amplifier_Operational:OPAMP",
        59.69,
        38.1,
        0.0,
        "",
    ),
    //
    // Rows APPENDED for `opamp_transimpedance`, same protocol as the two
    // fixtures above: the dump's 268 pre-existing rows were diffed against
    // the committed table and are byte-identical.
    (
        "opamp_transimpedance",
        "#FLG1",
        "power:PWR_FLAG",
        53.34,
        58.42,
        0.0,
        "",
    ),
    (
        "opamp_transimpedance",
        "#FLG2",
        "power:PWR_FLAG",
        69.85,
        41.91,
        180.0,
        "",
    ),
    (
        "opamp_transimpedance",
        "#FLG3",
        "power:PWR_FLAG",
        69.85,
        57.15,
        0.0,
        "",
    ),
    (
        "opamp_transimpedance",
        "#PWR1",
        "power:GND",
        53.34,
        58.42,
        0.0,
        "",
    ),
    (
        "opamp_transimpedance",
        "#PWR2",
        "power:GND",
        62.23,
        58.42,
        0.0,
        "",
    ),
    (
        "opamp_transimpedance",
        "#PWR3",
        "power:GND",
        64.77,
        46.99,
        0.0,
        "",
    ),
    (
        "opamp_transimpedance",
        "#PWR4",
        "power:VCC",
        69.85,
        41.91,
        0.0,
        "",
    ),
    (
        "opamp_transimpedance",
        "#PWR5",
        "power:VEE",
        69.85,
        57.15,
        180.0,
        "",
    ),
    (
        "opamp_transimpedance",
        "CF",
        "Device:C",
        50.8,
        35.56,
        90.0,
        "",
    ),
    (
        "opamp_transimpedance",
        "CPD",
        "Device:C",
        62.23,
        54.61,
        0.0,
        "",
    ),
    (
        "opamp_transimpedance",
        "IPD",
        "Simulation_SPICE:IDC",
        53.34,
        53.34,
        180.0,
        "",
    ),
    (
        "opamp_transimpedance",
        "RF",
        "Device:R_US",
        35.56,
        35.56,
        90.0,
        "",
    ),
    (
        "opamp_transimpedance",
        "X1",
        "Amplifier_Operational:OPAMP",
        72.39,
        49.53,
        0.0,
        "",
    ),
    (
        "port_shapes",
        "#FLG1",
        "power:PWR_FLAG",
        67.31,
        58.42,
        0.0,
        "",
    ),
    ("port_shapes", "#PWR1", "power:GND", 67.31, 58.42, 0.0, ""),
    ("port_shapes", "R1", "Device:R_US", 35.56, 43.18, 270.0, ""),
    ("port_shapes", "R2", "Device:R_US", 66.04, 35.56, 90.0, ""),
    ("port_shapes", "R3", "Device:R_US", 63.5, 45.72, 90.0, ""),
    ("port_shapes", "R4", "Device:R_US", 67.31, 54.61, 0.0, ""),
    (
        "rc_lowpass",
        "#FLG1",
        "power:PWR_FLAG",
        39.37,
        48.26,
        0.0,
        "",
    ),
    ("rc_lowpass", "#PWR1", "power:GND", 39.37, 48.26, 0.0, ""),
    ("rc_lowpass", "C1", "Device:C", 39.37, 44.45, 0.0, ""),
    ("rc_lowpass", "R1", "Device:R_US", 35.56, 35.56, 90.0, ""),
    (
        "rc_lowpass_ports",
        "#FLG1",
        "power:PWR_FLAG",
        39.37,
        48.26,
        0.0,
        "",
    ),
    (
        "rc_lowpass_ports",
        "#PWR1",
        "power:GND",
        39.37,
        48.26,
        0.0,
        "",
    ),
    ("rc_lowpass_ports", "C1", "Device:C", 39.37, 44.45, 0.0, ""),
    (
        "rc_lowpass_ports",
        "R1",
        "Device:R_US",
        35.56,
        35.56,
        90.0,
        "",
    ),
    (
        "rc_phase_shift",
        "#FLG1",
        "power:PWR_FLAG",
        39.37,
        68.58,
        0.0,
        "",
    ),
    (
        "rc_phase_shift",
        "#FLG2",
        "power:PWR_FLAG",
        83.82,
        34.29,
        180.0,
        "",
    ),
    (
        "rc_phase_shift",
        "#PWR1",
        "power:GND",
        39.37,
        68.58,
        0.0,
        "",
    ),
    (
        "rc_phase_shift",
        "#PWR2",
        "power:GND",
        53.34,
        62.23,
        0.0,
        "",
    ),
    (
        "rc_phase_shift",
        "#PWR3",
        "power:GND",
        68.58,
        62.23,
        0.0,
        "",
    ),
    (
        "rc_phase_shift",
        "#PWR4",
        "power:GND",
        95.25,
        80.01,
        0.0,
        "",
    ),
    (
        "rc_phase_shift",
        "#PWR5",
        "power:GND",
        100.33,
        80.01,
        0.0,
        "",
    ),
    (
        "rc_phase_shift",
        "#PWR6",
        "power:+12V",
        83.82,
        34.29,
        0.0,
        "",
    ),
    (
        "rc_phase_shift",
        "#PWR7",
        "power:+12V",
        99.06,
        31.75,
        0.0,
        "",
    ),
    ("rc_phase_shift", "C1", "Device:C", 39.37, 64.77, 0.0, ""),
    ("rc_phase_shift", "C2", "Device:C", 53.34, 58.42, 0.0, ""),
    ("rc_phase_shift", "C3", "Device:C", 68.58, 58.42, 0.0, ""),
    ("rc_phase_shift", "CE", "Device:C", 100.33, 76.2, 0.0, ""),
    ("rc_phase_shift", "CIN", "Device:C", 80.01, 49.53, 90.0, ""),
    ("rc_phase_shift", "COUT", "Device:C", 114.3, 45.72, 90.0, ""),
    (
        "rc_phase_shift",
        "Q1",
        "Device:Q_NPN_BCE",
        92.71,
        44.45,
        0.0,
        "",
    ),
    (
        "rc_phase_shift",
        "R1",
        "Device:R_US",
        35.56,
        55.88,
        90.0,
        "",
    ),
    (
        "rc_phase_shift",
        "R2",
        "Device:R_US",
        49.53,
        49.53,
        90.0,
        "",
    ),
    (
        "rc_phase_shift",
        "R3",
        "Device:R_US",
        64.77,
        49.53,
        90.0,
        "",
    ),
    ("rc_phase_shift", "RB", "Device:R_US", 83.82, 38.1, 0.0, ""),
    ("rc_phase_shift", "RC", "Device:R_US", 99.06, 35.56, 0.0, ""),
    ("rc_phase_shift", "RE", "Device:R_US", 95.25, 76.2, 0.0, ""),
    //
    // Rows APPENDED for `resistor_ladder_ref`, same protocol as
    // `compensated_divider` above: the dump's 252 pre-existing rows were
    // diffed against the committed table and are byte-identical, so the
    // 16 rows below are the whole diff.
    (
        "resistor_ladder_ref",
        "#FLG1",
        "power:PWR_FLAG",
        48.26,
        74.93,
        0.0,
        "",
    ),
    (
        "resistor_ladder_ref",
        "#FLG2",
        "power:PWR_FLAG",
        46.99,
        31.75,
        180.0,
        "",
    ),
    (
        "resistor_ladder_ref",
        "#PWR1",
        "power:GND",
        48.26,
        74.93,
        0.0,
        "",
    ),
    (
        "resistor_ladder_ref",
        "#PWR2",
        "power:GND",
        55.88,
        74.93,
        0.0,
        "",
    ),
    (
        "resistor_ladder_ref",
        "#PWR3",
        "power:GND",
        43.18,
        74.93,
        0.0,
        "",
    ),
    (
        "resistor_ladder_ref",
        "#PWR4",
        "power:GND",
        35.56,
        74.93,
        0.0,
        "",
    ),
    (
        "resistor_ladder_ref",
        "#PWR5",
        "power:+12V",
        46.99,
        31.75,
        0.0,
        "",
    ),
    (
        "resistor_ladder_ref",
        "CB2",
        "Device:C",
        55.88,
        71.12,
        0.0,
        "",
    ),
    (
        "resistor_ladder_ref",
        "CB3",
        "Device:C",
        43.18,
        71.12,
        0.0,
        "y",
    ),
    (
        "resistor_ladder_ref",
        "CB4",
        "Device:C",
        35.56,
        71.12,
        0.0,
        "",
    ),
    (
        "resistor_ladder_ref",
        "R1",
        "Device:R_US",
        46.99,
        35.56,
        0.0,
        "y",
    ),
    (
        "resistor_ladder_ref",
        "R2",
        "Device:R_US",
        36.83,
        45.72,
        0.0,
        "",
    ),
    (
        "resistor_ladder_ref",
        "R3",
        "Device:R_US",
        49.53,
        50.8,
        180.0,
        "",
    ),
    (
        "resistor_ladder_ref",
        "R4",
        "Device:R_US",
        39.37,
        52.07,
        180.0,
        "",
    ),
    (
        "resistor_ladder_ref",
        "R5",
        "Device:R_US",
        35.56,
        57.15,
        0.0,
        "",
    ),
    (
        "resistor_ladder_ref",
        "R6",
        "Device:R_US",
        48.26,
        71.12,
        0.0,
        "y",
    ),
    (
        "sallen_key_driven",
        "#FLG1",
        "power:PWR_FLAG",
        38.1,
        52.07,
        0.0,
        "",
    ),
    (
        "sallen_key_driven",
        "#FLG2",
        "power:PWR_FLAG",
        95.25,
        29.21,
        180.0,
        "",
    ),
    (
        "sallen_key_driven",
        "#FLG3",
        "power:PWR_FLAG",
        95.25,
        44.45,
        0.0,
        "",
    ),
    (
        "sallen_key_driven",
        "#PWR1",
        "power:GND",
        38.1,
        52.07,
        0.0,
        "",
    ),
    (
        "sallen_key_driven",
        "#PWR2",
        "power:GND",
        55.88,
        52.07,
        0.0,
        "",
    ),
    (
        "sallen_key_driven",
        "#PWR3",
        "power:GND",
        86.36,
        52.07,
        0.0,
        "",
    ),
    (
        "sallen_key_driven",
        "#PWR4",
        "power:VCC",
        95.25,
        29.21,
        0.0,
        "",
    ),
    (
        "sallen_key_driven",
        "#PWR5",
        "power:VEE",
        95.25,
        44.45,
        180.0,
        "",
    ),
    (
        "sallen_key_driven",
        "C1",
        "Device:C",
        71.12,
        35.56,
        90.0,
        "",
    ),
    ("sallen_key_driven", "C2", "Device:C", 55.88, 48.26, 0.0, ""),
    (
        "sallen_key_driven",
        "R1",
        "Device:R_US",
        35.56,
        40.64,
        0.0,
        "",
    ),
    (
        "sallen_key_driven",
        "R2",
        "Device:R_US",
        52.07,
        39.37,
        90.0,
        "",
    ),
    (
        "sallen_key_driven",
        "RA",
        "Device:R_US",
        82.55,
        39.37,
        90.0,
        "",
    ),
    (
        "sallen_key_driven",
        "RB",
        "Device:R_US",
        86.36,
        48.26,
        0.0,
        "",
    ),
    (
        "sallen_key_driven",
        "VIN",
        "Simulation_SPICE:VDC",
        38.1,
        46.99,
        0.0,
        "y",
    ),
    (
        "sallen_key_driven",
        "X1",
        "Amplifier_Operational:OPAMP",
        97.79,
        36.83,
        0.0,
        "",
    ),
    (
        "sallen_key_lpf",
        "#FLG1",
        "power:PWR_FLAG",
        59.69,
        63.5,
        0.0,
        "",
    ),
    (
        "sallen_key_lpf",
        "#FLG2",
        "power:PWR_FLAG",
        83.82,
        29.21,
        180.0,
        "",
    ),
    (
        "sallen_key_lpf",
        "#FLG3",
        "power:PWR_FLAG",
        83.82,
        44.45,
        0.0,
        "",
    ),
    ("sallen_key_lpf", "#PWR1", "power:GND", 59.69, 63.5, 0.0, ""),
    (
        "sallen_key_lpf",
        "#PWR2",
        "power:GND",
        91.44,
        69.85,
        0.0,
        "",
    ),
    (
        "sallen_key_lpf",
        "#PWR3",
        "power:VCC",
        83.82,
        29.21,
        0.0,
        "",
    ),
    (
        "sallen_key_lpf",
        "#PWR4",
        "power:VEE",
        83.82,
        44.45,
        180.0,
        "",
    ),
    ("sallen_key_lpf", "C1", "Device:C", 90.17, 46.99, 90.0, ""),
    ("sallen_key_lpf", "C2", "Device:C", 59.69, 59.69, 0.0, ""),
    (
        "sallen_key_lpf",
        "R1",
        "Device:R_US",
        35.56,
        67.31,
        90.0,
        "",
    ),
    ("sallen_key_lpf", "R2", "Device:R_US", 55.88, 50.8, 90.0, ""),
    (
        "sallen_key_lpf",
        "RA",
        "Device:R_US",
        87.63,
        57.15,
        90.0,
        "",
    ),
    ("sallen_key_lpf", "RB", "Device:R_US", 91.44, 66.04, 0.0, ""),
    (
        "sallen_key_lpf",
        "X1",
        "Amplifier_Operational:OPAMP",
        86.36,
        36.83,
        0.0,
        "",
    ),
    (
        "shunt_feedback_amp",
        "#FLG1",
        "power:PWR_FLAG",
        45.72,
        68.58,
        0.0,
        "",
    ),
    (
        "shunt_feedback_amp",
        "#FLG2",
        "power:PWR_FLAG",
        39.37,
        36.83,
        180.0,
        "",
    ),
    (
        "shunt_feedback_amp",
        "#PWR1",
        "power:GND",
        45.72,
        68.58,
        0.0,
        "",
    ),
    (
        "shunt_feedback_amp",
        "#PWR2",
        "power:GND",
        55.88,
        68.58,
        0.0,
        "",
    ),
    (
        "shunt_feedback_amp",
        "#PWR3",
        "power:+12V",
        39.37,
        36.83,
        0.0,
        "",
    ),
    (
        "shunt_feedback_amp",
        "#PWR4",
        "power:+12V",
        53.34,
        31.75,
        0.0,
        "",
    ),
    (
        "shunt_feedback_amp",
        "CE",
        "Device:C",
        55.88,
        64.77,
        0.0,
        "",
    ),
    (
        "shunt_feedback_amp",
        "CIN",
        "Device:C",
        35.56,
        52.07,
        90.0,
        "",
    ),
    (
        "shunt_feedback_amp",
        "COUT",
        "Device:C",
        68.58,
        41.91,
        90.0,
        "",
    ),
    (
        "shunt_feedback_amp",
        "Q1",
        "Device:Q_NPN_BCE",
        45.72,
        52.07,
        270.0,
        "",
    ),
    (
        "shunt_feedback_amp",
        "RB",
        "Device:R_US",
        39.37,
        40.64,
        0.0,
        "",
    ),
    (
        "shunt_feedback_amp",
        "RC",
        "Device:R_US",
        53.34,
        35.56,
        0.0,
        "",
    ),
    (
        "shunt_feedback_amp",
        "RE",
        "Device:R_US",
        45.72,
        64.77,
        0.0,
        "",
    ),
    (
        "shunt_feedback_amp",
        "RF",
        "Device:R_US",
        49.53,
        45.72,
        270.0,
        "",
    ),
    //
    // Rows APPENDED for `stepped_attenuator`, same protocol: the dump's
    // 281 pre-existing rows were diffed against the committed table and
    // are byte-identical.
    (
        "stepped_attenuator",
        "#FLG1",
        "power:PWR_FLAG",
        35.56,
        52.07,
        0.0,
        "",
    ),
    (
        "stepped_attenuator",
        "#PWR1",
        "power:GND",
        35.56,
        52.07,
        0.0,
        "",
    ),
    (
        "stepped_attenuator",
        "#PWR2",
        "power:GND",
        116.84,
        52.07,
        0.0,
        "",
    ),
    (
        "stepped_attenuator",
        "R1",
        "Device:R_US",
        44.45,
        41.91,
        270.0,
        "",
    ),
    (
        "stepped_attenuator",
        "R2",
        "Device:R_US",
        55.88,
        35.56,
        90.0,
        "",
    ),
    (
        "stepped_attenuator",
        "R3",
        "Device:R_US",
        63.5,
        43.18,
        90.0,
        "",
    ),
    (
        "stepped_attenuator",
        "R4",
        "Device:R_US",
        86.36,
        35.56,
        90.0,
        "",
    ),
    (
        "stepped_attenuator",
        "R5",
        "Device:R_US",
        93.98,
        43.18,
        90.0,
        "",
    ),
    (
        "stepped_attenuator",
        "R6",
        "Device:R_US",
        113.03,
        39.37,
        90.0,
        "",
    ),
    (
        "stepped_attenuator",
        "R7",
        "Device:R_US",
        116.84,
        48.26,
        0.0,
        "",
    ),
    (
        "stepped_attenuator",
        "VIN",
        "Simulation_SPICE:VDC",
        35.56,
        46.99,
        0.0,
        "",
    ),
    (
        "two_stage_amp",
        "#FLG1",
        "power:PWR_FLAG",
        36.83,
        95.25,
        0.0,
        "",
    ),
    (
        "two_stage_amp",
        "#FLG2",
        "power:PWR_FLAG",
        35.56,
        31.75,
        180.0,
        "",
    ),
    ("two_stage_amp", "#PWR1", "power:GND", 36.83, 95.25, 0.0, ""),
    (
        "two_stage_amp",
        "#PWR10",
        "power:+12V",
        85.09,
        31.75,
        0.0,
        "",
    ),
    ("two_stage_amp", "#PWR2", "power:GND", 50.8, 100.33, 0.0, ""),
    (
        "two_stage_amp",
        "#PWR3",
        "power:GND",
        60.96,
        100.33,
        0.0,
        "",
    ),
    ("two_stage_amp", "#PWR4", "power:GND", 64.77, 97.79, 0.0, ""),
    ("two_stage_amp", "#PWR5", "power:GND", 77.47, 99.06, 0.0, ""),
    ("two_stage_amp", "#PWR6", "power:GND", 85.09, 99.06, 0.0, ""),
    (
        "two_stage_amp",
        "#PWR7",
        "power:+12V",
        35.56,
        31.75,
        0.0,
        "",
    ),
    (
        "two_stage_amp",
        "#PWR8",
        "power:+12V",
        58.42,
        31.75,
        0.0,
        "",
    ),
    (
        "two_stage_amp",
        "#PWR9",
        "power:+12V",
        67.31,
        31.75,
        0.0,
        "",
    ),
    ("two_stage_amp", "CC", "Device:C", 64.77, 63.5, 90.0, ""),
    ("two_stage_amp", "CE1", "Device:C", 60.96, 96.52, 0.0, ""),
    ("two_stage_amp", "CE2", "Device:C", 85.09, 95.25, 0.0, ""),
    ("two_stage_amp", "CIN", "Device:C", 35.56, 63.5, 90.0, ""),
    ("two_stage_amp", "COUT", "Device:C", 102.87, 59.69, 90.0, ""),
    (
        "two_stage_amp",
        "Q1",
        "Device:Q_NPN_BCE",
        54.61,
        64.77,
        0.0,
        "y",
    ),
    (
        "two_stage_amp",
        "Q2",
        "Device:Q_NPN_BCE",
        80.01,
        66.04,
        0.0,
        "",
    ),
    (
        "two_stage_amp",
        "RB1",
        "Device:R_US",
        35.56,
        35.56,
        0.0,
        "y",
    ),
    ("two_stage_amp", "RB2", "Device:R_US", 36.83, 91.44, 0.0, ""),
    ("two_stage_amp", "RB3", "Device:R_US", 67.31, 35.56, 0.0, ""),
    ("two_stage_amp", "RB4", "Device:R_US", 64.77, 93.98, 0.0, ""),
    ("two_stage_amp", "RC1", "Device:R_US", 58.42, 35.56, 0.0, ""),
    ("two_stage_amp", "RC2", "Device:R_US", 85.09, 35.56, 0.0, ""),
    ("two_stage_amp", "RE1", "Device:R_US", 50.8, 96.52, 0.0, ""),
    ("two_stage_amp", "RE2", "Device:R_US", 77.47, 95.25, 0.0, ""),
    (
        "wien_bridge_osc",
        "#FLG1",
        "power:PWR_FLAG",
        43.18,
        72.39,
        0.0,
        "",
    ),
    (
        "wien_bridge_osc",
        "#FLG2",
        "power:PWR_FLAG",
        49.53,
        38.1,
        180.0,
        "",
    ),
    (
        "wien_bridge_osc",
        "#FLG3",
        "power:PWR_FLAG",
        49.53,
        53.34,
        0.0,
        "",
    ),
    (
        "wien_bridge_osc",
        "#PWR1",
        "power:GND",
        43.18,
        72.39,
        0.0,
        "",
    ),
    (
        "wien_bridge_osc",
        "#PWR2",
        "power:GND",
        48.26,
        71.12,
        0.0,
        "",
    ),
    (
        "wien_bridge_osc",
        "#PWR3",
        "power:GND",
        40.64,
        71.12,
        0.0,
        "",
    ),
    (
        "wien_bridge_osc",
        "#PWR4",
        "power:VCC",
        49.53,
        38.1,
        0.0,
        "",
    ),
    (
        "wien_bridge_osc",
        "#PWR5",
        "power:VEE",
        49.53,
        53.34,
        180.0,
        "",
    ),
    ("wien_bridge_osc", "CP", "Device:C", 48.26, 67.31, 0.0, "y"),
    ("wien_bridge_osc", "CS", "Device:C", 50.8, 35.56, 270.0, ""),
    (
        "wien_bridge_osc",
        "RF",
        "Device:R_US",
        46.99,
        54.61,
        270.0,
        "",
    ),
    (
        "wien_bridge_osc",
        "RG",
        "Device:R_US",
        40.64,
        67.31,
        0.0,
        "y",
    ),
    (
        "wien_bridge_osc",
        "RP",
        "Device:R_US",
        43.18,
        68.58,
        0.0,
        "",
    ),
    (
        "wien_bridge_osc",
        "RS",
        "Device:R_US",
        35.56,
        55.88,
        90.0,
        "",
    ),
    (
        "wien_bridge_osc",
        "X1",
        "Amplifier_Operational:OPAMP",
        52.07,
        45.72,
        0.0,
        "",
    ),
];

// Every emitted fixture, in alphabetical order. The port and
// definition-level sheets were absent here while the rest of the
// suite had already been extended to grade them, so accidental
// movement in the newest features was the least protected.
//
// `rc_phase_shift` (F0) sorts last, so its rows APPEND to `BASELINE`
// rather than interleaving: the ten v0.1 fixtures' rows stayed
// byte-identical when F0 landed, which is the property CLAUDE.md's
// "existing fixtures' budgets must not move" rule demands of a
// fixture addition.
const FIXTURES: &[&str] = &[
    "cascode_amp",
    "common_emitter",
    "compensated_divider",
    "diff_pair",
    "lc_ladder_lpf",
    "multivibrator",
    "named_rails",
    "opamp_definition_level",
    "opamp_inverting",
    "opamp_inverting_real",
    "opamp_transimpedance",
    "port_shapes",
    "rc_lowpass",
    "rc_lowpass_ports",
    "rc_phase_shift",
    "resistor_ladder_ref",
    "sallen_key_driven",
    "sallen_key_lpf",
    "shunt_feedback_amp",
    "stepped_attenuator",
    "two_stage_amp",
    "wien_bridge_osc",
];

#[test]
fn baseline_lock_all_fixtures() {
    let mut failures = Vec::new();
    let mut all_actual = Vec::new();

    for fix in FIXTURES {
        let dir = tempdir(fix);
        let cir = fixtures_dir().join(format!("{fix}.cir"));
        let sch = spice_to_kicad(&cir, &dir).expect("emit schematic");
        for row in extract_symbols(&sch) {
            all_actual.push(((*fix).to_string(), row.0, row.1, row.2, row.3, row.4, row.5));
        }
    }

    // Regeneration hook — see this file's module doc. Prints the
    // measured table in source syntax and skips the comparison, so a
    // deliberate layout change is recorded by copy-paste rather than by
    // hand-editing 100+ rustfmt-wrapped rows.
    if std::env::var_os("S2K_BASELINE_DUMP").is_some() {
        println!("const BASELINE: &[(&str, &str, &str, f64, f64, f64, &str)] = &[");
        for (fix, refdes, lib_id, x, y, rot, mirror) in &all_actual {
            println!("    ({fix:?}, {refdes:?}, {lib_id:?}, {x:?}, {y:?}, {rot:?}, {mirror:?}),");
        }
        println!("];");
        println!(
            "// S2K_BASELINE_DUMP: {} rows printed; comparison skipped. \
             Paste over the BASELINE block and run `cargo fmt`.",
            all_actual.len()
        );
        return;
    }

    let expected: Vec<_> = BASELINE
        .iter()
        .map(|t| {
            (
                t.0.to_string(),
                t.1.to_string(),
                t.2.to_string(),
                t.3,
                t.4,
                t.5,
                t.6.to_string(),
            )
        })
        .collect();

    // Detect differences with full context.
    let mut e_iter = expected.iter();
    let mut a_iter = all_actual.iter();
    let mut e_cur = e_iter.next();
    let mut a_cur = a_iter.next();
    loop {
        match (e_cur, a_cur) {
            (None, None) => break,
            (Some(e), None) => {
                failures.push(format!("MISSING in actual: {e:?}"));
                e_cur = e_iter.next();
            }
            (None, Some(a)) => {
                failures.push(format!("EXTRA in actual: {a:?}"));
                a_cur = a_iter.next();
            }
            (Some(e), Some(a)) => {
                if e == a {
                    e_cur = e_iter.next();
                    a_cur = a_iter.next();
                } else if (&e.0, &e.1) < (&a.0, &a.1) {
                    failures.push(format!("MISSING in actual: {e:?}"));
                    e_cur = e_iter.next();
                } else if (&e.0, &e.1) > (&a.0, &a.1) {
                    failures.push(format!("EXTRA in actual: {a:?}"));
                    a_cur = a_iter.next();
                } else {
                    failures.push(format!("DIFF\n  expected: {e:?}\n  actual:   {a:?}"));
                    e_cur = e_iter.next();
                    a_cur = a_iter.next();
                }
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "baseline_lock: {} differences\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}
