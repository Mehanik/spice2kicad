//! Pre-flight layout policy / conflict-check pass.
//!
//! This crate sits between [`spice_resolve`] and the (future) layout
//! pass. It validates `align` / `place` directives, removes
//! ill-formed or redundant constraints, and either
//!
//! * returns a cleaned [`CheckedNetlist`] together with a list of
//!   non-fatal warnings, or
//! * returns a list of diagnostics including at least one
//!   `Severity::Error` when the input is unsatisfiable.
//!
//! See `docs/layout-adr.md` ADR-5 and `docs/annotation-spec.md` §5
//! for the design rationale.
//!
//! # Diagnostic codes emitted
//!
//! - **E001** — `align` or `place` references an unknown refdes
//!   (fatal; collected, not bailed-on)
//! - **E006** — directional cycle in the `place` graph within an
//!   axis (fatal)
//! - **W101** — duplicate `place` directives on one refdes (first
//!   wins, the rest warn)
//! - **W102** — `align` cluster has fewer than two distinct members
//!   (directive dropped)
//! - **W104** — `place` directive on an element already fixed by
//!   `align` (`place` dropped)
//!
//! `E004` (cross-sheet `align`) is intentionally not detected here
//! — `ResolvedNetlist` is currently flat and lacks the subckt
//! scoping required.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use spice_diagnostics::{Diagnostic, Label, Severity, Span};
use spice_resolve::{
    AlignSpec, PlaceSpec, Relation, ResolvedElement, ResolvedNetlist, SheetInstance, SubcktPorts,
};

// ---------------------------------------------------------------------------
// Output type
// ---------------------------------------------------------------------------

