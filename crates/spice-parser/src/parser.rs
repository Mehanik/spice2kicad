//! Build a [`Netlist`] from the [`crate::lexer::Scanned`] token stream.
//!
//! SPICE is line-oriented and the tokeniser already groups continuation
//! lines, separates code from `;@` tags, and identifies `*@` block
//! annotations. This module assigns each logical line to one of:
//!
//! * a placeable element (R/C/L/V/I/D/Q/M/J/E/F/G/H/X/T/K) — added to the
//!   current scope's element list;
//! * a structural directive (`.subckt`/`.ends`) — opens or closes a
//!   subcircuit scope;
//! * a `.model` declaration — added to the netlist's model list;
//! * any other directive (`.tran`, `.ac`, `.include`, …) — preserved as a
//!   generic [`Directive`] for emitter use.
//!
//! `*@` block annotations are parsed into [`Annotation`] values and
//! attached to whichever scope is current (top level or the surrounding
//! `.subckt`); `;@` trailing tags are parsed into [`Tag`] values and
//! attached to the element they sit on.

use std::collections::HashSet;

use spice_diagnostics::{Diagnostic, FileId, Label, Span};

use crate::ast::{
    Annotation, Axis, Directive, Element, ElementKind, Model, Netlist, PinRef, PinmapEntry,
    Relation, SpannedAnnotation, SpannedTag, Subckt, Tag, Value,
};
use crate::lexer::{LineKind, LogicalLine, RawTag, Scanned, Word};
use crate::{ParseOutcome, ParseResult};

/// Parse a pre-scanned token stream into a [`ParseOutcome`].
pub fn parse(scanned: Scanned, file: FileId) -> ParseResult<ParseOutcome> {
    let mut diags: Vec<Diagnostic> = scanned.diagnostics;
    let mut nl = Netlist {
        title: scanned.title,
        ..Netlist::default()
    };
    let _ = scanned.title_span;
    let _ = file;

    let mut subckt_stack: Vec<Subckt> = Vec::new();

    for line in scanned.lines {
        match line.kind {
            LineKind::BlockAnnotation => {
                if let Some(ann) = parse_block_annotation(&line, &mut diags) {
                    let entry = SpannedAnnotation::new(ann, line.span);
                    if let Some(top) = subckt_stack.last_mut() {
                        top.annotations.push(entry);
                    } else {
                        nl.annotations.push(entry);
                    }
                }
            }
            LineKind::Code => {
                handle_code_line(&line, &mut subckt_stack, &mut nl, &mut diags);
            }
        }
    }

    // Unterminated subckts — close them anyway and emit a warning.
    while let Some(sub) = subckt_stack.pop() {
        diags.push(Diagnostic::warning(
            "W900",
            format!("subckt `{}` was never closed by `.ends`", sub.name),
            Label::new(Span::point(file, 0), "opened here"),
        ));
        nl.subckts.push(sub);
    }

    if diags
        .iter()
        .any(|d| d.severity == spice_diagnostics::Severity::Error)
    {
        Err(diags)
    } else {
        Ok(ParseOutcome {
            netlist: nl,
            diagnostics: diags,
        })
    }
}

fn handle_code_line(
    line: &LogicalLine,
    stack: &mut Vec<Subckt>,
    nl: &mut Netlist,
    diags: &mut Vec<Diagnostic>,
) {
    let Some(first) = line.words.first() else {
        return;
    };
    let head = first.text.clone();

    // Directive?
    if let Some(name) = head.strip_prefix('.') {
        let name_lc = name.to_ascii_lowercase();
        match name_lc.as_str() {
            "subckt" => {
                if let Some(sub) = parse_subckt_header(&line.words, diags) {
                    stack.push(sub);
                }
            }
            "ends" => {
                if let Some(sub) = stack.pop() {
                    nl.subckts.push(sub);
                } else {
                    diags.push(error(
                        "E900",
                        ".ends without matching .subckt",
                        Label::new(line.span, "stray .ends"),
                    ));
                }
            }
            "model" => {
                if let Some(model) = parse_model(&line.words, diags) {
                    if let Some(top) = stack.last_mut() {
                        // Models inside a subckt: store on the netlist with
                        // the bare name (resolution scope is not modelled
                        // here — the emitter only cares about the names).
                        let _ = top;
                    }
                    nl.models.push(model);
                }
            }
            "end" => {
                // End of deck. Subsequent lines are technically out of
                // spec; the scanner already gave us the lines so we just
                // stop processing further. We simulate this by clearing
                // the stack; remaining iteration continues but the stack
                // unbalance won't fire. Simpler: just record nothing.
            }
            _ => {
                let args = line.words.iter().skip(1).map(|w| w.text.clone()).collect();
                let dir = Directive {
                    name: name.to_owned(),
                    args,
                };
                if stack.last().is_some() {
                    // Subckt does not carry a directive list; nested
                    // simulation directives are dropped, but flagged so
                    // the user notices the silent loss.
                    let _ = dir;
                    diags.push(Diagnostic::warning(
                        "W910",
                        format!("directive `.{name}` inside .subckt is ignored"),
                        Label::new(line.span, ""),
                    ));
                } else {
                    nl.directives.push(dir);
                }
            }
        }
        return;
    }

    // Element line: the first character of the refdes determines the kind.
    let kind = element_kind_from_refdes(&head);
    let element = parse_element(head, kind, line, diags);

    if let Some(top) = stack.last_mut() {
        top.body.push(element);
    } else {
        nl.elements.push(element);
    }
}

