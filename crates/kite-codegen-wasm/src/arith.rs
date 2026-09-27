//! Debug-build integer arithmetic: the overflow checks, in functions of their
//! own.
//!
//! Wasm has no overflow-checking arithmetic, so a debug build's `+`, `-`, `*`,
//! `<<`, `>>` and unary `-` check by hand, and trap where the VM and the native
//! backend trap. Each check branches, and each branch used to be an `if` in
//! the function doing the arithmetic. That costs an engine far more than it
//! looks: V8's baseline compiler copies its record of every local in the
//! function at each `if`, and keeps the copies until the function is compiled.
//! A function of twenty thousand `let v = i + 1` had twenty thousand of each,
//! and took 9.3 GB and fifteen seconds to load; the release build of the same
//! file, which wraps and so has no `if`, took 48 MB. Called here instead, a
//! check is one `call` in its caller, whatever the caller's size.

use crate::*;

/// The operations with a checked form, in the order their helpers are laid
/// out. Negation is `0 - x`, and uses the subtraction's.
const CHECKED: [BinOp; 5] = [
    BinOp::AddInt,
    BinOp::SubInt,
    BinOp::MulInt,
    BinOp::Shl,
    BinOp::Shr,
];

/// The checked operations a program uses, each a function
/// `(i64, i64) -> i64`, and where they start in the function index space.
pub struct ArithHelpers {
    used: Vec<BinOp>,
    pub base: u32,
}

impl ArithHelpers {
    /// Every checked operation the program performs, in a stable order. A
    /// release build performs none: its arithmetic wraps.
    pub fn collect(program: &mir::Program, base: u32) -> ArithHelpers {
        let mut seen = [false; CHECKED.len()];
        for f in &program.fns {
            for b in &f.blocks {
                for s in &b.stmts {
                    let op = match s {
                        mir::Inst::Assign {
                            value: mir::Rvalue::Binary { op, .. },
                            ..
                        } => *op,
                        mir::Inst::Assign {
                            value:
                                mir::Rvalue::Unary {
                                    op: UnOp::NegInt, ..
                                },
                            ..
                        } => BinOp::SubInt,
                        _ => continue,
                    };
                    if let Some(i) = CHECKED.iter().position(|c| *c == op) {
                        seen[i] = true;
                    }
                }
            }
        }
        let used = CHECKED
            .iter()
            .zip(seen)
            .filter(|(_, s)| *s)
            .map(|(op, _)| *op)
            .collect();
        ArithHelpers { used, base }
    }

    /// The function index of `op`'s helper, when the program uses it.
    pub fn index(&self, op: BinOp) -> Option<u32> {
        self.used
            .iter()
            .position(|u| *u == op)
            .map(|i| self.base + i as u32)
    }

    /// Declare the helpers' one signature, returning each helper's type
    /// index.
    pub fn add_types(&self, section: &mut TypeSection, next: u32) -> Vec<u32> {
        if self.used.is_empty() {
            return Vec::new();
        }
        section
            .ty()
            .function(vec![ValType::I64, ValType::I64], vec![ValType::I64]);
        vec![next; self.used.len()]
    }

    /// Emit each helper's body, in the order [`Self::add_types`] declared them.
    pub fn emit(&self, code: &mut CodeSection) {
        for op in &self.used {
            code.function(&checked(*op));
        }
    }
}

