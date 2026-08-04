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
    /// A map. Entries are kept in insertion order, which the specification
    /// guarantees: Go randomises map iteration to stop people relying on it,
    /// and Kite instead promises an order, which is cheaper to reason about and
    /// removes a class of nondeterministic test failure.
    Map(Rc<Vec<(Value, Value)>>),
    /// A tuple: a positional record, read by index like a struct's fields.
    Tuple(Rc<Vec<Value>>),
    /// An enum value: which variant, and its payload.
    Enum(Rc<EnumValue>),
    /// A closure: the lifted function, and the values it captured. Captures
    /// are taken when the closure is made, so this holds values rather than a
    /// reference to the frame it came from — which is why a closure outliving
    /// its enclosing call is not a problem.
    Closure(Rc<ClosureValue>),
    /// A slice. Copy-on-write: assignment shares the buffer, and a mutation
    /// clones it first if anyone else is holding it. That is what gives `[T]`
    /// value semantics without copying on every assignment, and what keeps it
    /// `Share` when `T` is.
    Slice(Rc<Vec<Value>>),
    /// The result of a fallible function. The value slot holds `Nil` on the
    /// error path — but the taint analysis has already proved no program can
    /// read it there, so it is never observed.
    Pair(Rc<(Value, Value)>),
    /// An error value: a message. Kite errors are ordinary values.
    Err(Rc<str>),
    /// `nil`, or a present optional. Only ever produced where the type is
    /// `?T`; Kite has no null anywhere else.
    Nil,
}

#[derive(Debug)]
pub struct ClosureValue {
    pub func: u32,
    pub captures: Vec<Value>,
}

#[derive(Debug)]
pub struct StructValue {
    pub struct_id: u32,
    pub fields: RefCell<Vec<Value>>,
}

#[derive(Debug)]
pub struct EnumValue {
    pub enum_id: u32,
    pub variant: u32,
    pub fields: Vec<Value>,
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
            (Value::Enum(a), Value::Enum(b)) => {
                a.enum_id == b.enum_id && a.variant == b.variant && a.fields == b.fields
            }
            (Value::Slice(a), Value::Slice(b)) => a == b,
            (Value::Tuple(a), Value::Tuple(b)) => a == b,
            (Value::Map(a), Value::Map(b)) => a == b,
            (Value::Pair(a), Value::Pair(b)) => a == b,
            (Value::Err(a), Value::Err(b)) => a == b,
            (Value::Nil, Value::Nil) => true,
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
            Value::Enum(_) => "enum",
            Value::Slice(_) => "slice",
            Value::Tuple(_) => "tuple",
            Value::Map(_) => "map",
            Value::Pair(_) => "(T, error)",
            Value::Err(_) => "error",
            Value::Closure(_) => "closure",
            Value::Nil => "nil",
        }
    }
}

/// The width of one character with no font to ask. Matches the generated
/// glue's default, so the two backends measure text the same way under test.
const NOMINAL_ADVANCE: f64 = 8.0;

/// Every string operation, on characters rather than bytes.
///
/// Rust indexes strings by byte and Kite by character, so each of these walks
/// the `char` iterator. That is linear where a byte index would be constant,
/// and it is the cost of a string being text rather than storage.
fn string_op(
    op: kite_hir::StrKind,
    s: &str,
    arg: impl Fn(u8) -> Value,
) -> Result<Value, Trap> {
    use kite_hir::StrKind;
    let chars: Vec<char> = s.chars().collect();
    Ok(match op {
        StrKind::Len => Value::Int(chars.len() as i64),
        StrKind::Trim => Value::Str(Rc::from(s.trim())),
        StrKind::IndexOf => {
            let Value::Str(needle) = arg(1) else {
                return Err(Trap::TypeConfusion { op: "str.index_of", found: "not a str" });
            };
            // A byte offset from `find` means nothing to a caller counting
            // characters, so it is converted rather than returned.
            Value::Int(match s.find(needle.as_ref()) {
                Some(byte) => s[..byte].chars().count() as i64,
                None => -1,
            })
        }
        StrKind::Slice => {
            let (Value::Int(from), Value::Int(to)) = (arg(1), arg(2)) else {
                return Err(Trap::TypeConfusion { op: "str.slice", found: "not an int" });
            };
            // Clamped rather than trapping: an out-of-range slice is an
            // ordinary condition in text processing, unlike an out-of-range
            // index into a slice, which is a bug.
            let len = chars.len() as i64;
            let from = from.clamp(0, len) as usize;
            let to = to.clamp(from as i64, len) as usize;
            Value::Str(Rc::from(chars[from..to].iter().collect::<String>().as_str()))
        }
        StrKind::CodeAt => {
            let Value::Int(at) = arg(1) else {
                return Err(Trap::TypeConfusion { op: "str.code_at", found: "not an int" });
            };
            // -1 past the end rather than a trap, for the same reason `slice`
            // clamps: reading past the end is an ordinary condition in text
            // processing, and a loop that tests the answer is what a caller
            // writes anyway.
            match usize::try_from(at).ok().and_then(|i| chars.get(i)) {
                Some(c) => Value::Int(*c as i64),
                None => Value::Int(-1),
            }
        }
    })
}

