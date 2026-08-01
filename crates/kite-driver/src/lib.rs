//! Pipeline orchestration.
//!
//! One place that knows the pass order, so `kitec`, the test harness, and
//! eventually the language server all drive the compiler identically.

use kite_diag::{DiagBag, Diagnostic};
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

/// The standard library, compiled into every program ahead of its own source.
///
/// It is written in Kite rather than in the compiler. Everything in it is
/// expressible in the language, which is the point: a standard library needing
/// compiler support would be evidence that the language was missing something.
pub const PRELUDE: &str = include_str!("../../../std/prelude.kite");

/// The layout engine. Also written in Kite, and also pruned away entirely by
/// any program that does not use it.
pub const UI: &str = include_str!("../../../std/ui.kite");

/// Compile one file's text.
pub fn compile(path: impl AsRef<Path>, src: &str, emit: Emit) -> Compilation {
    let mut sources = SourceMap::new();
    // The prelude is added first, so its spans and the user's never collide and
    // a diagnostic inside it says which file it came from.
    let prelude = sources.add("<prelude>", PRELUDE);
    let ui = sources.add("<ui>", UI);
    let file = sources.add(path.as_ref(), src);
    let mut diags = DiagBag::new();
    let (output, chunk, wasm) =
        run_passes(prelude, ui, file, &sources, emit, &mut diags);

    let mut c = Compilation { sources, diags, output, chunk, wasm };
    c.diags.sort(&c.sources);
    c
}

fn run_passes(
    prelude: FileId,
    ui: FileId,
    file: FileId,
    sources: &SourceMap,
    emit: Emit,
    diags: &mut DiagBag,
) -> (
    String,
    Option<kite_codegen_kbc::Chunk>,
    Option<kite_codegen_wasm::WasmModule>,
) {
    let src = sources.text(file);
    let tokens = kite_lexer::tokenize(file, src, diags);
    let mut ast = kite_parser::parse(file, src, &tokens, diags);

    if emit == Emit::Ast {
        return (format!("{:#?}\n", ast), None, None);
    }

    // The prelude's declarations join the program as ordinary ones, except
    // where the program declares the same name. A prelude that could not be
    // shadowed would make every name in it permanently unusable — and there is
    // no module system yet to qualify one, so the program's own definition
    // simply wins.
    // The prelude is always in scope. The layout engine is not: it declares
    // types with ordinary names — `Size`, `Rect`, `Node` — and a program that
    // never mentions layout should not have to avoid them. `use std/ui` is
    // what asks for it.
    //
    // This is file-level opt-in, not a module system: the names it brings are
    // unqualified, and a program cannot ask for some of them and not others.
    // Real modules are the next piece of Phase 6.
    let wants_ui = ast
        .uses
        .iter()
        .any(|u| u.path.len() == 2 && u.path[0].name == "std" && u.path[1].name == "ui");
    let library: Vec<FileId> =
        if wants_ui { vec![prelude, ui] } else { vec![prelude] };

    let mut taken: std::collections::HashSet<String> = ast
        .items
        .iter()
        .filter_map(|i| i.declared_name())
        .map(|s| s.to_string())
        .collect();
    for id in &library {
        let text = sources.text(*id);
        let tokens = kite_lexer::tokenize(*id, text, diags);
        let parsed = kite_parser::parse(*id, text, &tokens, diags);
        let mut keep = Vec::with_capacity(parsed.items.len());
        for item in parsed.items {
            match item.declared_name() {
                Some(n) if taken.contains(n) => continue,
                Some(n) => {
                    taken.insert(n.to_string());
                    keep.push(item);
                }
                None => keep.push(item),
            }
        }
        ast.items.extend(keep);
    }

    // Resolution and checking still run after a syntax error — the parser
    // recovers, so later passes can report their own findings on the parts that
    // did parse. Code generation does not, because its input would be poisoned.
    let resolved = kite_resolve::resolve(&ast, diags);
    let mut hir = kite_types::check(&ast, &resolved, sources, diags);

    if emit == Emit::Hir {
        return (hir.to_string(), None, None);
    }
    if diags.has_errors() {
        return (String::new(), None, None);
    }

    // Specialise generic functions before lowering, so no backend ever sees a
    // type parameter. Nothing after this point knows generics exist.
    kite_hir::mono::monomorphise(&mut hir);
    // The prelude is in every program; without this a `hello world` would
    // carry every helper it never mentions.
    kite_hir::mono::prune(&mut hir);

    let mir = kite_mir::lower(&hir);
    if emit == Emit::Mir {
        return (mir.render(&hir.types).to_string(), None, None);
    }

    if emit == Emit::Wasm {
        // Report anything this target cannot lower, rather than emitting a
        // module that validates and then traps at run time with no
        // explanation.
        let gaps = kite_codegen_wasm::unsupported(&mir, &hir.types);
        if !gaps.is_empty() {
            for gap in &gaps {
                diags.push(
                    Diagnostic::error(
                        kite_diag::codes::E0204,
                        format!("the wasm target cannot lower {} yet", gap.what),
                    )
                    .with_primary(gap.span, format!("used in `{}`", gap.function))
                    .with_note(
                        "the bytecode target supports it: run without `--emit wasm`. \
                         See docs/06-roadmap.md for the remaining lowering steps",
                    ),
                );
            }
            return (String::new(), None, None);
        }
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
