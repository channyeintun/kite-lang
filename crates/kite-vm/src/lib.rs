//! The Kite bytecode virtual machine.
//!
//! A flat register file with per-frame bases, and a dispatch loop over
//! [`Op`]. Output goes to a caller-supplied sink so tests capture it without
//! touching the process's stdout.
//!
//! Failures here are **traps**, not errors. Division by zero and integer
//! overflow are bugs in the program, not runtime conditions, and Kite has no
//! `recover` — a trap ends the program. This matches the Wasm target, where the
//! same conditions become the `unreachable` instruction.

use kite_codegen_kbc::{Chunk, Native, Op, Reg};
use std::fmt;
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

/// The maximum call depth before the VM reports a trap rather than exhausting
/// the host stack.
pub const MAX_FRAMES: usize = 2048;

#[derive(Clone, Debug)]
pub enum Value {
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Rc<str>),
    /// A struct instance. Kite aggregates are GC references, so assignment
    /// copies the handle; `RefCell` is what lets a `var` field be written
    /// through one. The checker has already proved only `var` fields are.
    Struct(Rc<StructValue>),
}

#[derive(Debug)]
pub struct StructValue {
    pub struct_id: u32,
    pub fields: RefCell<Vec<Value>>,
}

/// Structural equality, per the specification: two structs are equal when
/// their fields are. Reference identity is `ptr.same`, not `==`.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Unit, Value::Unit) => true,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Struct(a), Value::Struct(b)) => {
                a.struct_id == b.struct_id && *a.fields.borrow() == *b.fields.borrow()
            }
            _ => false,
        }
    }
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Unit => "()",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Bool(_) => "bool",
            Value::Str(_) => "str",
            Value::Struct(_) => "struct",
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Unit => write!(f, "()"),
            Value::Int(v) => write!(f, "{}", v),
            Value::Float(v) => {
                // Print floats so they read back as Kite floats: `1.0`, not `1`.
                if v.fract() == 0.0 && v.is_finite() {
                    write!(f, "{:.1}", v)
                } else {
                    write!(f, "{}", v)
                }
            }
            Value::Bool(v) => write!(f, "{}", v),
            Value::Str(s) => write!(f, "{}", s),
            Value::Struct(s) => {
                // Debug-shaped output until the `Display` trait lands.
                write!(f, "{{")?;
                for (i, v) in s.fields.borrow().iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "}}")
            }
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Trap {
    DivideByZero,
    IntegerOverflow(&'static str),
    CallDepthExceeded,
    /// Reached `unreachable`. Indicates a compiler bug rather than a program
    /// bug, so it names where.
    Unreachable { function: String, pc: usize },
    NoEntryPoint,
    /// A register held a value of the wrong type. Only reachable through a
    /// codegen bug, since the type checker has already run.
    TypeConfusion { op: &'static str, found: &'static str },
}

impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Trap::DivideByZero => write!(f, "divide by zero"),
            Trap::IntegerOverflow(op) => write!(f, "integer overflow in `{}`", op),
            Trap::CallDepthExceeded => {
                write!(f, "call depth exceeded {} frames", MAX_FRAMES)
            }
            Trap::Unreachable { function, pc } => {
                write!(f, "reached unreachable code in `{}` at pc {}", function, pc)
            }
            Trap::NoEntryPoint => write!(f, "no `main` function"),
            Trap::TypeConfusion { op, found } => {
                write!(f, "`{}` received a `{}`", op, found)
            }
        }
    }
}

impl std::error::Error for Trap {}

struct Frame {
    func: u32,
    pc: usize,
    /// Index into the register file where this frame's registers begin.
    base: usize,
    /// Where the caller wants the result, as an absolute register index.
    ret_slot: usize,
}

pub fn run(chunk: &Chunk, out: &mut dyn Write) -> Result<(), Trap> {
    let entry = chunk.entry.ok_or(Trap::NoEntryPoint)?;
    Vm {
        chunk,
        out,
        regs: Vec::new(),
        frames: Vec::new(),
    }
    .execute(entry)
}

struct Vm<'a> {
    chunk: &'a Chunk,
    out: &'a mut dyn Write,
    regs: Vec<Value>,
    frames: Vec<Frame>,
}

/// Read two register operands and apply `$f`, or trap on type confusion.
macro_rules! arith {
    ($self:expr, $frame_base:expr, $dst:expr, $a:expr, $b:expr, $name:literal,
     $variant:ident, $out:ident, $f:expr) => {{
        let av = $self.get($frame_base, $a);
        let bv = $self.get($frame_base, $b);
        match (av, bv) {
            (Value::$variant(x), Value::$variant(y)) => {
                let r: Result<_, Trap> = $f(x, y);
                $self.set($frame_base, $dst, Value::$out(r?));
            }
            (other, _) => {
                return Err(Trap::TypeConfusion {
                    op: $name,
                    found: other.type_name(),
                })
            }
        }
    }};
}

