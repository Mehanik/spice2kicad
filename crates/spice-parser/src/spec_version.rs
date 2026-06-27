//! Annotation-spec version handshake.
//!
//! A file may declare the annotation-spec version it targets with a
//! `*@spec version=<value>` block directive (spec §4.7). This module
//! checks a parsed [`Netlist`]'s declared version against the version
//! this converter implements.
//!
//! Rules (spec §4.7):
//! - directive absent → assume the current version, no diagnostic
//!   (every existing zero-`*@spec` file must keep working);
//! - declared version this converter does not support → `E911`;
//! - declared version that matches → ok.

use spice_diagnostics::{Diagnostic, FileId, Label, Span};

use crate::Netlist;
use crate::ast::Annotation;

/// The annotation-spec version this converter implements.
pub const CURRENT_SPEC: &str = "0.1";

/// Check the netlist's declared `*@spec version=` (if any) against
/// [`CURRENT_SPEC`]. Returns one `E911` per unsupported declaration;
/// an empty list means "supported, or none declared".
///
/// The check is span-aware: each diagnostic points at the declaring
/// `*@spec` line when a span is available.
#[must_use]
pub fn check(netlist: &Netlist) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for ann in &netlist.annotations {
        if let Annotation::SpecVersion(declared) = &ann.annotation {
            if !is_supported(declared) {
                let span = ann.span.unwrap_or_else(|| Span::point(FileId(0), 0));
                diags.push(Diagnostic::error(
                    "E911",
                    format!(
                        "annotation-spec version `{declared}` is not supported \
                         (this converter implements {CURRENT_SPEC})"
                    ),
                    Label::new(span, ""),
                ));
            }
        }
    }
    diags
}

/// True when `declared` is a version this converter can honour.
///
/// v0.1 only implements its own version exactly; any other declared
/// value (a higher version, or an unparseable string) is unsupported.
fn is_supported(declared: &str) -> bool {
    declared == CURRENT_SPEC
}
