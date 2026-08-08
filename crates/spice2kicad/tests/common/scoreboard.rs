//! Measurement sink for the champion/challenger scoreboard (ADR-23).
//!
//! # Why a sink and not a second measuring binary
//!
//! Every verifier's metric function is a private `fn` in its own
//! integration-test *binary*, and Rust integration tests cannot import
//! each other. A separate "scoreboard" binary could therefore only
//! measure by (a) moving ~2 kLOC of measurement code into
//! `tests/common/`, or (b) re-implementing it. (b) is duplication — the
//! scoreboard would drift from the verifier it claims to mirror, which
//! is exactly the failure mode the project keeps paying for (MEMORY
//! "verify what a number measures"). (a) would also double the runtime,
//! because conversion is the dominant cost and is unmemoized: a
//! measuring binary re-converts all eleven fixtures for every metric it
//! computes.
//!
//! So the measurement stays where the assertion is, and the *verifier
//! itself* reports its number here. There is exactly one definition of
//! each metric, and it is the one the ratchet asserts on. The scoreboard
//! binary (`tests/scoreboard.rs`) is a pure aggregator over these
//! records.
//!
//! # Contract
//!
//! * Inert unless `S2K_SCOREBOARD_DIR` names a directory. On the normal
//!   `cargo test` path [`record`] does one failed `env::var` and
//!   returns, so the instrumentation costs nothing and changes nothing.
//! * [`record`] must be called *before* the assertion it feeds, and
//!   ideally outside any early-`return`/`panic` path, so a challenger
//!   that trips a ratchet still reports every fixture's number. A
//!   collect-then-assert verifier (the suite's dominant shape) gets this
//!   for free.
//! * Records are append-only `metric<TAB>fixture<TAB>value` lines. One
//!   file per process (each test binary is its own process); within a
//!   process a mutex serialises the threads.
//! * A metric/fixture pair recorded twice with *different* values is a
//!   defect the aggregator reports — it means two verifiers disagree
//!   about the same name.

use std::io::Write;
use std::sync::Mutex;

/// Serialises appends from the harness's test threads.
static LOCK: Mutex<()> = Mutex::new(());

/// Record one `(metric, fixture) -> value` measurement.
///
/// No-op unless `S2K_SCOREBOARD_DIR` is set. Never panics and never
/// fails a test: an unwritable sink degrades to a missing row in the
/// report, which the aggregator flags, rather than to a red suite.
pub fn record(metric: &str, fixture: &str, value: f64) {
    let Ok(dir) = std::env::var("S2K_SCOREBOARD_DIR") else {
        return;
    };
    if dir.trim().is_empty() {
        return;
    }
    let dir = std::path::PathBuf::from(dir);
    let _guard = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(format!("records-{}.tsv", std::process::id()));
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    // `{value}` on an f64 is round-trippable in Rust's Display; the
    // aggregator parses with `str::parse::<f64>`.
    let _ = writeln!(f, "{metric}\t{fixture}\t{value}");
}

/// Convenience for the common integer-count case.
pub fn record_count(metric: &str, fixture: &str, count: usize) {
    #[allow(clippy::cast_precision_loss)] // counts here are < 2^53 by construction
    record(metric, fixture, count as f64);
}
