//! Shared helpers for round-trip tests.
//!
//! The two non-trivial pieces here are:
//!   * [`Canonical`] — a graph-shaped view of a SPICE netlist that drops
//!     anything irrelevant to topology (sim directives, comments, ignored
//!     elements, cosmetic value formatting) so two netlists from different
//!     sides of a round-trip can be meaningfully compared.
//!   * [`Canonical::matches`] — equivalence under net-renaming. Refdes and
//!     pin-index are stable across a SPICE → KiCad → SPICE round-trip;
//!     net *labels* are not. We compare the partitions induced on
//!     `(refdes, pin_index)` pairs by net membership.
//!
//! Used by `roundtrip.rs`. Kept private to the test crate.

#![allow(dead_code)]

pub mod sexp;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

/// One element terminal: `R1` pin 1, `Q3` pin 2 (collector), …
type Terminal = (String, usize);

#[derive(Debug, Clone)]
pub struct Element {
    pub refdes: String,
    pub kind: char, // R, C, L, V, I, D, Q, M, J, X — first char of refdes, uppercased
    pub value: Option<String>,
    pub nodes: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Canonical {
    pub elements: Vec<Element>,
}

impl Canonical {
    /// Parse a SPICE-shaped file into canonical form.
    ///
    /// Lossy by design: drops simulation directives, models, comments,
    /// `*@ignore`d elements, and `;@ ignore` trailing tags.
    pub fn from_spice(source: &str) -> Self {
        let mut elements = Vec::new();
        // Two-pass: collect ignored refdes from `;@ ignore` tags first.
        let mut ignored: BTreeSet<String> = BTreeSet::new();
        let logical = join_continuations(source);

        for line in &logical {
            if has_ignore_tag(line) {
                if let Some(refdes) = first_token(strip_comment(line)) {
                    ignored.insert(refdes.to_ascii_uppercase());
                }
            }
        }

        // First pass: gather subckt definitions so we can expand top-level
        // `X<n>` instances into their body elements (matching what KiCad's
        // hierarchical-netlist exporter does on the round-tripped side).
        let subckts = collect_subckts(&logical, &ignored);

        let mut in_subckt = false;
        for line in &logical {
            let body = strip_comment(line).trim();
            if body.is_empty() {
                continue;
            }
            let lower = body.to_ascii_lowercase();
            if lower.starts_with(".subckt") {
                in_subckt = true;
                continue;
            }
            if lower.starts_with(".ends") {
                in_subckt = false;
                continue;
            }
            // Skip everything inside a `.subckt` block — body elements are
            // accounted for by `expand_subckt` when each X instance is
            // processed below.
            if in_subckt {
                continue;
            }
            // Skip directives, models, subckt headers.
            let first = body.chars().next().unwrap();
            if first == '.' || first == '*' {
                continue;
            }
            let Some(refdes) = first_token(body) else {
                continue;
            };
            let refdes_up = refdes.to_ascii_uppercase();
            if ignored.contains(&refdes_up) {
                continue;
            }
            let kind = refdes_up.chars().next().unwrap();
            let tokens: Vec<&str> = body.split_whitespace().collect();
            if kind == 'X' {
                // Last positional token is the subckt name; everything in
                // between is the port-net list. Expand the body if we
                // recognise the subckt.
                if let Some(model) = tokens.last()
                    && let Some(def) = subckts.get(&model.to_ascii_uppercase())
                {
                    let parent_nets: Vec<String> = tokens[1..tokens.len() - 1]
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect();
                    elements.extend(expand_subckt(def, &parent_nets));
                }
                continue;
            }
            let arity = element_arity(kind);
            if tokens.len() < 1 + arity {
                continue;
            }
            let nodes = tokens[1..=arity].iter().map(|s| (*s).to_string()).collect();
            // Value: for two-terminal passives, the next token. Coarse — we
            // mostly compare topology, so a missing/garbled value is fine.
            let value = tokens.get(1 + arity).map(|s| normalize_value(s));
            elements.push(Element {
                refdes: refdes_up,
                kind,
                value,
                nodes,
            });
        }

        elements.sort_by(|a, b| a.refdes.cmp(&b.refdes));
        Self { elements }
    }

