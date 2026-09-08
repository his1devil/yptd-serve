//! Named highlight groups with link inheritance, for ratatui terminal UIs.
//!
//! Three rules make this worth having over scattered `Color::Cyan` literals:
//!
//! 1. Render code names a *meaning* (`MessageAuthor`), never a color.
//! 2. Groups inherit through `link`, so one override of a parent restyles
//!    every surface that still points at it.
//! 3. Color and geometry are configured separately -- see [`border`].
//!
//! Defaults use the sixteen ANSI names wherever possible so the client adopts
//! whatever palette the user's terminal already has. Literal RGB appears only
//! where a specific pair must hold, such as mention backgrounds.

pub mod border;
pub mod color;
pub mod group;
pub mod options;

use ratatui::style::{Modifier, Style};
use ratatui::widgets::BorderType;

pub use border::BorderSurface;
pub use color::ColorSpec;
pub use group::HighlightGroup;
pub use options::ThemeOptions;

/// Marker drawn before the selected row in lists and pickers.
pub const DEFAULT_SELECTION_MARKER: &str = "▸ ";

/// A group's own contribution, before inheritance is applied.
#[derive(Clone, Copy, Debug, Default)]
struct Definition {
    link: Option<HighlightGroup>,
    style: Style,
    clear_foreground: bool,
    clear_background: bool,
}

/// A resolved theme: every group flattened to a concrete style.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    styles: Vec<Style>,
    borders: Vec<BorderType>,
    selection_marker: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self::from_options(&ThemeOptions::default(), &mut Vec::new())
    }
}

impl Theme {
    /// Resolves `options` against the built-in defaults.
    ///
    /// Never fails. Every invalid leaf -- an unknown group, a bad color, a
    /// link cycle -- is pushed onto `warnings` and skipped, leaving the
    /// built-in value in place. Valid siblings still apply.
    pub fn from_options(options: &ThemeOptions, warnings: &mut Vec<String>) -> Self {
        let mut definitions = built_in_definitions();
        apply_overrides(&mut definitions, options, warnings);

        Self {
            styles: resolve(&definitions, warnings),
            borders: resolve_borders(options, warnings),
            selection_marker: resolve_marker(options, warnings),
        }
    }

    pub fn style(&self, group: HighlightGroup) -> Style {
        self.styles[group as usize]
    }

    /// Layers `group` over `base`, so a caller can start from a row style and
    /// apply a span style on top.
    pub fn patch(&self, group: HighlightGroup, base: Style) -> Style {
        base.patch(self.style(group))
    }

    pub fn border_type(&self, surface: BorderSurface) -> BorderType {
        self.borders[surface as usize]
    }

    pub fn selection_marker(&self) -> &str {
        &self.selection_marker
    }
}

fn built_in_definitions() -> Vec<Definition> {
    HighlightGroup::ALL
        .iter()
        .map(|group| Definition {
            link: group.default_link(),
            style: group.default_style(),
            clear_foreground: false,
            clear_background: false,
        })
        .collect()
}

fn apply_overrides(
    definitions: &mut [Definition],
    options: &ThemeOptions,
    warnings: &mut Vec<String>,
) {
    for key in options.unknown.keys() {
        warnings.push(format!("theme: unknown top-level table `{key}` was ignored"));
    }

    for (name, override_options) in &options.highlight {
        let Some(group) = HighlightGroup::from_name(name) else {
            warnings.push(format!("theme: unknown highlight group `{name}` was ignored"));
            continue;
        };
        let slot = &mut definitions[group as usize];

        for field in override_options.unknown.keys() {
            warnings.push(format!(
                "theme: unknown field `{field}` on highlight group `{name}` was ignored"
            ));
        }

        if let Some(link) = &override_options.link {
            if link == "none" {
                slot.link = None;
            } else if let Some(parent) = HighlightGroup::from_name(link) {
                slot.link = Some(parent);
            } else {
                warnings.push(format!(
                    "theme: `{name}.link` names unknown group `{link}`; the built-in link was kept"
                ));
            }
        }

        apply_channel(
            override_options.foreground.as_deref(),
            name,
            "foreground",
            warnings,
            |spec| match spec {
                ColorSpec::Clear => slot.clear_foreground = true,
                other => {
                    slot.clear_foreground = false;
                    slot.style.fg = other.color();
                }
            },
        );
        apply_channel(
            override_options.background.as_deref(),
            name,
            "background",
            warnings,
            |spec| match spec {
                ColorSpec::Clear => slot.clear_background = true,
                other => {
                    slot.clear_background = false;
                    slot.style.bg = other.color();
                }
            },
        );

        // An explicit `false` must *remove* an inherited modifier, so it goes
        // into `sub_modifier` rather than merely being left unset.
        for (value, modifier) in [
            (override_options.bold, Modifier::BOLD),
            (override_options.italic, Modifier::ITALIC),
            (override_options.dim, Modifier::DIM),
            (override_options.underline, Modifier::UNDERLINED),
            (override_options.strikethrough, Modifier::CROSSED_OUT),
        ] {
            match value {
                Some(true) => {
                    slot.style = slot.style.add_modifier(modifier);
                }
                Some(false) => {
                    slot.style = slot.style.remove_modifier(modifier);
                }
                None => {}
            }
        }
    }
}