fn element_kind_from_refdes(refdes: &str) -> ElementKind {
    let Some(c) = refdes.chars().next() else {
        return ElementKind::Other;
    };
    match c.to_ascii_uppercase() {
        'R' => ElementKind::Resistor,
        'C' => ElementKind::Capacitor,
        'L' => ElementKind::Inductor,
        'V' => ElementKind::VoltageSrc,
        'I' => ElementKind::CurrentSrc,
        'D' => ElementKind::Diode,
        'Q' => ElementKind::Bjt,
        'M' => ElementKind::Mosfet,
        'J' => ElementKind::Jfet,
        'X' => ElementKind::Subckt,
        'K' => ElementKind::MutualInductance,
        'F' => ElementKind::Cccs,
        'H' => ElementKind::Ccvs,
        'E' => ElementKind::Vcvs,
        'G' => ElementKind::Vccs,
        _ => ElementKind::Other,
    }
}

/// Number of "node" terminals expected for each element kind. Returns
/// `None` for kinds with variable port count (X, Bjt, K, F, H).
fn fixed_node_count(kind: ElementKind) -> Option<usize> {
    match kind {
        ElementKind::Resistor
        | ElementKind::Capacitor
        | ElementKind::Inductor
        | ElementKind::VoltageSrc
        | ElementKind::CurrentSrc
        | ElementKind::Diode => Some(2),
        ElementKind::Jfet => Some(3),
        ElementKind::Mosfet | ElementKind::Vcvs | ElementKind::Vccs => Some(4),
        // Bjt: 3 or 4 nodes — disambiguated in parse_element.
        // MutualInductance: 2 inductor refs, no nets — handled in parse_element.
        // Cccs/Ccvs: 2 nodes then a control-source name — handled in parse_element.
        // Vcvs/Vccs: 4 nodes (out+, out-, ctrl+, ctrl-) then numeric gain.
        // Subckt/Other: variable.
        ElementKind::Bjt
        | ElementKind::MutualInductance
        | ElementKind::Cccs
        | ElementKind::Ccvs
        | ElementKind::Subckt
        | ElementKind::Other => None,
    }
}

/// True for kinds whose token after the nodes is a model/subckt name
/// (i.e. an identifier rather than a numeric value).
fn has_named_model(kind: ElementKind) -> bool {
    matches!(
        kind,
        ElementKind::Diode
            | ElementKind::Bjt
            | ElementKind::Mosfet
            | ElementKind::Jfet
            | ElementKind::Subckt
    )
}

