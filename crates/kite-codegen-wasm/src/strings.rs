//! The language-owned WebAssembly string runtime.
//!
//! A `str` is an array of Unicode scalar values (`i32` code points). All
//! language operations stay in Wasm; only `from_host` and `to_host` cross the
//! JavaScript boundary, a chunk at a time through one imported memory page.

use crate::*;

/// Four bytes per scalar and sixteen KiB per crossing call. This leaves most
/// of the fixed 64 KiB scratch page unused and keeps `String.fromCodePoint`'s
/// argument list comfortably below engine limits.
const SCALAR_CHUNK: i32 = 4096;

pub const FUNCTION_COUNT: u32 = 12;

/// Function indices of the string runtime.
#[derive(Clone, Copy)]
pub struct StringRuntime {
    pub base: u32,
}

impl StringRuntime {
    pub fn from_host(self) -> u32 {
        self.base
    }

    pub fn to_host(self) -> u32 {
        self.base + 1
    }

    pub fn concat(self) -> u32 {
        self.base + 2
    }

    pub fn eq(self) -> u32 {
        self.base + 3
    }

    pub fn compare(self) -> u32 {
        self.base + 4
    }

    pub fn len(self) -> u32 {
        self.base + 5
    }

    pub fn slice(self) -> u32 {
        self.base + 6
    }

    pub fn index_of(self) -> u32 {
        self.base + 7
    }

    pub fn trim(self) -> u32 {
        self.base + 8
    }

    pub fn code_at(self) -> u32 {
        self.base + 9
    }

    pub fn from_code(self) -> u32 {
        self.base + 10
    }

    fn is_space(self) -> u32 {
        self.base + 11
    }
}

fn str_type(layout: &TypeLayout) -> ValType {
    ValType::Ref(RefType {
        nullable: true,
        heap_type: HeapType::Concrete(layout.str_array),
    })
}

/// Declare the runtime signatures and return their type indices in function
/// emission order.
pub fn add_types(section: &mut TypeSection, base: u32, layout: &TypeLayout) -> Vec<u32> {
    let text = str_type(layout);
    let signatures = [
        (vec![EXTERN_REF_NULL], vec![text]),
        (vec![text], vec![EXTERN_REF_NULL]),
        (vec![text, text], vec![text]),
        (vec![text, text], vec![ValType::I32]),
        (vec![text, text], vec![ValType::I64]),
        (vec![text], vec![ValType::I64]),
        (vec![text, ValType::I64, ValType::I64], vec![text]),
        (vec![text, text], vec![ValType::I64]),
        (vec![text], vec![text]),
        (vec![text, ValType::I64], vec![ValType::I64]),
        (vec![ValType::I64], vec![text]),
        (vec![ValType::I32], vec![ValType::I32]),
    ];
    for (params, results) in signatures {
        section.ty().function(params, results);
    }
    (base..base + FUNCTION_COUNT).collect()
}

/// Emit runtime bodies in the same order as [`add_types`].
pub fn emit(code: &mut CodeSection, runtime: StringRuntime, layout: &TypeLayout, hosts: &Hosts) {
    code.function(&from_host(layout, hosts));
    code.function(&to_host(layout, hosts));
    code.function(&concat(layout));
    code.function(&eq(layout));
    code.function(&compare(layout));
    code.function(&len());
    code.function(&slice(layout));
    code.function(&index_of(layout));
    code.function(&trim(layout, runtime));
    code.function(&code_at(layout));
    code.function(&from_code(layout));
    code.function(&is_space());
}

