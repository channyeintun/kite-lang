//! Mid-level IR: explicit basic blocks and terminators.
//!
//! MIR is where control flow becomes a graph. That matters most for loops:
//! `continue` must jump to a block that runs the increment *before* testing
//! again, which is exactly why `for` survives HIR intact rather than being
//! flattened into an increment at the end of the body.
//!
//! Locals are not yet in SSA form. Phase 2 introduces the SSA construction the
//! optimisation passes want; the bytecode backend maps locals to registers
//! directly and does not need it.

use kite_hir::{BinOp, Builtin, EnumId, StructId, TraitId, TyId, TyKind, Types, UnOp};
use kite_span::Span;
use std::fmt;

mod async_;
mod lower;
pub use async_::frame_move;
pub use async_::transform as asyncify;
pub use async_::{DONE as TASK_DONE, VALUE as TASK_VALUE};
pub use lower::{lower, LITERAL_WINDOW};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Local(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BlockId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FnId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct StrId(pub u32);

impl Local {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl BlockId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl FnId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Default)]
pub struct Program {
    pub fns: Vec<Function>,
    /// Host functions, carried through from HIR. The Wasm backend turns each
    /// used one into an import and the glue declares it; the bytecode VM asks
    /// its embedder.
    pub externs: Vec<kite_hir::ExternDef>,
    /// Each host function's declared signature, by the same index as
    /// `externs`: a code per parameter, then `:` and a code for the result —
    /// `s` for `str`, `i` `int`, `f` `float`, `b` `bool`, `u` nothing, and
    /// `r` for anything else. What answers a host call checks what it reads
    /// and returns against this before it runs, natively and on the VM alike,
    /// so a program that declares a host function wrongly traps the same way
    /// on both. See [`extern_signature`].
    pub extern_sigs: Vec<Vec<u8>>,
    pub entry: Option<FnId>,
    /// Interned string constants, referenced by [`Operand::Str`].
    pub strings: Vec<String>,
    /// Dispatch tables, carried through unchanged from HIR. Lowering does not
    /// need them; the backends do.
    pub vtables: Vec<kite_hir::VTable>,
    /// Constructs lowering met that an earlier stage promised it never would.
    /// Non-empty means the program must not reach a backend; see
    /// [`internal_errors`].
    pub internal: Vec<Internal>,
    /// Per struct id, whether the struct has a `var` field. Such a struct
    /// can change while it is a map's key, so the runtimes' map indexes hash
    /// it by its type rather than its fields; see `kite-vm`'s `map` module.
    pub mutable_structs: Vec<bool>,
}

/// A host function's declared signature, in the encoding
/// [`Program::extern_sigs`] describes.
///
/// `str` gets its own letter rather than sharing "reference" with everything
/// else because it is the distinction a host needs: every reference is a
/// word natively, and only a string is something the host can read as a path.
pub fn extern_signature(e: &kite_hir::ExternDef, types: &Types) -> Vec<u8> {
    use kite_hir::TyKind;
    let code = |ty: TyId| match types.kind(ty) {
        TyKind::Str => b's',
        TyKind::Int => b'i',
        TyKind::Float => b'f',
        TyKind::Bool => b'b',
        TyKind::Unit | TyKind::Never | TyKind::Error => b'u',
        _ => b'r',
    };
    let mut sig: Vec<u8> = e.params.iter().map(|t| code(*t)).collect();
    sig.push(b':');
    sig.push(code(e.ret));
    sig
}

/// A bug in the compiler rather than in the program: something a stage found
/// that an earlier one promised it would not. Lowering records these instead
/// of guessing — it once dropped the statement, or panicked — and the driver
/// reports each as an internal compiler error.
#[derive(Clone, Debug)]
pub struct Internal {
    /// The function it is in, as named in MIR.
    pub function: String,
    pub span: Span,
    pub what: String,
}

