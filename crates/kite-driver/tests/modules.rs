//! Module loading, qualification, and visibility.
//!
//! A module is a directory. Its declarations are merged qualified, so the name
//! a user writes — `config.load` — is literally the name the compiler holds,
//! and no name can be reached without saying where it came from.

use kite_driver::{compile, Emit};
use std::path::{Path, PathBuf};

/// A throwaway directory holding a small program and its modules.
struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let dir = std::env::temp_dir().join(format!("kite-mod-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create project");
        Project { dir }
    }

    fn file(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create module directory");
        }
        std::fs::write(&path, text).expect("write file");
        path
    }

    fn run(&self, main: &Path) -> Result<String, String> {
        let src = std::fs::read_to_string(main).expect("read main");
        let c = compile(main, &src, Emit::Check);
        if c.failed() {
            return Err(c.render_diagnostics());
        }
        let mut out = Vec::new();
        c.run(&mut out).expect("the program runs");
        Ok(String::from_utf8(out).expect("utf-8"))
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_sibling_file_is_a_module_reached_by_its_name() {
    let p = Project::new("sibling");
    p.file("config.kite", "pub fn port() -> int {\n  return 8080\n}\n");
    let main = p.file(
        "main.kite",
        "use config\n\nfn main() {\n  io.print(config.port())\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "8080\n");
}

/// Every `.kite` file in a directory contributes to the same namespace, with
/// no per-file imports between them.
#[test]
fn a_directory_is_one_module_across_its_files() {
    let p = Project::new("directory");
    p.file("shapes/area.kite", "pub fn area(r: float) -> float {\n  return r * side()\n}\n");
    p.file("shapes/side.kite", "fn side() -> float {\n  return 2.0\n}\n");
    let main = p.file(
        "main.kite",
        "use shapes\n\nfn main() {\n  io.print(shapes.area(3.0))\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "6.0\n");
}

/// The whole point of qualification: an imported name cannot be reached
/// without saying where it came from.
#[test]
fn an_imported_name_is_not_in_scope_unqualified() {
    let p = Project::new("unqualified");
    p.file("config.kite", "pub fn port() -> int {\n  return 8080\n}\n");
    let main = p.file(
        "main.kite",
        "use config\n\nfn main() {\n  io.print(port())\n}\n",
    );
    let err = p.run(&main).expect_err("must not resolve");
    assert!(err.contains("E0111"), "{}", err);
}

#[test]
fn a_module_may_declare_a_type_of_the_same_name_as_the_program() {
    let p = Project::new("same-name");
    p.file(
        "geometry.kite",
        "pub struct Point {\n  pub x: int\n}\n\
         pub fn origin() -> Point {\n  return Point{x: 0}\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use geometry\n\nstruct Point {\n  label: str\n}\n\
         fn main() {\n  let mine = Point{label: \"here\"}\n\
         \x20 let theirs = geometry.origin()\n\
         \x20 io.print(\"\\(mine.label) \\(theirs.x)\")\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "here 0\n");
}

#[test]
fn an_unmarked_declaration_is_private_to_its_module() {
    let p = Project::new("private");
    p.file("secrets.kite", "fn key() -> int {\n  return 42\n}\n");
    let main = p.file(
        "main.kite",
        "use secrets\n\nfn main() {\n  io.print(secrets.key())\n}\n",
    );
    let err = p.run(&main).expect_err("must be private");
    assert!(err.contains("E0401"), "{}", err);
    assert!(err.contains("private to module `secrets`"), "{}", err);
}

#[test]
fn an_alias_is_how_the_module_is_spelled() {
    let p = Project::new("alias");
    p.file("configuration.kite", "pub fn port() -> int {\n  return 3000\n}\n");
    let main = p.file(
        "main.kite",
        "use configuration as cfg\n\nfn main() {\n  io.print(cfg.port())\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "3000\n");
}

/// Cycles make separate compilation, incremental rebuilds and initialisation
/// order all harder, and every one can be broken by extracting the shared part.
#[test]
fn a_module_cycle_is_an_error() {
    let p = Project::new("cycle");
    p.file("a.kite", "use b\n\npub fn one() -> int {\n  return b.two()\n}\n");
    p.file("b.kite", "use a\n\npub fn two() -> int {\n  return 2\n}\n");
    let main = p.file("main.kite", "use a\n\nfn main() {\n  io.print(a.one())\n}\n");
    let err = p.run(&main).expect_err("a cycle must be reported");
    assert!(err.contains("E0402"), "{}", err);
}

#[test]
fn an_unknown_module_says_where_it_looked() {
    let p = Project::new("missing");
    let main = p.file("main.kite", "use nowhere\n\nfn main() {\n}\n");
    let err = p.run(&main).expect_err("no such module");
    assert!(err.contains("E0400"), "{}", err);
}

#[test]
fn an_unknown_standard_module_lists_the_ones_there_are() {
    let c = compile("t.kite", "use std/nope\n\nfn main() {\n}\n", Emit::Check);
    let err = c.render_diagnostics();
    assert!(err.contains("E0400"), "{}", err);
    assert!(err.contains("std/ui"), "{}", err);
}

/// A module reaching another one is ordinary; only a cycle is not.
#[test]
fn a_module_may_import_another_module() {
    let p = Project::new("transitive");
    p.file("units.kite", "pub fn double(n: int) -> int {\n  return n * 2\n}\n");
    p.file(
        "totals.kite",
        "use units\n\npub fn total(n: int) -> int {\n  return units.double(n) + 1\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use totals\n\nfn main() {\n  io.print(totals.total(5))\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "11\n");
}

/// Nothing is compiled that nothing asked for. A program that imports no
/// module carries no module.
#[test]
fn an_unimported_module_contributes_nothing() {
    let c = compile("t.kite", "fn main() {\n  io.print(1)\n}\n", Emit::Check);
    assert!(!c.failed(), "{}", c.render_diagnostics());
    let with_ui = compile(
        "t.kite",
        "fn main() {\n  io.print(ui.line_height())\n}\n",
        Emit::Check,
    );
    assert!(with_ui.failed(), "`ui` must not be in scope without `use std/ui`");
}

// ---- packages -------------------------------------------------------------

/// A dependency the manifest declares is reachable from anywhere in the
/// package, not only from beside the file that imports it.
#[test]
fn a_manifest_dependency_is_on_the_module_path() {
    let p = Project::new("manifest");
    p.file(
        "kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nshared = { path = \"shared\" }\n",
    );
    p.file(
        "shared/greeting.kite",
        "pub fn greet(name: str) -> str {\n  return \"hello, \\(name)\"\n}\n",
    );
    // The importer is in `src/`, and the dependency is not beside it.
    let main = p.file(
        "src/main.kite",
        "use shared\n\nfn main() {\n  io.print(shared.greet(\"kite\"))\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "hello, kite\n");
}

/// What a package depends on is what it said, not what happens to be lying
/// next to the file that imported it.
#[test]
fn a_declared_dependency_wins_over_a_sibling_of_the_same_name() {
    let p = Project::new("shadowing");
    p.file(
        "kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"real\" }\n",
    );
    p.file("real/lib.kite", "pub fn which() -> str {\n  return \"declared\"\n}\n");
    p.file("src/lib/other.kite", "pub fn which() -> str {\n  return \"sibling\"\n}\n");
    let main = p.file(
        "src/main.kite",
        "use lib\n\nfn main() {\n  io.print(lib.which())\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "declared\n");
}

/// A derive inside a module is placed in that module, which is what lets it
/// reach the type's private fields and lets an unqualified name in the
/// generated body mean what it means at the declaration.
///
/// The interesting half is the call site: `models.User.decode(doc)` names a
/// type in another module and reaches an associated function the compiler
/// wrote. Nothing about that path is special-cased — the generated `impl` is
/// an ordinary one, in the module the type is in.
#[test]
fn a_derive_lands_in_the_module_of_the_type_it_is_for() {
    let p = Project::new("derive-module");
    p.file(
        "models/user.kite",
        "use std/json\n\n\
         @derive(Debug, Hash, Encode, Decode)\n\
         pub struct User {\n    pub name: str\n    pub age: int\n}\n\n\
         @derive(Debug)\n\
         pub enum Role {\n    Reader\n    Editor(level: int)\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use models\nuse std/json\n\n\
         fn main() {\n\
         \x20 let u = models.User{ name: \"ada\", age: 36 }\n\
         \x20 io.print(u.debug())\n\
         \x20 io.print(json.stringify(u.encode()))\n\
         \x20 let (doc, err) = json.parse(\"{\\\"name\\\":\\\"grace\\\",\\\"age\\\":45}\")\n\
         \x20 if err != nil {\n    return\n  }\n\
         \x20 let (back, berr) = models.User.decode(doc)\n\
         \x20 if berr != nil {\n    io.print(berr.message())\n    return\n  }\n\
         \x20 io.print(back.debug())\n\
         \x20 io.print(models.Role.Editor(level: 2).debug())\n\
         }\n",
    );
    assert_eq!(
        p.run(&main).expect("compiles"),
        "User{ name: \"ada\", age: 36 }\n\
         {\"name\":\"ada\",\"age\":36}\n\
         User{ name: \"grace\", age: 45 }\n\
         Editor(level: 2)\n"
    );
}