/// The line height with no font to ask. Matches the generated glue's default.
const NOMINAL_LINE_HEIGHT: f64 = 16.0;

/// `f64` to `i64` the way Wasm's `i64.trunc_sat_f64_s` does: towards zero,
/// clamped at the ends, and zero for NaN. Rust's `as` already has exactly this
/// behaviour, which is why this is a named function rather than a comment.
fn saturating_trunc(f: f64) -> i64 {
    f as i64
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
            // A closure has no text form; `io.print` rejects one long before
            // this, so this only ever appears in a debug dump.
            Value::Closure(c) => write!(f, "closure#{}", c.func),
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
            Value::Enum(e) => {
                write!(f, "#{}", e.variant)?;
                if e.fields.is_empty() {
                    return Ok(());
                }
                write!(f, "(")?;
                for (i, v) in e.fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            }
            Value::Slice(items) => {
                write!(f, "[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            Value::Pair(p) => write!(f, "({}, {})", p.0, p.1),
            Value::Err(m) => write!(f, "{}", m),
            Value::Tuple(items) => {
                write!(f, "(")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            }
            Value::Map(entries) => {
                write!(f, "{{")?;
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", k, v)?;
                }
                write!(f, "}}")
            }
            Value::Nil => write!(f, "nil"),
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
    /// An index outside a slice. A program bug, not a runtime condition, so it
    /// traps — `.get()` is the form for the case where it genuinely is one.
    IndexOutOfRange { index: i64, len: usize },
    /// A register held a value of the wrong type. Only reachable through a
    /// codegen bug, since the type checker has already run.
    TypeConfusion { op: &'static str, found: &'static str },
    /// `assert` or `require` found its claim false.
    Failed { message: String },
    /// A host function the program declared and the embedder did not supply.
    /// The bytecode target has no host of its own — no DOM, no network — so
    /// this is a statement about where the program is running, not a bug.
    NoHostFunction { name: String },
    /// Every remaining task is waiting and none of them can ever run. A
    /// program that awaits a task nothing will complete has a bug the
    /// scheduler can see but the compiler cannot.
    Deadlock { waiting: usize },
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
            Trap::Failed { message } => write!(f, "{}", message),
            Trap::NoHostFunction { name } => write!(
                f,
                "`{}` is a host function, and this runtime supplies no host",
                name
            ),
            Trap::Deadlock { waiting } => write!(
                f,
                "{} task{} can never make progress",
                waiting,
                if *waiting == 1 { "" } else { "s" }
            ),
            Trap::IndexOutOfRange { index, len } => write!(
                f,
                "index {} is out of range for a slice of length {}",
                index, len
            ),
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
    run_with_host(chunk, out, None)
}

/// Run a program with a host supplying its `extern` declarations.
pub fn run_with_host<'a>(
    chunk: &'a Chunk,
    out: &'a mut dyn Write,
    host: Option<&'a mut dyn Host>,
) -> Result<(), Trap> {
    let entry = chunk.entry.ok_or(Trap::NoEntryPoint)?;
    let mut vm = Vm {
        chunk,
        out,
        regs: Vec::new(),
        frames: Vec::new(),
        tasks: Vec::new(),
        clock: 0,
        wake_request: None,
        park_request: false,
        host_wait_request: false,
        floor: 0,
        result: Value::Unit,
        host,
        font_scale: 1.0,
    };
    vm.execute(entry)?;
    // `main` returning is not the program ending: a task it started is still
    // the program's work, and dropping it on the floor would make `async`
    // silently lossy. The scheduler runs until nothing is left.
    vm.drive()
}

/// The message a test's result carries, or `None` when it passed.
///
/// A test returns `(int, error)`; the error slot is what says whether it
/// passed. A bare `error` is accepted too, for a test written that way.
pub fn failure_message(value: &Value) -> Option<String> {
    match value {
        Value::Pair(p) => failure_message(&p.1),
        Value::Err(message) => Some(message.to_string()),
        _ => None,
    }
}

/// Run one function by name and hand back what it returned.
///
/// This is what `kitec test` drives: a test is an ordinary function, found by
/// its name, and its result is an `error` the runner reports.
pub fn run_function(chunk: &Chunk, name: &str, out: &mut dyn Write) -> Result<Value, Trap> {
    let index = chunk
        .functions
        .iter()
        .position(|f| f.name == name)
        .ok_or(Trap::NoEntryPoint)? as u32;
    let mut vm = Vm {
        chunk,
        out,
        regs: Vec::new(),
        frames: Vec::new(),
        tasks: Vec::new(),
        clock: 0,
        wake_request: None,
        park_request: false,
        host_wait_request: false,
        floor: 0,
        result: Value::Unit,
        host: None,
        font_scale: 1.0,
    };
    vm.execute(index)?;
    vm.drive()?;
    Ok(std::mem::replace(&mut vm.result, Value::Unit))
}

