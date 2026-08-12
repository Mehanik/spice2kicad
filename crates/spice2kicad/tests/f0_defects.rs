//! Tier-0 refusal, and the benchmark defect locks — currently **none**.
//!
//! # What this file is for
//!
//! Two jobs, and the first one is now empty:
//!
//! 1. **Defect locks.** A benchmark fixture the converter cannot grade
//!    is held here with its measured characterisation and an
//!    *unexpected-pass tripwire*, so the day the defect is fixed the
//!    lock fails and tells you to promote the fixture rather than
//!    letting a stale exclusion rot. This is a lock, never a skip.
//! 2. **The unconditional Tier-0 refusal**, at the CLI boundary — see
//!    [`tier0_geometry_is_refused_without_kicad_cli`].
//!
//! # Lock history (all discharged)
//!
//! * **`two_stage_amp`** — held on a *runtime* defect (it converted
//!   correctly but took ~112 s). Both levers its lock named landed and
//!   the conversion is now ~1.0 s. Promoted.
//! * **`shunt_feedback_amp` — Tier-0 (ADR-20).** The converter refused
//!   it: `MERGE: source nets ["c", "vcc"]`. ADR-20 attributed the
//!   residual to the owner-gated R-5 rail-pin defect, on the strength of
//!   phase 4.5's oracle reporting the *incoming* placement as already
//!   `severed = 2`. That reading has been superseded: phase 4.5's oracle
//!   **is the real router**, so `severed = 2` was measuring the router's
//!   own tree fragmentation, not the placement. With ADR-24 the same
//!   placement measures clean and the fixture converts. Promoted.
//! * **`sallen_key_driven` — Tier-0 (ADR-24).** The Sallen-Key filter
//!   with its stimulus DRAWN instead of `;@ ignore`d. `MERGE: source
//!   nets ["np", "out"]` at the default iteration count, and — the part
//!   that mattered — a **SPLIT of `np` from the bare deterministic
//!   seed**, with no annealer involved at all. Both were one router
//!   defect: net `np`'s exact Hwang Steiner point is the coordinate-wise
//!   median of its three pins, that median landed on `RA`'s foreign
//!   `inv` pin, and the per-segment conflict jog fragmented the tree
//!   from there. Promoted.
//!
//! R-5 (the rail pin facing into the circuit) is **not** fixed and is
//! still owner-gated; `shunt_feedback_amp` carries an XFAIL for it in
//! `tests/common/xfail.rs`, which is where a known Tier-1 aesthetic
//! defect belongs. What ADR-24 removes is the claim that R-5 was what
//! made this fixture *unconvertible*.
//!
//! Both fixtures are committed **unmodified** and are now registered
//! across every fixture-enumerating verifier with zero-slack baselines.

use std::fs;
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
/// **Placer-aware (ADR-23).** This file keeps its own conversion driver
/// — it needs the raw `Output`, which `common::spice_to_kicad` does not
/// surface — so it must forward `common::placer_args()` itself or the
/// `S2K_PLACER=<name>` selection silently does not reach it.
///
/// Note that `--no-layout-cache` is **not** forced here, unlike the
/// helper this file used to carry: the one remaining test needs the
/// ADR-4 sidecar to be *read*, because that is how it installs a
/// deterministic Tier-0-broken placement without depending on any
/// fixture being drawn badly. Callers that want a fresh conversion pass
/// it in `extra`.
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