fn apply_channel(
    raw: Option<&str>,
    group: &str,
    field: &str,
    warnings: &mut Vec<String>,
    mut set: impl FnMut(ColorSpec),
) {
    let Some(raw) = raw else { return };
    match color::parse(raw) {
        Ok(spec) => set(spec),
        Err(error) => warnings.push(format!("theme: `{group}.{field}`: {error}")),
    }
}

/// Flattens the link graph. Memoized, with cycle detection: a group caught in
/// a cycle resolves as if it were unlinked rather than hanging or panicking.
fn resolve(definitions: &[Definition], warnings: &mut Vec<String>) -> Vec<Style> {
    #[derive(Clone, Copy, PartialEq)]
    enum Slot {
        Pending,
        InProgress,
        Done,
    }

    let mut state = vec![Slot::Pending; definitions.len()];
    let mut styles = vec![Style::default(); definitions.len()];

    for index in 0..definitions.len() {
        resolve_one(index, definitions, &mut state, &mut styles, warnings);
    }

    fn resolve_one(
        index: usize,
        definitions: &[Definition],
        state: &mut [Slot],
        styles: &mut [Style],
        warnings: &mut Vec<String>,
    ) -> Style {
        match state[index] {
            Slot::Done => return styles[index],
            Slot::InProgress => {
                warnings.push(format!(
                    "theme: link cycle through `{}`; it was resolved without its link",
                    HighlightGroup::ALL[index].name()
                ));
                return Style::default();
            }
            Slot::Pending => {}
        }

        state[index] = Slot::InProgress;
        let definition = definitions[index];
        let inherited = match definition.link {
            Some(parent) => resolve_one(parent as usize, definitions, state, styles, warnings),
            None => Style::default(),
        };

        let mut style = inherited.patch(definition.style);
        if definition.clear_foreground {
            style.fg = None;
        }
        if definition.clear_background {
            style.bg = None;
        }

        styles[index] = style;
        state[index] = Slot::Done;
        style
    }

    styles
}

fn resolve_borders(options: &ThemeOptions, warnings: &mut Vec<String>) -> Vec<BorderType> {
    let mut configured: Vec<Option<BorderType>> = vec![None; BorderSurface::COUNT];

    for (name, shape) in &options.ui.border {
        let Some(surface) = BorderSurface::from_name(name) else {
            warnings.push(format!("theme: unknown border surface `{name}` was ignored"));
            continue;
        };
        match border::parse_shape(shape) {
            Some(border_type) => configured[surface as usize] = Some(border_type),
            None => warnings.push(format!(
                "theme: `ui.border.{name}`: `{shape}` is not a border shape (one of {})",
                border::SHAPE_NAMES.join(", ")
            )),
        }
    }

    // A configured `default` replaces every *omitted* surface, including the
    // ones whose built-in differs. An explicit surface still wins over it.
    let fallback = configured[BorderSurface::Default as usize];
    BorderSurface::ALL
        .iter()
        .map(|surface| {
            configured[*surface as usize]
                .or(fallback)
                .unwrap_or_else(|| surface.built_in())
        })
        .collect()
}

