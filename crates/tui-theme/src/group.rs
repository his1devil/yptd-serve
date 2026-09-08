//! The highlight-group vocabulary and its built-in defaults.
//!
//! One macro row per group carries the variant, its config name, its default
//! parent, and its default style. Adding a group is a one-line change and the
//! name in `theme.toml` can never drift from the enum, because it *is* the
//! variant identifier.
//!
//! Group names describe *meaning*, never appearance. Render code asks for
//! `MessageAuthor`, not "bold cyan" -- that indirection is the whole point:
//! one override of `Muted` restyles every secondary surface at once.

use ratatui::style::{Color, Modifier, Style};

fn plain() -> Style {
    Style::new()
}

fn fg(color: Color) -> Style {
    Style::new().fg(color)
}

/// The two modifiers ratatui has no built-in shorthand for. `bold`, `dim`,
/// `italic` and `underlined` come from `Style` itself.
trait Mods: Sized {
    fn crossed(self) -> Self;
    /// Cancels an inherited DIM, so a selected row reads bright even when its
    /// parent group is muted.
    fn no_dim(self) -> Self;
}

impl Mods for Style {
    fn crossed(self) -> Self {
        self.add_modifier(Modifier::CROSSED_OUT)
    }
    fn no_dim(self) -> Self {
        self.remove_modifier(Modifier::DIM)
    }
}

/// Palette constants used by more than one group, so a tweak stays in one place.
const ORANGE: Color = Color::Rgb(0xFF, 0xA5, 0x00);
const SCROLLBAR: Color = Color::Rgb(0xAA, 0xAA, 0xAA);
const MENTION_SELF_BG: Color = Color::Rgb(0x5C, 0x4C, 0x23);
const MENTION_OTHER_FG: Color = Color::Rgb(0xC1, 0xCE, 0xF7);
const MENTION_OTHER_BG: Color = Color::Rgb(0x28, 0x32, 0x5C);
const UNREAD_RULE: Color = Color::Rgb(0xED, 0x42, 0x45);

macro_rules! define_groups {
    ($($variant:ident => ($link:expr, $style:expr),)*) => {
        /// A named, themeable surface. Render code never names a color.
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(usize)]
        pub enum HighlightGroup {
            $($variant,)*
        }

        impl HighlightGroup {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];
            pub const COUNT: usize = Self::ALL.len();

            /// The name used in `theme.toml`. Identical to the variant.
            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($variant),)*
                }
            }

            /// Exact, case-sensitive. A near-miss is a typo, not a synonym.
            pub fn from_name(name: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|group| group.name() == name)
            }

            pub(crate) fn default_link(self) -> Option<Self> {
                match self {
                    $(Self::$variant => $link,)*
                }
            }

            pub(crate) fn default_style(self) -> Style {
                match self {
                    $(Self::$variant => $style,)*
                }
            }
        }
    };
}

use HighlightGroup as G;