/// A [`ResolvedNetlist`] whose layout directives have been validated
/// and de-conflicted. Downstream consumers (the layout pass) can
/// trust that:
///
/// * every refdes mentioned in `align` / `place` exists in
///   `elements`,
/// * no element appears in more than one `place` directive,
/// * no element appears in both `align` and `place`,
/// * the `place` graph contains no per-axis directional cycles, and
/// * every `align` cluster has at least two distinct members.
#[derive(Debug, Clone)]
pub struct CheckedNetlist {
    pub elements: Vec<ResolvedElement>,
    pub align: Vec<AlignSpec>,
    pub place: Vec<PlaceSpec>,
    pub subckts: Vec<SubcktPorts>,
    pub sheet_instances: Vec<SheetInstance>,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the policy / conflict-check pass over a [`ResolvedNetlist`].
///
/// On success returns the cleaned netlist together with any non-fatal
/// warnings. On failure returns the full diagnostic list (containing
/// at least one `Severity::Error`); any warnings accumulated before
/// the fatal error is detected are included so the user sees the
/// whole picture in one go.
// The function is a flat orchestration of the 10-step recipe in
// `docs/layout-adr.md` ADR-5; splitting it would obscure the order
// of checks the spec mandates.
#[allow(clippy::too_many_lines)]
pub fn check(
    resolved: ResolvedNetlist,
) -> Result<(CheckedNetlist, Vec<Diagnostic>), Vec<Diagnostic>> {
    let ResolvedNetlist {
        elements,
        align,
        place,
        subckts,
        sheet_instances,
    } = resolved;

    let mut diags: Vec<Diagnostic> = Vec::new();
    let known: HashSet<&str> = elements.iter().map(|e| e.refdes.as_str()).collect();

    // 1 + 2. E001: collect every unknown-refdes reference. Don't bail.
    for spec in &align {
        // TODO(E004): subckt scoping not yet preserved in ResolvedNetlist;
        // cross-sheet align detection is deferred.
        for r in &spec.refdes {
            if !known.contains(r.as_str()) {
                push_err(
                    &mut diags,
                    "E001",
                    format!("`align` references unknown refdes `{r}`"),
                    spec.span,
                );
            }
        }
    }
    for spec in &place {
        if !known.contains(spec.refdes.as_str()) {
            push_err(
                &mut diags,
                "E001",
                format!("`place` references unknown refdes `{}`", spec.refdes),
                spec.span,
            );
        }
        if !known.contains(spec.anchor.as_str()) {
            push_err(
                &mut diags,
                "E001",
                format!(
                    "`place` on `{}` references unknown anchor `{}`",
                    spec.refdes, spec.anchor
                ),
                spec.span,
            );
        }
    }

    // 3. If any E001 was emitted, return Err with the full set.
    if diags.iter().any(|d| d.severity == Severity::Error) {
        return Err(diags);
    }

    // 4. De-dup `align` members within each cluster and drop
    //    clusters with < 2 distinct members (W102).
    let mut clean_align: Vec<AlignSpec> = Vec::with_capacity(align.len());
    for spec in align {
        let mut seen: HashSet<String> = HashSet::new();
        let mut deduped: Vec<String> = Vec::with_capacity(spec.refdes.len());
        for r in &spec.refdes {
            if seen.insert(r.clone()) {
                deduped.push(r.clone());
            }
        }
        if deduped.len() < 2 {
            push_warn(
                &mut diags,
                "W102",
                format!(
                    "`align` cluster has {} distinct member(s); at least 2 required — directive dropped",
                    deduped.len()
                ),
                spec.span,
            );
            continue;
        }
        clean_align.push(AlignSpec {
            axis: spec.axis,
            refdes: deduped,
            span: spec.span,
        });
    }

    // 5. Build the set of refdeses fixed by `align`.
    let aligned: HashSet<String> = clean_align
        .iter()
        .flat_map(|a| a.refdes.iter().cloned())
        .collect();

    // 6 + 7. Walk `place` in input order, dropping align-fixed (W104)
    //    and duplicate (W101) entries.
    let mut clean_place: Vec<PlaceSpec> = Vec::with_capacity(place.len());
    let mut placed: HashSet<String> = HashSet::new();
    for spec in place {
        if aligned.contains(&spec.refdes) {
            push_warn(
                &mut diags,
                "W104",
                format!(
                    "`place` on `{}` ignored: element is already fixed by `align`",
                    spec.refdes
                ),
                spec.span,
            );
            continue;
        }
        if !placed.insert(spec.refdes.clone()) {
            push_warn(
                &mut diags,
                "W101",
                format!(
                    "duplicate `place` directive on `{}`; keeping the first",
                    spec.refdes
                ),
                spec.span,
            );
            continue;
        }
        clean_place.push(spec);
    }

    // 8. Build per-axis directional graphs and check for cycles.
    detect_cycles(&clean_place, &mut diags);

    // 9. If any error (E006) snuck in, surface everything.
    if diags.iter().any(|d| d.severity == Severity::Error) {
        return Err(diags);
    }

    // 10. Success.
    Ok((
        CheckedNetlist {
            elements,
            align: clean_align,
            place: clean_place,
            subckts,
            sheet_instances,
        },
        diags,
    ))
}

// ---------------------------------------------------------------------------
// Cycle detection
// ---------------------------------------------------------------------------

/// Which axis a [`Relation`] constrains.
fn relation_axis(r: Relation) -> Axis {
    match r {
        Relation::RightOf | Relation::LeftOf => Axis::X,
        Relation::Above | Relation::Below => Axis::Y,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    X,
    Y,
}

/// Detect directional cycles in the per-axis place graph using
/// iterative Tarjan's strongly connected components. A cycle exists
/// iff some SCC has size > 1 (we don't have self-loops because a
/// `place` directive's refdes and anchor are distinct after the E001
/// filter — but an anchor equal to its refdes would also yield a
/// self-cycle and is reported as such).
fn detect_cycles(place: &[PlaceSpec], diags: &mut Vec<Diagnostic>) {
    for axis in [Axis::X, Axis::Y] {
        let edges: Vec<(&str, &str, Option<Span>)> = place
            .iter()
            .filter(|p| relation_axis(p.relation) == axis)
            .map(|p| (p.refdes.as_str(), p.anchor.as_str(), p.span))
            .collect();
        if edges.is_empty() {
            continue;
        }
        for scc in tarjan_sccs(&edges) {
            if scc.len() < 2 {
                // Skip singletons unless self-loop — handled below.
                let Some(node) = scc.first() else { continue };
                if edges.iter().any(|(s, t, _)| s == node && t == node) {
                    let span = edges
                        .iter()
                        .find(|(s, t, _)| s == node && t == node)
                        .and_then(|(_, _, sp)| *sp);
                    push_err(
                        diags,
                        "E006",
                        format!("`place` self-cycle on `{node}` ({} axis)", axis_name(axis)),
                        span,
                    );
                }
                continue;
            }
            let mut members: Vec<&str> = scc.clone();
            members.sort_unstable();
            // Pick a representative span: the first edge whose
            // endpoints both lie in the SCC.
            let span = edges
                .iter()
                .find(|(s, t, _)| scc.contains(s) && scc.contains(t))
                .and_then(|(_, _, sp)| *sp);
            push_err(
                diags,
                "E006",
                format!(
                    "`place` graph has a cycle on the {} axis: {}",
                    axis_name(axis),
                    members.join(" → ")
                ),
                span,
            );
        }
    }
}

fn axis_name(a: Axis) -> &'static str {
    match a {
        Axis::X => "X",
        Axis::Y => "Y",
    }
}

/// Tarjan's SCC over an edge list. Returns SCCs as vectors of node
/// labels in arbitrary order. Iterative to avoid pathological stack
/// depth (graphs are tiny in practice but pedantic clippy nudges
/// us toward iterative anyway).
fn tarjan_sccs<'a>(edges: &[(&'a str, &'a str, Option<Span>)]) -> Vec<Vec<&'a str>> {
    // Build adjacency.
    let mut nodes: Vec<&str> = Vec::new();
    let mut idx: HashMap<&str, usize> = HashMap::new();
    for (s, t, _) in edges {
        for n in [*s, *t] {
            if let std::collections::hash_map::Entry::Vacant(e) = idx.entry(n) {
                e.insert(nodes.len());
                nodes.push(n);
            }
        }
    }
    let n = nodes.len();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (s, t, _) in edges {
        adj[idx[*s]].push(idx[*t]);
    }

    // Tarjan state.
    let mut index_counter: usize = 0;
    let mut stack: Vec<usize> = Vec::new();
    let mut on_stack: Vec<bool> = vec![false; n];
    let mut indices: Vec<Option<usize>> = vec![None; n];
    let mut lowlink: Vec<usize> = vec![0; n];
    let mut sccs: Vec<Vec<&str>> = Vec::new();

    // Iterative DFS frame: (node, next-child-index).
    for start in 0..n {
        if indices[start].is_some() {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(start, 0)];
        indices[start] = Some(index_counter);
        lowlink[start] = index_counter;
        index_counter += 1;
        stack.push(start);
        on_stack[start] = true;

        while let Some(&(v, i)) = work.last() {
            if i < adj[v].len() {
                let w = adj[v][i];
                work.last_mut().unwrap().1 += 1;
                if indices[w].is_none() {
                    indices[w] = Some(index_counter);
                    lowlink[w] = index_counter;
                    index_counter += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    work.push((w, 0));
                } else if on_stack[w] {
                    lowlink[v] = lowlink[v].min(indices[w].unwrap());
                }
            } else {
                // Post-order: finalize v.
                if lowlink[v] == indices[v].unwrap() {
                    let mut comp: Vec<&str> = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        comp.push(nodes[w]);
                        if w == v {
                            break;
                        }
                    }
                    sccs.push(comp);
                }
                work.pop();
                if let Some(&(parent, _)) = work.last() {
                    lowlink[parent] = lowlink[parent].min(lowlink[v]);
                }
            }
        }
    }

