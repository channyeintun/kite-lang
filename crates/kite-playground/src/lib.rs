//! The compiler, as a WebAssembly module.
//!
//! A language whose site cannot run the language is asking to be taken on
//! faith. `kitec` is Rust and already targets WebAssembly, so the playground
//! needs no server at all: the page compiles and runs Kite in the same tab,
//! with the same diagnostics a terminal shows, because it is the same code.
//!
//! The boundary is deliberately primitive — a pointer and a length in, a
//! pointer and a length out — rather than a binding framework. There is one
//! function to call and one buffer to read, and no dependency to fetch.

use std::io::Write;

/// A buffer the caller may write into. It is leaked: the caller owns it and
/// hands it back to [`kite_free`].
///
/// Every export here is prefixed. `free` on its own would replace libc's, and
/// every deallocation in the program would arrive here — which it did, once,
/// as a stack overflow with no other symptom.
///
/// # Safety
/// The caller must eventually pass the returned pointer and the same length
/// to [`kite_free`].
#[no_mangle]
pub extern "C" fn kite_alloc(len: usize) -> *mut u8 {
    let mut buffer = Vec::<u8>::with_capacity(len);
    let ptr = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    ptr
}

/// # Safety
/// `ptr` must have come from [`kite_alloc`] with the same `len`.
#[no_mangle]
pub unsafe extern "C" fn kite_free(ptr: *mut u8, len: usize) {
    drop(Vec::from_raw_parts(ptr, 0, len));
}

/// How long the last answer is. Read after any of the calls below.
static mut LENGTH: usize = 0;

#[no_mangle]
pub extern "C" fn kite_answer_length() -> usize {
    unsafe { LENGTH }
}

/// Compile and run a program, and answer with what it printed — or with the
/// diagnostics, rendered exactly as the terminal renders them.
///
/// # Safety
/// `ptr` and `len` must describe valid UTF-8 the caller owns.
#[no_mangle]
pub unsafe extern "C" fn kite_run(ptr: *const u8, len: usize) -> *mut u8 {
    answer(with_source(ptr, len, |src| {
        let compiled = kite_driver::compile("playground.kite", src, kite_driver::Emit::Check);
        let mut out = compiled.render_diagnostics();
        if compiled.failed() {
            return out;
        }
        let mut printed = Vec::new();
        match compiled.run(&mut printed) {
            Ok(true) => {}
            Ok(false) => {
                let _ = writeln!(printed, "note: this program has no `main` to run");
            }
            Err(trap) => {
                let _ = writeln!(printed, "\nerror: {}", trap);
                let _ = writeln!(printed, "note: traps are not catchable; Kite has no `recover`");
            }
        }
        out.push_str(&String::from_utf8_lossy(&printed));
        out
    }))
}

/// Check without running: the diagnostics alone, for typing into an editor.
///
/// # Safety
/// As [`kite_run`].
#[no_mangle]
pub unsafe extern "C" fn kite_check(ptr: *const u8, len: usize) -> *mut u8 {
    answer(with_source(ptr, len, |src| {
        let compiled = kite_driver::compile("playground.kite", src, kite_driver::Emit::Check);
        compiled.render_diagnostics()
    }))
}

/// One of the compiler's intermediate forms, by name: `ast`, `hir`, `mir` or
/// `kbc`. Seeing what a program becomes is most of what a playground is for.
///
/// # Safety
/// As [`run`], and `stage` must likewise be valid UTF-8.
#[no_mangle]
pub unsafe extern "C" fn kite_emit(
    ptr: *const u8,
    len: usize,
    stage_ptr: *const u8,
    stage_len: usize,
) -> *mut u8 {
    let stage = std::str::from_utf8(std::slice::from_raw_parts(stage_ptr, stage_len))
        .unwrap_or("mir")
        .to_string();
    answer(with_source(ptr, len, |src| {
        let Some(emit) = kite_driver::Emit::parse(&stage) else {
            return format!("error: unknown stage `{}`\n", stage);
        };
        let compiled = kite_driver::compile("playground.kite", src, emit);
        if compiled.failed() {
            return compiled.render_diagnostics();
        }
        if compiled.output.is_empty() {
            "note: that stage produces no text\n".to_string()
        } else {
            compiled.output
        }
    }))
}