    /// Compare two canonical netlists for round-trip equivalence.
    /// Returns Ok(()) on match, Err(message) describing the first mismatch.
    pub fn matches(&self, other: &Self) -> Result<(), String> {
        // 1. Same set of refdes + kinds.
        let mine: BTreeMap<&str, &Element> = self
            .elements
            .iter()
            .map(|e| (e.refdes.as_str(), e))
            .collect();
        let theirs: BTreeMap<&str, &Element> = other
            .elements
            .iter()
            .map(|e| (e.refdes.as_str(), e))
            .collect();

        for r in mine.keys() {
            if !theirs.contains_key(r) {
                return Err(format!("element {r} present in lhs, missing in rhs"));
            }
        }
        for r in theirs.keys() {
            if !mine.contains_key(r) {
                return Err(format!("element {r} present in rhs, missing in lhs"));
            }
        }
        for (r, lhs) in &mine {
            let rhs = theirs[r];
            if lhs.kind != rhs.kind {
                return Err(format!("{r}: kind {} vs {}", lhs.kind, rhs.kind));
            }
            if lhs.nodes.len() != rhs.nodes.len() {
                return Err(format!(
                    "{r}: arity {} vs {}",
                    lhs.nodes.len(),
                    rhs.nodes.len()
                ));
            }
            // Values are cosmetic; warn-only would be nice, but keep test
            // strict for now — emitter must round-trip values intact.
            if lhs.value != rhs.value {
                return Err(format!("{r}: value {:?} vs {:?}", lhs.value, rhs.value));
            }
        }

        // 2. Net partitions must agree.
        // Ground (`0`) is special: same in both worlds. Other names may differ.
        let lhs_part = partition(self);
        let rhs_part = partition(other);

        if lhs_part != rhs_part {
            return Err(format!(
                "net partitions differ\n  lhs: {lhs_part:?}\n  rhs: {rhs_part:?}"
            ));
        }
        Ok(())
    }
}

/// Build a partition of `(refdes, pin)` pairs by their net.
/// The partition is represented as a set of sets, so net *labels* drop out
/// — only equivalence classes remain. Ground (`0`) keeps its identity by
/// being the only class containing the synthetic `("0", 0)` sentinel.
fn partition(c: &Canonical) -> BTreeSet<BTreeSet<Terminal>> {
    let mut by_net: BTreeMap<String, BTreeSet<Terminal>> = BTreeMap::new();
    for e in &c.elements {
        for (i, n) in e.nodes.iter().enumerate() {
            let key = if is_ground(n) {
                "0".to_string()
            } else {
                n.clone()
            };
            by_net.entry(key).or_default().insert((e.refdes.clone(), i));
        }
    }
    // Tag the ground class so it can't accidentally match a renamed class.
    if let Some(g) = by_net.get_mut("0") {
        g.insert(("__GND__".to_string(), 0));
    }
    by_net.into_values().collect()
}

fn is_ground(n: &str) -> bool {
    matches!(n, "0" | "GND" | "gnd" | "Gnd")
}

/// Collect every `.subckt` block as `(name, ports, body-elements)`.
/// Body elements are stored in the same lossy form the canonicalizer
/// uses for top-level elements, so [`expand_subckt`] can plug them
/// straight into the partition graph after net substitution.
struct SubcktDef {
    ports: Vec<String>,
    body: Vec<Element>,
}

fn collect_subckts(logical: &[String], ignored: &BTreeSet<String>) -> BTreeMap<String, SubcktDef> {
    let mut out: BTreeMap<String, SubcktDef> = BTreeMap::new();
    let mut current: Option<(String, SubcktDef)> = None;
    for line in logical {
        let body = strip_comment(line).trim();
        if body.is_empty() {
            continue;
        }
        let lower = body.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix(".subckt") {
            // Re-tokenize against the *original* (case-preserved) line so
            // names round-trip cleanly.
            let _ = rest;
            let toks: Vec<&str> = body.split_whitespace().collect();
            if toks.len() >= 2 {
                let name = toks[1].to_ascii_uppercase();
                let ports = toks[2..].iter().map(|s| (*s).to_string()).collect();
                current = Some((
                    name,
                    SubcktDef {
                        ports,
                        body: Vec::new(),
                    },
                ));
            }
            continue;
        }
        if lower.starts_with(".ends") {
            if let Some((name, def)) = current.take() {
                out.insert(name, def);
            }
            continue;
        }
        let Some((_, def)) = current.as_mut() else {
            continue;
        };
        let first = body.chars().next().unwrap();
        if first == '.' || first == '*' {
            continue;
        }
        let Some(refdes) = first_token(body) else {
            continue;
        };
        let refdes_up = refdes.to_ascii_uppercase();
        if ignored.contains(&refdes_up) {
            continue;
        }
        let kind = refdes_up.chars().next().unwrap();
        if kind == 'X' {
            // Nested subckt instances aren't expanded here — KISS, the
            // first level is enough for the current fixture set.
            continue;
        }
        let arity = element_arity(kind);
        let tokens: Vec<&str> = body.split_whitespace().collect();
        if tokens.len() < 1 + arity {
            continue;
        }
        let nodes = tokens[1..=arity].iter().map(|s| (*s).to_string()).collect();
        let value = tokens.get(1 + arity).map(|s| normalize_value(s));
        def.body.push(Element {
            refdes: refdes_up,
            kind,
            value,
            nodes,
        });
    }
    out
}

/// Expand one `X<n>` instance into the body elements of its `.subckt`
/// definition with port nets remapped to the parent's nets.
fn expand_subckt(def: &SubcktDef, parent_nets: &[String]) -> Vec<Element> {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for (port, parent) in def.ports.iter().zip(parent_nets.iter()) {
        map.insert(port.clone(), parent.clone());
    }
    def.body
        .iter()
        .map(|el| Element {
            refdes: el.refdes.clone(),
            kind: el.kind,
            value: el.value.clone(),
            nodes: el
                .nodes
                .iter()
                .map(|n| map.get(n).cloned().unwrap_or_else(|| n.clone()))
                .collect(),
        })
        .collect()
}

fn element_arity(kind: char) -> usize {
    match kind {
        'Q' | 'J' => 3,
        // M (MOSFET): d g s b. E (VCVS) and G (VCCS) take four nodes:
        // out+, out-, ctrl+, ctrl-.
        'M' | 'E' | 'G' => 4,
        // R, C, L, V, I, D, X (subckt, variadic — caller doesn't compare topology of
        // X instances yet) and anything else default to 2.
        _ => 2,
    }
}

/// SPICE allows physical-line continuation with `+`. Collapse to logical lines.
fn join_continuations(source: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in source.lines() {
        if let Some(rest) = raw.strip_prefix('+') {
            if let Some(last) = out.last_mut() {
                last.push(' ');
                last.push_str(rest.trim_start());
                continue;
            }
        }
        out.push(raw.to_string());
    }
    out
}

fn strip_comment(line: &str) -> &str {
    line.split(';').next().unwrap_or("")
}

fn first_token(s: &str) -> Option<&str> {
    s.split_whitespace().next()
}

fn has_ignore_tag(line: &str) -> bool {
    // ;@ ignore  or  ;@ignore — anywhere on the line.
    let Some(idx) = line.find(";@") else {
        return false;
    };
    let tail = &line[idx + 2..];
    tail.split_whitespace().next() == Some("ignore")
}

/// Normalize SPICE value tokens so `1k`, `1K`, `1000` compare equal.
/// Coarse but enough for the topology-focused comparison we do here.
///
/// Also strips a leading `<refdes>.` prefix from model names so KiCad's
/// inline-model rename (`QGENERIC` → `Q1.QGENERIC`) doesn't trip the
/// round-trip comparator. The trailing model name is what's
/// semantically meaningful; the prefix is a KiCad-side
/// uniquification artefact.
fn normalize_value(s: &str) -> String {
    let s = s.trim().to_ascii_lowercase();
    // SPICE `4k7` infix-decimal: a single suffix letter sandwiched between
    // an integer and a fractional part (`4k7` → `4.7k`). Rewrite before
    // splitting so the rest of the pipeline sees a familiar form.
    let s = rewrite_infix_decimal(&s);
    let (num, suffix) = split_suffix(&s);
    let Ok(mut v) = num.parse::<f64>() else {
        // Non-numeric: model name or expression. Strip a leading
        // `prefix.` if the result is still non-numeric (the rename is
        // monotonic — KiCad never strips a prefix the user wrote).
        if let Some((_, rest)) = s.rsplit_once('.') {
            if !rest.is_empty() && rest.parse::<f64>().is_err() {
                return rest.to_string();
            }
        }
        return s;
    };
    let mult = match suffix {
        "f" => 1e-15,
        "p" => 1e-12,
        "n" => 1e-9,
        "u" | "µ" => 1e-6,
        "m" => 1e-3,
        "k" => 1e3,
        "meg" => 1e6,
        "g" => 1e9,
        "t" => 1e12,
        "" => 1.0,
        _ => return s,
    };
    v *= mult;
    format!("{v:e}")
}

/// Rewrite `4k7` → `4.7k`, leaving anything else unchanged. The suffix
/// must be one of the engineering letters we already recognise.
fn rewrite_infix_decimal(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut int_end = 0;
    while int_end < bytes.len() && bytes[int_end].is_ascii_digit() {
        int_end += 1;
    }
    if int_end == 0 || int_end == bytes.len() {
        return s.to_string();
    }
    let suffix_char = bytes[int_end];
    if !matches!(
        suffix_char,
        b'f' | b'p' | b'n' | b'u' | b'm' | b'k' | b'g' | b't'
    ) {
        return s.to_string();
    }
    let after = &s[int_end + 1..];
    if after.is_empty() || !after.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return s.to_string();
    }
    format!("{}.{after}{}", &s[..int_end], suffix_char as char)
}

