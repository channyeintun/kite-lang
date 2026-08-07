//! Source positions and file interning.
//!
//! Every IR node in the Kite compiler carries a [`Span`]. Diagnostics are a
//! product surface, not a phase, so span fidelity is maintained end-to-end
//! rather than reconstructed.

use std::fmt;
use std::path::{Path, PathBuf};

/// Handle to a file in a [`SourceMap`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FileId(pub u32);

/// A half-open byte range `[start, end)` within a single file.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(file: FileId, start: u32, end: u32) -> Self {
        debug_assert!(start <= end, "span start must not exceed end");
        Span { file, start, end }
    }

    /// A zero-width span at `offset`, used for "expected a token here" errors.
    pub fn empty_at(file: FileId, offset: u32) -> Self {
        Span { file, start: offset, end: offset }
    }

    /// The smallest span covering both operands. Panics across files, which
    /// would indicate an IR construction bug.
    pub fn to(self, other: Span) -> Span {
        assert_eq!(self.file, other.file, "cannot join spans across files");
        Span {
            file: self.file,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn len(self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}..{}", self.file.0, self.start, self.end)
    }
}

/// A 1-indexed line and column, for display. Column counts characters, not
/// bytes, so multi-byte identifiers point at the right place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LineCol {
    pub line: u32,
    pub col: u32,
}

pub struct SourceFile {
    pub name: PathBuf,
    pub text: String,
    /// Byte offset of the start of each line.
    line_starts: Vec<u32>,
}

impl SourceFile {
    fn new(name: PathBuf, text: String) -> Self {
        let mut line_starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        SourceFile { name, text, line_starts }
    }

    /// The line index (0-based) containing `offset`.
    fn line_index(&self, offset: u32) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }

    pub fn line_col(&self, offset: u32) -> LineCol {
        let li = self.line_index(offset);
        let line_start = self.line_starts[li] as usize;
        let col = self.text[line_start..offset as usize].chars().count() as u32 + 1;
        LineCol { line: li as u32 + 1, col }
    }

    /// Text of a 1-indexed line, without its terminator.
    pub fn line_text(&self, line: u32) -> &str {
        let li = (line - 1) as usize;
        if li >= self.line_starts.len() {
            return "";
        }
        let start = self.line_starts[li] as usize;
        let end = self
            .line_starts
            .get(li + 1)
            .map(|&e| e as usize)
            .unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }

    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    /// Byte offset of the start of a 1-indexed line.
    pub fn line_start(&self, line: u32) -> u32 {
        self.line_starts
            .get((line - 1) as usize)
            .copied()
            .unwrap_or(self.text.len() as u32)
    }
}

#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> Self {
        SourceMap::default()
    }

    pub fn add(&mut self, name: impl AsRef<Path>, text: impl Into<String>) -> FileId {
        let id = FileId(self.files.len() as u32);
        self.files
            .push(SourceFile::new(name.as_ref().to_path_buf(), text.into()));
        id
    }

    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    /// Every file, with its display name. The driver uses this to tell the
    /// program's own source from the standard library's.
    pub fn iter(&self) -> impl Iterator<Item = (FileId, String)> + '_ {
        self.files
            .iter()
            .enumerate()
            .map(|(i, f)| (FileId(i as u32), f.name.display().to_string()))
    }

    pub fn text(&self, id: FileId) -> &str {
        &self.file(id).text
    }

    /// The source text a span covers.
    pub fn snippet(&self, span: Span) -> &str {
        clamped(&self.file(span.file).text, span.start, span.end)
    }

    /// The source text a span covers, from whichever file it belongs to.
    pub fn span_text(&self, span: Span) -> &str {
        clamped(self.text(span.file), span.start, span.end)
    }

    /// The text of a file up to an offset. Used where a diagnostic needs to
    /// look backwards from a span, such as for the `pub` before a name.
    pub fn text_before(&self, span: Span) -> &str {
        clamped(self.text(span.file), 0, span.start)
    }

    pub fn line_col(&self, span: Span) -> LineCol {
        self.file(span.file).line_col(span.start)
    }
}

/// The `lo..hi` slice of `text`, clamped until it is one.
///
/// A `Span` is a pair of `u32`s that some earlier pass computed, and slicing a
/// `str` panics two different ways when they are wrong: out of range, and — the
/// one that actually happened — an index that falls inside a multi-byte
/// character. These accessors are called from the type checker and the
/// diagnostic renderer, which run inside `kite-lsp`, where a panic is not a
/// failed request but the end of the session; and inside the playground's wasm
/// module, where it traps the instance.
///
/// So a bad span costs a wrong-looking snippet rather than a dead process.
/// Producing one is still a bug where it is produced.
fn clamped(text: &str, lo: u32, hi: u32) -> &str {
    let mut lo = (lo as usize).min(text.len());
    let mut hi = (hi as usize).min(text.len());
    if lo > hi {
        return "";
    }
    while lo > 0 && !text.is_char_boundary(lo) {
        lo -= 1;
    }
    while hi > lo && !text.is_char_boundary(hi) {
        hi -= 1;
    }
    &text[lo..hi]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> (SourceMap, FileId) {
        let mut m = SourceMap::new();
        let f = m.add("t.kite", "fn main() {\n    let x = 1\n}\n");
        (m, f)
    }

    #[test]
    fn line_col_is_one_indexed() {
        let (m, f) = map();
        assert_eq!(m.file(f).line_col(0), LineCol { line: 1, col: 1 });
        assert_eq!(m.file(f).line_col(12), LineCol { line: 2, col: 1 });
        assert_eq!(m.file(f).line_col(16), LineCol { line: 2, col: 5 });
    }

    #[test]
    fn line_text_strips_terminator() {
        let (m, f) = map();
        assert_eq!(m.file(f).line_text(1), "fn main() {");
        assert_eq!(m.file(f).line_text(2), "    let x = 1");
        assert_eq!(m.file(f).line_text(3), "}");
    }

    #[test]
    fn column_counts_characters_not_bytes() {
        let mut m = SourceMap::new();
        // "héllo" — é occupies bytes 1..3, so the 'o' starts at byte 5 and is
        // the 5th character. A byte-counting implementation would say 6.
        let f = m.add("t.kite", "héllo");
        assert_eq!(m.file(f).line_col(5), LineCol { line: 1, col: 5 });
        assert_eq!(m.file(f).line_col(3), LineCol { line: 1, col: 3 });
    }

    #[test]
    fn span_join_covers_both() {
        let f = FileId(0);
        let a = Span::new(f, 2, 5);
        let b = Span::new(f, 9, 11);
        assert_eq!(a.to(b), Span::new(f, 2, 11));
        assert_eq!(b.to(a), Span::new(f, 2, 11));
    }

    #[test]
    fn snippet_returns_covered_text() {
        let (m, f) = map();
        assert_eq!(m.snippet(Span::new(f, 3, 7)), "main");
    }
}
