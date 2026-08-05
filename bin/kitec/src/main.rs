//! The Kite compiler CLI.
//!
//! Argument parsing is hand-written. It is thirty lines, has no dependency, and
//! the compiler's build time is a stated design target.

mod bundle;
mod pkg;

use kite_driver::{compile, Emit};
use std::io::{self, Write};
use std::process::ExitCode;

const USAGE: &str = "\
kitec — the Kite compiler

USAGE:
    kitec run   <file.kite>          compile and run
    kitec check <file.kite>          check without running
    kitec build <file.kite>          compile and report what was produced
    kitec test  <file.kite>          run every `test_` function in the file
    kitec fmt   <file.kite>          lay the file out the one way
    kitec doc   <file.kite>          the reference, from the doc comments
    kitec fix   <file.kite>          apply every machine-applicable suggestion
    kitec bundle <file.kite>         one executable that needs nothing installed
    kitec pkg   [directory]          resolve dependency versions, write `kite.lock`

OPTIONS:
    --release         build for release: `assert` is dropped, `require` is not
    --offline         with `pkg`, resolve only from what is already vendored
    --check           with `fmt`, report rather than rewrite
    --all             with `doc`, include what is not `pub`
    --native          with `run`, execute machine code under the JIT — no linker
    --js-strings      with `--emit wasm`, a `str` is a real JavaScript string
    --a11y            with `check`, audit what the program draws for labels,
                      touch target sizes and contrast
    --emit <stage>    check, ast, hir, mir, kbc, wasm, native
    --out <dir>       where `--emit wasm` and `--emit native` write artefacts
    --explain <CODE>  explain a diagnostic code, e.g. --explain E0301
    --version
    --help
";

