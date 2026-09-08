//! The shape of `theme.toml`.
//!
//! Unknown keys are captured rather than rejected. A theme file is edited by
//! hand and read at startup: one stale group name from an old release should
//! produce a warning next to the rest of a working theme, not a client that
//! refuses to launch.

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ThemeOptions {
    #[serde(default)]
    pub highlight: BTreeMap<String, HighlightOptions>,
    #[serde(default)]
    pub ui: UiOptions,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct HighlightOptions {
    pub link: Option<String>,
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub dim: Option<bool>,
    pub underline: Option<bool>,
    pub strikethrough: Option<bool>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct UiOptions {
    #[serde(default)]
    pub border: BTreeMap<String, String>,
    #[serde(default)]
    pub indicator: IndicatorOptions,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct IndicatorOptions {
    pub selection: Option<String>,
}

impl ThemeOptions {
    /// Parses `theme.toml`. A syntax error is fatal for the file as a whole --
    /// there is nothing partial to salvage from unparseable TOML -- but every
    /// semantic problem inside a valid file is reported as a warning instead.
    pub fn from_toml(source: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_representative_file() {
        let options = ThemeOptions::from_toml(
            r#"
            [highlight.Normal]
            foreground = "terminal_default"

            [highlight.MessageAuthor]
            link = "Strong"
            foreground = "cyan"
            bold = true

            [ui.border]
            default = "plain"
            composer = "rounded"

            [ui.indicator]
            selection = "❯ "
            "#,
        )
        .expect("valid toml");

        assert_eq!(options.highlight.len(), 2);
        let author = &options.highlight["MessageAuthor"];
        assert_eq!(author.link.as_deref(), Some("Strong"));
        assert_eq!(author.foreground.as_deref(), Some("cyan"));
        assert_eq!(author.bold, Some(true));
        assert_eq!(options.ui.border["composer"], "rounded");
        assert_eq!(options.ui.indicator.selection.as_deref(), Some("❯ "));
    }

    #[test]
    fn unknown_keys_are_captured_not_rejected() {
        let options = ThemeOptions::from_toml(
            r#"
            [highlight.MessageAuthor]
            foreground = "cyan"
            sparkle = true

            [nonsense]
            whatever = 1
            "#,
        )
        .expect("unknown keys must not fail the parse");

        assert!(options.highlight["MessageAuthor"].unknown.contains_key("sparkle"));
        assert!(options.unknown.contains_key("nonsense"));
    }

    #[test]
    fn an_empty_file_is_valid() {
        let options = ThemeOptions::from_toml("").expect("empty is valid");
        assert!(options.highlight.is_empty());
        assert!(options.ui.border.is_empty());
    }
}
