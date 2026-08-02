//! A minimal front half for the backend's own tests: source to MIR, without
//! the driver — the same shape as the Wasm backend's harness, plus the async
//! transform, because a backend must never see an `await`.

use kite_span::SourceMap;

pub struct Lowered {
    pub mir: kite_mir::Program,
    pub types: kite_hir::Types,
}

pub fn lower(src: &str) -> Lowered {
    let mut sources = SourceMap::new();
    let f = sources.add("t.kite", src);
    let mut diags = kite_diag::DiagBag::new();
    let tokens = kite_lexer::tokenize(f, src, &mut diags);
    let ast = kite_parser::parse(f, src, &tokens, &mut diags);
    let resolved = kite_resolve::resolve(&ast, &mut diags);
    let mut hir = kite_types::check(&ast, &resolved, &sources, &mut diags);
    assert!(
        !diags.has_errors(),
        "test source does not compile:\n{}",
        diags.render_all(&sources)
    );
    kite_hir::mono::monomorphise(&mut hir);
    let mut mir = kite_mir::lower(&hir);
    kite_mir::asyncify(&mut mir, &mut hir.types);
    Lowered { mir, types: hir.types }
}

/// Run natively, in process, and hand back what was printed.
pub fn run_native(src: &str) -> String {
    let l = lower(src);
    let mut out = Vec::new();
    kite_codegen_clif::run_jit(&l.mir, &l.types, &mut out).expect("the JIT runs");
    String::from_utf8(out).expect("output is valid UTF-8")
}

/// The oracle: the same program on the bytecode VM.
pub fn run_vm(src: &str) -> String {
    let l = lower(src);
    let chunk = kite_codegen_kbc::compile(&l.mir);
    let mut out = Vec::new();
    kite_vm::run(&chunk, &mut out).expect("the VM runs");
    String::from_utf8(out).expect("output is valid UTF-8")
}

/// Both backends, one assertion.
#[allow(dead_code)]
pub fn agree(src: &str) {
    let vm = run_vm(src);
    let native = run_native(src);
    assert_eq!(vm, native, "the VM and the native backend disagree");
}
