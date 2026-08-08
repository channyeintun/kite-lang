//! Pipeline orchestration.
//!
//! One place that knows the pass order, so `kitec`, the test harness, and
//! eventually the language server all drive the compiler identically.

use kite_diag::{DiagBag, Diagnostic};
use kite_span::{FileId, SourceMap, Span};
use std::io::Write;
use std::path::Path;

pub mod derive;
pub mod doctest;
pub mod host;
pub mod manifest;
pub mod modules;
pub mod semver;
pub mod solve;

pub use kite_codegen_wasm::{
    generate_api, generate_glue, generate_glue_with_hosts, generate_page, generate_server,
    listens, SOURCE_MAP_NAME,
};
pub use kite_vm::Trap;

/// Whether the native backend runs on this host. See
/// `kite_codegen_clif::supported_here` for what it refuses and why.
#[cfg(not(target_arch = "wasm32"))]
pub fn native_supported_here() -> Result<(), String> {
    kite_codegen_clif::supported_here()
}

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
    /// Machine code through Cranelift: an object file for the linker, and a
    /// JIT path for running without one.
    Native,
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
            "native" => Emit::Native,
            _ => return None,
        })
    }

    pub const NAMES: [&'static str; 7] = ["check", "ast", "hir", "mir", "kbc", "wasm", "native"];
}

/// A program held at MIR, ready for the native backend to finish either way:
/// an object file for the linker, or straight into this process under the
/// JIT. The MIR is kept rather than an artefact because the two consumers
/// want different artefacts and neither wants the other's.
pub struct NativeProgram {
    mir: kite_mir::Program,
    types: kite_hir::Types,
}

