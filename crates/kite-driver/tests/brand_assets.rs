//! The mark is drawn once.
//!
//! Four files carry the Kite mark: the site's two variants, the favicon, and
//! the editor extension's tile. They cannot all reference one file — an
//! extension is packaged offline, and a favicon has to stand alone — so three
//! of them hold a copy of the geometry.
//!
//! A copy that nobody checks is a copy that drifts, and a logo that is subtly
//! different in one of four places is worse than one that is wrong in all
//! four, because nobody notices. So the copies are checked here: the shape
//! data of each asset must equal the shape data of the file it came from.
//!
//! If this test fails, the fix is to change `site/kite-mark.svg` (or
//! `site/kite-mark-solo.svg`) and copy it out again — not to edit the copy.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    let path = repo().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e))
}

/// Every geometry-bearing attribute of every shape, in order: the `points` of
/// a polygon and the `d` of a path, each with its transform and stroke width.
///
/// Colour is deliberately not included. The same drawing in four colourways is
/// the point; the same drawing at four different geometries is the bug.
fn shapes(svg: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tag in svg.split('<').skip(1) {
        let name = tag.split([' ', '\n', '\t', '/', '>']).next().unwrap_or("");
        if name != "polygon" && name != "path" {
            continue;
        }
        let mut parts = Vec::new();
        for attr in ["points", "d", "transform", "stroke-width", "stroke-linejoin"] {
            if let Some(value) = attribute(tag, attr) {
                parts.push(format!("{}={}", attr, normalise(&value)));
            }
        }
        out.push(parts.join(" "));
    }
    out
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{}=\"", name);
    let start = tag.find(&needle)?;
    // `stroke-width` must not match inside `stroke-linejoin`, and `d` must not
    // match inside `stroke-width`: an attribute name begins after whitespace.
    let before = tag[..start].chars().next_back();
    if before.is_some_and(|c| !c.is_whitespace()) {
        return None;
    }
    let rest = &tag[start + needle.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Whitespace inside an attribute is layout, not geometry: a path broken over
/// two lines is the same path.
fn normalise(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The tile embeds the full mark, scaled by a group transform. The shapes
/// inside it are the mark's own and must match it exactly.
#[test]
fn the_editor_tile_carries_the_marks_geometry() {
    let mark = shapes(&read("site/kite-mark.svg"));
    let tile = shapes(&read("editors/vscode/icon.svg"));
    assert_eq!(mark.len(), 2, "the mark is a sail and a tail: {:?}", mark);
    assert_eq!(
        mark, tile,
        "editors/vscode/icon.svg has drifted from site/kite-mark.svg"
    );
}

#[test]
fn the_favicon_carries_the_small_marks_geometry() {
    let solo = shapes(&read("site/kite-mark-solo.svg"));
    let favicon = shapes(&read("site/favicon.svg"));
    assert_eq!(solo.len(), 1, "the small mark is a sail alone: {:?}", solo);
    assert_eq!(
        solo, favicon,
        "site/favicon.svg has drifted from site/kite-mark-solo.svg"
    );
}

/// The two variants are meant to differ — the small one drops the tail, widens
/// the sail and thickens the stroke — so a test that they are *the same* would
/// be wrong. What matters is that the small one really is the small one.
#[test]
fn the_small_mark_is_not_merely_the_large_one() {
    let mark = shapes(&read("site/kite-mark.svg"));
    let solo = shapes(&read("site/kite-mark-solo.svg"));
    assert_ne!(mark[0], solo[0]);
    assert!(solo[0].contains("stroke-width=8"), "{}", solo[0]);
    assert!(mark[0].contains("stroke-width=6"), "{}", mark[0]);
}

/// An SVG loaded through `<img>` is parsed as strict XML rather than as HTML,
/// and a `--` inside a comment takes the whole file out. Every one of these
/// files is loaded that way somewhere, and the failure is silent: the browser
/// draws a broken-image glyph and says nothing.
#[test]
fn no_brand_asset_has_a_double_hyphen_in_a_comment() {
    for rel in [
        "site/kite-mark.svg",
        "site/kite-mark-solo.svg",
        "site/favicon.svg",
        "site/social-preview.svg",
        "editors/vscode/icon.svg",
    ] {
        let svg = read(rel);
        let mut rest = svg.as_str();
        while let Some(start) = rest.find("<!--") {
            rest = &rest[start + 4..];
            let end = rest.find("-->").unwrap_or_else(|| panic!("{}: unclosed comment", rel));
            let body = &rest[..end];
            assert!(
                !body.contains("--"),
                "{}: a comment contains `--`, which makes the file invalid XML \
                 and unloadable through <img>",
                rel
            );
            rest = &rest[end + 3..];
        }
    }
}

/// Every page a reader lands on wears the same header, carries the favicon,
/// and reaches the same places. A page that quietly keeps an older one is the
/// failure this catches — it is invisible until someone lands on that page.
///
/// The documents are rendered at build time from `site/template.html`, so the
/// template is checked rather than its output: it is what every generated page
/// wears, it is the file in the repository, and the generated pages are not.
#[test]
fn every_page_wears_the_same_header() {
    let pages = ["index.html", "playground.html", "brand.html", "template.html"];
    for page in pages {
        let html = read(&format!("site/{}", page));
        assert!(
            html.contains("<link rel=\"icon\" href=\"") && html.contains("favicon.svg\""),
            "{} has no favicon",
            page
        );
        for needed in [
            "kite-mark.svg\"",
            "class=\"name\">Kite<",
            "class=\"chip\">draft<",
            "read/06-roadmap.html",
            "class=\"gh\"",
        ] {
            assert!(html.contains(needed), "{} is missing `{}`", page, needed);
        }
    }
}

/// The two old entry points still answer.
///
/// `docs.html?doc=…` and `reference.html?module=…` are in links people already
/// have, and in every deployed copy of these documents. They are redirects now
/// rather than pages, and a redirect that stopped translating the query string
/// would send every one of those links to the front page — which looks like
/// working and is not.
#[test]
fn the_old_document_urls_still_go_somewhere() {
    for (page, fallback) in [
        ("docs.html", "read/specification.html"),
        ("reference.html", "reference/prelude.html"),
    ] {
        let html = read(&format!("site/{}", page));
        assert!(html.contains("location.replace"), "{} does not redirect", page);
        assert!(html.contains(fallback), "{} has no default target", page);
        assert!(
            html.contains("doc") && html.contains("module"),
            "{} drops the query string it exists to translate",
            page
        );
    }
}