fn main() -> ExitCode {
    // This binary may *be* a Kite program: `kitec bundle` copies the compiler
    // and appends the source to it. Running that copy runs the program, and
    // everything below is unreachable there.
    if let Some(program) = bundle::embedded() {
        return bundle::run(&program);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", USAGE);
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("kitec {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    if let Some(i) = args.iter().position(|a| a == "--explain") {
        return match args.get(i + 1) {
            Some(code) => explain(code),
            None => fail("`--explain` needs a diagnostic code, e.g. `--explain E0301`"),
        };
    }

    let mut command = None;
    let mut path = None;
    let mut emit = None;
    let mut out_dir = None;
    let mut check_only = false;
    let mut include_private = false;
    let mut release = false;
    let mut offline = false;
    let mut native = false;
    let mut js_strings = false;
    let mut a11y = false;
    let mut i = 0;

    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--emit" => {
                let Some(v) = args.get(i + 1) else {
                    return fail(&format!(
                        "`--emit` needs a stage: {}",
                        Emit::NAMES.join(", ")
                    ));
                };
                let Some(e) = Emit::parse(v) else {
                    return fail(&format!(
                        "unknown emit stage `{}`; expected one of: {}",
                        v,
                        Emit::NAMES.join(", ")
                    ));
                };
                emit = Some(e);
                i += 2;
            }
            "--out" => {
                let Some(v) = args.get(i + 1) else {
                    return fail("`--out` needs a directory");
                };
                out_dir = Some(v.clone());
                i += 2;
            }
            "--check" => {
                check_only = true;
                i += 1;
            }
            "--all" => {
                include_private = true;
                i += 1;
            }
            "--release" => {
                release = true;
                i += 1;
            }
            "--native" => {
                native = true;
                i += 1;
            }
            "--js-strings" => {
                js_strings = true;
                i += 1;
            }
            "--a11y" => {
                a11y = true;
                i += 1;
            }
            "--offline" => {
                offline = true;
                i += 1;
            }
            "run" | "check" | "build" | "test" | "fmt" | "doc" | "fix" | "bundle" | "pkg"
                if command.is_none() =>
            {
                command = Some(a.clone());
                i += 1;
            }
            _ if a.starts_with('-') => {
                return fail(&format!("unknown option `{}`", a));
            }
            _ if path.is_none() => {
                path = Some(a.clone());
                i += 1;
            }
            _ => return fail(&format!("unexpected argument `{}`", a)),
        }
    }

    let command = command.unwrap_or_else(|| "run".to_string());

    // `pkg` takes a directory rather than a file, and defaults to this one.
    if command == "pkg" {
        let dir = path.unwrap_or_else(|| ".".to_string());
        return pkg::run(std::path::Path::new(&dir), offline);
    }

    let Some(path) = path else {
        return fail("expected a source file\n\nUSAGE:\n    kitec run <file.kite>");
    };

    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return fail(&format!("cannot read `{}`: {}", path, e)),
    };

    // Formatting reads tokens, not a compiled program: a file that does not
    // parse still formats, which is exactly when someone reaches for it.
    if command == "fmt" {
        return format_file(&path, &src, check_only);
    }

    // Documentation is extracted from the source, not from a compiled
    // program: a file that does not compile still has doc comments, and its
    // reference is often what someone is reading to find out why.
    if command == "doc" {
        let name = std::path::Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("module");
        let docs = kite_doc::extract(name, &src);
        print!("{}", kite_doc::markdown(&docs, !include_private));
        return ExitCode::SUCCESS;
    }

    if command == "fix" {
        return fix_file(&path, &src);
    }

    if command == "bundle" {
        return bundle::write(&path, &src, out_dir.as_deref(), release);
    }

    // `run --native` is the same compilation `--emit native` performs; the
    // two differ only in what happens to the machine code afterwards.
    if native && emit.is_none() {
        emit = Some(Emit::Native);
    }
    let emit = emit.unwrap_or(Emit::Check);
    // A `str` is either an index into a table the glue holds or the JavaScript
    // string itself. The second needs the JS String Builtins, which every
    // current browser and Node 23 upwards have — but not every engine — so it
    // is asked for rather than assumed.
    let strings = if js_strings {
        kite_driver::Strings::Builtins
    } else {
        kite_driver::Strings::Table
    };
    if js_strings && emit != Emit::Wasm {
        return fail("`--js-strings` only means anything with `--emit wasm`");
    }
    // The native backend refuses some hosts, and it is better to hear that
    // before a compilation than after one.
    if emit == Emit::Native {
        if let Err(why) = kite_driver::native_supported_here() {
            return fail(&why);
        }
    }
    let result = kite_driver::compile_strings(&path, &src, emit, release, strings);

    if !result.diags.is_empty() {
        eprint!("{}", result.render_diagnostics());
    }
    if result.failed() {
        return ExitCode::FAILURE;
    }

    if !result.output.is_empty() {
        print!("{}", result.output);
    }

    // `--emit wasm` writes artefacts rather than printing them.
    if let Some(module) = &result.wasm {
        let dir = out_dir.as_deref().unwrap_or(".");
        if let Err(e) = std::fs::create_dir_all(dir) {
            return fail(&format!("cannot create `{}`: {}", dir, e));
        }
        let wasm_path = format!("{}/app.wasm", dir);
        let js_path = format!("{}/app.js", dir);
        if let Err(e) = std::fs::write(&wasm_path, &module.bytes) {
            return fail(&format!("cannot write `{}`: {}", wasm_path, e));
        }
        let glue = kite_driver::generate_glue_for(
            &module.strings,
            "app.wasm",
            &module.hosts,
            strings,
        );
        if let Err(e) = std::fs::write(&js_path, glue) {
            return fail(&format!("cannot write `{}`: {}", js_path, e));
        }
        // A page to open, so a compiled program is something to look at rather
        // than three files and instructions.
        //
        // **An existing page is never overwritten.** The generated one is a
        // starting point, and the point of this target is that a Kite program
        // goes into a page somebody else wrote — served by anything, styled by
        // whatever stylesheet it already has. Replacing that page with a
        // scaffold on every build would destroy the actual work, quietly, and
        // it would do it most often to the person furthest along.
        let html_path = format!("{}/index.html", dir);
        let name = std::path::Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("kite");
        let page_kept = std::path::Path::new(&html_path).exists();
        if !page_kept {
            if let Err(e) = std::fs::write(&html_path, kite_driver::generate_page(name)) {
                return fail(&format!("cannot write `{}`: {}", html_path, e));
            }
        }
        // A program that listens gets an adapter that can listen for it. A page
        // cannot open a socket, so this is a second entry point rather than
        // another group in the glue — and a program that only fetches never
        // sees it.
        if kite_driver::listens(&module.hosts) {
            let serve_path = format!("{}/serve.mjs", dir);
            if let Err(e) = std::fs::write(&serve_path, kite_driver::generate_server("app.wasm")) {
                return fail(&format!("cannot write `{}`: {}", serve_path, e));
            }
            eprintln!(
                "wrote {} ({} bytes), {}, {} and {}\n\
                 note: this program listens — run it with `node {}`",
                wasm_path,
                module.bytes.len(),
                js_path,
                html_path,
                serve_path,
                serve_path
            );
            return ExitCode::SUCCESS;
        }
        if page_kept {
            eprintln!(
                "wrote {} ({} bytes) and {}\n\
                 note: {} was already there and was left alone",
                wasm_path,
                module.bytes.len(),
                js_path,
                html_path
            );
        } else {
            eprintln!(
                "wrote {} ({} bytes), {} and {}",
                wasm_path,
                module.bytes.len(),
                js_path,
                html_path
            );
        }
        return ExitCode::SUCCESS;
    }

    // The native backend: `run --native` executes under the JIT with no
    // linker anywhere; anything else writes an object file and asks `cc` to
    // join it with the runtime.
    if let Some(program) = &result.native {
        if command == "run" {
            if !program.has_entry() {
                return fail(&format!(
                    "`{}` has no `main` function\n\nnote: a program needs `fn main()` as its \
                     entry point",
                    path
                ));
            }
            let stdout = io::stdout();
            let mut out = stdout.lock();
            return match program.run(&mut out) {
                Ok(()) => {
                    let _ = out.flush();
                    ExitCode::SUCCESS
                }
                Err(e) => fail(&e),
            };
        }
        return write_native(program, &path, out_dir.as_deref());
    }

    match command.as_str() {
        "check" => {
            if a11y {
                return audit_a11y(&result, &path);
            }
            if result.output.is_empty() {
                eprintln!("ok");
            }
            ExitCode::SUCCESS
        }
        "build" => {
            eprintln!("compiled `{}` to bytecode", path);
            ExitCode::SUCCESS
        }
        "test" => run_tests(&result, &path),

        "run" => {
            if !result.is_runnable() {
                return fail(&format!(
                    "`{}` has no `main` function\n\nnote: a program needs `fn main()` as its \
                     entry point",
                    path
                ));
            }
            let stdout = io::stdout();
            let mut out = stdout.lock();
            match result.run(&mut out) {
                Ok(_) => {
                    let _ = out.flush();
                    ExitCode::SUCCESS
                }
                Err(trap) => {
                    let _ = out.flush();
                    eprintln!("\nerror: {}", trap);
                    eprintln!("note: traps are not catchable; Kite has no `recover`");
                    ExitCode::FAILURE
                }
            }
        }
        _ => unreachable!("command was validated above"),
    }
}

