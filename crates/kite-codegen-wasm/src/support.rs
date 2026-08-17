//! What this target can lower, and what it cannot yet.
//!
//! The backend emits `unreachable` for constructs it does not handle. That is
//! the right thing for a *provably* dead path, but it would be dishonest for a
//! construct that is simply not implemented: the module would validate, ship,
//! and then trap at run time with no explanation.
//!
//! So the driver asks first. A program using something this target cannot lower
//! is reported at compile time, naming the construct and where it appears.

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

/// Scan a program for constructs the WebAssembly backend cannot lower.
///
/// Returns them in source order so the first report is the most useful one.
pub fn unsupported(program: &mir::Program, types: &Types) -> Vec<Unsupported> {
    let mut found = Vec::new();

    for f in &program.fns {
        let mut note = |what: &'static str| {
            if !found.iter().any(|u: &Unsupported| {
                u.what == what && u.function == f.name
            }) {
                found.push(Unsupported {
                    what,
                    function: f.name.clone(),
                    span: f.span,
                });
            }
        };

        // A signature mentioning an unlowered type is enough on its own.
        for l in &f.locals {
            if let Some(what) = unsupported_type(l.ty, types) {
                note(what);
            }
        }

        for block in &f.blocks {
            for stmt in &block.stmts {
                match stmt {
                    mir::Inst::Assign { .. } => {}
                    mir::Inst::SetField { .. }
                    | mir::Inst::SetIndex { .. }
                    | mir::Inst::SlicePush { .. }
                    | mir::Inst::MapSet { .. }
                    | mir::Inst::MapRemove { .. } => {}
                }
            }
        }
    }

    found
}

/// The construct name for a type this target cannot represent.
///
/// Nothing the language can express is refused today. The check stays because
/// the next construct added will need it, and because a backend that silently
/// emits a trapping module for something it cannot do is the failure mode this
/// whole module exists to prevent.
fn unsupported_type(_ty: kite_hir::TyId, _types: &Types) -> Option<&'static str> {
    None
}