/// The program, laid out the one way.
///
/// # Safety
/// As [`kite_run`].
#[no_mangle]
pub unsafe extern "C" fn kite_format(ptr: *const u8, len: usize) -> *mut u8 {
    answer(with_source(ptr, len, kite_fmt::format))
}

/// The program's reference, from its doc comments.
///
/// # Safety
/// As [`kite_run`].
#[no_mangle]
pub unsafe extern "C" fn kite_docs(ptr: *const u8, len: usize) -> *mut u8 {
    answer(with_source(ptr, len, |src| {
        let docs = kite_doc::extract("playground", src);
        kite_doc::markdown(&docs, true)
    }))
}

/// Run `f` over the caller's source text.
///
/// # Safety
/// `ptr` and `len` must describe a valid UTF-8 buffer.
unsafe fn with_source(ptr: *const u8, len: usize, f: impl FnOnce(&str) -> String) -> String {
    let bytes = std::slice::from_raw_parts(ptr, len);
    match std::str::from_utf8(bytes) {
        Ok(src) => f(src),
        Err(_) => "error: the source is not valid UTF-8\n".to_string(),
    }
}

/// Compile a program the way `kitec build --emit wasm` does, and hand back
/// every file it would have written.
///
/// **This is what makes a build tool need no compiler installed.** A bundler
/// plugin can depend on this module through npm and be done — no native
/// binary, no download at install time, no platform matrix, and the same
/// compiler on every operating system because it *is* the same compiler.
///
/// The artefacts are framed rather than returned one at a time, because
/// `app.wasm` is bytes and the rest is text and a caller wants all of them
/// from one compile:
///
/// ```text
/// u32  count
/// for each:  u32 name length, name, u32 body length, body
/// ```
///
/// Little-endian, which is what Wasm's memory is. A failed compile answers
/// with a count of zero and the diagnostics as the only entry, named
/// `diagnostics` — so a caller reads the same frame either way.
///
/// # Safety
/// As [`kite_run`].
#[no_mangle]
pub unsafe extern "C" fn kite_build(
    ptr: *const u8,
    len: usize,
    release: u32,
    js_strings: u32,
) -> *mut u8 {
    let module = match module_input(ptr, len) {
        Ok(module) => module,
        Err(message) => return frame(&[("diagnostics", message.into_bytes())]),
    };
    let compiled = kite_driver::compile_provided(
        "main.kite",
        &module.entry,
        kite_driver::Emit::Wasm,
        release != 0,
        if js_strings != 0 {
            kite_driver::Strings::Builtins
        } else {
            kite_driver::Strings::Table
        },
        module.siblings,
    );
    if compiled.failed() {
        return frame(&[("diagnostics", compiled.render_diagnostics().into_bytes())]);
    }
    let Some(module) = compiled.wasm.as_ref() else {
        return frame(&[("diagnostics", b"error: no module was produced\n".to_vec())]);
    };
    let glue = kite_driver::generate_glue_with_hosts(&module.strings, "app.wasm", &module.hosts);
    let (api_js, api_dts) = kite_driver::generate_api(&module.api, "app.wasm");
    frame(&[
        ("app.wasm", module.bytes.clone()),
        ("app.js", glue.into_bytes()),
        ("api.js", api_js.into_bytes()),
        ("api.d.ts", api_dts.into_bytes()),
    ])
}

/// Check a whole module, the way [`kite_build`] compiles one.
///
/// [`kite_check`] takes a single file, which is right for a playground and
/// wrong for a project: a Kite module is a *directory*, so a program that says
/// `use checkout` has a sibling the checker must be able to see. Without this
/// a build tool could compile a project it could not check.
///
/// Answers with the diagnostics, rendered as a terminal renders them, or with
/// nothing at all when the module is clean.
///
/// # Safety
/// As [`kite_run`].
#[no_mangle]
pub unsafe extern "C" fn kite_check_module(ptr: *const u8, len: usize) -> *mut u8 {
    let module = match module_input(ptr, len) {
        Ok(module) => module,
        Err(message) => return answer(message),
    };
    let compiled = kite_driver::compile_provided(
        "main.kite",
        &module.entry,
        kite_driver::Emit::Check,
        false,
        kite_driver::Strings::Table,
        module.siblings,
    );
    answer(compiled.render_diagnostics())
}

/// A framed module: the program, and its siblings by module name.
struct ModuleInput {
    entry: String,
    siblings: std::collections::HashMap<String, String>,
}