fn parse_element(
    designator: String,
    kind: ElementKind,
    line: &LogicalLine,
    diags: &mut Vec<Diagnostic>,
) -> Element {
    let mut element = Element::new(designator, kind, Vec::new());

    // Strip key=value tail. Anything matching `<word> = <word>` becomes
    // a (key, value) param; positional tokens stay in the bare list.
    let bare_words = collect_positional(&line.words[1..], &mut element.params);

    match kind {
        // BJT: 3 or 4 nodes; last bare token is model name. ngspice inp2q.c.
        ElementKind::Bjt => {
            let total = bare_words.len();
            if total < 4 {
                diags.push(Diagnostic::warning(
                    "W907",
                    format!(
                        "Q{} requires at least 3 nodes and a model name",
                        element.designator
                    ),
                    Label::new(line.span, "malformed BJT"),
                ));
            }
            let node_count = match total {
                0..=3 => total.saturating_sub(1), // degenerate — take what we can
                4 => 3,                           // Q c b e MODEL
                _ => 4,                           // Q c b e s MODEL (5 tokens = 4-terminal)
            };
            for w in bare_words.iter().take(node_count) {
                element.nodes.push(w.text.clone());
            }
            let rest = &bare_words[node_count..];
            if !rest.is_empty() {
                element.value = Some(Value::String(rest[0].text.clone()));
                for extra in &rest[1..] {
                    element
                        .params
                        .push((String::new(), Value::String(extra.text.clone())));
                }
            }
        }
        // K (mutual inductance). Form: K L1 L2 coupling.
        // L1/L2 are inductor refdes references; live in `coupled`, not `nodes`.
        ElementKind::MutualInductance => {
            for w in bare_words.iter().take(2) {
                element.coupled.push(w.text.clone());
            }
            if let Some(coupling) = bare_words.get(2) {
                element.value = Some(parse_value_token(&coupling.text));
            }
        }
        // F (CCCS) / H (CCVS). Form: <refdes> n+ n- Vname gain.
        // ngspice inp2f.c / inp2h.c. The Vname refdes goes in `control`,
        // not `params` — it's a typed cross-reference, not a user param.
        ElementKind::Cccs | ElementKind::Ccvs => {
            for w in bare_words.iter().take(2) {
                element.nodes.push(w.text.clone());
            }
            if let Some(ctrl) = bare_words.get(2) {
                element.control = Some(ctrl.text.clone());
            }
            if let Some(gain) = bare_words.get(3) {
                element.value = Some(parse_value_token(&gain.text));
            }
            if bare_words.len() < 4 {
                let kind_name = if matches!(kind, ElementKind::Cccs) {
                    "CCCS (F)"
                } else {
                    "CCVS (H)"
                };
                diags.push(error(
                    "E905",
                    format!(
                        "{} `{}` requires `n+ n- Vname gain` (got {} token(s))",
                        kind_name,
                        element.designator,
                        bare_words.len()
                    ),
                    Label::new(line.span, "malformed F/H source"),
                ));
            }
        }
        _ => {
            // Decide how many positional tokens are nodes.
            let node_count = match fixed_node_count(kind) {
                Some(n) => bare_words.len().min(n),
                None => {
                    // Subckt instance: last bare token is the subckt name;
                    // rest are nodes.
                    bare_words.len().saturating_sub(1)
                }
            };
            for w in bare_words.iter().take(node_count) {
                element.nodes.push(w.text.clone());
            }
            let rest = &bare_words[node_count..];
            if rest.is_empty() {
                // No value — fine for things like `Vsense in 0` (zero-volt sense).
            } else if has_named_model(kind) {
                element.value = Some(Value::String(rest[0].text.clone()));
                for extra in &rest[1..] {
                    element
                        .params
                        .push((String::new(), Value::String(extra.text.clone())));
                }
            } else {
                element.value = Some(combine_value_tokens(rest));
            }
        }
    }

    attach_tags(&mut element, &line.tags, diags);
    element
}

fn attach_tags(element: &mut Element, tags: &[RawTag], diags: &mut Vec<Diagnostic>) {
    for raw in tags {
        if let Some(tag) = parse_tag(raw, diags) {
            element.tags.push(SpannedTag::new(tag, raw.body_span));
        }
    }
}

/// Walk through `words`, peeling off `<key> = <value>` sequences as
/// params and returning the positional tokens as a clone.
fn collect_positional(words: &[Word], params: &mut Vec<(String, Value)>) -> Vec<Word> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let next_is_eq = words.get(i + 1).is_some_and(|w| w.text == "=");
        let has_rhs = words.get(i + 2).is_some();
        if next_is_eq && has_rhs && is_identifier(&words[i].text) {
            let key = words[i].text.clone();
            let val = parse_value_token(&words[i + 2].text);
            params.push((key, val));
            i += 3;
        } else {
            out.push(words[i].clone());
            i += 1;
        }
    }
    out
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Combine one or more bare tokens that follow the node list into a
/// single [`Value`]. A single numeric token becomes [`Value::Number`];
/// anything else is preserved verbatim as [`Value::String`] (joined by
/// single spaces) so source specs like `AC 1` or `SIN ( 0 1 1k )` are
/// not lost.
fn combine_value_tokens(words: &[Word]) -> Value {
    if words.len() == 1
        && let Some(n) = parse_spice_number(&words[0].text)
    {
        return Value::Number(n);
    }
    let joined = words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Value::String(joined)
}

