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

/// An `impl` inside a module is held to its trait exactly as one in the
/// program's own file is. Its names used to be looked up from the root, where
/// they mean nothing, so a module's implementations went unchecked — and a
/// `dyn` call reached one with the wrong signature.
#[test]
fn an_impl_inside_a_module_is_checked_against_its_trait() {
    let p = Project::new("impl-in-module");
    p.file(
        "shapes/shapes.kite",
        "pub trait Area {\n  fn area(self) -> int\n  fn name(self) -> str\n}\n\n\
         pub struct Sq {\n  pub side: int\n}\n\n\
         impl Area for Sq {\n  fn area(self, extra: str) -> str {\n    return \"x\"\n  }\n}\n\n\
         pub struct Tri {\n  pub base: int\n}\n\n\
         impl Area for Tri {\n  fn area(self) -> int {\n    return 1\n  }\n\
         \x20 fn name(self) -> str {\n    return \"tri\"\n  }\n}\n\n\
         impl Area for Tri {\n  fn area(self) -> int {\n    return 2\n  }\n\
         \x20 fn name(self) -> str {\n    return \"tri\"\n  }\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use shapes\n\nfn main() {\n  io.print(shapes.Sq{ side: 3 }.side)\n}\n",
    );
    let err = p.run(&main).expect_err("the impls are wrong");
    assert!(err.contains("does not implement `name` of `Area`"), "{}", err);
    assert!(err.contains("takes 1 parameter, but the trait declares 0"), "{}", err);
    assert!(err.contains("E0112"), "{}", err);
}

/// A field, a method and an associated function are private to the module
/// that declares their type unless marked `pub` (§4.3) — and so is a type
/// named in a signature. Only top-level functions used to be held to it: an
/// importer could read and write a private field, forge a value through a
/// literal or a `..` update, take one apart in a pattern, and call a private
/// method.
#[test]
fn a_member_is_private_to_the_module_of_its_type() {
    let p = Project::new("private-members");
    p.file(
        "acct/acct.kite",
        "pub struct Conn {\n  pub host: str\n  secret: str\n  var retries: int\n}\n\n\
         struct Key {\n  code: int\n}\n\n\
         pub fn open(h: str) -> Conn {\n  return Conn{ host: h, secret: \"s\", retries: 0 }\n}\n\n\
         impl Conn {\n  fn reveal(self) -> str {\n    return self.secret\n  }\n\
         \x20 pub fn host_name(self) -> str {\n    return self.host\n  }\n\
         \x20 fn make() -> Conn {\n    return Conn{ host: \"m\", secret: \"m\", retries: 0 }\n  }\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use acct\n\n\
         fn take(k: acct.Key) -> int {\n  return 1\n}\n\n\
         fn main() {\n\
         \x20 var c = acct.open(\"h\")\n\
         \x20 io.print(c.host)\n\
         \x20 io.print(c.host_name())\n\
         \x20 io.print(c.secret)\n\
         \x20 c.retries = 5\n\
         \x20 let d = acct.Conn{ ..c, host: \"y\" }\n\
         \x20 match c {\n    acct.Conn{ secret, .. } => io.print(secret),\n  }\n\
         \x20 io.print(c.reveal())\n\
         \x20 let m = acct.Conn.make()\n\
         }\n",
    );
    let err = p.run(&main).expect_err("private members are private");
    for want in [
        "`acct.Key` is private to module `acct`",
        "field `secret` is private to module `acct`",
        "field `retries` is private to module `acct`",
        "`acct.Conn` cannot be built outside module `acct`",
        "method `reveal` is private to module `acct`",
        "associated function `make` is private to module `acct`",
    ] {
        assert!(err.contains(want), "missing {:?} in:\n{}", want, err);
    }
    assert!(!err.contains("`host`"), "a `pub` field was refused:\n{}", err);
    assert!(!err.contains("host_name"), "a `pub` method was refused:\n{}", err);
}

/// The methods of a trait implementation are as visible as the trait (§4.3).
/// Every trait method used to count as `pub`, so another module could call a
/// private trait's method — a default one included — although naming the
/// trait in a bound was refused.
#[test]
fn a_private_traits_methods_are_private_to_its_module() {
    let p = Project::new("private-trait-methods");
    p.file(
        "lib/lib.kite",
        "trait Secret {\n  fn reveal(self) -> str\n  fn twice(self) -> str {\n    return self.reveal() + self.reveal()\n  }\n}\n\n\
         pub trait Open {\n  fn show(self) -> str\n}\n\n\
         pub struct Thing {\n  pub n: int\n}\n\n\
         impl Secret for Thing {\n  fn reveal(self) -> str {\n    return \"revealed \\(self.n)\"\n  }\n}\n\n\
         impl Open for Thing {\n  fn show(self) -> str {\n    return self.twice()\n  }\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use lib\n\n\
         fn main() {\n\
         \x20 let t = lib.Thing{ n: 2 }\n\
         \x20 io.print(t.show())\n\
         \x20 io.print(t.reveal())\n\
         \x20 io.print(t.twice())\n\
         }\n",
    );
    let err = p.run(&main).expect_err("a private trait's methods are private");
    assert!(err.contains("method `reveal` is private to module `lib`"), "{}", err);
    assert!(err.contains("method `twice` is private to module `lib`"), "{}", err);
    assert!(err.contains("which is not marked `pub`"), "{}", err);
    assert!(!err.contains("`show`"), "a `pub` trait's method was refused:\n{}", err);

    let fine = p.file(
        "fine.kite",
        "use lib\n\nfn main() {\n  io.print(lib.Thing{ n: 2 }.show())\n}\n",
    );
    assert_eq!(p.run(&fine).expect("a pub trait's method is callable"), "revealed 2revealed 2\n");
}

/// The standard library's opaque handles are private for a reason: a key
/// built from an `int` is a key to whatever that number names.
#[test]
fn a_standard_librarys_opaque_value_cannot_be_forged() {
    let p = Project::new("forge");
    let main = p.file(
        "main.kite",
        "use std/crypto\n\nfn main() {\n  let k = crypto.Key{ handle: 3 }\n  io.print(1)\n}\n",
    );
    let err = p.run(&main).expect_err("the handle is private");
    assert!(err.contains("cannot be built outside module `crypto`"), "{}", err);
}

