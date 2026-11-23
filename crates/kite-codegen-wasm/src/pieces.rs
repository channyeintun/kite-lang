//! Long literals: their constant chunks, in functions of their own.
//!
//! A literal longer than [`STR_LITERAL_CHUNK`] is allocated at its full length
//! and filled a chunk at a time (see `Emitter::fixed_array`). Written inline,
//! that is every element in the function the literal is in, and a literal of a
//! hundred and twenty thousand strings made one function of 1.9 MB that the
//! validator accepts and that V8 cannot compile: its machine code needs "more
//! than half of the code space", and the engine does not refuse the module, it
//! aborts the process. Each string is an `array.new_fixed` of its own, an
//! allocation, which is most of what that machine code is.
//!
//! So each run of constants in a chunk is built by a function of its own,
//! `() -> (ref $array)`, and the literal's function calls it where the run's
//! elements would have been; an element that reads a local is stored in place
//! with `array.set`, one `local.get` and no allocation. No function then holds
//! more than one chunk of a literal's constants, whatever its length and
//! wherever its locals fall: the literal's own function holds a call per run
//! and a store per local, and each piece holds at most one chunk. (A local in
//! every chunk once kept every chunk's constants inline, and 120,000 strings
//! with one local per 4,000 aborted V8 as before.) Identical runs — a table of
//! a hundred thousand `"same"` — are one piece called many times, which is
//! sound because each call builds a fresh array.
//!
//! The pieces come last in the function index space, after every other helper,
//! and are made as bodies are emitted rather than collected beforehand; only
//! their signatures, one per array type a long literal fills, are declared
//! ahead, because the type section comes first.

use crate::*;
use std::cell::RefCell;
use std::collections::HashMap;

/// The pieces a module's long literals call, and where they start in the
/// function index space.
pub struct Pieces {
    /// Per array type some long literal fills, the type index of a piece
    /// that builds one chunk of it.
    sigs: HashMap<u32, u32>,
    pub base: u32,
    made: RefCell<Made>,
}

#[derive(Default)]
struct Made {
    /// Each piece's type index, its body, and the function that first asked
    /// for it — which is the one named if the piece is too large.
    bodies: Vec<(u32, Function, String, kite_span::Span)>,
    /// A body's bytes, and the piece already built from them.
    seen: HashMap<Vec<u8>, u32>,
}

impl Pieces {
    /// The array types a literal longer than one chunk fills, sorted, so the
    /// signatures are declared in a stable order. A map literal counts by its
    /// entries before constant keys are collapsed, which may declare a
    /// signature nothing calls; that is a few bytes, in a program with a
    /// literal of thousands of entries.
    pub fn collect(program: &mir::Program, types: &Types, layout: &TypeLayout) -> Vec<u32> {
        let mut arrays: Vec<u32> = Vec::new();
        for f in &program.fns {
            for b in &f.blocks {
                for s in &b.stmts {
                    let mir::Inst::Assign { dst, value } = s else {
                        continue;
                    };
                    let ty = f.locals[dst.index()].ty;
                    match value {
                        mir::Rvalue::SliceNew { elems } if elems.len() > STR_LITERAL_CHUNK => {
                            if let TyKind::Slice(elem) = *types.kind(ty) {
                                arrays.extend(layout.slice_type(elem));
                            }
                        }
                        mir::Rvalue::MapNew { entries } if entries.len() / 2 > STR_LITERAL_CHUNK => {
                            if let Some(ml) = layout.map_layout(ty) {
                                arrays.extend([ml.keys, ml.values]);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        arrays.sort_unstable();
        arrays.dedup();
        arrays
    }

    /// Declare a piece's signature for each of `arrays`, from type index
    /// `next` on. `base` is where the pieces will start in the function index
    /// space.
    pub fn declare(arrays: &[u32], section: &mut TypeSection, next: u32, base: u32) -> Pieces {
        let mut sigs = HashMap::new();
        for (i, &array) in arrays.iter().enumerate() {
            let result = ValType::Ref(RefType {
                nullable: false,
                heap_type: HeapType::Concrete(array),
            });
            section.ty().function(vec![], vec![result]);
            sigs.insert(array, next + i as u32);
        }
        Pieces {
            sigs,
            base,
            made: RefCell::new(Made::default()),
        }
    }

    /// Whether a chunk of `array_type` may be built by a piece.
    pub fn fills(&self, array_type: u32) -> bool {
        self.sigs.contains_key(&array_type)
    }

    /// The function index of the piece whose body is `body`, made now if no
    /// identical one exists. `body` builds one chunk of `array_type`, which
    /// [`Self::fills`] has said yes to.
    pub fn piece(&self, array_type: u32, body: Function, from: &mir::Function) -> u32 {
        let mut bytes = Vec::new();
        wasm_encoder::Encode::encode(&body, &mut bytes);
        let mut made = self.made.borrow_mut();
        if let Some(&at) = made.seen.get(&bytes) {
            return at;
        }
        let at = self.base + made.bodies.len() as u32;
        made.seen.insert(bytes, at);
        made.bodies.push((self.sigs[&array_type], body, from.name.clone(), from.span));
        at
    }

    /// Each piece's type index, in function index order.
    pub fn type_indices(&self) -> Vec<u32> {
        self.made.borrow().bodies.iter().map(|(t, ..)| *t).collect()
    }

    /// Every piece past [`MAX_BODY_BYTES`], named by the function its literal
    /// is in: a single chunk of very long strings can still be too much.
    pub fn too_large(&self) -> Vec<TooLarge> {
        self.made
            .borrow()
            .bodies
            .iter()
            .filter(|(_, body, ..)| body.byte_len() > MAX_BODY_BYTES)
            .map(|(_, body, name, span)| TooLarge {
                function: name.clone(),
                span: *span,
                locals: false,
                what: format!(
                    "{} bytes of code in one chunk of a literal, more than the {} an engine \
                     accepts in one function",
                    body.byte_len(),
                    MAX_BODY_BYTES
                ),
            })
            .collect()
    }

    /// Emit every piece's body, in the order they were made.
    pub fn emit(&self, code: &mut CodeSection) {
        for (_, body, ..) in &self.made.borrow().bodies {
            code.function(body);
        }
    }
}
