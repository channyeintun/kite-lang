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
//!
//! A slice or map in a struct field is written through it by being taken
//! into a local, changed, and put back (`q.items.push(v)`; §5.4), and the
//! take would always copy if the local's flag started clear, which made a
//! loop of pushes through a field quadratic. A field has nowhere to keep a
//! flag, so it is kept on the object, as `kite-rt`'s `HELD_BIT`: the
//! write-back sets it when the local owned what it put back, and the next
//! take reads it as the local's flag. What makes that sound is that every
//! other read of the field clears it before the reference can go anywhere —
//! every read but a glance, which keeps nothing and is over before anything
//! could write in place (`kite_mir::held_reads`). So the bit is set only
//! while the field is the one holder, and a write that finds it set has
//! nothing else to be seen through.
//!
//! A map is a value in exactly the same way, and has room to spare and a
//! hash index for the same reason (see `kite-rt`'s maps), so `m[k] = v` and
//! `m.remove(k)` keep the same flag by the same rule. The VM's map shares
//! itself through `Rc` as its slice does, and `Rc::make_mut` decides there.

use kite_mir as mir;

/// The slice locals `f` writes into or pushes onto, and the map locals it
/// writes into or removes from: the ones that need an owned flag. Every other
/// local only ever reads, so whether it shares its value is nobody's
/// business.
pub fn written(f: &mir::Function) -> Vec<mir::Local> {
    let mut out: Vec<mir::Local> = Vec::new();
    for b in &f.blocks {
        for s in &b.stmts {
            let target = match s {
                mir::Inst::SlicePush { local, .. }
                | mir::Inst::MapSet { local, .. }
                | mir::Inst::MapRemove { local, .. } => Some(*local),
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
/// of the slice or map it produced: a literal, a range (which copies), or a
/// map's keys or values, each made on the spot. Anything read out of somewhere
/// else — a local, a call's answer, a field, an element — may be shared with
/// where it came from, so the first write to it copies.
pub fn fresh(value: &mir::Rvalue) -> bool {
    matches!(
        value,
        mir::Rvalue::SliceNew { .. }
            | mir::Rvalue::MapNew { .. }
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
///
/// A direct call keeps only the arguments its callee may keep, and a builtin
/// only when it is one that keeps ([`mir::Kept`], [`mir::builtin_keeps`]): a
/// callee that only reads its parameter hands the caller back the one holder
/// it had, so `if !seen(m, w) { m[w] = 1 }` does not copy `m` on every write.
pub fn escaping_operands<'a>(stmt: &'a mir::Inst, kept: &mir::Kept) -> Vec<&'a mir::Operand> {
    use mir::Rvalue as R;
    let value = match stmt {
        mir::Inst::Assign { value, .. } => value,
        mir::Inst::SetField { value, .. } => return vec![value],
        // The base is the slice or map being written, which is not an escape.
        mir::Inst::SetIndex { value, .. } => return vec![value],
        mir::Inst::SlicePush { value, .. } => return vec![value],
        mir::Inst::MapSet { key, value, .. } => return vec![key, value],
        // A removal compares its key and keeps nothing.
        mir::Inst::MapRemove { .. } => return Vec::new(),
    };
    match value {
        R::Use(o) | R::Wrap { value: o } | R::Unwrap { value: o } => vec![o],
        R::Call { callee, args } => kept.kept_args(*callee, args),
        R::CallBuiltin { builtin, args } => {
            if mir::builtin_keeps(*builtin) {
                args.iter().collect()
            } else {
                Vec::new()
            }
        }
        R::CallVirtual { args, .. } | R::CallExtern { args, .. } | R::StrOp { args, .. } => {
            args.iter().collect()
        }
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
        // What a take hands the local is put back by the write-back, which is
        // the `SetField` that comes next; the take itself keeps nothing.
        | R::FieldTake { .. }
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
        | R::Yield
        // A flag, and a frame's field read back: neither keeps the frame,
        // and the flag is a bool.
        | R::Owned { .. }
        | R::Unspill { .. } => Vec::new(),
    }
}
