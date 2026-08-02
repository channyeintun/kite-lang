//! Golden transcripts of the drawing calls, across eight scripts.
//!
//! The roadmap asks for golden-image tests against the DOM renderer across
//! Latin, Cyrillic, Arabic, Hebrew, Devanagari, Thai, CJK and Burmese. There
//! is no canvas in Node and this project takes no dependency to fake one, so
//! these are named for what they are: golden **transcripts**, not golden
//! images. `examples/scripts.kite` lays out and paints a sample of each
//! script, and what it prints — every drawing call, in order, under the
//! nominal font both backends share — is compared against committed expected
//! output in `tests/golden/`, and across the two backends.
//!
//! What a transcript catches: a bidi reordering regression moves a run or
//! changes its x; a line-breaking regression moves text between lines; an
//! advance regression moves every x after it; an Arabic joining regression
//! changes the presentation forms baked into the drawn text. What it cannot
//! catch: a rasterisation difference. A wrong glyph drawn at the right
//! position, an antialiasing change, a font fallback — all invisible here,
//! because nothing in this test rasterises. That half needs a browser and
//! pixels, and it remains open.
//!
//! The transcript is also exactly what both real renderers consume: the
//! recorded call list *is* the retained scene graph, and the DOM and canvas
//! renderers are replays of it. The second test closes that loop — the same
//! program, recorded and replayed, must print the same transcript — so "the
//! two paths are told to draw the same thing" is asserted rather than
//! assumed, even though what each makes of it is not.

use kite_driver::{compile, Emit};
use std::path::{Path, PathBuf};
use std::process::Command;

fn example_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/scripts.kite")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn run_on_vm() -> String {
    let path = example_path();
    let src = std::fs::read_to_string(&path).expect("read examples/scripts.kite");
    let c = compile(&path, &src, Emit::Check);
    assert!(
        !c.failed(),
        "scripts.kite does not compile:\n{}",
        c.render_diagnostics()
    );
    let mut out = Vec::new();
    c.run(&mut out)
        .unwrap_or_else(|t| panic!("scripts.kite trapped: {}", t));
    String::from_utf8(out).expect("utf-8")
}

/// Split the transcript at its `== name ==` markers.
fn sections(output: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in output.lines() {
        if let Some(name) = line.strip_prefix("== ").and_then(|l| l.strip_suffix(" ==")) {
            out.push((name.to_string(), String::new()));
        } else if let Some((_, body)) = out.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    out
}

/// Every script's transcript matches its committed golden file, byte for
/// byte. A change here is either a regression or a deliberate improvement,
/// and committing the new golden is how the second is distinguished from the
/// first — with the diff in review, where it can be argued with.
#[test]
fn the_transcripts_match_their_goldens() {
    let output = run_on_vm();
    let found = sections(&output);
    let expected = [
        "latin",
        "cyrillic",
        "arabic",
        "arabic-joined",
        "hebrew",
        "devanagari",
        "thai",
        "cjk",
        "burmese",
    ];
    assert_eq!(
        found.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        expected,
        "the example's sections drifted from the eight scripts (plus the joined arabic sample)"
    );
    let mut mismatches = Vec::new();
    for (name, body) in &found {
        let path = golden_dir().join(format!("{}.txt", name));
        // Line endings are settled before comparing: git hands Windows a
        // checkout with CRLF, and a golden that differed only in that would be
        // a failure about the checkout rather than about the text. What these
        // files record is the order and the advances of runs, and neither is
        // an `\r`.
        let golden = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("no golden at {}: {}", path.display(), e))
            .replace("\r\n", "\n");
        if &golden != body {
            mismatches.push(format!(
                "{}:\n  expected:\n{}\n  found:\n{}",
                name, golden, body
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The same program on the Wasm backend, twice over: drawn directly through
/// the text renderer, the transcript must match the VM's exactly; and
/// recorded then replayed — which is the path both real renderers take — the
/// draw calls must replay to the same lines. The first is the differential
/// claim, the second is the scene-graph claim. Neither says a pixel is
/// right; both say every pixel is asked for identically.
#[test]
fn both_backends_and_the_replay_print_the_same_transcript() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let vm = run_on_vm();

    let path = example_path();
    let src = std::fs::read_to_string(&path).expect("read examples/scripts.kite");
    let c = compile(&path, &src, Emit::Wasm);
    assert!(
        !c.failed(),
        "scripts.kite does not compile to wasm:\n{}",
        c.render_diagnostics()
    );
    let module = c.wasm.as_ref().expect("a module");
    let dir = std::env::temp_dir().join(format!("kite-golden-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("work directory");
    std::fs::write(dir.join("app.wasm"), &module.bytes).expect("write wasm");
    std::fs::write(
        dir.join("app.js"),
        kite_driver::generate_glue(&module.strings, "app.wasm"),
    )
    .expect("write glue");

    // Directly: the default text renderer describes each call.
    std::fs::write(
        dir.join("direct.mjs"),
        "import { readFile } from \"node:fs/promises\";\n\
         import { run, setWriter } from \"./app.js\";\n\
         const out = [];\n\
         setWriter((l) => out.push(l));\n\
         await run(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n\
         process.stdout.write(out.map((l) => l + \"\\n\").join(\"\"));\n",
    )
    .expect("write direct runner");
    let direct = Command::new("node")
        .arg(dir.join("direct.mjs"))
        .output()
        .expect("node runs");
    assert!(
        direct.status.success(),
        "direct run failed under node:\n{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    let wasm = String::from_utf8(direct.stdout).expect("utf-8");
    assert_eq!(
        vm, wasm,
        "the two backends printed different transcripts"
    );

    // Recorded and replayed: what the DOM and canvas renderers would be fed.
    std::fs::write(
        dir.join("replay.mjs"),
        "import { readFile } from \"node:fs/promises\";\n\
         import { run, setWriter, setRenderer, recordingRenderer, replay, textRenderer } from \"./app.js\";\n\
         const markers = [];\n\
         setWriter((l) => markers.push(l));\n\
         const recorder = recordingRenderer();\n\
         setRenderer(recorder);\n\
         await run(new Uint8Array(await readFile(new URL(\"./app.wasm\", import.meta.url))));\n\
         const out = [];\n\
         setWriter((l) => out.push(l));\n\
         replay(recorder.calls, textRenderer);\n\
         process.stdout.write(out.map((l) => l + \"\\n\").join(\"\"));\n",
    )
    .expect("write replay runner");
    let replayed = Command::new("node")
        .arg(dir.join("replay.mjs"))
        .output()
        .expect("node runs");
    assert!(
        replayed.status.success(),
        "replay run failed under node:\n{}",
        String::from_utf8_lossy(&replayed.stderr)
    );
    let replayed = String::from_utf8(replayed.stdout).expect("utf-8");
    let vm_draws: String = vm
        .lines()
        .filter(|l| !l.starts_with("== "))
        .map(|l| format!("{}\n", l))
        .collect();
    assert_eq!(
        vm_draws, replayed,
        "the recorded scene graph did not replay to the transcript"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
