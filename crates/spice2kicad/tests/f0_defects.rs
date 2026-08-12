//! Benchmark defect locks — the fixtures the current converter cannot
//! grade, and why.
//!
//! F0 (see `docs/v0.2-roadmap.md` § "Findings / status log") added three
//! harder fixtures so the placer work has circuits with real headroom.
//! Two of them — `rc_phase_shift` and `two_stage_amp` — convert and are
//! fully registered across the fixture-enumerating verifiers. The third
//! is held here:
//!
//!  * **`shunt_feedback_amp` — Tier-0.** The converter refuses to emit
//!    it: no routing of its stage-3 placement avoids merging the
//!    collector net into the `vcc` rail. Deterministic, and rooted in
//!    the SA end-state at its *default* iteration count — see the full
//!    diagnosis on the test below, which supersedes the original
//!    "base/emitter short" reading.
//!
//! **`two_stage_amp` was PROMOTED out of this file**, which is the lock
//! mechanism working as designed. It was held here on a *runtime* defect
//! — it converted correctly but took ~112 s (**unoptimised** debug build,
//! `7f707e6`) where the median fixture took 0.4 s, so registering it
//! across ~15 verifiers would have added ~25 CPU-minutes to every suite
//! run. The two levers its lock named as the fix both landed (`6a18a8b`
//! skip-the-trial-route, `2d3c81b` memoise-phase-4.5) and `[profile.dev]`
//! moved to `opt-level = 2`; the conversion is now **~1.0 s**. Its own
//! unexpected-pass tripwire fired, and the fixture is now graded across
//! every fixture-enumerating table with zero-slack baselines. The lock
//! and its floor constant are deleted rather than left standing — a
//! stale exclusion is exactly what this file exists to prevent.
//!
//! The `.cir` file is committed **unmodified**. Slimming a fixture until
//! it passes hides the defect that makes it interesting, so it is not
//! simplified; it is reproduced here so the evidence stays runnable and
//! cannot rot silently.
//!
//! This is a *defect lock*, not a skip: it carries an unexpected-pass
//! tripwire, the same contract as `tests/common/xfail.rs`. When the
//! underlying defect is fixed, the test fails and tells you to promote
//! the fixture into the graded suite and delete the lock.
//!
//! # F2 (the second benchmark wave)
//!
//! F2 added four fixtures the placer draws badly but correctly — they
//! are registered across the graded suite — and one it cannot draw at
//! all:
//!
//!  * **`sallen_key_driven` — Tier-0.** The Sallen-Key filter with its
//!    stimulus DRAWN instead of `;@ ignore`d. Its passing twin
//!    `sallen_key_lpf` is in the graded suite, so the defect is
//!    attributable to a single input difference. See the lock below.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> common::TempDir {
    common::TempDir::new("f0", name)
}

/// Run the CLI over `fixture`, capturing stdout/stderr.
///
/// `--no-layout-cache` is load-bearing: without it a re-conversion into
/// a used directory pins every element from the ADR-4 sidecar and the
/// placer stage under test becomes a no-op (CLAUDE.md § "Useful
/// commands", the layout-cache trap).
///
/// **Placer-aware (ADR-23).** This file keeps its own conversion driver
/// — it needs the raw `Output`, which `common::spice_to_kicad` does not
/// surface — so it must forward `common::placer_args()` itself or the
/// selection silently does not reach it. ADR-23's "Known limits of the
/// instrument" recorded exactly that hole: `f0_defects` was not
/// placer-aware, so a `S2K_PLACER=<name>` run left this file on the
/// champion and the strongest acceptance test the project has for a
/// replacement placer — the `shunt_feedback_amp` Tier-0 net-merge
/// refusal (ADR-20) — was absent from every challenger's scoreboard row.
/// It is now forwarded, and the refusal is reported as a Tier-0 metric
/// (see [`shunt_feedback_amp_conversion_is_a_tier0_net_short`]).
fn convert(fixture: &str, out_dir: &Path, extra: &[&str]) -> Output {
    let src = fixtures_dir().join(format!("{fixture}.cir"));
    let out = out_dir.join(format!("{fixture}.kicad_sch"));
    let bin = env!("CARGO_BIN_EXE_spice2kicad");
    let lib_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("crates/kicad-symbols/tests/fixtures");
    let mut cmd = Command::new(bin);
    cmd.arg(&src)
        .arg("-t")
        .arg("schematic")
        .arg("-o")
        .arg(&out)
        .arg("--no-layout-cache")
        .arg("-l")
        .arg(lib_dir.join("Device.kicad_sym"))
        .arg("-l")
        .arg(lib_dir.join("Simulation_SPICE.kicad_sym"))
        .arg("-l")
        .arg(lib_dir.join("Amplifier_Operational.kicad_sym"))
        .arg("-l")
        .arg(lib_dir.join("power.kicad_sym"))
        .args(common::placer_args());
    cmd.args(extra);
    cmd.output().expect("invoke spice2kicad")
}