impl<'a> Vm<'a> {
    fn execute(&mut self, entry: u32) -> Result<(), Trap> {
        let proto = self.chunk.function(entry);
        self.regs = vec![Value::Unit; proto.frame_size];
        self.frames.push(Frame {
            func: entry,
            pc: 0,
            base: 0,
            // The entry frame's result is discarded; slot 0 is a safe sink
            // because the frame is torn down immediately after.
            ret_slot: 0,
        });

        loop {
            let (func, pc, base) = {
                let f = self.frames.last().expect("at least one frame");
                (f.func, f.pc, f.base)
            };
            let proto = self.chunk.function(func);

            if pc >= proto.code.len() {
                // Falling off the end of a function returns unit. Codegen
                // always emits a terminator, so this is belt and braces.
                if self.pop_frame(Value::Unit) {
                    return Ok(());
                }
                continue;
            }

            self.frames.last_mut().unwrap().pc = pc + 1;
            let op = proto.code[pc].clone();

            match op {
                // ---- loads ----------------------------------------------
                Op::LoadInt { dst, value } => self.set(base, dst, Value::Int(value)),
                Op::LoadFloat { dst, value } => self.set(base, dst, Value::Float(value)),
                Op::LoadBool { dst, value } => self.set(base, dst, Value::Bool(value)),
                Op::LoadUnit { dst } => self.set(base, dst, Value::Unit),
                Op::LoadStr { dst, idx } => {
                    let s = self.chunk.strings[idx as usize].clone();
                    self.set(base, dst, Value::Str(s));
                }
                Op::Move { dst, src } => {
                    let v = self.get(base, src);
                    self.set(base, dst, v);
                }

                // ---- integer arithmetic ----------------------------------
                Op::AddInt { dst, a, b } => arith!(self, base, dst, a, b, "+", Int, Int, |x: i64,
                                                                                          y: i64| {
                    x.checked_add(y).ok_or(Trap::IntegerOverflow("+"))
                }),
                Op::SubInt { dst, a, b } => arith!(self, base, dst, a, b, "-", Int, Int, |x: i64,
                                                                                          y: i64| {
                    x.checked_sub(y).ok_or(Trap::IntegerOverflow("-"))
                }),
                Op::MulInt { dst, a, b } => arith!(self, base, dst, a, b, "*", Int, Int, |x: i64,
                                                                                          y: i64| {
                    x.checked_mul(y).ok_or(Trap::IntegerOverflow("*"))
                }),
                Op::DivInt { dst, a, b } => arith!(self, base, dst, a, b, "/", Int, Int, |x: i64,
                                                                                          y: i64| {
                    if y == 0 {
                        Err(Trap::DivideByZero)
                    } else {
                        x.checked_div(y).ok_or(Trap::IntegerOverflow("/"))
                    }
                }),
                Op::RemInt { dst, a, b } => arith!(self, base, dst, a, b, "%", Int, Int, |x: i64,
                                                                                          y: i64| {
                    if y == 0 {
                        Err(Trap::DivideByZero)
                    } else {
                        x.checked_rem(y).ok_or(Trap::IntegerOverflow("%"))
                    }
                }),

                // ---- float arithmetic ------------------------------------
                Op::AddFloat { dst, a, b } => {
                    arith!(self, base, dst, a, b, "+", Float, Float, |x: f64, y: f64| Ok(x + y))
                }
                Op::SubFloat { dst, a, b } => {
                    arith!(self, base, dst, a, b, "-", Float, Float, |x: f64, y: f64| Ok(x - y))
                }
                Op::MulFloat { dst, a, b } => {
                    arith!(self, base, dst, a, b, "*", Float, Float, |x: f64, y: f64| Ok(x * y))
                }
                // IEEE-754 division by zero yields an infinity, and does not
                // trap. Only integer division does.
                Op::DivFloat { dst, a, b } => {
                    arith!(self, base, dst, a, b, "/", Float, Float, |x: f64, y: f64| Ok(x / y))
                }

                Op::ConcatStr { dst, a, b } => {
                    let (av, bv) = (self.get(base, a), self.get(base, b));
                    match (av, bv) {
                        (Value::Str(x), Value::Str(y)) => {
                            let joined: Rc<str> = Rc::from(format!("{}{}", x, y).as_str());
                            self.set(base, dst, Value::Str(joined));
                        }
                        (other, _) => {
                            return Err(Trap::TypeConfusion {
                                op: "+",
                                found: other.type_name(),
                            })
                        }
                    }
                }

                // ---- bitwise ---------------------------------------------
                Op::BitAnd { dst, a, b } => {
                    arith!(self, base, dst, a, b, "&", Int, Int, |x: i64, y: i64| Ok(x & y))
                }
                Op::BitOr { dst, a, b } => {
                    arith!(self, base, dst, a, b, "|", Int, Int, |x: i64, y: i64| Ok(x | y))
                }
                Op::BitXor { dst, a, b } => {
                    arith!(self, base, dst, a, b, "^", Int, Int, |x: i64, y: i64| Ok(x ^ y))
                }
                Op::Shl { dst, a, b } => {
                    arith!(self, base, dst, a, b, "<<", Int, Int, |x: i64, y: i64| {
                        if !(0..64).contains(&y) {
                            Err(Trap::IntegerOverflow("<<"))
                        } else {
                            Ok(x << y)
                        }
                    })
                }
                Op::Shr { dst, a, b } => {
                    arith!(self, base, dst, a, b, ">>", Int, Int, |x: i64, y: i64| {
                        if !(0..64).contains(&y) {
                            Err(Trap::IntegerOverflow(">>"))
                        } else {
                            Ok(x >> y)
                        }
                    })
                }

                // ---- unary -----------------------------------------------
                Op::NegInt { dst, a } => match self.get(base, a) {
                    Value::Int(v) => {
                        let r = v.checked_neg().ok_or(Trap::IntegerOverflow("-"))?;
                        self.set(base, dst, Value::Int(r));
                    }
                    other => {
                        return Err(Trap::TypeConfusion { op: "-", found: other.type_name() })
                    }
                },
                Op::NegFloat { dst, a } => match self.get(base, a) {
                    Value::Float(v) => self.set(base, dst, Value::Float(-v)),
                    other => {
                        return Err(Trap::TypeConfusion { op: "-", found: other.type_name() })
                    }
                },
                Op::Not { dst, a } => match self.get(base, a) {
                    Value::Bool(v) => self.set(base, dst, Value::Bool(!v)),
                    other => {
                        return Err(Trap::TypeConfusion { op: "!", found: other.type_name() })
                    }
                },

                // ---- comparison ------------------------------------------
                Op::EqInt { dst, a, b } => {
                    arith!(self, base, dst, a, b, "==", Int, Bool, |x, y| Ok(x == y))
                }
                Op::NeInt { dst, a, b } => {
                    arith!(self, base, dst, a, b, "!=", Int, Bool, |x, y| Ok(x != y))
                }
                Op::LtInt { dst, a, b } => {
                    arith!(self, base, dst, a, b, "<", Int, Bool, |x, y| Ok(x < y))
                }
                Op::LeInt { dst, a, b } => {
                    arith!(self, base, dst, a, b, "<=", Int, Bool, |x, y| Ok(x <= y))
                }
                Op::GtInt { dst, a, b } => {
                    arith!(self, base, dst, a, b, ">", Int, Bool, |x, y| Ok(x > y))
                }
                Op::GeInt { dst, a, b } => {
                    arith!(self, base, dst, a, b, ">=", Int, Bool, |x, y| Ok(x >= y))
                }

                Op::EqFloat { dst, a, b } => {
                    arith!(self, base, dst, a, b, "==", Float, Bool, |x, y| Ok(x == y))
                }
                Op::NeFloat { dst, a, b } => {
                    arith!(self, base, dst, a, b, "!=", Float, Bool, |x, y| Ok(x != y))
                }
                Op::LtFloat { dst, a, b } => {
                    arith!(self, base, dst, a, b, "<", Float, Bool, |x, y| Ok(x < y))
                }
                Op::LeFloat { dst, a, b } => {
                    arith!(self, base, dst, a, b, "<=", Float, Bool, |x, y| Ok(x <= y))
                }
                Op::GtFloat { dst, a, b } => {
                    arith!(self, base, dst, a, b, ">", Float, Bool, |x, y| Ok(x > y))
                }
                Op::GeFloat { dst, a, b } => {
                    arith!(self, base, dst, a, b, ">=", Float, Bool, |x, y| Ok(x >= y))
                }

                Op::EqBool { dst, a, b } => {
                    arith!(self, base, dst, a, b, "==", Bool, Bool, |x, y| Ok(x == y))
                }
                Op::NeBool { dst, a, b } => {
                    arith!(self, base, dst, a, b, "!=", Bool, Bool, |x, y| Ok(x != y))
                }
                Op::EqStr { dst, a, b } => {
                    arith!(self, base, dst, a, b, "==", Str, Bool, |x: Rc<str>, y: Rc<str>| Ok(
                        x == y
                    ))
                }
                Op::NeStr { dst, a, b } => {
                    arith!(self, base, dst, a, b, "!=", Str, Bool, |x: Rc<str>, y: Rc<str>| Ok(
                        x != y
                    ))
                }

                // Structural comparison. `Value`'s `PartialEq` walks
                // aggregates field by field.
                Op::EqValue { dst, a, b } => {
                    let (x, y) = (self.get(base, a), self.get(base, b));
                    self.set(base, dst, Value::Bool(x == y));
                }
                Op::NeValue { dst, a, b } => {
                    let (x, y) = (self.get(base, a), self.get(base, b));
                    self.set(base, dst, Value::Bool(x != y));
                }

                // ---- control ---------------------------------------------
                Op::Jump { target } => {
                    self.frames.last_mut().unwrap().pc = target as usize;
                }
                Op::JumpIfFalse { cond, target } => match self.get(base, cond) {
                    Value::Bool(false) => {
                        self.frames.last_mut().unwrap().pc = target as usize;
                    }
                    Value::Bool(true) => {}
                    other => {
                        return Err(Trap::TypeConfusion {
                            op: "branch",
                            found: other.type_name(),
                        })
                    }
                },

                Op::NewStruct { dst, struct_id, base: arg_base, count } => {
                    let mut fields = Vec::with_capacity(count as usize);
                    for i in 0..count as usize {
                        fields.push(self.regs[base + arg_base as usize + i].clone());
                    }
                    self.set(
                        base,
                        dst,
                        Value::Struct(Rc::new(StructValue {
                            struct_id,
                            fields: RefCell::new(fields),
                        })),
                    );
                }

                Op::GetField { dst, obj, index } => match self.get(base, obj) {
                    Value::Struct(s) => {
                        let v = s.fields.borrow()[index as usize].clone();
                        self.set(base, dst, v);
                    }
                    other => {
                        return Err(Trap::TypeConfusion {
                            op: "field access",
                            found: other.type_name(),
                        })
                    }
                },

                Op::SetField { obj, index, src } => {
                    let value = self.get(base, src);
                    match self.get(base, obj) {
                        Value::Struct(s) => {
                            s.fields.borrow_mut()[index as usize] = value;
                        }
                        other => {
                            return Err(Trap::TypeConfusion {
                                op: "field assignment",
                                found: other.type_name(),
                            })
                        }
                    }
                }

                Op::Call { dst, func: callee, base: arg_base, argc } => {
                    self.call(callee, base, arg_base, argc, dst)?;
                }

                Op::CallNative { dst, native, base: arg_base, argc } => {
                    let result = self.call_native(native, base, arg_base, argc)?;
                    self.set(base, dst, result);
                }

                Op::Return { src } => {
                    let v = src.map(|r| self.get(base, r)).unwrap_or(Value::Unit);
                    if self.pop_frame(v) {
                        return Ok(());
                    }
                }

                Op::Unreachable => {
                    return Err(Trap::Unreachable {
                        function: proto.name.clone(),
                        pc,
                    })
                }
            }
        }
    }

