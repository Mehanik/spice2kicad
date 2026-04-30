//! Line-oriented SPICE scanner.
//!
//! SPICE is line-oriented: each non-blank, non-comment line is either an
//! element, a directive (`.something`), or a structural marker. Lines that
//! begin with `+` continue the previous logical line. Lines beginning with
//! `*` are comments — except for `*@…`, which carries a block-form
//! annotation (see `docs/annotation-spec.md`). Trailing `;…` text is an
//! inline comment, and `;@…` within it is a trailing-tag annotation.
//!
//! The scanner is responsible for:
//!
//! * Splitting source into physical lines while tracking byte offsets.
//! * Stitching `+`-continuation lines into one logical line.
//! * Splitting code from inline (`;` or qualifying `$`) comments and harvesting `;@` tags.
//! * Tokenising the code (or `*@` body) into whitespace/`=`/`(`/`)`-
//!   separated [`Word`]s with source spans.
//! * Skipping `.control … .endc` blocks entirely (per spec §8 caveat 2).
//!
//! Higher-level interpretation (which words are nodes, which are values,
//! which directive is which) is the parser's job.

use spice_diagnostics::{Diagnostic, FileId, Label, Span};

/// One logical line: either a code line or a `*@` block-annotation line.
#[derive(Debug, Clone)]
pub struct LogicalLine {
    pub kind: LineKind,
    pub words: Vec<Word>,
    /// Trailing `;@…` tags collected from this logical line and any of
    /// its `+`-continuation lines.
    pub tags: Vec<RawTag>,
    /// Span covering the first physical line through the last
    /// continuation line.
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// An element or directive line (anything that isn't a `*@` block
    /// annotation).
    Code,
    /// A `*@…` block-form annotation. `words` contains the body after
    /// the `*@` marker.
    BlockAnnotation,
}

/// A whitespace/`=`/`(`/`)`-separated word with a source span.
#[derive(Debug, Clone)]
pub struct Word {
    pub text: String,
    pub span: Span,
}

/// A trailing `;@<body>` tag captured from a code line.
#[derive(Debug, Clone)]
pub struct RawTag {
    pub body: String,
    /// Span of `;@<body>` including the marker bytes — for "this whole
    /// tag is malformed" diagnostics.
    pub outer_span: Span,
    /// Span of `<body>` only — preferred for diagnostic labels that
    /// point at the value text rather than the marker.
    pub body_span: Span,
}

/// Output of [`scan`].
#[derive(Debug, Clone)]
pub struct Scanned {
    /// SPICE convention: the first physical line of the deck is the title
    /// line, regardless of whether it looks like a comment.
    pub title: String,
    pub title_span: Span,
    pub lines: Vec<LogicalLine>,
    /// Lexer-level diagnostics (e.g. `.if`-block skipping).
    pub diagnostics: Vec<Diagnostic>,
}

