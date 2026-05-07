//! Resolve a parsed SPICE [`Netlist`] against a KiCad [`Library`] to a
//! [`ResolvedNetlist`].
//!
//! This crate implements the pass on the upstream side of ADR-2's
//! "resolved-AST boundary": every element is bound to a concrete
//! `Lib:Name` symbol, its terminals are mapped to KiCad pin numbers,
//! and roles like "this V source is actually a power rail" are
//! recorded explicitly. Layout-only directives (`place`, `align`)
//! are passed through unchanged for the layout pass to consume.
//!
//! # Diagnostic codes emitted
//!
//! - **E002** — symbol pin count mismatch (with or without `pinmap`)
//! - **E003** — unknown library symbol
//! - **E005** — invalid `pinmap` (unknown pin, out-of-range
//!   spice index, duplicate spice index, duplicate kicad pin)
//! - **E008** — default pin mapping cannot synthesize because the
//!   symbol is missing a canonical pin name for the element's kind
//!   (e.g. a 3-pin BJT-target symbol with no pin named `B`)
//! - **W103** — multiple conflicting tags on one element (e.g. two
//!   `;@ symbol=` tags); the first is kept
//!
//! `E001`/`E004` and `W101`/`W102`/`W104` are owned by other passes
//! and are not emitted here.

#![forbid(unsafe_code)]

mod default_pinmap;

use std::collections::{HashMap, HashSet};

use crate::default_pinmap::{DefaultPinmapError, synthesize as synthesize_default_pinmap};

use kicad_symbols::{Library, Pin, Symbol};
use spice_diagnostics::{Diagnostic, Label, Severity, Span};
use spice_parser::ast::{Annotation, Element, Netlist, SpannedAnnotation, SpannedTag, Subckt, Tag};
pub use spice_parser::ast::{Axis, ElementKind, PinRef, PinmapEntry, Relation, Value};

// ---------------------------------------------------------------------------
// Public output types
// ---------------------------------------------------------------------------

/// A fully resolved netlist: every kept element has a concrete symbol
/// and an explicit terminal-to-pin mapping. Layout directives are
/// carried through verbatim.
#[derive(Debug, Clone, Default)]
pub struct ResolvedNetlist {
    pub elements: Vec<ResolvedElement>,
    /// `*@align` directives preserved for the layout pass.
    pub align: Vec<AlignSpec>,
    /// `;@ place=…` tags preserved for the layout pass.
    pub place: Vec<PlaceSpec>,
    /// One entry per `.subckt` definition. Carries the port list (used
    /// by the layout signal-flow term) and the resolved body elements
    /// (placed on a child hierarchical sheet by the emitter).
    pub subckts: Vec<SubcktPorts>,
    /// Top-level `X<n>` instances. Each becomes a `(sheet …)` block on
    /// the parent schematic. Nested X instances (X inside a subckt
    /// body) are not yet supported.
    pub sheet_instances: Vec<SheetInstance>,
}

/// Port list and resolved body for a `.subckt` definition.
#[derive(Debug, Clone)]
pub struct SubcktPorts {
    pub name: String,
    pub ports: Vec<String>,
    /// Resolved elements that live inside this `.subckt` body. They
    /// appear only on the corresponding child hierarchical sheet, not
    /// on the parent schematic.
    pub elements: Vec<ResolvedElement>,
}

/// A top-level `X<n> ... <subckt-name>` instance. Lowered to a KiCad
/// hierarchical-sheet block by the emitter.
#[derive(Debug, Clone)]
pub struct SheetInstance {
    pub refdes: String,
    pub subckt_name: String,
    /// SPICE nodes wired to the instance, in the same order as
    /// `SubcktPorts.ports` for the matching subckt definition.
    pub nodes: Vec<String>,
}