// --- shunt_feedback_amp: Tier-0 net short ---------------------------------

/// **Defect lock (Tier-0, V11).** Converting `shunt_feedback_amp` at
/// default settings produces a placement whose collector trunk cannot
/// be routed without terminating on `RC`'s own `vcc` pin — merging the
/// collector net into the `vcc` rail. The converter **refuses**: it
/// exits non-zero without writing a `.kicad_sch`, which is the correct
/// behaviour for a converter that cannot honour Tier-0.
///
/// **Current failure (measured on this tree, deterministic):**
///
/// ```text
/// route: v11: net index 1 has 1 endpoint and 0 interior foreign-pin
///        coincidences left after active rerouting
/// ERROR: net partition: MERGE: source nets ["c", "vcc"] share one
///        geometric component (KiCad imports them as ONE net); …
/// ```
///
/// i.e. the collector net `c` and the `vcc` rail are merged: the `c`
/// trunk terminates on `RC`'s own `vcc` pin.
///
/// **The refusal is unconditional (ADR-21), and since ADR-22 it is
/// *geometric*.** Until ADR-21 the only thing turning this into a
/// non-zero exit was the CLI's *optional* post-emit `kicad-cli`
/// connectivity check — so with `--no-verify`, or on any machine without
/// KiCad, the converter emitted the shorted schematic at exit 0. ADR-21
/// closed that by escalating the router's `v11:` warning *string*;
/// ADR-22 replaced the string match with
/// `kicad_emitter::connectivity::check_partition`, which reconstructs the
/// whole net partition from the final ink and refuses on the
/// **consequence** rather than on the diagnostic. This fixture is now
/// refused for what its geometry does — merging `c` into `vcc` — no
/// matter which warning the router prints on the way there. See
/// [`v11_residue_is_refused_without_kicad_cli`], the regression test for
/// the hole specifically.
///
/// # Diagnosis (superseding the original entry)
///
/// The first version of this lock recorded the defect as a base/emitter
/// short and localised it to "one specific SA end-state whose placement
/// the router's conflict-resolution cascade cannot legalise". Both
/// halves were investigated; the second is right, the first was only the
/// *first* symptom. What is actually going on:
///
/// 1. **The SA end-state is Tier-0 broken before decoration.** At the
///    default 200 refine iterations the annealer places `Q1` at
///    `(46.99, 45.72)`, and the placement that reaches layout phase 4.5
///    already leaves **two signal nets severed** (`severed = 2`, measured
///    by phase 4.5's own real-router oracle). Nothing downstream of the
///    placer can move an element, so this is a *placer* defect.
/// 2. **Phase 4.5 used to "repair" it by shorting.** Its acceptance
///    objective scored `(v13, v12, v5, bends)` and held `severed` as a
///    mere floor, so it had no reason to *seek* the repair — and the
///    repair it stumbled into was rotating `Q1` until its base pin sat
///    exactly on `RE`'s pin 1. That is the original recorded symptom:
///    base merged into emitter. Two pins on one coordinate is a short no
///    router can undo (it moves wires, not pins), which is why
///    `conflict::resolve_conflicts` burned its whole iteration bound and
///    logged "endpoint conflicts left after 6 resolve iterations".
/// 3. **With Tier-0 leading the objective, that specific short is gone
///    and the underlying placement defect is exposed.** Phase 4.5 now
///    scores `(severed, coincident, v11, v13, v12, v5, bends)` and its
///    oracle sees rail-glyph pins and the real obstacle set. It repairs
///    the severance (`severed` 2 → 0) without a pin-on-pin short
///    (`coincident` 0), but the best pose available still leaves one
///    unresolved wire-on-foreign-pin residue (`v11` 1) — the `c` trunk
///    on `RC`'s `vcc` pin.
/// 4. **No orientation of `Q1` fixes it.** All eight V14-allowed poses
///    were enumerated against the real router at this position; every one
///    scores non-zero on at least one Tier-0 count. The lever is
///    *position*, and phase 4.5 owns orientation only.
/// 5. **Root cause: the deferred R-5 rail-pin defect.** `RC` (and `RB`)
///    are placed with their **rail pin facing into the circuit** — `RC`
///    is rot 180, so its `vcc` pin is its *lower* pin, and the `+12V`
///    glyph therefore hangs *downward* into the routing channel the
///    collector and emitter trunks need. `tests/placement_quality.rs::
///    v14_rail_pin_faces_rail` measures exactly this and flags
///    `shunt_feedback_amp` (`#PWR3` below `RB`'s body centre). R-5 is a
///    known, owner-gated item: CLAUDE.md records that the R-5 fix "could
///    not land because it tripped a single fixture's Tier-1 ratchet", and
///    the global-improvement escape needs owner sign-off. On this fixture
///    the same defect escalates from Tier-1 aesthetics to Tier-0
///    correctness, which is new evidence for that decision.
///
/// **`--refine-iterations` sweep** (unchanged, and now explained): 0, 1,
/// 20, 40, 60, 80, 100 and 400 all convert cleanly; 150 and 200 (the
/// default) fail. It is not a monotone gradient — it is which placement
/// the SA lands on, and only some of them box `Q1` in.
///
/// **Do not chase this by moving the `--refine-iterations` default.**
/// That relocates the trap rather than removing it.
///
/// This test is deliberately **not** `#[ignore]`d: the failing
/// conversion is cheap (0.18 s, current optimised dev profile — it
/// refuses before decoration), and an `#[ignore]`d lock would never
/// notice the day the defect is fixed.
///
/// # Scoreboard (ADR-23)
///
/// This lock reports two Tier-0 metrics, which is the point of making
/// this file placer-aware. ADR-20 calls the refusal "the strongest
/// acceptance test for any replacement placer", yet it was absent from
/// every challenger's row because `f0_defects` kept its own,
/// placer-blind conversion driver. Both are recorded BEFORE the
/// assertions below (the sink's contract), so a challenger that FIXES
/// the refusal still reports its `0` even as this test panics with
/// UNEXPECTED PASS — the measurement survives the very outcome that
/// makes the lock red.
///
/// * `t0.convert_fail` — the converter would not emit a file at all.
/// * `t0.partition` — the specific ADR-22 finding: the collector net
///   `c` and the `vcc` rail land in one geometric component.
///
/// Both are **1 on the champion**. That is why the scoreboard's Tier-0
/// gate compares against the champion rather than against zero; see the
/// Tier-0 block in `tests/scoreboard.rs` for why the absolute form would
/// otherwise veto every challenger.
#[test]
fn shunt_feedback_amp_conversion_is_a_tier0_net_short() {
    let tmp = tempdir("shunt_feedback_amp");
    let out = convert("shunt_feedback_amp", &tmp, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    common::scoreboard::record_count(
        "t0.convert_fail",
        "shunt_feedback_amp",
        usize::from(!out.status.success()),
    );
    common::scoreboard::record_count(
        "t0.partition",
        "shunt_feedback_amp",
        usize::from(stderr.contains("net partition: MERGE:")),
    );

    assert!(
        !out.status.success(),
        "UNEXPECTED PASS: `shunt_feedback_amp` now converts cleanly (exit {:?}). \
         The Tier-0 base/emitter short this lock records is FIXED. Promote the \
         fixture into the graded suite — register it in the fixture-enumerating \
         tables across crates/spice2kicad/tests/ with zero-slack baselines — and \
         DELETE this test. See docs/v0.2-roadmap.md § F0.\nstderr:\n{stderr}",
        out.status.code(),
    );
    // Match the emitter's own partition finding, which is printed on one
    // unwrapped line. Do NOT match the headline sentence of a wrapped
    // message: the CLI's connectivity report wraps "…does not wire up
    // the   source circuit." with a run of spaces, and an earlier
    // version of this lock asserted a substring that was never present.
    //
    // The net pair is the load-bearing part: it pins *which* short this
    // lock records (ADR-20 § "Root cause": the `c` trunk terminating on
    // `RC`'s own `vcc` pin, rooted in the owner-gated R-5 rail-pin
    // defect). The set is rendered from a `BTreeSet`, so the order is
    // deterministic.
    assert!(
        stderr.contains(r#"MERGE: source nets ["c", "vcc"]"#),
        "`shunt_feedback_amp` failed to convert, but NOT with the recorded Tier-0 \
         merge of the collector net into the `vcc` rail. This lock describes one \
         specific defect; a different failure is a new regression to diagnose, not \
         this one.\nstderr:\n{stderr}",
    );
}

/// **Regression test for the ADR-21 / ADR-22 hole.** A net merge — here
/// a routed wire endpoint left sitting on a *foreign* net's pin, which
/// KiCad joins on load — must be refused **unconditionally**: not
/// behind `--no-verify`, not behind an env var, and not dependent on
/// `kicad-cli` being installed.
///
/// Before ADR-21 this exact invocation exited **0** and wrote a
/// `.kicad_sch` shorting the collector net to `vcc`. The escalation to
/// `EmitError::V11Violation` was gated on `SPICE2KICAD_V11_STRICT`, so
/// the only thing catching the short was the CLI's *optional* post-emit
/// `kicad-cli` connectivity check — unavailable on any machine without
/// KiCad, and skipped outright by `--no-verify`.
///
/// ADR-22 then removed the last dependence on the *shape* of the defect:
/// the refusal no longer comes from string-matching the router's `v11:`
/// warning but from `check_partition` finding two source nets in one
/// geometric component. That matters here because it is what closes the
/// sibling hole ADR-21 could only document — `conflict:`, which is the
/// same Tier-0 consequence with no escalation of its own.
///
/// **What is under test is the exit code.** `--no-verify` removes the
/// post-emit check, so a non-zero exit here can only come from the
/// emitter's own refusal. Asserting on stderr text would not
/// distinguish the two paths (both print a `v11:` line), and stderr
/// substrings have already burned one author on this file — hence the
/// two assertions below: exit status first, then the *absence* of an
/// output file, which pins that the refusal happens before any bytes
/// are written rather than after.
///
/// This is deliberately in the F0 defect file next to the lock it
/// shares a fixture with. The day `shunt_feedback_amp` is promoted,
/// this test moves rather than dies: point it at any fixture that still
/// trips a Tier-0 residue, or delete it only once no fixture can.
#[test]
fn v11_residue_is_refused_without_kicad_cli() {
    let tmp = tempdir("v11-unconditional");
    let out = convert("shunt_feedback_amp", &tmp, &["--no-verify"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(1),
        "TIER-0 HOLE REOPENED: `shunt_feedback_amp --no-verify` exited {:?}. A \
         routed wire left on a foreign net's pin is a silent net merge on KiCad \
         load; the converter must refuse rather than emit it, with no dependence \
         on `kicad-cli` or on any env gate. See ADR-21 / ADR-22.\nstderr:\n{stderr}",
        out.status.code(),
    );
    let emitted = tmp.join("shunt_feedback_amp.kicad_sch");
    assert!(
        !emitted.exists(),
        "the converter refused (exit 1) but still wrote {}. A refusal must not \
         leave a schematic on disk that a later step could pick up.",
        emitted.display(),
    );
}

// --- sallen_key_driven: Tier-0 net merge on a DRAWN stimulus --------------

/// **Defect lock (Tier-0, V11 / ADR-22 partition).** `sallen_key_driven`
/// is `sallen_key_lpf` with the stimulus drawn rather than hidden. The
/// converter refuses it: the emitted geometry puts the op-amp's
/// non-inverting input net `np` and its output net `out` in ONE
/// geometric component, which KiCad would import as a single net —
/// i.e. the filter's feedback loop shorted.
///
/// ```text
/// ERROR: net partition: MERGE: source nets ["np", "out"] share one
///        geometric component (KiCad imports them as ONE net); …
/// ```
///
/// # Why this fixture exists, and what makes it different
///
/// The roadmap's B2/B3 post-mortem established that **every** fixture in
/// the suite `;@ ignore`s its stimulus, so `layers.rs::assign_x_layers`
/// finds `sources` empty and returns `no_source_fallback` on 100% of
/// real input — the rooted-DAG layering path, and everything downstream
/// of it (`break_cycles` included), had never executed on a real
/// circuit. F2 added two fixtures that do reach it. One,
/// `lc_ladder_lpf`, converts cleanly and is graded. The other is this.
///
/// **Three control arms, so the attribution is not a guess:**
///
/// 1. `sallen_key_lpf` — the same circuit with `;@ ignore` on VIN —
///    converts cleanly and is fully graded. So the topology alone is
///    fine.
/// 2. `lc_ladder_lpf` — a different topology, also with a drawn source,
///    also rooted — converts cleanly and is fully graded. So a drawn
///    source alone is fine.
/// 3. Adding `*@port in=input` back to THIS file — a purely cosmetic
///    label directive that changes no topology — makes it convert
///    cleanly. That is the finding worth recording: a decoration-level
///    annotation flips a Tier-0 correctness outcome, which is the
///    "global, unattributable consequences" property ADR-17 diagnosed,
///    reached from a new direction.
///
/// # Characterisation (measured on this tree, deterministic)
///
/// Three `--no-layout-cache` runs produce byte-identical output and the
/// same non-zero exit. A `--refine-iterations` sweep:
///
/// | setting | result |
/// | --- | --- |
/// | `--no-refine` | **SPLIT**: `np` reconstructs as 2 islands |
/// | 0, 1, 10, 50, 100, 150 | clean |
/// | 199, 200 (the default) | **MERGE**: `np` + `out` in one component |
/// | 201, 400 | clean |
///
/// The `--no-refine` row is the one that is genuinely new. `--no-refine`
/// ablates *both* the SA and phase 4.5, so what it leaves is the bare
/// deterministic seed — and the bare seed is **already Tier-0 broken
/// here**, severing `np` into two islands before any annealer runs.
/// `shunt_feedback_amp`, the project's other Tier-0 lock, is clean at
/// `--no-refine`; its defect is an SA end-state. This one is not. It is
/// therefore direct evidence against reading the SA as the sole source
/// of Tier-0 placement failures, and a second acceptance test for any
/// replacement placer — one that a purely deterministic constructive
/// seed would have to pass on its own merits.
///
/// Not `#[ignore]`d: the failing conversion is cheap (it refuses before
/// decoration), and an ignored lock would never notice the day the
/// defect is fixed.
#[test]
fn sallen_key_driven_conversion_is_a_tier0_net_merge() {
    let tmp = tempdir("sallen_key_driven");
    let out = convert("sallen_key_driven", &tmp, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    common::scoreboard::record_count(
        "t0.convert_fail",
        "sallen_key_driven",
        usize::from(!out.status.success()),
    );
    common::scoreboard::record_count(
        "t0.partition",
        "sallen_key_driven",
        usize::from(stderr.contains("net partition: MERGE:")),
    );

    assert!(
        !out.status.success(),
        "UNEXPECTED PASS: `sallen_key_driven` now converts cleanly (exit {:?}). The \
         Tier-0 np/out merge this lock records is FIXED. Promote the fixture into \
         the graded suite — register it across the fixture-enumerating tables in \
         crates/spice2kicad/tests/ with zero-slack baselines — and DELETE this \
         test. See docs/v0.2-roadmap.md § F2.\nstderr:\n{stderr}",
        out.status.code(),
    );
    // The net pair pins *which* short this lock records. A different
    // failure is a new regression to diagnose, not this one.
    assert!(
        stderr.contains(r#"MERGE: source nets ["np", "out"]"#),
        "`sallen_key_driven` failed to convert, but NOT with the recorded Tier-0 \
         merge of the op-amp's non-inverting input into its output.\nstderr:\n{stderr}",
    );
}

/// The control arm, kept as a test so it cannot silently rot: the
/// **bare deterministic seed** — `--no-refine`, i.e. no SA and no phase
/// 4.5 — is Tier-0 broken on this fixture too, and with a *different*
/// finding (a SPLIT, not a MERGE).
///
/// This is the half of the characterisation that matters most for the
/// v0.2 placer direction, and it is the half a single default-settings
/// lock would lose. ADR-17's retirement records that the bare seed has
/// the same blast radius as the SA; this says something sharper — the
/// bare seed can be *wrong*, not merely different. Any replacement that
/// is "the deterministic seed, without the annealer" must clear this.
#[test]
fn sallen_key_driven_bare_seed_is_also_tier0_broken() {
    let tmp = tempdir("sallen_key_driven-seed");
    let out = convert("sallen_key_driven", &tmp, &["--no-refine", "--no-verify"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "UNEXPECTED PASS: the bare seed (`--no-refine`) now converts `sallen_key_driven` \
         cleanly (exit {:?}). Re-measure the whole lock above — its diagnosis rests on \
         the seed being broken independently of the SA.\nstderr:\n{stderr}",
        out.status.code(),
    );
    assert!(
        stderr.contains(r#"SPLIT: source net "np""#),
        "the bare seed refused, but not with the recorded SPLIT of `np` into two \
         islands.\nstderr:\n{stderr}",
    );
}
