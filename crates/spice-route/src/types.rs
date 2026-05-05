//! Public router data types.
//!
//! [`RouteRequest`] / [`RouteResult`] are the stable interface between
//! `kicad-emitter` and `spice-route`. Internal stage modules
//! (`rails`, `steiner`, `cleanup`) consume these but layer their own
//! private types on top.

use lexpr::Value as Sexpr;

/// One pin on a routed net.
#[derive(Debug, Clone)]
pub struct PinRef {
    /// Index of the placed element this pin belongs to (caller-defined).
    pub element_idx: usize,
    /// KiCad pin number on that element.
    pub pin_number: u16,
    /// World X in millimetres, after rotation.
    pub x_mm: f64,
    /// World Y in millimetres, after rotation.
    pub y_mm: f64,
    /// Outward direction of the pin in world coordinates, post-rotation.
    pub outward: Direction,
}

/// Cardinal direction the pin's stem points away from its symbol body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// One net to route, with its pins and class hint.
#[derive(Debug, Clone)]
pub struct NetSpec {
    pub name: String,
    pub class: spice_layout::net_class::NetClass,
    pub pins: Vec<PinRef>,
}

/// Input to [`crate::route`].
#[derive(Debug, Clone)]
pub struct RouteRequest<'a> {
    pub nets: &'a [NetSpec],
    /// Sheet scope (root or hierarchical sheet name) — used for
    /// scoping junctions and labels in future stages.
    pub scope: &'a str,
}

/// Output of [`crate::route`].
///
/// `sexprs` is a flat list of `(wire …)` / `(junction …)` /
/// `(symbol …)` / `(label …)` nodes ready to splice into the
/// emitted schematic. Order is not significant — the emitter may
/// re-order before final write.
#[derive(Debug, Clone, Default)]
pub struct RouteResult {
    pub sexprs: Vec<Sexpr>,
    /// Diagnostics from rip-up failures, missing power symbols, etc.
    pub warnings: Vec<String>,
}

/// One axis-parallel wire segment in world millimetres.
///
/// Used internally by stage modules and exposed for tests / future
/// sub-crate consumers. The on-grid invariant (1.27 mm) is the
/// caller's responsibility.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

/// One routed net's intermediate result, before serialisation.
#[derive(Debug, Clone, Default)]
pub struct RoutedNet {
    pub segments: Vec<Segment>,
    pub junctions: Vec<(f64, f64)>,
}
