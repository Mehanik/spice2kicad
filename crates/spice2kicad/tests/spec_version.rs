//! CLI-level annotation-spec version handshake (spec §4.7).
//!
//! Exercises the wiring added between parse and resolve in `main.rs`:
//! an unsupported `*@spec version=` is a hard error (E911) that exits
//! non-zero; an absent directive or a matching version converts as
//! normal. Uses the `-t netlist` target so no symbol libraries are
//! needed.

use std::path::Path;
use std::process::Command;

mod common;

/// Write a one-off deck into its own self-deleting directory. The
/// returned guard must stay bound for as long as the CLI needs the file
/// (the converter also drops its `.net` output beside it).
fn write_input(name: &str, body: &str) -> common::Emitted {
    let dir = common::TempDir::new("specver", name);
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write input");
    common::Emitted::new(dir, path)
}

fn run_netlist(src: &Path) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_spice2kicad");
    Command::new(bin)
        .arg(src)
        .arg("-t")
        .arg("netlist")
        .output()
        .expect("spawn spice2kicad")
}

#[test]
fn cli_current_version_converts() {
    let src = write_input(
        "ver_ok.cir",
        "* t\n*@spec version=0.1\nR1 a b 1k\nV1 a 0 5\n.end\n",
    );
    let out = run_netlist(&src);
    assert!(
        out.status.success(),
        "expected success; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn cli_absent_version_converts() {
    let src = write_input("ver_absent.cir", "* t\nR1 a b 1k\nV1 a 0 5\n.end\n");
    let out = run_netlist(&src);
    assert!(
        out.status.success(),
        "absent directive must convert; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("E911"),
        "absent directive must not emit E911"
    );
}

#[test]
fn cli_unsupported_version_errors() {
    let src = write_input(
        "ver_bad.cir",
        "* t\n*@spec version=2.0\nR1 a b 1k\nV1 a 0 5\n.end\n",
    );
    let out = run_netlist(&src);
    assert!(
        !out.status.success(),
        "unsupported version must exit non-zero"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("E911"),
        "unsupported version must emit E911; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
