//! The retained scene graph, and the diff over it.
//!
//! A `view` describes the whole picture every frame, because a function from a
//! model to a picture cannot describe anything less. What keeps that
//! affordable is that the picture is recorded rather than painted, and the
//! recording survives between frames: the next one is compared with it, and
//! only the difference is painted.
//!
//! The diff and the damage calculation are pure functions of two recordings,
//! so they are tested here directly, under Node, with no browser and no
//! canvas. What they *drive* — patching DOM nodes in place, clipping a canvas
//! to a damage rectangle — needs a real document and is not tested here; that
//! is stated rather than implied.

use std::process::Command;

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Run a script against the generated glue, and return what it printed.
fn under_node(name: &str, script: &str) -> String {
    let dir = std::env::temp_dir().join(format!("kite-scene-{}-{}", name, std::process::id()));
    std::fs::create_dir_all(&dir).expect("work directory");
    // The glue is generated from a program, so a trivial one stands in: what is
    // under test is the frame machinery the glue always carries, not anything
    // the program does.
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

const HARNESS: &str = r#"import { diffFrames, damageOf, callBounds, sameFrame, rectsOverlap } from "./app.js";
const say = (label, value) => console.log(label + " " + JSON.stringify(value));
"#;

#[test]
fn an_unchanged_frame_produces_no_work() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = under_node(
        "unchanged",
        &format!(
            "{HARNESS}\
             const frame = [['r', 0, 0, 10, 10, 255], ['t', 1, 1, 'hi', 0]];\n\
             const d = diffFrames(frame, [['r', 0, 0, 10, 10, 255], ['t', 1, 1, 'hi', 0]]);\n\
             say('same', d.same);\n\
             say('damage', damageOf(frame, frame, d));\n"
        ),
    );
    assert_eq!(out, "same true\ndamage []\n");
}

#[test]
fn a_first_frame_is_never_the_same_as_nothing() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = under_node(
        "first",
        &format!(
            "{HARNESS}\
             const d = diffFrames(null, [['r', 0, 0, 10, 10, 255]]);\n\
             say('same', d.same);\n\
             say('patchable', d.patchable);\n"
        ),
    );
    assert_eq!(out, "same false\npatchable false\n");
}

/// The case the whole phase is for: one label in a long frame changed, and
/// what has to be repainted is that label's rectangle and nothing else.
#[test]
fn one_changed_label_damages_only_its_own_rectangle() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = under_node(
        "one-label",
        &format!(
            "{HARNESS}\
             const before = [\n\
             \x20 ['r', 0, 0, 200, 100, 16777215],\n\
             \x20 ['t', 10, 10, 'one', 0],\n\
             \x20 ['t', 10, 40, 'two', 0],\n\
             \x20 ['t', 10, 70, 'three', 0],\n\
             ];\n\
             const after = [\n\
             \x20 ['r', 0, 0, 200, 100, 16777215],\n\
             \x20 ['t', 10, 10, 'one', 0],\n\
             \x20 ['t', 10, 40, 'TWO!', 0],\n\
             \x20 ['t', 10, 70, 'three', 0],\n\
             ];\n\
             const d = diffFrames(before, after);\n\
             say('from', d.from);\n\
             say('oldEnd', d.oldEnd);\n\
             say('newEnd', d.newEnd);\n\
             say('patchable', d.patchable);\n\
             say('damage', damageOf(before, after, d));\n"
        ),
    );
    // Only index 2 differs, so the damage is the union of the old run's box and
    // the new one's — the new text is wider, and the rectangle has to cover the
    // pixels the old one left behind as well as the ones the new one needs.
    assert_eq!(
        out,
        "from 2\noldEnd 3\nnewEnd 3\npatchable true\ndamage [[10,40,32,16]]\n"
    );
}

#[test]
fn an_inserted_row_leaves_the_rows_after_it_alone() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = under_node(
        "inserted",
        &format!(
            "{HARNESS}\
             const before = [['t', 0, 0, 'a', 0], ['t', 0, 20, 'c', 0]];\n\
             const after = [['t', 0, 0, 'a', 0], ['t', 0, 10, 'b', 0], ['t', 0, 20, 'c', 0]];\n\
             const d = diffFrames(before, after);\n\
             say('from', d.from);\n\
             say('oldEnd', d.oldEnd);\n\
             say('newEnd', d.newEnd);\n\
             say('patchable', d.patchable);\n"
        ),
    );
    // The common suffix is found, so the only difference is the inserted row.
    // The frames are different lengths, so nothing may be patched in place —
    // node `i` and call `i` would no longer describe the same thing.
    assert_eq!(out, "from 1\noldEnd 1\nnewEnd 2\npatchable false\n");
}

/// A clip moves everything drawn inside it, and this diff does not track which
/// calls those were. Repainting everything is the honest answer.
#[test]
fn a_changed_clip_gives_up_and_says_so() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = under_node(
        "clip",
        &format!(
            "{HARNESS}\
             const before = [['c', 0, 0, 10, 10], ['t', 0, 0, 'a', 0], ['u']];\n\
             const after = [['c', 0, 5, 10, 10], ['t', 0, 0, 'a', 0], ['u']];\n\
             const d = diffFrames(before, after);\n\
             say('damage', damageOf(before, after, d));\n"
        ),
    );
    assert_eq!(out, "damage null\n");
}