/// Run every `test_` function, reporting each and summarising.
///
/// A failure is an error *value* with a message, not a trap, so one failing
/// test does not stop the rest — which is the whole reason `std/test`'s
/// assertions return errors rather than asserting.
/// `kite check --a11y` — audit what the program draws.
///
/// It runs the program. That is the whole idea and it is why this is not a
/// lint: the tree a lint could read is built by `view`, from a model, and
/// nothing about it is knowable until something has run. So the program is run
/// under the bytecode VM, its drawing calls are captured as a transcript, and
/// the transcript is audited — labels, touch targets, contrast.
///
/// A Lighthouse score on a canvas application is a score for a page with one
/// element in it. This looks at the picture instead.
fn audit_a11y(result: &kite_driver::Compilation, path: &str) -> ExitCode {
    if !result.is_runnable() {
        return fail(&format!(
            "`{}` has no `main` function\n\nnote: `--a11y` audits what a program draws, so it \
             has to run it — give the file a `fn main()` that paints a frame",
            path
        ));
    }
    let mut transcript: Vec<u8> = Vec::new();
    if let Err(trap) = result.run(&mut transcript) {
        eprintln!("error: {}", trap);
        return ExitCode::FAILURE;
    }
    let text = String::from_utf8_lossy(&transcript);
    let findings = kite_driver::a11y::audit(&text);
    if findings.is_empty() {
        // Said plainly, and said honestly: this is what was checked, not a
        // claim that the program is accessible.
        eprintln!("ok — no unlabelled controls, undersized targets or low-contrast text");
        return ExitCode::SUCCESS;
    }
    for finding in &findings {
        eprintln!("a11y: {}\n", finding);
    }
    eprintln!(
        "{} finding{} in `{}`",
        findings.len(),
        if findings.len() == 1 { "" } else { "s" },
        path
    );
    ExitCode::FAILURE
}

