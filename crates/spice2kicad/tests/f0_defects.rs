//! F0 defect locks — the two benchmark fixtures the current converter
//! cannot grade, and why.
//!
//! F0 (see `docs/v0.2-roadmap.md` § "Findings / status log") added three
//! harder fixtures so the placer work has circuits with real headroom.
//! One of them — `rc_phase_shift` — converts and is fully registered
//! across the fixture-enumerating verifiers. The other two convert
//! *badly*, in two different ways, and are held here instead:
//!
//!  * **`shunt_feedback_amp` — Tier-0.** The converter refuses to emit
//!    it: no routing of its stage-3 placement avoids merging the
//!    collector net into the `vcc` rail. Deterministic, and rooted in
//!    the SA end-state at its *default* iteration count — see the full
//!    diagnosis on the test below, which supersedes the original
//!    "base/emitter short" reading.
//!  * **`two_stage_amp` — runtime.** It converted *correctly* but took
//!    ~112 s (**unoptimised** debug build, `7f707e6`) where the median
//!    fixture took 0.4 s, so registering it across ~15 verifiers would
//!    have added ~25 CPU-minutes to every suite run. **That no longer
//!    reproduces**: 1.05 s on the current optimised dev profile. The
//!    lock is kept but flagged — see the re-measurement on the test.
//!
//! Both `.cir` files are committed **unmodified**. Slimming a fixture
//! until it passes hides the defect that makes it interesting, so
//! neither is simplified; they are reproduced here so the evidence stays
//! runnable and cannot rot silently.
//!
//! These are *defect locks*, not skips: each carries an unexpected-pass
//! tripwire, the same contract as `tests/common/xfail.rs`. When the
//! underlying defect is fixed, the test fails and tells you to promote
//! the fixture into the graded suite and delete the lock.

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
        .arg(lib_dir.join("power.kicad_sym"));
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
#[test]
fn shunt_feedback_amp_conversion_is_a_tier0_net_short() {
    let tmp = tempdir("shunt_feedback_amp");
    let out = convert("shunt_feedback_amp", &tmp, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

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

// --- two_stage_amp: pathological conversion time --------------------------

/// Conversion time above which `two_stage_amp` still counts as
/// reproducing the defect this lock records.
///
/// Measured 112 s on an otherwise-idle machine (**unoptimised** debug
/// build — `opt-level = 0` — on `7f707e6`, before the workspace switched
/// `[profile.dev]` to `opt-level = 2`); the floor was set an order of
/// magnitude below that so machine speed could not make the lock flaky,
/// while still being ~75× the median fixture's 0.4 s.
///
/// The floor is left where it is deliberately. It no longer holds (see
/// the re-measurement on the test below) and moving it is a
/// promote-or-delete decision about the whole lock, not a number to
/// re-tune.
const TWO_STAGE_AMP_SLOW_FLOOR_SECS: f64 = 15.0;

/// **Defect lock (runtime) — STALE, kept pending a promote-or-delete
/// decision.** As recorded, `two_stage_amp` converted *correctly* —
/// exit 0, connectivity check clean — but took ~112 s where every v0.1
/// fixture took 0.3–4.5 s, so it was committed but **not registered**:
/// ~15 fixture-enumerating verifiers each converting it would have added
/// roughly 25 CPU-minutes to every `just test`.
///
/// **That is no longer true on this tree.** The two levers this comment
/// itself named as the fix have since landed (`6a18a8b` skip-the-trial-
/// route, `2d3c81b` memoise-phase-4.5), and `[profile.dev]` moved to
/// `opt-level = 2`. Re-measured below: **1.05 s**, i.e. under the 15 s
/// floor, so the assertion in this test would now report UNEXPECTED
/// PASS. The test stays `#[ignore]`d (the suite is unaffected) and the
/// lock is left standing rather than silently deleted, because
/// promoting the fixture means registering it across the
/// fixture-enumerating tables with zero-slack baselines — an owner call,
/// not a hygiene edit.
///
/// **What was measured — original vs. now.** Both columns are idle
/// machine, `--no-layout-cache`. `7f707e6` is an **unoptimised** debug
/// build (`opt-level = 0`); "now" is the current **optimised dev
/// profile** (`opt-level = 2`, `debug-assertions = true`) on
/// `chore/hygiene`, which is exactly the binary the test harness runs
/// (`CARGO_BIN_EXE_spice2kicad`). Median of three runs.
///
/// | conversion                              | `7f707e6`, unopt | now, dev opt-2 |
/// | --------------------------------------- | ---------------- | -------------- |
/// | `two_stage_amp` (default)               | 112 s            | 1.05 s         |
/// | `two_stage_amp --no-verify`             | 118 s            | 0.70 s         |
/// | `two_stage_amp --no-refine`             | 0.43 s           | 0.36 s         |
/// | `two_stage_amp --no-refine --no-verify` | 0.09 s           | 0.04 s         |
/// | `rc_phase_shift` (default)              | 11.5 s           | 0.44 s         |
/// | `common_emitter` (default)              | 4.5 s            | 0.46 s         |
/// | median v0.1 fixture                     | 0.4 s            | 0.45 s         |
///
/// **The profile is not the story; the code is.** Control arm, same
/// tree, same command, rebuilt with `CARGO_PROFILE_DEV_OPT_LEVEL=0` so
/// only the optimisation level differs: `two_stage_amp` takes **5.4 s**.
/// So of the 112 s → 1.05 s fall, ~21× is the phase-4.5 work and ~5× is
/// the profile. Quoting either alone would misattribute it.
///
/// **It is NOT the memory defect it was once reported as.** An earlier
/// F0 attempt recorded this fixture as needing ">8 GB of virtual memory,
/// nondeterministically OOM-killed". That does not reproduce here:
/// sampling `/proc/<pid>/status` across a full conversion gives a
/// **VmPeak of 25.8 MB**, and the conversion completes under a
/// `ulimit -v 8388608` (8 GiB) cap every time. Memory is a non-issue on
/// this tree; the cost is entirely CPU. (That earlier number was taken
/// on the `ed51164` ADR-19 M4 tree, since reverted.)
///
/// **First-look diagnosis — a bounded product, not unbounded growth**
/// (all figures in this paragraph and the next are from the original
/// unoptimised `7f707e6` measurement).
/// `--no-refine` removes 99.6 % of the cost, and per CLAUDE.md /
/// ADR-17 that flag ablates *two* passes: the SA and the phase-4.5
/// routing-aware orientation refinement. Phase 4.5 uses the **real
/// router** as its oracle, re-routing the whole sheet once per candidate
/// orientation. Its loops are all capped
/// (`refine.rs`: `MAX_SWEEPS = 4`, `MAX_ACTIVE = 4`,
/// `MAX_COMBINATIONS = 512`), so this is *not* the "unbounded router
/// segment growth" failure class — it is the **product** of ~10³ trial
/// routes and a per-route cost that is itself elevated on this fixture.
/// One route of this sheet costs ~50 ms (`--no-refine --no-verify` =
/// 0.09 s for the entire conversion), and the router logs
/// `cross-net overlap: nets 3/1 unresolved by single-track jog` — i.e.
/// every trial route runs the full V11/V12 conflict cascade to
/// exhaustion. 10³ × 50 ms ≈ the observed time.
///
/// Corroborating the "product" reading, a `--refine-iterations` sweep is
/// **non-monotone**: 0 → 16 s, 1 → 17 s, 20 → 5 s, 200 → 125 s. Cost
/// tracks *which placement the SA lands on* (how many at-risk elements
/// phase 4.5 then trial-routes, and how conflicted each route is), not
/// the iteration count.
///
/// Fixing it was explicitly out of scope for F0. The plausible levers,
/// in the order a fixer should try them, were (a) cache/memoise phase
/// 4.5's per-candidate route measurements, and (b) give the router's
/// conflict cascade an early-out when a candidate is already worse than
/// the incumbent — neither of which changes emitted geometry. **Both
/// have since landed** (`2d3c81b` and `6a18a8b` respectively), which is
/// where the 21× came from.
///
/// Still `#[ignore]`d — not because it is slow any more (it is 1.05 s),
/// but because un-ignoring it would fail on the stale floor. Run it with:
///
/// ```sh
/// cargo test -p spice2kicad --test f0_defects -- --ignored --nocapture
/// ```
#[test]
#[ignore = "STALE runtime lock: recorded ~112 s (unoptimised debug, 7f707e6), now 1.05 s on the optimised dev profile; see the doc comment (F0 runtime defect lock)"]
fn two_stage_amp_conversion_is_pathologically_slow() {
    let tmp = tempdir("two_stage_amp");
    let start = std::time::Instant::now();
    let out = convert("two_stage_amp", &tmp, &[]);
    let elapsed = start.elapsed().as_secs_f64();
    let stderr = String::from_utf8_lossy(&out.stderr);
    eprintln!(
        "two_stage_amp: converted in {elapsed:.1} s (exit {:?})",
        out.status.code()
    );

    assert!(
        out.status.success(),
        "`two_stage_amp` failed to convert. This lock records a fixture that is \
         CORRECT but slow; a conversion failure is a different, worse defect and \
         needs its own diagnosis.\nstderr:\n{stderr}",
    );
    assert!(
        elapsed > TWO_STAGE_AMP_SLOW_FLOOR_SECS,
        "UNEXPECTED PASS: `two_stage_amp` converted in {elapsed:.1} s, under the \
         {TWO_STAGE_AMP_SLOW_FLOOR_SECS} s floor this lock records (it was 112 s on \
         7f707e6, unoptimised debug build). The phase-4.5 trial-routing cost is \
         FIXED. Register the fixture \
         across the fixture-enumerating tables in crates/spice2kicad/tests/ with \
         zero-slack baselines and DELETE this test. See docs/v0.2-roadmap.md § F0.",
    );
}