fn parse_value_token(s: &str) -> Value {
    if let Some(n) = parse_spice_number(s) {
        Value::Number(n)
    } else {
        Value::String(s.to_owned())
    }
}

// ---------------------------------------------------------------------------
// `.subckt` / `.model`
// ---------------------------------------------------------------------------

fn parse_subckt_header(words: &[Word], diags: &mut Vec<Diagnostic>) -> Option<Subckt> {
    // `.subckt NAME port port port [key=val]...`
    let mut iter = words.iter().skip(1);
    let Some(name_w) = iter.next() else {
        diags.push(error(
            "E901",
            ".subckt missing name",
            Label::new(words[0].span, ""),
        ));
        return None;
    };
    let name = name_w.text.clone();

    let rest: Vec<Word> = iter.cloned().collect();
    // ngspice accepts `.subckt NAME ports... params: KEY=val ...`. The
    // `params:` keyword (case-insensitive) splits the trailing word list
    // into ports (before) and parameters (after). When absent, the entire
    // tail is fed through `collect_positional`, which picks any inline
    // `key=val` triples out of the port list.
    let split_at = rest
        .iter()
        .position(|w| w.text.eq_ignore_ascii_case("params:"));
    let mut params = Vec::new();
    let ports: Vec<String> = if let Some(idx) = split_at {
        let positional = collect_positional(&rest[..idx], &mut params);
        let _ = collect_positional(&rest[idx + 1..], &mut params);
        positional.into_iter().map(|w| w.text).collect()
    } else {
        let positional = collect_positional(&rest, &mut params);
        positional.into_iter().map(|w| w.text).collect()
    };

    Some(Subckt {
        name,
        ports,
        params,
        body: Vec::new(),
        annotations: Vec::new(),
    })
}

fn parse_model(words: &[Word], diags: &mut Vec<Diagnostic>) -> Option<Model> {
    // `.model NAME TYPE [(] key=val ... [)]`
    let mut iter = words.iter().skip(1);
    let name = iter.next()?.text.clone();
    let Some(type_w) = iter.next() else {
        diags.push(error(
            "E902",
            ".model missing type",
            Label::new(words[0].span, ""),
        ));
        return None;
    };
    let model_type = type_w.text.clone();
    // Strip surrounding parens from the param block, if any.
    let raw: Vec<Word> = iter
        .filter(|w| w.text != "(" && w.text != ")")
        .cloned()
        .collect();
    let mut params = Vec::new();
    let leftover = collect_positional(&raw, &mut params);
    // Any unclaimed positional tokens are stored as keyless params for
    // round-tripping (defensive — common SPICE models won't trip this).
    for w in leftover {
        params.push((String::new(), Value::String(w.text)));
    }
    Some(Model {
        name,
        model_type,
        params,
    })
}

// ---------------------------------------------------------------------------
// Annotations: `;@` trailing tags and `*@` block directives
// ---------------------------------------------------------------------------

fn parse_tag(raw: &RawTag, diags: &mut Vec<Diagnostic>) -> Option<Tag> {
    let body = raw.body.trim();
    // Forms:
    //   directive=value [extra args]
    //   directive [args]
    let (directive, rest_after_eq, rest_after_space) = split_directive(body);
    let directive_lc = directive.to_ascii_lowercase();

    match directive_lc.as_str() {
        "symbol" => {
            let v = rest_after_eq.or(rest_after_space)?;
            Some(Tag::Symbol(first_token(v).to_owned()))
        }
        "pinmap" => {
            let v = rest_after_eq.or(rest_after_space)?;
            parse_pinmap(v, raw, diags)
        }
        "place" => {
            // `place=right-of V1` or `place right-of V1`
            let v = rest_after_eq.or(rest_after_space)?;
            parse_place(v).or_else(|| {
                diags.push(error(
                    "E903",
                    format!("invalid place directive: `{v}`"),
                    Label::new(raw.body_span, ""),
                ));
                None
            })
        }
        "power" => {
            let v = rest_after_eq.or(rest_after_space)?;
            Some(Tag::Power(first_token(v).to_owned()))
        }
        "ignore" => Some(Tag::Ignore),
        _ => {
            diags.push(Diagnostic::warning(
                "W103",
                format!("unknown tag directive `{directive}`"),
                Label::new(raw.outer_span, ""),
            ));
            None
        }
    }
}

