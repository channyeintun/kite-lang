//! Module-level constants, worked out before any body is checked.
//!
//! A `let` at the top of a module names a value the compiler computes while
//! compiling, and every use of the name *is* that value — the path is replaced
//! by a literal in the HIR, so nothing downstream of the checker learns that
//! constants exist.
//!
//! That is the whole of the feature, and it is deliberately the whole of it.
//! The alternative — a constant initialised by running Kite at startup — needs
//! an initialisation order across modules, a rule for what a cycle means, and
//! an answer for what a program observes if it reads one during that order.
//! None of those questions has an answer a reader could guess, which is the
//! bar this language sets.
//!
//! What may appear on the right-hand side is therefore: a literal, an
//! interpolation whose holes are all constant, an operator applied to
//! constants, and another constant. Not a call — see [`codes::E0118`].

use kite_ast::{self as ast, BinaryOp, UnaryOp};
use kite_diag::{codes, DiagBag, Diagnostic};
use kite_resolve::{ResolveMap, Res};
use kite_span::{SourceMap, Span};

use crate::{decode_escapes_into, parse_float, parse_int};

/// A constant's value: the four types a constant can have.
///
/// Not slices, maps or structs. Each of those is an allocation, so a constant
/// of one would be either a fresh value per use — surprising for something
/// spelled like a name — or one shared mutable object, which is the thing
/// module-level `var` is refused for. A slice of constants is an ordinary
/// `let` inside the function that wants it.
#[derive(Clone, Debug, PartialEq)]
pub enum ConstValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl ConstValue {
    /// What this is, for a diagnostic that has to name it.
    pub fn type_name(&self) -> &'static str {
        match self {
            ConstValue::Bool(_) => "bool",
            ConstValue::Int(_) => "int",
            ConstValue::Float(_) => "float",
            ConstValue::Str(_) => "str",
        }
    }

    /// The text this value renders as inside `\( )`, where that text is the
    /// same on every target.
    ///
    /// A `float` is `None`, and that is not an oversight. The two hosts do not
    /// agree on how to write one: the native runtime formats with Rust's
    /// shortest round-trip and the browser with JavaScript's, and those differ
    /// at the exponent boundary — `1e21` against
    /// `1000000000000000000000`. Rendering here would pick one of them at
    /// compile time and hand the same program a different string depending on
    /// where it was built, which is worse than not allowing it.
    fn rendered(&self) -> Option<String> {
        match self {
            ConstValue::Bool(b) => Some(b.to_string()),
            ConstValue::Int(i) => Some(i.to_string()),
            ConstValue::Str(s) => Some(s.clone()),
            ConstValue::Float(_) => None,
        }
    }
}

/// Every module-level constant's value, by the index the resolver gave it.
///
/// An entry is `None` when evaluation failed. The diagnostic has already been
/// reported by then, so a use of it stays silent rather than reporting the
/// same mistake once per mention.
#[derive(Default, Debug)]
pub struct ConstTable {
    values: Vec<Option<ConstValue>>,
}

