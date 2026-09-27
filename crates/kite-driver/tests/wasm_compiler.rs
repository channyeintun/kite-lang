//! The compiler as WebAssembly, held to the compiler as a binary.
//!
//! `@kite-lang/compiler-wasm` is what a bundler depends on, and its whole
//! claim is that it is not a second compiler: same crate, different target,
//! identical output. A claim like that is worth an assertion, because the day
//! it stops being true a project's build and its author's terminal start
//! disagreeing about what the program means — and nothing else would notice.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repository root")
}

fn kitec() -> PathBuf {
    // `cargo test` puts the test binary next to the compiler it built.
    let mut path = std::env::current_exe().expect("this test binary");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(if cfg!(windows) { "kitec.exe" } else { "kitec" })
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kite-wasmc-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("work directory");
    dir
}

/// The artefacts a bundler consumes, built both ways and compared byte for
/// byte — on the starter, because it is the case with a sibling module and
/// two `use std/…` imports rather than a single file.
#[test]
fn the_wasm_compiler_and_the_binary_build_identical_artefacts() {
    let compiler = kitec();
    let wasm = root().join("packages/kite-wasm/kite-compiler.wasm");
    if !compiler.exists() {
        eprintln!("skipping: no kitec binary at {}", compiler.display());
        return;
    }
    // Built by `packages/kite-wasm/build.sh` and not checked in, the way every
    // other build artefact here is not checked in.
    if !wasm.exists() {
        eprintln!("skipping: run packages/kite-wasm/build.sh first");
        return;
    }
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipping: node is not installed");
        return;
    }

    let dir = work_dir("identical");
    let entry = root().join("examples/vite-starter/src/main.kite");

    let native = dir.join("native");
    let built = Command::new(&compiler)
        .args([
            "build",
            entry.to_str().unwrap(),
            "--emit",
            "wasm",
            "--out",
            native.to_str().unwrap(),
        ])
        .output()
        .expect("kitec runs");
    assert!(
        built.status.success(),
        "the native build failed:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );

    let via_wasm = dir.join("wasm");
    let bin = root().join("packages/kite-wasm/kitec.js");
    let out = Command::new("node")
        .args([
            bin.to_str().unwrap(),
            "build",
            entry.to_str().unwrap(),
            "--out",
            via_wasm.to_str().unwrap(),
        ])
        .output()
        .expect("node runs");
    assert!(
        out.status.success(),
        "the WebAssembly build failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // The wrapper is written by both or by neither: the starter has no
    // `pub fn` of its own, and a program with no interface gets no `api.js`
    // from either. The source map is not compared — each names its sources
    // relative to where it was written, and the two were written apart.
    for name in ["api.js", "api.d.ts"] {
        assert_eq!(
            native.join(name).exists(),
            via_wasm.join(name).exists(),
            "{} is written by one compiler and not the other",
            name
        );
    }
    for name in ["app.wasm", "app.js", "api.js", "api.d.ts"] {
        if !native.join(name).exists() {
            continue;
        }
        let a = std::fs::read(native.join(name)).unwrap_or_else(|_| panic!("native {}", name));
        let b = std::fs::read(via_wasm.join(name)).unwrap_or_else(|_| panic!("wasm {}", name));
        assert_eq!(
            a,
            b,
            "{} differs between the WebAssembly compiler and the binary ({} vs {} bytes)",
            name,
            a.len(),
            b.len()
        );
    }
}

/// The WebAssembly compiler, or a reason to skip.
fn wasm_compiler() -> Option<PathBuf> {
    let wasm = root().join("packages/kite-wasm/kite-compiler.wasm");
    if !wasm.exists() {
        eprintln!("skipping: run packages/kite-wasm/build.sh first");
        return None;
    }
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipping: node is not installed");
        return None;
    }
    Some(root().join("packages/kite-wasm/compiler.js"))
}