define_groups! {
    // ---- semantic parents -------------------------------------------------
    // Override these six and the whole client changes character.
    Normal          => (None,               fg(Color::Reset).bg(Color::Reset)),
    Strong          => (None,               plain().bold()),
    Emphasis        => (None,               plain().italic()),
    Muted           => (None,               plain().dim()),
    Title           => (Some(G::Strong),    plain()),
    Heading         => (Some(G::Strong),    plain()),

    // ---- derived text roles ----------------------------------------------
    Decoration      => (Some(G::Muted),     plain()),
    Hint            => (Some(G::Muted),     plain()),
    Description     => (Some(G::Muted),     plain()),
    Shortcut        => (Some(G::Muted),     plain()),
    FieldLabel      => (Some(G::Muted),     plain()),
    Timestamp       => (Some(G::Muted),     plain()),
    Placeholder     => (Some(G::Muted),     plain()),
    Disabled        => (Some(G::Muted),     plain()),
    Loading         => (Some(G::Muted),     plain()),
    Edited          => (Some(G::Muted),     plain().italic()),
    Revoked         => (Some(G::Muted),     plain().crossed()),

    // ---- structure --------------------------------------------------------
    Border          => (None,               fg(Color::DarkGray)),
    FocusBorder     => (None,               fg(Color::Cyan)),
    Selection       => (None,               fg(Color::Cyan).bold().no_dim()),
    SelectionBorder => (None,               fg(Color::Green).bold()),

    PaneBorder          => (Some(G::Border),         plain()),
    FocusedPaneBorder   => (Some(G::FocusBorder),    plain().bold()),
    PaneTitle           => (Some(G::Title),          plain()),
    ModalBorder         => (Some(G::FocusBorder),    plain().bold()),
    ModalTitle          => (Some(G::Title),          plain()),
    ComposerBorder      => (Some(G::Border),         plain()),
    ActiveComposerBorder=> (Some(G::FocusBorder),    plain().bold()),
    ComposerTitle       => (Some(G::Title),          plain()),
    PickerBorder        => (Some(G::FocusBorder),    plain()),
    SelectedRow         => (Some(G::Selection),      plain()),
    SelectionMarker     => (Some(G::Selection),      plain()),
    ActiveField         => (None,                    fg(Color::Cyan).bold()),
    ActiveTab           => (Some(G::Selection),      plain()),
    ScrollbarThumb      => (None,                    fg(SCROLLBAR)),
    ScrollbarTrack      => (Some(G::ScrollbarThumb), plain().dim()),

    // ---- status line ------------------------------------------------------
    StatusTitle     => (Some(G::Title),     fg(Color::Cyan)),
    StatusLabel     => (Some(G::Muted),     plain()),
    StatusError     => (Some(G::Error),     plain().bold()),
    StatusWarning   => (Some(G::Warning),   plain().bold()),

    // ---- message content --------------------------------------------------
    MessageAuthor       => (Some(G::Strong),         plain()),
    // One colour per person, picked from their id so it never changes on
    // them. Six, not sixteen: a chat window with a dozen hues reads as
    // noise, and these have to stay legible on both light and dark grounds.
    Author1             => (None,                    fg(Color::Cyan).bold()),
    Author2             => (None,                    fg(Color::Green).bold()),
    Author3             => (None,                    fg(Color::Magenta).bold()),
    Author4             => (None,                    fg(Color::Blue).bold()),
    Author5             => (None,                    fg(Color::Yellow).bold()),
    Author6             => (None,                    fg(Color::Red).bold()),
    MessageTimestamp    => (Some(G::Timestamp),      plain()),
    MessageBody         => (None,                    fg(Color::Reset)),
    MessageSecondary    => (Some(G::Muted),          plain()),
    MessageSelectedBorder => (Some(G::SelectionBorder), plain()),
    MessageAttachment   => (None,                    fg(Color::Cyan)),
    MessageLink         => (None,                    fg(Color::Cyan).underlined()),
    InlineCode          => (None,                    fg(ORANGE)),
    CodeBlockBorder     => (Some(G::Border),         plain().dim()),
    // Fenced-code body, distinct from InlineCode: a whole block tinted like an
    // inline snippet reads as one long highlight. Syntect takes this over once
    // it lands; until then it follows the normal message color.
    CodeBlockText       => (Some(G::MessageBody),    plain()),
    MarkdownHeading1    => (Some(G::Heading),        fg(Color::Cyan)),
    MarkdownHeading2    => (Some(G::Heading),        plain().underlined()),
    MarkdownHeading3    => (Some(G::Heading),        plain()),
    MarkdownQuote       => (None,                    fg(Color::DarkGray)),
    MarkdownMarker      => (None,                    fg(Color::DarkGray)),
    QuotePreview        => (Some(G::Muted),          plain()),
    Reaction            => (None,                    fg(Color::Cyan)),
    SelfReaction        => (None,                    fg(Color::Yellow)),
    DateDivider         => (Some(G::Decoration),     plain()),
    UnreadDivider       => (None,                    fg(UNREAD_RULE)),
    UnreadNotice        => (None,                    fg(Color::Cyan).bold()),

    // ---- mentions ---------------------------------------------------------
    MentionSelf     => (None,               fg(Color::Yellow).bg(MENTION_SELF_BG)),
    MentionOther    => (None,               fg(MENTION_OTHER_FG).bg(MENTION_OTHER_BG)),
    MentionAll      => (Some(G::MentionSelf), plain().bold()),
    BotBadge        => (Some(G::Normal),    fg(Color::Magenta).bold()),

    // ---- navigation -------------------------------------------------------
    CategoryHeading     => (Some(G::Heading),   plain()),
    MemberGroupHeading  => (Some(G::Heading),   plain()),
    NavActive           => (None,               fg(Color::Green).bold()),
    NavUnread           => (None,               fg(Color::Reset)),
    NavMentioned        => (None,               fg(ORANGE)),
    NavMuted            => (Some(G::Muted),     plain()),
    UnreadBadge         => (None,               fg(Color::Reset)),
    MentionBadge        => (None,               fg(ORANGE).bold()),

    // ---- presence (OpenIM has two states, not four) -----------------------
    PresenceOnline  => (None,               fg(Color::Green)),
    PresenceOffline => (Some(G::Normal),    plain().dim()),

    // ---- OpenIM / yptd specific -------------------------------------------
    SyncProgress    => (None,               fg(Color::Cyan)),
    ReadReceipt     => (Some(G::Muted),     plain()),
    TypingIndicator => (Some(G::Muted),     plain().italic()),
    ConnectionLost  => (Some(G::Error),     plain().bold()),
    SendPending     => (Some(G::Muted),     plain()),
    SendFailed      => (Some(G::Error),     plain()),
    GhostMessage    => (Some(G::Muted),     plain()),
    AgentStream     => (None,               fg(Color::Cyan)),
    AgentTool       => (Some(G::Muted),     plain().italic()),

    // ---- feedback ---------------------------------------------------------
    Error           => (None,               fg(Color::Red)),
    Warning         => (None,               fg(Color::Yellow)),
    Success         => (None,               fg(Color::Green)),
    Info            => (None,               fg(Color::Cyan)),
    Editing         => (None,               fg(Color::Yellow)),
    Tag             => (None,               fg(Color::Cyan)),
    GaugeFill       => (None,               fg(Color::Cyan)),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for group in HighlightGroup::ALL {
            assert!(seen.insert(group.name()), "duplicate name {}", group.name());
            assert_eq!(HighlightGroup::from_name(group.name()), Some(*group));
        }
        assert_eq!(seen.len(), HighlightGroup::COUNT);
    }

    #[test]
    fn from_name_is_case_sensitive() {
        assert!(HighlightGroup::from_name("messageauthor").is_none());
        assert!(HighlightGroup::from_name("MessageAuthor").is_some());
    }

    #[test]
    fn enum_discriminants_index_the_table() {
        for (index, group) in HighlightGroup::ALL.iter().enumerate() {
            assert_eq!(*group as usize, index, "{} is out of order", group.name());
        }
    }

    #[test]
    fn default_links_never_cycle() {
        for start in HighlightGroup::ALL {
            let mut cursor = *start;
            for _ in 0..HighlightGroup::COUNT {
                match cursor.default_link() {
                    Some(parent) => cursor = parent,
                    None => break,
                }
            }
            assert!(
                cursor.default_link().is_none(),
                "{} sits on a default link cycle",
                start.name()
            );
        }
    }
}
