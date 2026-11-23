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

/// A bare unit pattern is read against the scrutinee's enum (§9.3), and a
/// private enum's variants are as private as the enum, bare or qualified. The
/// bare spelling matched `Draft` from outside while `state.Phase.Draft` was
/// `E0401`; a public enum's bare unit variant still matches.
#[test]
fn a_private_enums_bare_unit_variant_cannot_be_matched_from_outside() {
    let p = Project::new("bare-private-variant");
    p.file(
        "state.kite",
        "enum Phase {\n  Draft\n  Live\n}\n\npub enum Open {\n  Yes\n  No\n}\n\n\
         pub struct Doc {\n  phase: Phase\n}\n\n\
         pub fn draft() -> Doc {\n  return Doc{ phase: Draft }\n}\n\n\
         pub fn phase_of(d: Doc) -> Phase {\n  return d.phase\n}\n\n\
         pub fn open() -> Open {\n  return Yes\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use state\n\nfn main() {\n  match state.phase_of(state.draft()) {\n\
         \x20   Draft => io.print(\"draft\"),\n    _ => io.print(\"other\"),\n  }\n}\n",
    );
    let said = p.run(&main).expect_err("`Phase` is private").replace('\\', "/");
    assert!(said.contains("E0401"), "{}", said);
    assert!(said.contains("`state.Phase` is private to module `state`"), "{}", said);
    assert!(said.contains("main.kite:5:5"), "at the pattern: {}", said);
    assert_eq!(said.matches("error[").count(), 1, "one cause, one error: {}", said);

    let main = p.file(
        "main.kite",
        "use state\n\nfn main() {\n  match state.open() {\n\
         \x20   Yes => io.print(\"yes\"),\n    No => io.print(\"no\"),\n  }\n}\n",
    );
    assert_eq!(p.run(&main).expect("a public enum's variant"), "yes\n");
}

/// An `impl` header names its type and its trait, and a private one cannot be
/// named outside its module (§4.3). Both headers were accepted, while every
/// other position naming `m.Secret` was `E0401`.
#[test]
fn an_impl_header_cannot_name_a_private_type_or_trait() {
    let p = Project::new("private-in-header");
    p.file(
        "m.kite",
        "struct Secret {\n  n: int\n}\n\ntrait Hidden {\n  fn reveal(self) -> int\n}\n\n\
         pub fn make() -> Secret {\n  return Secret{ n: 1 }\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use m\n\ntrait Probe {\n  fn tag(self) -> str\n}\n\n\
         impl Probe for m.Secret {\n  fn tag(self) -> str {\n    return \"x\"\n  }\n}\n\n\
         struct Mine {\n  n: int\n}\n\n\
         impl m.Hidden for Mine {\n  fn reveal(self) -> int {\n    return 1\n  }\n}\n\n\
         fn main() {\n  io.print(m.make().tag())\n  io.print(Mine{ n: 1 }.reveal())\n}\n",
    );
    let said = p.run(&main).expect_err("both are private").replace('\\', "/");
    assert!(said.contains("`m.Secret` is private to module `m`"), "{}", said);
    assert!(said.contains("main.kite:7:16"), "at the type in the header: {}", said);
    assert!(said.contains("`m.Hidden` is private to module `m`"), "{}", said);
    assert!(said.contains("main.kite:17:6"), "at the trait in the header: {}", said);
    assert_eq!(said.matches("error[").count(), 2, "one cause, one error each: {}", said);
}

/// A `pub` alias does not export the private type it names: it is refused
/// where it is declared. It was usable in a signature, and a literal through
/// it said ``m.Door` is private`` — of a `pub` alias — with a second `E0401`.
#[test]
fn a_pub_alias_cannot_name_a_private_type() {
    let p = Project::new("pub-alias-private");
    p.file(
        "m.kite",
        "struct Also {\n  pub n: int\n}\n\npub type Door = Also\npub type Doors = [Also]\n\n\
         pub struct Open {\n  pub n: int\n}\n\npub type Gate = Open\n",
    );
    let main = p.file(
        "main.kite",
        "use m\n\nfn take(a: m.Door) -> int {\n  return a.n\n}\n\n\
         fn main() {\n  io.print(take(m.Door{ n: 3 }))\n  io.print(m.Gate{ n: 4 }.n)\n}\n",
    );
    let said = p.run(&main).expect_err("`Also` is private").replace('\\', "/");
    assert!(said.contains("a `pub` alias cannot name `Also`"), "{}", said);
    assert!(said.contains("m.kite:5:17"), "at the alias: {}", said);
    assert!(said.contains("m.kite:6:19"), "inside a slice too: {}", said);
    assert!(!said.contains("main.kite"), "nothing at the uses: {}", said);
    assert_eq!(said.matches("error[").count(), 2, "{}", said);

    // A `pub` alias of a `pub` type is that type, by either name.
    let main = p.file("main.kite", "use m\n\nfn main() {\n  io.print(m.Gate{ n: 4 }.n)\n}\n");
    p.file("m.kite", "pub struct Open {\n  pub n: int\n}\n\npub type Gate = Open\n");
    assert_eq!(p.run(&main).expect("compiles"), "4\n");
}

/// A misspelled member of an imported module is reported at the member, with
/// the name it probably meant. It was ``cannot find `m` `` with the caret on
/// `m` — a module the first line imports — and no suggestion.
#[test]
fn a_misspelled_member_is_not_reported_as_a_missing_module() {
    let p = Project::new("misspelled-member");
    p.file("m.kite", "pub fn one() -> int {\n  return 1\n}\n\nfn onf() -> int {\n  return 2\n}\n");
    let main = p.file("main.kite", "use m as mm\n\nfn main() {\n  io.print(mm.on())\n}\n");
    let said = p.run(&main).expect_err("no `on` in `m`").replace('\\', "/");
    assert!(said.contains("cannot find `on` in module `mm`"), "{}", said);
    assert!(said.contains("main.kite:4:15"), "at the member: {}", said);
    // The private `onf` is as near, and not a name that would compile.
    assert!(said.contains("a similar name is in `mm`: `one`"), "{}", said);
    assert_eq!(said.matches("error[").count(), 1, "{}", said);
}