fn run_tests(result: &kite_driver::Compilation, path: &str) -> ExitCode {
    let tests = result.tests();
    if tests.is_empty() {
        eprintln!(
            "no tests in `{}`\n\nnote: a test is a `pub fn test_…() -> (int, error)`",
            path
        );
        return ExitCode::SUCCESS;
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut failed = 0;
    for name in &tests {
        // A test's own output belongs under its name, so `io.print` inside one
        // is a debugging aid rather than a jumble.
        let mut captured = Vec::new();
        let outcome = result.run_test(name, &mut captured);
        let printed = String::from_utf8_lossy(&captured).to_string();
        match outcome {
            Ok(None) => {
                let _ = writeln!(out, "ok       {}", name);
            }
            Ok(Some(message)) => {
                failed += 1;
                let _ = writeln!(out, "FAILED   {}\n         {}", name, message);
            }
            Err(trap) => {
                failed += 1;
                let _ = writeln!(out, "TRAPPED  {}\n         {}", name, trap);
            }
        }
        for line in printed.lines() {
            let _ = writeln!(out, "         | {}", line);
        }
    }

    let _ = writeln!(
        out,
        "\n{} passed, {} failed",
        tests.len() - failed,
        failed
    );
    let _ = out.flush();
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `kitec fix` — apply what the compiler already knows how to do.
///
/// Every fix a diagnostic carries is a replacement of one span, and applying
/// them back to front means no earlier edit moves a later one. Only fixes
/// pointing into the file being compiled are applied: a suggestion about the
/// standard library is not the user's to take.
fn fix_file(path: &str, src: &str) -> ExitCode {
    let result = compile(path, src, kite_driver::Emit::Check);
    let mut edits: Vec<(u32, u32, String)> = Vec::new();
    let file = result
        .sources
        .iter()
        .find(|(_, name)| name == path)
        .map(|(id, _)| id);
    for d in result.diags.iter() {
        for edit in d.fixes.iter().flat_map(|f| f.edits.iter()) {
            if Some(edit.span.file) != file {
                continue;
            }
            edits.push((edit.span.start, edit.span.end, edit.replacement.clone()));
        }
    }
    if edits.is_empty() {
        eprintln!("nothing to fix in {}", path);
        eprint!("{}", result.render_diagnostics());
        return ExitCode::SUCCESS;
    }
    // Back to front: an edit near the end cannot move one before it.
    edits.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    let mut out = src.to_string();
    let applied = edits.len();
    for (start, end, replacement) in edits {
        out.replace_range(start as usize..end as usize, &replacement);
    }
    match std::fs::write(path, &out) {
        Ok(()) => {
            eprintln!(
                "applied {} fix{} to {}",
                applied,
                if applied == 1 { "" } else { "es" },
                path
            );
            ExitCode::SUCCESS
        }
        Err(e) => fail(&format!("cannot write `{}`: {}", path, e)),
    }
}

/// `--emit native` — write the object file, then link it into an executable.
///
/// The object is written first and survives a failed link: someone
/// cross-checking the codegen with `objdump` has a use for it that no
/// executable serves.
fn write_native(
    program: &kite_driver::NativeProgram,
    path: &str,
    out_dir: Option<&str>,
) -> ExitCode {
    let dir = out_dir.unwrap_or(".");
    if let Err(e) = std::fs::create_dir_all(dir) {
        return fail(&format!("cannot create `{}`: {}", dir, e));
    }
    let object = match program.object() {
        Ok(bytes) => bytes,
        Err(e) => return fail(&e),
    };
    let obj_path = format!("{}/app.o", dir);
    if let Err(e) = std::fs::write(&obj_path, &object) {
        return fail(&format!("cannot write `{}`: {}", obj_path, e));
    }
    let stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("app");
    let exe_path = format!("{}/{}", dir, stem);
    let Some(runtime) = find_runtime_lib() else {
        eprintln!(
            "wrote {} ({} bytes)\n\
             note: `libkite_rt.a` was not found, so no executable was linked; \
             set KITE_RT_LIB to its path, or link the object yourself",
            obj_path,
            object.len()
        );
        return ExitCode::SUCCESS;
    };
    let linked = std::process::Command::new("cc")
        .arg(&obj_path)
        .arg(&runtime)
        .arg("-o")
        .arg(&exe_path)
        .output();
    match linked {
        Ok(o) if o.status.success() => {
            eprintln!("wrote {} ({} bytes) and {}", obj_path, object.len(), exe_path);
            ExitCode::SUCCESS
        }
        Ok(o) => fail(&format!(
            "`cc` could not link `{}`:\n{}",
            obj_path,
            String::from_utf8_lossy(&o.stderr)
        )),
        Err(e) => {
            // No linker is not no result: the object file stands, and the JIT
            // path needs no linker at all.
            eprintln!(
                "wrote {} ({} bytes)\n\
                 note: `cc` is unavailable ({}), so no executable was linked; \
                 `kitec run --native` needs no linker",
                obj_path,
                object.len(),
                e
            );
            ExitCode::SUCCESS
        }
    }
}

/// Where the runtime's static library is. Next to this binary in a
/// development tree, or wherever `KITE_RT_LIB` says in an installed one.
fn find_runtime_lib() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KITE_RT_LIB") {
        let p = std::path::PathBuf::from(p);
        return p.exists().then_some(p);
    }
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();
    // `target/debug/kitec` sits beside `libkite_rt.a`; a test binary sits one
    // level further down, in `deps/`.
    for _ in 0..3 {
        let candidate = dir.join("libkite_rt.a");
        if candidate.exists() {
            return Some(candidate);
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

/// `kitec fmt` — rewrite a file, or say whether it would change.
fn format_file(path: &str, src: &str, check_only: bool) -> ExitCode {
    let formatted = kite_fmt::format(src);
    if formatted == src {
        if !check_only {
            eprintln!("{} is already formatted", path);
        }
        return ExitCode::SUCCESS;
    }
    if check_only {
        eprintln!("{} is not formatted", path);
        return ExitCode::FAILURE;
    }
    match std::fs::write(path, &formatted) {
        Ok(()) => {
            eprintln!("formatted {}", path);
            ExitCode::SUCCESS
        }
        Err(e) => fail(&format!("cannot write `{}`: {}", path, e)),
    }
}

fn explain(code: &str) -> ExitCode {
    let code = code.to_uppercase();
    match kite_diag::codes::explain(&code) {
        Some((summary, body)) => {
            println!("{}: {}\n", code, summary);
            println!("{}", body);
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("error: `{}` is not a known diagnostic code", code);
            let known: Vec<&str> = kite_diag::codes::all().iter().map(|(c, _)| *c).collect();
            eprintln!("note: known codes: {}", known.join(", "));
            ExitCode::FAILURE
        }
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("error: {}", message);
    ExitCode::FAILURE
}