/// **Regression test for the ADR-21 / ADR-22 hole.** A geometry that
/// draws the wrong circuit must be refused **unconditionally**: not
/// behind `--no-verify`, not behind an env var, and not dependent on
/// `kicad-cli` being installed. The refusal must also happen *before*
/// any bytes reach disk, so no later step can pick up a schematic the
/// converter has already judged wrong (ADR-21 D3).
///
/// # Why it no longer rides on a broken fixture
///
/// This test used to point at `shunt_feedback_amp`, the one fixture
/// whose own placement tripped a Tier-0 residue. Its doc said what to do
/// the day that fixture was promoted: *"point it at any fixture that
/// still trips a Tier-0 residue, or delete it only once no fixture
/// can."* ADR-24 fixed the router defect and **no fixture can any
/// more** — which would leave the property untested precisely because
/// the converter got better, the worst possible reason to lose a gate.
///
/// So the broken geometry is now *installed*, not *found*: the ADR-4
/// layout-cache sidecar stacks `R1` and `C1` of `rc_lowpass` so that
/// `C1`'s ground pin lands exactly on `R1`'s input pin — net `0` on net
/// `in`, the two ends of the chain. That is a placer-independent, deterministic
/// Tier-0 fault which no router pass can undo — `spice-route` moves
/// wires, not pins — and it is exactly the `PinCoincidence` pre-flight
/// ADR-22 D3 kept for this class.
///
/// **Scope, stated honestly.** This covers the *CLI contract*: a Tier-0
/// geometric fault exits non-zero with `--no-verify` and leaves nothing
/// on disk. It does **not** exercise the ADR-22 partition
/// reconstruction itself — the SPLIT and MERGE findings are graded by
/// `kicad_emitter`'s own unit tests, which assert
/// `EmitError::NetPartition` for each, and by
/// `roundtrip_connectivity.rs` on every graded fixture. Those two halves
/// together are what ADR-21/22 asked for; neither alone is.
#[test]
fn tier0_geometry_is_refused_without_kicad_cli() {
    let tmp = tempdir("tier0-unconditional");
    let src = fixtures_dir().join("rc_lowpass.cir");
    let circuit = fs::canonicalize(&src)
        .unwrap_or(src)
        .to_string_lossy()
        .into_owned();
    // `<basename>.layout.json`, next to the output the CLI is about to
    // write. `C1` sits six grid cells above `R1` on the same column, so
    // `C1`'s lower pin (net `0`) lands exactly on `R1`'s upper pin (net
    // `in`) — the two FAR ENDS of the chain, shorted.
    //
    // Note the parts are *stacked*, not superimposed. Pinning both to
    // one origin does not work: the pins coincide but so do the bodies,
    // and `spice_layout::legalize` separates overlapping bodies even for
    // pinned elements, so the fault is repaired before the emitter ever
    // sees it. Six cells apart the bodies clear (half-height 2.54 mm,
    // pin reach 3.81 mm) and only the pins touch, which the legalizer
    // has no term for. That asymmetry is itself worth knowing: body
    // overlap is legalized, pin coincidence is not.
    let sidecar = tmp.join("rc_lowpass.layout.json");
    fs::write(
        &sidecar,
        format!(
            r#"{{"version":3,"circuit":{circuit:?},"positions":{{
                 "R1":{{"x":20,"y":26,"rotation":0,"mirror":false}},
                 "C1":{{"x":20,"y":20,"rotation":0,"mirror":false}}
               }},"page_shifts":{{}}}}"#
        ),
    )
    .expect("write layout sidecar");

    let out = convert("rc_lowpass", &tmp, &["--no-verify"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        stderr.contains("layout cache hit"),
        "the sidecar was not loaded, so this test measured an ordinary conversion \
         rather than the pinned Tier-0 geometry it installs. Check the sidecar path \
         and the `circuit` identity field.\nstderr:\n{stderr}",
    );
    assert!(
        stderr.contains("(25.40, 29.21): 0 + in"),
        "the pinned placement did not produce the recorded `0`/`in` pin coincidence, \
         so whatever this test measured, it was not the fault it installs. Re-derive \
         the sidecar coordinates against the symbol pin geometry.\nstderr:\n{stderr}",
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "TIER-0 HOLE REOPENED: a placement whose pins coincide across two nets \
         exited {:?} under `--no-verify`. Coincident pins are a silent net merge on \
         KiCad load; the converter must refuse rather than emit it, with no \
         dependence on `kicad-cli` and no env gate. See ADR-21 / ADR-22.\nstderr:\n{stderr}",
        out.status.code(),
    );
    let emitted = tmp.join("rc_lowpass.kicad_sch");
    assert!(
        !emitted.exists(),
        "the converter refused (exit 1) but still wrote {}. A refusal must not \
         leave a schematic on disk that a later step could pick up.",
        emitted.display(),
    );
}

/// The control arm for the test above, and the thing that stops it
/// rotting into a tautology: **without** the sabotaged sidecar the very
/// same invocation converts cleanly and writes the file.
///
/// Without this, a change that made the CLI exit 1 on everything — a
/// missing library, a parse error, a panic — would leave the refusal
/// test green while the converter emitted nothing at all.
#[test]
fn the_same_invocation_converts_cleanly_without_the_sabotaged_sidecar() {
    let tmp = tempdir("tier0-control");
    let out = convert("rc_lowpass", &tmp, &["--no-verify", "--no-layout-cache"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the control conversion failed, so the refusal test above proves nothing.\
         \nstderr:\n{stderr}",
    );
    assert!(
        tmp.join("rc_lowpass.kicad_sch").exists(),
        "the control conversion exited 0 but wrote no schematic",
    );
}