/// The same for a module in scope without a `use`: `io.prnt` said
/// ``cannot find `io` ``. One whose member is not near a builtin — `text.trim`
/// with no `use std/text` — is a missing `use`, and is not told it is a typo.
/// The near name offered for a misspelt member fits where it is written: a
/// call is offered a function, never a trait or a type it cannot call.
#[test]
fn a_misspelled_call_is_offered_only_a_function() {
    let p = Project::new("misspelled-call");
    p.file(
        "m.kite",
        "pub trait Outer {\n  fn out(self) -> int\n}\n\n\
         pub fn other() -> int {\n  return 1\n}\n\n\
         pub fn inner() -> int {\n  return 2\n}\n",
    );
    let main = p.file("main.kite", "use m\n\nfn main() {\n  io.print(m.outer())\n}\n");
    let said = p.run(&main).expect_err("misspelt");
    assert!(said.contains("cannot find `outer` in module `m`"), "{}", said);
    assert!(!said.contains("`Outer`"), "a trait cannot be called: {}", said);
    let main = p.file("near.kite", "use m\n\nfn main() {\n  io.print(m.innr())\n}\n");
    let said = p.run(&main).expect_err("misspelt");
    assert!(said.contains("a similar name is in `m`: `inner`"), "{}", said);
}

/// An unknown bare type is told about the types this module can name, as it
/// would write them — not other modules' private ones, nor modules it never
/// imported — and about `m.Name` when an imported module has that type.
#[test]
fn an_unknown_type_lists_only_what_this_module_can_name() {
    let p = Project::new("known-types");
    p.file("m.kite", "pub struct Shown {\n  pub n: int\n}\n\nstruct Hidden {\n  n: int\n}\n");
    p.file("other.kite", "pub struct Elsewhere {\n  pub n: int\n}\n");
    p.file("b.kite", "use other\n\npub fn f() -> int {\n  return other.Elsewhere{ n: 1 }.n\n}\n");
    let main = p.file(
        "main.kite",
        "use m as mm\nuse b\n\nfn main() {\n  let x: Zzzzzz = 1\n  let y: Shown = mm.Shown{ n: b.f() }\n}\n",
    );
    let said = p.run(&main).expect_err("unknown types");
    assert!(said.contains("known types: bool, int, float, str, mm.Shown"), "{}", said);
    assert!(!said.contains("Hidden"), "private: {}", said);
    assert!(!said.contains("Elsewhere"), "not imported here: {}", said);
    assert!(said.contains("write `mm.Shown`"), "{}", said);
}

#[test]
fn a_misspelled_type_is_not_reported_as_a_missing_module() {
    let p = Project::new("misspelled-type");
    p.file("m.kite", "pub struct Pt {\n  pub x: int\n}\n");
    for (name, body) in [
        ("s1.kite", "  let p = m.Pnt{ x: 1 }\n  io.print(p.x)\n"),
        ("s2.kite", "  let p: m.Pnt = m.Pt{ x: 1 }\n  io.print(p.x)\n"),
        ("s3.kite", "  let p = m.Pt{ x: 1 }\n  match p {\n    m.Ptt{ x: x } => io.print(x),\n  }\n"),
    ] {
        let main = p.file(name, &format!("use m\n\nfn main() {{\n{}}}\n", body));
        let said = p.run(&main).expect_err("misspelt");
        assert!(said.contains("cannot find `P"), "{}", said);
        assert!(said.contains("in module `m`"), "{}", said);
        assert!(said.contains("a similar type is in `m`: `Pt`"), "{}", said);
        assert!(!said.contains("is that module imported"), "{}", said);
        assert!(said.contains("1 previous error"), "{}", said);
    }
}

#[test]
fn a_misspelled_builtin_is_not_reported_as_a_missing_module() {
    let p = Project::new("misspelled-builtin");
    let main = p.file("main.kite", "fn main() {\n  io.prnt(\"x\")\n}\n");
    let said = p.run(&main).expect_err("no `io.prnt`").replace('\\', "/");
    assert!(said.contains("cannot find `prnt` in module `io`"), "{}", said);
    assert!(said.contains("main.kite:2:6"), "at the member: {}", said);
    assert!(said.contains("a similar name is in `io`: `print`"), "{}", said);
    assert_eq!(said.matches("error[").count(), 1, "{}", said);

    let main = p.file("main.kite", "fn main() {\n  io.print(text.trim(\" x \"))\n}\n");
    let said = p.run(&main).expect_err("`std/text` is not imported");
    assert!(!said.contains("in module `text`"), "{}", said);
}

