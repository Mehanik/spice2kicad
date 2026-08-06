//! F0 defect locks — the two benchmark fixtures the current converter
//! cannot grade, and why.
//!
//! F0 (see `docs/v0.2-roadmap.md` § "Findings / status log") added three
//! harder fixtures so the placer work has circuits with real headroom.
//! One of them — `rc_phase_shift` — converts and is fully registered
//! across the fixture-enumerating verifiers. The other two convert
//! *badly*, in two different ways, and are held here instead:
//!
//!  * **`shunt_feedback_amp` — Tier-0.** The converter's own post-emit
//!    connectivity check rejects its own output: the emitted schematic
//!    shorts the `b` (base) and `e` (emitter) nets. Deterministic, and
//!    caused by the stage-3 SA at its *default* iteration count.
//!  * **`two_stage_amp` — runtime.** It converts *correctly*, but takes
//!    ~112 s where the median fixture takes 0.4 s. Registering it across
//!    ~15 verifiers would add ~25 CPU-minutes to every suite run.
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

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tempdir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("spice2kicad-f0-{pid}-{seq}-{name}"));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
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

// --- shunt_feedback_amp: Tier-0 base/emitter short ------------------------

/// **Defect lock (Tier-0, V11).** Converting `shunt_feedback_amp` with
/// default settings emits a schematic that does not wire up the source
/// circuit, and the CLI's post-emit connectivity check rejects it:
///
/// ```text
/// route: conflict: net index 0 has endpoint conflicts left after 6 resolve iterations
/// ERROR: the emitted schematic does not wire up the source circuit.
///   net in the source but split in the schematic: {"CE.0", "Q1.2", "RE.0"}
///   net in the source but split in the schematic: {"CIN.1", "Q1.1", "RB.1", "RF.1"}
///   net in the schematic but not the source: {"CE.0", "CIN.1", "Q1.1", "Q1.2", …}
/// ```
///
/// The two source nets `e` and `b` are merged into one — a silent short
/// of the transistor's base to its emitter (V11: "geometric coincidence
/// must not silently short two nets"). Nothing is emitted that a user
/// could open; the CLI exits non-zero, which is the correct behaviour
/// for a converter that cannot honour Tier-0.
///
/// **Measured on `7f707e6`, deterministic**: three consecutive
/// `--no-layout-cache` runs produced byte-identical output and the same
/// non-zero exit.
///
/// **Localised to the stage-3 SA, not the router and not phase 4.5.**
/// Sweeping `--refine-iterations`: 0, 1, 20, 40, 60, 80, 100 and 400 all
/// convert **cleanly**; 150 and 200 (200 is the default) both fail.
/// `--no-refine` is clean too. The failure is therefore not a monotone
/// "more annealing is worse" gradient — it is one specific SA end-state
/// whose placement the router's conflict-resolution cascade cannot
/// legalise, so it gives up ("endpoint conflicts left after 6 resolve
/// iterations") and leaves two nets coincident. That is the
/// placement-vs-router disagreement recorded in MEMORY
/// "flow-orientation wall", here escalated from a Tier-2 aesthetic
/// disagreement to a Tier-0 correctness failure.
///
/// This test is deliberately **not** `#[ignore]`d: the failing
/// conversion costs ~4 s, well within the suite's budget, and an
/// `#[ignore]`d lock would never notice the day the defect is fixed.
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
    // Match the net-partition line, not the headline sentence: the CLI
    // wraps "…does not wire up the   source circuit." with a run of
    // spaces, so the obvious substring is not actually present.
    assert!(
        stderr.contains("net in the source but split in the schematic"),
        "`shunt_feedback_amp` failed to convert, but NOT with the recorded Tier-0 \
         connectivity error. This lock describes one specific defect; a different \
         failure is a new regression to diagnose, not this one.\nstderr:\n{stderr}",
    );
}

// --- two_stage_amp: pathological conversion time --------------------------

/// Conversion time above which `two_stage_amp` still counts as
/// reproducing the defect this lock records.
///
/// Measured 112 s on an otherwise-idle machine (debug build, `7f707e6`);
/// the floor is set an order of magnitude below that so machine speed
/// cannot make the lock flaky, while still being ~75× the median
/// fixture's 0.4 s.
const TWO_STAGE_AMP_SLOW_FLOOR_SECS: f64 = 15.0;

/// **Defect lock (runtime).** `two_stage_amp` converts *correctly* —
/// exit 0, connectivity check clean — but takes ~112 s where every v0.1
/// fixture takes 0.3–4.5 s. It is therefore committed but **not
/// registered**: ~15 fixture-enumerating verifiers each converting it
/// would add roughly 25 CPU-minutes to every `just test`.
///
/// **What was measured** (debug build, `7f707e6`, idle machine,
/// `--no-layout-cache`):
///
/// | conversion                              | wall  |
/// | --------------------------------------- | ----- |
/// | `two_stage_amp` (default)               | 112 s |
/// | `two_stage_amp --no-verify`             | 118 s |
/// | `two_stage_amp --no-refine`             | 0.43 s|
/// | `two_stage_amp --no-refine --no-verify` | 0.09 s|
/// | `rc_phase_shift` (default)              | 11.5 s|
/// | `common_emitter` (default)              | 4.5 s |
/// | median v0.1 fixture                     | 0.4 s |
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
/// **First-look diagnosis — a bounded product, not unbounded growth.**
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
/// Fixing it is explicitly out of scope for F0. The plausible levers, in
/// the order a fixer should try them, are (a) cache/memoise phase 4.5's
/// per-candidate route measurements, and (b) give the router's conflict
/// cascade an early-out when a candidate is already worse than the
/// incumbent — neither of which changes emitted geometry.
///
/// `#[ignore]`d because a 112 s test does not belong in the default
/// suite. Run it with:
///
/// ```sh
/// cargo test -p spice2kicad --test f0_defects -- --ignored --nocapture
/// ```
#[test]
#[ignore = "converts correctly but takes ~112 s; see the doc comment (F0 runtime defect lock)"]
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
         7f707e6). The phase-4.5 trial-routing cost is FIXED. Register the fixture \
         across the fixture-enumerating tables in crates/spice2kicad/tests/ with \
         zero-slack baselines and DELETE this test. See docs/v0.2-roadmap.md § F0.",
    );
}