/// Convert a JavaScript string to a scalar array.
fn from_host(layout: &TypeLayout, hosts: &Hosts) -> Function {
    let text = str_type(layout);
    // 0 host value; 1 answer; 2 length; 3 offset; 4 chunk; 5 cursor.
    let mut f = Function::new(vec![(1, text), (4, ValType::I32)]);

    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::Call(hosts.at(host::TEXT_LEN)));
    f.instruction(&Instruction::LocalTee(2));
    f.instruction(&Instruction::ArrayNewDefault(layout.str_array));
    f.instruction(&Instruction::LocalSet(1));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(3));

    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));

    // chunk = min(length - offset, SCALAR_CHUNK)
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::LocalTee(4));
    f.instruction(&Instruction::I32Const(SCALAR_CHUNK));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Const(SCALAR_CHUNK));
    f.instruction(&Instruction::I32LtU);
    f.instruction(&Instruction::Select);
    f.instruction(&Instruction::Call(hosts.at(host::TEXT_FILL)));
    f.instruction(&Instruction::LocalTee(4));
    // A bridge returning no progress would otherwise make this loop eternal.
    f.instruction(&Instruction::I32Eqz);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::Unreachable);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(5));
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));

    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::I32Const(4));
    f.instruction(&Instruction::I32Mul);
    f.instruction(&Instruction::I32Load(MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    f.instruction(&Instruction::ArraySet(layout.str_array));

    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(5));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(3));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::End);
    f
}

/// Convert a scalar array to a JavaScript string.
fn to_host(layout: &TypeLayout, hosts: &Hosts) -> Function {
    // 0 text; 1 length; 2 offset; 3 chunk; 4 cursor; 5 answer.
    let mut f = Function::new(vec![(4, ValType::I32), (1, EXTERN_REF_NULL)]);

    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalTee(1));
    f.instruction(&Instruction::I32Eqz);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::Call(hosts.at(host::TEXT_PUSH)));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));

    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::LocalTee(3));
    f.instruction(&Instruction::I32Const(SCALAR_CHUNK));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Const(SCALAR_CHUNK));
    f.instruction(&Instruction::I32LtU);
    f.instruction(&Instruction::Select);
    f.instruction(&Instruction::LocalSet(3));

    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(4));
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));

    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Const(4));
    f.instruction(&Instruction::I32Mul);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::I32Store(MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));

    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(4));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32Eqz);
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::Call(hosts.at(host::TEXT_PUSH)));
    f.instruction(&Instruction::LocalSet(5));

    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::End);
    f
}

fn concat(layout: &TypeLayout) -> Function {
    let mut f = Function::new(vec![(1, str_type(layout))]);
    // answer = new array(a.len + b.len)
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::ArrayNewDefault(layout.str_array));
    f.instruction(&Instruction::LocalSet(2));

    for (src, dest_offset) in [(0, None), (1, Some(0))] {
        f.instruction(&Instruction::LocalGet(2));
        match dest_offset {
            None => {
                f.instruction(&Instruction::I32Const(0));
            }
            Some(first) => {
                f.instruction(&Instruction::LocalGet(first));
                f.instruction(&Instruction::ArrayLen);
            }
        };
        f.instruction(&Instruction::LocalGet(src));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::LocalGet(src));
        f.instruction(&Instruction::ArrayLen);
        f.instruction(&Instruction::ArrayCopy {
            array_type_index_dst: layout.str_array,
            array_type_index_src: layout.str_array,
        });
    }
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::End);
    f
}

fn eq(layout: &TypeLayout) -> Function {
    // 0 and 1 strings; 2 cursor.
    let mut f = Function::new(vec![(1, ValType::I32)]);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32Ne);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::I32Ne);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::End);
    f
}

fn compare(layout: &TypeLayout) -> Function {
    // 0 and 1 strings; 2 cursor; 3 and 4 current scalars.
    let mut f = Function::new(vec![(3, ValType::I32)]);
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));

    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::LocalSet(3));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::LocalSet(4));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Ne);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32LtU);
    f.instruction(&Instruction::If(BlockType::Result(ValType::I64)));
    f.instruction(&Instruction::I64Const(-1));
    f.instruction(&Instruction::Else);
    f.instruction(&Instruction::I64Const(1));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32LtU);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I64Const(-1));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32GtU);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I64Const(1));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::End);
    f
}

fn len() -> Function {
    let mut f = Function::new(Vec::new());
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::End);
    f
}

