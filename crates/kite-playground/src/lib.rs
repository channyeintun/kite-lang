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
    answer(with_source(ptr, len, |src| kite_fmt::format(src)))
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