/// Scan `source` into logical lines. Never fails; the parser handles
/// semantic errors. The returned diagnostics list is currently empty —
/// reserved for future lexer-level warnings.
#[allow(clippy::too_many_lines)]
pub fn scan(source: &str, file: FileId) -> Scanned {
    let physical = split_physical_lines(source, file);

    // Title line: first physical line, even if it begins with `*`.
    let (title, title_span, body_start) = match physical.first() {
        Some(first) => (
            first.text.trim_start_matches('*').trim().to_owned(),
            first.span,
            1,
        ),
        None => (String::new(), Span::point(file, 0), 0),
    };

    let mut out: Vec<LogicalLine> = Vec::new();
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut in_control = false;
    // Depth of `.if … .endif` nesting. We don't evaluate conditions —
    // both branches would otherwise survive and collide refdes-wise, so
    // the whole block is dropped with a single W911 per top-level `.if`.
    let mut conditional_depth: usize = 0;

    for phys in &physical[body_start..] {
        let trimmed = phys.text.trim_start();
        let leading_ws = phys.text.len() - trimmed.len();
        let leading_offset = phys.span.start + leading_ws;

        // `.control` block: skip everything between `.control` and `.endc`.
        if !in_control && starts_with_dot_keyword(trimmed, "control") {
            in_control = true;
            continue;
        }
        if in_control {
            if starts_with_dot_keyword(trimmed, "endc") {
                in_control = false;
            }
            continue;
        }

        // `.if … .endif` blocks: skip with one warning per top-level `.if`.
        // `.elseif`/`.else` neither open nor close — they live inside the
        // skipped block.
        if starts_with_dot_keyword(trimmed, "if") {
            if conditional_depth == 0 {
                diagnostics.push(Diagnostic::warning(
                    "W911",
                    "conditional blocks are ignored",
                    Label::new(phys.span, ".if"),
                ));
            }
            conditional_depth += 1;
            continue;
        }
        if conditional_depth > 0 {
            if starts_with_dot_keyword(trimmed, "endif") {
                conditional_depth -= 1;
            }
            continue;
        }

        // Pure blank.
        if trimmed.is_empty() {
            continue;
        }

        // Continuation: append to last logical line.
        if let Some(rest) = trimmed.strip_prefix('+')
            && let Some(last) = out.last_mut()
            && last.kind == LineKind::Code
        {
            let rest_offset = leading_offset + 1;
            let (code, code_span, tags) = split_code_and_tags(rest, rest_offset, file);
            tokenise_into(code, code_span.start, file, &mut last.words);
            last.tags.extend(tags);
            last.span = Span::merge(last.span, phys.span).unwrap_or(last.span);
            continue;
        }

        // `$` at start of a line is a whole-line comment in ngspice
        // (`inpcom.c` `inp_stripcomments_line`). The qualifying-prev-char
        // rule only governs *inline* `$` — at column zero it is always a
        // comment introducer regardless of what follows.
        if trimmed.starts_with('$') {
            continue;
        }

        // Comment line, possibly an annotation.
        if let Some(after_star) = trimmed.strip_prefix('*') {
            if let Some(body) = after_star.strip_prefix('@') {
                // `*@…` — block annotation. Body span starts after the `*@`.
                let body_offset = leading_offset + 2;
                let mut words = Vec::new();
                tokenise_into(body, body_offset, file, &mut words);
                out.push(LogicalLine {
                    kind: LineKind::BlockAnnotation,
                    words,
                    tags: Vec::new(),
                    span: phys.span,
                });
            }
            // Otherwise: pure comment, drop.
            continue;
        }

        // Standalone trailing-tag line: `;@…` with nothing before it.
        // Treat it as belonging to the previous logical line, per spec §2.2.
        if trimmed.starts_with(';') {
            if let Some(last) = out.last_mut()
                && last.kind == LineKind::Code
            {
                let (_, _, tags) = split_code_and_tags(trimmed, leading_offset, file);
                last.tags.extend(tags);
                last.span = Span::merge(last.span, phys.span).unwrap_or(last.span);
            }
            continue;
        }

        // Code line.
        let (code, code_span, tags) = split_code_and_tags(trimmed, leading_offset, file);
        let mut words = Vec::new();
        tokenise_into(code, code_span.start, file, &mut words);
        out.push(LogicalLine {
            kind: LineKind::Code,
            words,
            tags,
            span: phys.span,
        });
    }

    Scanned {
        title,
        title_span,
        lines: out,
        diagnostics,
    }
}

/// Does `trimmed` begin with the SPICE directive `.<kw>` (case-insensitive,
/// followed by whitespace or end of line)?
fn starts_with_dot_keyword(trimmed: &str, kw: &str) -> bool {
    if !trimmed.starts_with('.') {
        return false;
    }
    let after = &trimmed[1..];
    if after.len() < kw.len() {
        return false;
    }
    if !after[..kw.len()].eq_ignore_ascii_case(kw) {
        return false;
    }
    match after.as_bytes().get(kw.len()) {
        None => true,
        Some(b) => b.is_ascii_whitespace(),
    }
}