/// Everything wrong with a lowered program that is the compiler's fault: what
/// lowering recorded, and any `await` or `yield` the state-machine transform
/// left behind.
///
/// Every backend assumes the transform removed each suspension, and each once
/// panicked on one it had not — which happens for an `await` inside a closure,
/// because a closure's body is lifted into a function of its own that is not
/// `async`, so the transform never visits it. The checker refuses those; this
/// is what stands behind it. Ask after [`asyncify`], before any backend.
pub fn internal_errors(program: &Program) -> Vec<Internal> {
    let mut found = program.internal.clone();
    for f in &program.fns {
        let suspends = f.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
            matches!(
                s,
                Inst::Assign { value: Rvalue::Await { .. } | Rvalue::Yield, .. }
            )
        });
        if suspends {
            found.push(Internal {
                function: f.name.clone(),
                span: f.span,
                what: "an `await` survived the state-machine transform: this function \
                       suspends but is not `async`"
                    .to_string(),
            });
        }
    }
    found
}

#[derive(Debug)]
pub struct Function {
    pub name: String,
    /// Declared `async`. The state-machine transform rewrites these into a
    /// starter that hands the scheduler a resume closure, and a resume
    /// function that picks up where the last suspension left off.
    pub is_async: bool,
    /// Whether the host may call this. Only a `pub` free function is
    /// exportable: method names are not unique across types — two types may
    /// each have an `area` — and a Wasm module may not export one name twice.
    pub exportable: bool,
    /// Parameters occupy locals `0..param_count`.
    pub param_count: usize,
    pub locals: Vec<LocalDecl>,
    pub ret: TyId,
    pub blocks: Vec<BasicBlock>,
    pub span: Span,
}

impl Function {
    pub fn entry_block(&self) -> BlockId {
        BlockId(0)
    }

    pub fn block(&self, id: BlockId) -> &BasicBlock {
        &self.blocks[id.index()]
    }
}

