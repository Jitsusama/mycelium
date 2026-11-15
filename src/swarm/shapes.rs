//! The marks a terminal draws each orchestration's shape by, the same the
//! launcher and the map draw it by, so a person knows one orchestration by
//! one shape wherever they see it.
//!
//! The marks are the account's theme's, read from its `theme.json` at each
//! run, so a theme switch reaches the command without a rebuild: a shape
//! named `down-triangle` is drawn by the theme's `shapeDownTriangle`. On
//! the console, whose font draws none of them, it takes the theme's
//! stand-in. A mark the theme turns, as it draws the down triangle by the
//! triangle turned a half turn, can't be turned in a terminal, so it's
//! drawn by a down triangle the terminal's font has, never by the bare
//! mark, which reads as the triangle.
//!
//! A shape the theme has no mark for, or a machine with no theme, draws
//! nothing, rather than a mark of mycelium's own that the other surfaces
//! don't draw.

use std::collections::BTreeMap;
use std::fs;

use serde_json::Value;

use crate::place;

/// The prefix the theme names each shape's mark under.
const MARK: &str = "shape";

/// What a terminal draws for a mark the theme turns, which it can't turn.
const TURNED: &str = "\u{25bc}";

/// The theme's mark for each shape, by the name the node gives it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shapes {
    marks: BTreeMap<String, String>,
}

impl Shapes {
    /// The marks of the account's theme, its stand-ins on the console
    /// (`TERM=linux`); none where there's no theme to read.
    #[must_use]
    pub fn read() -> Self {
        let console = std::env::var("TERM").is_ok_and(|term| term == "linux");
        fs::read_to_string(place::theme())
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .map(|theme| Self::of(&theme, console))
            .unwrap_or_default()
    }

    /// The shapes' marks in `theme`, a theme.json, or their stand-ins
    /// where `console`.
    #[must_use]
    pub fn of(theme: &Value, console: bool) -> Self {
        let drawn = if console { "console" } else { "glyph" };
        let marks = theme["marks"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(name, mark)| {
                let shape = shape(name.strip_prefix(MARK)?)?;
                let turned = !console && mark["turned"].as_bool().unwrap_or_default();
                let mark = mark[drawn].as_str().filter(|mark| !mark.is_empty())?;
                Some((shape, if turned { TURNED } else { mark }.to_owned()))
            })
            .collect();
        Self { marks }
    }

    /// The mark `shape`, such as `hexagon`, is drawn by, where the theme
    /// has one.
    #[must_use]
    pub fn mark(&self, shape: &str) -> Option<&str> {
        self.marks.get(shape).map(String::as_str)
    }
}

/// The shape a mark's name after its prefix names, as the node spells
/// it: `DownTriangle` is `down-triangle`. None where it isn't one.
fn shape(named: &str) -> Option<String> {
    let mut words = String::new();
    for letter in named.chars() {
        if letter.is_ascii_uppercase() {
            if !words.is_empty() {
                words.push('-');
            }
            words.push(letter.to_ascii_lowercase());
        } else if letter.is_ascii_lowercase() {
            words.push(letter);
        } else {
            return None;
        }
    }
    named
        .starts_with(|first: char| first.is_ascii_uppercase())
        .then_some(words)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::Shapes;

    /// A theme with marks for a hexagon and a square, and one that isn't
    /// a shape's.
    fn theme() -> serde_json::Value {
        json!({"marks": {
            "shapeHexagon": {"glyph": "\u{f02d8}", "console": "H"},
            "shapeSquare": {"glyph": "\u{f0764}", "console": "#"},
            "separator": {"glyph": "·", "console": "."},
        }})
    }

    #[test]
    fn a_shape_takes_the_themes_mark_by_its_name() {
        let shapes = Shapes::of(&theme(), false);

        assert_eq!(
            (shapes.mark("hexagon"), shapes.mark("square")),
            (Some("\u{f02d8}"), Some("\u{f0764}")),
            "each shape takes the mark the theme names after it"
        );
    }

    #[test]
    fn a_mark_the_theme_turns_is_drawn_by_a_down_triangle_a_terminal_has() {
        let theme = json!({"marks": {"shapeDownTriangle": {
            "glyph": "\u{f0536}\u{fe0e}", "turned": true, "console": "v",
        }}});

        assert_eq!(
            (
                Shapes::of(&theme, false).mark("down-triangle"),
                Shapes::of(&theme, true).mark("down-triangle"),
            ),
            (Some("\u{25bc}"), Some("v")),
            "never the bare triangle, which reads as the one pointing up"
        );
    }

    #[test]
    fn the_console_takes_the_themes_stand_ins() {
        let shapes = Shapes::of(&theme(), true);

        assert_eq!(
            shapes.mark("hexagon"),
            Some("H"),
            "the console's font draws only the stand-in"
        );
    }

    #[test]
    fn a_shape_the_theme_has_no_mark_for_draws_nothing() {
        let shapes = Shapes::of(&theme(), false);

        assert_eq!(
            (shapes.mark("star"), shapes.mark("separator")),
            (None, None),
            "no mark is made up, and a mark not a shape's is no shape"
        );
    }

    #[test]
    fn no_theme_draws_no_shapes() {
        assert_eq!(
            Shapes::of(&json!({}), false),
            Shapes::default(),
            "a machine with no theme draws no marks"
        );
    }
}
