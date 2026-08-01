//! Pipeline orchestration.
//!
//! One place that knows the pass order, so `kitec`, the test harness, and
//! eventually the language server all drive the compiler identically.

use kite_diag::{DiagBag, Diagnostic};
use kite_span::{FileId, SourceMap, Span};
use std::io::Write;
use std::path::Path;

pub mod modules;

pub use kite_codegen_wasm::{generate_glue, generate_glue_with_hosts, generate_page};
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
    /// What an editor needs: where every name was used and where it was
    /// declared. Built from the same resolution the checker ran on, because a
    /// language server that re-derives its own answers is a second compiler
    /// that disagrees with the first one.
    pub index: Index,
}

/// Where names are, for an editor.
#[derive(Default)]
pub struct Index {
    /// Every resolved use: where it was written, where it was declared, and
    /// what to say about it.
    pub uses: Vec<Use>,
    /// Every top-level declaration, for a symbol list and for completion.
    pub symbols: Vec<Symbol>,
}

pub struct Use {
    pub at: Span,
    pub declared_at: Span,
    /// A one-line description: the signature, or the type of a binding.
    pub label: String,
    pub kind: &'static str,
}

pub struct Symbol {
    pub name: String,
    pub at: Span,
    pub kind: &'static str,
    pub label: String,
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

    /// The tests this program declares, in source order.
    ///
    /// A test is a function whose name starts with `test_`. There is no
    /// attribute syntax in Kite and there is not going to be one: a naming
    /// convention needs no machinery, and `grep test_` finds every test.
    pub fn tests(&self) -> Vec<String> {
        let Some(chunk) = &self.chunk else { return Vec::new() };
        chunk
            .functions
            .iter()
            .filter(|f| f.name.starts_with("test_"))
            .map(|f| f.name.clone())
            .collect()
    }

    /// Run one test, and report what it said went wrong.
    ///
    /// A test returns `(int, error)`, so a failure arrives as an error value
    /// with a message rather than as a trap — which is what lets the rest of
    /// the tests run.
    pub fn run_test(&self, name: &str, out: &mut dyn Write) -> Result<Option<String>, Trap> {
        let Some(chunk) = &self.chunk else { return Ok(None) };
        let value = kite_vm::run_function(chunk, name, out)?;
        Ok(kite_vm::failure_message(&value))
    }
}

/// The standard library, compiled into every program ahead of its own source.
///
/// It is written in Kite rather than in the compiler. Everything in it is
/// expressible in the language, which is the point: a standard library needing
/// compiler support would be evidence that the language was missing something.
///
/// The prelude is the one library file whose names arrive **unqualified**:
/// `map`, `filter`, `Display`. Everything else is a module, reached through
/// `use` and written qualified at every use site.
pub const PRELUDE: &str = include_str!("../../../std/prelude.kite");

/// Compile one file's text.
pub fn compile(path: impl AsRef<Path>, src: &str, emit: Emit) -> Compilation {
    let mut sources = SourceMap::new();
    // The prelude is added first, so its spans and the user's never collide and
    // a diagnostic inside it says which file it came from.
    let prelude = sources.add("<prelude>", PRELUDE);
    let path = path.as_ref().to_path_buf();
    let file = sources.add(&path, src);
    let mut diags = DiagBag::new();
    let (output, chunk, wasm, index) =
        run_passes(prelude, file, &path, &mut sources, emit, &mut diags);

    let mut c = Compilation { sources, diags, output, chunk, wasm, index };
    // The standard library's own advice is not the user's to act on.
    let library: Vec<FileId> = c
        .sources
        .iter()
        .filter(|(_, name)| name.starts_with('<') && name.ends_with('>'))
        .map(|(id, _)| id)
        .collect();
    c.diags.silence_warnings_in(|f| library.contains(&f));
    c.diags.sort(&c.sources);
    c
}

