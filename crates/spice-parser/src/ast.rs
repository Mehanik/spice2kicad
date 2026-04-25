//! Typed AST for a SPICE netlist.

#[derive(Debug, Clone, Default)]
pub struct Netlist {
    pub title: String,
    pub elements: Vec<Element>,
    pub subckts: Vec<Subckt>,
    pub models: Vec<Model>,
    pub directives: Vec<Directive>,
}

pub type NodeRef = String;
pub type Ident = String;

#[derive(Debug, Clone)]
pub struct Element {
    pub designator: Ident,
    pub kind: ElementKind,
    pub nodes: Vec<NodeRef>,
    pub value: Option<Value>,
    pub params: Vec<(Ident, Value)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementKind {
    Resistor,   // R
    Capacitor,  // C
    Inductor,   // L
    VoltageSrc, // V
    CurrentSrc, // I
    Diode,      // D
    Bjt,        // Q
    Mosfet,     // M
    Jfet,       // J
    Subckt,     // X
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
