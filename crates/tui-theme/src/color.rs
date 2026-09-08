//! Color values as they appear in `theme.toml`.
//!
//! Four states, not two. `none` and `terminal_default` are different things:
//! `none` leaves whatever is underneath the cell alone, `terminal_default`
//! explicitly paints the terminal's own default over it. Themes need both --
//! a selected row that only sets a foreground must not erase the background
//! its parent group established.

use ratatui::style::Color;

/// One color channel as written in config.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorSpec {
    /// `none` -- clear this channel after inheritance so nothing is painted.
    Clear,
    /// `terminal_default` -- paint the terminal's default color.
    TerminalDefault,
    /// A named ANSI color or a `#RRGGBB` literal.
    Fixed(Color),
}

impl ColorSpec {
    /// Resolves to the ratatui color to paint, or `None` when the channel is
    /// cleared.
    pub fn color(self) -> Option<Color> {
        match self {
            Self::Clear => None,
            Self::TerminalDefault => Some(Color::Reset),
            Self::Fixed(color) => Some(color),
        }
    }
}

/// Parses a config color value.
///
/// Accepts `none`, `terminal_default`, the sixteen canonical ANSI names, and
/// six-digit hex with an optional leading `#`. Aliases such as `reset`,
/// `darkgray`, or `bright_red` are deliberately rejected: silently accepting
/// near-misses makes a typo look like it worked.
pub fn parse(value: &str) -> Result<ColorSpec, ColorError> {
    let trimmed = value.trim();
    match trimmed {
        "none" => return Ok(ColorSpec::Clear),
        "terminal_default" => return Ok(ColorSpec::TerminalDefault),
        _ => {}
    }

    if let Some(color) = ansi_color(trimmed) {
        return Ok(ColorSpec::Fixed(color));
    }

    let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        let channel = |range: std::ops::Range<usize>| {
            u8::from_str_radix(&hex[range], 16).expect("validated as hex above")
        };
        return Ok(ColorSpec::Fixed(Color::Rgb(
            channel(0..2),
            channel(2..4),
            channel(4..6),
        )));
    }

    Err(ColorError {
        value: trimmed.to_owned(),
    })
}

fn ansi_color(name: &str) -> Option<Color> {
    Some(match name {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" => Color::Gray,
        "dark_gray" => Color::DarkGray,
        "light_red" => Color::LightRed,
        "light_green" => Color::LightGreen,
        "light_yellow" => Color::LightYellow,
        "light_blue" => Color::LightBlue,
        "light_magenta" => Color::LightMagenta,
        "light_cyan" => Color::LightCyan,
        "white" => Color::White,
        _ => return None,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColorError {
    pub value: String,
}

impl std::fmt::Display for ColorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}` is not a color: use none, terminal_default, an ANSI name, or #RRGGBB",
            self.value
        )
    }
}

impl std::error::Error for ColorError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_four_value_shapes() {
        assert_eq!(parse("none"), Ok(ColorSpec::Clear));
        assert_eq!(parse("terminal_default"), Ok(ColorSpec::TerminalDefault));
        assert_eq!(parse("cyan"), Ok(ColorSpec::Fixed(Color::Cyan)));
        assert_eq!(
            parse("#FFA500"),
            Ok(ColorSpec::Fixed(Color::Rgb(255, 165, 0)))
        );
        assert_eq!(parse("ffa500"), Ok(ColorSpec::Fixed(Color::Rgb(255, 165, 0))));
    }

    #[test]
    fn clear_and_terminal_default_resolve_differently() {
        assert_eq!(ColorSpec::Clear.color(), None);
        assert_eq!(ColorSpec::TerminalDefault.color(), Some(Color::Reset));
    }

    #[test]
    fn rejects_near_miss_aliases() {
        for alias in ["reset", "darkgray", "bright_red", "grey", "#FFF", "nope"] {
            assert!(parse(alias).is_err(), "{alias} should not parse");
        }
    }

    #[test]
    fn ignores_surrounding_whitespace() {
        assert_eq!(parse("  cyan  "), Ok(ColorSpec::Fixed(Color::Cyan)));
    }
}