/// `op(a, b)`, trapping where the true result does not fit or the shift
/// count is out of range.
fn checked(op: BinOp) -> Function {
    // Parameters: 0 a, 1 b. Local: 2 the wrapped result.
    let (a, b, r) = (0, 1, 2);
    let mut f = Function::new(vec![(1, ValType::I64)]);
    let trap_if_negative = |f: &mut Function| {
        f.instruction(&Instruction::I64Const(0));
        f.instruction(&Instruction::I64LtS);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::Unreachable);
        f.instruction(&Instruction::End);
    };
    match op {
        BinOp::AddInt => {
            f.instruction(&Instruction::LocalGet(a));
            f.instruction(&Instruction::LocalGet(b));
            f.instruction(&Instruction::I64Add);
            f.instruction(&Instruction::LocalSet(r));
            // Overflow when both operands differ in sign from the result:
            // `(a ^ r) & (b ^ r) < 0`.
            f.instruction(&Instruction::LocalGet(a));
            f.instruction(&Instruction::LocalGet(r));
            f.instruction(&Instruction::I64Xor);
            f.instruction(&Instruction::LocalGet(b));
            f.instruction(&Instruction::LocalGet(r));
            f.instruction(&Instruction::I64Xor);
            f.instruction(&Instruction::I64And);
            trap_if_negative(&mut f);
        }
        BinOp::SubInt => {
            f.instruction(&Instruction::LocalGet(a));
            f.instruction(&Instruction::LocalGet(b));
            f.instruction(&Instruction::I64Sub);
            f.instruction(&Instruction::LocalSet(r));
            // `(a ^ b) & (a ^ r) < 0`: the operands differed in sign and the
            // result took the wrong one.
            f.instruction(&Instruction::LocalGet(a));
            f.instruction(&Instruction::LocalGet(b));
            f.instruction(&Instruction::I64Xor);
            f.instruction(&Instruction::LocalGet(a));
            f.instruction(&Instruction::LocalGet(r));
            f.instruction(&Instruction::I64Xor);
            f.instruction(&Instruction::I64And);
            trap_if_negative(&mut f);
        }
        BinOp::MulInt => {
            f.instruction(&Instruction::LocalGet(a));
            f.instruction(&Instruction::LocalGet(b));
            f.instruction(&Instruction::I64Mul);
            f.instruction(&Instruction::LocalSet(r));
            // Division is the check, but it needs two guards of its own:
            // dividing by zero traps, and `MIN / -1` overflows. Zero never
            // overflows, and the `MIN * -1` pair always does.
            f.instruction(&Instruction::LocalGet(a));
            f.instruction(&Instruction::I64Const(0));
            f.instruction(&Instruction::I64Ne);
            f.instruction(&Instruction::If(BlockType::Empty));
            f.instruction(&Instruction::LocalGet(a));
            f.instruction(&Instruction::I64Const(-1));
            f.instruction(&Instruction::I64Eq);
            f.instruction(&Instruction::LocalGet(b));
            f.instruction(&Instruction::I64Const(i64::MIN));
            f.instruction(&Instruction::I64Eq);
            f.instruction(&Instruction::I32And);
            f.instruction(&Instruction::If(BlockType::Empty));
            f.instruction(&Instruction::Unreachable);
            f.instruction(&Instruction::End);
            f.instruction(&Instruction::LocalGet(r));
            f.instruction(&Instruction::LocalGet(a));
            f.instruction(&Instruction::I64DivS);
            f.instruction(&Instruction::LocalGet(b));
            f.instruction(&Instruction::I64Ne);
            f.instruction(&Instruction::If(BlockType::Empty));
            f.instruction(&Instruction::Unreachable);
            f.instruction(&Instruction::End);
            f.instruction(&Instruction::End);
        }
        // A count outside `0..=63` traps, where the instruction alone would
        // take it modulo 64. Compared unsigned, so a negative count is out of
        // range with the rest.
        _ => {
            f.instruction(&Instruction::LocalGet(b));
            f.instruction(&Instruction::I64Const(64));
            f.instruction(&Instruction::I64GeU);
            f.instruction(&Instruction::If(BlockType::Empty));
            f.instruction(&Instruction::Unreachable);
            f.instruction(&Instruction::End);
            f.instruction(&Instruction::LocalGet(a));
            f.instruction(&Instruction::LocalGet(b));
            f.instruction(&if op == BinOp::Shl {
                Instruction::I64Shl
            } else {
                Instruction::I64ShrS
            });
            f.instruction(&Instruction::LocalSet(r));
        }
    }
    f.instruction(&Instruction::LocalGet(r));
    f.instruction(&Instruction::End);
    f
}