fn run_passes(
    prelude: FileId,
    file: FileId,
    path: &Path,
    sources: &mut SourceMap,
    emit: Emit,
    diags: &mut DiagBag,
) -> (
    String,
    Option<kite_codegen_kbc::Chunk>,
    Option<kite_codegen_wasm::WasmModule>,
    Index,
) {
    let src = sources.text(file).to_string();
    let tokens = kite_lexer::tokenize(file, &src, diags);
    let mut ast = kite_parser::parse(file, &src, &tokens, diags);

    if emit == Emit::Ast {
        return (format!("{:#?}\n", ast), None, None, Index::default());
    }

    // Modules the program reaches, transitively. Nothing is compiled that
    // nothing asked for, which is what keeps a `hello world` from carrying the
    // standard library.
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    let loader = modules::Loader::load(&ast, dir, sources, diags);

    // Every item's module, aligned with the merged item list. The program's own
    // items and the prelude's are the root module.
    let mut item_modules: Vec<String> = vec![String::new(); ast.items.len()];

    // The prelude's declarations join the program as ordinary ones, except
    // where the program declares the same name. A prelude that could not be
    // shadowed would make every name in it permanently unusable, so the
    // program's own definition simply wins — silently, because that is what a
    // prelude is for.
    let own: std::collections::HashSet<String> = ast
        .items
        .iter()
        .filter_map(|i| i.declared_name())
        .map(|s| s.to_string())
        .collect();
    let mut from_library: std::collections::HashMap<String, Span> =
        std::collections::HashMap::new();

    {
        let text = sources.text(prelude).to_string();
        let tokens = kite_lexer::tokenize(prelude, &text, diags);
        let parsed = kite_parser::parse(prelude, &text, &tokens, diags);
        let mut keep = Vec::with_capacity(parsed.items.len());
        for item in parsed.items {
            let Some(name) = item.declared_name().map(|s| s.to_string()) else {
                keep.push(item);
                continue;
            };
            if own.contains(&name) {
                continue;
            }
            // One prelude declaration shadowing another is a bug in the
            // standard library: the first file is left calling a name that no
            // longer means what it did.
            if let Some(first) = from_library.get(&name) {
                diags.push(
                    kite_diag::Diagnostic::error(
                        kite_diag::codes::E0112,
                        format!("the standard library declares `{}` twice", name),
                    )
                    .with_primary(item.span(), "declared again here")
                    .with_secondary(*first, "first declared here")
                    .with_note(
                        "one library file shadowing another would leave the first calling a \
                         name that no longer means what it did — rename one of them",
                    ),
                );
                continue;
            }
            from_library.insert(name, item.span());
            keep.push(item);
        }
        item_modules.extend(std::iter::repeat_n(String::new(), keep.len()));
        ast.items.extend(keep);
    }

    // Each module's declarations are merged qualified, so `load` in module
    // `config` is declared as `config.load` — unforgeable as an identifier,
    // and exactly what an importer writes.
    for module in &loader.loaded {
        for id in &module.files {
            let text = sources.text(*id).to_string();
            let tokens = kite_lexer::tokenize(*id, &text, diags);
            let mut parsed = kite_parser::parse(*id, &text, &tokens, diags);
            modules::qualify_items(&module.name, &mut parsed.items);
            item_modules.extend(std::iter::repeat_n(module.name.clone(), parsed.items.len()));
            ast.items.extend(parsed.items);
        }
    }

    // Resolution and checking still run after a syntax error — the parser
    // recovers, so later passes can report their own findings on the parts that
    // did parse. Code generation does not, because its input would be poisoned.
    let module_map = kite_resolve::Modules {
        of_item: item_modules,
        aliases: loader.aliases.clone(),
    };
    let resolved = kite_resolve::resolve_modules(&ast, module_map, diags);
    let mut hir = kite_types::check(&ast, &resolved, sources, diags);
    // Built before the early returns below: an editor asks about a file most
    // often when it does *not* compile, and an index that vanished on the
    // first error would be an index nobody could use.
    let index = build_index(&resolved, sources);

    if emit == Emit::Hir {
        return (hir.to_string(), None, None, index);
    }
    if diags.has_errors() {
        return (String::new(), None, None, index);
    }

    // Specialise generic functions before lowering, so no backend ever sees a
    // type parameter. Nothing after this point knows generics exist.
    kite_hir::mono::monomorphise(&mut hir);
    // The prelude is in every program; without this a `hello world` would
    // carry every helper it never mentions.
    kite_hir::mono::prune(&mut hir);

    let mut mir = kite_mir::lower(&hir);
    // `async fn` becomes a starter and a resume function here, once, so both
    // backends see ordinary functions and neither knows concurrency exists.
    kite_mir::asyncify(&mut mir, &mut hir.types);
    if emit == Emit::Mir {
        return (mir.render(&hir.types).to_string(), None, None, index);
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
            return (String::new(), None, None, index);
        }
        let module = kite_codegen_wasm::compile(&mir, &hir.types);
        return (String::new(), None, Some(module), index);
    }

    let chunk = kite_codegen_kbc::compile(&mir);
    if emit == Emit::Kbc {
        return (chunk.to_string(), Some(chunk), None, index);
    }

    (String::new(), Some(chunk), None, index)
}