#[test]
fn overlapping_damage_is_merged_and_a_crowd_of_it_collapses() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = under_node(
        "merge",
        &format!(
            "{HARNESS}\
             const before = [['r', 0, 0, 10, 10, 1], ['r', 5, 5, 10, 10, 1]];\n\
             const after = [['r', 0, 0, 10, 10, 2], ['r', 5, 5, 10, 10, 2]];\n\
             say('merged', damageOf(before, after, diffFrames(before, after)));\n\
             const wide = [];\n\
             const wider = [];\n\
             for (let i = 0; i < 40; i += 1) {{\n\
             \x20 wide.push(['r', i * 20, 0, 5, 5, 1]);\n\
             \x20 wider.push(['r', i * 20, 0, 5, 5, 2]);\n\
             }}\n\
             const many = damageOf(wide, wider, diffFrames(wide, wider), 4);\n\
             say('collapsed', many);\n"
        ),
    );
    assert_eq!(
        out,
        "merged [[0,0,15,15]]\ncollapsed [[0,0,785,5]]\n"
    );
}

#[test]
fn a_rectangles_bounds_are_its_own_and_a_runs_are_measured() {
    if !node_available() {
        eprintln!("skipping: node is not installed");
        return;
    }
    let out = under_node(
        "bounds",
        &format!(
            "{HARNESS}\
             say('rect', callBounds(['r', 1, 2, 3, 4, 0]));\n\
             say('text', callBounds(['t', 1, 2, 'abc', 0]));\n\
             say('bigger', callBounds(['t', 1, 2, 'abc', 0, 32, 400]));\n\
             say('clip', callBounds(['c', 1, 2, 3, 4]));\n\
             say('unclip', callBounds(['u']));\n"
        ),
    );
    // The nominal advance is eight units a character and the nominal line
    // height sixteen, which is what the bytecode VM uses too — so a damage
    // rectangle is comparable across backends under test.
    assert_eq!(
        out,
        "rect [1,2,3,4]\ntext [1,2,24,16]\nbigger [1,2,48,32]\nclip null\nunclip null\n"
    );
}

/// A minimal `document` — enough for `domRenderer`, and no more.
///
/// The DOM renderer is the one part of the glue that needs a document, and the
/// header above says it is not tested here. That was true until an alpha set on
/// one box dimmed a whole scrolling list: `opacity` on an element forms a
/// stacking context, so it applies to everything inside it, and a clip is
/// *structure* rather than paint. The stub is twenty lines and the bug was
/// invisible from the recording alone, which is what makes it worth having.
const DOM_STUB: &str = r#"
const make = (tag) => ({
  tag,
  style: {},
  children: [],
  textContent: '',
  dataset: {},
  setAttribute() {},
  removeAttribute() {},
  appendChild(child) { this.children.push(child); child.parent = this; return child; },
  replaceChildren(...kids) { this.children = kids; },
  remove() {},
  getBoundingClientRect: () => ({ left: 0, top: 0, width: 0, height: 0 }),
});
globalThis.document = { createElement: make };
const root = make('div');
"#;

#[test]
fn a_clip_is_structure_and_never_carries_the_alpha_in_force() {
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let out = under_node(
        "clip-opacity",
        &format!(
            r#"import {{ domRenderer, setAlpha }} from "./app.js";
{stub}
const say = (label, value) => console.log(label + " " + JSON.stringify(value));
const r = domRenderer(root);

// A translucent thing, then a clip opened while that alpha is still in force —
// which is exactly what a state layer followed by a scrolling viewport does.
setAlpha(0.08);
r.rect(0, 0, 10, 10, 0x000000);
r.clip(0, 0, 100, 100);
r.rect(0, 0, 10, 10, 0xffffff);
r.unclip();

const painted = [];
const walk = (el) => {{
  if (el.style.overflow === 'hidden') painted.push(['clip', el.style.opacity]);
  else if (el.tag) painted.push([el.tag, el.style.opacity]);
  for (const kid of el.children) walk(kid);
}};
for (const kid of root.children) walk(kid);
const clips = painted.filter((p) => p[0] === 'clip');
say("clipOpacity", clips.map((c) => c[1]));
say("translucentThingsStillFade", painted.some((p) => p[1] === '0.08'));
"#,
            stub = DOM_STUB
        ),
    );
    // A clip must be fully opaque — an empty string is how the renderer says
    // "no opacity of my own". Anything else dims everything it contains.
    assert!(
        out.contains(r#"clipOpacity [""]"#),
        "a clip carried an opacity:\n{}",
        out
    );
    // …while the things that actually draw still honour the alpha.
    assert!(
        out.contains("translucentThingsStillFade true"),
        "alpha stopped reaching the calls that draw:\n{}",
        out
    );
}