#[derive(Debug)]
pub struct LocalDecl {
    pub ty: TyId,
    /// Source name, or `None` for a compiler temporary.
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct BasicBlock {
    pub stmts: Vec<Inst>,
    pub term: Terminator,
}

#[derive(Clone, Debug)]
pub enum Inst {
    Assign { dst: Local, value: Rvalue },
    /// Write through a reference. Separate from `Assign` because the
    /// destination is a heap slot rather than a local.
    SetField { base: Operand, index: u32, value: Operand },
    SetIndex { base: Operand, index: Operand, value: Operand },
    /// Appends in place. Slices are copy-on-write, so this targets a local
    /// rather than an arbitrary operand.
    SlicePush { local: Local, value: Operand },
    MapSet { local: Local, key: Operand, value: Operand },
    MapRemove { local: Local, key: Operand },
}

#[derive(Clone, Debug)]
pub enum Rvalue {
    Use(Operand),
    Binary { op: BinOp, lhs: Operand, rhs: Operand },
    Unary { op: UnOp, operand: Operand },
    Call { callee: FnId, args: Vec<Operand> },
    /// Dispatched at run time from the receiver's concrete type.
    CallVirtual { trait_id: TraitId, method: u32, args: Vec<Operand> },
    /// A closure value: the lifted function, plus what it captured.
    ClosureNew { func: FnId, captures: Vec<Operand> },
    /// Calling a function value. The captures it carries are prepended to the
    /// arguments, so the lifted function is called like any other.
    CallClosure { callee: Operand, args: Vec<Operand> },
    /// A value rendered as text. `from` is its type, which the Wasm backend
    /// needs because its host calls are typed and the VM's values are not.
    ToStr { operand: Operand, from: TyId },
    /// A string operation, all of which are host calls.
    StrOp { op: kite_hir::StrKind, args: Vec<Operand> },
    /// A written numeric conversion. `from` and `to` are both needed: the VM
    /// reads the value's own tag, but Wasm needs both statically.
    Cast { operand: Operand, from: TyId, to: TyId },
    CallBuiltin { builtin: Builtin, args: Vec<Operand> },
    StructNew { struct_id: StructId, fields: Vec<Operand> },
    FieldGet { base: Operand, index: u32 },
    /// Field `index` of `base`, read to be changed and written straight back
    /// by a `SetField` of this local to the same field, with nothing between
    /// that reads the field (see [`kite_hir::ExprKind::FieldTake`]). Means what
    /// `FieldGet` means. A backend may give the local the field's own slice or
    /// map, owned, when nothing else holds it — each says how it knows that
    /// (see [`held_reads`]) — and whatever it gives the local, the write-back
    /// puts it back.
    FieldTake { base: Operand, index: u32 },
    EnumNew { enum_id: EnumId, variant: u32, fields: Vec<Operand> },
    /// The variant index of an enum value, for dispatching a `match`.
    TagOf { base: Operand },
    /// A payload field of a known enum variant. Separate from `FieldGet`
    /// because a backend using subtyped variant records has to cast first, and
    /// only the pattern that matched knows which variant it is.
    VariantGet { base: Operand, enum_id: EnumId, variant: u32, index: u32 },
    TupleNew { elems: Vec<Operand> },
    MapNew { entries: Vec<Operand> },
    MapGet { base: Operand, key: Operand },
    MapLen { base: Operand },
    /// A map's keys and values as slices, in insertion order.
    MapKeys { base: Operand },
    MapValues { base: Operand },
    SliceNew { elems: Vec<Operand> },
    IsNil { value: Operand },
    Wrap { value: Operand },
    Unwrap { value: Operand },
    PairNew { value: Operand, error: Operand },
    PairValue { base: Operand },
    PairError { base: Operand },
    /// An error value: message, the value it carries, that value's type tag,
    /// and the error it wrapped. `value` and `cause` are `Nil` and `tag` is
    /// zero where there is nothing to carry.
    ErrorNew {
        message: Operand,
        value: Operand,
        tag: Operand,
        cause: Operand,
    },
    ErrorMessage { base: Operand },
    /// The error this one wrapped, or nil.
    ErrorCause { base: Operand },
    /// The type tag of the carried value, or zero. Downcasting is this
    /// compared against a constant, so it needs no operation of its own.
    ErrorTag { base: Operand },
    /// The carried value as an optional: present when the error's tag is
    /// `tag`. One operation so the operand is read once.
    ErrorAs { base: Operand, tag: u32 },
    /// Bounds-checked; traps on failure.
    IndexGet { base: Operand, index: Operand },
    SliceLen { base: Operand },
    /// Bounds-checked; yields an optional rather than trapping.
    SliceGet { base: Operand, index: Operand },
    /// `xs[a..b]` — a fresh slice of the half-open window, clamped to what is
    /// there rather than trapping. See [`kite_hir::ExprKind::SliceRange`].
    SliceRange { base: Operand, start: Operand, end: Operand },
    /// `await t`. Never reaches a backend: the state-machine transform splits
    /// the block here into "the task is finished, take its value" and "it is
    /// not, save where we are and return to the scheduler".
    Await { task: Operand },
    /// `task.yield()` — suspend unconditionally. Also never reaches a backend.
    Yield,
    /// Whether `value`, a local, is the only holder of the slice or map in it:
    /// the owned flag a backend keeps so a write can skip the copy (see the
    /// native backend's `slices`). `false` for anything a backend keeps no
    /// flag for, which is always safe. Made only by the state-machine
    /// transform, which saves the flag in the frame with the local.
    Owned { value: Operand },
    /// A spilled local read back out of its frame — field `index` of `base` —
    /// with `owned`, the [`Rvalue::Owned`] saved beside it, saying whether the
    /// local was its value's only holder when it was spilled.
    ///
    /// A suspension is a move: the local dies with the poll that spilled it,
    /// so the frame's field is the one holder, and this hands it back. Nothing
    /// reads the field again before the next spill writes it, so a backend
    /// whose sharing is a reference count empties it here — leaving it would
    /// make every resumed slice look shared, and its next `push` a copy.
    Unspill { base: Operand, index: u32, owned: Operand },
    /// A call across the declared host boundary.
    CallExtern { index: u32, args: Vec<Operand> },
}

#[derive(Clone, Debug)]
pub enum Operand {
    Local(Local),
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(StrId),
    /// The unit value. Never materialised by a backend.
    Unit,
    Nil,
    /// Bits for a slot nothing may read: a task's value before it has one, a
    /// frame's local before it is written, the value half of a failed result.
    /// The type is carried because a typed slot needs a value of its own type,
    /// and `nil` is not one for an `int`.
    Default(TyId),
}

#[derive(Clone, Debug, Default)]
pub enum Terminator {
    Goto(BlockId),
    Branch { cond: Operand, then: BlockId, else_: BlockId },
    Return(Option<Operand>),
    /// A block that cannot be reached. Left in place rather than pruned so
    /// block indices stay stable.
    #[default]
    Unreachable,
}

impl Inst {
    /// Every operand the instruction reads, the written local of an in-place
    /// write (`SlicePush`, `MapSet`, `MapRemove`) not among them: that is a
    /// write, whatever it reads on the way.
    pub fn operands(&self) -> Vec<&Operand> {
        match self {
            Inst::Assign { value, .. } => value.operands(),
            Inst::SetField { base, value, .. } => vec![base, value],
            Inst::SetIndex { base, index, value } => vec![base, index, value],
            Inst::SlicePush { value, .. } => vec![value],
            Inst::MapSet { key, value, .. } => vec![key, value],
            Inst::MapRemove { key, .. } => vec![key],
        }
    }
}

impl Rvalue {
    /// Every operand the rvalue reads. Exhaustive, with no catch-all, so a new
    /// form fails to compile here until someone says what it reads.
    pub fn operands(&self) -> Vec<&Operand> {
        use Rvalue as R;
        match self {
            R::Use(o)
            | R::Unary { operand: o, .. }
            | R::ToStr { operand: o, .. }
            | R::Cast { operand: o, .. }
            | R::FieldGet { base: o, .. }
            | R::FieldTake { base: o, .. }
            | R::TagOf { base: o }
            | R::VariantGet { base: o, .. }
            | R::MapLen { base: o }
            | R::MapKeys { base: o }
            | R::MapValues { base: o }
            | R::IsNil { value: o }
            | R::Wrap { value: o }
            | R::Unwrap { value: o }
            | R::PairValue { base: o }
            | R::PairError { base: o }
            | R::ErrorMessage { base: o }
            | R::ErrorCause { base: o }
            | R::ErrorTag { base: o }
            | R::ErrorAs { base: o, .. }
            | R::SliceLen { base: o }
            | R::Owned { value: o }
            | R::Await { task: o } => vec![o],
            R::Unspill { base, owned, .. } => vec![base, owned],
            R::Binary { lhs, rhs, .. } => vec![lhs, rhs],
            R::MapGet { base, key } => vec![base, key],
            R::IndexGet { base, index } | R::SliceGet { base, index } => vec![base, index],
            R::SliceRange { base, start, end } => vec![base, start, end],
            R::PairNew { value, error } => vec![value, error],
            R::ErrorNew { message, value, tag, cause } => vec![message, value, tag, cause],
            R::Call { args, .. }
            | R::CallVirtual { args, .. }
            | R::StrOp { args, .. }
            | R::CallBuiltin { args, .. }
            | R::CallExtern { args, .. } => args.iter().collect(),
            R::ClosureNew { captures, .. } => captures.iter().collect(),
            R::CallClosure { callee, args } => std::iter::once(callee).chain(args).collect(),
            R::StructNew { fields, .. } | R::EnumNew { fields, .. } => fields.iter().collect(),
            R::TupleNew { elems } | R::SliceNew { elems } => elems.iter().collect(),
            R::MapNew { entries } => entries.iter().collect(),
            R::Yield => Vec::new(),
        }
    }
}

impl Terminator {
    /// The operand the terminator reads, if it reads one.
    pub fn operand(&self) -> Option<&Operand> {
        match self {
            Terminator::Branch { cond, .. } => Some(cond),
            Terminator::Return(value) => value.as_ref(),
            Terminator::Goto(_) | Terminator::Unreachable => None,
        }
    }

    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Terminator::Goto(b) => vec![*b],
            Terminator::Branch { then, else_, .. } => vec![*then, *else_],
            Terminator::Return(_) | Terminator::Unreachable => Vec::new(),
        }
    }
}