/// A SPICE element bound to a KiCad symbol.
#[derive(Debug, Clone)]
pub struct ResolvedElement {
    pub refdes: String,
    pub kind: ElementKind,
    pub lib_id: String,
    /// Owned clone of the library symbol — pin geometry is attached.
    pub symbol: Symbol,
    /// Maps SPICE terminals (1-based) to KiCad pin numbers. Index
    /// `i` (0-based) holds the KiCad pin number that corresponds to
    /// SPICE terminal `i + 1`.
    pub pin_mapping: Vec<String>,
    /// SPICE node names — same as input, terminal order preserved.
    pub nodes: Vec<String>,
    pub value: Option<Value>,
    pub role: ElementRole,
}

/// Functional role of a resolved element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElementRole {
    Normal,
    /// Voltage source flagged with `;@ power=<rail>`. Layout/emitter
    /// substitute a power flag instead of drawing the source body.
    Power(String),
}

/// A pass-through `*@align` directive.
#[derive(Debug, Clone)]
pub struct AlignSpec {
    pub axis: Axis,
    pub refdes: Vec<String>,
    pub span: Option<Span>,
}

/// A pass-through `;@ place=` tag.
#[derive(Debug, Clone)]
pub struct PlaceSpec {
    pub refdes: String,
    pub relation: Relation,
    pub anchor: String,
    pub span: Option<Span>,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Resolve a [`Netlist`] against a [`Library`].
///
/// On success returns a fully populated [`ResolvedNetlist`]. On
/// failure returns the list of diagnostics collected so far,
/// including any non-fatal warnings that preceded the first fatal
/// error. The resolver does not currently have a "success with
/// warnings" path — warnings are only surfaced when paired with at
/// least one fatal diagnostic. (When that becomes a real need, this
/// signature can be widened to `(ResolvedNetlist, Vec<Diagnostic>)`
/// without breaking the error path.)
pub fn resolve(netlist: &Netlist, library: &Library) -> Result<ResolvedNetlist, Vec<Diagnostic>> {
    let mut diags: Vec<Diagnostic> = Vec::new();
    let mut out_elements: Vec<ResolvedElement> = Vec::new();
    let mut place: Vec<PlaceSpec> = Vec::new();
    let mut sheet_instances: Vec<SheetInstance> = Vec::new();

    // Top-level annotations only apply to top-level elements.
    let block_symbols = collect_symbol_defaults(&netlist.annotations);

    let defined_subckts: HashSet<&str> = netlist.subckts.iter().map(|s| s.name.as_str()).collect();

    for element in &netlist.elements {
        // Top-level `X…` instances become hierarchical-sheet blocks
        // unless the user supplied an explicit `;@ symbol=` (in which
        // case the user opted into a flat-symbol mapping and we keep
        // the existing path). Instances whose subckt is not defined
        // in the file fall through to the regular element resolver,
        // which will emit `E003` for the missing symbol mapping.
        if element.kind == ElementKind::Subckt
            && !has_explicit_symbol_tag(element)
            && !has_block_symbol_override(element, &block_symbols)
            && let Some(name) = subckt_name(element)
            && defined_subckts.contains(name.as_str())
        {
            // Skip if `;@ ignore` is set.
            if element.tags.iter().any(|t| matches!(t.tag, Tag::Ignore)) {
                continue;
            }
            sheet_instances.push(SheetInstance {
                refdes: element.designator.clone(),
                subckt_name: name,
                nodes: element.nodes.clone(),
            });
            continue;
        }
        resolve_element(
            element,
            &block_symbols,
            library,
            &mut out_elements,
            &mut place,
            &mut diags,
        );
    }

    // Subckts: each subckt has its own scope of *@symbol defaults.
    // Body elements live on the child hierarchical sheet, so we collect
    // them into a per-subckt list rather than the flat top-level
    // `out_elements`. (ADR-2 / spec §3.)
    let mut subckts: Vec<SubcktPorts> = Vec::with_capacity(netlist.subckts.len());
    for subckt in &netlist.subckts {
        let body = resolve_subckt(subckt, library, &mut place, &mut diags);
        subckts.push(SubcktPorts {
            name: subckt.name.clone(),
            ports: subckt.ports.clone(),
            elements: body,
        });
    }

    let align = collect_align(&netlist.annotations)
        .into_iter()
        .chain(
            netlist
                .subckts
                .iter()
                .flat_map(|s| collect_align(&s.annotations)),
        )
        .collect();

    if diags.iter().any(|d| d.severity == Severity::Error) {
        return Err(diags);
    }
    // Drop any non-fatal warnings on the floor for now (see doc on
    // `resolve` above). They have nowhere to go on the success path.
    Ok(ResolvedNetlist {
        elements: out_elements,
        align,
        place,
        subckts,
        sheet_instances,
    })
}

fn has_explicit_symbol_tag(element: &Element) -> bool {
    element.tags.iter().any(|t| matches!(t.tag, Tag::Symbol(_)))
}

/// True if any block-form `*@symbol … for=<glob>` matches this element's
/// refdes. Used to suppress the default `X<n>` → hierarchical-sheet
/// routing when the user has opted into a flat-symbol mapping for that
/// instance (CLAUDE.md V8).
fn has_block_symbol_override(element: &Element, block_symbols: &[BlockSymbol<'_>]) -> bool {
    block_symbols
        .iter()
        .any(|bs| glob_matches(bs.glob, &element.designator))
}

/// For an `X…` instance, the subckt name is the last positional token,
/// stored by the parser in `value` as `Value::String`.
fn subckt_name(element: &Element) -> Option<String> {
    match &element.value {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn resolve_subckt(
    subckt: &Subckt,
    library: &Library,
    place: &mut Vec<PlaceSpec>,
    diags: &mut Vec<Diagnostic>,
) -> Vec<ResolvedElement> {
    let block_symbols = collect_symbol_defaults(&subckt.annotations);
    let mut body: Vec<ResolvedElement> = Vec::new();
    for element in &subckt.body {
        resolve_element(element, &block_symbols, library, &mut body, place, diags);
    }
    body
}

// ---------------------------------------------------------------------------
// Per-element resolution
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ElementTags<'a> {
    symbol: Option<&'a str>,
    symbol_span: Option<Span>,
    pinmap: Option<&'a [PinmapEntry]>,
    pinmap_span: Option<Span>,
    power: Option<&'a str>,
    ignore: bool,
    ignore_span: Option<Span>,
}

fn collect_tags<'a>(
    refdes: &str,
    tags: &'a [SpannedTag],
    diags: &mut Vec<Diagnostic>,
) -> ElementTags<'a> {
    let mut out = ElementTags::default();
    for spanned in tags {
        match &spanned.tag {
            Tag::Symbol(lib_id) => {
                if out.symbol.is_some() {
                    push_warn(
                        diags,
                        "W103",
                        format!(
                            "element `{refdes}` has multiple `symbol=` tags; keeping the first"
                        ),
                        spanned.span,
                    );
                } else {
                    out.symbol = Some(lib_id.as_str());
                    out.symbol_span = spanned.span;
                }
            }
            Tag::Pinmap(entries) => {
                if out.pinmap.is_some() {
                    push_warn(
                        diags,
                        "W103",
                        format!(
                            "element `{refdes}` has multiple `pinmap=` tags; keeping the first"
                        ),
                        spanned.span,
                    );
                } else {
                    out.pinmap = Some(entries.as_slice());
                    out.pinmap_span = spanned.span;
                }
            }
            Tag::Power(rail) => {
                if out.power.is_some() {
                    push_warn(
                        diags,
                        "W103",
                        format!("element `{refdes}` has multiple `power=` tags; keeping the first"),
                        spanned.span,
                    );
                } else {
                    out.power = Some(rail.as_str());
                }
            }
            Tag::Ignore => {
                out.ignore = true;
                out.ignore_span = spanned.span;
            }
            Tag::Place { .. } => {
                // Place tags are pass-through; handled separately.
            }
        }
    }
    if out.ignore && (out.symbol.is_some() || out.pinmap.is_some() || out.power.is_some()) {
        push_warn(
            diags,
            "W103",
            format!(
                "element `{refdes}` has `ignore` together with other directives; the element is dropped"
            ),
            out.ignore_span,
        );
    }
    out
}

#[allow(clippy::too_many_lines)]
fn resolve_element(
    element: &Element,
    block_symbols: &[BlockSymbol<'_>],
    library: &Library,
    out_elements: &mut Vec<ResolvedElement>,
    place: &mut Vec<PlaceSpec>,
    diags: &mut Vec<Diagnostic>,
) {
    // Always record place tags, even for elements that turn out to be
    // ignored — the layout pass owns that policy decision.
    for spanned in &element.tags {
        if let Tag::Place { relation, anchor } = &spanned.tag {
            place.push(PlaceSpec {
                refdes: element.designator.clone(),
                relation: *relation,
                anchor: anchor.clone(),
                span: spanned.span,
            });
        }
    }

    let tags = collect_tags(&element.designator, &element.tags, diags);

    if tags.ignore {
        return;
    }

    // 1. Determine lib_id (and any block-form pinmap that came with it).
    let (lib_id, block_pinmap) = match resolve_lib_id(element, &tags, block_symbols) {
        Ok(r) => r,
        Err(reason) => {
            push_err(
                diags,
                "E003",
                format!("no symbol mapping for `{}`: {reason}", element.designator),
                tags.symbol_span,
            );
            return;
        }
    };

    // 2. Look up symbol.
    let Some(symbol) = library.lookup(&lib_id) else {
        push_err(
            diags,
            "E003",
            format!(
                "unknown library symbol `{lib_id}` for element `{}`",
                element.designator
            ),
            tags.symbol_span,
        );
        return;
    };

    // 3. Pin mapping.
    let arity = element.nodes.len();
    let pin_count = symbol.pin_count();

    let effective_pinmap = tags.pinmap.or(block_pinmap);
    let pin_mapping = if let Some(entries) = effective_pinmap {
        match build_pinmap(
            &element.designator,
            arity,
            symbol,
            entries,
            tags.pinmap_span,
            diags,
        ) {
            Some(m) => m,
            None => return,
        }
    } else {
        // No user-supplied pinmap. Synthesize one from the kind's
        // canonical pin-name table so we map by name (V11) rather than
        // by parsed declaration order. Any failure here is fatal in
        // the same way an explicit-but-broken pinmap would be.
        match synthesize_default_pinmap(element.kind, symbol, arity) {
            Ok(entries) => {
                match build_pinmap(&element.designator, arity, symbol, &entries, None, diags) {
                    Some(m) => m,
                    None => return,
                }
            }
            Err(DefaultPinmapError::ArityMismatch { .. }) => {
                push_err(
                    diags,
                    "E002",
                    format!(
                        "element `{}` has {arity} terminal(s) but symbol `{lib_id}` has {pin_count} pin(s); add `;@ pinmap=…`",
                        element.designator
                    ),
                    None,
                );
                return;
            }
            Err(DefaultPinmapError::MissingNamedPin {
                expected,
                lib_id: bad_lib,
            }) => {
                push_err(
                    diags,
                    "E008",
                    format!(
                        "default pin mapping for {kind:?} element `{refdes}` expected pin name `{expected}` on symbol `{bad_lib}`; supply `;@ pinmap=…` to override",
                        kind = element.kind,
                        refdes = element.designator,
                    ),
                    None,
                );
                return;
            }
        }
    };

    // 4. Role.
    let role = match tags.power {
        Some(rail) => ElementRole::Power(rail.to_owned()),
        None => ElementRole::Normal,
    };

    out_elements.push(ResolvedElement {
        refdes: element.designator.clone(),
        kind: element.kind,
        lib_id,
        symbol: symbol.clone(),
        pin_mapping,
        nodes: element.nodes.clone(),
        value: element.value.clone(),
        role,
    });
}

fn build_pinmap(
    refdes: &str,
    arity: usize,
    symbol: &Symbol,
    entries: &[PinmapEntry],
    pinmap_span: Option<Span>,
    diags: &mut Vec<Diagnostic>,
) -> Option<Vec<String>> {
    if entries.len() != arity {
        push_err(
            diags,
            "E002",
            format!(
                "element `{refdes}` has {arity} terminal(s) but `pinmap` lists {n}",
                n = entries.len()
            ),
            pinmap_span,
        );
        return None;
    }

    // Build name/number indexes for the symbol.
    let by_number: HashMap<&str, &Pin> =
        symbol.pins.iter().map(|p| (p.number.as_str(), p)).collect();
    let by_name: HashMap<&str, &Pin> = symbol.pins.iter().map(|p| (p.name.as_str(), p)).collect();

    let mut out: Vec<String> = vec![String::new(); arity];
    let mut filled = vec![false; arity];
    let mut seen_kicad: HashSet<String> = HashSet::new();

    for entry in entries {
        if entry.spice_index < 1 || entry.spice_index > arity {
            push_err(
                diags,
                "E005",
                format!(
                    "element `{refdes}` pinmap: spice index {} is out of range 1..={arity}",
                    entry.spice_index
                ),
                pinmap_span,
            );
            return None;
        }
        let idx = entry.spice_index - 1;
        if filled[idx] {
            push_err(
                diags,
                "E005",
                format!(
                    "element `{refdes}` pinmap: duplicate spice index {}",
                    entry.spice_index
                ),
                pinmap_span,
            );
            return None;
        }
        let pin = match &entry.kicad_pin {
            PinRef::Number(n) => by_number.get(n.as_str()).copied(),
            PinRef::Name(n) => by_name.get(n.as_str()).copied(),
        };
        let Some(pin) = pin else {
            let what = match &entry.kicad_pin {
                PinRef::Number(n) => format!("number `{n}`"),
                PinRef::Name(n) => format!("name `{n}`"),
            };
            push_err(
                diags,
                "E005",
                format!(
                    "element `{refdes}` pinmap: symbol `{}` has no pin with {what}",
                    symbol.lib_id
                ),
                pinmap_span,
            );
            return None;
        };
        if !seen_kicad.insert(pin.number.clone()) {
            push_err(
                diags,
                "E005",
                format!(
                    "element `{refdes}` pinmap: kicad pin `{}` referenced more than once",
                    pin.number
                ),
                pinmap_span,
            );
            return None;
        }
        out[idx].clone_from(&pin.number);
        filled[idx] = true;
    }

    if !filled.iter().all(|b| *b) {
        // Missing index. We checked length and uniqueness above so
        // this is unreachable if entries are well-formed, but the
        // explicit check guards against future refactors.
        push_err(
            diags,
            "E005",
            format!("element `{refdes}` pinmap is incomplete"),
            pinmap_span,
        );
        return None;
    }

    Some(out)
}

// ---------------------------------------------------------------------------
// Symbol resolution
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct BlockSymbol<'a> {
    lib_id: &'a str,
    glob: &'a str,
    pinmap: Option<&'a [PinmapEntry]>,
}

fn collect_symbol_defaults(annotations: &[SpannedAnnotation]) -> Vec<BlockSymbol<'_>> {
    annotations
        .iter()
        .filter_map(|a| match &a.annotation {
            Annotation::SymbolDefault {
                lib_id,
                for_glob,
                pinmap,
            } => Some(BlockSymbol {
                lib_id: lib_id.as_str(),
                glob: for_glob.as_str(),
                pinmap: pinmap.as_deref(),
            }),
            Annotation::Align { .. } => None,
        })
        .collect()
}