/// Returns `(directive_word, value_after_equals, value_after_space)`.
/// Only one of the latter two is `Some`. Whitespace between the
/// directive word and a following `=` is skipped, so `symbol\t=\tval`
/// parses the same as `symbol=val`.
fn split_directive(body: &str) -> (&str, Option<&str>, Option<&str>) {
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' {
        i += 1;
    }
    let directive = &body[..i];
    // Skip whitespace before deciding whether the separator is `=`.
    let mut j = i;
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    if j < bytes.len() && bytes[j] == b'=' {
        return (directive, Some(body[j + 1..].trim_start()), None);
    }
    if i == bytes.len() {
        return (directive, None, None);
    }
    (directive, None, Some(body[i..].trim_start()))
}

fn first_token(s: &str) -> &str {
    s.split_ascii_whitespace().next().unwrap_or("")
}

fn parse_place(s: &str) -> Option<Tag> {
    let mut it = s.split_ascii_whitespace();
    let rel = it.next()?;
    let anchor = it.next()?;
    let relation = match rel.to_ascii_lowercase().as_str() {
        "right-of" => Relation::RightOf,
        "left-of" => Relation::LeftOf,
        "above" => Relation::Above,
        "below" => Relation::Below,
        _ => return None,
    };
    Some(Tag::Place {
        relation,
        anchor: anchor.to_owned(),
    })
}

fn parse_pinmap(s: &str, raw: &RawTag, diags: &mut Vec<Diagnostic>) -> Option<Tag> {
    parse_pinmap_entries(s, raw.body_span, raw.outer_span, diags).map(Tag::Pinmap)
}

fn parse_pinmap_entries(
    s: &str,
    body_span: Span,
    outer_span: Span,
    diags: &mut Vec<Diagnostic>,
) -> Option<Vec<PinmapEntry>> {
    let mut entries = Vec::new();
    let mut seen_spice: HashSet<usize> = HashSet::new();
    let mut seen_kicad: HashSet<String> = HashSet::new();
    let mut malformed = false;
    for chunk in s.split(',') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        let Some((lhs, rhs)) = chunk.split_once(':') else {
            malformed = true;
            break;
        };
        let Ok(spice_index) = lhs.trim().parse::<usize>() else {
            malformed = true;
            break;
        };
        let rhs = rhs.trim();
        if !seen_spice.insert(spice_index) {
            diags.push(error(
                "E005",
                format!("pinmap repeats SPICE terminal index {spice_index}"),
                Label::new(body_span, ""),
            ));
            return None;
        }
        let kicad_key = rhs.to_ascii_lowercase();
        if !seen_kicad.insert(kicad_key) {
            diags.push(error(
                "E005",
                format!("pinmap repeats KiCad pin `{rhs}`"),
                Label::new(body_span, ""),
            ));
            return None;
        }
        let kicad_pin = if rhs.chars().all(|c| c.is_ascii_digit()) {
            PinRef::Number(rhs.to_owned())
        } else {
            PinRef::Name(rhs.to_owned())
        };
        entries.push(PinmapEntry {
            spice_index,
            kicad_pin,
        });
    }
    if malformed || entries.is_empty() {
        diags.push(error(
            "E005",
            format!("invalid pinmap: `{s}`"),
            Label::new(outer_span, ""),
        ));
        return None;
    }
    Some(entries)
}