/// Whether field `index` of a `base_ty` value is one a write through a field
/// can change in place (§5.4): a `var` field of a struct the program declared,
/// holding a slice or a map. A frame's or a task's fields — named with a `$`,
/// which no program can write — are never written through, so they never
/// need the backends' bookkeeping for one (see [`held_reads`]).
pub fn takeable_field(types: &Types, base_ty: TyId, index: u32) -> bool {
    let TyKind::Struct(sid) = *types.kind(base_ty) else {
        return false;
    };
    let Some(field) = types.struct_def(sid).fields.get(index as usize) else {
        return false;
    };
    field.mutable
        && !field.name.starts_with('$')
        && matches!(types.kind(field.ty), TyKind::Slice(_) | TyKind::Map(..))
}

/// The locals that only glance at a field's slice or map, by local: for each,
/// the block that reads the field into it and the last statement there that
/// reads the local.
///
/// A write through a field (`q.items.push(v)`) goes straight into the field's
/// slice when nothing else holds it, and on a target with no reference count
/// the backend learns that from a mark on the slice itself: set by the
/// write-back, and cleared by any read of the field that could keep what it
/// read — into a binding, a call, a closure, another field, a task. Clearing
/// it on *every* read would make the commonest loop of all quadratic again:
/// `if q.items.len() < n { q.items.push(v) }` reads the field each time round.
/// So a read is let off when its local is assigned once, read only in the
/// same block, only by reads that keep nothing and run no program code — a
/// length, an element, a lookup, a window (which copies), a comparison — and
/// with nothing between the field read and the last of those that could
/// write a slice or map in place: no call of any kind, no suspension, no
/// in-place write, no `FieldTake`. Such a local has stopped being read before
/// anything could change what it holds, so it holds nothing a write could be
/// seen through. The state-machine transform's spills and reloads of it
/// ([`frame_move`]) are not reads: the frame's copy only ever goes back into
/// the local, which reads nothing before the block reads the field again.
///
/// The bytecode VM's sharing is a reference count, which a local that still
/// holds the slice would spoil; it empties these locals after their last
/// read, which is the same fact put to the opposite use.
pub fn held_reads(func: &Function) -> Vec<Option<(usize, usize)>> {
    let n = func.locals.len();
    // Where each local is assigned, if exactly once by a `FieldGet`; `None`
    // once anything rules it out.
    let mut def: Vec<Option<(usize, usize)>> = vec![None; n];
    let mut ruled_out = vec![false; n];
    for r in ruled_out.iter_mut().take(func.param_count) {
        *r = true;
    }
    for (bi, b) in func.blocks.iter().enumerate() {
        for (si, s) in b.stmts.iter().enumerate() {
            if frame_move(func, s).is_some() {
                continue;
            }
            let written = match s {
                Inst::Assign { dst, value } => {
                    let i = dst.index();
                    if def[i].is_some() || !matches!(value, Rvalue::FieldGet { .. }) {
                        ruled_out[i] = true;
                    }
                    def[i] = Some((bi, si));
                    continue;
                }
                Inst::SlicePush { local, .. }
                | Inst::MapSet { local, .. }
                | Inst::MapRemove { local, .. }
                | Inst::SetIndex { base: Operand::Local(local), .. } => *local,
                Inst::SetIndex { .. } | Inst::SetField { .. } => continue,
            };
            ruled_out[written.index()] = true;
        }
    }
    let mut last: Vec<Option<(usize, usize)>> = vec![None; n];
    for (bi, b) in func.blocks.iter().enumerate() {
        for (si, s) in b.stmts.iter().enumerate() {
            let glance = match s {
                Inst::Assign {
                    value:
                        Rvalue::SliceLen { base }
                        | Rvalue::MapLen { base }
                        | Rvalue::MapKeys { base }
                        | Rvalue::MapValues { base }
                        | Rvalue::IndexGet { base, .. }
                        | Rvalue::SliceGet { base, .. }
                        | Rvalue::MapGet { base, .. }
                        | Rvalue::SliceRange { base, .. },
                    ..
                } => Some(base),
                _ => None,
            };
            let moved = frame_move(func, s);
            for o in s.operands() {
                let Operand::Local(l) = o else { continue };
                if moved == Some(*l) {
                    continue;
                }
                let i = l.index();
                let looks = matches!(s, Inst::Assign { value: Rvalue::Binary { .. }, .. })
                    || matches!(glance, Some(Operand::Local(g)) if g == l);
                match def[i] {
                    Some((db, ds)) if looks && db == bi && ds < si => last[i] = Some((bi, si)),
                    _ => ruled_out[i] = true,
                }
            }
        }
        if let Some(Operand::Local(l)) = b.term.operand() {
            ruled_out[l.index()] = true;
        }
    }
    (0..n)
        .map(|i| {
            if ruled_out[i] {
                return None;
            }
            let (bi, si) = def[i]?;
            // Read by nothing at all, it is over the moment it is made.
            let (_, end) = last[i].unwrap_or((bi, si));
            let stmts = &func.blocks[bi].stmts;
            let quiet = stmts[si + 1..end.max(si + 1)].iter().all(|s| match s {
                Inst::Assign { value, .. } => !may_write_in_place(value),
                Inst::SetField { .. } => true,
                Inst::SetIndex { .. }
                | Inst::SlicePush { .. }
                | Inst::MapSet { .. }
                | Inst::MapRemove { .. } => false,
            });
            quiet.then_some((bi, end))
        })
        .collect()
}

