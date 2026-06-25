//! Emit a KiCad-style flat netlist (`.net`).
//!
//! The output follows KiCad's `(export (version "E") …)` schema (see
//! `eeschema/netlist_exporters/netlist_exporter_xml.cpp` in the KiCad
//! source tree, which builds the same node tree that is serialised to a
//! `.net` file). We emit the two load-bearing sections — `(components …)`
//! and `(nets …)` — derived directly from the parsed SPICE netlist.
//!
//! A parsed SPICE netlist carries no library/footprint metadata, so each
//! `(comp …)` records only its refdes and value; the `libsource` `lib`
//! is left blank (KiCad accepts empty attributes here). Net membership is
//! reconstructed by walking every element's terminals: a SPICE node name
//! is a net, and each element terminal that lands on it becomes a
//! `(node (ref …) (pin …))` with `pin` set to the 1-based SPICE terminal
//! index.

use crate::EmitError;
use crate::sexpr::Sexpr;
use spice_parser::Netlist;
use spice_parser::ast::{Element, ElementKind, Value};

/// Render a parsed SPICE netlist as a KiCad `.net` S-expression string.
pub fn emit(netlist: &Netlist) -> Result<String, EmitError> {
    let export = Sexpr::List(vec![
        Sexpr::Atom("export".to_string()),
        Sexpr::List(vec![
            Sexpr::Atom("version".to_string()),
            Sexpr::QString("E".to_string()),
        ]),
        components(netlist),
        nets(netlist),
    ]);

    Ok(export.to_pretty())
}

/// `(components (comp (ref …) (value …) (libsource …)) …)`
fn components(netlist: &Netlist) -> Sexpr {
    let mut comps = vec![Sexpr::Atom("components".to_string())];

    for el in schematic_elements(netlist) {
        comps.push(Sexpr::List(vec![
            Sexpr::Atom("comp".to_string()),
            Sexpr::List(vec![
                Sexpr::Atom("ref".to_string()),
                Sexpr::QString(el.designator.clone()),
            ]),
            Sexpr::List(vec![
                Sexpr::Atom("value".to_string()),
                Sexpr::QString(value_text(el)),
            ]),
            Sexpr::List(vec![
                Sexpr::Atom("libsource".to_string()),
                Sexpr::List(vec![
                    Sexpr::Atom("lib".to_string()),
                    Sexpr::QString(String::new()),
                ]),
                Sexpr::List(vec![
                    Sexpr::Atom("part".to_string()),
                    Sexpr::QString(el.designator.clone()),
                ]),
            ]),
        ]));
    }

    Sexpr::List(comps)
}

/// `(nets (net (code …) (name …) (node (ref …) (pin …))) …)`
fn nets(netlist: &Netlist) -> Sexpr {
    use std::collections::HashMap;

    // Collect nets in first-seen order so output is deterministic and
    // mirrors the source. `members[name]` = [(refdes, 1-based pin)].
    let mut order: Vec<String> = Vec::new();
    let mut members: HashMap<String, Vec<(String, usize)>> = HashMap::new();

    for el in schematic_elements(netlist) {
        for (idx, node) in el.nodes.iter().enumerate() {
            members
                .entry(node.clone())
                .or_insert_with(|| {
                    order.push(node.clone());
                    Vec::new()
                })
                .push((el.designator.clone(), idx + 1));
        }
    }

    let mut nets = vec![Sexpr::Atom("nets".to_string())];
    for (code, name) in order.iter().enumerate() {
        let mut net = vec![
            Sexpr::Atom("net".to_string()),
            Sexpr::List(vec![
                Sexpr::Atom("code".to_string()),
                // KiCad net codes are 1-based positive integers.
                Sexpr::QString((code + 1).to_string()),
            ]),
            Sexpr::List(vec![
                Sexpr::Atom("name".to_string()),
                Sexpr::QString(name.clone()),
            ]),
        ];
        for (refdes, pin) in &members[name] {
            net.push(Sexpr::List(vec![
                Sexpr::Atom("node".to_string()),
                Sexpr::List(vec![
                    Sexpr::Atom("ref".to_string()),
                    Sexpr::QString(refdes.clone()),
                ]),
                Sexpr::List(vec![
                    Sexpr::Atom("pin".to_string()),
                    Sexpr::QString(pin.to_string()),
                ]),
            ]));
        }
        nets.push(Sexpr::List(net));
    }

    Sexpr::List(nets)
}

/// Elements that appear as schematic components: skip simulation-only
/// constructs (mutual inductance carries no nodes; `Other` is parser
/// debris such as a dangling `+` continuation). Everything with
/// terminals gets a `comp` and contributes to nets.
fn schematic_elements(netlist: &Netlist) -> impl Iterator<Item = &Element> {
    netlist.elements.iter().filter(|el| {
        !matches!(el.kind, ElementKind::MutualInductance | ElementKind::Other)
            && !el.nodes.is_empty()
    })
}

/// Human-readable value text for a `comp` `value` field. SPICE numbers
/// render at their literal magnitude; expressions/strings pass through;
/// a value-less element falls back to `~` (KiCad's empty marker).
fn value_text(el: &Element) -> String {
    match &el.value {
        Some(Value::Number(n)) => {
            if n.is_finite() && n.fract() == 0.0 {
                format!("{n:.0}")
            } else {
                format!("{n}")
            }
        }
        Some(Value::String(s) | Value::Expr(s)) => s.clone(),
        None => "~".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spice_diagnostics::FileId;
    use spice_parser::parse;

    fn netlist_of(src: &str) -> String {
        let outcome = parse(src, FileId(0)).expect("parse");
        emit(&outcome.netlist).expect("emit")
    }

    #[test]
    fn export_header_and_sections_present() {
        let out = netlist_of("* rc\nR1 in out 1k\nC1 out 0 10n\n");
        assert!(out.starts_with("(export (version \"E\")"), "header: {out}");
        assert!(out.contains("(components"), "components section: {out}");
        assert!(out.contains("(nets"), "nets section: {out}");
    }

    #[test]
    fn components_have_refs_and_values() {
        let out = netlist_of("* rc\nR1 in out 1k\nC1 out 0 10n\n");
        assert!(out.contains("(comp (ref \"R1\")"), "R1 comp: {out}");
        assert!(out.contains("(comp (ref \"C1\")"), "C1 comp: {out}");
        // 1k → 1000 magnitude.
        assert!(out.contains("(value \"1000\")"), "R1 value: {out}");
    }

    #[test]
    fn nets_collect_shared_nodes() {
        let out = netlist_of("* rc\nR1 in out 1k\nC1 out 0 10n\n");
        // `out` is shared by R1 pin 2 and C1 pin 1.
        assert!(out.contains("(name \"out\")"), "out net: {out}");
        assert!(
            out.contains("(node (ref \"R1\") (pin \"2\"))"),
            "R1.2 node: {out}"
        );
        assert!(
            out.contains("(node (ref \"C1\") (pin \"1\"))"),
            "C1.1 node: {out}"
        );
    }

    #[test]
    fn empty_netlist_still_emits_sections() {
        let out = netlist_of("* just a title\n");
        assert!(out.contains("(components)"), "empty components: {out}");
        assert!(out.contains("(nets)"), "empty nets: {out}");
    }
}