impl NativeProgram {
    /// Whether there is a `main` to run — the same question `is_runnable`
    /// answers for the bytecode path.
    pub fn has_entry(&self) -> bool {
        self.mir.entry.is_some()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeProgram {
    /// A relocatable object file, for `cc` to join with `libkite_rt.a`.
    pub fn object(&self) -> Result<Vec<u8>, String> {
        kite_codegen_clif::compile_object(&self.mir, &self.types)
    }

    /// Compile into this process and run to completion — `kitec run
    /// --native`, with no linker anywhere.
    pub fn run(&self, out: &mut dyn Write) -> Result<(), String> {
        kite_codegen_clif::run_jit(&self.mir, &self.types, out)
    }
}

pub struct Compilation {
    pub sources: SourceMap,
    pub diags: DiagBag,
    /// The requested artefact, rendered. Empty for [`Emit::Check`].
    pub output: String,
    chunk: Option<kite_codegen_kbc::Chunk>,
    /// The compiled WebAssembly module, when `--emit wasm` was requested.
    pub wasm: Option<kite_codegen_wasm::WasmModule>,
    /// The program held for the native backend, when `--emit native` was
    /// requested.
    pub native: Option<NativeProgram>,
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
    /// Every name grouped with all its occurrences — what rename and
    /// find-references walk. Built from the same resolution as `uses`.
    pub bindings: Vec<Binding>,
    /// What the checker inferred but the source never says, for inlay hints.
    pub hints: Vec<Hint>,
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

/// One name and every place it is written.
///
/// Only what the resolver's own table covers is here: locals, free functions,
/// and type declarations. A method's call sites are the checker's business —
/// they need the receiver's type — so methods are deliberately absent, and a
/// rename that cannot see every occurrence refuses rather than missing one.
pub struct Binding {
    /// As written at a use site: a prelude name is spelled without its module.
    pub name: String,
    pub declared_at: Span,
    /// Occurrences written exactly as `name`, declaration excluded. These are
    /// the spans a rename may edit.
    pub uses: Vec<Span>,
    /// Occurrences that resolve through this declaration but spell more than
    /// its name — `Rect.square`, a qualified `Shape.Circle`, a shorthand
    /// `Point{ x }` whose one identifier is also a field. Findable, but not
    /// editable: a rename with any of these refuses.
    pub mentions: Vec<Span>,
    pub kind: &'static str,
    /// For a local, the name span of the function whose table holds it; a
    /// top-level name has none. Two locals can only collide inside one scope,
    /// which is what a rename checks before inventing a clash.
    pub scope: Option<Span>,
}

/// Something the checker worked out that the source never says, shown inline
/// after the span it belongs to.
pub struct Hint {
    /// The name or callee the hint follows.
    pub after: Span,
    /// The content, without punctuation: a type for a binding, a comma-joined
    /// argument list for a call. The client chooses the `: ` or the `<>`.
    pub text: String,
    /// `"binding"` or `"generics"` — which of those punctuations it takes.
    pub kind: &'static str,
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
            // With a host, so that `@host("fs")` resolves instead of trapping.
            // A namespace nothing implements still traps, naming the function
            // — which is the right answer for a program that asks a browser
            // for a file.
            Some(c) => {
                let mut host = host::NativeHost;
                kite_vm::run_with_host(c, out, Some(&mut host)).map(|_| true)
            }
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

    /// The source map for the compiled WebAssembly module, if there is one.
    ///
    /// Rendered here rather than in the code generator because this is what
    /// holds the [`SourceMap`] a span is resolved through. §16 requires the
    /// map "so browser stack traces name `.kite` files and lines"; what it
    /// carries is one entry per function, pointing at the line the function
    /// was declared on. A MIR instruction has no span to do better with.
    pub fn wasm_source_map(&self) -> Option<String> {
        let module = self.wasm.as_ref()?;
        // A release build carries no debug information, and a map with no
        // entries is a file that exists only to be fetched and found useless.
        if module.source_spans.is_empty() {
            return None;
        }
        let mut sources: Vec<String> = Vec::new();
        let mut spans = Vec::with_capacity(module.source_spans.len());
        for (offset, span) in &module.source_spans {
            let file = self.sources.file(span.file).name.display().to_string();
            let at = self.sources.line_col(*span);
            if !sources.contains(&file) {
                sources.push(file.clone());
            }
            spans.push(kite_codegen_wasm::sourcemap::FunctionSpan {
                offset: *offset,
                file,
                line: at.line,
                column: at.col,
            });
        }
        Some(kite_codegen_wasm::sourcemap::render(&spans, &sources))
    }

    /// Run one named function that takes nothing and answers with nothing.
    ///
    /// For a documentation example, which is a fence of statements rather than
    /// a `test_…` returning `(int, error)`: it fails by trapping — an `assert`
    /// that did not hold — and there is no failure *value* to read back.
    pub fn run_named(&self, name: &str, out: &mut dyn Write) -> Result<(), Trap> {
        let Some(chunk) = &self.chunk else { return Ok(()) };
        kite_vm::run_function(chunk, name, out).map(|_| ())
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

/// Compile one file's text, for a debug build.
pub fn compile(path: impl AsRef<Path>, src: &str, emit: Emit) -> Compilation {
    compile_with(path, src, emit, false)
}

/// Compile, in a named build mode.
///
/// `release` changes two things, and neither is a semantic one: `assert` is
/// dropped, and the Wasm target's debug information — its name section and its
/// source map — goes with it. A build mode that changed what a program *means*
/// would make testing the debug build meaningless, which is why the list is
/// this short and why arithmetic overflow, which does differ, lives in the MIR
/// operation rather than in a backend's idea of the mode.
pub fn compile_with(
    path: impl AsRef<Path>,
    src: &str,
    emit: Emit,
    release: bool,
) -> Compilation {
    compile_provided(path, src, emit, release, std::collections::HashMap::new())
}

/// Compile with some modules handed over rather than read from disk.
///
/// For a caller that already holds the sources and has no filesystem to point
/// at: the compiler built for WebAssembly, a bundler that has read the files,
/// an editor with unsaved buffers. A name given here is used before any
/// directory is searched.
pub fn compile_provided(
    path: impl AsRef<Path>,
    src: &str,
    emit: Emit,
    release: bool,
    provided: std::collections::HashMap<String, String>,
) -> Compilation {
    let mut sources = SourceMap::new();
    // The prelude is added first, so its spans and the user's never collide and
    // a diagnostic inside it says which file it came from.
    let prelude = sources.add("<prelude>", PRELUDE);
    let path = path.as_ref().to_path_buf();
    let file = sources.add(&path, src);
    let mut diags = DiagBag::new();
    let (output, chunk, wasm, native, index) = run_passes(
        prelude, file, &path, &mut sources, emit, release, provided, &mut diags,
    );

    let mut c = Compilation { sources, diags, output, chunk, wasm, native, index };
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

#[allow(clippy::too_many_arguments)]
fn run_passes(
    prelude: FileId,
    file: FileId,
    path: &Path,
    sources: &mut SourceMap,
    emit: Emit,
    release: bool,
    provided: std::collections::HashMap<String, String>,
    diags: &mut DiagBag,
) -> (
    String,
    Option<kite_codegen_kbc::Chunk>,
    Option<kite_codegen_wasm::WasmModule>,
    Option<NativeProgram>,
    Index,
) {
    let src = sources.text(file).to_string();
    let tokens = kite_lexer::tokenize(file, &src, diags);
    let mut ast = kite_parser::parse(file, &src, &tokens, diags);

    if emit == Emit::Ast {
        return (format!("{:#?}\n", ast), None, None, None, Index::default());
    }

    // Modules the program reaches, transitively. Nothing is compiled that
    // nothing asked for, which is what keeps a `hello world` from carrying the
    // standard library.
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    let loader = modules::Loader::load_with(&ast, dir, provided, sources, diags);

    // Every item's module, aligned with the merged item list. The program's own
    // items and the prelude's are the root module.
    let mut item_modules: Vec<String> = vec![String::new(); ast.items.len()];

    // The prelude is a module like any other, and its declarations are
    // qualified like any other's. What makes it the prelude is only that its
    // names are searched from everywhere without being written — and that it
    // is searched *last*, so a program declaring its own `take` shadows the
    // prelude's without breaking the prelude, whose own calls find its own
    // first.
    {
        let text = sources.text(prelude).to_string();
        let tokens = kite_lexer::tokenize(prelude, &text, diags);
        let mut parsed = kite_parser::parse(prelude, &text, &tokens, diags);
        modules::qualify_items(kite_resolve::PRELUDE, &mut parsed.items);
        item_modules.extend(std::iter::repeat_n(
            kite_resolve::PRELUDE.to_string(),
            parsed.items.len(),
        ));
        ast.items.extend(parsed.items);
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

    // `@derive` last, once every declaration is in the list: a derived body
    // walks the fields, and a field may be a type from another module. What it
    // produces is ordinary Kite, parsed here like anything else — so nothing
    // after this point knows derivation happened, and `--emit hir` shows what
    // actually ran.
    if let Some(derived) = derive::expand(&ast.items, &item_modules, diags) {
        let id = sources.add("<derive>", &derived.source);
        let text = sources.text(id).to_string();
        let tokens = kite_lexer::tokenize(id, &text, diags);
        let parsed = kite_parser::parse(id, &text, &tokens, diags);
        // Each generated `impl` is placed in the module of the type it is for,
        // so it reaches that type's private fields exactly as an `impl` written
        // beside the declaration would.
        for (i, item) in parsed.items.into_iter().enumerate() {
            item_modules.push(derived.modules.get(i).cloned().unwrap_or_default());
            ast.items.push(item);
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
    let mut solved = kite_types::Solved::default();
    let mut hir = kite_types::check_recording(&ast, &resolved, sources, diags, release, &mut solved);
    // Built before the early returns below: an editor asks about a file most
    // often when it does *not* compile, and an index that vanished on the
    // first error would be an index nobody could use.
    let mut index = build_index(&resolved, sources);
    // Inlay hints come from the checker's own answers rather than from a
    // re-derivation here, for the same reason everything else in the index
    // does: one compiler, one opinion.
    for (at, text) in solved.bindings {
        // A declaration is also worth hovering — it is the most likely place
        // to, and the inlay hint beside it is not an answer for someone whose
        // hints are off.
        index.uses.push(Use {
            at,
            declared_at: at,
            label: format!("{}: {}", sources.span_text(at), text),
            kind: "variable",
        });
        index.hints.push(Hint { after: at, text, kind: "binding" });
    }
    for (at, text) in solved.calls {
        index.hints.push(Hint { after: at, text, kind: "generics" });
    }
    index.hints.sort_by_key(|h| (h.after.file.0, h.after.start));

    // Locals and methods, which only the checker knows.
    //
    // `build_index` reads the resolver's table, and the resolver knows which
    // *slot* a local use names without knowing what is in it, and cannot find
    // a method at all — finding one needs the receiver's type. So both used to
    // be skipped, and hovering a variable or a method answered nothing. That
    // is most of what anybody hovers.
    for (at, name, ty) in solved.locals {
        index.uses.push(Use {
            at,
            // A local's declaration lives in the function's own table rather
            // than in the resolver's, so there is nothing here to jump to.
            // Pointing at the use itself keeps "go to definition" on a
            // variable a no-op rather than a jump somewhere wrong.
            declared_at: at,
            label: format!("{}: {}", name, ty),
            kind: "variable",
        });
    }
    for (at, signature) in solved.methods {
        index.uses.push(Use { at, declared_at: at, label: signature, kind: "method" });
    }

    if emit == Emit::Hir {
        return (hir.to_string(), None, None, None, index);
    }
    if diags.has_errors() {
        return (String::new(), None, None, None, index);
    }

    // Specialise generic functions before lowering, so no backend ever sees a
    // type parameter. Nothing after this point knows generics exist.
    kite_hir::mono::monomorphise(&mut hir);
    // The prelude is in every program; without this a `hello world` would
    // carry every helper it never mentions.
    kite_hir::mono::prune(&mut hir);
    // `==` inside a generic function was checked as a structural comparison,
    // because nothing was known about the type. Now that specialisation has
    // made it concrete, a primitive gets the primitive's own comparison.
    kite_hir::mono::specialise_equality(&mut hir);

    let mut mir = kite_mir::lower(&hir);
    // `async fn` becomes a starter and a resume function here, once, so both
    // backends see ordinary functions and neither knows concurrency exists.
    kite_mir::asyncify(&mut mir, &mut hir.types);
    if emit == Emit::Mir {
        return (mir.render(&hir.types).to_string(), None, None, None, index);
    }

    // A host object is the web's, and only the web's. A target asked to emit
    // an artefact that cannot hold one is told so here, rather than at the
    // point some backend has to invent a representation for a reference it
    // cannot keep — which is the failure mode that produced the integer handle
    // table this type exists to replace.
    //
    // `Emit::Check` is deliberately not among them. Checking asks whether this
    // is valid Kite, and a program written for the web is; it is also what
    // `kitec run` and the language server use, and a web program should not
    // stop type-checking in an editor. Running one on the bytecode VM fails at
    // its first host call, which already says which function had no host.
    if matches!(emit, Emit::Native | Emit::Kbc) {
        let gaps = host_types_used(&mir, &hir.types);
        let refused = !gaps.is_empty();
        for gap in gaps {
            diags.push(
                Diagnostic::error(
                    kite_diag::codes::E0204,
                    "`JsValue` is a host object, and this target has no host",
                )
                .with_primary(gap.span, format!("used in `{}`", gap.function))
                .with_note(
                    "a `JsValue` is a reference the JavaScript engine owns. There is \
                     nothing outside a browser for one to refer to, so it is refused \
                     here rather than lowered to a number that would mean nothing",
                )
                .with_note("build for the web: `--emit wasm`"),
            );
        }
        if refused {
            return (String::new(), None, None, None, index);
        }
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
            return (String::new(), None, None, None, index);
        }
        let module = kite_codegen_wasm::compile_with(&mir, &hir.types, !release);
        // The last thing that can catch a bad lowering. Everything above this
        // line checks the program; this checks the compiler, and it is the only
        // check whose absence is invisible until a browser refuses the module.
        if let Err(e) = kite_codegen_wasm::validate(&module) {
            diags.push(
                Diagnostic::error(
                    kite_diag::codes::E0900,
                    "the compiler emitted an invalid WebAssembly module",
                )
                .with_note(e)
                .with_note(
                    "this is a bug in Kite, not in this program — please report it, \
                     with the program if it can be shared",
                ),
            );
            return (String::new(), None, None, None, index);
        }
        return (String::new(), None, Some(module), None, index);
    }

    if emit == Emit::Native {
        // The same courtesy the Wasm branch extends: anything this target
        // cannot lower is a diagnostic here, not a binary that traps with no
        // explanation. On wasm32 there is no native backend to ask, and
        // nothing there ever asks for one.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let gaps = kite_codegen_clif::unsupported(&mir, &hir.types);
            if !gaps.is_empty() {
                for gap in &gaps {
                    diags.push(
                        Diagnostic::error(
                            kite_diag::codes::E0204,
                            format!("the native target cannot lower {} yet", gap.what),
                        )
                        .with_primary(gap.span, format!("used in `{}`", gap.function))
                        .with_note(
                            "the bytecode target supports it: run without `--emit native`. \
                             See docs/06-roadmap.md for the remaining lowering steps",
                        ),
                    );
                }
                return (String::new(), None, None, None, index);
            }
        }
        let native = NativeProgram { mir, types: hir.types };
        return (String::new(), None, None, Some(native), index);
    }

    let chunk = kite_codegen_kbc::compile(&mir);
    if emit == Emit::Kbc {
        return (chunk.to_string(), Some(chunk), None, None, index);
    }

    (String::new(), Some(chunk), None, None, index)
}

/// What an editor needs, from the resolution the checker already ran.
/// Where a host object appears in a program.
struct HostTypeUse {
    function: String,
    span: kite_span::Span,
}

/// Every function whose locals mention `JsValue`, once each.
///
/// A signature or a local is enough: a type that cannot be represented cannot
/// be held, so there is no need to look at what is done with it. One report per
/// function, because a function that threads a node through twenty statements
/// has one problem and not twenty.
fn host_types_used(program: &kite_mir::Program, types: &kite_hir::Types) -> Vec<HostTypeUse> {
    let mut found: Vec<HostTypeUse> = Vec::new();
    for f in &program.fns {
        // A signature or a local is enough. A type that cannot be represented
        // cannot be held, so there is nothing to learn from what is done with
        // it — and the walk reaches through `?Element`, `[Element]` and the
        // opaque wrapper struct that `std/dom` is built on.
        if f.locals.iter().any(|l| types.mentions_host_value(l.ty)) {
            found.push(HostTypeUse { function: f.name.clone(), span: f.span });
        }
    }
    found
}

fn build_index(resolved: &kite_resolve::ResolveMap, sources: &SourceMap) -> Index {
    use kite_resolve::Res;
    use std::collections::HashMap;
    let mut index = Index::default();

    for (i, f) in resolved.fns.iter().enumerate() {
        if f.owner.is_some() {
            continue;
        }
        let label = signature_text(sources, f.span, f.param_count);
        index.symbols.push(Symbol {
            // The name as it is *written*: a prelude name is spelled without
            // its module, and offering `prelude.filter` to someone typing
            // `fil` would be offering something that does not compile.
            name: spelled(&f.name),
            at: f.span,
            kind: if f.is_extern { "host function" } else { "function" },
            label,
        });
        let _ = i;
    }
    for t in &resolved.types {
        index.symbols.push(Symbol {
            name: spelled(&t.name),
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

    // ---- bindings ----------------------------------------------------------

    // The resolver records which local slot a use names, but not whose table
    // the slot is in. Function bodies never overlap — a closure's locals live
    // in the enclosing function's table — so the owner of a local use is the
    // nearest function declared at or before it in its file.
    let mut fn_order: Vec<(u32, u32, usize)> = resolved
        .fns
        .iter()
        .enumerate()
        .map(|(i, f)| (f.span.file.0, f.span.start, i))
        .collect();
    fn_order.sort_unstable();
    let owner_of = |at: Span| -> Option<usize> {
        let n = fn_order.partition_point(|&(file, start, _)| (file, start) <= (at.file.0, at.start));
        fn_order[..n]
            .last()
            .filter(|&&(file, _, _)| file == at.file.0)
            .map(|&(_, _, i)| i)
    };

    let mut of_fn: HashMap<u32, usize> = HashMap::new();
    let mut of_type: HashMap<u32, usize> = HashMap::new();
    let mut of_local: HashMap<(usize, u32), usize> = HashMap::new();
    for (i, f) in resolved.fns.iter().enumerate() {
        // A method is deliberately not a binding: its call sites need the
        // receiver's type, so they are the checker's and not in this table —
        // and a rename that cannot see every occurrence must not start.
        if f.owner.is_some() {
            continue;
        }
        of_fn.insert(i as u32, index.bindings.len());
        index.bindings.push(Binding {
            name: spelled(&f.name),
            declared_at: f.span,
            uses: Vec::new(),
            mentions: Vec::new(),
            kind: if f.is_extern { "host function" } else { "function" },
            scope: None,
        });
    }
    for (i, t) in resolved.types.iter().enumerate() {
        of_type.insert(i as u32, index.bindings.len());
        index.bindings.push(Binding {
            name: spelled(&t.name),
            declared_at: t.span,
            uses: Vec::new(),
            mentions: Vec::new(),
            kind: t.kind.describe(),
            scope: None,
        });
    }
    for (i, locals) in resolved.locals.iter().enumerate() {
        for (j, l) in locals.iter().enumerate() {
            // `self` is a keyword, and a synthetic slot was never written;
            // neither is a name anyone can rename.
            if l.synthetic || l.name == "self" {
                continue;
            }
            of_local.insert((i, j as u32), index.bindings.len());
            index.bindings.push(Binding {
                name: l.name.clone(),
                declared_at: l.span,
                uses: Vec::new(),
                mentions: Vec::new(),
                kind: "local",
                scope: Some(resolved.fns[i].span),
            });
        }
    }
    for (at, res) in &resolved.uses {
        let found = match res {
            Res::Fn(i) => of_fn.get(i).copied(),
            Res::Type(i) => of_type.get(i).copied(),
            // A qualified variant spells its enum's name, so it counts as a
            // mention of the enum below. An unqualified one spells only its
            // own name, which has no recorded declaration to group under.
            Res::Variant(i, _) => of_type.get(i).copied(),
            Res::Local(id) => owner_of(*at).and_then(|f| of_local.get(&(f, *id)).copied()),
            Res::Builtin(_) => None,
        };
        let Some(b) = found else { continue };
        let text = sources
            .text(at.file)
            .get(at.start as usize..at.end as usize)
            .unwrap_or("");
        let binding = &mut index.bindings[b];
        if text == binding.name && !resolved.pinned.contains(at) {
            binding.uses.push(*at);
        } else if resolved.pinned.contains(at) || text.starts_with(&format!("{}.", binding.name)) {
            binding.mentions.push(*at);
        }
        // Anything else — an unqualified variant, a use whose written form
        // does not carry this name at all — is not an occurrence of it.
    }
    for b in &mut index.bindings {
        b.uses.sort_by_key(|s| (s.file.0, s.start));
        b.mentions.sort_by_key(|s| (s.file.0, s.start));
    }

    index.uses.sort_by_key(|u| (u.at.file.0, u.at.start));
    index.symbols.sort_by_key(|s| (s.at.file.0, s.at.start));
    index
}

/// How a declared name is written at a use site.
///
/// Every module's names are qualified, and that is how they are written — but
/// the prelude's are in scope without saying so, which is what makes it the
/// prelude.
fn spelled(name: &str) -> String {
    name.strip_prefix(&format!("{}.", kite_resolve::PRELUDE))
        .unwrap_or(name)
        .to_string()
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
                // Wasm and native are bytes, not text.
                Emit::Wasm => assert!(c.wasm.is_some(), "wasm produced no module"),
                Emit::Native => assert!(c.native.is_some(), "native produced no program"),
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
        assert!(Emit::parse("object").is_none());
    }

    #[test]
    fn a_compiled_program_runs() {
        let c = compile("t.kite", "fn main() {\n  io.print(7)\n}\n", Emit::Check);
        let mut out = Vec::new();
        assert!(c.run(&mut out).unwrap());
        assert_eq!(String::from_utf8(out).unwrap(), "7\n");
    }
}