fn split_suffix(s: &str) -> (&str, &str) {
    let cut = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+' || *c == 'e'))
        .map_or(s.len(), |(i, _)| i);
    (&s[..cut], &s[cut..])
}

// --- driver bits ---------------------------------------------------------

/// Run `spice2kicad` against a fixture, return the path to the .kicad_sch.
pub fn spice_to_kicad(fixture: &Path, out_dir: &Path) -> Result<std::path::PathBuf, String> {
    let stem = fixture.file_stem().unwrap().to_string_lossy();
    let out = out_dir.join(format!("{stem}.kicad_sch"));
    let bin = env!("CARGO_BIN_EXE_spice2kicad");
    // Tests share the kicad-symbols fixture libraries (Device + Simulation_SPICE).
    let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .to_path_buf();
    let lib_dir = workspace.join("crates/kicad-symbols/tests/fixtures");
    let status = Command::new(bin)
        .arg(fixture)
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
        .status()
        .map_err(|e| format!("failed to invoke spice2kicad: {e}"))?;
    if !status.success() {
        return Err(format!("spice2kicad exited with {status}"));
    }
    Ok(out)
}

/// Run `kicad-cli sch export netlist --format spice` on a schematic.
/// Returns Ok(None) if `kicad-cli` is not installed (caller may skip).
pub fn kicad_to_spice(sch: &Path, out_dir: &Path) -> Result<Option<String>, String> {
    if which_kicad_cli().is_none() {
        return Ok(None);
    }
    let stem = sch.file_stem().unwrap().to_string_lossy();
    let out = out_dir.join(format!("{stem}.roundtrip.cir"));
    let status = Command::new("kicad-cli")
        .args(["sch", "export", "netlist", "--format", "spice", "-o"])
        .arg(&out)
        .arg(sch)
        .status()
        .map_err(|e| format!("failed to invoke kicad-cli: {e}"))?;
    if !status.success() {
        return Err(format!("kicad-cli exited with {status}"));
    }
    let body =
        std::fs::read_to_string(&out).map_err(|e| format!("reading {}: {e}", out.display()))?;
    Ok(Some(body))
}

fn which_kicad_cli() -> Option<()> {
    Command::new("kicad-cli")
        .arg("version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| ())
}

pub fn require_kicad_cli() -> bool {
    std::env::var("REQUIRE_KICAD_CLI")
        .map(|v| v == "1")
        .unwrap_or(false)
}
