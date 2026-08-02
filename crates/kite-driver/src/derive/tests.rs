//! The expander's own tests.
//!
//! These check the *text* it produces and the diagnostics it refuses with.
//! Whether the produced text behaves correctly is checked where behaviour is
//! checked — in the differential corpus, on both backends, because a derived
//! body that agreed with itself and with nothing else would be worthless.

use crate::{compile, Emit};

fn errors_of(src: &str) -> String {
    let c = compile("derive_test.kite", src, Emit::Check);
    c.render_diagnostics()
}

#[test]
fn a_derived_debug_renders_every_field() {
    let out = run(
        "@derive(Debug)\n\
         struct Point {\n    x: int\n    label: str\n}\n\
         fn main() {\n\
         \x20   io.print(Point{ x: 1, label: \"a\\\"b\" }.debug())\n\
         }\n",
    );
    assert_eq!(out, "Point{ x: 1, label: \"a\\\"b\" }\n");
}

#[test]
fn a_derived_debug_walks_slices_maps_and_optionals() {
    let out = run(
        "@derive(Debug)\n\
         struct Bag {\n    tags: [str]\n    counts: {str: int}\n    note: Option<int>\n}\n\
         fn main() {\n\
         \x20   let full = Bag{ tags: [\"a\", \"b\"], counts: {\"n\": 2}, note: 7 }\n\
         \x20   io.print(full.debug())\n\
         \x20   let empty: [str] = []\n\
         \x20   let none: {str: int} = {}\n\
         \x20   io.print(Bag{ tags: empty, counts: none, note: nil }.debug())\n\
         }\n",
    );
    assert_eq!(
        out,
        "Bag{ tags: [\"a\", \"b\"], counts: {\"n\": 2}, note: 7 }\n\
         Bag{ tags: [], counts: {}, note: nil }\n"
    );
}

#[test]
fn a_derived_debug_names_the_variant() {
    let out = run(
        "@derive(Debug)\n\
         enum Shape {\n    Dot\n    Rect(int, int)\n    Named(label: str)\n}\n\
         fn main() {\n\
         \x20   io.print(Shape.Dot.debug())\n\
         \x20   io.print(Shape.Rect(2, 3).debug())\n\
         \x20   io.print(Shape.Named(label: \"x\").debug())\n\
         }\n",
    );
    assert_eq!(out, "Dot\nRect(2, 3)\nNamed(label: \"x\")\n");
}

#[test]
fn equal_values_hash_alike_and_different_ones_do_not() {
    let out = run(
        "@derive(Hash)\n\
         struct Key {\n    name: str\n    n: int\n}\n\
         fn main() {\n\
         \x20   let a = Key{ name: \"one\", n: 1 }\n\
         \x20   let b = Key{ name: \"one\", n: 1 }\n\
         \x20   let c = Key{ name: \"one\", n: 2 }\n\
         \x20   io.print(a.hash() == b.hash())\n\
         \x20   io.print(a.hash() == c.hash())\n\
         }\n",
    );
    assert_eq!(out, "true\nfalse\n");
}

#[test]
fn a_derived_pair_round_trips_through_json() {
    let out = run(
        "use std/json\n\
         @derive(Encode, Decode)\n\
         struct User {\n    name: str\n    age: int\n    tags: [str]\n}\n\
         fn round(u: User) -> (User, error) {\n\
         \x20   let (doc, err) = json.parse(json.stringify(u.encode()))\n\
         \x20   check err\n\
         \x20   return User.decode(doc)\n\
         }\n\
         fn main() {\n\
         \x20   let (back, err) = round(User{ name: \"ada\", age: 36, tags: [\"maths\"] })\n\
         \x20   if err != nil {\n        io.print(err.message())\n        return\n    }\n\
         \x20   io.print(back.name)\n\
         \x20   io.print(back.age)\n\
         \x20   io.print(back.tags[0])\n\
         }\n",
    );
    assert_eq!(out, "ada\n36\nmaths\n");
}

/// The whole walk at once: an enum with all three payload shapes, nested in a
/// struct with a slice, a map and an optional, through text and back.
#[test]
fn every_shape_a_derive_walks_survives_a_round_trip() {
    let out = run(SCENE);
    assert_eq!(
        out,
        "Scene{ title: \"one\", shapes: [Dot, Rect(2, 3), Named(label: \"n\", size: 1.5)], \
         meta: {\"k\": \"v\"}, note: nil }\n\
         {\"title\":\"one\",\"shapes\":[\"Dot\",{\"Rect\":[2,3]},\
         {\"Named\":{\"label\":\"n\",\"size\":1.5}}],\"meta\":{\"k\":\"v\"},\"note\":null}\n\
         true\n\
         true\n"
    );
}

