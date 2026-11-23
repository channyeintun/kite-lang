//! Which parameters a function may keep.
//!
//! On a target with no reference count, a slice or map local may be written
//! in place while a flag says it is its value's one holder (see the native
//! backend's `slices`), and any read that could keep the reference clears
//! the flag. Passing the local to a function was such a read, always — so a
//! loop that asked a helper about its own map and then wrote it
//! (`if !seen(m, w) { m[w] = 1 }`) copied the whole map on every write.
//!
//! But a callee that only reads its parameter hands nothing back that could
//! see a later write: when it returns, the caller's local is the one holder
//! again, exactly as it was. This works out, per function and parameter,
//! whether that is proved. A parameter is *kept* when the function stores it
//! anywhere — a field, an element, a map, a tuple, a struct or enum value, an
//! optional, a pair, an error — captures it, returns it, renders it through a
//! string operation, hands it to a function value, a `dyn` method, the host,
//! a builtin that keeps (see [`builtin_keeps`]) or a function that keeps that
//! parameter, or does any of these to a local it was moved into. An `async`
//! function keeps every parameter: after the state-machine transform its
//! starter stores them in the task's frame, which is a store like any other,
//! so this falls out without a rule of its own.
//!
//! Anything this does not know, it counts as keeping, and every form is named
//! with no catch-all, so a new MIR form has to be decided.
//!
//! The answer is the least fixed point of "keeps", computed from "keeps
//! nothing" upward: a parameter is marked when one of its own uses keeps it,
//! or when it is passed to a parameter already marked, until nothing changes.
//! That is the sound end to start from, recursion included. A run of the
//! program that keeps a parameter does it through a finite chain of calls
//! ending in one instruction that keeps — and by induction on that chain's
//! length, from the instruction back up, every parameter along it is marked.
//! A parameter that is only ever passed round a cycle of calls and never
//! kept by any of them stays unmarked, which is the truth about it.
//!
//! A write into a parameter is not a keep: a parameter starts unowned on
//! every backend, so the first write copies, and what it writes into is the
//! callee's own copy.

use crate::*;
use kite_hir::Builtin;

/// By function, by parameter: whether the function may keep what it is given
/// there. See the module documentation.
#[derive(Debug, Default)]
pub struct Kept {
    by_fn: Vec<Vec<bool>>,
    /// By function: whether calling it may write in place a slice or map
    /// that existed before the call, or run code this cannot see. See
    /// [`Kept::quiet`].
    stirs: Vec<bool>,
}

impl Kept {
    /// Work it out for every function of `program`, which must be past the
    /// state-machine transform: what a backend sees is what is judged.
    pub fn of(program: &Program) -> Kept {
        let mut kept = Kept {
            by_fn: program.fns.iter().map(|f| vec![false; f.param_count]).collect(),
            stirs: stirs(program),
        };
        // The carriers of each parameter: the locals its value can be moved
        // into. They do not depend on what other functions keep.
        let carriers: Vec<Vec<Vec<bool>>> = program
            .fns
            .iter()
            .map(|f| (0..f.param_count).map(|p| carriers(f, Local(p as u32))).collect())
            .collect();
        loop {
            let mut changed = false;
            for (fi, f) in program.fns.iter().enumerate() {
                for (p, carrier) in carriers[fi].iter().enumerate() {
                    if kept.by_fn[fi][p] {
                        continue;
                    }
                    if keeps_any(f, carrier, &kept) {
                        kept.by_fn[fi][p] = true;
                        changed = true;
                    }
                }
            }
            if !changed {
                return kept;
            }
        }
    }

    /// Whether `callee` may keep its parameter `param`. A function or a
    /// parameter this has no answer for keeps.
    pub fn keeps(&self, callee: FnId, param: usize) -> bool {
        self.by_fn
            .get(callee.index())
            .and_then(|ps| ps.get(param))
            .copied()
            .unwrap_or(true)
    }

    /// Whether a call to `callee` is *quiet*: whatever it does, it writes in
    /// place no slice or map that was there before it was called, and runs
    /// no code but its own and its callees'. A function that may is one that
    /// takes a field (`FieldTake`, which is how a local comes to own a slice
    /// or map it did not make), resumes a frame, renders a value (which can
    /// reach a `Display` the program wrote), calls a function value, a `dyn`
    /// method, the host or a builtin that runs code (see
    /// [`builtin_runs_code`]), or calls one that may. Every other in-place
    /// write is to a local that owns what it writes, which in such a
    /// function is only ever something the call made.
    ///
    /// A read of a field's slice that only glances may span a quiet call it
    /// passes the slice to, when the callee keeps nothing of it
    /// ([`crate::held_reads`]).
    pub fn quiet(&self, callee: FnId) -> bool {
        !self.stirs.get(callee.index()).copied().unwrap_or(true)
    }

