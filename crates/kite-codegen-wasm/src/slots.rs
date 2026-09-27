//! Which Wasm local each MIR local lives in.
//!
//! MIR makes a temporary for every intermediate value and never reuses one,
//! so a function of a thousand `if`s has thousands of locals, nearly all of
//! them made and read within one block. Given a Wasm local each, they cost the
//! engine far more than their number suggests: this backend's dispatch loop
//! makes every block a place control flow meets, and V8 keeps a record of
//! every local at each of those, in its baseline compiler and again in its
//! optimising one. A function of 1,500 `if` statements took over a minute and
//! 2 GB to load, debug build or release, where the bytecode VM ran it at once.
//!
//! So a local that lives within one block — written there before anything
//! reads it, and neither read nor written in any other — shares a Wasm local
//! of its type with others of its kind whose lives do not overlap. Every block
//! starts with all of those free, since nothing in one is live on entry to
//! another. Everything else — a parameter, a local that crosses blocks or is
//! read before it is written, a slice written in place, whose owned flag is
//! kept by its MIR local — keeps a Wasm local of its own, as every local used
//! to.

use crate::*;

/// The Wasm locals of a function body, besides its parameters.
pub struct Slots {
    /// The Wasm local each MIR local lives in, by MIR local.
    pub of: Vec<u32>,
    /// The types of the locals to declare after the parameters, in order.
    pub declared: Vec<ValType>,
}

/// Where a local occurs: its one block, its first and last position there,
/// and whether it may share.
#[derive(Clone, Copy)]
struct Seen {
    block: usize,
    first: usize,
    last: usize,
    shares: bool,
}

pub fn assign(f: &mir::Function, types: &Types, layout: &TypeLayout) -> Slots {
    let n = f.locals.len();
    let mut seen: Vec<Option<Seen>> = vec![None; n];
    // Slices written in place, which keep a Wasm local of their own.
    let mut flagged: Vec<usize> = Vec::new();
    // The terminator counts as the position after the last statement.
    let mut note = |l: &mir::Local, block: usize, at: usize, reads: bool| {
        let i = l.index();
        match &mut seen[i] {
            None => {
                seen[i] = Some(Seen {
                    block,
                    first: at,
                    last: at,
                    shares: !reads,
                });
            }
            Some(s) if s.block == block => s.last = at,
            Some(s) => s.shares = false,
        }
    };
    for (bi, block) in f.blocks.iter().enumerate() {
        for (si, stmt) in block.stmts.iter().enumerate() {
            // Reads first, so a local read by the statement that first writes
            // it counts as read before it is written.
            for o in stmt.operands() {
                if let mir::Operand::Local(l) = o {
                    note(l, bi, si, true);
                }
            }
            match stmt {
                mir::Inst::Assign { dst, .. } => note(dst, bi, si, false),
                // A write in place reads what it writes into.
                mir::Inst::SlicePush { local, .. }
                | mir::Inst::MapSet { local, .. }
                | mir::Inst::MapRemove { local, .. } => note(local, bi, si, true),
                mir::Inst::SetIndex { .. } | mir::Inst::SetField { .. } => {}
            }
            if let mir::Inst::SlicePush { local, .. }
            | mir::Inst::SetIndex {
                base: mir::Operand::Local(local),
                ..
            } = stmt
            {
                flagged.push(local.index());
            }
        }
        if let Some(mir::Operand::Local(l)) = block.term.operand() {
            note(l, bi, block.stmts.len(), true);
        }
    }
    for l in flagged {
        if let Some(s) = &mut seen[l] {
            s.shares = false;
        }
    }

    let val_type = |l: usize| val_type_with(f.locals[l].ty, types, layout);
    let base = f.param_count as u32;
    let mut of: Vec<u32> = (0..n as u32).collect();
    let mut declared: Vec<ValType> = Vec::new();
    // Sharing locals by block, in the order they are first written.
    let mut by_block: Vec<Vec<usize>> = vec![Vec::new(); f.blocks.len()];
    let mut never: Vec<usize> = Vec::new();
    for l in f.param_count..n {
        match seen[l] {
            Some(s) if s.shares => by_block[s.block].push(l),
            // Nothing reads or writes it, so any local of its type will do.
            None => never.push(l),
            Some(_) => {
                of[l] = base + declared.len() as u32;
                declared.push(val_type(l));
            }
        }
    }

    // The shared Wasm locals, by type. Every block starts with all of them
    // free, takes them in creation order, and reuses what it gives back.
    let mut pool: Vec<(ValType, Vec<u32>)> = Vec::new();
    fn type_of(vt: ValType, pool: &mut Vec<(ValType, Vec<u32>)>) -> usize {
        match pool.iter().position(|(t, _)| *t == vt) {
            Some(t) => t,
            None => {
                pool.push((vt, Vec::new()));
                pool.len() - 1
            }
        }
    }
    for locals in &mut by_block {
        locals.sort_by_key(|&l| seen[l].map(|s| s.first));
        let mut next = vec![0usize; pool.len()];
        let mut given_back: Vec<Vec<u32>> = vec![Vec::new(); pool.len()];
        // Taken locals, soonest free first: the position each is last used
        // at, its type, and its index.
        let mut live: std::collections::BinaryHeap<std::cmp::Reverse<(usize, usize, u32)>> =
            std::collections::BinaryHeap::new();
        for &l in locals.iter() {
            let Some(s) = seen[l] else { continue };
            // A local last used at this position is still being read by the
            // statement that writes this one, so it stays taken.
            while let Some(&std::cmp::Reverse((last, t, at))) = live.peek() {
                if last >= s.first {
                    break;
                }
                live.pop();
                given_back[t].push(at);
            }
            let t = type_of(val_type(l), &mut pool);
            if t == next.len() {
                next.push(0);
                given_back.push(Vec::new());
            }
            let at = match given_back[t].pop() {
                Some(at) => at,
                None => {
                    if next[t] == pool[t].1.len() {
                        pool[t].1.push(base + declared.len() as u32);
                        declared.push(pool[t].0);
                    }
                    next[t] += 1;
                    pool[t].1[next[t] - 1]
                }
            };
            live.push(std::cmp::Reverse((s.last, t, at)));
            of[l] = at;
        }
    }
    for l in never {
        let t = type_of(val_type(l), &mut pool);
        if pool[t].1.is_empty() {
            pool[t].1.push(base + declared.len() as u32);
            declared.push(pool[t].0);
        }
        of[l] = pool[t].1[0];
    }
    Slots { of, declared }
}