struct Vm<'a> {
    chunk: &'a Chunk,
    out: &'a mut dyn Write,
    regs: Vec<Value>,
    frames: Vec<Frame>,
    /// Tasks that have been started and not yet finished, in the order they
    /// were spawned. A task is a closure the scheduler polls; the state
    /// machine behind it is ordinary compiled code.
    tasks: Vec<Scheduled>,
    /// A **virtual** clock, in milliseconds. Nothing here waits on wall time:
    /// when every task is blocked on a deadline, the clock jumps to the
    /// earliest one. That makes `sleep` free in tests and, more importantly,
    /// makes the bytecode and Wasm backends produce the same ordering — a
    /// scheduler that raced real timers could not be differentially tested.
    clock: i64,
    /// Set by `task.wake_at` during a poll, and read once it returns.
    wake_request: Option<i64>,
    /// Set by `task.park`: this task is waiting for some other one to finish,
    /// and there is no deadline that would make polling it worthwhile.
    park_request: bool,
    /// Set by `task.wait_host`: this task is waiting on the host — a fetch, a
    /// file, something outside the program entirely.
    host_wait_request: bool,
    /// Frame depth at which the current nested run stops. Polling a task runs
    /// its closure to its own return, not to the bottom of the stack.
    floor: usize,
    /// The value the frame at the floor returned.
    result: Value,
    /// What answers a call across the host boundary, if anything does.
    host: Option<&'a mut dyn Host>,
    /// The font `draw.font` last selected, as a multiple of the nominal size.
    /// A backend with no font at all still has to *scale* with one, or a
    /// layout measured at 22dp here and at 22dp in a browser would disagree
    /// about everything but the default.
    font_scale: f64,
}

/// What a program's `extern` declarations reach.
///
/// The bytecode VM is the embedding target, and this is what embedding means:
/// a host supplies the functions a program declared. A name it does not know
/// is a trap that says so rather than a silent zero.
pub trait Host {
    fn call(&mut self, name: &str, args: &[Value]) -> Result<Value, Trap>;

    /// Give the host a turn when every task is waiting on it.
    ///
    /// Returns whether anything happened. A host with nothing outstanding
    /// answers `false`, and the scheduler reports a deadlock rather than
    /// spinning — which is the truth: nothing was ever going to arrive.
    fn wait(&mut self) -> Result<bool, Trap> {
        Ok(false)
    }
}