    /// The arguments of a direct call `callee(args)` the callee may keep.
    pub fn kept_args<'a>(&self, callee: FnId, args: &'a [Operand]) -> Vec<&'a Operand> {
        args.iter()
            .enumerate()
            .filter(|(i, _)| self.keeps(callee, *i))
            .map(|(_, a)| a)
            .collect()
    }
}

/// Whether a builtin may keep a slice or map handed to it. One by one, with
/// no catch-all: a new builtin has to be decided.
pub fn builtin_keeps(builtin: Builtin) -> bool {
    match builtin {
        // Hand a closure to the scheduler or to the host, which call it later.
        Builtin::TaskSpawn | Builtin::JsFunc => true,
        // Write what they are given, compare it, or read numbers and text:
        // nothing given to one is reachable once it returns.
        Builtin::IoPrint
        | Builtin::IoError
        | Builtin::IoReadLine
        | Builtin::DrawRect
        | Builtin::DrawRRect
        | Builtin::DrawFont
        | Builtin::DrawDRRect
        | Builtin::DrawAlpha
        | Builtin::DrawText
        | Builtin::DrawField
        | Builtin::DrawImage
        | Builtin::DrawSemantics
        | Builtin::TextWidth
        | Builtin::TextHeight
        | Builtin::TextFromCode
        | Builtin::DrawClip
        | Builtin::DrawUnclip
        | Builtin::TaskWakeAt
        | Builtin::TaskPark
        | Builtin::TaskParkOn
        | Builtin::TaskWaitHost
        | Builtin::TimeNow
        | Builtin::PtrSame
        | Builtin::Require => false,
    }
}

/// Whether a builtin may run code of the program's, or suspend so that
/// other code runs. One by one, with no catch-all.
pub fn builtin_runs_code(builtin: Builtin) -> bool {
    match builtin {
        // Writing a value renders it, which can reach a `Display` the
        // program wrote; a closure handed on can be called; a suspension
        // lets every other task run.
        Builtin::IoPrint
        | Builtin::IoError
        | Builtin::TaskSpawn
        | Builtin::JsFunc
        | Builtin::TaskWakeAt
        | Builtin::TaskPark
        | Builtin::TaskParkOn
        | Builtin::TaskWaitHost => true,
        // Numbers and text in, numbers and text out.
        Builtin::IoReadLine
        | Builtin::DrawRect
        | Builtin::DrawRRect
        | Builtin::DrawFont
        | Builtin::DrawDRRect
        | Builtin::DrawAlpha
        | Builtin::DrawText
        | Builtin::DrawField
        | Builtin::DrawImage
        | Builtin::DrawSemantics
        | Builtin::TextWidth
        | Builtin::TextHeight
        | Builtin::TextFromCode
        | Builtin::DrawClip
        | Builtin::DrawUnclip
        | Builtin::TimeNow
        | Builtin::PtrSame
        | Builtin::Require => false,
    }
}

/// By function, whether calling it may stir: the complement of
/// [`Kept::quiet`]. The least fixed point of "stirs", upward from "quiet",
/// for the reason the module gives for "keeps": a call that stirs does so
/// through a finite chain of calls ending in one statement that does.
fn stirs(program: &Program) -> Vec<bool> {
    let mut stirs = vec![false; program.fns.len()];
    loop {
        let mut changed = false;
        for (fi, f) in program.fns.iter().enumerate() {
            if stirs[fi] {
                continue;
            }
            let found = f.blocks.iter().flat_map(|b| &b.stmts).any(|s| match s {
                Inst::Assign { value, .. } => stirs_at(value, &stirs),
                Inst::SetField { .. }
                | Inst::SetIndex { .. }
                | Inst::SlicePush { .. }
                | Inst::MapSet { .. }
                | Inst::MapRemove { .. } => false,
            });
            if found {
                stirs[fi] = true;
                changed = true;
            }
        }
        if !changed {
            return stirs;
        }
    }
}

/// Whether evaluating `value` may stir, given what is known of each function.
/// Exhaustive, with no catch-all.
fn stirs_at(value: &Rvalue, stirs: &[bool]) -> bool {
    use Rvalue as R;
    match value {
        R::Call { callee, .. } => stirs.get(callee.index()).copied().unwrap_or(true),
        R::CallBuiltin { builtin, .. } => builtin_runs_code(*builtin),
        // A rendering can reach a `Display` the program wrote.
        R::ToStr { from, .. } => !crate::renders_itself(*from),
        R::CallVirtual { .. }
        | R::CallClosure { .. }
        | R::CallExtern { .. }
        | R::FieldTake { .. }
        | R::Await { .. }
        | R::Yield
        | R::Owned { .. }
        | R::Unspill { .. } => true,
        // A string operation is the runtime's, on text.
        R::StrOp { .. }
        | R::Use(_)
        | R::Binary { .. }
        | R::Unary { .. }
        | R::ClosureNew { .. }
        | R::Cast { .. }
        | R::StructNew { .. }
        | R::FieldGet { .. }
        | R::EnumNew { .. }
        | R::TagOf { .. }
        | R::VariantGet { .. }
        | R::TupleNew { .. }
        | R::MapNew { .. }
        | R::MapGet { .. }
        | R::MapLen { .. }
        | R::MapKeys { .. }
        | R::MapValues { .. }
        | R::SliceNew { .. }
        | R::IsNil { .. }
        | R::Wrap { .. }
        | R::Unwrap { .. }
        | R::PairNew { .. }
        | R::PairValue { .. }
        | R::PairError { .. }
        | R::ErrorNew { .. }
        | R::ErrorMessage { .. }
        | R::ErrorCause { .. }
        | R::ErrorTag { .. }
        | R::ErrorAs { .. }
        | R::IndexGet { .. }
        | R::SliceLen { .. }
        | R::SliceGet { .. }
        | R::SliceRange { .. } => false,
    }
}