/// An `impl` belongs to the module that declares its type, or — for a trait
/// implementation — the trait (§8.2, §10.2). There are no extension methods,
/// and no third module may implement someone else's trait for someone else's
/// type. A trait of one's own may still be implemented for an imported type.
#[test]
fn an_impl_belongs_to_the_module_of_its_type_or_its_trait() {
    let p = Project::new("coherence");
    p.file(
        "acct/acct.kite",
        "pub struct Conn {\n  pub host: str\n}\n\n\
         pub fn open(h: str) -> Conn {\n  return Conn{ host: h }\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use acct\n\n\
         impl Display for acct.Conn {\n  fn show(self) -> str {\n    return \"conn\"\n  }\n}\n\n\
         impl acct.Conn {\n  fn extra(self) -> int {\n    return 7\n  }\n}\n\n\
         trait Named {\n  fn name(self) -> str\n}\n\n\
         impl Named for acct.Conn {\n  fn name(self) -> str {\n    return self.host\n  }\n}\n\n\
         fn main() {\n  io.print(acct.open(\"h\").name())\n}\n",
    );
    let err = p.run(&main).expect_err("both are outside their modules");
    assert!(err.contains("`Display` cannot be implemented for `acct.Conn` here"), "{}", err);
    assert!(err.contains("`acct.Conn` cannot be given methods outside module `acct`"), "{}", err);
    assert!(!err.contains("Named"), "a local trait for an imported type was refused:\n{}", err);
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
    assert!(err.contains("std/dom"), "{}", err);
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
    let with_dom = compile(
        "t.kite",
        "fn main() {\n  io.print(dom.title())\n}\n",
        Emit::Check,
    );
    assert!(
        with_dom.failed(),
        "`dom` must not be in scope without `use std/dom`"
    );
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

// ---- packages -----------------------------------------------------------------
//
// A dependency is a directory somewhere else, and the whole question is whether
// a module inside it means what it would mean at home. Both halves are tested,
// because a project is compiled two ways: from a filesystem by `kitec`, and
// from a map of sources by a bundler that has already read them.

/// A declared dependency's own imports resolve inside the dependency.
///
/// This did not hold. A one-file module never recorded its own directory, so
/// it resolved its imports from wherever it had been imported *from* — for a
/// sibling that is the same directory and nothing showed, but across a package
/// boundary it meant a dependency's `use helper` reached the application's
/// `helper.kite`. A dependency reading the program that depends on it, with no
/// diagnostic.
#[test]
fn a_dependency_reaches_its_own_modules_and_not_the_programs() {
    let p = Project::new("dep-siblings");
    p.file("kitex/kite.toml", "[package]\nname = \"kitex\"\nversion = \"0.1.0\"\n");
    p.file(
        "kitex/greet.kite",
        "use helper\n\npub fn hello() -> str {\n  return helper.who()\n}\n",
    );
    p.file("kitex/helper.kite", "pub fn who() -> str {\n  return \"the package\"\n}\n");
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nkitex = { path = \"../kitex\" }\n",
    );
    // The application has a `helper` of its own, never imported by `main`. The
    // dependency must not see it.
    p.file("app/src/helper.kite", "pub fn who() -> str {\n  return \"the application\"\n}\n");
    let main = p.file(
        "app/src/main.kite",
        "use kitex/greet\n\nfn main() {\n  io.print(greet.hello())\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "the package\n");
}

/// Two modules of one name, reached two ways, are still two modules.
#[test]
fn a_dependencys_module_does_not_displace_the_programs_own() {
    let p = Project::new("dep-distinct");
    p.file("kitex/kite.toml", "[package]\nname = \"kitex\"\nversion = \"0.1.0\"\n");
    p.file("kitex/doc.kite", "pub fn who() -> str {\n  return \"theirs\"\n}\n");
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nkitex = { path = \"../kitex\" }\n",
    );
    p.file("app/src/doc.kite", "pub fn who() -> str {\n  return \"ours\"\n}\n");
    let main = p.file(
        "app/src/main.kite",
        "use doc\nuse kitex/doc as shared\n\n\
         fn main() {\n  io.print(doc.who())\n  io.print(shared.who())\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "ours\ntheirs\n");
}

/// The same, for a host that hands the sources over instead — a bundler.
///
/// The keys are whole `use` paths. They were last segments, so an
/// application's `doc` and a package's `doc` were one entry and only one of
/// them existed: every `use kitex/doc` in the program silently reached
/// whichever the host happened to insert. A bundler that reads a manifest has
/// two of everything by construction, which is what made the flat namespace
/// the thing standing between packages and any build that is not a filesystem.
#[test]
fn provided_modules_are_keyed_by_their_whole_path() {
    let mut provided = std::collections::HashMap::new();
    provided.insert("doc".to_string(), "pub fn who() -> str {\n  return \"ours\"\n}\n".to_string());
    provided.insert(
        "kitex/doc".to_string(),
        "use helper\n\npub fn who() -> str {\n  return helper.who()\n}\n".to_string(),
    );
    // Both packages have a `helper`. The one inside `kitex` is the one
    // `kitex/doc` must see.
    provided.insert(
        "helper".to_string(),
        "pub fn who() -> str {\n  return \"the application\"\n}\n".to_string(),
    );
    provided.insert(
        "kitex/helper".to_string(),
        "pub fn who() -> str {\n  return \"the package\"\n}\n".to_string(),
    );

    let src = "use doc\nuse kitex/doc as shared\n\n\
               fn main() {\n  io.print(doc.who())\n  io.print(shared.who())\n}\n";
    let c = kite_driver::compile_provided("main.kite", src, Emit::Check, false, provided);
    assert!(!c.failed(), "{}", c.render_diagnostics());
    let mut out = Vec::new();
    c.run(&mut out).expect("the program runs");
    assert_eq!(String::from_utf8(out).expect("utf-8"), "ours\nthe package\n");
}

/// A package may not reach out to the program that depends on it.
///
/// An unqualified `use` inside a package names a sibling *of that package*.
/// When there is none, the answer is that there is none — not the
/// application's module of the same name, which is what a flat lookup would
/// have found and what no filesystem would ever have produced.
#[test]
fn a_package_cannot_reach_the_application_by_a_bare_name() {
    let mut provided = std::collections::HashMap::new();
    provided.insert(
        "kitex/doc".to_string(),
        "use helper\n\npub fn who() -> str {\n  return helper.who()\n}\n".to_string(),
    );
    provided.insert(
        "helper".to_string(),
        "pub fn who() -> str {\n  return \"the application\"\n}\n".to_string(),
    );
    let src = "use kitex/doc as shared\n\nfn main() {\n  io.print(shared.who())\n}\n";
    let c = kite_driver::compile_provided("main.kite", src, Emit::Check, false, provided);
    assert!(c.failed(), "a package reached the application's `helper`");
    let said = c.render_diagnostics();
    assert!(said.contains("cannot find module `helper`"), "{}", said);
}

// ---- a module reaches only what it imports ------------------------------------

/// A qualified name resolves only where it was imported.
///
/// It used to resolve everywhere. Declarations are merged under their
/// qualified names, so `b.secret` was an item in the program the moment
/// *anybody* loaded `b`, and the lookup of a name as written found it from any
/// module. Whether a name resolved in one file depended on a `use` line in a
/// file that had nothing to do with it.
#[test]
fn a_module_cannot_reach_another_it_did_not_import() {
    let p = Project::new("unimported");
    p.file("b.kite", "pub fn secret() -> str {\n  return \"b\"\n}\n");
    // No `use b` in this file. `main` has one, which used to be enough.
    p.file("a.kite", "pub fn go() -> str {\n  return b.secret()\n}\n");
    let main = p.file(
        "main.kite",
        "use a\nuse b\n\nfn main() {\n  io.print(a.go())\n}\n",
    );
    let said = p.run(&main).expect_err("`b` is not imported in a.kite");
    assert!(said.contains("cannot find `b`"), "{}", said);
}

/// The same across two directory modules, where there is no argument about
/// what a module is.
#[test]
fn a_directory_module_cannot_reach_a_sibling_it_did_not_import() {
    let p = Project::new("unimported-dirs");
    p.file("one/x.kite", "pub fn thing() -> str {\n  return \"one\"\n}\n");
    p.file("two/y.kite", "pub fn go() -> str {\n  return one.thing()\n}\n");
    let main = p.file(
        "main.kite",
        "use one\nuse two\n\nfn main() {\n  io.print(two.go())\n}\n",
    );
    let said = p.run(&main).expect_err("`one` is not imported in two/y.kite");
    assert!(said.contains("cannot find `one`"), "{}", said);
}

/// With the import, it resolves — under its own name and under an alias.
#[test]
fn an_imported_module_resolves_however_it_is_spelled() {
    let p = Project::new("imported");
    p.file("b.kite", "pub fn secret() -> str {\n  return \"b\"\n}\n");
    p.file("a.kite", "use b\n\npub fn go() -> str {\n  return b.secret()\n}\n");
    p.file(
        "c.kite",
        "use b as bee\n\npub fn go() -> str {\n  return bee.secret()\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use a\nuse c\n\nfn main() {\n  io.print(a.go())\n  io.print(c.go())\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "b\nb\n");
}

/// The entry file is nobody's import either.
///
/// Its declarations are the only ones left unqualified, so a bare name looked
/// up in the merged list reached them from any module — and there is no `use`
/// that could have asked for it, because nothing can import the entry.
#[test]
fn a_module_cannot_reach_the_entry_file() {
    let p = Project::new("entry-reach");
    p.file("sub/y.kite", "pub fn go() -> str {\n  return top()\n}\n");
    let main = p.file(
        "main.kite",
        "use sub\n\npub fn top() -> str {\n  return \"entry\"\n}\n\n\
         fn main() {\n  io.print(sub.go())\n}\n",
    );
    let said = p.run(&main).expect_err("the entry is not importable");
    assert!(said.contains("cannot find `top`"), "{}", said);
}

/// A type at the head of a dotted name is not a module and is not gated.
///
/// `User.decode`, `Role.Editor` and `NotFound.is` all look like a module
/// access and none of them is one.
#[test]
fn a_type_at_the_head_of_a_name_is_not_gated() {
    let p = Project::new("type-head");
    p.file(
        "models.kite",
        "use std/json\n\n@derive(Debug, Encode, Decode)\n\
         pub struct User {\n    pub name: str\n}\n",
    );
    p.file(
        "sub.kite",
        "use models\n\npub fn go() -> str {\n\
         \x20 let u = models.User{ name: \"ada\" }\n  return u.debug()\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use sub\n\nfn main() {\n  io.print(sub.go())\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "User{ name: \"ada\" }\n");
}

/// A value widened to an `Option` on the value side of a fallible return.
///
/// `return t, nil` where the function answers `(Option<Thing>, error)` checked
/// clean and then emitted a module the engine refused: the checker waved the
/// `Thing` through — which is right, a `T` may stand where an `Option<T>` is
/// wanted — but never inserted the widening node, so the backend put a bare
/// `Thing` into a slot typed `Option<Thing>`. `kitec check` said ok and
/// `kitec build` said E0900, which is the worst pairing: the tool that runs
/// in an editor was happy and the one that ships was not.
///
/// The error side of the same expression had always been coerced. This is the
/// value side of it.
#[test]
fn a_value_widened_to_an_option_in_a_fallible_return_builds() {
    let p = Project::new("widen-fallible");
    let main = p.file(
        "main.kite",
        "struct Thing {\n  n: int\n}\n\n\
         fn make() -> (Option<Thing>, error) {\n\
         \x20 let t = Thing{ n: 7 }\n\
         \x20 return t, nil\n\
         }\n\n\
         fn main() {\n\
         \x20 let (t, err) = make()\n\
         \x20 if err != nil {\n    return\n  }\n\
         \x20 if t == nil {\n    return\n  }\n\
         \x20 io.print(t.n)\n\
         }\n",
    );
    // Runs, and — the half that was broken — compiles to a module that
    // validates.
    assert_eq!(p.run(&main).expect("compiles"), "7\n");

    let src = std::fs::read_to_string(&main).expect("read");
    let wasm = kite_driver::compile(&main, &src, Emit::Wasm);
    assert!(!wasm.failed(), "{}", wasm.render_diagnostics());
}

// ---- a module is where its source is -------------------------------------------
//
// A module used to be identified by its `use` path as the importer wrote it,
// so `use util` in `a/` and `use util` in `b/` were one module to the loader.
// The second was a false collision when both existed, and — worse — was
// silently answered by the first when its own did not.

/// Two directories may each have a `util` of their own. §13.1: a spelling
/// belongs to the file that writes it, so two files may spell different
/// modules alike.
#[test]
fn two_directories_may_each_have_a_util_of_their_own() {
    let p = Project::new("two-utils");
    p.file("a/a.kite", "use util\n\npub fn go() -> str {\n  return util.name()\n}\n");
    p.file("a/util/util.kite", "pub fn name() -> str {\n  return \"a-util\"\n}\n");
    p.file("b/b.kite", "use util\n\npub fn go() -> str {\n  return util.name()\n}\n");
    p.file("b/util/util.kite", "pub fn name() -> str {\n  return \"b-util\"\n}\n");
    let main = p.file(
        "main.kite",
        "use a\nuse b\n\nfn main() {\n  io.print(a.go())\n  io.print(b.go())\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "a-util\nb-util\n");
}

/// A module that lacks one is told so, rather than handed another module's.
///
/// This printed "a's private util" twice: `b`'s `use util` found nothing, was
/// compared against the `util` `a` had loaded under the same spelling, and was
/// answered by it — a module reading another module's private code with no
/// diagnostic, and only because of the order of two `use` lines in `main`.
#[test]
fn a_missing_module_is_not_answered_by_another_of_the_same_spelling() {
    let p = Project::new("missing-util");
    p.file("a/a.kite", "use util\n\npub fn go() -> str {\n  return util.name()\n}\n");
    p.file("a/util/util.kite", "pub fn name() -> str {\n  return \"a's private util\"\n}\n");
    p.file("b/b.kite", "use util\n\npub fn go() -> str {\n  return util.name()\n}\n");
    let main = p.file(
        "main.kite",
        "use a\nuse b\n\nfn main() {\n  io.print(a.go())\n  io.print(b.go())\n}\n",
    );
    let said = p.run(&main).expect_err("`b` has no util");
    assert!(said.contains("E0400"), "{}", said);
    assert!(said.contains("cannot find module `util`"), "{}", said);
    // A diagnostic names the file the way the platform spells its path.
    assert!(said.replace('\\', "/").contains("b/b.kite"), "the error is at b's `use`: {}", said);
}

/// The same across a package boundary, which is the case Phase 28 set out to
/// close: a dependency's `use helper` bound to the application's `helper`
/// whenever the application had imported its own first.
#[test]
fn a_dependency_cannot_reach_an_application_module_it_lacks() {
    let p = Project::new("dep-lacks-helper");
    p.file("kitex/kite.toml", "[package]\nname = \"kitex\"\nversion = \"0.1.0\"\n");
    p.file(
        "kitex/greet.kite",
        "use helper\n\npub fn hello() -> str {\n  return helper.who()\n}\n",
    );
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nkitex = { path = \"../kitex\" }\n",
    );
    p.file(
        "app/src/helper.kite",
        "pub fn who() -> str {\n  return \"the application (private data)\"\n}\n",
    );
    let main = p.file(
        "app/src/main.kite",
        "use helper\nuse kitex/greet\n\n\
         fn main() {\n  io.print(helper.who())\n  io.print(greet.hello())\n}\n",
    );
    let said = p.run(&main).expect_err("the package has no helper");
    assert!(said.contains("cannot find module `helper`"), "{}", said);
    assert!(said.contains("greet.kite"), "the error is in the package: {}", said);
}

/// The application's `util` and a dependency's own `util` are two modules.
#[test]
fn an_application_util_and_a_dependencys_util_are_two_modules() {
    let p = Project::new("app-and-dep-util");
    p.file("md/kite.toml", "[package]\nname = \"md\"\nversion = \"1.0.0\"\n");
    p.file("md/md.kite", "use util\n\npub fn render() -> str {\n  return util.me()\n}\n");
    p.file("md/util.kite", "pub fn me() -> str {\n  return \"md-util\"\n}\n");
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nmd = { path = \"../md\" }\n",
    );
    p.file("app/src/util.kite", "pub fn me() -> str {\n  return \"app-util\"\n}\n");
    let main = p.file(
        "app/src/main.kite",
        "use md\nuse util\n\nfn main() {\n  io.print(md.render())\n  io.print(util.me())\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "md-util\napp-util\n");
}

/// `x` beside the entry and `lib/x` inside `lib` are two modules, so a chain
/// through both is not a cycle. It was reported as one, because both were
/// spelled `x`.
#[test]
fn a_nested_module_of_the_same_spelling_is_not_a_cycle() {
    let p = Project::new("not-a-cycle");
    p.file("x.kite", "use lib\n\npub fn top() -> str {\n  return lib.mid()\n}\n");
    p.file("lib/lib.kite", "use x\n\npub fn mid() -> str {\n  return x.leaf()\n}\n");
    p.file("lib/x/x.kite", "pub fn leaf() -> str {\n  return \"leaf\"\n}\n");
    let main = p.file("main.kite", "use x\n\nfn main() {\n  io.print(x.top())\n}\n");
    assert_eq!(p.run(&main).expect("compiles"), "leaf\n");
}

/// A diagnostic names a module by where it is, so two `util`s are told apart
/// in what the compiler says as well as in what it does.
#[test]
fn a_privacy_error_names_the_module_by_where_it_is() {
    let p = Project::new("identity-in-diagnostic");
    p.file("a/a.kite", "use util\n\npub fn go() -> str {\n  return util.hidden()\n}\n");
    p.file("a/util/util.kite", "fn hidden() -> str {\n  return \"x\"\n}\n");
    let main = p.file("main.kite", "use a\n\nfn main() {\n  io.print(a.go())\n}\n");
    let said = p.run(&main).expect_err("private");
    assert!(said.contains("private to module `a/util`"), "{}", said);
}

// ---- each package's dependencies are its own ---------------------------------

/// A dependency may use what it declares. There was one table of
/// dependencies, read from the program's manifest, so this looked for `b`
/// inside `a` and failed.
#[test]
fn a_dependency_uses_what_it_declares() {
    let p = Project::new("transitive-deps");
    p.file(
        "a/kite.toml",
        "[package]\nname = \"a\"\nversion = \"1.0.0\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
    );
    p.file("a/a.kite", "use b\n\npub fn hello() -> str {\n  return b.name()\n}\n");
    p.file("b/kite.toml", "[package]\nname = \"b\"\nversion = \"1.0.0\"\n");
    p.file("b/b.kite", "pub fn name() -> str {\n  return \"from b\"\n}\n");
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\na = { path = \"../a\" }\n",
    );
    let main = p.file("app/main.kite", "use a\n\nfn main() {\n  io.print(a.hello())\n}\n");
    assert_eq!(p.run(&main).expect("compiles"), "from b\n");
}

/// And may not use what only the program declares — §13.2: there is no
/// transitive-dependency hoisting.
#[test]
fn a_dependency_cannot_use_what_only_the_program_declares() {
    let p = Project::new("no-hoisting");
    p.file("a/kite.toml", "[package]\nname = \"a\"\nversion = \"1.0.0\"\n");
    p.file("a/a.kite", "use c\n\npub fn hello() -> str {\n  return c.name()\n}\n");
    p.file("c/kite.toml", "[package]\nname = \"c\"\nversion = \"1.0.0\"\n");
    p.file("c/c.kite", "pub fn name() -> str {\n  return \"from c\"\n}\n");
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\na = { path = \"../a\" }\nc = { path = \"../c\" }\n",
    );
    let main = p.file("app/main.kite", "use a\n\nfn main() {\n  io.print(a.hello())\n}\n");
    let said = p.run(&main).expect_err("`a` does not declare `c`");
    assert!(said.contains("cannot find module `c`"), "{}", said);
}

/// A git dependency a dependency declares is where `kitec pkg` put it: in the
/// program's `.kite/vendor`, which is the one place it puts every package.
#[test]
fn a_dependencys_git_dependency_is_read_from_the_programs_vendor() {
    let p = Project::new("vendored-transitive");
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\na = { git = \"https://example.com/a\", tag = \"v1.0.0\" }\n",
    );
    p.file(
        "app/.kite/vendor/a/kite.toml",
        "[package]\nname = \"a\"\nversion = \"1.0.0\"\n\n\
         [dependencies]\nb = { git = \"https://example.com/b\", tag = \"v2.0.0\" }\n",
    );
    p.file(
        "app/.kite/vendor/a/a.kite",
        "use b\n\npub fn hello() -> str {\n  return b.name()\n}\n",
    );
    p.file("app/.kite/vendor/b/kite.toml", "[package]\nname = \"b\"\nversion = \"2.0.0\"\n");
    p.file("app/.kite/vendor/b/b.kite", "pub fn name() -> str {\n  return \"vendored b\"\n}\n");
    let main = p.file("app/src/main.kite", "use a\n\nfn main() {\n  io.print(a.hello())\n}\n");
    assert_eq!(p.run(&main).expect("compiles"), "vendored b\n");
}

/// Two packages of one name, from two places, are refused rather than one of
/// them silently answering for both — the rule `kitec pkg` already applies to
/// the same manifests: a name means one thing.
#[test]
fn two_packages_of_one_name_from_two_places_are_refused() {
    let p = Project::new("two-shared");
    for (dir, who) in [("s1", "first"), ("s2", "second")] {
        p.file(
            &format!("{}/kite.toml", dir),
            "[package]\nname = \"shared\"\nversion = \"1.0.0\"\n",
        );
        p.file(
            &format!("{}/shared.kite", dir),
            &format!("pub fn who() -> str {{\n  return \"{}\"\n}}\n", who),
        );
    }
    p.file(
        "a/kite.toml",
        "[package]\nname = \"a\"\nversion = \"1.0.0\"\n\n[dependencies]\nshared = { path = \"../s1\" }\n",
    );
    p.file("a/a.kite", "use shared\n\npub fn go() -> str {\n  return shared.who()\n}\n");
    p.file(
        "b/kite.toml",
        "[package]\nname = \"b\"\nversion = \"1.0.0\"\n\n[dependencies]\nshared = { path = \"../s2\" }\n",
    );
    p.file("b/b.kite", "use shared\n\npub fn go() -> str {\n  return shared.who()\n}\n");
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\na = { path = \"../a\" }\nb = { path = \"../b\" }\n",
    );
    let main = p.file(
        "app/main.kite",
        "use a\nuse b\n\nfn main() {\n  io.print(a.go())\n  io.print(b.go())\n}\n",
    );
    let said = p.run(&main).expect_err("`shared` is two packages");
    assert!(said.contains("E0404"), "{}", said);
    assert!(said.contains("a package's name means one thing"), "{}", said);
}

/// A manifest that does not parse is reported where it is wrong. It was
/// treated as no manifest, so a typo in `[package]` read as `cannot find
/// module` at every `use` of a dependency.
#[test]
fn a_manifest_that_does_not_read_is_reported_at_its_line() {
    let p = Project::new("broken-manifest");
    p.file("a/a.kite", "pub fn hello() -> str {\n  return \"a\"\n}\n");
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
         [dependencies]\na = { path = \"../a\" }\n",
    );
    let main = p.file("app/main.kite", "use a\n\nfn main() {\n  io.print(a.hello())\n}\n");
    let said = p.run(&main).expect_err("the manifest does not read");
    assert!(said.contains("E0405"), "{}", said);
    assert!(said.contains("kite.toml:4"), "points at the line: {}", said);
    assert!(said.contains("has no `edition`"), "{}", said);
    // And the `use` it broke says so, where an editor showing only the
    // file's own diagnostics shows it.
    assert!(said.contains("kite.toml` did not read (E0405)"), "{}", said);
}

/// A directory module checked on its own, as an editor renaming a name its
/// files share asks for it: every file's uses of a private name are that
/// name's, and they are found although no file of the module was the entry.
#[test]
fn a_directory_module_checked_whole_finds_every_files_uses() {
    let p = Project::new("check-module");
    let load = p.file(
        "config/load.kite",
        "use util\n\nfn helper() -> int {\n  return util.base()\n}\n",
    );
    let schema = p.file(
        "config/schema.kite",
        "pub fn port() -> int {\n  return helper() + helper()\n}\n",
    );
    p.file("config/util.kite", "pub fn base() -> int {\n  return 40\n}\n");
    let c = kite_driver::check_module(p.dir.join("config"), kite_driver::modules::Files::Disk);
    assert!(!c.failed(), "{}", c.render_diagnostics());
    let helper = c
        .index
        .bindings
        .iter()
        .find(|b| b.name.ends_with(".helper"))
        .expect("helper is a binding");
    assert_eq!(c.sources.file(helper.declared_at.file).name, load);
    let in_schema = helper.uses.iter().filter(|s| c.sources.file(s.file).name == schema).count();
    assert_eq!(in_schema, 2, "{:?}", helper.uses);
}

/// A file not yet saved, in a directory not yet created, under a linked
/// directory: the loader asks for the directory by where it really is, and
/// the buffer is keyed the same way. The buffer was keyed by the linked
/// spelling, the two never met, and the module was `cannot find module`.
#[cfg(unix)]
#[test]
fn an_unsaved_directory_under_a_linked_directory_is_a_module() {
    let p = Project::new("linked-unsaved");
    let link = std::env::temp_dir().join(format!("kite-mod-linked-{}", std::process::id()));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&p.dir, &link).expect("a link");
    let files = kite_driver::modules::Files::edited(vec![(
        link.join("fresh/new.kite"),
        "pub fn v() -> int {\n  return 3\n}\n".to_string(),
    )]);
    let src = "use fresh\n\nfn main() {\n  io.print(fresh.v())\n}\n";
    let c = kite_driver::compile_files(link.join("main.kite"), src, Emit::Check, false, files);
    let _ = std::fs::remove_file(&link);
    assert!(!c.failed(), "{}", c.render_diagnostics());
}

/// `prelude` is reserved like the standard library's names. A module of that
/// name was accepted, and since the prelude is found by its name from
/// everywhere, its declarations became every module's unqualified fallback —
/// private ones included.
#[test]
fn a_module_called_prelude_is_refused() {
    let p = Project::new("prelude-module");
    p.file("prelude.kite", "fn sneaky() -> int {\n  return 1\n}\n");
    let main = p.file("main.kite", "use prelude\n\nfn main() {\n  io.print(sneaky())\n}\n");
    let said = p.run(&main).expect_err("reserved");
    assert!(said.contains("E0403"), "{}", said);
}

/// A module spelled like one the standard library puts in every file is
/// refused. Only a path's last segment was checked, never the name after
/// `as`, so `use util as errors` was accepted and one spelling reached two
/// modules: `errors.new` stayed the standard library's, `errors.only` reached
/// `util`, and `use util as io` left `io.print` the builtin with no word said.
#[test]
fn a_module_spelled_like_an_always_available_one_is_refused() {
    let p = Project::new("reserved-spelling");
    p.file(
        "util.kite",
        "pub fn new(s: str) -> str {\n  return \"mine \" + s\n}\n\n\
         pub fn print(s: str) {\n}\n",
    );
    for (spelling, body) in [
        ("errors", "io.print(errors.new(\"x\").message())"),
        ("io", "io.print(\"which print?\")"),
        ("prelude", "io.print(prelude.new(\"x\"))"),
        ("json", "io.print(json.new(\"x\"))"),
    ] {
        let main = p.file(
            &format!("main_{}.kite", spelling),
            &format!("use util as {}\n\nfn main() {{\n  {}\n}}\n", spelling, body),
        );
        let said = p.run(&main).expect_err("reserved");
        assert!(said.contains("E0403"), "{}: {}", spelling, said);
        assert!(said.contains(&format!("`{}` is the name of", spelling)), "{}", said);
    }
    // And a sibling named after one, which the last segment was already
    // checked for — `io` is one now too.
    p.file("io.kite", "pub fn print(s: str) {\n}\n");
    let main = p.file("main_io_file.kite", "use io\n\nfn main() {\n  io.print(\"x\")\n}\n");
    assert!(p.run(&main).expect_err("reserved").contains("E0403"));
    // A `std` module spelled as itself is its own spelling, and any other
    // name is free.
    let main = p.file(
        "main_ok.kite",
        "use std/errors as errors\nuse std/json as json\nuse util as mine\n\n\
         fn main() {\n  io.print(json.stringify(json.Json.Null))\n  io.print(mine.new(\"x\"))\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "null\nmine x\n");
}

/// A standard module is `std/<name>`, exactly. Only the last segment used to
/// be read, so any path ending in `json` under `std` was `std/json`.
#[test]
fn a_standard_module_is_one_segment_under_std() {
    let c = compile("t.kite", "use std/bogus/nested/json\n\nfn main() {\n}\n", Emit::Check);
    let said = c.render_diagnostics();
    assert!(said.contains("no standard module `std/bogus/nested/json`"), "{}", said);
}

// ---- the entry file and bare variants go through the gate too -----------------

/// The entry reaches only what it imports.
///
/// Its own declarations are the only unqualified ones, so the first lookup
/// step — "the asking module's own" — turned `secret.describe` into
/// `secret.describe` and found module `secret`'s item. That step is ungated,
/// so importing `helper`, which imports `secret`, was enough to call `secret`.
#[test]
fn the_entry_cannot_reach_a_module_only_its_imports_import() {
    let p = Project::new("entry-gate");
    p.file("secret.kite", "pub fn describe() -> str {\n  return \"secret\"\n}\n");
    p.file(
        "helper.kite",
        "use secret\n\npub fn hi() -> str {\n  return secret.describe()\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use helper\n\nfn main() {\n  io.print(secret.describe())\n  io.print(helper.hi())\n}\n",
    );
    let said = p.run(&main).expect_err("`secret` is not imported by main");
    assert!(said.contains("cannot find `secret`"), "{}", said);
}

/// A bare variant is one of the asking module's own enums' or the prelude's.
///
/// The index was one table for the whole program, so `let m = Magic` reached
/// a private enum in a module the entry never imported.
#[test]
fn a_bare_variant_does_not_reach_another_modules_enum() {
    let p = Project::new("bare-variant");
    p.file("secret.kite", "enum Hidden {\n  Magic\n  Other\n}\n\npub fn ok() -> str {\n  return \"ok\"\n}\n");
    p.file("helper.kite", "use secret\n\npub fn hi() -> str {\n  return secret.ok()\n}\n");
    let main = p.file(
        "main.kite",
        "use helper\n\nfn main() {\n  let m = Magic\n  io.print(helper.hi())\n}\n",
    );
    let said = p.run(&main).expect_err("`Magic` is another module's");
    assert!(said.contains("cannot find `Magic`"), "{}", said);
}

/// Two modules may each name a variant alike: neither is ambiguous in the
/// other. A program's `Token.Number` made `Number` ambiguous inside
/// `std/json`, which then failed to compile its own `match`.
#[test]
fn a_variant_name_is_not_ambiguous_across_modules() {
    let p = Project::new("variant-scope");
    let main = p.file(
        "main.kite",
        "use std/json\n\nenum Token {\n  Number(float)\n  Word(str)\n}\n\n\
         fn main() {\n\
         \x20 let (doc, err) = json.parse(\"[1, 2]\")\n\
         \x20 if err != nil {\n    return\n  }\n\
         \x20 io.print(json.stringify(doc))\n\
         \x20 let t = Number(1.5)\n\
         \x20 match t {\n    Number(n) => io.print(n),\n    Word(w) => io.print(w),\n  }\n\
         }\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "[1,2]\n1.5\n");
}

/// A derive walks a field typed through an aliased module the way the module
/// spells it. `a: m.Point` under `use models as m` read as "no such type".
#[test]
fn a_derive_walks_a_field_typed_through_an_aliased_module() {
    let p = Project::new("derive-aliased-field");
    p.file(
        "lib/models.kite",
        "@derive(Debug)\npub struct Point {\n  pub x: int\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use lib/models as m\n\n@derive(Debug)\nstruct Line {\n  a: m.Point\n}\n\n\
         fn main() {\n  io.print(Line{ a: m.Point{ x: 1 } }.debug())\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "Line{ a: Point{ x: 1 } }\n");
}

/// A module that derives `Encode` under its own spelling of `std/json`, used
/// from a program that spells it differently.
#[test]
fn a_json_derive_in_a_module_uses_that_modules_spelling() {
    let p = Project::new("derive-json-spelling");
    p.file(
        "models.kite",
        "use std/json as j\n\n@derive(Encode)\npub struct P {\n  pub x: int\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use std/json\nuse models\n\n\
         fn main() {\n  io.print(json.stringify(models.P{ x: 1 }.encode()))\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "{\"x\":1}\n");
}

/// A derived body binds no name the module spells a module by. A derived
/// `decode` takes a parameter `doc` and names nested types by the module's
/// spelling, so under `use g as doc` it asked the parameter for `doc.Id`.
#[test]
fn a_derive_binds_no_name_its_module_spells_a_module_by() {
    let p = Project::new("derive-local-spelling");
    p.file("g/g.kite", "@derive(Debug, Encode, Decode)\npub struct Id {\n  pub n: int\n}\n");
    for spelling in ["doc", "src_1", "tag_1"] {
        let main = p.file(
            &format!("main_{}.kite", spelling),
            &format!(
                "use g as {s}\n\n\
                 @derive(Debug, Encode, Decode)\nenum E {{\n  A\n  B(id: {s}.Id)\n}}\n\n\
                 @derive(Debug, Encode, Decode)\nstruct P {{\n  id: {s}.Id\n  e: E\n}}\n\n\
                 fn main() {{\n\
                 \x20 let (p, err) = P.decode(P{{ id: {s}.Id{{ n: 1 }}, e: E.B(id: {s}.Id{{ n: 2 }}) }}.encode())\n\
                 \x20 if err == nil {{\n    io.print(p.debug())\n  }}\n}}\n",
                s = spelling
            ),
        );
        assert_eq!(
            p.run(&main).expect("compiles"),
            "P{ id: Id{ n: 1 }, e: B(id: Id{ n: 2 }) }\n",
            "spelled `{}`",
            spelling
        );
    }
}

/// A derive inside a module reaches the prelude's helpers, not the module's
/// own functions of the same names. It called them unqualified, and a bare
/// name finds the module's own first.
#[test]
fn a_derive_in_a_module_reaches_the_prelude_past_its_own_helpers() {
    let p = Project::new("derive-prelude-helpers");
    p.file(
        "shapes/shapes.kite",
        "@derive(Debug, Hash)\npub struct P {\n  pub name: str\n  pub n: int\n}\n",
    );
    p.file(
        "shapes/helpers.kite",
        "fn debug_str(text: str) -> str {\n  return text\n}\n\n\
         fn hash_int(value: int) -> int {\n  return 0\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use shapes\n\nfn main() {\n\
         \x20 io.print(shapes.P{ name: \"q\", n: 1 }.debug())\n\
         \x20 io.print(shapes.P{ name: \"q\", n: 1 }.hash() == shapes.P{ name: \"q\", n: 2 }.hash())\n\
         }\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "P{ name: \"q\", n: 1 }\nfalse\n");
}

// ---- a package the program never declared ------------------------------------

/// The program's own module and a package only a dependency declares may
/// share a name. Both were identified as `b`, so whichever loaded second was
/// refused as a second package of one name — the application's `use b`, or
/// the dependency's, depending on the order of the program's `use` lines.
#[test]
fn a_program_module_may_share_a_name_with_a_package_it_never_declared() {
    let p = Project::new("undeclared-package-name");
    p.file(
        "a/kite.toml",
        "[package]\nname = \"a\"\nversion = \"1.0.0\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
    );
    p.file("a/a.kite", "use b\n\npub fn hello() -> str {\n  return \"a+\" + b.name()\n}\n");
    p.file("b/kite.toml", "[package]\nname = \"b\"\nversion = \"1.0.0\"\n");
    p.file("b/b.kite", "use util\n\npub fn name() -> str {\n  return util.who()\n}\n");
    p.file("b/util.kite", "pub fn who() -> str {\n  return \"from b\"\n}\n");
    let manifest =
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\na = { path = \"../a\" }\n";
    p.file("app/kite.toml", manifest);
    p.file("app/b.kite", "pub fn mine() -> str {\n  return \"the app's own b\"\n}\n");
    let body = "fn main() {\n  io.print(a.hello())\n  io.print(b.mine())\n}\n";
    for (name, uses) in [("main.kite", "use a\nuse b\n"), ("main2.kite", "use b\nuse a\n")] {
        let main = p.file(&format!("app/{}", name), &format!("{}\n{}", uses, body));
        assert_eq!(p.run(&main).expect("compiles"), "a+from b\nthe app's own b\n", "{}", name);
    }
    // The same for a nested path: the application's `b/util` and the
    // package's `util`, which it named `b/util` too.
    p.file("nested/kite.toml", manifest);
    p.file("nested/b/util.kite", "pub fn who() -> str {\n  return \"the app's b/util\"\n}\n");
    let main = p.file(
        "nested/main.kite",
        "use a\nuse b/util\n\nfn main() {\n  io.print(a.hello())\n  io.print(util.who())\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "a+from b\nthe app's b/util\n");
}

/// And a package the program *does* declare is still the one name: the
/// program and its dependency naming it from two places is refused, as
/// `kitec pkg` refuses it.
#[test]
fn a_package_the_program_declares_is_one_package_everywhere() {
    let p = Project::new("declared-package-name");
    for (dir, who) in [("b1", "first"), ("b2", "second")] {
        p.file(&format!("{}/kite.toml", dir), "[package]\nname = \"b\"\nversion = \"1.0.0\"\n");
        p.file(
            &format!("{}/b.kite", dir),
            &format!("pub fn who() -> str {{\n  return \"{}\"\n}}\n", who),
        );
    }
    p.file(
        "a/kite.toml",
        "[package]\nname = \"a\"\nversion = \"1.0.0\"\n\n[dependencies]\nb = { path = \"../b2\" }\n",
    );
    p.file("a/a.kite", "use b\n\npub fn go() -> str {\n  return b.who()\n}\n");
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\na = { path = \"../a\" }\nb = { path = \"../b1\" }\n",
    );
    let main = p.file(
        "app/main.kite",
        "use b\nuse a\n\nfn main() {\n  io.print(b.who())\n  io.print(a.go())\n}\n",
    );
    let said = p.run(&main).expect_err("`b` is two packages");
    assert!(said.contains("`b` is already the name of another module"), "{}", said);
}

// ---- cycles through the entry file -------------------------------------------

/// A cycle back to the entry file is reported in the entry file.
///
/// The entry is not loaded as a module, so it was not on the stack a cycle is
/// found against: it was read a second time, as module `a`, and the cycle
/// reported inside that copy — under another file's identity, so an editor
/// checking `a.kite`, which shows only `a.kite`'s own diagnostics, showed the
/// program as clean.
#[test]
fn a_cycle_back_to_the_entry_is_reported_in_the_entry() {
    let p = Project::new("entry-cycle");
    p.file("b.kite", "use a\n\npub fn fb() -> int {\n  return 1\n}\n");
    let src = "use b\n\npub fn fa() -> int {\n  return b.fb()\n}\n\nfn main() {\n  io.print(fa())\n}\n";
    let main = p.file("a.kite", src);
    let c = compile(&main, src, Emit::Check);
    let cycle = c
        .diags
        .iter()
        .find(|d| d.code.map(|c| c.0) == Some("E0402"))
        .expect("the cycle is reported");
    let entry = c.sources.iter().find(|(_, name)| Path::new(name) == main.as_path()).map(|(id, _)| id);
    assert_eq!(cycle.primary_span().map(|s| s.file), entry, "{}", c.render_diagnostics());
    // And the entry is not compiled twice to report it.
    let copies = c.sources.iter().filter(|(_, name)| name.ends_with("a.kite")).count();
    assert_eq!(copies, 1, "{}", c.render_diagnostics());
}

/// A manifest that is there and does not read — here one Latin-1 byte in a
/// comment — is reported where it stops reading, rather than taken for no
/// manifest and every dependency reported missing.
#[test]
fn a_manifest_that_is_not_utf8_is_reported_rather_than_ignored() {
    let p = Project::new("latin1-manifest");
    p.file("a/a.kite", "pub fn hello() -> str {\n  return \"a\"\n}\n");
    std::fs::create_dir_all(p.dir.join("app")).expect("create");
    std::fs::write(
        p.dir.join("app/kite.toml"),
        b"[package]\nname = \"app\"\nversion = \"0.1.0\"\n# caf\xe9\n\n\
          [dependencies]\na = { path = \"../a\" }\n"
            .as_slice(),
    )
    .expect("write");
    let main = p.file("app/main.kite", "use a\n\nfn main() {\n  io.print(a.hello())\n}\n");
    let said = p.run(&main).expect_err("the manifest does not read");
    assert!(said.contains("E0405"), "{}", said);
    assert!(said.contains("kite.toml:4"), "points at the line: {}", said);
    assert!(said.contains("not UTF-8"), "{}", said);
}

/// A syntax error in an imported module is one diagnostic. The loader parsed
/// the file to find its imports and the driver parsed it again to merge it,
/// and both reported.
#[test]
fn a_syntax_error_in_an_imported_module_is_reported_once() {
    let p = Project::new("syntax-once");
    p.file("bad.kite", "pub fn f() -> int {\n  return 1 +\n}\n\nconst x = $\n");
    let main = p.file("main.kite", "use bad\n\nfn main() {\n  io.print(bad.f())\n}\n");
    let said = p.run(&main).expect_err("does not parse");
    assert_eq!(said.matches("invalid character `$`").count(), 1, "{}", said);
    assert_eq!(said.matches("expected an expression").count(), 1, "{}", said);
}