/// Whether evaluating `value` could write some slice or map in place, or run
/// code of the program's that could: see [`held_reads`]. Exhaustive, with no
/// catch-all, so a new form has to be decided.
fn may_write_in_place(value: &Rvalue) -> bool {
    use Rvalue as R;
    match value {
        R::Call { .. }
        | R::CallVirtual { .. }
        | R::CallClosure { .. }
        | R::CallBuiltin { .. }
        | R::CallExtern { .. }
        | R::StrOp { .. }
        // A rendering can reach a `Display` the program wrote.
        | R::ToStr { .. }
        | R::Await { .. }
        | R::Yield
        | R::FieldTake { .. } => true,
        R::Use(_)
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
        | R::SliceRange { .. }
        | R::Owned { .. }
        | R::Unspill { .. } => false,
    }
}

/// Blocks reachable from the entry.
pub fn reachable_blocks(func: &Function) -> Vec<bool> {
    let mut seen = vec![false; func.blocks.len()];
    let mut stack = vec![func.entry_block()];
    while let Some(b) = stack.pop() {
        if seen[b.index()] {
            continue;
        }
        seen[b.index()] = true;
        for s in func.blocks[b.index()].term.successors() {
            stack.push(s);
        }
    }
    seen
}

// ---------------------------------------------------------------------------
// Display, for `kitec --emit mir`
// ---------------------------------------------------------------------------

