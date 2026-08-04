//! The accessibility audit behind `kite check --a11y`.
//!
//! §7 of the standard library design says a Lighthouse score is not evidence:
//! a canvas application can score perfectly and be unusable, because the audit
//! is looking at a page that contains one element. So the audit here does not
//! look at a page. It looks at what the program *drew*.
//!
//! Running the program under the bytecode VM produces a transcript — one line
//! per drawing call, including the `semantics` declarations `ui.paint` emits
//! for every control. That transcript is the same picture the DOM and canvas
//! renderers build, expressed as text, which makes it the one place all three
//! backends can be checked at once and the only one that needs no browser.
//!
//! What it can see, it checks properly. What it cannot see, it does not guess
//! at — and the limits are stated in [`Finding`] rather than left for someone
//! to discover from a wrong answer.

use std::fmt;

/// Roles that a person is expected to be able to operate.
///
/// The numbers are `ui.role_code`. Written out rather than derived, because
/// this is a wire format and the enum's declaration order is not.
const INTERACTIVE: &[i64] = &[
    1,  // Button
    2,  // Link
    3,  // Checkbox
    4,  // Radio
    5,  // Switch
    6,  // Slider
    7,  // TextField
    10, // Tab
    11, // MenuItem
];

fn role_name(code: i64) -> &'static str {
    match code {
        1 => "button",
        2 => "link",
        3 => "checkbox",
        4 => "radio",
        5 => "switch",
        6 => "slider",
        7 => "text field",
        8 => "image",
        9 => "heading",
        10 => "tab",
        11 => "menu item",
        12 => "dialog",
        13 => "progress bar",
        _ => "group",
    }
}

/// The smallest a control may be and still be reliably hittable.
///
/// 48dp, which is what Material, the WCAG 2.2 target-size rule and Apple's
/// own guidance all land within a point or two of. A control smaller than this
/// is not a style choice; it is a control some people cannot press.
const MINIMUM_TARGET: f64 = 48.0;

/// The contrast a body-sized run of text needs against what is behind it.
///
/// WCAG AA. Large text is allowed 3.0 instead, and this audit does **not**
/// grant that exemption — see [`Finding::Contrast`].
const MINIMUM_CONTRAST: f64 = 4.5;

#[derive(Debug, Clone)]
pub enum Finding {
    /// A control that announces as its role and nothing else — "button", with
    /// no idea which. The defect §7 is built to prevent.
    NoLabel { id: String, role: i64 },
    /// A control smaller than a fingertip.
    SmallTarget { id: String, role: i64, width: f64, height: f64 },
    /// Text too close in colour to what is behind it.
    ///
    /// The background is the last painted rectangle containing the run's
    /// origin, which is what the painter's algorithm makes it. Two things this
    /// cannot know: the *size* of the run, because selecting a font paints
    /// nothing and so leaves no trace in a transcript — so the 3.0 allowance
    /// for large text is never applied and a large heading may be reported
    /// that a person would pass; and any translucency, because `alpha` is a
    /// separate call and the blend is the renderer's.
    Contrast { body: String, ink: i64, behind: i64, ratio: f64 },
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Finding::NoLabel { id, role } => write!(
                f,
                "`{}` is a {} with no label\n  a reader announces it as \"{}\" and nothing else",
                id,
                role_name(*role),
                role_name(*role)
            ),
            Finding::SmallTarget { id, role, width, height } => write!(
                f,
                "`{}` is a {} of {}×{}dp\n  smaller than the {}dp minimum for something \
                 meant to be pressed",
                id,
                role_name(*role),
                trim(*width),
                trim(*height),
                trim(MINIMUM_TARGET)
            ),
            Finding::Contrast { body, ink, behind, ratio } => write!(
                f,
                "\"{}\" is #{:06x} on #{:06x}, a contrast of {:.2}\n  below the {} \
                 needed for body text",
                body, ink, behind, ratio, MINIMUM_CONTRAST
            ),
        }
    }
}

/// Trim a float the way the transcript prints one.
fn trim(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{}", v)
    }
}

/// One rectangle that has been painted, and its colour.
struct Painted {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    colour: i64,
}