    // ---- frames -----------------------------------------------------------

    fn call(
        &mut self,
        callee: u32,
        base: usize,
        arg_base: Reg,
        argc: u8,
        dst: Reg,
    ) -> Result<(), Trap> {
        if self.frames.len() >= MAX_FRAMES {
            return Err(Trap::CallDepthExceeded);
        }

        let proto = self.chunk.function(callee);
        let new_base = self.regs.len();
        self.regs
            .resize(new_base + proto.frame_size, Value::Unit);

        // Copy arguments from the caller's window into the callee's parameter
        // registers, which are always locals 0..argc.
        for i in 0..argc as usize {
            let v = self.regs[base + arg_base as usize + i].clone();
            self.regs[new_base + i] = v;
        }

        self.frames.push(Frame {
            func: callee,
            pc: 0,
            base: new_base,
            ret_slot: base + dst as usize,
        });
        Ok(())
    }

    /// Returns true when the last frame was popped and execution is complete.
    fn pop_frame(&mut self, value: Value) -> bool {
        let frame = self.frames.pop().expect("a frame to pop");
        self.regs.truncate(frame.base);
        if self.frames.is_empty() {
            return true;
        }
        self.regs[frame.ret_slot] = value;
        false
    }

    fn call_native(
        &mut self,
        native: Native,
        base: usize,
        arg_base: Reg,
        argc: u8,
    ) -> Result<Value, Trap> {
        match native {
            Native::IoPrint => {
                let v = if argc == 0 {
                    Value::Unit
                } else {
                    self.regs[base + arg_base as usize].clone()
                };
                // A closed pipe is the host's problem, not the program's, so it
                // is not a trap.
                let _ = writeln!(self.out, "{}", v);
                Ok(Value::Unit)
            }
        }
    }

    // ---- register file ----------------------------------------------------

    #[inline]
    fn get(&self, base: usize, r: Reg) -> Value {
        self.regs[base + r as usize].clone()
    }

    #[inline]
    fn set(&mut self, base: usize, r: Reg, v: Value) {
        self.regs[base + r as usize] = v;
    }
}

#[cfg(test)]
mod tests;
