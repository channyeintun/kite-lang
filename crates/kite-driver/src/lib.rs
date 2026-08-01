//! Pipeline orchestration.
//!
//! One place that knows the pass order, so `kitec`, the test harness, and
//! eventually the language server all drive the compiler identically.

use kite_diag::DiagBag;
use kite_span::{FileId, SourceMap};
use std::io::Write;
use std::path::Path;

pub use kite_codegen_wasm::generate_glue;
pub use kite_vm::Trap;

/// How far to run the pipeline, and what to hand back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Emit {
    /// Stop after checking. Produces no artefact.
    Check,
    Ast,
    Hir,
    Mir,
    /// Disassembled bytecode.
    Kbc,
    /// A WebAssembly module, plus the JavaScript glue that instantiates it.
    Wasm,
}

impl Emit {
    pub fn parse(s: &str) -> Option<Emit> {
        Some(match s {
            "check" => Emit::Check,
            "ast" => Emit::Ast,
            "hir" => Emit::Hir,
            "mir" => Emit::Mir,
            "kbc" => Emit::Kbc,
            "wasm" => Emit::Wasm,
            _ => return None,
        })
    }

    pub const NAMES: [&'static str; 6] = ["check", "ast", "hir", "mir", "kbc", "wasm"];
}

pub struct Compilation {
    pub sources: SourceMap,
    pub diags: DiagBag,
    /// The requested artefact, rendered. Empty for [`Emit::Check`].
    pub output: String,
    chunk: Option<kite_codegen_kbc::Chunk>,
    /// The compiled WebAssembly module, when `--emit wasm` was requested.
    pub wasm: Option<kite_codegen_wasm::WasmModule>,
}

impl Compilation {
    pub fn failed(&self) -> bool {
        self.diags.has_errors()
    }

    pub fn render_diagnostics(&self) -> String {
        self.diags.render_all(&self.sources)
    }

    pub fn is_runnable(&self) -> bool {
        self.chunk.as_ref().is_some_and(|c| c.entry.is_some())
    }

    /// Run the compiled program, writing its output to `out`.
    ///
    /// Returns `Ok(false)` when there is nothing to run.
    pub fn run(&self, out: &mut dyn Write) -> Result<bool, Trap> {
        match &self.chunk {
            None => Ok(false),
            Some(c) => kite_vm::run(c, out).map(|_| true),
        }
    }
}

/// Compile one file's text.
pub fn compile(path: impl AsRef<Path>, src: &str, emit: Emit) -> Compilation {
    let mut sources = SourceMap::new();
    let file = sources.add(path.as_ref(), src);
    let mut diags = DiagBag::new();
    let (output, chunk, wasm) = run_passes(file, src, emit, &mut diags);

    let mut c = Compilation { sources, diags, output, chunk, wasm };
    c.diags.sort(&c.sources);
    c
}

fn run_passes(
    file: FileId,
    src: &str,
    emit: Emit,
    diags: &mut DiagBag,
) -> (
    String,
    Option<kite_codegen_kbc::Chunk>,
    Option<kite_codegen_wasm::WasmModule>,
) {
    let tokens = kite_lexer::tokenize(file, src, diags);
    let ast = kite_parser::parse(file, src, &tokens, diags);

    if emit == Emit::Ast {
        return (format!("{:#?}\n", ast), None, None);
    }

    // Resolution and checking still run after a syntax error — the parser
    // recovers, so later passes can report their own findings on the parts that
    // did parse. Code generation does not, because its input would be poisoned.
    let resolved = kite_resolve::resolve(&ast, diags);
    let hir = kite_types::check(&ast, &resolved, src, diags);

    if emit == Emit::Hir {
        return (hir.to_string(), None, None);
    }
    if diags.has_errors() {
        return (String::new(), None, None);
    }

    let mir = kite_mir::lower(&hir);
    if emit == Emit::Mir {
        return (mir.render(&hir.types).to_string(), None, None);
    }

    if emit == Emit::Wasm {
        let module = kite_codegen_wasm::compile(&mir, &hir.types);
        return (String::new(), None, Some(module));
    }

    let chunk = kite_codegen_kbc::compile(&mir);
    if emit == Emit::Kbc {
        return (chunk.to_string(), Some(chunk), None);
    }

    (String::new(), Some(chunk), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_emit_stage_produces_output_for_a_valid_program() {
        let src = "fn main() {\n  io.print(1 + 2)\n}\n";
        for name in Emit::NAMES {
            let emit = Emit::parse(name).unwrap();
            let c = compile("t.kite", src, emit);
            assert!(!c.failed(), "{} failed:\n{}", name, c.render_diagnostics());
            match emit {
                Emit::Check => {}
                // Wasm is bytes, not text.
                Emit::Wasm => assert!(c.wasm.is_some(), "wasm produced no module"),
                _ => assert!(!c.output.is_empty(), "{} produced no output", name),
            }
        }
    }

    #[test]
    fn a_broken_program_reports_and_produces_no_chunk() {
        let c = compile("t.kite", "fn main() {\n  let x: int = \"s\"\n}\n", Emit::Check);
        assert!(c.failed());
        assert!(!c.is_runnable());
    }

    #[test]
    fn a_program_without_main_is_not_runnable() {
        let c = compile("t.kite", "fn helper() {\n}\n", Emit::Check);
        assert!(!c.failed(), "{}", c.render_diagnostics());
        assert!(!c.is_runnable());
    }

    #[test]
    fn emit_names_all_parse() {
        for n in Emit::NAMES {
            assert!(Emit::parse(n).is_some(), "{} does not parse", n);
        }
        assert!(Emit::parse("native").is_none());
    }

    #[test]
    fn a_compiled_program_runs() {
        let c = compile("t.kite", "fn main() {\n  io.print(7)\n}\n", Emit::Check);
        let mut out = Vec::new();
        assert_eq!(c.run(&mut out).unwrap(), true);
        assert_eq!(String::from_utf8(out).unwrap(), "7\n");
    }
}