/// Run a script with `compiler` imported from the package, and return what it
/// printed.
fn with_compiler(name: &str, compiler: &Path, script: &str) -> String {
    let dir = work_dir(name);
    let path = compiler.to_string_lossy().replace('\\', "/");
    let url = if path.starts_with('/') {
        format!("file://{}", path)
    } else {
        format!("file:///{}", path)
    };
    std::fs::write(
        dir.join("run.mjs"),
        format!("import {{ compiler }} from {:?};\n{}", url, script),
    )
    .expect("write script");
    let out = Command::new("node").arg(dir.join("run.mjs")).output().expect("node runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// The deepest program the front end accepts builds, and a call that traps
/// does not take the compiler with it.
///
/// With wasm-ld's default 1 MiB stack, eight hundred string literals joined
/// by `+` ran the compiler out of stack — `memory access out of bounds`, where
/// the native compiler handled a thousand — and every call on that instance
/// after it failed the same way, so one such file broke a Vite dev server
/// until it was restarted. The Wasm compiler has a 16 MiB stack now, and past
/// that a wasm call runs out of the JavaScript engine's own, which nothing in
/// the module can enlarge — so its parser refuses a chain past 1,024 links
/// (E0102). The longest chain it accepts builds here, and so does one of
/// method calls, the costliest kind, inside the deepest nest it accepts.
///
/// For the second half nothing ordinary traps any more, so a trap is
/// supplied: the first instance's `kite_run` is made to throw the way a trap
/// does, every time. The call that meets it throws; the next one runs on a
/// fresh instance, where it used to meet the same trap again.
#[test]
fn a_deep_input_neither_overflows_nor_breaks_the_compiler() {
    let Some(compiler) = wasm_compiler() else {
        return;
    };
    let script = r#"
const chain = (n) =>
  "fn main() {\n  let s = " +
  Array.from({ length: n }, (_, i) => `"line ${i}\\n"`).join(" +\n    ") +
  "\n  io.print(s.len())\n}\n";
const hello = 'fn main() {\n  io.print("ok")\n}\n';
const calls = (n) =>
  'fn main() {\n  let s = ' + "(".repeat(250) + '" x "' + ".trim()".repeat(n) + ")".repeat(250) +
  "\n  io.print(s)\n}\n";

// The first instance's `kite_run` traps, and — as after a real trap, whose
// memory is wherever it stopped — goes on trapping. Only a new instance works.
const instantiate = WebAssembly.instantiate;
WebAssembly.instantiate = async (...args) => {
  const result = await instantiate(...args);
  const exports = { ...result.exports ?? result.instance.exports };
  exports.kite_run = () => {
    throw new WebAssembly.RuntimeError("unreachable");
  };
  return result.instance ? { module: result.module, instance: { exports } } : { exports };
};

const c = await compiler();
console.log("1024 links: " + (c.build({ entry: chain(1025) })["app.wasm"].length > 0));
console.log("512 calls, 250 deep: " + (c.build({ entry: calls(512) })["app.wasm"].length > 0));
try {
  c.build({ entry: chain(1026) });
  console.log("1025 links: built");
} catch (e) {
  console.log("1025 links: " + (e.name === "BuildFailed" && e.message.includes("E0102")));
}
try {
  c.build({ entry: chain(20000) });
  console.log("20000 links: built");
} catch (e) {
  console.log("20000 links: " + (e.name === "BuildFailed" ? "refused" : "trapped"));
}
try {
  c.run(hello);
  console.log("the trap: not met");
} catch (e) {
  console.log("the trap: " + (e instanceof WebAssembly.RuntimeError));
}
console.log("afterwards: " + JSON.stringify(c.run(hello)));
"#;
    let out = with_compiler("deep", &compiler, script);
    assert_eq!(
        out,
        "1024 links: true\n512 calls, 250 deep: true\n1025 links: true\n20000 links: refused\n\
         the trap: true\nafterwards: \"ok\\n\"\n",
        "{}",
        out
    );
}

/// `build` answers with the files `kitec build` writes, and only those: the
/// source map a debug module names, and no wrapper for a program with no
/// interface of its own. Diagnostics name the file they are about.
#[test]
fn the_wasm_compiler_writes_what_kitec_writes() {
    let Some(compiler) = wasm_compiler() else {
        return;
    };
    let script = r#"
const c = await compiler();
const lib = c.build({ entry: "pub fn twice(n: int) -> int {\n  return n * 2\n}\nfn main() {\n}\n" });
console.log(Object.keys(lib).sort().join(" "));
const page = c.build({ entry: "fn main() {\n  io.print(1)\n}\n", release: true });
console.log(Object.keys(page).sort().join(" "));
console.log(c.checkModule({ entry: "fn main() {\n  let x: int = \"s\"\n}\n", path: "src/app.kite" })
  .includes("src/app.kite:2"));
"#;
    let out = with_compiler("files", &compiler, script);
    assert_eq!(
        out,
        "api.d.ts api.js app.js app.wasm app.wasm.map\napp.js app.wasm\ntrue\n",
        "{}",
        out
    );
}

/// `npx kitec check` fails on an error and not on a warning, as the native
/// `kitec` does — any output at all used to count as failure.
#[test]
fn check_fails_on_errors_and_not_on_warnings() {
    if wasm_compiler().is_none() {
        return;
    }
    let dir = work_dir("check");
    let warn = dir.join("warned.kite");
    std::fs::write(&warn, "fn main() {\n  let x = 1.5\n  io.print(x as int)\n}\n").expect("write");
    let bad = dir.join("broken.kite");
    std::fs::write(&bad, "fn main() {\n  let x: int = \"s\"\n}\n").expect("write");
    let bin = root().join("packages/kite-wasm/kitec.js");
    let run = |file: &Path| {
        Command::new("node")
            .args([bin.to_str().unwrap(), "check", file.to_str().unwrap()])
            .output()
            .expect("node runs")
    };
    let warned = run(&warn);
    let broken = run(&bad);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(warned.status.success(), "{}", String::from_utf8_lossy(&warned.stdout));
    assert!(
        String::from_utf8_lossy(&warned.stdout).contains("warned.kite:3"),
        "{}",
        String::from_utf8_lossy(&warned.stdout)
    );
    assert!(!broken.status.success(), "{}", String::from_utf8_lossy(&broken.stdout));
}

/// `kitec --version` works from an install path with a space in it.
///
/// It found `package.json` through `new URL(import.meta.url).pathname`, which
/// is percent-encoded — `sp%20ace` — and on Windows is `/C:/…`, so the file
/// was not there and the command crashed.
#[test]
fn version_works_from_a_path_with_a_space() {
    if wasm_compiler().is_none() {
        return;
    }
    let dir = work_dir("space").join("with a space");
    std::fs::create_dir_all(&dir).expect("directory");
    let package = root().join("packages/kite-wasm");
    for name in ["kitec.js", "compiler.js", "package.json"] {
        std::fs::copy(package.join(name), dir.join(name)).expect("copy");
    }
    let out = Command::new("node")
        .args([dir.join("kitec.js").to_str().unwrap(), "--version"])
        .output()
        .expect("node runs");
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("kitec "),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}
