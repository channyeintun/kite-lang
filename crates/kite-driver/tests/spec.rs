//! The specification, checked against the compiler.
//!
//! A specification that nothing checks drifts, and this one had. Appendix A —
//! titled *A complete program* — used `use std/io`, `impl Error for LoadError`
//! and `json.decode<[Task]>`, none of which exist. Two were the appendix being
//! wrong about the language; the third is the language being unfinished. Either
//! way nothing noticed, for as long as nothing was looking.
//!
//! Only Appendix A is compiled, and deliberately. The rest of the document is
//! fragments — a signature, three lines of a match, a type without the program
//! around it — and a test that demanded every one of them stand alone would
//! either fail constantly or force the prose to be written around the harness.
//! The appendix is the one block that claims to be whole, so it is the one held
//! to it.

use kite_driver::{compile, Emit};
use std::path::Path;

fn specification() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../SPECIFICATION.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e))
}

/// The first fenced `kite` block after a heading.
fn block_after(doc: &str, heading: &str) -> String {
    let start = doc
        .find(heading)
        .unwrap_or_else(|| panic!("the specification has no `{}`", heading));
    let rest = &doc[start..];
    let open = rest.find("```kite\n").expect("a kite block follows the heading");
    let body = &rest[open + "```kite\n".len()..];
    let close = body.find("```").expect("the block is closed");
    body[..close].to_string()
}

/// Appendix A compiles.
///
/// It is the document's claim about what a real Kite program looks like. If it
/// does not compile, either the claim is wrong or the language moved — and both
/// are worth being told about on the run that caused it rather than by whoever
/// types it in months later.
#[test]
fn the_complete_program_in_appendix_a_compiles() {
    let src = block_after(&specification(), "## Appendix A — A complete program");
    assert!(
        src.contains("pub async fn main"),
        "the extracted block is not the program: {}",
        &src[..src.len().min(200)]
    );
    let c = compile(Path::new("appendix-a.kite"), &src, Emit::Check);
    assert!(
        !c.failed(),
        "Appendix A does not compile:\n{}",
        c.render_diagnostics()
    );
}

/// The appendix compiles for the web too.
///
/// A program in a document about a Wasm-first language should reach the target
/// the language exists for, and the checker alone would not have said whether
/// it does.
#[test]
fn the_complete_program_reaches_the_web_target() {
    let src = block_after(&specification(), "## Appendix A — A complete program");
    let c = compile(Path::new("appendix-a.kite"), &src, Emit::Wasm);
    assert!(
        !c.failed(),
        "Appendix A does not compile to wasm:\n{}",
        c.render_diagnostics()
    );
}

/// The document does not claim to be unimplemented.
///
/// It said "Design document. Not yet implemented." while three backends ran
/// 765 tests against it. A status line nobody maintains is worse than none,
/// because it is read first and believed.
#[test]
fn the_status_line_is_not_the_old_one() {
    let doc = specification();
    assert!(
        !doc.contains("Not yet implemented"),
        "the specification still says it is unimplemented"
    );
}
