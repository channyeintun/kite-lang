//! Which slice writes may go straight into the slice.
//!
//! A Kite slice is a *value* — `let a = xs; xs.push(1)` leaves `a` as it was —
//! and the obvious way to get that natively is the one this backend started
//! with: every `push` and every `xs[i] = v` copied the whole slice. Correct,
//! and quadratic: a hundred thousand pushes took four seconds where the
//! bytecode VM took sixteen milliseconds.
//!
//! So a slice object has room to spare (see `kite-rt`'s `slice_len`), and a
//! write goes straight into it when nothing else can see it. What decides
//! "nothing else can see it" is a flag per mutated slice local: set when the
//! function made the slice itself, cleared the moment the reference is copied
//! anywhere. The VM decides the same thing with `Rc::make_mut`'s reference
//! count; this heap has no count to ask, but it has the compiler, which can
//! see every place a local is read. The Wasm backend keeps the same flag for
//! the same reason (its `slices` module), and the rule here is the same rule.
//!
//! The flag is a run-time value rather than a static fact because a loop is
//! exactly where the difference matters: the first `push` onto a slice a
//! caller handed in copies, and the next hundred thousand go straight in.

use kite_mir as mir;

/// The slice locals `f` writes into or pushes onto: the ones that need an
/// owned flag. Every other local only ever reads, so whether it shares its
/// slice is nobody's business.
pub fn written(f: &mir::Function) -> Vec<mir::Local> {
    let mut out: Vec<mir::Local> = Vec::new();
    for b in &f.blocks {
        for s in &b.stmts {
            let target = match s {
                mir::Inst::SlicePush { local, .. } => Some(*local),
                mir::Inst::SetIndex {
                    base: mir::Operand::Local(l),
                    ..
                } => Some(*l),
                _ => None,
            };
            if let Some(l) = target {
                if !out.contains(&l) {
                    out.push(l);
                }
            }
        }
    }
    out
}

/// Whether an assignment of `value` leaves its destination the only holder
/// of the slice it produced: a literal, a range (which copies), or a map's
/// keys or values, each made on the spot. Anything read out of somewhere
/// else — a local, a call's answer, a field, an element — may be shared with
/// where it came from, so the first write to it copies.
pub fn fresh(value: &mir::Rvalue) -> bool {
    matches!(
        value,
        mir::Rvalue::SliceNew { .. }
            | mir::Rvalue::SliceRange { .. }
            | mir::Rvalue::MapKeys { .. }
            | mir::Rvalue::MapValues { .. }
    )
}

/// The operands of an instruction through which a slice's reference may be
/// kept: stored, passed, captured, boxed, moved or returned. Each such read
/// of a flagged local clears its flag before the instruction runs.
///
/// Deliberately exhaustive, with no catch-all: an operand position this does
/// not know about would be a way for a shared slice to be written in place,
/// so a new MIR construct fails to compile here until someone decides.
///
/// One difference from the Wasm backend's list: `Unwrap` counts. An unwrap
/// whose operand is not an optional is a move on this backend — the same
/// pointer under a second name — and a move is an escape.
pub fn escaping_operands(stmt: &mir::Inst) -> Vec<&mir::Operand> {
    use mir::Rvalue as R;
    let value = match stmt {
        mir::Inst::Assign { value, .. } => value,
        mir::Inst::SetField { value, .. } => return vec![value],
        // The base is the slice being written, which is not an escape.
        mir::Inst::SetIndex { value, .. } => return vec![value],
        mir::Inst::SlicePush { value, .. } => return vec![value],
        mir::Inst::MapSet { key, value, .. } => return vec![key, value],
        // A removal compares its key and keeps nothing.
        mir::Inst::MapRemove { .. } => return Vec::new(),
    };
    match value {
        R::Use(o) | R::Wrap { value: o } | R::Unwrap { value: o } => vec![o],
        R::Call { args, .. }
        | R::CallVirtual { args, .. }
        | R::CallBuiltin { args, .. }
        | R::CallExtern { args, .. }
        | R::StrOp { args, .. } => args.iter().collect(),
        R::ClosureNew { captures, .. } => captures.iter().collect(),
        R::CallClosure { callee, args } => std::iter::once(callee).chain(args).collect(),
        R::StructNew { fields, .. } | R::EnumNew { fields, .. } => fields.iter().collect(),
        R::TupleNew { elems } | R::SliceNew { elems } => elems.iter().collect(),
        R::MapNew { entries } => entries.iter().collect(),
        R::PairNew { value, error } => vec![value, error],
        R::ErrorNew {
            message,
            value,
            tag,
            cause,
        } => vec![message, value, tag, cause],
        // Reads that keep nothing: a comparison, a rendering, a length, an
        // element, a window (which copies), a test, a lookup by key.
        R::Binary { .. }
        | R::Unary { .. }
        | R::ToStr { .. }
        | R::Cast { .. }
        | R::FieldGet { .. }
        | R::TagOf { .. }
        | R::VariantGet { .. }
        | R::MapGet { .. }
        | R::MapLen { .. }
        | R::MapKeys { .. }
        | R::MapValues { .. }
        | R::IsNil { .. }
        | R::PairValue { .. }
        | R::PairError { .. }
        | R::ErrorMessage { .. }
        | R::ErrorCause { .. }
        | R::ErrorTag { .. }
        | R::ErrorAs { .. }
        | R::IndexGet { .. }
        | R::SliceLen { .. }
        | R::SliceGet { .. }
        | R::SliceRange { .. }
        | R::Await { .. }
        | R::Yield => Vec::new(),
    }
}