fn collect_align(annotations: &[SpannedAnnotation]) -> Vec<AlignSpec> {
    annotations
        .iter()
        .filter_map(|a| match &a.annotation {
            Annotation::Align { axis, refdes } => Some(AlignSpec {
                axis: *axis,
                refdes: refdes.clone(),
                span: a.span,
            }),
            Annotation::SymbolDefault { .. } => None,
        })
        .collect()
}

fn resolve_lib_id<'a>(
    element: &Element,
    tags: &ElementTags<'_>,
    block_symbols: &'a [BlockSymbol<'a>],
) -> Result<(String, Option<&'a [PinmapEntry]>), String> {
    if let Some(s) = tags.symbol {
        return Ok((s.to_owned(), None));
    }
    // Latest matching block annotation wins. Spec §4.1: "If two
    // [block] directives match the same element, the more specific
    // wins; on a tie, the later one wins." We do not implement
    // specificity yet (KISS — ADR principle 7); last-match-wins is a
    // safe subset.
    if let Some(matched) = block_symbols
        .iter()
        .rev()
        .find(|bs| glob_matches(bs.glob, &element.designator))
    {
        return Ok((matched.lib_id.to_owned(), matched.pinmap));
    }
    if let Some(default) = default_lib_id(element.kind) {
        return Ok((default.to_owned(), None));
    }
    Err(match element.kind {
        ElementKind::Subckt => {
            "subckt instances (`X…`) require an explicit `;@ symbol=` tag".to_owned()
        }
        ElementKind::Cccs | ElementKind::Ccvs | ElementKind::MutualInductance => {
            "current-controlled sources (F/H) and mutual inductance (K) have no canonical \
             KiCad symbol; supply `;@ symbol=Lib:Name` (and `;@ pinmap=…` if needed)"
                .to_owned()
        }
        ElementKind::Other => "no built-in default for this element kind".to_owned(),
        _ => "no built-in default and no matching annotation".to_owned(),
    })
}