/// Read the framed input both [`kite_build`] and [`kite_check_module`] take.
///
/// The first entry is the program and the rest are its siblings, by module
/// name — because a Kite module is a directory and this side has no directory
/// to read.
///
/// # Safety
/// `ptr` and `len` must describe a buffer the caller owns.
unsafe fn module_input(ptr: *const u8, len: usize) -> Result<ModuleInput, String> {
    let bytes = std::slice::from_raw_parts(ptr, len);
    let Some(files) = unframe(bytes) else {
        return Err("error: the input is malformed\n".to_string());
    };
    let Some((_, entry)) = files.first() else {
        return Err("error: no program was given\n".to_string());
    };
    let Ok(src) = std::str::from_utf8(entry) else {
        return Err("error: the source is not valid UTF-8\n".to_string());
    };
    let mut siblings = std::collections::HashMap::new();
    for (name, body) in files.iter().skip(1) {
        if let Ok(text) = std::str::from_utf8(body) {
            siblings.insert(name.clone(), text.to_string());
        }
    }
    Ok(ModuleInput { entry: src.to_string(), siblings })
}

/// The framing described on [`kite_build`], read back.
fn unframe(bytes: &[u8]) -> Option<Vec<(String, Vec<u8>)>> {
    let u32_at = |at: usize| -> Option<usize> {
        let slice = bytes.get(at..at + 4)?;
        Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]) as usize)
    };
    let count = u32_at(0)?;
    let mut out = Vec::with_capacity(count);
    let mut at = 4;
    for _ in 0..count {
        let name_len = u32_at(at)?;
        at += 4;
        let name = String::from_utf8(bytes.get(at..at + name_len)?.to_vec()).ok()?;
        at += name_len;
        let body_len = u32_at(at)?;
        at += 4;
        out.push((name, bytes.get(at..at + body_len)?.to_vec()));
        at += body_len;
    }
    Some(out)
}

/// The framing described on [`kite_build`].
fn frame(entries: &[(&str, Vec<u8>)]) -> *mut u8 {
    let mut out = Vec::new();
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (name, body) in entries {
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
    }
    out.shrink_to_fit();
    let ptr = out.as_mut_ptr();
    unsafe { LENGTH = out.len() };
    std::mem::forget(out);
    ptr
}

/// Hand a string back as a buffer the caller reads and frees.
fn answer(text: String) -> *mut u8 {
    let mut bytes = text.into_bytes();
    bytes.shrink_to_fit();
    let ptr = bytes.as_mut_ptr();
    unsafe { LENGTH = bytes.len() };
    std::mem::forget(bytes);
    ptr
}

#[cfg(test)]
mod tests {
    /// The playground is the compiler, so its own tests are about the
    /// boundary rather than about compiling: that a program's output comes
    /// back, and that a broken one comes back as diagnostics rather than as a
    /// panic.
    #[test]
    fn a_program_runs_and_its_output_comes_back() {
        let src = "fn main() {\n  io.print(6 * 7)\n}\n";
        let answer = unsafe {
            let ptr = super::kite_run(src.as_ptr(), src.len());
            let len = super::kite_answer_length();
            String::from_utf8(std::slice::from_raw_parts(ptr, len).to_vec()).unwrap()
        };
        assert_eq!(answer, "42\n");
    }

    #[test]
    fn a_broken_program_comes_back_as_diagnostics() {
        let src = "fn main() {\n  let x: int = \"s\"\n}\n";
        let answer = unsafe {
            let ptr = super::kite_run(src.as_ptr(), src.len());
            let len = super::kite_answer_length();
            String::from_utf8(std::slice::from_raw_parts(ptr, len).to_vec()).unwrap()
        };
        assert!(answer.contains("E0200"), "{}", answer);
    }

    #[test]
    fn a_stage_can_be_asked_for() {
        let src = "fn main() {\n  io.print(1)\n}\n";
        let stage = "mir";
        let answer = unsafe {
            let ptr = super::kite_emit(src.as_ptr(), src.len(), stage.as_ptr(), stage.len());
            let len = super::kite_answer_length();
            String::from_utf8(std::slice::from_raw_parts(ptr, len).to_vec()).unwrap()
        };
        assert!(answer.contains("fn main"), "{}", answer);
    }
}
