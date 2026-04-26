//! Diagnostic rendering — the only place `ariadne` is allowed to
//! appear. Library crates emit neutral [`Diagnostic`]s; the CLI
//! turns them into terminal output here.

use std::io;
use std::path::{Path, PathBuf};

use ariadne::{Color, Label as AriadneLabel, Report, ReportKind};
use spice_diagnostics::{Diagnostic, FileId, Severity};

/// Owns the source text for every file referenced by diagnostic
/// spans. The `FileId.0` value indexes into the internal `Vec`.
#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<(PathBuf, String)>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a source file and obtain a [`FileId`] for embedding
    /// in [`spice_diagnostics::Span`]s.
    pub fn add(&mut self, path: PathBuf, contents: String) -> FileId {
        let id = u32::try_from(self.files.len()).expect("source map overflow");
        self.files.push((path, contents));
        FileId(id)
    }

    pub fn get(&self, id: FileId) -> Option<(&Path, &str)> {
        self.files
            .get(id.0 as usize)
            .map(|(p, s)| (p.as_path(), s.as_str()))
    }
}

// ariadne keys cache entries by something with `Display + Hash +
// Eq + Clone`. We use the file's display path; that's what the
// renderer prints in the gutter.
type CacheKey = String;

fn cache_key(sources: &SourceMap, file: FileId) -> CacheKey {
    sources.get(file).map_or_else(
        || format!("<unknown file {}>", file.0),
        |(p, _)| p.display().to_string(),
    )
}

fn build_cache(sources: &SourceMap) -> impl ariadne::Cache<CacheKey> {
    let entries: Vec<(CacheKey, String)> = sources
        .files
        .iter()
        .map(|(p, s)| (p.display().to_string(), s.clone()))
        .collect();
    ariadne::sources(entries)
}

fn report_kind(severity: Severity) -> ReportKind<'static> {
    match severity {
        Severity::Error => ReportKind::Error,
        Severity::Warning => ReportKind::Warning,
        Severity::Note => ReportKind::Advice,
    }
}

pub fn render_diagnostic(
    diag: &Diagnostic,
    sources: &SourceMap,
    out: &mut impl io::Write,
) -> io::Result<()> {
    let primary_key = cache_key(sources, diag.primary.span.file);
    let primary_range = diag.primary.span.start..diag.primary.span.end;

    let mut report = Report::build(
        report_kind(diag.severity),
        (primary_key.clone(), primary_range.clone()),
    )
    .with_code(diag.code)
    .with_message(&diag.message)
    .with_label(
        AriadneLabel::new((primary_key, primary_range))
            .with_message(&diag.primary.message)
            .with_color(Color::Red),
    );

    for label in &diag.secondary {
        let key = cache_key(sources, label.span.file);
        report = report.with_label(
            AriadneLabel::new((key, label.span.start..label.span.end))
                .with_message(&label.message)
                .with_color(Color::Blue),
        );
    }

    if let Some(help) = &diag.help {
        report = report.with_help(help);
    }

    report
        .finish()
        .write(build_cache(sources), out)
        .map_err(|e| io::Error::other(format!("ariadne render error: {e}")))
}

pub fn render_all(
    diags: &[Diagnostic],
    sources: &SourceMap,
    out: &mut impl io::Write,
) -> io::Result<()> {
    for d in diags {
        render_diagnostic(d, sources, out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use spice_diagnostics::{Label, Span};

    #[test]
    fn render_includes_code_and_message() {
        let mut sources = SourceMap::new();
        let file = sources.add(
            PathBuf::from("test.cir"),
            "R1 in out 1k\nC1 out 0 100n\n".to_string(),
        );

        let diag = Diagnostic::error(
            "E001",
            "unknown refdes 'R99'",
            Label::new(Span::new(file, 0, 2), "no such element"),
        )
        .with_help("did you mean R1?");

        let mut out = Vec::new();
        render_diagnostic(&diag, &sources, &mut out).expect("render ok");
        let text = String::from_utf8(out).expect("utf8");
        // ANSI styling around tokens; assert plain substrings only.
        assert!(text.contains("E001"), "missing code: {text}");
        assert!(text.contains("unknown refdes"), "missing message: {text}");
    }

    #[test]
    fn source_map_round_trip() {
        let mut sm = SourceMap::new();
        let id = sm.add(PathBuf::from("a.cir"), "hi".to_string());
        let (p, s) = sm.get(id).expect("present");
        assert_eq!(p, Path::new("a.cir"));
        assert_eq!(s, "hi");
        assert!(sm.get(FileId(99)).is_none());
    }
}