/// Pairs a program with the arena its `TyId`s belong to, so it can be printed.
pub struct Display_<'a> {
    pub program: &'a Program,
    pub types: &'a Types,
}

impl Program {
    /// Render for `kitec --emit mir`. A `TyId` is meaningless without the
    /// arena, so it must be supplied.
    pub fn render<'a>(&'a self, types: &'a Types) -> Display_<'a> {
        Display_ { program: self, types }
    }
}

impl fmt::Display for Display_<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, s) in self.program.strings.iter().enumerate() {
            writeln!(f, "str{} = {:?}", i, s)?;
        }
        if !self.program.strings.is_empty() {
            writeln!(f)?;
        }
        for func in &self.program.fns {
            self.write_fn(f, func)?;
            writeln!(f)?;
        }
        Ok(())
    }
}

impl Display_<'_> {
    fn write_fn(&self, f: &mut fmt::Formatter<'_>, func: &Function) -> fmt::Result {
        write!(f, "fn {}(", func.name)?;
        for i in 0..func.param_count {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "_{}: {}", i, self.types.name(func.locals[i].ty))?;
        }
        writeln!(f, ") -> {} {{", self.types.name(func.ret))?;

        for (i, l) in func.locals.iter().enumerate().skip(func.param_count) {
            let ty = self.types.name(l.ty);
            match &l.name {
                Some(n) => writeln!(f, "  let _{}: {}   // {}", i, ty, n)?,
                None => writeln!(f, "  let _{}: {}", i, ty)?,
            }
        }

        for (i, b) in func.blocks.iter().enumerate() {
            writeln!(f, "  bb{}:", i)?;
            for s in &b.stmts {
                writeln!(f, "    {}", s)?;
            }
            writeln!(f, "    {}", b.term)?;
        }
        writeln!(f, "}}")
    }
}