#[derive(Debug, Clone)]
struct PhysicalLine<'a> {
    text: &'a str,
    span: Span,
}

fn split_physical_lines(source: &str, file: FileId) -> Vec<PhysicalLine<'_>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (idx, ch) in source.char_indices() {
        if ch == '\n' {
            let end = idx;
            // Trim a trailing CR (CRLF input).
            let line_end = if end > start && source.as_bytes()[end - 1] == b'\r' {
                end - 1
            } else {
                end
            };
            out.push(PhysicalLine {
                text: &source[start..line_end],
                span: Span::new(file, start, line_end),
            });
            start = idx + 1;
        }
    }
    if start < source.len() {
        out.push(PhysicalLine {
            text: &source[start..],
            span: Span::new(file, start, source.len()),
        });
    }
    out
}

/// Return the byte index of the first `$` in `text` that qualifies as a
/// comment introducer under ngspice's rule: the character immediately before
/// the `$` must be an ASCII space, tab, or comma. `$` at position 0 is NOT
/// a comment introducer (matches `inp_stripcomments_line` in ngspice's
/// `inpcom.c`, which checks `d[-2] >= s` before accepting the preceding char).
fn first_dollar_comment(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    for i in 1..bytes.len() {
        if bytes[i] == b'$' {
            let prev = bytes[i - 1];
            if prev == b' ' || prev == b'\t' || prev == b',' {
                return Some(i);
            }
        }
    }
    None
}

/// Split `text` (which starts at byte offset `offset` in the source) into
/// the leading code part and any `;@` tags. Returns `(code, code_span, tags)`
/// where `code` is the substring before the first `;` or qualifying `$`
/// (ngspice rule: `$` preceded by space, tab, or comma) and `code_span` is
/// the span of that substring in the source.
///
/// A qualifying `$` is treated as plain prose — no `$@` annotation form
/// exists. If `;` appears before any qualifying `$`, the existing `;@` tag
/// harvesting logic applies as before.
fn split_code_and_tags(text: &str, offset: usize, file: FileId) -> (&str, Span, Vec<RawTag>) {
    let semi = text.find(';');
    let dollar = first_dollar_comment(text);

    // Pick whichever comment introducer appears first.
    let dollar_wins = match (semi, dollar) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(s), Some(d)) => d < s,
    };
    if dollar_wins {
        let d = dollar.expect("dollar_wins implies dollar is Some");
        return (&text[..d], Span::new(file, offset, offset + d), Vec::new());
    }
    let Some(cut) = semi else {
        return (
            text,
            Span::new(file, offset, offset + text.len()),
            Vec::new(),
        );
    };

    let semi = cut;
    let code = &text[..semi];
    let code_span = Span::new(file, offset, offset + semi);

    // After the first `;`, the remainder of the line may contain one or
    // more `;@…` tags (each terminated by the next `;` or EOL). Plain
    // text between markers is prose comment; we only capture annotation
    // segments.
    let tail = &text[semi..]; // includes the leading `;`
    let mut tags = Vec::new();
    let bytes = tail.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b';' {
            i += 1;
            continue;
        }
        // Found a `;`. Look at what follows (allowing whitespace before `@`).
        let mut j = i + 1;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'@' {
            // `;@…` — capture body until next `;`.
            let body_start_in_tail = j + 1;
            let mut k = body_start_in_tail;
            while k < bytes.len() && bytes[k] != b';' {
                k += 1;
            }
            let body = &tail[body_start_in_tail..k];
            tags.push(RawTag {
                body: body.to_owned(),
                outer_span: Span::new(file, offset + semi + i, offset + semi + k),
                body_span: Span::new(file, offset + semi + body_start_in_tail, offset + semi + k),
            });
            i = k;
        } else {
            // Plain `;` prose comment — runs until next `;` or EOL.
            let mut k = i + 1;
            while k < bytes.len() && bytes[k] != b';' {
                k += 1;
            }
            i = k;
        }
    }

    (code, code_span, tags)
}

