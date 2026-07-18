//! Post-emit verification: let KiCad judge the schematic we just wrote.
//!
//! Every stage before this one reasons about a *model* of KiCad — where
//! text renders, which wires connect. Models drift, and when they do the
//! failure is silent: the file is well-formed, it opens, and the circuit
//! is wrong. Two such drifts shipped in this converter. Labels rendered
//! in the wrong direction for months because the geometry model and the
//! test that graded it were the same code. A branch ending on a trunk's
//! mid-span emitted an electrically split net, because the router
//! believed a junction dot connected it and KiCad connects wires only at
//! endpoints.
//!
//! Neither was caught by reasoning harder. Both were caught by running
//! KiCad. So we run KiCad — on every conversion, not just in tests, since
//! the tests only protect this repo's nine fixtures and users convert
//! their own netlists.
//!
//! The check is connectivity, compared as a **partition**: two pins share
//! a net in the emitted schematic exactly when they share one in the
//! source. Net *names* are deliberately ignored, because KiCad renames
//! freely (`out` becomes `/out`, `0` becomes `GND`) and those renames are
//! correct.
//!
//! A mismatch is a hard error and never a silent repair: it means a
//! pipeline bug, and the whole point is that it must surface. The written
//! file is left in place as the debugging artifact.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

/// A pin, named `REFDES.terminal-index`.
type Pin = String;

/// The result of comparing emitted connectivity against the source.
pub struct Report {
    /// Net-groups present in the source but not the emitted schematic.
    pub missing: Vec<BTreeSet<Pin>>,
    /// Net-groups present in the emitted schematic but not the source.
    pub extra: Vec<BTreeSet<Pin>>,
    /// Elements the source declares that the schematic does not contain.
    pub dropped: Vec<String>,
}

impl Report {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.missing.is_empty() && self.extra.is_empty() && self.dropped.is_empty()
    }
}

/// True when `kicad-cli` is callable. Verification degrades to a no-op
/// when it is absent rather than failing the conversion — the tool is a
/// soft dependency, used to judge output, not to produce it.
#[must_use]
pub fn kicad_cli_available() -> bool {
    Command::new("kicad-cli")
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Export `sch` to a SPICE netlist via `kicad-cli`.
fn export_netlist(sch: &Path) -> Result<String, String> {
    let out = sch.with_extension("verify.cir");
    let status = Command::new("kicad-cli")
        .args(["sch", "export", "netlist", "--format", "spice", "-o"])
        .arg(&out)
        .arg(sch)
        .output()
        .map_err(|e| format!("running kicad-cli: {e}"))?;
    if !status.status.success() {
        return Err(format!(
            "kicad-cli netlist export failed: {}",
            String::from_utf8_lossy(&status.stderr).trim()
        ));
    }
    let body = std::fs::read_to_string(&out).map_err(|e| format!("reading {}: {e}", out.display()));
    let _ = std::fs::remove_file(&out);
    body
}

/// How many leading tokens after the refdes are nodes, by device letter.
/// `None` marks a device whose arity is variable or whose instance the
/// emitter may flatten (subckt calls) — not comparable.
fn node_count(refdes: &str) -> Option<usize> {
    match refdes.chars().next()?.to_ascii_uppercase() {
        'R' | 'C' | 'L' | 'D' | 'V' | 'I' => Some(2),
        'Q' => Some(3),
        // MOSFET (drain/gate/source/bulk) and the controlled sources
        // (two output nodes, two controlling nodes) all take four.
        'M' | 'E' | 'G' | 'F' | 'H' => Some(4),
        _ => None,
    }
}

/// Parse `refdes -> nodes` out of SPICE text.
fn parse_spice(text: &str) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty()
            || line.starts_with('*')
            || line.starts_with('+')
            || line.starts_with('.')
        {
            continue;
        }
        let code = line.split(';').next().unwrap_or("").trim();
        let mut toks = code.split_whitespace();
        let Some(refdes) = toks.next() else { continue };
        let Some(n) = node_count(refdes) else {
            continue;
        };
        let nodes: Vec<String> = toks.take(n).map(str::to_ascii_lowercase).collect();
        if nodes.len() == n {
            out.insert(refdes.to_ascii_uppercase(), nodes);
        }
    }
    out
}

/// Group pins by the net they sit on, discarding the net's name.
fn partition(elements: &BTreeMap<String, Vec<String>>) -> BTreeSet<BTreeSet<Pin>> {
    let mut by_net: BTreeMap<&str, BTreeSet<Pin>> = BTreeMap::new();
    for (refdes, nodes) in elements {
        for (i, node) in nodes.iter().enumerate() {
            by_net
                .entry(node.as_str())
                .or_default()
                .insert(format!("{refdes}.{i}"));
        }
    }
    by_net.into_values().collect()
}

/// Compare the emitted schematic's connectivity against `expected`.
///
/// `expected` is `refdes -> node names` for the elements the schematic is
/// supposed to contain — the caller supplies it from the resolved
/// netlist, already filtered for annotation-driven omissions (`ignore`
/// drops an element, `power` turns a source into rail glyphs).
pub fn check_connectivity(
    sch: &Path,
    expected: &BTreeMap<String, Vec<String>>,
) -> Result<Report, String> {
    let emitted = parse_spice(&export_netlist(sch)?);

    // Only devices this parser understands can be judged missing. A
    // subckt call has variable arity and its instance may be flattened
    // into a hierarchical sheet, so `X1` legitimately has no counterpart
    // in the exported netlist — reporting it would make the check cry
    // wolf on every hierarchical fixture.
    let dropped: Vec<String> = expected
        .keys()
        .filter(|k| node_count(k).is_some() && !emitted.contains_key(*k))
        .cloned()
        .collect();

    // Compare only elements both sides agree exist with the same pin
    // count: an element legitimately removed or flattened is not a wiring
    // error, and claiming otherwise would make the check cry wolf.
    let shared: BTreeSet<&String> = expected
        .keys()
        .filter(|k| {
            emitted
                .get(*k)
                .is_some_and(|e| e.len() == expected[*k].len())
        })
        .collect();
    let restrict = |m: &BTreeMap<String, Vec<String>>| -> BTreeMap<String, Vec<String>> {
        m.iter()
            .filter(|(k, _)| shared.contains(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };

    let want = partition(&restrict(expected));
    let got = partition(&restrict(&emitted));
    Ok(Report {
        missing: want.difference(&got).cloned().collect(),
        extra: got.difference(&want).cloned().collect(),
        dropped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_ignores_net_renaming() {
        let a = parse_spice("R1 in out 1k\nC1 out 0 100n\n");
        let b = parse_spice("R1 in /out 1k\nC1 /out GND 100n\n");
        assert_eq!(partition(&a), partition(&b));
    }

    #[test]
    fn partition_catches_a_moved_connection() {
        let a = parse_spice("R1 in out 1k\nC1 out 0 100n\n");
        let b = parse_spice("R1 in out 1k\nC1 in 0 100n\n");
        assert_ne!(partition(&a), partition(&b));
    }

    #[test]
    fn partition_catches_a_split_net() {
        // The measured failure: one pin drops off a three-pin net.
        let a = parse_spice("R1 c out 1k\nC1 c 0 100n\nR2 c 0 10k\n");
        let b = parse_spice("R1 dangling out 1k\nC1 c 0 100n\nR2 c 0 10k\n");
        assert_ne!(partition(&a), partition(&b));
    }
}