fn default_lib_id(kind: ElementKind) -> Option<&'static str> {
    Some(match kind {
        ElementKind::Resistor => "Device:R",
        ElementKind::Capacitor => "Device:C",
        ElementKind::Inductor => "Device:L",
        ElementKind::Diode => "Device:D",
        ElementKind::Bjt => "Device:Q_NPN_BCE",
        ElementKind::Mosfet => "Device:Q_NMOS_GDS",
        ElementKind::Jfet => "Device:Q_NJFET_GDS",
        ElementKind::VoltageSrc => "Simulation_SPICE:VDC",
        ElementKind::CurrentSrc => "Simulation_SPICE:IDC",
        ElementKind::Vcvs => "Simulation_SPICE:ESOURCE",
        ElementKind::Vccs => "Simulation_SPICE:GSOURCE",
        ElementKind::Subckt
        | ElementKind::MutualInductance
        | ElementKind::Cccs
        | ElementKind::Ccvs
        | ElementKind::Other => return None,
    })
}

/// Match a glob against a refdes. Supports only `*` (matches any run
/// including empty); case-insensitive ASCII; no other metacharacters.
fn glob_matches(glob: &str, refdes: &str) -> bool {
    let g = glob.as_bytes();
    let r = refdes.as_bytes();
    glob_match_bytes(g, r)
}

fn glob_match_bytes(g: &[u8], r: &[u8]) -> bool {
    // Simple recursive matcher; refdes strings are short.
    let mut gi = 0;
    let mut ri = 0;
    let mut star: Option<(usize, usize)> = None;
    while ri < r.len() {
        if gi < g.len() && g[gi] == b'*' {
            star = Some((gi, ri));
            gi += 1;
        } else if gi < g.len() && eq_ci(g[gi], r[ri]) {
            gi += 1;
            ri += 1;
        } else if let Some((sg, sr)) = star {
            gi = sg + 1;
            ri = sr + 1;
            star = Some((sg, sr + 1));
        } else {
            return false;
        }
    }
    while gi < g.len() && g[gi] == b'*' {
        gi += 1;
    }
    gi == g.len()
}

fn eq_ci(a: u8, b: u8) -> bool {
    a.eq_ignore_ascii_case(&b)
}

// ---------------------------------------------------------------------------
// Diagnostic helpers
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
        // Make it explicit in tooling that the location was not
        // supplied (hand-constructed AST in tests).
        d = d.with_help("source span unavailable for this diagnostic");
    }
    d
}