impl fmt::Display for Inst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Inst::Assign { dst, value } => write!(f, "_{} = {}", dst.0, value),
            Inst::SetField { base, index, value } => {
                write!(f, "{}.{} = {}", base, index, value)
            }
            Inst::SetIndex { base, index, value } => write!(f, "{}[{}] = {}", base, index, value),
            Inst::SlicePush { local, value } => write!(f, "_{}.push({})", local.0, value),
            Inst::MapSet { local, key, value } => {
                write!(f, "_{}[{}] = {}", local.0, key, value)
            }
            Inst::MapRemove { local, key } => write!(f, "_{}.remove({})", local.0, key),
        }
    }
}

impl fmt::Display for Rvalue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rvalue::Use(o) => write!(f, "{}", o),
            Rvalue::Binary { op, lhs, rhs } => write!(f, "{:?}({}, {})", op, lhs, rhs),
            Rvalue::Unary { op, operand } => write!(f, "{:?}({})", op, operand),
            Rvalue::Call { callee, args } => {
                write!(f, "call fn{}(", callee.0)?;
                write_operands(f, args)?;
                write!(f, ")")
            }
            Rvalue::ClosureNew { func, captures } => {
                write!(f, "closure fn{}[", func.0)?;
                write_operands(f, captures)?;
                write!(f, "]")
            }
            Rvalue::CallClosure { callee, args } => {
                write!(f, "{}(", callee)?;
                write_operands(f, args)?;
                write!(f, ")")
            }
            Rvalue::ToStr { operand, .. } => write!(f, "str {}", operand),
            Rvalue::StrOp { op, args } => {
                write!(f, "{} ", op.name())?;
                write_operands(f, args)
            }
            Rvalue::Cast { operand, to, .. } => write!(f, "{} as ty{}", operand, to.0),
            Rvalue::CallVirtual { trait_id, method, args } => {
                write!(f, "virtual trait{}#{}(", trait_id.0, method)?;
                write_operands(f, args)?;
                write!(f, ")")
            }
            Rvalue::CallBuiltin { builtin, args } => {
                write!(f, "call {}(", builtin.path())?;
                write_operands(f, args)?;
                write!(f, ")")
            }
            Rvalue::StructNew { struct_id, fields } => {
                write!(f, "struct{}{{", struct_id.0)?;
                write_operands(f, fields)?;
                write!(f, "}}")
            }
            Rvalue::FieldGet { base, index } => write!(f, "{}.{}", base, index),
            Rvalue::FieldTake { base, index } => write!(f, "take {}.{}", base, index),
            Rvalue::EnumNew { enum_id, variant, fields } => {
                write!(f, "enum{}#{}(", enum_id.0, variant)?;
                write_operands(f, fields)?;
                write!(f, ")")
            }
            Rvalue::TagOf { base } => write!(f, "tag {}", base),
            Rvalue::VariantGet { base, variant, index, .. } => {
                write!(f, "{}#{}.{}", base, variant, index)
            }
            Rvalue::MapNew { entries } => write!(f, "{{{} entries}}", entries.len() / 2),
            Rvalue::MapGet { base, key } => write!(f, "{}[{}]", base, key),
            Rvalue::MapLen { base } => write!(f, "len {}", base),
            Rvalue::MapKeys { base } => write!(f, "keys {}", base),
            Rvalue::MapValues { base } => write!(f, "values {}", base),
            Rvalue::TupleNew { elems } => {
                write!(f, "(")?;
                write_operands(f, elems)?;
                write!(f, ")")
            }
            Rvalue::SliceNew { elems } => {
                write!(f, "[")?;
                write_operands(f, elems)?;
                write!(f, "]")
            }
            Rvalue::IndexGet { base, index } => write!(f, "{}[{}]", base, index),
            Rvalue::IsNil { value } => write!(f, "is-nil {}", value),
            Rvalue::Wrap { value } => write!(f, "wrap {}", value),
            Rvalue::Unwrap { value } => write!(f, "unwrap {}", value),
            Rvalue::PairNew { value, error } => write!(f, "pair({}, {})", value, error),
            Rvalue::PairValue { base } => write!(f, "{}.0", base),
            Rvalue::PairError { base } => write!(f, "{}.1", base),
            Rvalue::ErrorNew { message, value, tag, cause } => {
                write!(f, "error({}, {}, {}, {})", message, value, tag, cause)
            }
            Rvalue::ErrorMessage { base } => write!(f, "{}.message()", base),
            Rvalue::ErrorCause { base } => write!(f, "{}.cause()", base),
            Rvalue::ErrorTag { base } => write!(f, "{}.tag", base),
            Rvalue::ErrorAs { base, tag } => write!(f, "{}.as({})", base, tag),
            Rvalue::SliceLen { base } => write!(f, "len {}", base),
            Rvalue::SliceGet { base, index } => write!(f, "{}.get({})", base, index),
            Rvalue::SliceRange { base, start, end } => {
                write!(f, "{}[{}..{}]", base, start, end)
            }
            Rvalue::Await { task } => write!(f, "await {}", task),
            Rvalue::Yield => write!(f, "yield"),
            Rvalue::Owned { value } => write!(f, "owned {}", value),
            Rvalue::Unspill { base, index, owned } => {
                write!(f, "unspill {}.{} owned {}", base, index, owned)
            }
            Rvalue::CallExtern { index, args } => {
                write!(f, "host#{}(", index)?;
                write_operands(f, args)?;
                write!(f, ")")
            }
        }
    }
}

fn write_operands(f: &mut fmt::Formatter<'_>, args: &[Operand]) -> fmt::Result {
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{}", a)?;
    }
    Ok(())
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Local(l) => write!(f, "_{}", l.0),
            Operand::Int(v) => write!(f, "{}", v),
            Operand::Float(v) => write!(f, "{:?}", v),
            Operand::Bool(v) => write!(f, "{}", v),
            Operand::Str(s) => write!(f, "str{}", s.0),
            Operand::Unit => write!(f, "()"),
            Operand::Nil => write!(f, "nil"),
            Operand::Default(_) => write!(f, "<default>"),
        }
    }
}

impl fmt::Display for Terminator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Terminator::Goto(b) => write!(f, "goto bb{}", b.0),
            Terminator::Branch { cond, then, else_ } => {
                write!(f, "branch {} ? bb{} : bb{}", cond, then.0, else_.0)
            }
            Terminator::Return(Some(o)) => write!(f, "return {}", o),
            Terminator::Return(None) => write!(f, "return"),
            Terminator::Unreachable => write!(f, "unreachable"),
        }
    }
}

#[cfg(test)]
mod tests;
