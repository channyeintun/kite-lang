//! The Kite compiler CLI.
//!
//! Argument parsing is hand-written. It is thirty lines, has no dependency, and
//! the compiler's build time is a stated design target.

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

OPTIONS:
    --check           with `fmt`, report rather than rewrite
    --all             with `doc`, include what is not `pub`
    --emit <stage>    check, ast, hir, mir, kbc, wasm
    --out <dir>       where `--emit wasm` writes app.wasm and app.js
    --explain <CODE>  explain a diagnostic code, e.g. --explain E0301
    --version
    --help
";

fn main() -> ExitCode {
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
            "run" | "check" | "build" | "test" | "fmt" | "doc" | "fix" if command.is_none() => {
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

    let emit = emit.unwrap_or(Emit::Check);
    let result = compile(&path, &src, emit);

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
        let glue =
            kite_driver::generate_glue_with_hosts(&module.strings, "app.wasm", &module.hosts);
        if let Err(e) = std::fs::write(&js_path, glue) {
            return fail(&format!("cannot write `{}`: {}", js_path, e));
        }
        // A page to open, so a compiled program is something to look at rather
        // than three files and instructions.
        let html_path = format!("{}/index.html", dir);
        let name = std::path::Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("kite");
        if let Err(e) = std::fs::write(&html_path, kite_driver::generate_page(name)) {
            return fail(&format!("cannot write `{}`: {}", html_path, e));
        }
        eprintln!(
            "wrote {} ({} bytes), {} and {}",
            wasm_path,
            module.bytes.len(),
            js_path,
            html_path
        );
        return ExitCode::SUCCESS;
    }

    match command.as_str() {
        "check" => {
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