fn parse_block_annotation(line: &LogicalLine, diags: &mut Vec<Diagnostic>) -> Option<Annotation> {
    let directive = line.words.first()?.text.to_ascii_lowercase();
    match directive.as_str() {
        "symbol" => {
            // `*@symbol Lib:Name for=GLOB` — collect positional + key=value.
            let tail = &line.words[1..];
            let mut params: Vec<(String, Value)> = Vec::new();
            let positional = collect_positional(tail, &mut params);
            let Some(lib_id_word) = positional.first() else {
                diags.push(Diagnostic::warning(
                    "E909",
                    "*@symbol requires Lib:Name positional",
                    Label::new(line.span, ""),
                ));
                return None;
            };
            let lib_id = lib_id_word.text.clone();
            let for_entry = params.iter().find(|(k, _)| k.eq_ignore_ascii_case("for"));
            let Some((_, for_value)) = for_entry else {
                diags.push(Diagnostic::warning(
                    "E908",
                    "*@symbol requires for=GLOB",
                    Label::new(line.span, ""),
                ));
                return None;
            };
            let for_glob = match for_value {
                Value::String(s) => s.clone(),
                Value::Number(n) => format!("{n}"),
                Value::Expr(e) => e.clone(),
            };
            let pinmap = params
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("pinmap"))
                .and_then(|(_, v)| {
                    let s = match v {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => format!("{n}"),
                        Value::Expr(e) => e.clone(),
                    };
                    parse_pinmap_entries(&s, line.span, line.span, diags)
                });
            Some(Annotation::SymbolDefault {
                lib_id,
                for_glob,
                pinmap,
            })
        }
        "align" => {
            // `*@align <axis> ref ref ref ...`
            let tail = &line.words[1..];
            if tail.len() < 2 {
                diags.push(error(
                    "E904",
                    "align requires axis and at least one refdes",
                    Label::new(line.span, ""),
                ));
                return None;
            }
            let axis = match tail[0].text.to_ascii_lowercase().as_str() {
                "horizontal" => Axis::Horizontal,
                "vertical" => Axis::Vertical,
                other => {
                    diags.push(error(
                        "E904",
                        format!("unknown align axis `{other}`"),
                        Label::new(tail[0].span, ""),
                    ));
                    return None;
                }
            };
            let refdes = tail[1..].iter().map(|w| w.text.clone()).collect();
            Some(Annotation::Align { axis, refdes })
        }
        other => {
            diags.push(Diagnostic::warning(
                "W103",
                format!("unknown block annotation `{other}`"),
                Label::new(line.span, ""),
            ));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// SPICE numbers with engineering suffixes
// ---------------------------------------------------------------------------

/// Parse a SPICE number: optional sign, mantissa, optional exponent,
/// optional engineering-suffix multiplier, optional unit-letter trailer.
/// Supports the `4k7` infix form (suffix between integer and fractional
/// digits), used by LTspice/PSpice and increasingly tolerated elsewhere.
/// No underscore digit grouping (matches ngspice).
pub(crate) fn parse_spice_number(s: &str) -> Option<f64> {
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let mut i = 0;

    // Sign.
    if bytes[i] == b'+' || bytes[i] == b'-' {
        i += 1;
    }

    // Mantissa: digits, optional dot, more digits.
    let m_int_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    let int_end = i;
    let mut frac_end = int_end;
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        frac_end = i;
    }
    if frac_end == m_int_start {
        return None; // no digits at all
    }

    // Exponent: accept e/E (standard) and d/D (Fortran-style, ngspice inpeval.c:120).
    let mut d_exp = false; // true when we consumed a 'd'/'D' marker
    if i < bytes.len() && matches!(bytes[i], b'e' | b'E' | b'd' | b'D') {
        // Require sign or digit after the marker to distinguish from suffix letters.
        let j = i + 1;
        let next = bytes.get(j).copied();
        if matches!(next, Some(b'+' | b'-')) || matches!(next, Some(b) if b.is_ascii_digit()) {
            d_exp = matches!(bytes[i], b'd' | b'D');
            i = j;
            if matches!(bytes.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
            let exp_digits_start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i == exp_digits_start {
                return None;
            }
        }
    }

    let mantissa_end = i;
    // Rust's f64 parser does not accept 'd'/'D' as exponent marker; swap to 'e'.
    // The lexer guarantees at most one exponent marker in the consumed mantissa,
    // so a plain `replace` is sufficient.
    let mantissa: f64 = if d_exp {
        s[..mantissa_end].replace(['d', 'D'], "e").parse().ok()?
    } else {
        s[..mantissa_end].parse().ok()?
    };

    // Engineering suffix: longest match of T/G/Meg/K/M/U/N/P/F/Mil
    // (case-insensitive). After the suffix, an optional run of digits
    // implements the `4k7` infix-decimal form, and an optional trailing
    // unit-letter run (Hz, F, Ohm, V, A, …) is silently dropped.
    let (mult, after_suffix, has_suffix) = peel_eng_suffix(&s[mantissa_end..]);
    let mut value = mantissa * mult;

    // `4k7` form: only if there was no decimal point in the mantissa.
    let had_dot = (m_int_start..int_end).len() != (m_int_start..frac_end).len();
    let mut tail = after_suffix;
    if !had_dot && has_suffix {
        let tail_bytes = tail.as_bytes();
        let mut k = 0;
        while k < tail_bytes.len() && tail_bytes[k].is_ascii_digit() {
            k += 1;
        }
        if k > 0 {
            // Append `k` decimal digits.
            let extra: f64 = tail[..k].parse().ok()?;
            let scale = 10f64.powi(i32::try_from(k).ok()?);
            // We already multiplied integer part by mult; fractional
            // part needs (extra / 10^k) * mult.
            value += (extra / scale) * mult;
            tail = &tail[k..];
        }
    }

    // Anything left must be a pure unit-letter run (Hz, F, V, A, Ω, …);
    // reject digits/punctuation in the tail. Also reject a leading
    // exponent-marker byte: `1e5e` must not silently parse as 1e5, and
    // a stray `e`/`d` after a number is malformed input.
    if !tail.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    if matches!(tail.as_bytes().first(), Some(b'e' | b'E' | b'd' | b'D')) {
        return None;
    }

    Some(value)
}

fn peel_eng_suffix(s: &str) -> (f64, &str, bool) {
    // Order matters: `Meg` before `M`, `Mil` before `M`.
    const SUFFIXES: &[(&str, f64)] = &[
        ("meg", 1e6),
        ("mil", 25.4e-6),
        ("t", 1e12),
        ("g", 1e9),
        ("k", 1e3),
        ("m", 1e-3),
        ("u", 1e-6),
        ("n", 1e-9),
        ("p", 1e-12),
        ("f", 1e-15),
        ("a", 1e-18), // atto; ngspice inpeval.c:172-175
    ];
    let lower = s.to_ascii_lowercase();
    for (suf, mult) in SUFFIXES {
        if lower.starts_with(suf) {
            return (*mult, &s[suf.len()..], true);
        }
    }
    (1.0, s, false)
}

// ---------------------------------------------------------------------------
// Diagnostic helpers
// ---------------------------------------------------------------------------

fn error(code: &'static str, message: impl Into<String>, label: Label) -> Diagnostic {
    Diagnostic::error(code, message, label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_engineering_suffixes() {
        assert!((parse_spice_number("1k").unwrap() - 1e3).abs() < 1e-6);
        assert!((parse_spice_number("100n").unwrap() - 100e-9).abs() < 1e-15);
        assert!((parse_spice_number("4k7").unwrap() - 4700.0).abs() < 1e-6);
        assert!((parse_spice_number("1Meg").unwrap() - 1e6).abs() < 1e-3);
        assert!((parse_spice_number("10Meg").unwrap() - 10e6).abs() < 1.0);
        assert!((parse_spice_number("1e-15").unwrap() - 1e-15).abs() < 1e-20);
        assert!((parse_spice_number("3.3k").unwrap() - 3300.0).abs() < 1e-6);
        // Unit-letter trailer ignored.
        assert!((parse_spice_number("1kHz").unwrap() - 1e3).abs() < 1e-6);
        assert!((parse_spice_number("100nF").unwrap() - 100e-9).abs() < 1e-15);
    }

    #[test]
    fn rejects_non_numbers() {
        assert!(parse_spice_number("R1").is_none());
        assert!(parse_spice_number("AC").is_none());
        assert!(parse_spice_number("").is_none());
    }

    #[test]
    fn rejects_double_exponent_marker() {
        // F1: `1ee5` and `1e5e` must not parse as 1e5.
        assert!(parse_spice_number("1ee5").is_none());
        assert!(parse_spice_number("1e5e").is_none());
    }

    #[test]
    fn fortran_d_exponent() {
        // F1: D-marker path smoke test.
        assert!((parse_spice_number("1d3").unwrap() - 1000.0).abs() < 1e-6);
        assert!((parse_spice_number("1D-3").unwrap() - 1e-3).abs() < 1e-9);
    }

    #[test]
    fn q_too_few_tokens() {
        // F6: malformed BJT line emits W907.
        use spice_diagnostics::FileId;
        let src = "* t\nQ1 a b\n";
        let scanned = crate::lexer::scan(src, FileId(0));
        let outcome = crate::parser::parse(scanned, FileId(0)).expect("ok");
        assert!(
            outcome.diagnostics.iter().any(|d| d.code == "W907"),
            "expected W907; got: {:?}",
            outcome.diagnostics
        );
    }
}
