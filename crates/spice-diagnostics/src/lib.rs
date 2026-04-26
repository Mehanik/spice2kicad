//! Neutral diagnostic types shared across spice2kicad libraries.
//!
//! Libraries (parser, layout, etc.) emit [`Diagnostic`] values; the
//! CLI is responsible for rendering them to the user (typically via
//! `ariadne`). Keeping the type renderer-agnostic means library
//! crates do not pull in terminal-styling dependencies.
//!
//! See `docs/layout-adr.md` ADR-6 and `docs/annotation-spec.md` §7
//! for the design rationale and the stable code catalog.

#![forbid(unsafe_code)]

/// Severity level of a [`Diagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

/// Identifier of a source file within a [`SourceMap`]-like store.
///
/// The numeric value is opaque to library code; only the renderer
/// (the CLI) knows how to turn a `FileId` back into a path and
/// contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub u32);

/// A byte-offset range within a single source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub file: FileId,
    /// Byte offset, inclusive.
    pub start: usize,
    /// Byte offset, exclusive.
    pub end: usize,
}

impl Span {
    pub fn new(file: FileId, start: usize, end: usize) -> Self {
        Self { file, start, end }
    }

    /// Construct a zero-width span at `offset`.
    pub fn point(file: FileId, offset: usize) -> Self {
        Self {
            file,
            start: offset,
            end: offset,
        }
    }

    /// Merge two spans into the smallest span covering both.
    ///
    /// Returns `None` if the spans live in different files.
    pub fn merge(a: Span, b: Span) -> Option<Span> {
        if a.file != b.file {
            return None;
        }
        Some(Span {
            file: a.file,
            start: a.start.min(b.start),
            end: a.end.max(b.end),
        })
    }
}

/// A spanned message attached to a [`Diagnostic`].
#[derive(Debug, Clone)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

impl Label {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

/// A user-facing diagnostic message.
///
/// `code` is one of the stable codes defined in
/// `docs/annotation-spec.md` §7 (e.g. `"E001"`, `"W101"`).
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
    pub primary: Label,
    pub secondary: Vec<Label>,
    pub help: Option<String>,
}

impl Diagnostic {
    fn new(
        code: &'static str,
        severity: Severity,
        message: impl Into<String>,
        primary: Label,
    ) -> Self {
        Self {
            code,
            severity,
            message: message.into(),
            primary,
            secondary: Vec::new(),
            help: None,
        }
    }

    pub fn error(code: &'static str, message: impl Into<String>, primary: Label) -> Self {
        Self::new(code, Severity::Error, message, primary)
    }

    pub fn warning(code: &'static str, message: impl Into<String>, primary: Label) -> Self {
        Self::new(code, Severity::Warning, message, primary)
    }

    pub fn note(code: &'static str, message: impl Into<String>, primary: Label) -> Self {
        Self::new(code, Severity::Note, message, primary)
    }

    #[must_use]
    pub fn with_secondary(mut self, label: Label) -> Self {
        self.secondary.push(label);
        self
    }

    #[must_use]
    pub fn with_help(mut self, text: impl Into<String>) -> Self {
        self.help = Some(text.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fid() -> FileId {
        FileId(0)
    }

    #[test]
    fn span_point_is_zero_width() {
        let s = Span::point(fid(), 7);
        assert_eq!(s.start, 7);
        assert_eq!(s.end, 7);
    }

    #[test]
    fn span_new_records_endpoints() {
        let s = Span::new(fid(), 3, 8);
        assert_eq!((s.start, s.end), (3, 8));
        assert_eq!(s.file, fid());
    }

    #[test]
    fn span_merge_same_file() {
        let a = Span::new(fid(), 2, 5);
        let b = Span::new(fid(), 4, 9);
        let merged = Span::merge(a, b).expect("same file merges");
        assert_eq!(merged.start, 2);
        assert_eq!(merged.end, 9);

        // Order-independent.
        let merged2 = Span::merge(b, a).unwrap();
        assert_eq!(merged, merged2);
    }

    #[test]
    fn span_merge_different_files_returns_none() {
        let a = Span::new(FileId(0), 0, 3);
        let b = Span::new(FileId(1), 0, 3);
        assert!(Span::merge(a, b).is_none());
    }

    #[test]
    fn diagnostic_error_constructor() {
        let d = Diagnostic::error(
            "E001",
            "unknown refdes",
            Label::new(Span::new(fid(), 0, 2), "here"),
        );
        assert_eq!(d.code, "E001");
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.message, "unknown refdes");
        assert_eq!(d.primary.message, "here");
        assert!(d.secondary.is_empty());
        assert!(d.help.is_none());
    }

    #[test]
    fn diagnostic_warning_and_note_constructors() {
        let w = Diagnostic::warning("W101", "conflict", Label::new(Span::point(fid(), 0), ""));
        assert_eq!(w.severity, Severity::Warning);
        let n = Diagnostic::note("N000", "fyi", Label::new(Span::point(fid(), 0), ""));
        assert_eq!(n.severity, Severity::Note);
    }

    #[test]
    fn diagnostic_chainable_builders() {
        let d = Diagnostic::error(
            "E001",
            "boom",
            Label::new(Span::new(fid(), 0, 1), "primary"),
        )
        .with_secondary(Label::new(Span::new(fid(), 5, 6), "see also"))
        .with_secondary(Label::new(Span::new(fid(), 9, 10), "and here"))
        .with_help("try frobbing the widget");

        assert_eq!(d.secondary.len(), 2);
        assert_eq!(d.secondary[0].message, "see also");
        assert_eq!(d.secondary[1].message, "and here");
        assert_eq!(d.help.as_deref(), Some("try frobbing the widget"));
    }
}
