//! ngspice corpus smoke test.
//!
//! Set `NGSPICE_SRC` to a checkout root (e.g. `~/Projects/ngspice`).
//! The test walks every `.cir` under `$NGSPICE_SRC/tests/` and asserts
//! that our parser returns `Ok(_)` for each one. Designed to surface
//! ngspice-flavour syntax we haven't yet handled.
//!
//! With `NGSPICE_FAIL_FAST=1` the test stops at the first failure and
//! prints diagnostics; otherwise it collects every failure and reports
//! them all at the end.

mod common;

use std::path::PathBuf;

use common::{fid, fmt_diags};
use spice_parser::parse;

fn corpus_root() -> Option<PathBuf> {
    let raw = std::env::var("NGSPICE_SRC").ok()?;
    let root = PathBuf::from(raw).join("tests");
    if !root.is_dir() {
        eprintln!("NGSPICE_SRC/tests not found at {}", root.display());
        return None;
    }
    Some(root)
}

fn collect_cir_files(root: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_cir_files(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("cir") {
            out.push(p);
        }
    }
}

#[test]
fn ngspice_corpus_parses() {
    let Some(root) = corpus_root() else {
        eprintln!("NGSPICE_SRC unset — skipping ngspice corpus test");
        return;
    };
    let fail_fast = std::env::var("NGSPICE_FAIL_FAST").is_ok();

    let mut files = Vec::new();
    collect_cir_files(&root, &mut files);
    files.sort();

    let mut failures: Vec<(PathBuf, String)> = Vec::new();
    for path in &files {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                failures.push((path.clone(), format!("read error: {e}")));
                continue;
            }
        };
        match parse(&source, fid()) {
            Ok(_) => {}
            Err(diags) => {
                let msg = fmt_diags(&diags);
                assert!(!fail_fast, "first failure at {}: {msg}", path.display());
                failures.push((path.clone(), msg));
            }
        }
    }

    eprintln!(
        "ngspice corpus: {} files, {} failures",
        files.len(),
        failures.len()
    );

    if !failures.is_empty() {
        for (p, msg) in &failures {
            eprintln!("  FAIL {}: {msg}", p.display());
        }
        panic!("{} ngspice corpus files failed to parse", failures.len());
    }
}