impl ConstTable {
    pub fn get(&self, index: u32) -> Option<&ConstValue> {
        self.values.get(index as usize)?.as_ref()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Where a constant is in its evaluation.
#[derive(Clone, PartialEq)]
enum Slot {
    /// Not looked at yet.
    Waiting,
    /// On the stack right now. Reaching one of these again is a cycle.
    Running,
    Done(Option<ConstValue>),
}

/// Work out every constant in the program.
pub fn evaluate(
    file: &ast::SourceFile,
    resolved: &ResolveMap,
    sources: &SourceMap,
    diags: &mut DiagBag,
) -> ConstTable {
    let mut ev = Eval {
        file,
        resolved,
        sources,
        diags,
        slots: vec![Slot::Waiting; resolved.consts.len()],
        stack: Vec::new(),
    };
    for i in 0..resolved.consts.len() {
        ev.value_of(i as u32);
    }
    ConstTable {
        values: ev
            .slots
            .into_iter()
            .map(|s| match s {
                Slot::Done(v) => v,
                // Nothing can still be waiting or running once the loop above
                // has asked for every one of them.
                _ => None,
            })
            .collect(),
    }
}

struct Eval<'a> {
    file: &'a ast::SourceFile,
    resolved: &'a ResolveMap,
    sources: &'a SourceMap,
    diags: &'a mut DiagBag,
    slots: Vec<Slot>,
    /// The chain of constants currently being evaluated, for the cycle report.
    stack: Vec<u32>,
}

impl<'a> Eval<'a> {
    /// One constant's value, computing it if this is the first ask.
    fn value_of(&mut self, index: u32) -> Option<ConstValue> {
        match &self.slots[index as usize] {
            Slot::Done(v) => return v.clone(),
            Slot::Running => {
                self.report_cycle(index);
                // Marked done-with-nothing so the other members of the cycle
                // report nothing further: one cycle, one diagnostic.
                self.slots[index as usize] = Slot::Done(None);
                return None;
            }
            Slot::Waiting => {}
        }

        self.slots[index as usize] = Slot::Running;
        self.stack.push(index);

        let decl_index = self.resolved.consts[index as usize].decl_index;
        let value = match &self.file.items[decl_index] {
            ast::Item::Const(c) => self.eval(&c.value),
            _ => None,
        };

        self.stack.pop();
        // A cycle already wrote `Done(None)` here; leave that alone so the
        // second visit does not resurrect a half-computed answer.
        if self.slots[index as usize] == Slot::Running {
            self.slots[index as usize] = Slot::Done(value.clone());
            return value;
        }
        None
    }

    fn report_cycle(&mut self, index: u32) {
        let start = self.stack.iter().position(|&i| i == index).unwrap_or(0);
        let chain: Vec<String> = self.stack[start..]
            .iter()
            .map(|&i| bare_name(&self.resolved.consts[i as usize].name).to_string())
            .collect();
        let here = &self.resolved.consts[index as usize];
        let name = bare_name(&here.name).to_string();
        let mut d = Diagnostic::error(
            codes::E0119,
            format!("`{}` is defined in terms of itself", name),
        )
        .with_primary(here.span, "this constant needs its own value to have a value");
        if chain.len() > 1 {
            d = d.with_note(format!("the chain is {} → {}", chain.join(" → "), name));
        }
        self.diags.push(d);
    }

