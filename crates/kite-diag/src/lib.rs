//! Diagnostics.
//!
//! Error message quality is a language design constraint in Kite, not a
//! post-implementation concern. Every diagnostic carries a stable code, a
//! primary span, secondary spans explaining *why*, and where possible a
//! machine-applicable fix.
//!
//! The renderer is hand-written rather than delegated to a crate so the output
//! format is pinned exactly by the specification and by snapshot tests.

use kite_span::{SourceMap, Span};
use std::fmt::Write as _;

pub mod codes;
mod render;

pub use codes::Code;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        }
    }
}

/// Whether a label is the thing that is wrong, or context explaining why.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LabelStyle {
    /// Rendered with `^`. The location of the problem.
    Primary,
    /// Rendered with `-`. The declaration or constraint that caused it.
    Secondary,
}

#[derive(Clone, Debug)]
pub struct Label {
    pub span: Span,
    pub style: LabelStyle,
    pub message: String,
}

impl Label {
    pub fn primary(span: Span, message: impl Into<String>) -> Self {
        Label { span, style: LabelStyle::Primary, message: message.into() }
    }

    pub fn secondary(span: Span, message: impl Into<String>) -> Self {
        Label { span, style: LabelStyle::Secondary, message: message.into() }
    }
}

/// A machine-applicable edit. `kite fix` applies these.
#[derive(Clone, Debug)]
pub struct Fix {
    pub message: String,
    pub edits: Vec<Edit>,
}

#[derive(Clone, Debug)]
pub struct Edit {
    pub span: Span,
    pub replacement: String,
}

impl Fix {
    pub fn replace(message: impl Into<String>, span: Span, replacement: impl Into<String>) -> Self {
        Fix {
            message: message.into(),
            edits: vec![Edit { span, replacement: replacement.into() }],
        }
    }
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Option<Code>,
    /// One line, lowercase, no trailing period.
    pub message: String,
    pub labels: Vec<Label>,
    pub notes: Vec<String>,
    pub fixes: Vec<Fix>,
}

impl Diagnostic {
    pub fn error(code: Code, message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Error,
            code: Some(code),
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
            fixes: Vec::new(),
        }
    }

    pub fn warning(code: Code, message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Warning,
            code: Some(code),
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
            fixes: Vec::new(),
        }
    }

    pub fn with_primary(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label::primary(span, message));
        self
    }

    pub fn with_secondary(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label::secondary(span, message));
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_fix(mut self, fix: Fix) -> Self {
        self.fixes.push(fix);
        self
    }

    pub fn primary_span(&self) -> Option<Span> {
        self.labels
            .iter()
            .find(|l| l.style == LabelStyle::Primary)
            .map(|l| l.span)
    }

    pub fn render(&self, sources: &SourceMap) -> String {
        render::render(self, sources)
    }
}

/// Collects diagnostics across compiler passes.
///
/// Passes push into the bag and keep going where they can, so one run reports
/// as much as it soundly may. A pass that would cascade stops instead.
#[derive(Default)]
pub struct DiagBag {
    diagnostics: Vec<Diagnostic>,
}

impl DiagBag {
    pub fn new() -> Self {
        DiagBag::default()
    }

    pub fn push(&mut self, d: Diagnostic) {
        self.diagnostics.push(d);
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.severity == Severity::Error)
    }

    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }

    pub fn len(&self) -> usize {
        self.diagnostics.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter()
    }

    pub fn take(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    /// Order by source position so output is deterministic regardless of the
    /// order passes happened to discover problems.
    pub fn sort(&mut self, _sources: &SourceMap) {
        self.diagnostics.sort_by_key(|d| {
            d.primary_span()
                .map(|s| (s.file.0, s.start, s.end))
                .unwrap_or((u32::MAX, u32::MAX, u32::MAX))
        });
    }

    pub fn render_all(&self, sources: &SourceMap) -> String {
        let mut out = String::new();
        for (i, d) in self.diagnostics.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&d.render(sources));
        }
        let errors = self.error_count();
        if errors > 0 {
            let _ = write!(
                out,
                "\nerror: could not compile due to {} previous error{}\n",
                errors,
                if errors == 1 { "" } else { "s" }
            );
        }
        out
    }
}