/// Split `text` (located at byte `offset` in the source) into whitespace,
/// `=`, `(`, `)`-separated words and append them to `out`.
fn tokenise_into(text: &str, offset: usize, file: FileId, out: &mut Vec<Word>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b == b'=' || b == b'(' || b == b')' {
            out.push(Word {
                text: (b as char).to_string(),
                span: Span::new(file, offset + i, offset + i + 1),
            });
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'('
            && bytes[i] != b')'
        {
            i += 1;
        }
        out.push(Word {
            text: text[start..i].to_owned(),
            span: Span::new(file, offset + start, offset + i),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fid() -> FileId {
        FileId(0)
    }

    fn words(line: &LogicalLine) -> Vec<&str> {
        line.words.iter().map(|w| w.text.as_str()).collect()
    }

    #[test]
    fn title_is_first_physical_line() {
        let s = "* hello world\nR1 a b 1k\n";
        let out = scan(s, fid());
        assert_eq!(out.title, "hello world");
        assert_eq!(out.lines.len(), 1);
        assert_eq!(words(&out.lines[0]), ["R1", "a", "b", "1k"]);
    }

    #[test]
    fn continuation_appends_words_and_tags() {
        let s = "* t\nM1 d g s b NMOS L=1u  ;@ symbol=Device:Q_NMOS\n+ W=10u\n";
        let out = scan(s, fid());
        assert_eq!(out.lines.len(), 1);
        let l = &out.lines[0];
        assert_eq!(
            words(l),
            [
                "M1", "d", "g", "s", "b", "NMOS", "L", "=", "1u", "W", "=", "10u"
            ]
        );
        assert_eq!(l.tags.len(), 1);
        assert!(l.tags[0].body.contains("symbol=Device:Q_NMOS"));
    }

    #[test]
    fn block_annotation_emits_block_line() {
        let s = "* t\n*@symbol Device:R_US for=R*\nR1 a b 1\n";
        let out = scan(s, fid());
        assert_eq!(out.lines.len(), 2);
        assert_eq!(out.lines[0].kind, LineKind::BlockAnnotation);
        assert_eq!(
            words(&out.lines[0]),
            ["symbol", "Device:R_US", "for", "=", "R*"]
        );
        assert_eq!(out.lines[1].kind, LineKind::Code);
    }

    #[test]
    fn pure_comment_dropped() {
        let s = "* t\n* this is a comment\nR1 a b 1\n";
        let out = scan(s, fid());
        assert_eq!(out.lines.len(), 1);
        assert_eq!(words(&out.lines[0])[0], "R1");
    }

    #[test]
    fn control_block_skipped() {
        let s = "* t\n.control\nplot v(out)\n.endc\nR1 a b 1\n";
        let out = scan(s, fid());
        assert_eq!(out.lines.len(), 1);
        assert_eq!(words(&out.lines[0])[0], "R1");
    }

    #[test]
    fn multiple_tags_on_one_line() {
        let s = "* t\nR1 a b 1k ;@ symbol=Device:R ;@ place=right-of V1\n";
        let out = scan(s, fid());
        assert_eq!(out.lines[0].tags.len(), 2);
        assert!(out.lines[0].tags[0].body.contains("symbol=Device:R"));
        assert!(out.lines[0].tags[1].body.contains("place=right-of V1"));
    }

    #[test]
    fn standalone_tag_line_attaches_to_previous() {
        let s = "* t\nR1 a b 1k\n  ;@ place=right-of V1\n";
        let out = scan(s, fid());
        assert_eq!(out.lines.len(), 1);
        assert_eq!(out.lines[0].tags.len(), 1);
    }

    #[test]
    fn equals_and_parens_are_separators() {
        let s = "* t\n.model M NPN (BF=200 IS=1e-15)\n";
        let out = scan(s, fid());
        assert_eq!(
            words(&out.lines[0]),
            [
                ".model", "M", "NPN", "(", "BF", "=", "200", "IS", "=", "1e-15", ")"
            ]
        );
    }
}