    fn eval(&mut self, e: &ast::Expr) -> Option<ConstValue> {
        match e {
            ast::Expr::Int(span) => match parse_int(self.text(*span)) {
                Some(v) => Some(ConstValue::Int(v)),
                None => {
                    self.diags.push(
                        Diagnostic::error(codes::E0004, "invalid integer literal")
                            .with_primary(*span, "does not fit in `int`"),
                    );
                    None
                }
            },
            ast::Expr::Float(span) => match parse_float(self.text(*span)) {
                Some(v) => Some(ConstValue::Float(v)),
                None => {
                    self.diags.push(
                        Diagnostic::error(codes::E0004, "invalid float literal")
                            .with_primary(*span, "cannot be parsed"),
                    );
                    None
                }
            },
            ast::Expr::Bool { value, .. } => Some(ConstValue::Bool(*value)),
            ast::Expr::Str(span) => Some(ConstValue::Str(self.string_value(*span))),

            ast::Expr::Interpolated { parts, .. } => {
                let mut out = String::new();
                // Every hole is evaluated even after one has failed, so a
                // constant with two mistakes in it reports both.
                let mut ok = true;
                for part in parts {
                    match part {
                        ast::StrPart::Text(span) => {
                            let raw = self.text(*span).to_string();
                            out.push_str(&decode_escapes_into(&raw, *span, self.diags));
                        }
                        ast::StrPart::Hole(inner) => match self.eval(inner) {
                            Some(v) => match v.rendered() {
                                Some(text) => out.push_str(&text),
                                None => {
                                    ok = false;
                                    self.diags.push(
                                        Diagnostic::error(
                                            codes::E0118,
                                            "a `float` cannot be interpolated into a constant",
                                        )
                                        .with_primary(inner.span(), "this is a `float`")
                                        .with_note(
                                            "the browser and the native runtime write a float \
                                             differently at the exponent boundary, so the text \
                                             would depend on which backend built the program",
                                        )
                                        .with_note(
                                            "interpolate it where it is used, in a function, \
                                             where the running host decides",
                                        ),
                                    );
                                }
                            },
                            None => ok = false,
                        },
                    }
                }
                ok.then_some(ConstValue::Str(out))
            }

            ast::Expr::Paren { inner, .. } => self.eval(inner),

            ast::Expr::Unary { op, operand, span } => {
                let v = self.eval(operand)?;
                match (op, &v) {
                    (UnaryOp::Neg, ConstValue::Int(i)) => Some(ConstValue::Int(i.wrapping_neg())),
                    (UnaryOp::Neg, ConstValue::Float(f)) => Some(ConstValue::Float(-f)),
                    (UnaryOp::Not, ConstValue::Bool(b)) => Some(ConstValue::Bool(!b)),
                    _ => {
                        self.operator_error(op.text(), &[&v], *span);
                        None
                    }
                }
            }

            ast::Expr::Binary { op, lhs, rhs, span } => {
                let a = self.eval(lhs);
                let b = self.eval(rhs);
                self.binary(*op, a?, b?, *span)
            }

            ast::Expr::Path(p) => match self.resolved.lookup_use(p.span) {
                Some(Res::Const(i)) => self.value_of(i),
                // A name that resolved to something else, or to nothing. The
                // resolver has already said so if it was nothing; if it was a
                // function or a type, this is the "not a constant" case.
                _ => {
                    self.not_constant(p.span, "a name that is not a constant");
                    None
                }
            },

            // `limits.MAX_BODY`. The parser does not decide what `a.b` means,
            // so another module's constant arrives as a field access with the
            // resolver's answer recorded against it.
            ast::Expr::Field { span, .. } => match self.resolved.lookup_use(*span) {
                Some(Res::Const(i)) => self.value_of(i),
                _ => {
                    self.not_constant(*span, "a field read");
                    None
                }
            },

            ast::Expr::Call { span, .. } => {
                self.not_constant(*span, "a call");
                None
            }
            ast::Expr::Error(_) => None,
            other => {
                self.not_constant(other.span(), "this");
                None
            }
        }
    }

