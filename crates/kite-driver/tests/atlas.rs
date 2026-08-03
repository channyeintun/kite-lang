//! The glyph atlas's plan and cache, tested where they can be: under Node.
//!
//! There is no canvas in Node and this project takes no dependency to fake
//! one, so what is tested here is exactly what is testable without a
//! rasteriser: the *plan* — which runs the atlas may serve, where each glyph
//! lands, that a combining mark advances nothing, that a right-to-left run
//! reverses by cluster — and the *cache* — that a glyph is rasterised once
//! and blitted thereafter, which is the measurable half of "prove it helps".
//!
//! What is not tested here is pixel identity between a blitted tile and
//! `fillText`, because that needs a browser. The plan is built so the
//! question mostly cannot arise — a run is refused unless its glyph advances
//! sum to the measured width of the whole run — and the one admitted
//! difference, snapping blits to the device pixel grid, is stated in the
//! glue where it happens.

use std::process::Command;

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Run a script against the generated glue, and return what it printed.
fn under_node(name: &str, script: &str) -> String {
    let dir = std::env::temp_dir().join(format!("kite-atlas-{}-{}", name, std::process::id()));
    std::fs::create_dir_all(&dir).expect("work directory");
    std::fs::write(
        dir.join("app.js"),
        kite_driver::generate_glue(&["x".to_string()], "app.wasm"),
    )
    .expect("write glue");
    std::fs::write(dir.join("run.mjs"), script).expect("write script");
    let output = Command::new("node")
        .arg(dir.join("run.mjs"))
        .output()
        .expect("node runs");
    assert!(
        output.status.success(),
        "{} failed under node:\n{}",
        name,
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8(output.stdout).expect("utf-8")
}

const HARNESS: &str = r#"import { atlasPlan, glyphAtlas, firstStrongRtl } from "./app.js";
const say = (label, value) => console.log(label + " " + JSON.stringify(value));
"#;

/// The nominal measurer: every code point eight units, like the bytecode VM.
#[test]
fn a_plain_run_is_planned_one_glyph_per_character() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = under_node(
        "plain",
        &format!(
            "{HARNESS}\
             say('plan', atlasPlan('abc'));\n"
        ),
    );
    assert_eq!(
        out,
        "plan [{\"ch\":\"a\",\"x\":0},{\"ch\":\"b\",\"x\":8},{\"ch\":\"c\",\"x\":16}]\n"
    );
}

/// The rule the task names: a `Mn` advances nothing. The mark shares its pen
/// with the character after it, and the run is no wider for carrying it.
#[test]
fn a_combining_mark_advances_nothing() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = under_node(
        "marks",
        &format!(
            "{HARNESS}\
             const m = (s) => [...s].reduce((t, c) => t + (c === '\\u0301' ? 0 : 8), 0);\n\
             const plan = atlasPlan('ab\\u0301c', m);\n\
             say('mark-x', plan[2].x);\n\
             say('next-x', plan[3].x);\n\
             say('count', plan.length);\n"
        ),
    );
    // The mark rides at the pen its base already moved to, and `c` shares it.
    assert_eq!(out, "mark-x 16\nnext-x 16\ncount 4\n");
}

/// Everything the plan must refuse rather than guess at. Each of these would
/// draw wrongly one glyph at a time, and the refusal is what routes it to
/// `fillText`, which draws it correctly.
///
/// The Brahmic cases are here because they were once *missing*. The refusals
/// were a list of scripts named one at a time, and Burmese was not on it: a
/// medial ra is encoded after its consonant and drawn wrapped around the front
/// of it, so one tile per code point scattered the marks along the line. The
/// rule is now an allow-list, and these are the proof that it holds for
/// scripts nobody thought to name.
#[test]
fn the_plan_refuses_what_it_cannot_prove() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = under_node(
        "refusals",
        &format!(
            "{HARNESS}\
             say('emoji', atlasPlan('\\u{{1F44D}}'));\n\
             say('zwj', atlasPlan('a\\u200Db'));\n\
             say('selector', atlasPlan('a\\uFE0Fb'));\n\
             say('arabic', atlasPlan('\\u0628\\u0633'));\n\
             say('mixed', atlasPlan('a\\u05D0'));\n\
             say('bracket-rtl', atlasPlan('\\u05D0('));\n\
             say('burmese', atlasPlan('\\u1006\\u101A\\u103A\\u101C\\u103A'));\n\
             say('medial-ra', atlasPlan('\\u1021\\u1015\\u103C\\u1014\\u103A'));\n\
             say('devanagari', atlasPlan('\\u0928\\u092E\\u0938\\u094D\\u0924\\u0947'));\n\
             say('thai', atlasPlan('\\u0E2A\\u0E27\\u0E31\\u0E2A\\u0E14\\u0E35'));\n\
             say('hangul-jamo', atlasPlan('\\u1100\\u1161'));\n\
             const kerned = (s) => [...s].length === 1 ? 8 : 14;\n\
             say('kerned', atlasPlan('AV', kerned));\n"
        ),
    );
    assert_eq!(
        out,
        "emoji null\nzwj null\nselector null\narabic null\nmixed null\nbracket-rtl null\n\
         burmese null\nmedial-ra null\ndevanagari null\nthai null\nhangul-jamo null\n\
         kerned null\n"
    );
}