/// The locals of `f` that may hold `param`'s value: the parameter itself,
/// and every local a `Use` or an `Unwrap` of a carrier is assigned to — the
/// two forms that hand the same reference on under another name. Whether a
/// carrier is later given something else does not matter; it stays one.
fn carriers(f: &Function, param: Local) -> Vec<bool> {
    let mut is = vec![false; f.locals.len()];
    is[param.index()] = true;
    loop {
        let mut changed = false;
        for s in f.blocks.iter().flat_map(|b| &b.stmts) {
            if let Inst::Assign {
                dst,
                value: Rvalue::Use(Operand::Local(c)) | Rvalue::Unwrap { value: Operand::Local(c) },
            } = s
            {
                if is[c.index()] && !is[dst.index()] {
                    is[dst.index()] = true;
                    changed = true;
                }
            }
        }
        if !changed {
            return is;
        }
    }
}

/// Whether any carrier is read anywhere in `f` in a way that may keep it.
fn keeps_any(f: &Function, carrier: &[bool], kept: &Kept) -> bool {
    let is = |o: &Operand| matches!(o, Operand::Local(l) if carrier[l.index()]);
    for b in &f.blocks {
        for s in &b.stmts {
            let keeping: Vec<&Operand> = match s {
                Inst::Assign { value, .. } => kept_operands(value, kept),
                // Stored: into a field, an element, a slice, a map.
                Inst::SetField { value, .. } | Inst::SetIndex { value, .. } => vec![value],
                Inst::SlicePush { value, .. } => vec![value],
                Inst::MapSet { key, value, .. } => vec![key, value],
                // A removal compares its key and keeps nothing.
                Inst::MapRemove { .. } => Vec::new(),
            };
            if keeping.into_iter().any(is) {
                return true;
            }
        }
        if let Terminator::Return(Some(o)) = &b.term {
            if is(o) {
                return true;
            }
        }
    }
    false
}

/// The operands of `value` through which what they hold may be kept beyond
/// the evaluation. A `Use` or `Unwrap` hands its operand to the destination,
/// which [`carriers`] follows instead.
fn kept_operands<'a>(value: &'a Rvalue, kept: &Kept) -> Vec<&'a Operand> {
    use Rvalue as R;
    match value {
        R::Use(_) | R::Unwrap { .. } => Vec::new(),
        R::Call { callee, args } => kept.kept_args(*callee, args),
        R::CallBuiltin { builtin, args } => {
            if builtin_keeps(*builtin) {
                args.iter().collect()
            } else {
                Vec::new()
            }
        }
        // Function values, `dyn` methods and the host are never looked into.
        R::CallVirtual { args, .. } | R::CallExtern { args, .. } | R::StrOp { args, .. } => {
            args.iter().collect()
        }
        R::CallClosure { callee, args } => std::iter::once(callee).chain(args).collect(),
        R::ClosureNew { captures, .. } => captures.iter().collect(),
        R::StructNew { fields, .. } | R::EnumNew { fields, .. } => fields.iter().collect(),
        R::TupleNew { elems } | R::SliceNew { elems } => elems.iter().collect(),
        R::MapNew { entries } => entries.iter().collect(),
        R::Wrap { value } => vec![value],
        R::PairNew { value, error } => vec![value, error],
        R::ErrorNew { message, value, tag, cause } => vec![message, value, tag, cause],
        // Only the state-machine transform makes these, in functions no
        // direct call reaches; counted as keeping all the same.
        R::Await { task } => vec![task],
        R::Owned { value } => vec![value],
        R::Unspill { base, owned, .. } => vec![base, owned],
        R::Yield => Vec::new(),
        // Reads that keep nothing: a comparison, a rendering, a conversion,
        // a length, an element, a lookup by key, a window or a map's keys or
        // values (each a copy), a test.
        R::Binary { .. }
        | R::Unary { .. }
        | R::ToStr { .. }
        | R::Cast { .. }
        | R::FieldGet { .. }
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
        | R::SliceRange { .. } => Vec::new(),
    }
}