    fn binary(
        &mut self,
        op: BinaryOp,
        a: ConstValue,
        b: ConstValue,
        span: Span,
    ) -> Option<ConstValue> {
        use BinaryOp::*;
        use ConstValue::*;

        // Comparison and the two logical operators answer for every pair of
        // like types, so they come first and the arithmetic table below only
        // has to deal with numbers and strings.
        match (op, &a, &b) {
            (Eq, _, _) if same_shape(&a, &b) => return Some(Bool(a == b)),
            (Ne, _, _) if same_shape(&a, &b) => return Some(Bool(a != b)),
            (And, Bool(x), Bool(y)) => return Some(Bool(*x && *y)),
            (Or, Bool(x), Bool(y)) => return Some(Bool(*x || *y)),
            _ => {}
        }

        let out = match (op, &a, &b) {
            (Add, Int(x), Int(y)) => Some(Int(x.wrapping_add(*y))),
            (Sub, Int(x), Int(y)) => Some(Int(x.wrapping_sub(*y))),
            (Mul, Int(x), Int(y)) => Some(Int(x.wrapping_mul(*y))),
            // Division by zero traps at run time. In a constant there is no
            // run time to trap in, so it is a compile error — which is the
            // better place for it to be found anyway.
            (Div, Int(_), Int(0)) | (Rem, Int(_), Int(0)) => {
                self.diags.push(
                    Diagnostic::error(codes::E0118, "this constant divides by zero")
                        .with_primary(span, "there is no value for this")
                        .with_note(
                            "at run time this would trap; in a constant it is found here \
                             instead",
                        ),
                );
                return None;
            }
            (Div, Int(x), Int(y)) => Some(Int(x.wrapping_div(*y))),
            (Rem, Int(x), Int(y)) => Some(Int(x.wrapping_rem(*y))),
            (BitAnd, Int(x), Int(y)) => Some(Int(x & y)),
            (BitOr, Int(x), Int(y)) => Some(Int(x | y)),
            (BitXor, Int(x), Int(y)) => Some(Int(x ^ y)),
            (Shl, Int(x), Int(y)) => Some(Int(x.wrapping_shl(*y as u32))),
            (Shr, Int(x), Int(y)) => Some(Int(x.wrapping_shr(*y as u32))),
            (Lt, Int(x), Int(y)) => Some(Bool(x < y)),
            (Le, Int(x), Int(y)) => Some(Bool(x <= y)),
            (Gt, Int(x), Int(y)) => Some(Bool(x > y)),
            (Ge, Int(x), Int(y)) => Some(Bool(x >= y)),

            (Add, Float(x), Float(y)) => Some(Float(x + y)),
            (Sub, Float(x), Float(y)) => Some(Float(x - y)),
            (Mul, Float(x), Float(y)) => Some(Float(x * y)),
            (Div, Float(x), Float(y)) => Some(Float(x / y)),
            (Rem, Float(x), Float(y)) => Some(Float(x % y)),
            (Lt, Float(x), Float(y)) => Some(Bool(x < y)),
            (Le, Float(x), Float(y)) => Some(Bool(x <= y)),
            (Gt, Float(x), Float(y)) => Some(Bool(x > y)),
            (Ge, Float(x), Float(y)) => Some(Bool(x >= y)),

            (Add, Str(x), Str(y)) => Some(Str(format!("{}{}", x, y))),
            (Lt, Str(x), Str(y)) => Some(Bool(x < y)),
            (Le, Str(x), Str(y)) => Some(Bool(x <= y)),
            (Gt, Str(x), Str(y)) => Some(Bool(x > y)),
            (Ge, Str(x), Str(y)) => Some(Bool(x >= y)),

            _ => None,
        };
        if out.is_none() {
            self.operator_error(op.text(), &[&a, &b], span);
        }
        out
    }

    fn operator_error(&mut self, op: &str, operands: &[&ConstValue], span: Span) {
        let types: Vec<&str> = operands.iter().map(|v| v.type_name()).collect();
        self.diags.push(
            Diagnostic::error(
                codes::E0201,
                format!("`{}` cannot be applied to {}", op, types.join(" and ")),
            )
            .with_primary(span, "no such operation on these types")
            .with_note("Kite has no operator overloading and no implicit conversion"),
        );
    }

    fn not_constant(&mut self, span: Span, what: &str) {
        self.diags.push(
            Diagnostic::error(codes::E0118, "this is not a constant")
                .with_primary(span, format!("{} cannot be worked out while compiling", what))
                .with_note(
                    "a module-level `let` may be a literal, an operator applied to constants, \
                     an interpolation whose holes are constants, or another constant",
                )
                .with_note(
                    "for anything else, write a `fn` that returns it and call it where the \
                     value is wanted",
                ),
        );
    }

    fn text(&self, span: Span) -> &'a str {
        self.sources.span_text(span)
    }

    fn string_value(&mut self, span: Span) -> String {
        let raw = self.text(span);
        if let Some(s) = raw.strip_prefix("\"\"\"") {
            let body = s.strip_suffix("\"\"\"").unwrap_or(s);
            let dedented = crate::dedent_block(body);
            return decode_escapes_into(&dedented, span, self.diags);
        }
        let s = raw.strip_prefix('"').unwrap_or(raw);
        let inner = s.strip_suffix('"').unwrap_or(s);
        decode_escapes_into(inner, span, self.diags)
    }
}

/// Whether two values are the same type, which is what `==` needs before it
/// can answer. Comparing an `int` with a `str` is a mistake, not `false`.
fn same_shape(a: &ConstValue, b: &ConstValue) -> bool {
    a.type_name() == b.type_name()
}

/// A qualified name with its module stripped, for a message a reader
/// recognises: they wrote `LIMIT`, not `config.LIMIT`.
fn bare_name(name: &str) -> &str {
    name.rsplit_once('.').map(|(_, n)| n).unwrap_or(name)
}