/// What an editor needs, from the resolution the checker already ran.
fn build_index(resolved: &kite_resolve::ResolveMap, sources: &SourceMap) -> Index {
    use kite_resolve::Res;
    let mut index = Index::default();

    for (i, f) in resolved.fns.iter().enumerate() {
        if f.owner.is_some() {
            continue;
        }
        let label = signature_text(sources, f.span, f.param_count);
        index.symbols.push(Symbol {
            name: f.name.clone(),
            at: f.span,
            kind: if f.is_extern { "host function" } else { "function" },
            label,
        });
        let _ = i;
    }
    for t in &resolved.types {
        index.symbols.push(Symbol {
            name: t.name.clone(),
            at: t.span,
            kind: t.kind.describe(),
            label: format!("{} {}", t.kind.describe(), t.name),
        });
    }

    for (at, res) in &resolved.uses {
        let (declared_at, label, kind) = match res {
            Res::Fn(i) => {
                let f = &resolved.fns[*i as usize];
                (f.span, signature_text(sources, f.span, f.param_count), "function")
            }
            Res::Type(i) => {
                let t = &resolved.types[*i as usize];
                (t.span, format!("{} {}", t.kind.describe(), t.name), t.kind.describe())
            }
            Res::Variant(i, v) => {
                let t = &resolved.types[*i as usize];
                (t.span, format!("variant #{} of {}", v, t.name), "variant")
            }
            Res::Builtin(b) => (*at, format!("{} — a compiler builtin", b.path()), "builtin"),
            // A local's declaration is in the function's own table, which the
            // index does not carry: the editor gets the name and where it was
            // used, which is what hovering one asks for.
            Res::Local(_) => continue,
        };
        index.uses.push(Use { at: *at, declared_at, label, kind });
    }
    index.uses.sort_by_key(|u| (u.at.file.0, u.at.start));
    index.symbols.sort_by_key(|s| (s.at.file.0, s.at.start));
    index
}

/// The first line of a declaration, which is its signature.
fn signature_text(sources: &SourceMap, at: Span, param_count: usize) -> String {
    let text = sources.text(at.file);
    // Back up to the start of the line, then take it up to the body.
    let line_start = text[..at.start as usize]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let line_end = text[at.start as usize..]
        .find('\n')
        .map(|i| at.start as usize + i)
        .unwrap_or(text.len());
    let line = text[line_start..line_end].trim();
    let line = line.strip_suffix('{').unwrap_or(line).trim();
    if line.is_empty() {
        format!("takes {} argument(s)", param_count)
    } else {
        line.to_string()
    }
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
        assert!(c.run(&mut out).unwrap());
        assert_eq!(String::from_utf8(out).unwrap(), "7\n");
    }
}
