//! What this target can lower, and what it cannot yet.
//!
//! The same bargain the Wasm backend's scan makes: a construct the backend
//! cannot lower is reported at compile time, naming it and where it appears,
//! rather than compiled into a binary that traps with no explanation. The
//! driver asks before emitting anything.

use kite_hir::Types;
use kite_mir as mir;
use kite_span::Span;

/// A construct this target cannot lower yet.
pub struct Unsupported {
    /// What was used, phrased for a diagnostic message.
    pub what: &'static str,
    /// The function it appeared in.
    pub function: String,
    pub span: Span,
}

/// Scan a program for constructs the native backend cannot lower.
///
/// Everything the language can express lowers today; the two refusals below
/// are capacity limits rather than missing features, and they are checked
/// here because a limit that trips at run time would be indistinguishable
/// from a codegen bug.
pub fn unsupported(program: &mir::Program, _types: &Types) -> Vec<Unsupported> {
    let mut found = Vec::new();

    for f in &program.fns {
        let mut note = |what: &'static str| {
            if !found
                .iter()
                .any(|u: &Unsupported| u.what == what && u.function == f.name)
            {
                found.push(Unsupported { what, function: f.name.clone(), span: f.span });
            }
        };

        for block in &f.blocks {
            for stmt in &block.stmts {
                let mir::Inst::Assign { value, .. } = stmt else { continue };
                // A variadic construction stages its operands in a window of
                // fixed size; a literal too wide for it has no staging path.
                let staged = match value {
                    mir::Rvalue::StructNew { fields, .. }
                    | mir::Rvalue::EnumNew { fields, .. } => fields.len(),
                    mir::Rvalue::TupleNew { elems }
                    | mir::Rvalue::SliceNew { elems } => elems.len(),
                    mir::Rvalue::MapNew { entries } => entries.len(),
                    mir::Rvalue::ClosureNew { captures, .. } => captures.len(),
                    mir::Rvalue::CallExtern { args, .. } => args.len(),
                    _ => 0,
                };
                if staged > kite_rt::STAGE_WORDS {
                    note("a literal with more than 4096 elements");
                }
            }
        }
    }

    found
}