fn slice(layout: &TypeLayout) -> Function {
    // 0 text; 1 from; 2 to; 3 start; 4 end; 5 answer.
    let mut f = Function::new(vec![(2, ValType::I32), (1, str_type(layout))]);

    // start = clamp(from, 0, length)
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::I64LeS);
    f.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::Else);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::I64GtS);
    f.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::Else);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32WrapI64);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalSet(3));

    // end = clamp(to, start, length)
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::I64LeS);
    f.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::Else);
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::I64GtS);
    f.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::Else);
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32WrapI64);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalSet(4));

    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::ArrayNewDefault(layout.str_array));
    f.instruction(&Instruction::LocalSet(5));
    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::ArrayCopy {
        array_type_index_dst: layout.str_array,
        array_type_index_src: layout.str_array,
    });
    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::End);
    f
}

fn index_of(layout: &TypeLayout) -> Function {
    // 0 haystack; 1 needle; 2 start; 3 needle cursor; 4 last possible start.
    let mut f = Function::new(vec![(3, ValType::I32)]);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32Eqz);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32GtU);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I64Const(-1));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::LocalSet(4));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(2));

    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32GtU);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(3));

    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::I32Ne);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(3));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::I64Const(-1));
    f.instruction(&Instruction::End);
    f
}

fn trim(layout: &TypeLayout, runtime: StringRuntime) -> Function {
    // 0 text; 1 start; 2 end; 3 answer.
    let mut f = Function::new(vec![(2, ValType::I32), (1, str_type(layout))]);
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(1));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::LocalSet(2));

    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::Call(runtime.is_space()));
    f.instruction(&Instruction::I32Eqz);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(1));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32LeU);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::Call(runtime.is_space()));
    f.instruction(&Instruction::I32Eqz);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::LocalSet(2));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::ArrayNewDefault(layout.str_array));
    f.instruction(&Instruction::LocalSet(3));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::ArrayCopy {
        array_type_index_dst: layout.str_array,
        array_type_index_src: layout.str_array,
    });
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::End);
    f
}

fn code_at(layout: &TypeLayout) -> Function {
    let mut f = Function::new(Vec::new());
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::I64LtS);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I64Const(-1));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::ArrayLen);
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::I64GeS);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::I64Const(-1));
    f.instruction(&Instruction::Return);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32WrapI64);
    f.instruction(&Instruction::ArrayGet(layout.str_array));
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::End);
    f
}

fn from_code(layout: &TypeLayout) -> Function {
    let mut f = Function::new(Vec::new());
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::I64GeS);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I64Const(0x10ffff));
    f.instruction(&Instruction::I64LeS);
    f.instruction(&Instruction::I32And);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I64Const(0xd800));
    f.instruction(&Instruction::I64LtS);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I64Const(0xdfff));
    f.instruction(&Instruction::I64GtS);
    f.instruction(&Instruction::I32Or);
    f.instruction(&Instruction::I32And);
    f.instruction(&Instruction::If(BlockType::Result(str_type(layout))));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32WrapI64);
    f.instruction(&Instruction::ArrayNewFixed {
        array_type_index: layout.str_array,
        array_size: 1,
    });
    f.instruction(&Instruction::Else);
    f.instruction(&Instruction::ArrayNewFixed {
        array_type_index: layout.str_array,
        array_size: 0,
    });
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);
    f
}

/// Unicode White_Space, matching Rust's `char::is_whitespace`, which defines
/// the VM and native backend behavior.
fn is_space() -> Function {
    let mut f = Function::new(Vec::new());
    // U+0009..U+000D
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32Const(0x09));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32Const(0x0d));
    f.instruction(&Instruction::I32LeU);
    f.instruction(&Instruction::I32And);
    for cp in [0x20, 0x85, 0xa0, 0x1680] {
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::I32Const(cp));
        f.instruction(&Instruction::I32Eq);
        f.instruction(&Instruction::I32Or);
    }
    // U+2000..U+200A
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32Const(0x2000));
    f.instruction(&Instruction::I32GeU);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32Const(0x200a));
    f.instruction(&Instruction::I32LeU);
    f.instruction(&Instruction::I32And);
    f.instruction(&Instruction::I32Or);
    for cp in [0x2028, 0x2029, 0x202f, 0x205f, 0x3000] {
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::I32Const(cp));
        f.instruction(&Instruction::I32Eq);
        f.instruction(&Instruction::I32Or);
    }
    f.instruction(&Instruction::End);
    f
}