fn resolve_marker(options: &ThemeOptions, warnings: &mut Vec<String>) -> String {
    let Some(marker) = options.ui.indicator.selection.as_deref() else {
        return DEFAULT_SELECTION_MARKER.to_owned();
    };
    // Zero-width or multi-line markers would break the column alignment every
    // list pane relies on.
    if marker.is_empty() || marker.contains('\n') {
        warnings.push(format!(
            "theme: `ui.indicator.selection` must be one non-empty line; kept `{DEFAULT_SELECTION_MARKER}`"
        ));
        return DEFAULT_SELECTION_MARKER.to_owned();
    }
    marker.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn theme(source: &str) -> (Theme, Vec<String>) {
        let options = ThemeOptions::from_toml(source).expect("valid toml");
        let mut warnings = Vec::new();
        let theme = Theme::from_options(&options, &mut warnings);
        (theme, warnings)
    }

    #[test]
    fn defaults_resolve_without_warnings() {
        let (theme, warnings) = theme("");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(theme.style(HighlightGroup::Border).fg, Some(Color::DarkGray));
        assert_eq!(theme.style(HighlightGroup::FocusBorder).fg, Some(Color::Cyan));
        assert_eq!(theme.selection_marker(), DEFAULT_SELECTION_MARKER);
    }

    #[test]
    fn children_inherit_through_link() {
        let (theme, _) = theme("");
        // FocusedPaneBorder links to FocusBorder and adds bold.
        let style = theme.style(HighlightGroup::FocusedPaneBorder);
        assert_eq!(style.fg, Some(Color::Cyan));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn overriding_a_parent_moves_every_linked_child() {
        let (theme, warnings) = theme(
            r#"
            [highlight.Muted]
            foreground = "magenta"
            "#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        for child in [
            HighlightGroup::Hint,
            HighlightGroup::Timestamp,
            HighlightGroup::MessageSecondary,
            HighlightGroup::ReadReceipt,
        ] {
            assert_eq!(
                theme.style(child).fg,
                Some(Color::Magenta),
                "{} did not follow Muted",
                child.name()
            );
        }
    }

    #[test]
    fn a_direct_field_wins_over_the_linked_parent() {
        let (theme, _) = theme(
            r#"
            [highlight.Muted]
            foreground = "magenta"

            [highlight.Hint]
            foreground = "green"
            "#,
        );
        assert_eq!(theme.style(HighlightGroup::Hint).fg, Some(Color::Green));
        assert_eq!(theme.style(HighlightGroup::Timestamp).fg, Some(Color::Magenta));
    }

    #[test]
    fn link_none_detaches_a_child_from_its_built_in_parent() {
        let (theme, _) = theme(
            r#"
            [highlight.Muted]
            foreground = "magenta"

            [highlight.Hint]
            link = "none"
            "#,
        );
        assert_eq!(theme.style(HighlightGroup::Hint).fg, None);
        assert!(!theme.style(HighlightGroup::Hint).add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn foreground_none_clears_the_channel_after_inheritance() {
        let (theme, _) = theme(
            r#"
            [highlight.Muted]
            foreground = "magenta"

            [highlight.Hint]
            foreground = "none"
            "#,
        );
        assert_eq!(theme.style(HighlightGroup::Hint).fg, None);
        // The inherited DIM modifier survives; only the color channel cleared.
        assert!(theme.style(HighlightGroup::Hint).add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn bold_false_removes_an_inherited_modifier() {
        let (theme, _) = theme(
            r#"
            [highlight.PaneTitle]
            bold = false
            "#,
        );
        let style = theme.style(HighlightGroup::PaneTitle);
        assert!(!style.add_modifier.contains(Modifier::BOLD));
        assert!(style.sub_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn a_link_cycle_warns_and_still_produces_a_theme() {
        let (theme, warnings) = theme(
            r#"
            [highlight.Hint]
            link = "Description"

            [highlight.Description]
            link = "Hint"
            "#,
        );
        assert!(
            warnings.iter().any(|w| w.contains("link cycle")),
            "{warnings:?}"
        );
        // Still usable: every group resolved to something.
        let _ = theme.style(HighlightGroup::Hint);
        let _ = theme.style(HighlightGroup::Description);
    }

    #[test]
    fn one_bad_leaf_does_not_discard_its_valid_siblings() {
        let (theme, warnings) = theme(
            r#"
            [highlight.MessageAuthor]
            foreground = "chartreuse"
            bold = true

            [highlight.Nonexistent]
            foreground = "cyan"
            "#,
        );
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        let style = theme.style(HighlightGroup::MessageAuthor);
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(style.fg, None, "the invalid color must not have applied");
    }

    #[test]
    fn border_default_fills_omitted_surfaces_and_explicit_wins() {
        let (theme, warnings) = theme(
            r#"
            [ui.border]
            default = "double"
            composer = "thick"
            "#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(theme.border_type(BorderSurface::Pane), BorderType::Double);
        assert_eq!(theme.border_type(BorderSurface::Message), BorderType::Double);
        assert_eq!(theme.border_type(BorderSurface::Composer), BorderType::Thick);
    }

    #[test]
    fn built_in_borders_survive_when_default_is_absent() {
        let (theme, _) = theme(
            r#"
            [ui.border]
            pane = "double"
            "#,
        );
        assert_eq!(theme.border_type(BorderSurface::Pane), BorderType::Double);
        assert_eq!(theme.border_type(BorderSurface::Composer), BorderType::Rounded);
        assert_eq!(theme.border_type(BorderSurface::Modal), BorderType::Plain);
    }

    #[test]
    fn an_unusable_selection_marker_falls_back_with_a_warning() {
        for marker in ["\"\"", "\"a\\nb\""] {
            let (theme, warnings) = theme(&format!("[ui.indicator]\nselection = {marker}"));
            assert_eq!(theme.selection_marker(), DEFAULT_SELECTION_MARKER);
            assert!(!warnings.is_empty(), "{marker} should warn");
        }
    }

    #[test]
    fn every_group_resolves_to_a_concrete_style() {
        let (theme, _) = theme("");
        assert_eq!(theme.styles.len(), HighlightGroup::COUNT);
        assert_eq!(theme.borders.len(), BorderSurface::COUNT);
    }
}