/// A right-to-left run reverses by cluster: the last letter is drawn first,
/// and a mark stays with its base. Unjoined Arabic is refused above, but the
/// presentation forms `std/text.join_arabic` produces are single glyphs and
/// are served.
#[test]
fn a_right_to_left_run_reverses_by_cluster() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = under_node(
        "rtl",
        &format!(
            "{HARNESS}\
             const m = (s) => [...s].reduce((t, c) => t + (c === '\\u05B0' ? 0 : 8), 0);\n\
             const hebrew = atlasPlan('\\u05D0\\u05B0\\u05D1', m);\n\
             say('first', hebrew[0]);\n\
             say('base', hebrew[1]);\n\
             say('mark', hebrew[2]);\n\
             const forms = atlasPlan('\\uFE91\\uFEB4\\uFEE2');\n\
             say('joined-first', forms[0].ch === '\\uFEE2');\n\
             say('strong', [firstStrongRtl('\\u05D0'), firstStrongRtl('abc'), firstStrongRtl('123')]);\n"
        ),
    );
    assert_eq!(
        out,
        "first {\"ch\":\"\u{05D1}\",\"x\":0}\n\
         base {\"ch\":\"\u{05D0}\",\"x\":8}\n\
         mark {\"ch\":\"\u{05B0}\",\"x\":16}\n\
         joined-first true\n\
         strong [true,false,false]\n"
    );
}

/// The cache is the point of the atlas: the first occurrence of a glyph
/// rasterises a tile, every later occurrence copies it. The tile maker is
/// injected so this runs where no canvas exists, and the blit positions are
/// asserted against the same arithmetic `fillText` would use — which is as
/// close to "the same picture" as a test without a rasteriser can get.
#[test]
fn a_glyph_is_rasterised_once_and_blitted_thereafter() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = under_node(
        "cache",
        &format!(
            "{HARNESS}\
             const blits = [];\n\
             const ctx = {{ drawImage: (tile, x, y) => blits.push([x, y]) }};\n\
             let made = 0;\n\
             const stub = (ch, colour) => {{\n\
             \x20 made += 1;\n\
             \x20 return {{ canvas: ch, left: 0, top: 0, w: 8, h: 16 }};\n\
             }};\n\
             const atlas = glyphAtlas(ctx, 'stub', stub);\n\
             say('served', atlas.text(0, 0, 'aaa', 0));\n\
             say('made', made);\n\
             say('blits', blits);\n\
             say('again', atlas.text(0, 20, 'aaa', 0));\n\
             say('still', made);\n\
             say('reused', atlas.stats().reused);\n\
             say('recolour', atlas.text(0, 40, 'a', 7));\n\
             say('now', made);\n"
        ),
    );
    // Three blits from one rasterisation; the second run makes nothing new;
    // a new colour is a new tile, because the key includes the colour.
    assert_eq!(
        out,
        "served true\nmade 1\nblits [[0,0],[8,0],[16,0]]\nagain true\nstill 1\nreused 5\nrecolour true\nnow 2\n"
    );
}

/// Where a tile cannot be made, the whole run is declined — never half a run
/// from tiles and half from `fillText`.
#[test]
fn a_run_the_tiles_cannot_serve_is_declined_whole() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = under_node(
        "decline",
        &format!(
            "{HARNESS}\
             const blits = [];\n\
             const ctx = {{ drawImage: (...args) => blits.push(args) }};\n\
             const atlas = glyphAtlas(ctx, 'stub', () => null);\n\
             say('served', atlas.text(0, 0, 'ab', 0));\n\
             say('blits', blits.length);\n\
             say('fallbacks', atlas.stats().fallbacks);\n"
        ),
    );
    assert_eq!(out, "served false\nblits 0\nfallbacks 1\n");
}