/// One live task: how to resume it, and when it is worth trying again.
struct Scheduled {
    poll: Value,
    /// The earliest clock reading at which polling could achieve anything.
    wake_at: Option<i64>,
    /// Waiting on another task rather than on the clock.
    parked: bool,
    /// Waiting on the host rather than on anything in the program.
    waiting_on_host: bool,
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
        self.execute_frames()
    }

    /// The dispatch loop, running until the frame this run started at returns.
    fn execute_frames(&mut self) -> Result<(), Trap> {
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
                Op::LoadNil { dst } => self.set(base, dst, Value::Nil),
                Op::NewPair { dst, value, error } => {
                    let v = self.get(base, value);
                    let e = self.get(base, error);
                    self.set(base, dst, Value::Pair(Rc::new((v, e))));
                }
                Op::PairValue { dst, obj } => match self.get(base, obj) {
                    Value::Pair(p) => {
                        let v = p.0.clone();
                        self.set(base, dst, v);
                    }
                    other => {
                        return Err(Trap::TypeConfusion {
                            op: "value of a fallible result",
                            found: other.type_name(),
                        })
                    }
                },
                Op::PairError { dst, obj } => match self.get(base, obj) {
                    Value::Pair(p) => {
                        let e = p.1.clone();
                        self.set(base, dst, e);
                    }
                    other => {
                        return Err(Trap::TypeConfusion {
                            op: "error of a fallible result",
                            found: other.type_name(),
                        })
                    }
                },
                Op::NewError { dst, message } => {
                    let m = match self.get(base, message) {
                        Value::Str(s) => s,
                        other => {
                            return Err(Trap::TypeConfusion {
                                op: "errors.new",
                                found: other.type_name(),
                            })
                        }
                    };
                    self.set(base, dst, Value::Err(m));
                }
                Op::ErrorMessage { dst, obj } => match self.get(base, obj) {
                    Value::Err(m) => self.set(base, dst, Value::Str(m)),
                    Value::Nil => self.set(base, dst, Value::Str(Rc::from(""))),
                    other => {
                        return Err(Trap::TypeConfusion {
                            op: "message",
                            found: other.type_name(),
                        })
                    }
                },
                Op::IsNil { dst, obj } => {
                    let v = matches!(self.get(base, obj), Value::Nil);
                    self.set(base, dst, Value::Bool(v));
                }
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
                // The release forms. Overflow is defined here rather than
                // undefined: it wraps, which is what the hardware does and what
                // section 3.1 promises.
                Op::AddIntWrap { dst, a, b } => arith!(self, base, dst, a, b, "+", Int, Int, |x: i64,
                                                                                              y: i64| {
                    Ok(x.wrapping_add(y))
                }),
                Op::SubIntWrap { dst, a, b } => arith!(self, base, dst, a, b, "-", Int, Int, |x: i64,
                                                                                              y: i64| {
                    Ok(x.wrapping_sub(y))
                }),
                Op::MulIntWrap { dst, a, b } => arith!(self, base, dst, a, b, "*", Int, Int, |x: i64,
                                                                                              y: i64| {
                    Ok(x.wrapping_mul(y))
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
                    // An enum payload and a tuple element are read the same
                    // way; the checker has already proved the shape.
                    Value::Enum(e) => {
                        let v = e.fields[index as usize].clone();
                        self.set(base, dst, v);
                    }
                    Value::Tuple(items) => {
                        let v = items[index as usize].clone();
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

                Op::NewEnum { dst, enum_id, variant, base: arg_base, count } => {
                    let mut fields = Vec::with_capacity(count as usize);
                    for i in 0..count as usize {
                        fields.push(self.regs[base + arg_base as usize + i].clone());
                    }
                    self.set(
                        base,
                        dst,
                        Value::Enum(Rc::new(EnumValue { enum_id, variant, fields })),
                    );
                }

                Op::TagOf { dst, obj } => match self.get(base, obj) {
                    Value::Enum(e) => self.set(base, dst, Value::Int(e.variant as i64)),
                    other => {
                        return Err(Trap::TypeConfusion {
                            op: "match",
                            found: other.type_name(),
                        })
                    }
                },

                Op::NewTuple { dst, base: arg_base, count } => {
                    let mut items = Vec::with_capacity(count as usize);
                    for i in 0..count as usize {
                        items.push(self.regs[base + arg_base as usize + i].clone());
                    }
                    self.set(base, dst, Value::Tuple(Rc::new(items)));
                }

                Op::NewMap { dst, base: arg_base, count } => {
                    let mut entries: Vec<(Value, Value)> = Vec::new();
                    let mut i = 0;
                    while i + 1 < count as usize {
                        let k = self.regs[base + arg_base as usize + i].clone();
                        let v = self.regs[base + arg_base as usize + i + 1].clone();
                        // A repeated key keeps its original position, so
                        // insertion order stays well defined.
                        match entries.iter_mut().find(|(ek, _)| *ek == k) {
                            Some(slot) => slot.1 = v,
                            None => entries.push((k, v)),
                        }
                        i += 2;
                    }
                    self.set(base, dst, Value::Map(Rc::new(entries)));
                }

                Op::MapGet { dst, obj, key } => {
                    let (m, k) = (self.get(base, obj), self.get(base, key));
                    let Value::Map(entries) = m else {
                        return Err(Trap::TypeConfusion { op: "map index", found: m.type_name() });
                    };
                    let v = entries
                        .iter()
                        .find(|(ek, _)| *ek == k)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(Value::Nil);
                    self.set(base, dst, v);
                }

                Op::MapSet { obj, key, src } => {
                    let (k, v) = (self.get(base, key), self.get(base, src));
                    let slot = base + obj as usize;
                    let Value::Map(entries) = &mut self.regs[slot] else {
                        return Err(Trap::TypeConfusion {
                            op: "map assignment",
                            found: self.regs[slot].type_name(),
                        });
                    };
                    let entries = Rc::make_mut(entries);
                    match entries.iter_mut().find(|(ek, _)| *ek == k) {
                        Some(existing) => existing.1 = v,
                        None => entries.push((k, v)),
                    }
                }

                Op::MapLen { dst, obj } => {
                    let m = self.get(base, obj);
                    let Value::Map(entries) = m else {
                        return Err(Trap::TypeConfusion { op: "len", found: m.type_name() });
                    };
                    self.set(base, dst, Value::Int(entries.len() as i64));
                }

                Op::CallExtern { dst, index, base: arg_base, argc } => {
                    let name = self.chunk.externs[index as usize].clone();
                    let args: Vec<Value> = (0..argc as usize)
                        .map(|i| self.regs[base + arg_base as usize + i].clone())
                        .collect();
                    let value = match self.host.as_mut() {
                        Some(h) => h.call(&name, &args)?,
                        None => return Err(Trap::NoHostFunction { name }),
                    };
                    self.set(base, dst, value);
                }

                // Ordering on strings, by code point: Rust's `str` ordering is
                // exactly that, and the glue's is too, so the two backends
                // sort the same way.
                Op::LtStr { dst, a, b }
                | Op::LeStr { dst, a, b }
                | Op::GtStr { dst, a, b }
                | Op::GeStr { dst, a, b } => {
                    let (x, y) = (self.get(base, a), self.get(base, b));
                    let (Value::Str(x), Value::Str(y)) = (&x, &y) else {
                        return Err(Trap::TypeConfusion {
                            op: "string comparison",
                            found: x.type_name(),
                        });
                    };
                    let ordering = x.as_ref().cmp(y.as_ref());
                    let result = match proto.code[pc] {
                        Op::LtStr { .. } => ordering.is_lt(),
                        Op::LeStr { .. } => ordering.is_le(),
                        Op::GtStr { .. } => ordering.is_gt(),
                        _ => ordering.is_ge(),
                    };
                    self.set(base, dst, Value::Bool(result));
                }

                Op::MapKeys { dst, obj } | Op::MapValues { dst, obj } => {
                    let keys = matches!(proto.code[pc], Op::MapKeys { .. });
                    let m = self.get(base, obj);
                    let Value::Map(entries) = m else {
                        return Err(Trap::TypeConfusion {
                            op: "keys",
                            found: m.type_name(),
                        });
                    };
                    // Insertion order, which the specification guarantees, so
                    // `keys` and `values` line up element for element.
                    let items: Vec<Value> = entries
                        .iter()
                        .map(|(k, v)| if keys { k.clone() } else { v.clone() })
                        .collect();
                    self.set(base, dst, Value::Slice(Rc::new(items)));
                }

                Op::NewSlice { dst, base: arg_base, count } => {
                    let mut items = Vec::with_capacity(count as usize);
                    for i in 0..count as usize {
                        items.push(self.regs[base + arg_base as usize + i].clone());
                    }
                    self.set(base, dst, Value::Slice(Rc::new(items)));
                }

                Op::GetIndex { dst, obj, index } => {
                    let (seq, i) = (self.get(base, obj), self.get(base, index));
                    let items = Self::as_slice(&seq)?;
                    let i = Self::as_index(&i)?;
                    match usize::try_from(i).ok().and_then(|u| items.get(u)) {
                        Some(v) => {
                            let v = v.clone();
                            self.set(base, dst, v);
                        }
                        None => {
                            return Err(Trap::IndexOutOfRange { index: i, len: items.len() })
                        }
                    }
                }

                Op::SetIndex { obj, index, src } => {
                    let value = self.get(base, src);
                    let i = Self::as_index(&self.get(base, index))?;
                    let slot = base + obj as usize;
                    let Value::Slice(items) = &mut self.regs[slot] else {
                        return Err(Trap::TypeConfusion {
                            op: "index assignment",
                            found: self.regs[slot].type_name(),
                        });
                    };
                    let len = items.len();
                    match usize::try_from(i).ok().filter(|u| *u < len) {
                        // Copy-on-write: clone only if the buffer is shared.
                        Some(u) => Rc::make_mut(items)[u] = value,
                        None => return Err(Trap::IndexOutOfRange { index: i, len }),
                    }
                }

                Op::SliceLen { dst, obj } => {
                    let seq = self.get(base, obj);
                    let len = Self::as_slice(&seq)?.len();
                    self.set(base, dst, Value::Int(len as i64));
                }

                Op::SliceGet { dst, obj, index } => {
                    let (seq, i) = (self.get(base, obj), self.get(base, index));
                    let items = Self::as_slice(&seq)?;
                    let i = Self::as_index(&i)?;
                    let v = usize::try_from(i)
                        .ok()
                        .and_then(|u| items.get(u))
                        .cloned()
                        .unwrap_or(Value::Nil);
                    self.set(base, dst, v);
                }

                Op::SlicePush { obj, src } => {
                    let value = self.get(base, src);
                    let slot = base + obj as usize;
                    let Value::Slice(items) = &mut self.regs[slot] else {
                        return Err(Trap::TypeConfusion {
                            op: "push",
                            found: self.regs[slot].type_name(),
                        });
                    };
                    Rc::make_mut(items).push(value);
                }

                Op::Call { dst, func: callee, base: arg_base, argc } => {
                    self.call(callee, base, arg_base, argc, dst)?;
                }

                Op::Closure { dst, func, base: arg_base, count } => {
                    let captures = (0..count as usize)
                        .map(|i| self.get(base, arg_base + i as Reg))
                        .collect();
                    self.set(base, dst, Value::Closure(Rc::new(ClosureValue { func, captures })));
                }

                Op::CallClosure { dst, callee, base: arg_base, argc } => {
                    let Value::Closure(c) = self.get(base, callee) else {
                        return Err(Trap::TypeConfusion {
                            op: "call.closure",
                            found: self.get(base, callee).type_name(),
                        });
                    };
                    self.call_with(c.func, base, arg_base, argc, dst, &c.captures)?;
                }

                Op::StrOp { dst, op, base: arg_base, argc } => {
                    let a = |i: u8| self.get(base, arg_base + i as Reg);
                    let subject = a(0);
                    let Value::Str(s) = &subject else {
                        return Err(Trap::TypeConfusion {
                            op: op.name(),
                            found: subject.type_name(),
                        });
                    };
                    let _ = argc;
                    let out = string_op(op, s, a)?;
                    self.set(base, dst, out);
                }

                Op::IntToFloat { dst, src } => {
                    let v = self.get(base, src);
                    let out = match v {
                        Value::Int(i) => Value::Float(i as f64),
                        // Already a float: writing the cast anyway is allowed.
                        Value::Float(f) => Value::Float(f),
                        other => {
                            return Err(Trap::TypeConfusion {
                                op: "as float",
                                found: other.type_name(),
                            })
                        }
                    };
                    self.set(base, dst, out);
                }

                Op::FloatToInt { dst, src } => {
                    let v = self.get(base, src);
                    let out = match v {
                        // Truncation towards zero, and saturating rather than
                        // trapping — the same rule Wasm's `trunc_sat` follows,
                        // so both backends agree on every input including NaN.
                        Value::Float(f) => Value::Int(saturating_trunc(f)),
                        Value::Int(i) => Value::Int(i),
                        other => {
                            return Err(Trap::TypeConfusion {
                                op: "as int",
                                found: other.type_name(),
                            })
                        }
                    };
                    self.set(base, dst, out);
                }

                Op::ToStr { dst, src } => {
                    // `Display` on a `Value` is the same code that `io.print`
                    // uses, so `io.print(x)` and `io.print("\(x)")` cannot
                    // disagree about how a value looks.
                    let text = self.get(base, src).to_string();
                    self.set(base, dst, Value::Str(Rc::from(text.as_str())));
                }

                Op::CallVirtual { dst, table, method, base: arg_base, argc } => {
                    // The receiver is the first argument. Its own tag chooses
                    // the body, which is the whole of dynamic dispatch: the
                    // value carries its type, so nothing extra is stored.
                    let receiver = self.get(base, arg_base);
                    let tag = match &receiver {
                        Value::Struct(s) => s.struct_id,
                        Value::Enum(e) => 0x8000_0000 | e.enum_id,
                        other => {
                            return Err(Trap::TypeConfusion {
                                op: "call.virtual",
                                found: other.type_name(),
                            })
                        }
                    };
                    let Some(callee) = self.chunk.vtables[table as usize].lookup(tag, method)
                    else {
                        return Err(Trap::TypeConfusion {
                            op: "call.virtual",
                            found: receiver.type_name(),
                        });
                    };
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
        self.call_with(callee, base, arg_base, argc, dst, &[])
    }

    /// `leading` occupies the callee's first registers, ahead of the arguments.
    /// That is how a closure's captures arrive: a lifted function takes them as
    /// its leading parameters, so it is entered exactly like any other.
    fn call_with(
        &mut self,
        callee: u32,
        base: usize,
        arg_base: Reg,
        argc: u8,
        dst: Reg,
        leading: &[Value],
    ) -> Result<(), Trap> {
        if self.frames.len() >= MAX_FRAMES {
            return Err(Trap::CallDepthExceeded);
        }

        let proto = self.chunk.function(callee);
        let new_base = self.regs.len();
        self.regs
            .resize(new_base + proto.frame_size, Value::Unit);

        for (i, v) in leading.iter().enumerate() {
            self.regs[new_base + i] = v.clone();
        }
        // Arguments follow, in the callee's parameter registers.
        for i in 0..argc as usize {
            let v = self.regs[base + arg_base as usize + i].clone();
            self.regs[new_base + leading.len() + i] = v;
        }

        self.frames.push(Frame {
            func: callee,
            pc: 0,
            base: new_base,
            ret_slot: base + dst as usize,
        });
        Ok(())
    }

    /// Returns true when the frame that just returned was the one this run
    /// started at — the bottom of the stack for a program, or the poll of a
    /// task for the scheduler.
    fn pop_frame(&mut self, value: Value) -> bool {
        let frame = self.frames.pop().expect("a frame to pop");
        self.regs.truncate(frame.base);
        if self.frames.len() <= self.floor {
            self.result = value;
            return true;
        }
        self.regs[frame.ret_slot] = value;
        false
    }

    // ---- the scheduler ----------------------------------------------------

    /// Poll every live task until none is left.
    ///
    /// Round-robin, in spawn order, which is what makes the interleaving
    /// deterministic and comparable against the other backend. A task that
    /// cannot run yet is skipped; when *nothing* can run, the clock jumps to
    /// the earliest deadline rather than waiting for it, so a program that
    /// sleeps costs no real time under test.
    fn drive(&mut self) -> Result<(), Trap> {
        while !self.tasks.is_empty() {
            let mut polled = false;
            let mut completed = false;
            let mut i = 0;
            while i < self.tasks.len() {
                if self.tasks[i].parked
                    || self.tasks[i].waiting_on_host
                    || self.tasks[i].wake_at.is_some_and(|w| w > self.clock)
                {
                    i += 1;
                    continue;
                }
                polled = true;
                self.tasks[i].wake_at = None;
                self.wake_request = None;
                self.park_request = false;
                self.host_wait_request = false;
                let poll = self.tasks[i].poll.clone();
                let done = self.poll_task(poll)?;
                if i < self.tasks.len() {
                    self.tasks[i].wake_at = self.wake_request.take();
                    self.tasks[i].parked = std::mem::take(&mut self.park_request);
                    self.tasks[i].waiting_on_host =
                        std::mem::take(&mut self.host_wait_request);
                }
                if done {
                    self.tasks.remove(i);
                    completed = true;
                } else {
                    i += 1;
                }
            }
            // A task finishing is what a parked task is waiting for, and may
            // be what a sleeping one wanted too — so a completion wakes
            // everything and lets each decide for itself.
            if completed {
                for t in 0..self.tasks.len() {
                    self.tasks[t].parked = false;
                    self.tasks[t].wake_at = None;
                }
            }
            if !polled {
                // Everything is waiting. Give the host its turn first — a
                // fetch or a file is something only it can finish — then the
                // clock; and if nothing is waiting on either, nothing will
                // ever happen.
                let on_host = self.tasks.iter().any(|t| t.waiting_on_host);
                if on_host {
                    let progressed = match self.host.as_mut() {
                        Some(h) => h.wait()?,
                        None => false,
                    };
                    if progressed {
                        for t in 0..self.tasks.len() {
                            self.tasks[t].waiting_on_host = false;
                        }
                        continue;
                    }
                }
                match self.tasks.iter().filter_map(|t| t.wake_at).min() {
                    Some(next) if next > self.clock => {
                        self.clock = next;
                        for t in 0..self.tasks.len() {
                            self.tasks[t].waiting_on_host = false;
                        }
                    }
                    _ => return Err(Trap::Deadlock { waiting: self.tasks.len() }),
                }
            }
        }
        Ok(())
    }

    /// Run one task's resume closure to its own return.
    fn poll_task(&mut self, poll: Value) -> Result<bool, Trap> {
        let Value::Closure(c) = &poll else {
            return Err(Trap::TypeConfusion { op: "task.poll", found: poll.type_name() });
        };
        let (func, captures) = (c.func, c.captures.clone());
        let saved_floor = self.floor;
        self.floor = self.frames.len();
        // The closure's captures are its leading parameters, exactly as at any
        // other call site; a resume function takes one, its frame.
        let base = self.regs.len();
        self.call_with(func, base, 0, 0, 0, &captures)?;
        self.execute_frames()?;
        self.floor = saved_floor;
        match std::mem::replace(&mut self.result, Value::Unit) {
            Value::Bool(b) => Ok(b),
            other => Err(Trap::TypeConfusion { op: "task.poll", found: other.type_name() }),
        }
    }

    fn call_native(
        &mut self,
        native: Native,
        base: usize,
        arg_base: Reg,
        argc: u8,
    ) -> Result<Value, Trap> {
        match native {
            // The VM has no window. Writing each call out is what lets the
            // differential suite compare drawing against WebAssembly at all,
            // and what makes a layout testable without a browser.
            Native::DrawRect => {
                let a = |i: usize| self.regs[base + arg_base as usize + i].clone();
                let _ = writeln!(
                    self.out,
                    "rect {} {} {} {} {}",
                    a(0),
                    a(1),
                    a(2),
                    a(3),
                    a(4)
                );
                Ok(Value::Unit)
            }
            Native::DrawRRect => {
                let a = |i: usize| self.regs[base + arg_base as usize + i].clone();
                let _ = writeln!(
                    self.out,
                    "rrect {} {} {} {} {} {}",
                    a(0),
                    a(1),
                    a(2),
                    a(3),
                    a(4),
                    a(5)
                );
                Ok(Value::Unit)
            }
            Native::DrawDRRect => {
                let a = |i: usize| self.regs[base + arg_base as usize + i].clone();
                let _ = writeln!(
                    self.out,
                    "drrect {} {} {} {} {} {} {}",
                    a(0),
                    a(1),
                    a(2),
                    a(3),
                    a(4),
                    a(5),
                    a(6)
                );
                Ok(Value::Unit)
            }
            Native::DrawAlpha => {
                let a = |i: usize| self.regs[base + arg_base as usize + i].clone();
                // Written, unlike a font selection: alpha changes the pixels a
                // call produces and shows up nowhere else in the transcript, so
                // a backend that ignored it would otherwise agree with one that
                // did not.
                let _ = writeln!(self.out, "alpha {}", a(0));
                Ok(Value::Unit)
            }
            Native::DrawFont => {
                let a = |i: usize| self.regs[base + arg_base as usize + i].clone();
                let size = match a(0) {
                    Value::Float(f) => f,
                    _ => NOMINAL_LINE_HEIGHT,
                };
                self.font_scale = size / NOMINAL_LINE_HEIGHT;
                // Selecting a font writes nothing. It is state, and unlike a
                // clip it changes no pixel by itself — its whole effect is on
                // the runs of text that follow, which is where a backend that
                // got it wrong shows up: the text lands somewhere else. It
                // also has to stay silent because *measurement* selects a
                // font, and `ui.layout` must not write drawing calls.
                Ok(Value::Unit)
            }
            Native::DrawText => {
                let a = |i: usize| self.regs[base + arg_base as usize + i].clone();
                let _ = writeln!(self.out, "text {} {} {} {}", a(0), a(1), a(2), a(3));
                Ok(Value::Unit)
            }

            // A field is a *region*, and a transcript renderer has no regions —
            // so it writes what the field says, the way it writes what a label
            // says. That is also what the canvas renderer does, and the reason
            // this call degrades rather than failing where it cannot be
            // honoured: only a DOM has a real `<input>` to become.
            Native::DrawField => {
                let a = |i: usize| self.regs[base + arg_base as usize + i].clone();
                let _ = writeln!(
                    self.out,
                    "field {} {} {} {} {} {} {}",
                    a(0), a(1), a(2), a(3), a(4), a(5), a(6)
                );
                Ok(Value::Unit)
            }

            // No font here, so the nominal advance stands. This is the same
            // number the generated glue uses by default, which is what keeps a
            // layout comparable across backends under test — a browser installs
            // a real measurer and gets different, better numbers.
            Native::TextWidth => {
                let v = self.regs[base + arg_base as usize].clone();
                let Value::Str(s) = &v else {
                    return Err(Trap::TypeConfusion {
                        op: "text.width",
                        found: v.type_name(),
                    });
                };
                Ok(Value::Float(s.chars().count() as f64 * NOMINAL_ADVANCE * self.font_scale))
            }

            Native::TextHeight => Ok(Value::Float(NOMINAL_LINE_HEIGHT * self.font_scale)),

            Native::DrawClip => {
                let a = |i: usize| self.regs[base + arg_base as usize + i].clone();
                let _ = writeln!(self.out, "clip {} {} {} {}", a(0), a(1), a(2), a(3));
                Ok(Value::Unit)
            }
            Native::DrawUnclip => {
                let _ = writeln!(self.out, "unclip");
                Ok(Value::Unit)
            }

            // A task's resume closure, handed over by the state machine the
            // compiler wrote. The scheduler owns it from here.
            Native::TaskSpawn => {
                let poll = self.regs[base + arg_base as usize].clone();
                self.tasks.push(Scheduled {
                    poll,
                    wake_at: None,
                    parked: false,
                    waiting_on_host: false,
                });
                Ok(Value::Unit)
            }
            Native::TaskWakeAt => {
                let Value::Int(ms) = self.regs[base + arg_base as usize] else {
                    return Err(Trap::TypeConfusion {
                        op: "task.wake_at",
                        found: "not an int",
                    });
                };
                // A hint, not a promise. The code that asked re-checks the
                // clock, so an early poll is harmless and a late one only
                // costs time.
                self.wake_request = Some(ms);
                Ok(Value::Unit)
            }
            Native::TaskPark => {
                self.park_request = true;
                Ok(Value::Unit)
            }
            Native::TaskWaitHost => {
                self.host_wait_request = true;
                Ok(Value::Unit)
            }
            Native::TimeNow => Ok(Value::Int(self.clock)),

            // The checker admits only structs, enums and maps here, and each
            // of those is an `Rc` — so identity is the handle comparison the
            // representation already supports. A pair that reached this point
            // holding anything else is a compiler bug rather than a program
            // error, which is what `TypeConfusion` says.
            Native::PtrSame => {
                let a = self.regs[base + arg_base as usize].clone();
                let b = self.regs[base + arg_base as usize + 1].clone();
                let same = match (&a, &b) {
                    (Value::Struct(x), Value::Struct(y)) => Rc::ptr_eq(x, y),
                    (Value::Enum(x), Value::Enum(y)) => Rc::ptr_eq(x, y),
                    (Value::Map(x), Value::Map(y)) => Rc::ptr_eq(x, y),
                    _ => {
                        return Err(Trap::TypeConfusion {
                            op: "ptr.same",
                            found: a.type_name(),
                        })
                    }
                };
                Ok(Value::Bool(same))
            }

            // A failed claim is a trap: not catchable, and it says what was
            // claimed. Kite has no `recover`, so this ends the program.
            Native::Require => {
                let cond = self.regs[base + arg_base as usize].clone();
                if matches!(cond, Value::Bool(true)) {
                    return Ok(Value::Unit);
                }
                let message = match self.regs.get(base + arg_base as usize + 1) {
                    Some(Value::Str(s)) => s.to_string(),
                    _ => "a claim about this program does not hold".to_string(),
                };
                Err(Trap::Failed { message })
            }

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

    fn as_slice(v: &Value) -> Result<&Rc<Vec<Value>>, Trap> {
        match v {
            Value::Slice(items) => Ok(items),
            other => Err(Trap::TypeConfusion {
                op: "slice operation",
                found: other.type_name(),
            }),
        }
    }

    fn as_index(v: &Value) -> Result<i64, Trap> {
        match v {
            Value::Int(i) => Ok(*i),
            other => Err(Trap::TypeConfusion {
                op: "index",
                found: other.type_name(),
            }),
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