/// Audit a transcript.
///
/// Findings come out in the order the calls were made, which is the order the
/// controls were laid out — so reading them top to bottom walks the picture.
pub fn audit(transcript: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut painted: Vec<Painted> = Vec::new();
    // A control may be declared on every frame, and a program that draws ten
    // frames should not report the same missing label ten times. Text is
    // deduplicated by what it says *and* the two colours, so the same run
    // drawn twice is one finding while the same words in two different places
    // are two.
    let mut seen: Vec<String> = Vec::new();
    let mut said: Vec<(String, i64, i64)> = Vec::new();

    for line in transcript.lines() {
        let mut parts = line.split(' ');
        let Some(kind) = parts.next() else { continue };
        match kind {
            "rect" | "rrect" => {
                // `rect x y w h colour`, `rrect x y w h radius colour` — the
                // colour is last either way, so it is taken from the end.
                let fields: Vec<&str> = parts.collect();
                if fields.len() < 5 {
                    continue;
                }
                let nums: Option<Vec<f64>> =
                    fields[..4].iter().map(|f| f.parse().ok()).collect();
                let Some(nums) = nums else { continue };
                let Ok(colour) = fields[fields.len() - 1].parse::<i64>() else { continue };
                painted.push(Painted { x: nums[0], y: nums[1], w: nums[2], h: nums[3], colour });
            }
            "semantics" => {
                // `semantics x y w h role flags id label…` — the label is last
                // because it is the only field that may contain a space.
                let fields: Vec<&str> = parts.collect();
                if fields.len() < 7 {
                    continue;
                }
                let (Ok(w), Ok(h)) = (fields[2].parse::<f64>(), fields[3].parse::<f64>()) else {
                    continue;
                };
                let (Ok(role), Ok(flags)) =
                    (fields[4].parse::<i64>(), fields[5].parse::<i64>())
                else {
                    continue;
                };
                let id = fields[6].to_string();
                let label = fields[7..].join(" ");

                if seen.contains(&id) {
                    continue;
                }
                seen.push(id.clone());

                // A disabled control is not in anyone's way: it cannot be
                // reached, pressed or focused, so neither rule applies to it.
                let disabled = flags & 1 != 0;
                if disabled {
                    continue;
                }
                if label.trim().is_empty() && INTERACTIVE.contains(&role) {
                    out.push(Finding::NoLabel { id: id.clone(), role });
                }
                if INTERACTIVE.contains(&role) && (w < MINIMUM_TARGET || h < MINIMUM_TARGET) {
                    out.push(Finding::SmallTarget { id, role, width: w, height: h });
                }
            }
            "text" => {
                // `text x y body… colour` — the body may contain spaces, so
                // the colour is taken from the end and the middle is the body.
                let fields: Vec<&str> = parts.collect();
                if fields.len() < 4 {
                    continue;
                }
                let (Ok(x), Ok(y)) = (fields[0].parse::<f64>(), fields[1].parse::<f64>()) else {
                    continue;
                };
                let Ok(ink) = fields[fields.len() - 1].parse::<i64>() else { continue };
                let body = fields[2..fields.len() - 1].join(" ");
                if body.trim().is_empty() {
                    continue;
                }
                // The painter's algorithm: what is behind this run is the last
                // rectangle drawn that contains where it starts.
                let Some(behind) = painted
                    .iter()
                    .rev()
                    .find(|p| x >= p.x && y >= p.y && x <= p.x + p.w && y <= p.y + p.h)
                else {
                    continue;
                };
                let ratio = contrast(ink, behind.colour);
                if ratio < MINIMUM_CONTRAST {
                    let key = (body.clone(), ink, behind.colour);
                    if said.contains(&key) {
                        continue;
                    }
                    said.push(key);
                    out.push(Finding::Contrast {
                        body,
                        ink,
                        behind: behind.colour,
                        ratio,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// WCAG relative luminance of an 0xRRGGBB colour.
fn luminance(colour: i64) -> f64 {
    let channel = |shift: u32| {
        let v = ((colour >> shift) & 0xff) as f64 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// The WCAG contrast ratio between two colours, from 1.0 to 21.0.
pub fn contrast(a: i64, b: i64) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (lighter, darker) = if la > lb { (la, lb) } else { (lb, la) };
    (lighter + 0.05) / (darker + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_on_white_is_the_maximum() {
        assert!((contrast(0x000000, 0xffffff) - 21.0).abs() < 0.01);
    }

    #[test]
    fn a_colour_against_itself_is_one() {
        assert!((contrast(0x336699, 0x336699) - 1.0).abs() < 0.001);
    }

    #[test]
    fn an_unlabelled_button_is_found() {
        let found = audit("semantics 0.0 0.0 48.0 48.0 1 0 play ");
        assert_eq!(found.len(), 1);
        assert!(matches!(found[0], Finding::NoLabel { .. }));
    }

    #[test]
    fn a_labelled_button_is_not() {
        let found = audit("semantics 0.0 0.0 48.0 48.0 1 0 play Play the track");
        assert!(found.is_empty(), "{:?}", found);
    }

    #[test]
    fn a_label_may_contain_spaces() {
        let found = audit("semantics 0.0 0.0 48.0 48.0 1 0 play Play");
        assert!(found.is_empty(), "{:?}", found);
    }

    #[test]
    fn a_small_target_is_found() {
        let found = audit("semantics 0.0 0.0 24.0 24.0 1 0 x Close");
        assert_eq!(found.len(), 1);
        assert!(matches!(found[0], Finding::SmallTarget { .. }));
    }

    #[test]
    fn a_group_is_neither_pressed_nor_sized() {
        let found = audit("semantics 0.0 0.0 2.0 2.0 0 0 row ");
        assert!(found.is_empty(), "{:?}", found);
    }

    #[test]
    fn a_disabled_control_is_exempt() {
        let found = audit("semantics 0.0 0.0 10.0 10.0 1 1 off ");
        assert!(found.is_empty(), "{:?}", found);
    }

    #[test]
    fn the_same_control_is_reported_once_across_frames() {
        let found = audit(
            "semantics 0.0 0.0 24.0 24.0 1 0 x Close\nsemantics 0.0 0.0 24.0 24.0 1 0 x Close",
        );
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn grey_on_white_is_found() {
        let found = audit("rect 0.0 0.0 100.0 100.0 16777215\ntext 4.0 4.0 hello there 12632256");
        assert_eq!(found.len(), 1, "{:?}", found);
        assert!(matches!(found[0], Finding::Contrast { .. }));
    }

    #[test]
    fn black_on_white_is_not() {
        let found = audit("rect 0.0 0.0 100.0 100.0 16777215\ntext 4.0 4.0 hello 0");
        assert!(found.is_empty(), "{:?}", found);
    }

    #[test]
    fn text_over_nothing_is_not_guessed_at() {
        let found = audit("text 4.0 4.0 hello 12632256");
        assert!(found.is_empty(), "{:?}", found);
    }
}