/// Another module's constant in a pattern is its value, exactly as one of the
/// program's own is, and is held to the same visibility as anywhere else.
#[test]
fn an_imported_constant_in_a_pattern_is_its_value() {
    let p = Project::new("const-pattern");
    p.file("limits.kite", "pub let MAX = 99\nlet SECRET = 7\n");
    let main = p.file(
        "main.kite",
        "use limits\n\nfn main() {\n  io.print(match 99 { limits.MAX => \"max\", _ => \"no\" })\n\
         \x20 io.print(match 98 { limits.MAX => \"max\", _ => \"no\" })\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "max\nno\n");
    let main = p.file(
        "secret.kite",
        "use limits\n\nfn main() {\n  io.print(match 7 { limits.SECRET => \"s\", _ => \"no\" })\n}\n",
    );
    let err = p.run(&main).expect_err("must be private");
    assert!(err.contains("E0401"), "{}", err);
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

/// An alias of a standard-library module is the whole module, its builtins
/// included: after `use std/errors as errs`, `errs.wrap` resolved and
/// `errs.new` was `E0111`. And a note offering `errors.wrap` names it the way
/// the `use` spells it, rather than asking for a second `use std/errors`.
#[test]
fn an_alias_of_a_standard_module_reaches_its_builtins() {
    let p = Project::new("std-alias");
    let main = p.file(
        "main.kite",
        "use std/errors as errs\n\nfn main() {\n  let e = errs.wrap(errs.new(\"boom\"), \"loading\")\n  \
         if e != nil {\n    io.print(e.message())\n  }\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "loading: boom\n");

    let main = p.file(
        "dropped.kite",
        "use std/errors as errs\n\nfn main() {\n  var e = errs.new(\"x\")\n  e = errs.new(\"y\")\n  \
         if e != nil {\n    io.print(e.message())\n  }\n}\n",
    );
    let err = p.run(&main).expect_err("the first failure is dropped");
    assert!(err.contains("E0302") && !err.contains("E0111"), "{}", err);
    assert!(err.contains("`e = errs.wrap(e, \"…\")`"), "{}", err);
    assert!(!err.contains("after `use std/errors`"), "{}", err);
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

/// A `use` below the declarations is refused for where it is, and a module
/// it names that is not there is still reported: two mistakes. Only a path
/// written from the file (`use ../nowhere`), whose one error says how paths
/// are looked for, is not reported missing again.
#[test]
fn a_late_use_of_a_missing_module_is_reported_missing_too() {
    let p = Project::new("late-missing");
    let main = p.file("main.kite", "fn main() {\n}\n\nuse nowhere\nuse ../near\n");
    let src = std::fs::read_to_string(&main).expect("read main");
    let c = compile(&main, &src, Emit::Check);
    let codes: Vec<&str> = c.diags.iter().filter_map(|d| d.code.map(|c| c.0)).collect();
    assert_eq!(codes.iter().filter(|c| **c == "E0400").count(), 1, "{}", c.render_diagnostics());
    assert_eq!(codes.iter().filter(|c| **c == "E0100").count(), 2, "{}", c.render_diagnostics());
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
    // In a directory of its own: `use md` reads every `.kite` file directly
    // in `md/`, and a file of a module cannot also be a module by itself.
    p.file("md/util/util.kite", "pub fn me() -> str {\n  return \"md-util\"\n}\n");
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

// ---- a `use` is looked for outward, as far as the package's root -------------

/// §13.1's own tree: `ui/` imports `config/` beside it. A module resolved
/// `use` only from its own directory, so the example the section is built
/// around was `cannot find module config` at `ui/app.kite`.
#[test]
fn the_specifications_tree_compiles() {
    let p = Project::new("spec-tree");
    p.file("kite.toml", "[package]\nname = \"myapp\"\nversion = \"0.1.0\"\n");
    p.file("src/money.kite", "pub fn cents(n: int) -> int {\n  return n * 100\n}\n");
    p.file(
        "src/config/load.kite",
        "pub fn load(path: str) -> (Config, error) {\n  return Config{ name: path }, nil\n}\n",
    );
    p.file("src/config/schema.kite", "pub struct Config {\n  pub name: str\n}\n");
    p.file(
        "src/ui/app.kite",
        "use config\nuse money\n\npub struct App {\n  pub config: config.Config\n}\n\n\
         pub fn run(a: App) {\n  io.print(\"\\(a.config.name) \\(money.cents(2))\")\n}\n",
    );
    p.file("src/ui/theme.kite", "pub fn dark() -> bool {\n  return true\n}\n");
    let main = p.file(
        "src/main.kite",
        "use config\nuse money\nuse ui\n\n\
         fn main() {\n  let err = start()\n  if err != nil {\n\
         \x20   io.print(\"cannot start: \\(err.message())\")\n  }\n}\n\n\
         fn start() -> error {\n  let (cfg, err) = config.load(\"app.toml\")\n  check err\n\
         \x20 ui.run(ui.App{ config: cfg })\n  return nil\n}\n",
    );
    // One `config`, reached from two places: `main`'s `cfg` is the type
    // `ui.App` holds.
    assert_eq!(p.run(&main).expect("compiles"), "app.toml 200\n");
}

/// The nearest wins: a directory module's own `util/` over the `util.kite`
/// beside it, and a one-file module's own neighbour over one further out.
#[test]
fn the_nearest_module_of_a_spelling_wins() {
    let p = Project::new("nearest");
    p.file("util.kite", "pub fn name() -> str {\n  return \"outer\"\n}\n");
    p.file("a/a.kite", "use util\n\npub fn go() -> str {\n  return util.name()\n}\n");
    p.file("a/util/util.kite", "pub fn name() -> str {\n  return \"a's own\"\n}\n");
    p.file("b/b.kite", "use util\n\npub fn go() -> str {\n  return util.name()\n}\n");
    let main = p.file(
        "main.kite",
        "use a\nuse b\nuse util\n\n\
         fn main() {\n  io.print(a.go())\n  io.print(b.go())\n  io.print(util.name())\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "a's own\nouter\nouter\n");
}

/// A directory is a module when it holds a `.kite` file. A `config/` holding
/// only data beside `config.kite` was taken for an empty module, and the
/// first sign was `cannot find` at a name `config.kite` declares.
#[test]
fn a_directory_without_kite_files_does_not_hide_the_file_beside_it() {
    let p = Project::new("data-dir");
    p.file("config/app.toml", "name = \"demo\"\n");
    p.file("config.kite", "pub fn name() -> str {\n  return \"demo\"\n}\n");
    let main = p.file("main.kite", "use config\n\nfn main() {\n  io.print(config.name())\n}\n");
    assert_eq!(p.run(&main).expect("compiles"), "demo\n");

    // With no `config.kite`, nothing answers, and the note says why the
    // directory did not.
    std::fs::remove_file(p.dir.join("config.kite")).expect("remove");
    let said = p.run(&main).expect_err("no module `config`").replace('\\', "/");
    assert!(said.contains("cannot find module `config`"), "{}", said);
    assert!(said.contains("is a directory with no `.kite` file directly in it"), "{}", said);
}

/// A `use` that finds nothing says every place it looked, nearest first.
#[test]
fn a_missing_module_names_every_place_it_was_looked_for() {
    let p = Project::new("looked-everywhere");
    p.file("a/b/b.kite", "use gone\n\npub fn go() -> int {\n  return 1\n}\n");
    let main = p.file("main.kite", "use a/b\n\nfn main() {\n  io.print(b.go())\n}\n");
    let said = p.run(&main).expect_err("no `gone` anywhere").replace('\\', "/");
    assert!(said.contains("cannot find module `gone`"), "{}", said);
    for place in ["a/b/gone`", "a/b/gone.kite`", "/a/gone`", "/a/gone.kite`"] {
        assert!(said.contains(place), "{} is not among the places: {}", place, said);
    }
    let root = format!("{}/gone.kite`", p.dir.display()).replace('\\', "/");
    assert!(said.contains(&root), "the entry's directory is the last place: {}", said);
}

/// The walk stops at a package's root. A dependency kept inside the program's
/// own tree is still a package of its own, and its `use helper` does not reach
/// the program's `helper.kite` a directory further out.
#[test]
fn a_dependency_inside_the_program_does_not_walk_out_into_it() {
    let p = Project::new("dep-inside");
    p.file(
        "kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nkx = { path = \"src/vendored/kx\" }\n",
    );
    p.file("src/vendored/kx/kite.toml", "[package]\nname = \"kx\"\nversion = \"1.0.0\"\n");
    p.file(
        "src/vendored/kx/greet.kite",
        "use helper\n\npub fn hello() -> str {\n  return helper.who()\n}\n",
    );
    p.file("src/helper.kite", "pub fn who() -> str {\n  return \"the application\"\n}\n");
    let main = p.file(
        "src/main.kite",
        "use helper\nuse kx/greet\n\nfn main() {\n  io.print(greet.hello())\n}\n",
    );
    let said = p.run(&main).expect_err("the package has no helper");
    assert!(said.contains("cannot find module `helper`"), "{}", said);
    assert!(said.contains("greet.kite"), "the error is in the package: {}", said);
}

/// Inside a dependency, its directory modules reach each other as the
/// program's do.
#[test]
fn a_dependencys_directory_modules_reach_each_other() {
    let p = Project::new("dep-directory-siblings");
    p.file("md/kite.toml", "[package]\nname = \"md\"\nversion = \"1.0.0\"\n");
    p.file("md/parse/parse.kite", "pub fn word() -> str {\n  return \"parsed\"\n}\n");
    p.file(
        "md/render/render.kite",
        "use parse\n\npub fn html() -> str {\n  return \"<p>\\(parse.word())</p>\"\n}\n",
    );
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nmd = { path = \"../md\" }\n",
    );
    let main = p.file(
        "app/main.kite",
        "use md/render\n\nfn main() {\n  io.print(render.html())\n}\n",
    );
    assert_eq!(p.run(&main).expect("compiles"), "<p>parsed</p>\n");
}

/// Two directory modules that reach each other by the walk are a cycle, as
/// any two modules that import each other are.
#[test]
fn sibling_directory_modules_importing_each_other_are_a_cycle() {
    let p = Project::new("sibling-cycle");
    p.file("one/a.kite", "use two\n\npub fn a() -> int {\n  return two.b()\n}\n");
    p.file("two/b.kite", "use one\n\npub fn b() -> int {\n  return 2\n}\n");
    let main = p.file("main.kite", "use one\n\nfn main() {\n  io.print(one.a())\n}\n");
    let said = p.run(&main).expect_err("a cycle");
    assert!(said.contains("E0402"), "{}", said);
}

/// Handed over by name, a module inside a package walks outward to the
/// package's own modules and stops there.
#[test]
fn a_provided_module_walks_out_as_far_as_its_package() {
    let mut provided = std::collections::HashMap::new();
    provided.insert(
        "kitex/sub/doc".to_string(),
        "use helper\n\npub fn who() -> str {\n  return helper.who()\n}\n".to_string(),
    );
    provided.insert(
        "kitex/helper".to_string(),
        "pub fn who() -> str {\n  return \"the package\"\n}\n".to_string(),
    );
    let src = "use kitex/sub/doc\n\nfn main() {\n  io.print(doc.who())\n}\n";
    let c = kite_driver::compile_provided("main.kite", src, Emit::Check, false, provided.clone());
    assert!(!c.failed(), "{}", c.render_diagnostics());
    let mut out = Vec::new();
    c.run(&mut out).expect("the program runs");
    assert_eq!(String::from_utf8(out).expect("utf-8"), "the package\n");

    // Without the package's own, the application's is not the answer.
    provided.remove("kitex/helper");
    provided.insert(
        "helper".to_string(),
        "pub fn who() -> str {\n  return \"the application\"\n}\n".to_string(),
    );
    let c = kite_driver::compile_provided("main.kite", src, Emit::Check, false, provided);
    assert!(c.failed(), "a package reached the application's `helper`");
    assert!(c.render_diagnostics().contains("cannot find module `helper`"));
}

/// One file is one module. A file of a directory module importing its own
/// sibling loaded that sibling a second time as a module of its own, and one
/// declaration became two types: ``expected `lib/shape.Pt`, found `lib.Pt` ``.
#[test]
fn a_file_of_a_directory_module_cannot_import_its_sibling() {
    let p = Project::new("own-sibling");
    p.file("lib/shape.kite", "pub struct Pt {\n  pub x: int\n}\n");
    p.file(
        "lib/use_it.kite",
        "use shape\n\npub fn origin() -> Pt {\n  return Pt{ x: 0 }\n}\n",
    );
    let main = p.file("main.kite", "use lib\n\nfn main() {\n  io.print(lib.origin().x)\n}\n");
    let said = p.run(&main).expect_err("the sibling is part of `lib`").replace('\\', "/");
    assert!(said.contains("E0404"), "{}", said);
    assert!(said.contains("lib/shape.kite` is already part of the module `lib`"), "{}", said);
    assert!(said.contains("lib/use_it.kite:1:1"), "at the `use`: {}", said);
    assert!(said.contains("remove this `use`"), "{}", said);
}

/// A module inside a directory module, reaching one of that module's files
/// by walking out, is not told to `use` the module that imports it — taking
/// that advice was a cycle.
#[test]
fn a_nested_module_is_not_told_to_import_the_module_around_it() {
    let p = Project::new("nested-member");
    p.file(
        "ui/view.kite",
        "use widgets\n\npub struct View {\n  pub n: int\n}\n\n\
         pub fn top() -> int {\n  return widgets.w().n\n}\n",
    );
    p.file("ui/widgets/w.kite", "use view\n\npub fn w() -> view.View {\n  return view.View{ n: 3 }\n}\n");
    let main = p.file("main.kite", "use ui\n\nfn main() {\n  io.print(ui.top())\n}\n");
    let said = p.run(&main).expect_err("one file, two modules");
    assert!(said.contains("E0404"), "{}", said);
    assert!(!said.contains("`use ui`"), "that would be a cycle: {}", said);
    assert!(said.contains("cannot import `ui` back"), "{}", said);
    assert!(said.contains("`ui` was loaded here"), "{}", said);
}

/// The same from outside, in either order: `use lib/shape` beside `use lib`.
#[test]
fn a_file_of_a_directory_module_cannot_be_imported_on_its_own() {
    let p = Project::new("member-alone");
    p.file("lib/shape.kite", "pub struct Pt {\n  pub x: int\n}\n");
    p.file("lib/make.kite", "pub fn origin() -> Pt {\n  return Pt{ x: 0 }\n}\n");
    for uses in ["use lib\nuse lib/shape\n", "use lib/shape\nuse lib\n"] {
        let main = p.file(
            "main.kite",
            &format!("{}\nfn main() {{\n  io.print(lib.origin().x)\n}}\n", uses),
        );
        let said = p.run(&main).expect_err("one file, two modules").replace('\\', "/");
        assert!(said.contains("E0404"), "{}", said);
        assert!(said.contains("is already"), "{}", said);
        assert!(said.contains("main.kite:2:1"), "at the second `use`: {}", said);
        // And it points at the first one, and says which to change.
        if uses.starts_with("use lib/shape") {
            assert!(said.contains("`lib/shape` was imported on its own here"), "{}", said);
            assert!(said.contains("remove the `use lib/shape`"), "{}", said);
        } else {
            assert!(said.contains("`lib` was loaded here"), "{}", said);
            assert!(said.contains("remove this `use`: the `use lib` above reaches"), "{}", said);
        }
    }
}

/// The notes are in the words the `use`s wrote, not the modules' identities:
/// below a `kite.toml`, `lib` beside `src/main.kite` is the module `src/lib`,
/// and "remove the `use src/lib/shape`" and "reach … as `src/lib.…`" named
/// lines and code that are not there. The two `use`s in two files are told
/// how to meet: the earlier `use lib/shape` becomes a `use lib`.
#[test]
fn a_member_imported_on_its_own_is_told_the_fix_in_the_words_written() {
    let p = Project::new("member-alone-words");
    p.file("kite.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n");
    p.file("src/lib/shape.kite", "pub fn area() -> int {\n  return 4\n}\n");
    p.file("src/lib/core.kite", "pub fn top() -> int {\n  return 1\n}\n");
    p.file("src/other/o.kite", "use lib\npub fn o() -> int {\n  return lib.top()\n}\n");
    let main = p.file(
        "src/main.kite",
        "use lib/shape\nuse other\nfn main() {\n  io.print(shape.area() + other.o())\n}\n",
    );
    let said = p.run(&main).expect_err("one file, two modules").replace('\\', "/");
    assert!(said.contains("make the `use lib/shape` a `use lib`"), "{}", said);
    assert!(said.contains("declares as `lib.…`"), "{}", said);
    assert!(!said.contains("use src/"), "{}", said);
    // And the other way round, from a module that is not the entry.
    p.file("src/x/y.kite", "use lib/shape\npub fn y() -> int {\n  return shape.area()\n}\n");
    let main = p.file("src/main.kite", "use lib\nuse x\nfn main() {\n  io.print(lib.top() + x.y())\n}\n");
    let said = p.run(&main).expect_err("one file, two modules").replace('\\', "/");
    assert!(said.contains("make this `use` a `use lib`: it reaches"), "{}", said);
    assert!(said.contains("as `lib.…`"), "{}", said);
}

/// A privacy error names the module as the reader's `use` writes it, and the
/// declaration it points at is in the module's own file, which is what tells
/// two `util`s apart. It said ``private to module `a/util` `` beside the
/// `util.hidden` that was written: one module named two ways.
#[test]
fn a_privacy_error_names_the_module_as_the_use_writes_it() {
    let p = Project::new("identity-in-diagnostic");
    p.file("a/a.kite", "use util\n\npub fn go() -> str {\n  return util.hidden()\n}\n");
    p.file("a/util/util.kite", "fn hidden() -> str {\n  return \"x\"\n}\n");
    let main = p.file("main.kite", "use a\n\nfn main() {\n  io.print(a.go())\n}\n");
    let said = p.run(&main).expect_err("private").replace('\\', "/");
    assert!(said.contains("`util.hidden` is private to module `util`"), "{}", said);
    assert!(said.contains("a/util/util.kite:1:4"), "{}", said);
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
    // The one error: not a `cannot find module` for the `use` it broke, nor
    // a `cannot find` at `a.hello` under that.
    assert!(!said.contains("E0400"), "the `use` is not a second error: {}", said);
    assert!(!said.contains("E0111"), "`a.hello` is not a third: {}", said);
    assert!(said.contains("1 previous error"), "{}", said);
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
    // Beside the module rather than inside it: a file inside `config/` is
    // part of `config`, and cannot be imported as a module of its own.
    p.file("util.kite", "pub fn base() -> int {\n  return 40\n}\n");
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

/// An `impl` block's parameter given a type that cannot be compared is one
/// report per file, at the first call there: a call in another file is not
/// labelled on a report elsewhere, so folding it in would say nothing of that
/// file's mistake. Each of the two files that gives `W` a function key is
/// named.
#[test]
fn a_type_that_cannot_be_compared_is_reported_in_each_file_that_gives_it() {
    let p = Project::new("compared-in-each-file");
    p.file(
        "w.kite",
        "pub struct W<K> {\n  pub var m: { K: int }\n}\n\nimpl<K> W<K> {\n  \
         pub fn new() -> W<K> {\n    return W{ m: { } }\n  }\n\n  \
         pub fn put(var self, k: K) {\n    self.m[k] = 1\n  }\n}\n",
    );
    p.file(
        "a.kite",
        "use w\n\npub fn fa() -> int {\n  var x: w.W<fn() -> int> = w.W.new()\n  \
         x.put(|| 2)\n  return x.m.len()\n}\n",
    );
    let main = p.file(
        "main.kite",
        "use w\nuse a\n\nfn main() {\n  var y: w.W<fn() -> int> = w.W.new()\n  \
         y.put(|| 3)\n  y.put(|| 4)\n  io.print(a.fa() + y.m.len())\n}\n",
    );
    let said = p.run(&main).expect_err("E0201");
    assert_eq!(said.matches("error[E0201]").count(), 2, "{}", said);
    assert!(said.contains("a.kite:4:"), "{}", said);
    assert!(said.contains("main.kite:5:"), "{}", said);
    assert_eq!(said.matches("and given to `put` here").count(), 3, "{}", said);
}

/// A module's own constant hides the prelude's function of its name, so
/// `max(1, 2)` beside `let max = 3` is E0205 — and its note says the
/// constant hides the prelude's `max` and is what to rename, in the entry
/// file and in a module alike. It said "drop the `()`", which is no help to
/// a call that wanted the prelude's function. A constant that hides nothing
/// keeps that note.
#[test]
fn calling_a_constant_that_hides_a_prelude_function_says_so() {
    let p = Project::new("const-hides-prelude");
    p.file("util.kite", "let max = 3\n\npub fn top() -> int {\n  return max(1, 2)\n}\n");
    let main = p.file(
        "main.kite",
        "use util\n\nlet min = 1\nlet LIMIT = 4\n\n\
         fn main() {\n  io.print(util.top() + min(1, 2) + LIMIT())\n}\n",
    );
    let said = p.run(&main).expect_err("E0205");
    assert_eq!(said.matches("E0205").count(), 3, "{}", said);
    for name in ["max", "min"] {
        let note = format!("this module's constant `{0}` hides the prelude's function `{0}`", name);
        assert!(said.contains(&note), "{}", said);
    }
    assert_eq!(said.matches("rename the constant").count(), 2, "{}", said);
    assert_eq!(said.matches("drop the `()`").count(), 1, "{}", said);
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
    p.file("b/util/util.kite", "pub fn who() -> str {\n  return \"from b\"\n}\n");
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

/// `use dep` for a dependency whose directory holds no `.kite` file directly
/// is `E0400`. It fell back to `dep.kite` beside the dependency's directory —
/// a file outside the package, which the lockfile does not hash.
#[test]
fn a_dependency_root_never_falls_back_to_a_file_beside_it() {
    let p = Project::new("dep-root-file");
    p.file("dep/kite.toml", "[package]\nname = \"dep\"\nversion = \"0.1.0\"\n");
    p.file("dep/sub/s.kite", "pub fn inside() -> str {\n  return \"inside\"\n}\n");
    p.file("dep.kite", "pub fn outside() -> str {\n  return \"outside\"\n}\n");
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\ndep = { path = \"../dep\" }\n",
    );
    let main = p.file("app/main.kite", "use dep\n\nfn main() {\n  io.print(dep.outside())\n}\n");
    let said = p.run(&main).expect_err("the package has no root module");
    assert!(said.contains("E0400"), "{}", said);
    assert!(said.contains("no `.kite` file directly in it"), "{}", said);
    assert!(!said.contains("dep.kite`"), "the file beside it is not looked for: {}", said);
    // A module inside the package is still reached.
    let inner = p.file("app/inner.kite", "use dep/sub\n\nfn main() {\n  io.print(sub.inside())\n}\n");
    assert_eq!(p.run(&inner).expect("compiles"), "inside\n");
    // And with the package's directory missing altogether, still not the file.
    std::fs::remove_dir_all(p.dir.join("dep")).expect("remove");
    let said = p.run(&main).expect_err("there is no package");
    assert!(said.contains("E0400"), "{}", said);
}

/// A module reached through a symbolic link and directly is one module, and
/// its own `use config` walks out from where it is, whichever `use` came
/// first. It walked out along the route that loaded it, so swapping the two
/// `use` lines changed which `config` it reached.
#[cfg(unix)]
#[test]
fn a_module_reached_through_a_link_resolves_from_where_it_is() {
    let p = Project::new("linked-route");
    p.file("real/r.kite", "use config\n\npub fn f() -> str {\n  return config.c()\n}\n");
    p.file("config.kite", "pub fn c() -> str {\n  return \"top\"\n}\n");
    p.file("a/config.kite", "pub fn c() -> str {\n  return \"a\"\n}\n");
    std::os::unix::fs::symlink("../real", p.dir.join("a/link")).expect("a link");
    let body = "fn main() {\n  io.print(real.f())\n  io.print(link.f())\n}\n";
    let one = p.file("m1.kite", &format!("use real\nuse a/link\n\n{}", body));
    let two = p.file("m2.kite", &format!("use a/link\nuse real\n\n{}", body));
    assert_eq!(p.run(&one).expect("compiles"), "top\ntop\n");
    assert_eq!(p.run(&two).expect("compiles"), "top\ntop\n");
}

/// A dependency vendored inside the program is reachable by its name and by a
/// path from the program. Its own `use util` resolved within whichever package
/// the first `use` reached it through — the program's `util.kite` one way,
/// `E0400` the other. One module belongs to one package: the second route is
/// `E0404`, in either order.
#[test]
fn a_module_reached_as_part_of_two_packages_is_refused() {
    let p = Project::new("two-packages-route");
    p.file(
        "kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nd = \"src/vend/d\"\n",
    );
    p.file("src/util.kite", "pub fn secret() -> str {\n  return \"program\"\n}\n");
    p.file("src/vend/d/kite.toml", "[package]\nname = \"d\"\nversion = \"1.0.0\"\n");
    p.file("src/vend/d/inner/x.kite", "pub fn x() -> int {\n  return 1\n}\n");
    let body = "fn main() {\n  io.print(inner.x())\n}\n";
    for (name, uses) in [
        ("src/m1.kite", "use vend/d/inner as vi\nuse d/inner\n"),
        ("src/m2.kite", "use d/inner\nuse vend/d/inner as vi\n"),
    ] {
        let main = p.file(name, &format!("{}\n{}", uses, body));
        let said = p.run(&main).expect_err("one module, two packages");
        assert!(said.contains("E0404"), "{}", said);
        assert!(said.contains("it was first reached here"), "{}", said);
        // The module is named once, in the title, as the `use`s spell it: the
        // note did not say it again as the canonical absolute path.
        assert!(said.contains("inner` is already loaded as part of"), "{}", said);
        assert!(said.contains("= note: it is one module,"), "no path in the note: {}", said);
    }
    // One route alone is fine.
    let one = p.file("src/one.kite", &format!("use d/inner\n\n{}", body));
    assert_eq!(p.run(&one).expect("compiles"), "1\n");
}

/// The program's package begins at its `kite.toml`, so a file of the package
/// compiled on its own — `kitec test ui/u.kite` — reaches what it reaches when
/// `main.kite` is the entry, and names modules the same way. Its own directory
/// was the root, and `use config` beside `ui/` was `E0400`.
#[test]
fn a_file_checked_on_its_own_resolves_from_its_packages_root() {
    let p = Project::new("root-at-manifest");
    p.file("kite.toml", "[package]\nname = \"t\"\nversion = \"0.1.0\"\n");
    p.file("config/c.kite", "pub fn name() -> str {\n  return \"cfg\"\n}\nfn hidden() {}\n");
    p.file("main.kite", "use ui\n\nfn main() {\n  io.print(ui.title())\n}\n");
    let ui = p.file(
        "ui/u.kite",
        "use config\n\npub fn title() -> str {\n  return \"ui:\" + config.name()\n}\n\n\
         fn main() {\n  io.print(title())\n}\n",
    );
    assert_eq!(p.run(&ui).expect("compiles"), "ui:cfg\n");
    let tests = kite_driver::compile_tests(&ui, &std::fs::read_to_string(&ui).unwrap(), false);
    assert!(!tests.failed(), "{}", tests.render_diagnostics());
    // Named from the root, as from `main.kite`.
    let bad = p.file("ui/bad.kite", "use config\n\nfn main() {\n  config.hidden()\n}\n");
    let said = p.run(&bad).expect_err("private");
    assert!(said.contains("private to module `config`"), "{}", said);
    // Without a manifest the entry's own directory is the root, as it was.
    std::fs::remove_file(p.dir.join("kite.toml")).expect("remove");
    let said = p.run(&ui).expect_err("no package above");
    assert!(said.contains("E0400"), "{}", said);
}

/// A package only a dependency declares is named in a diagnostic as every
/// `use` spells it. Its identities carry an `@` to keep them apart from the
/// program's own modules, and the `@` was printed: `private to module @b`.
#[test]
fn a_package_only_a_dependency_declares_is_named_without_its_mark() {
    let p = Project::new("marked-package");
    p.file("b/kite.toml", "[package]\nname = \"b\"\nversion = \"1.0.0\"\n");
    p.file("b/m.kite", "fn hid() -> int {\n  return 2\n}\n\npub struct T {\n  pub n: int\n}\n");
    p.file(
        "a/kite.toml",
        "[package]\nname = \"a\"\nversion = \"1.0.0\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
    );
    p.file(
        "a/m.kite",
        "use b\n\npub fn fa() -> int {\n  let x: int = b.T{ n: 1 }\n  return b.hid() + x\n}\n",
    );
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\na = { path = \"../a\" }\n",
    );
    let main = p.file("app/main.kite", "use a\n\nfn main() {\n  io.print(a.fa())\n}\n");
    let said = p.run(&main).expect_err("private, and a mismatch");
    assert!(said.contains("private to module `b`"), "{}", said);
    assert!(said.contains("`b.T`"), "{}", said);
    assert!(!said.contains("@b"), "{}", said);
}

/// But not where the program has a module of that name too: then there are
/// two `b.T`s, and without the mark a mismatch between them read `expected
/// b.T, found b.T`.
#[test]
fn a_mark_stays_where_the_program_has_a_module_of_the_name() {
    let p = Project::new("marked-package-clash");
    p.file("b/kite.toml", "[package]\nname = \"b\"\nversion = \"1.0.0\"\n");
    p.file("b/b.kite", "pub struct T {\n  pub v: int\n}\n");
    p.file(
        "a/kite.toml",
        "[package]\nname = \"a\"\nversion = \"1.0.0\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
    );
    p.file("a/a.kite", "use b\n\npub fn get() -> b.T {\n  return b.T{ v: 1 }\n}\n");
    p.file(
        "app/kite.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\na = { path = \"../a\" }\n",
    );
    p.file("app/b.kite", "pub struct T {\n  pub v: int\n}\n");
    let main = p.file(
        "app/main.kite",
        "use a\nuse b\n\nfn main() {\n  let t: b.T = a.get()\n  io.print(t.v)\n}\n",
    );
    let said = p.run(&main).expect_err("two types called b.T");
    assert!(said.contains("expected `b.T`, found `@b.T`"), "{}", said);
}

/// A `use` matches a file's or a directory's name exactly, case included, on
/// every system. Where the file system ignores case — macOS, Windows — `use
/// Util` loaded `util.kite`, and the program was `cannot find module` on
/// Linux.
#[test]
fn a_use_matches_the_name_on_disk_exactly() {
    let p = Project::new("case-exact");
    p.file("util.kite", "pub fn one() -> int {\n  return 1\n}\n");
    p.file("lib/a.kite", "pub fn two() -> int {\n  return 2\n}\n");
    let ignores_case = p.dir.join("UTIL.kite").is_file();
    for (name, uses, call) in
        [("m1.kite", "use Util", "Util.one()"), ("m2.kite", "use Lib", "Lib.two()")]
    {
        let main = p.file(name, &format!("{}\n\nfn main() {{\n  io.print({})\n}}\n", uses, call));
        let said = p.run(&main).expect_err("not the name on disk");
        assert!(said.contains("E0400"), "{}", said);
        if ignores_case {
            assert!(said.contains("spelled differently"), "{}", said);
            assert!(!said.contains("no `.kite` file directly in it"), "{}", said);
        }
    }
    let main = p.file("ok.kite", "use util\nuse lib\n\nfn main() {\n  io.print(util.one() + lib.two())\n}\n");
    assert_eq!(p.run(&main).expect("compiles"), "3\n");
}

/// An opaque struct — a `pub struct` with a field its module did not mark
/// `pub` — cannot be taken apart outside its module, not even by naming only
/// the fields it publishes (§4.3). That was accepted, with or without `..`.
#[test]
fn an_opaque_struct_cannot_be_destructured_by_its_public_fields() {
    let p = Project::new("opaque-pattern");
    p.file(
        "m.kite",
        "pub struct P {\n  pub k: int\n  hidden: int\n}\n\n\
         pub fn make() -> P {\n  return P{ k: 1, hidden: 2 }\n}\n",
    );
    for (name, pattern) in [("a.kite", "m.P{ k: k }"), ("b.kite", "m.P{ k: k, .. }")] {
        let main = p.file(
            name,
            &format!(
                "use m\n\nfn main() {{\n  let p = m.make()\n  match p {{\n    {} => io.print(k),\n  }}\n}}\n",
                pattern
            ),
        );
        let said = p.run(&main).expect_err("opaque");
        assert!(said.contains("E0401"), "{}", said);
        assert!(said.contains("cannot be taken apart outside module `m`"), "{}", said);
        assert!(said.contains("1 previous error"), "{}", said);
    }
    // Reading a public field with `.` is still fine.
    let main = p.file("ok.kite", "use m\n\nfn main() {\n  io.print(m.make().k)\n}\n");
    assert_eq!(p.run(&main).expect("compiles"), "1\n");
}

/// A package that depends on itself through others is refused by `kitec
/// run` and `check` as `kitec pkg` refuses it — even when no `use` reaches
/// the package that closes the cycle, whose manifest the loader never read.
#[test]
fn a_package_cycle_is_refused_by_every_command() {
    let p = Project::new("package-cycle");
    let manifest = |name: &str, dependency: &str| {
        format!("[package]\nname = \"{}\"\nversion = \"1.0.0\"\n\n[dependencies]\n{}\n", name, dependency)
    };
    p.file("a/kite.toml", &manifest("a", "b = \"../b\""));
    p.file("a/a.kite", "pub fn f() -> int {\n  return 1\n}\n");
    p.file("b/kite.toml", &manifest("b", "app = \"../app\""));
    p.file("b/b.kite", "pub fn g() -> int {\n  return 2\n}\n");
    p.file("app/kite.toml", &manifest("app", "a = \"../a\""));
    let main = p.file("app/main.kite", "use a\n\nfn main() {\n  io.print(a.f())\n}\n");
    let said = p.run(&main).expect_err("a package cycle");
    assert!(said.contains("E0402"), "{}", said);
    assert!(said.contains("dependency cycle: app -> a -> b -> app"), "{}", said);
    assert!(said.contains("kite.toml:6"), "at the dependency on `a`: {}", said);
    // Without the edge back, it compiles.
    p.file("b/kite.toml", "[package]\nname = \"b\"\nversion = \"1.0.0\"\n");
    assert_eq!(p.run(&main).expect("compiles"), "1\n");
}

/// One cause, one error. Building another module's private struct was
/// ``private to module `m` `` and then ``cannot be built outside module `m` ``
/// at the same span; `5 = x` was the parser's `E0114` and then the checker's
/// `E0200`.
#[test]
fn a_private_struct_literal_and_a_literal_target_are_one_error_each() {
    let p = Project::new("one-cause");
    p.file("m.kite", "struct Hid {\n  n: int\n}\n\npub fn one() -> int {\n  return 1\n}\n");
    let main = p.file(
        "main.kite",
        "use m\n\nfn main() {\n  let h = m.Hid{ n: 1 }\n  io.print(m.one())\n}\n",
    );
    let said = p.run(&main).expect_err("private");
    assert!(said.contains("private to module `m`"), "{}", said);
    assert!(said.contains("1 previous error"), "{}", said);
    let main = p.file("assign.kite", "fn main() {\n  var x = 1\n  5 = x\n  io.print(x)\n}\n");
    let said = p.run(&main).expect_err("not a place");
    assert!(said.contains("E0114"), "{}", said);
    assert!(said.contains("1 previous error"), "{}", said);
}

/// A cycle back to the entry file is one error. The closing `use main`
/// recorded a spelling for a module that does not exist — the entry is not a
/// module — so `main.base()` behind it said `main` declares nothing public of
/// a name it declares `pub`.
#[test]
fn a_cycle_back_to_the_entry_is_one_error() {
    let p = Project::new("entry-cycle-once");
    p.file("helper/h.kite", "use main\n\npub fn f() -> int {\n  return main.base() + 1\n}\n");
    let main = p.file(
        "main.kite",
        "use helper\n\npub fn base() -> int {\n  return 1\n}\n\nfn main() {\n  io.print(helper.f())\n}\n",
    );
    let said = p.run(&main).expect_err("a cycle");
    assert!(said.contains("E0402"), "{}", said);
    assert!(!said.contains("E0111"), "{}", said);
    assert!(said.contains("1 previous error"), "{}", said);
}

/// A module that imports itself is told so, and to delete the line — not
/// handed an empty chain and advice to extract a third module.
#[test]
fn a_module_that_imports_itself_is_told_so() {
    let p = Project::new("self-import");
    let entry = p.file("selfentry.kite", "use selfentry\n\nfn main() {\n  io.print(1)\n}\n");
    let said = p.run(&entry).expect_err("a cycle of one");
    assert!(said.contains("module `selfentry` imports itself"), "{}", said);
    assert!(said.contains("remove this `use`"), "{}", said);
    assert!(!said.contains("the chain is"), "{}", said);
    p.file("ui/a.kite", "use ui\n\npub fn f() -> int {\n  return 1\n}\n");
    let main = p.file("main.kite", "use ui\n\nfn main() {\n  io.print(ui.f())\n}\n");
    let said = p.run(&main).expect_err("a cycle of one");
    assert!(said.contains("module `ui` imports itself"), "{}", said);
}

/// A module is named in `E0111` and `E0204` the way the `use` that brought it
/// in writes it. The title and the note said `money`; the label under the
/// member said `src/money`, its identity below the package's `kite.toml`,
/// which nothing in the program ever wrote.
#[test]
fn a_missing_member_names_its_module_as_the_use_writes_it() {
    let p = Project::new("missing-member-label");
    p.file("kite.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n");
    p.file("src/money.kite", "pub fn zero() -> int {\n  return 0\n}\n");
    let main = p.file(
        "src/main.kite",
        "use money\n\nfn main() {\n  let a: money.Nothing = 1\n  io.print(money.nothing())\n}\n",
    );
    let said = p.run(&main).expect_err("members that are not there");
    assert!(said.contains("E0111") && said.contains("E0204"), "{}", said);
    assert!(said.contains("`money` declares nothing public of this name"), "{}", said);
    assert!(said.contains("`money` declares no public type of this name"), "{}", said);
    assert!(!said.contains("src/money"), "{}", said);
}

/// A block string that never closes reads the rest of the file as its text, so
/// a `\(a)` after it is some later line and not a hole its author wrote. It
/// named the very binding being defined, and was `cannot find `a`` as a second
/// error for the one mistake. (Not a corpus file: a file with a lexical error
/// cannot be formatted, and the tree's Kite is always formatted.)
#[test]
fn a_hole_swallowed_by_an_unterminated_block_string_is_not_a_second_error() {
    let p = Project::new("swallowed-hole");
    let main = p.file("main.kite", "fn main() {\n    let a = \"\"\"ab\n    io.print(\"\\(a)\")\n}\n");
    let said = p.run(&main).expect_err("an unterminated string");
    assert!(said.contains("E0001"), "{}", said);
    assert!(!said.contains("E0111"), "{}", said);
    assert!(said.contains("1 previous error"), "{}", said);
}
