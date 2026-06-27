//! Typed AST for a SPICE netlist.

use spice_diagnostics::Span;

#[derive(Debug, Clone, Default)]
pub struct Netlist {
    pub title: String,
    pub elements: Vec<Element>,
    pub subckts: Vec<Subckt>,
    pub models: Vec<Model>,
    pub directives: Vec<Directive>,
    /// Block-level `*@…` annotations declared at the top level of the
    /// file (i.e. outside any `.subckt` body).
    pub annotations: Vec<SpannedAnnotation>,
}

pub type NodeRef = String;
pub type Ident = String;

#[derive(Debug, Clone)]
pub struct Element {
    pub designator: Ident,
    pub kind: ElementKind,
    /// `nodes[i]` is a net name. Refdes references (e.g. K's coupled
    /// inductors) live in `coupled`; controlling-source refdes (F/H's
    /// Vname) lives in `control`.
    pub nodes: Vec<NodeRef>,
    pub value: Option<Value>,
    pub params: Vec<(Ident, Value)>,
    /// Trailing `;@…` annotations on this element. Empty if none.
    pub tags: Vec<SpannedTag>,
    /// Refdes of a controlling element (set on F/H — the SPICE Vname
    /// that names the voltage source whose current we read).
    pub control: Option<Ident>,
    /// Refdes references to coupled elements (set on K — the two
    /// inductor refdes the coupling refers to).
    pub coupled: Vec<Ident>,
}

impl Element {
    /// Construct an element with no tags. Convenience for tests and
    /// for the (future) parser to use before it has tag-parsing wired up.
    #[must_use]
    pub fn new(designator: impl Into<Ident>, kind: ElementKind, nodes: Vec<NodeRef>) -> Self {
        Self {
            designator: designator.into(),
            kind,
            nodes,
            value: None,
            params: Vec::new(),
            tags: Vec::new(),
            control: None,
            coupled: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementKind {
    Resistor,   // R
    Capacitor,  // C
    Inductor,   // L
    VoltageSrc, // V
    CurrentSrc, // I
    Diode,      // D
    Bjt,        // Q — 3 or 4 terminals (c b e [sub] model)
    Mosfet,     // M
    Jfet,       // J
    Subckt,     // X
    /// K — mutual inductance; `coupled` stores [L1, L2] (inductor refdes
    /// refs), `nodes` is empty, `value` stores the coupling coefficient.
    MutualInductance, // K
    /// F — current-controlled current source; nodes=[out+, out-],
    /// `control` holds the Vname refdes, value = gain.
    Cccs, // F
    /// H — current-controlled voltage source; same shape as Cccs.
    Ccvs, // H
    /// E — voltage-controlled voltage source; nodes=[out+, out-, ctrl+, ctrl-],
    /// value = gain.
    Vcvs, // E
    /// G — voltage-controlled current source; same shape as Vcvs.
    Vccs, // G
    Other,
}

#[derive(Debug, Clone)]
pub enum Value {
    Number(f64),
    String(String),
    Expr(String),
}

#[derive(Debug, Clone)]
pub struct Subckt {
    pub name: Ident,
    pub ports: Vec<NodeRef>,
    pub params: Vec<(Ident, Value)>,
    pub body: Vec<Element>,
    /// Block-level `*@…` annotations declared *inside* this subckt body.
    pub annotations: Vec<SpannedAnnotation>,
}

#[derive(Debug, Clone)]
pub struct Model {
    pub name: Ident,
    pub model_type: Ident,
    pub params: Vec<(Ident, Value)>,
}

#[derive(Debug, Clone)]
pub struct Directive {
    pub name: Ident,
    pub args: Vec<String>,
}

// ---------------------------------------------------------------------------
// Annotations (carrier-level types — see docs/annotation-spec.md)
// ---------------------------------------------------------------------------

/// A trailing `;@<directive>=<value>` tag on a SPICE element line.
#[derive(Debug, Clone)]
pub enum Tag {
    /// `;@ symbol=Lib:Name`
    Symbol(String),
    /// `;@ pinmap=1:2,2:1` — list preserves source order.
    Pinmap(Vec<PinmapEntry>),
    /// `;@ place=<relation> <anchor>` — passed through to the layout pass
    /// without validation here.
    Place { relation: Relation, anchor: String },
    /// `;@ power=<rail>` — only meaningful on voltage sources.
    Power(String),
    /// `;@ ignore` — element is dropped from the schematic.
    Ignore,
}

/// A `Tag` with optional source span.
#[derive(Debug, Clone)]
pub struct SpannedTag {
    pub tag: Tag,
    /// Span pointing at the `;@…` text in the source. `None` for
    /// hand-constructed test inputs; the real parser will always set it.
    pub span: Option<Span>,
}

impl SpannedTag {
    #[must_use]
    pub fn bare(tag: Tag) -> Self {
        Self { tag, span: None }
    }

    #[must_use]
    pub fn new(tag: Tag, span: Span) -> Self {
        Self {
            tag,
            span: Some(span),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PinmapEntry {
    /// 1-based SPICE terminal index.
    pub spice_index: usize,
    pub kicad_pin: PinRef,
}

#[derive(Debug, Clone)]
pub enum PinRef {
    /// e.g. `"1"`, `"2"`.
    Number(String),
    /// e.g. `"A"`, `"K"`, `"G"`.
    Name(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    RightOf,
    LeftOf,
    Above,
    Below,
}

/// A block-level `*@…` annotation directive.
#[derive(Debug, Clone)]
pub enum Annotation {
    /// `*@symbol Lib:Name for=<glob>` — default symbol mapping for any
    /// element whose refdes matches the glob. May carry an optional
    /// `pinmap=…` clause that flows through to matched elements that
    /// don't supply their own trailing `;@ pinmap=` tag.
    SymbolDefault {
        lib_id: String,
        for_glob: String,
        pinmap: Option<Vec<PinmapEntry>>,
    },
    /// `*@align <axis> R1 R2 …`
    Align { axis: Axis, refdes: Vec<String> },
    /// `*@spec version=<value>` — declares the annotation-spec version
    /// the file targets. Absent means "assume the current version".
    /// The version-handshake pass (in the CLI pipeline) rejects a
    /// declared version the converter does not support.
    SpecVersion(String),
}

#[derive(Debug, Clone)]
pub struct SpannedAnnotation {
    pub annotation: Annotation,
    pub span: Option<Span>,
}

impl SpannedAnnotation {
    #[must_use]
    pub fn bare(annotation: Annotation) -> Self {
        Self {
            annotation,
            span: None,
        }
    }

    #[must_use]
    pub fn new(annotation: Annotation, span: Span) -> Self {
        Self {
            annotation,
            span: Some(span),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}