    sccs
}

// ---------------------------------------------------------------------------
// Diagnostic helpers (mirror spice-resolve conventions)
// ---------------------------------------------------------------------------

fn push_err(diags: &mut Vec<Diagnostic>, code: &'static str, message: String, span: Option<Span>) {
    diags.push(make_diag(Severity::Error, code, message, span));
}

fn push_warn(diags: &mut Vec<Diagnostic>, code: &'static str, message: String, span: Option<Span>) {
    diags.push(make_diag(Severity::Warning, code, message, span));
}

fn make_diag(
    severity: Severity,
    code: &'static str,
    message: String,
    span: Option<Span>,
) -> Diagnostic {
    // Hand-constructed test inputs may carry `None` spans; build a
    // placeholder so the renderer never sees a missing primary label.
    let primary = span.map_or_else(
        || Label::new(Span::point(spice_diagnostics::FileId(0), 0), ""),
        |s| Label::new(s, ""),
    );
    let mut d = match severity {
        Severity::Error => Diagnostic::error(code, message, primary),
        Severity::Warning => Diagnostic::warning(code, message, primary),
        Severity::Note => Diagnostic::note(code, message, primary),
    };
    if span.is_none() {
        d = d.with_help("source span unavailable for this diagnostic");
    }
    d
}