/// The same program, for the differential suite to run on both backends: a
/// derived body is generated Kite, so if the two backends disagreed about it
/// they would be disagreeing about ordinary code.
pub const SCENE: &str = "use std/json\n\
     @derive(Debug, Hash, Encode, Decode)\n\
     enum Shape {\n    Dot\n    Rect(int, int)\n    Named(label: str, size: float)\n}\n\
     @derive(Debug, Hash, Encode, Decode)\n\
     struct Scene {\n\
     \x20   title: str\n    shapes: [Shape]\n    meta: {str: str}\n    note: Option<str>\n}\n\
     fn round(s: Scene) -> (Scene, error) {\n\
     \x20   let text = json.stringify(s.encode())\n\
     \x20   io.print(text)\n\
     \x20   let (doc, err) = json.parse(text)\n\
     \x20   check err\n\
     \x20   return Scene.decode(doc)\n\
     }\n\
     fn main() {\n\
     \x20   let s = Scene{\n\
     \x20       title: \"one\",\n\
     \x20       shapes: [Shape.Dot, Shape.Rect(2, 3), Shape.Named(label: \"n\", size: 1.5)],\n\
     \x20       meta: {\"k\": \"v\"},\n\
     \x20       note: nil,\n\
     \x20   }\n\
     \x20   io.print(s.debug())\n\
     \x20   let (back, err) = round(s)\n\
     \x20   if err != nil {\n        io.print(err.message())\n        return\n    }\n\
     \x20   io.print(back.debug() == s.debug())\n\
     \x20   io.print(back.hash() == s.hash())\n\
     }\n";

#[test]
fn a_missing_field_is_an_error_rather_than_a_zero() {
    let out = run(
        "use std/json\n\
         @derive(Decode)\n\
         struct User {\n    name: str\n    age: int\n}\n\
         fn main() {\n\
         \x20   let (doc, err) = json.parse(\"{\\\"name\\\": \\\"ada\\\"}\")\n\
         \x20   if err != nil {\n        return\n    }\n\
         \x20   let (u, uerr) = User.decode(doc)\n\
         \x20   if uerr != nil {\n        io.print(uerr.message())\n        return\n    }\n\
         \x20   io.print(u.name)\n\
         }\n",
    );
    assert_eq!(out, "User.age: expected a whole number\n");
}

#[test]
fn deriving_something_nothing_derives_says_what_does() {
    let text = errors_of("@derive(Ord)\nstruct P {\n    x: int\n}\nfn main() {\n}\n");
    assert!(text.contains("nothing derives `Ord`"), "{}", text);
    assert!(text.contains("Debug, Hash, Encode and Decode"), "{}", text);
}

#[test]
fn display_is_refused_with_the_reason_it_is_refused() {
    let text = errors_of("@derive(Display)\nstruct P {\n    x: int\n}\nfn main() {\n}\n");
    assert!(text.contains("mechanical answer would be wrong"), "{}", text);
}

#[test]
fn eq_is_refused_because_the_language_already_does_it() {
    let text = errors_of("@derive(Eq)\nstruct P {\n    x: int\n}\nfn main() {\n}\n");
    assert!(text.contains("already structural"), "{}", text);
}

#[test]
fn a_field_whose_type_does_not_derive_names_that_type() {
    let text = errors_of(
        "struct Inner {\n    x: int\n}\n\
         @derive(Debug)\n\
         struct Outer {\n    inner: Inner\n}\n\
         fn main() {\n}\n",
    );
    assert!(text.contains("`Inner` does not derive `Debug`"), "{}", text);
}

#[test]
fn a_nested_type_that_derives_is_walked_through() {
    let out = run(
        "@derive(Debug)\n\
         struct Inner {\n    x: int\n}\n\
         @derive(Debug)\n\
         struct Outer {\n    inner: Inner\n}\n\
         fn main() {\n\
         \x20   io.print(Outer{ inner: Inner{ x: 3 } }.debug())\n\
         }\n",
    );
    assert_eq!(out, "Outer{ inner: Inner{ x: 3 } }\n");
}

#[test]
fn a_hand_written_impl_beside_a_derive_is_refused_once() {
    let text = errors_of(
        "@derive(Debug)\n\
         struct P {\n    x: int\n}\n\
         impl Debug for P {\n    fn debug(self) -> str {\n        return \"P\"\n    }\n}\n\
         fn main() {\n}\n",
    );
    assert!(text.contains("already implements `Debug`"), "{}", text);
}

#[test]
fn a_derive_on_a_generic_type_says_why_not() {
    let text = errors_of("@derive(Debug)\nstruct Box<T> {\n    value: T\n}\nfn main() {\n}\n");
    assert!(text.contains("generic type"), "{}", text);
    assert!(text.contains("bound on every one"), "{}", text);
}

#[test]
fn a_derive_in_front_of_a_function_is_refused() {
    let text = errors_of("@derive(Debug)\nfn main() {\n}\n");
    assert!(text.contains("only for a struct or an enum"), "{}", text);
}

#[test]
fn nothing_is_generated_when_nothing_derives() {
    let c = compile("plain.kite", "fn main() {\n    io.print(1)\n}\n", Emit::Check);
    assert!(
        !c.sources.iter().any(|(_, name)| name == "<derive>"),
        "a program with no derives should not carry a generated file"
    );
}

/// Compile and run, returning what the program printed.
fn run(src: &str) -> String {
    let c = compile("derive_test.kite", src, Emit::Check);
    assert!(!c.failed(), "{}", c.render_diagnostics());
    let mut out = Vec::new();
    c.run(&mut out).expect("runs");
    String::from_utf8(out).expect("utf-8")
}
