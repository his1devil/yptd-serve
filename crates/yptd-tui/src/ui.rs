//! Rendering. Every color comes from a named highlight group -- there is not a
//! single `Color::` literal below this line.

use im_model::{Body, ConversationId, Message, SendState, Visibility};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as TuiLine, Span as TuiSpan};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use tui_richtext::{
    Line as RichLine, LineStyle, PrefixKind, RichText, Span, SpanKind, layout_blocks,
    parse_markdown, wrap,
};
use tui_theme::{BorderSurface, HighlightGroup as HG, Theme};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, NavRow, Pane};
use crate::browser::{Browser, Kind as BrowseKind};
use crate::picker::Picker;
use crate::layout;
use crate::media::Media;
use crate::syntax;

/// Columns reserved on the left of every message for the selection marker.
/// Always reserved, never conditional: if selecting a message changed the
/// content width, every cursor move would re-wrap the text and the viewport
/// would jump under the reader.
const GUTTER: u16 = 2;
const SELECTED_MARK: &str = "▌ ";
/// A thinner bar for a message that names you, so a mention is findable while
/// scrolling past without reading a word.
const MENTION_MARK: &str = "▏ ";
const UNSELECTED_MARK: &str = "  ";

/// Consecutive messages from one sender inside this window share a header.
const AUTHOR_GROUP_MS: i64 = 5 * 60_000;

/// Laid-out message rows, kept between frames.
///
/// Building them means parsing markdown, running syntax highlighting and
/// wrapping every line to the pane width. None of that changes when the
/// cursor moves or the viewport scrolls, which is most of what a person does,
/// so doing it per frame is the difference between scrolling that glides and
/// scrolling that stutters.
#[derive(Default)]
pub struct Cache {
    key: Option<(ConversationId, usize, u64, u64, i64, i64)>,
    lines: Vec<PaneLine>,
}

impl Cache {
    /// Returns the rows for this state, laying them out only when something
    /// they depend on has changed.
    fn rows(
        &mut self,
        app: &App,
        theme: &Theme,
        media: &Media,
        width: usize,
    ) -> &[PaneLine] {
        let key = (
            app.open.clone(),
            width,
            app.snapshot.revision(),
            media.revision(),
            app.utc_offset_ms,
            // Only the day matters, so the rows survive until midnight rather
            // than being thrown away every second.
            day_index(app.now_ms + app.utc_offset_ms),
        );
        if self.key.as_ref() != Some(&key) {
            self.lines = build_message_lines(app, theme, media, width, app.now_ms);
            self.key = Some(key);
        }
        &self.lines
    }
}

pub fn draw(frame: &mut Frame, app: &mut App, media: &mut Media, theme: &Theme, cache: &mut Cache) {
    let areas = layout::areas(frame.area(), app, app.composer_rows());

    draw_status(frame, areas.status, app, media, theme);
    if areas.nav.width > 0 {
        draw_conversations(frame, areas.nav, app, theme);
    }
    draw_messages(frame, areas.messages, app, media, theme, cache);
    if areas.members.width > 0 {
        draw_members(frame, areas.members, app, theme);
    }
    draw_composer(frame, areas.composer, app, theme);
    if let Some(picker) = &app.picker {
        draw_picker(frame, frame.area(), picker, theme);
    }
    if let Some(browser) = &app.browser {
        draw_browser(frame, frame.area(), browser, theme);
    }
}

// ---------------------------------------------------------------- picker ---

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// The modal list. Drawn last so it sits over every pane, and cleared
/// underneath so the panes do not bleed through the gaps between spans.
fn draw_picker(frame: &mut Frame, area: Rect, picker: &Picker, theme: &Theme) {
    let visible = picker.visible();
    let width = area.width.saturating_sub(4).clamp(24, 60);
    let rows = visible.len().clamp(1, 12) as u16;
    // Border 2 + filter line 1 + hint line 1.
    let height = (rows + 4).min(area.height.saturating_sub(2)).max(5);
    let rect = centered(area, width, height);
    frame.render_widget(Clear, rect);

    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(theme.border_type(BorderSurface::Picker))
        .border_style(theme.style(HG::PickerBorder))
        .title(TuiLine::from(TuiSpan::styled(
            format!(" {} ", picker.title),
            theme.style(HG::ModalTitle),
        )));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.height < 3 || inner.width < 6 {
        return;
    }

    let prompt = TuiSpan::styled("› ", theme.style(HG::Muted));
    let filter_line = if picker.filter.is_empty() {
        TuiLine::from(vec![
            prompt,
            TuiSpan::styled("输入过滤", theme.style(HG::Placeholder)),
        ])
    } else {
        TuiLine::from(vec![
            prompt,
            TuiSpan::styled(picker.filter.clone(), theme.style(HG::Normal)),
        ])
    };
    frame.render_widget(
        Paragraph::new(filter_line),
        Rect { height: 1, ..inner },
    );

    let list_height = inner.height.saturating_sub(2) as usize;
    let start = picker.cursor.saturating_sub(list_height.saturating_sub(1));
    let mut lines: Vec<TuiLine<'static>> = Vec::new();
    if visible.is_empty() {
        lines.push(TuiLine::from(TuiSpan::styled(
            "  没有匹配的人",
            theme.style(HG::Muted),
        )));
    }
    for (row, &index) in visible.iter().enumerate().skip(start).take(list_height) {
        let item = &picker.items[index];
        let at_cursor = row == picker.cursor;
        let ticked = picker.selected.contains(&item.id);
        let mut spans = vec![TuiSpan::styled(
            if at_cursor { SELECTED_MARK } else { UNSELECTED_MARK },
            theme.style(HG::SelectionMarker),
        )];
        if picker.multi {
            spans.push(TuiSpan::styled(
                if ticked { "[x] " } else { "[ ] " },
                theme.style(if ticked { HG::Selection } else { HG::Muted }),
            ));
        }
        spans.push(TuiSpan::styled(
            item.label.clone(),
            theme.style(if at_cursor { HG::SelectedRow } else { HG::Normal }),
        ));
        if !item.detail.is_empty() && item.detail != item.label {
            spans.push(TuiSpan::styled(
                format!("  {}", item.detail),
                theme.style(HG::Description),
            ));
        }
        lines.push(TuiLine::from(spans));
    }
    frame.render_widget(
        Paragraph::new(lines),
        Rect {
            y: inner.y + 1,
            height: list_height as u16,
            ..inner
        },
    );

    let hint = if picker.multi {
        format!("已选 {}   ↑↓ 移动  Space 勾选  Enter 确定  Esc 取消", picker.selected.len())
    } else {
        "↑↓ 移动  Enter 选定  Esc 取消".to_owned()
    };
    frame.render_widget(
        Paragraph::new(TuiLine::from(TuiSpan::styled(hint, theme.style(HG::Hint)))),
        Rect {
            y: inner.y + inner.height - 1,
            height: 1,
            ..inner
        },
    );

    // The caret lives in the filter: typing is the primary gesture here.
    let column = 2 + picker.filter.width() as u16;
    frame.set_cursor_position((inner.x + column.min(inner.width - 1), inner.y));
}

/// The file browser. Same shape as the picker, plus a directory to walk.
fn draw_browser(frame: &mut Frame, area: Rect, browser: &Browser, theme: &Theme) {
    let entries = browser.visible();
    let width = area.width.saturating_sub(4).clamp(30, 72);
    let rows = entries.len().clamp(1, 12) as u16;
    let height = (rows + 4).min(area.height.saturating_sub(2)).max(6);
    let rect = centered(area, width, height);
    frame.render_widget(Clear, rect);

    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(theme.border_type(BorderSurface::Picker))
        .border_style(theme.style(HG::PickerBorder))
        .title(TuiLine::from(TuiSpan::styled(
            // The tail of a path is what identifies it, so a long one loses
            // its head rather than its name.
            format!(
                " 选择图片 · {} ",
                truncate_left(&browser.dir_label(), width.saturating_sub(14) as usize)
            ),
            theme.style(HG::ModalTitle),
        )));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.height < 3 || inner.width < 10 {
        return;
    }

    let filter = if browser.filter.is_empty() {
        TuiLine::from(vec![
            TuiSpan::styled("› ", theme.style(HG::Muted)),
            TuiSpan::styled("输入过滤", theme.style(HG::Placeholder)),
        ])
    } else {
        TuiLine::from(vec![
            TuiSpan::styled("› ", theme.style(HG::Muted)),
            TuiSpan::styled(browser.filter.clone(), theme.style(HG::Normal)),
        ])
    };
    frame.render_widget(Paragraph::new(filter), Rect { height: 1, ..inner });

    let list_height = inner.height.saturating_sub(2) as usize;
    let start = browser.cursor.saturating_sub(list_height.saturating_sub(1));
    let mut lines: Vec<TuiLine<'static>> = Vec::new();
    if let Some(error) = &browser.error {
        lines.push(TuiLine::from(TuiSpan::styled(
            format!("  {error}"),
            theme.style(HG::Error),
        )));
    } else if entries.is_empty() {
        lines.push(TuiLine::from(TuiSpan::styled(
            "  这里没有图片",
            theme.style(HG::Muted),
        )));
    }
    for (row, entry) in entries.iter().enumerate().skip(start).take(list_height) {
        let at_cursor = row == browser.cursor;
        let mut spans = vec![TuiSpan::styled(
            if at_cursor { SELECTED_MARK } else { UNSELECTED_MARK },
            theme.style(HG::SelectionMarker),
        )];
        match entry.kind {
            BrowseKind::Parent | BrowseKind::Directory => {
                spans.push(TuiSpan::styled("📁 ", theme.style(HG::Muted)));
                spans.push(TuiSpan::styled(
                    entry.name.clone(),
                    theme.style(if at_cursor { HG::SelectedRow } else { HG::Strong }),
                ));
            }
            BrowseKind::Image => {
                let ticked = browser.is_selected(entry);
                spans.push(TuiSpan::styled(
                    if ticked { "[x] " } else { "[ ] " },
                    theme.style(if ticked { HG::Selection } else { HG::Muted }),
                ));
                spans.push(TuiSpan::styled(
                    entry.name.clone(),
                    theme.style(if at_cursor { HG::SelectedRow } else { HG::Normal }),
                ));
                spans.push(TuiSpan::styled(
                    format!("  {}", format_bytes(entry.bytes)),
                    theme.style(HG::Description),
                ));
            }
        }
        lines.push(TuiLine::from(spans));
    }
    frame.render_widget(
        Paragraph::new(lines),
        Rect {
            y: inner.y + 1,
            height: list_height as u16,
            ..inner
        },
    );

    let picked = browser.selected_count();
    let hint = if picked > 0 {
        format!("已选 {picked} 张   Space 勾选  Enter 发送  Esc 取消")
    } else {
        "↑↓ 移动  Enter 进目录 / 选中  Space 多选  Backspace 上一级".to_owned()
    };
    frame.render_widget(
        Paragraph::new(TuiLine::from(TuiSpan::styled(
            truncate(&hint, inner.width as usize),
            theme.style(HG::Hint),
        ))),
        Rect {
            y: inner.y + inner.height - 1,
            height: 1,
            ..inner
        },
    );
    let column = 2 + browser.filter.width() as u16;
    frame.set_cursor_position((inner.x + column.min(inner.width - 1), inner.y));
}

/// Renders one frame without a live cursor, for the off-screen capture paths.
pub fn draw_static(frame: &mut Frame, app: &App, theme: &Theme) {
    let mut clone = app.clone_for_render();
    let mut media = Media::disabled();
    draw(frame, &mut clone, &mut media, theme, &mut Cache::default());
}

fn pane_block<'a>(app: &App, theme: &Theme, pane: Pane, title: String) -> Block<'a> {
    let focused = app.pane == pane;
    Block::new()
        .borders(Borders::ALL)
        .border_type(theme.border_type(BorderSurface::Pane))
        .border_style(theme.style(if focused {
            HG::FocusedPaneBorder
        } else {
            HG::PaneBorder
        }))
        .title(format!(" {} {} ", pane.index() + 1, title))
        .title_style(theme.style(HG::PaneTitle))
}

// ---------------------------------------------------------------- status ---

fn draw_status(frame: &mut Frame, area: Rect, app: &App, media: &Media, theme: &Theme) {
    let mut left = vec![
        TuiSpan::styled("yptd", theme.style(HG::StatusTitle)),
        TuiSpan::styled("  ", theme.style(HG::StatusLabel)),
    ];
    if let Some(me) = app.snapshot.me.as_ref() {
        let name = if app.snapshot.my_name.is_empty() {
            me.0.as_str()
        } else {
            app.snapshot.my_name.as_str()
        };
        left.push(TuiSpan::styled(name.to_owned(), theme.style(HG::Normal)));
        left.push(TuiSpan::styled("  ", theme.style(HG::StatusLabel)));
    }
    if app.snapshot.connected {
        left.push(TuiSpan::styled("● 已连接", theme.style(HG::PresenceOnline)));
    } else {
        left.push(TuiSpan::styled("● 连接中断", theme.style(HG::ConnectionLost)));
    }
    if let Some(percent) = app.snapshot.sync_percent {
        left.push(TuiSpan::styled(
            format!("  同步 {percent}%"),
            theme.style(HG::SyncProgress),
        ));
    }
    if let Some(notice) = &app.notice {
        left.push(TuiSpan::styled(format!("  {notice}"), theme.style(HG::Warning)));
    }

    let unread = app.snapshot.total_unread();
    let mentions = app.snapshot.total_mentions();
    let mut right_spans = Vec::new();
    if media.enabled() {
        right_spans.push(TuiSpan::styled(
            format!("图 {}  ", media.status_label()),
            theme.style(HG::StatusLabel),
        ));
    }
    let right = TuiLine::from({
        right_spans.extend([
        TuiSpan::styled("未读 ", theme.style(HG::StatusLabel)),
        TuiSpan::styled(unread.to_string(), theme.style(HG::UnreadBadge)),
        TuiSpan::styled("  提及 ", theme.style(HG::StatusLabel)),
        TuiSpan::styled(mentions.to_string(), theme.style(HG::MentionBadge)),
        TuiSpan::styled(" ", theme.style(HG::StatusLabel)),
        ]);
        right_spans
    })
    .right_aligned();

    frame.render_widget(Paragraph::new(TuiLine::from(left)), area);
    frame.render_widget(Paragraph::new(right), area);
}

// --------------------------------------------------------- conversations ---

fn draw_conversations(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let block = pane_block(app, theme, Pane::Conversations, Pane::Conversations.title().to_owned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let marker_width = theme.selection_marker().width() as u16;
    let width = inner.width.saturating_sub(marker_width) as usize;
    let mut lines = Vec::new();

    for (row_index, row) in app.nav_rows().iter().enumerate() {
        let selected = app.pane == Pane::Conversations && row_index == app.nav_cursor;
        let mut spans = vec![TuiSpan::styled(
            if selected {
                theme.selection_marker().to_owned()
            } else {
                " ".repeat(marker_width as usize)
            },
            theme.style(HG::SelectionMarker),
        )];

        match row {
            NavRow::Category {
                name,
                collapsed,
                count,
            } => {
                spans.push(TuiSpan::styled(
                    format!("{} {name}", if *collapsed { "▸" } else { "▾" }),
                    theme.style(HG::CategoryHeading),
                ));
                if *collapsed {
                    // The fold arrow shares a shape with the selection marker,
                    // so the count carries the state as well as the glyph does.
                    spans.push(TuiSpan::styled(
                        format!(" {count}"),
                        theme.style(HG::MessageSecondary),
                    ));
                }
            }
            NavRow::Conversation { index } => {
                let Some(conversation) = app.snapshot.conversations.get(*index) else {
                    continue;
                };
                let open = conversation.id == app.open;
                let name_group = if open {
                    HG::NavActive
                } else if conversation.muted {
                    HG::NavMuted
                } else if conversation.mentions > 0 {
                    HG::NavMentioned
                } else if conversation.unread > 0 {
                    HG::NavUnread
                } else {
                    HG::Normal
                };

                let badge = if conversation.mentions > 0 {
                    Some((format!("@{}", conversation.mentions), HG::MentionBadge))
                } else if conversation.unread > 0 {
                    Some((conversation.unread.to_string(), HG::UnreadBadge))
                } else {
                    None
                };
                let badge_width = badge
                    .as_ref()
                    .map_or(0, |(text, _)| text.width() + 1);

                let label = format!(
                    "  {}{}",
                    App::conversation_glyph(conversation.kind),
                    conversation.name
                );
                let label = truncate(&label, width.saturating_sub(badge_width));
                spans.push(TuiSpan::styled(label, theme.style(name_group)));
                if let Some((text, group)) = badge {
                    spans.push(TuiSpan::styled(" ", theme.style(HG::Normal)));
                    spans.push(TuiSpan::styled(text, theme.style(group)));
                }
                if conversation.muted {
                    spans.push(TuiSpan::styled(" 🔕", theme.style(HG::NavMuted)));
                }
            }
        }
        lines.push(TuiLine::from(spans));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

// --------------------------------------------------------------- members ---

fn draw_members(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let block = pane_block(app, theme, Pane::Members, Pane::Members.title().to_owned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = Vec::new();
    let mut flat_index = 0usize;
    for (role, bucket) in app.members_by_role() {
        lines.push(TuiLine::from(TuiSpan::styled(
            format!("{} {}", role.label(), bucket.len()),
            theme.style(HG::MemberGroupHeading),
        )));
        for member in bucket {
            let selected = app.pane == Pane::Members && flat_index == app.member_cursor;
            let mut spans = vec![
                TuiSpan::styled(
                    if selected {
                        theme.selection_marker().to_owned()
                    } else {
                        " ".repeat(theme.selection_marker().width())
                    },
                    theme.style(HG::SelectionMarker),
                ),
                TuiSpan::styled(
                    if member.online { "● " } else { "○ " },
                    theme.style(if member.online {
                        HG::PresenceOnline
                    } else {
                        HG::PresenceOffline
                    }),
                ),
                TuiSpan::styled(
                    member.name.clone(),
                    theme.style(if member.online {
                        HG::Normal
                    } else {
                        HG::PresenceOffline
                    }),
                ),
            ];
            if member.is_bot {
                spans.push(TuiSpan::styled(" BOT", theme.style(HG::BotBadge)));
            }
            lines.push(TuiLine::from(spans));
            flat_index += 1;
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

// -------------------------------------------------------------- composer ---

fn draw_composer(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let active = app.mode == crate::app::Mode::Insert;
    let title = format!(" {} ", app.mode.label());
    let count = app.composer.char_count();
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(theme.border_type(BorderSurface::Composer))
        .border_style(theme.style(if active {
            HG::ActiveComposerBorder
        } else {
            HG::ComposerBorder
        }))
        .title(TuiLine::from(TuiSpan::styled(
            title,
            theme.style(HG::ComposerTitle),
        )))
        .title(
            TuiLine::from(TuiSpan::styled(
                format!(" {count}/2000 "),
                theme.style(if count > 2000 { HG::Error } else { HG::MessageSecondary }),
            ))
            .right_aligned(),
        );
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // The reply strip sits inside the composer frame, above the text: what is
    // being answered belongs with the answer, not with the message list.
    if let Some(target) = app.reply_target() {
        let summary = format!("{}: {}", target.sender_name, one_line(target.text()));
        let room = inner.width.saturating_sub(2) as usize;
        frame.render_widget(
            Paragraph::new(TuiLine::from(vec![
                TuiSpan::styled("↩ ", theme.style(HG::MarkdownQuote)),
                TuiSpan::styled(truncate(&summary, room), theme.style(HG::QuotePreview)),
            ])),
            Rect { height: 1, ..inner },
        );
        inner = Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(1),
            ..inner
        };
        if inner.height == 0 {
            return;
        }
    }

    if !app.pending.is_empty() {
        let names: Vec<String> = app
            .pending
            .iter()
            .map(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.display().to_string())
            })
            .collect();
        let summary = format!("{} 个待发：{}", names.len(), names.join("  "));
        frame.render_widget(
            Paragraph::new(TuiLine::from(vec![
                TuiSpan::styled("📎 ", theme.style(HG::MessageAttachment)),
                TuiSpan::styled(
                    truncate(&summary, inner.width.saturating_sub(3) as usize),
                    theme.style(HG::Tag),
                ),
            ])),
            Rect { height: 1, ..inner },
        );
        inner = Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(1),
            ..inner
        };
        if inner.height == 0 {
            return;
        }
    }

    if app.composer.is_empty() && !active {
        frame.render_widget(
            Paragraph::new(TuiLine::from(TuiSpan::styled(
                if app.reply_to.is_some() {
                    "按 i 输入回复，Esc 取消引用"
                } else if !app.pending.is_empty() {
                    "按 i 补一句说明，Enter 发送，Esc 取消"
                } else if app.snapshot.conversations.is_empty() {
                    "按 : 输入 :new 群名 建群，或 :dm 找人私聊"
                } else {
                    "按 i 输入消息，r 引用，: 进入命令"
                },
                theme.style(HG::Placeholder),
            ))),
            inner,
        );
        return;
    }

    let lines: Vec<TuiLine<'static>> = app
        .composer
        .lines()
        .map(|line| TuiLine::from(TuiSpan::styled(line.to_owned(), theme.style(HG::Normal))))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);

    if active {
        // The terminal caret is the only cursor a person trusts; drawing a
        // fake block would double up with the real one on most terminals.
        let (line, column) = app.composer.cursor_position();
        let x = inner.x.saturating_add(column.min(inner.width.saturating_sub(1) as usize) as u16);
        let y = inner.y.saturating_add(line.min(inner.height.saturating_sub(1) as usize) as u16);
        frame.set_cursor_position((x, y));
    }
}

/// One command and what it does, padded so that two of these centre to the
/// same left edge. `Paragraph` centres each line on its own, so equal width
/// is the only thing that lines them up.
fn command_row(command: &str, description: &str, theme: &Theme) -> TuiLine<'static> {
    const COMMAND_CELLS: usize = 12;
    const DESCRIPTION_CELLS: usize = 22;
    let pad = |text: &str, cells: usize| {
        let mut out = text.to_owned();
        for _ in text.width()..cells {
            out.push(' ');
        }
        out
    };
    TuiLine::from(vec![
        TuiSpan::styled(pad(command, COMMAND_CELLS), theme.style(HG::Shortcut)),
        TuiSpan::styled(pad(description, DESCRIPTION_CELLS), theme.style(HG::Description)),
    ])
    .centered()
}

/// What to do when there is nothing yet: the two commands that make a
/// conversation, centred in the empty pane.
fn draw_empty_state(frame: &mut Frame, inner: Rect, theme: &Theme) {
    let rows: Vec<TuiLine<'static>> = vec![
        TuiLine::from(TuiSpan::styled("还没有会话", theme.style(HG::Title))).centered(),
        TuiLine::from(""),
        TuiLine::from(vec![
            TuiSpan::styled("按 ", theme.style(HG::Muted)),
            TuiSpan::styled(":", theme.style(HG::Shortcut)),
            TuiSpan::styled(" 进入命令行，然后", theme.style(HG::Muted)),
        ])
        .centered(),
        TuiLine::from(""),
        command_row(":new 群名", "建一个群，接着挑人进来", theme),
        command_row(":dm", "找服务器上的人私聊", theme),
        TuiLine::from(""),
        TuiLine::from(TuiSpan::styled(
            "别人把你拉进群，这里也会自己出现",
            theme.style(HG::Hint),
        ))
        .centered(),
    ];
    // Sit the block a little above centre; dead centre reads as too low
    // once the composer is on screen.
    let top = inner.height.saturating_sub(rows.len() as u16) / 3;
    let area = Rect {
        y: inner.y + top,
        height: inner.height.saturating_sub(top),
        ..inner
    };
    frame.render_widget(Paragraph::new(rows), area);
}

// -------------------------------------------------------------- messages ---

/// The line above a picture block: a filename when there is one picture, a
/// count when there are several.
fn attachment_label(
    attachments: &[&im_model::Attachment],
    media: &Media,
    theme: &Theme,
) -> Vec<TuiSpan<'static>> {
    let glyph = attachments[0].kind.glyph();
    if attachments.len() > 1 {
        return vec![TuiSpan::styled(
            format!("{glyph} {} 张图片", attachments.len()),
            theme.style(HG::MessageAttachment),
        )];
    }
    let attachment = attachments[0];
    let key = attachment.key();
    let mut label = format!("{glyph} {}", attachment.name);
    // OpenIM records no byte count for an uploaded picture, so prefer the
    // pixel size once it is decoded -- more useful for an image anyway --
    // and print nothing rather than a confident "0 B".
    if let Some((w, h)) = media.dimensions(key) {
        label.push_str(&format!("  {w}×{h}"));
    } else if attachment.bytes > 0 {
        label.push_str("  ");
        label.push_str(&format_bytes(attachment.bytes));
    }
    if let Some(reason) = media.failure(key) {
        label.push_str("  (");
        label.push_str(reason);
        label.push(')');
    }
    vec![TuiSpan::styled(label, theme.style(HG::MessageAttachment))]
}

/// Lays out a run's pictures, or `None` when none of them can be drawn.
///
/// A picture whose size is not known yet still gets a tile, using a common
/// photo shape: reserving its space up front means the block does not jump
/// when the download lands.
fn album_placement(
    attachments: &[&im_model::Attachment],
    media: &Media,
    width: usize,
) -> Option<Placement> {
    let cell = media.cell_size()?;
    const ASSUMED: crate::album::Source = crate::album::Source { width: 1600, height: 1200 };
    let drawable: Vec<&&im_model::Attachment> = attachments
        .iter()
        .filter(|a| a.kind == im_model::AttachmentKind::Image && media.failure(a.key()).is_none())
        .collect();
    if drawable.is_empty() {
        return None;
    }
    let sources: Vec<crate::album::Source> = drawable
        .iter()
        .map(|a| {
            media
                .dimensions(a.key())
                .map(|(width, height)| crate::album::Source { width, height })
                .unwrap_or(ASSUMED)
        })
        .collect();
    let columns = u16::try_from(width).unwrap_or(u16::MAX);
    let album = crate::album::layout(&sources, columns, cell);
    if album.is_empty() {
        return None;
    }
    Some(Placement {
        keys: drawable.iter().map(|a| a.key().to_owned()).collect(),
        tiles: album.tiles,
    })
}

fn block_height(placement: &Placement) -> u16 {
    placement
        .tiles
        .iter()
        .map(|tile| tile.y.saturating_add(tile.height))
        .max()
        .unwrap_or(0)
}

/// One flattened output line plus the message it belongs to, so the gutter and
/// the scroll arithmetic can both work in line space.
struct PaneLine {
    message_index: Option<usize>,
    /// Whether the message on this row names the local user.
    mentions_me: bool,
    /// Which run of messages this row belongs to, as (first, length). The
    /// selection marker is decided from this at draw time rather than baked
    /// in, so moving the cursor costs nothing and the laid-out lines can be
    /// kept between frames.
    run: Option<(usize, usize)>,
    line: TuiLine<'static>,
    /// Set on the first row of a reserved picture block. The rows below it
    /// are blank placeholders, so the height is known during layout and
    /// cannot shift when the pictures are finally painted.
    album: Option<Placement>,
}

/// Where a block's pictures go, relative to the row that carries it.
#[derive(Clone, Debug)]
struct Placement {
    /// Media cache keys, in the order the tiles index them.
    keys: Vec<String>,
    tiles: Vec<crate::album::Tile>,
}

fn draw_messages(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    media: &mut Media,
    theme: &Theme,
    cache: &mut Cache,
) {
    let title = match app.conversation() {
        Some(conversation) => format!(
            "{}{}  ·  {} 人",
            App::conversation_glyph(conversation.kind),
            conversation.name,
            conversation.member_count
        ),
        None => Pane::Messages.title().to_owned(),
    };
    let block = pane_block(app, theme, Pane::Messages, title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let content_width = inner.width.saturating_sub(GUTTER) as usize;
    if content_width == 0 || inner.height == 0 {
        return;
    }

    // A brand-new account lands here with nothing at all. An empty pane
    // teaches nobody the two commands that fix it.
    if app.snapshot.conversations.is_empty() {
        app.message_line_map = Vec::new();
        draw_empty_state(frame, inner, theme);
        return;
    }

    let lines = cache.rows(app, theme, media, content_width);
    let top = viewport_top(app, lines, inner.height as usize);
    let window: Vec<&PaneLine> = lines.iter().skip(top).take(inner.height as usize).collect();

    // Record which message each visible row belongs to, so a click resolves
    // against exactly the frame the user saw.
    app.message_line_map = window.iter().map(|entry| entry.message_index).collect();

    // Paint the text first so the reserved rows are cleared, then the
    // pictures on top of them.
    let blocks: Vec<(usize, Placement)> = window
        .iter()
        .enumerate()
        .filter_map(|(row, entry)| entry.album.clone().map(|album| (row, album)))
        .collect();
    // The gutter is the one part that depends on where the cursor is, so it
    // goes on here rather than into the kept rows.
    let visible: Vec<TuiLine<'static>> = window
        .iter()
        .map(|entry| with_gutter(entry, row_selected(app, entry), theme))
        .collect();
    frame.render_widget(Paragraph::new(visible), inner);

    let bottom = inner.y.saturating_add(inner.height);
    for (row, placement) in blocks {
        let top = inner.y.saturating_add(row as u16);
        for tile in &placement.tiles {
            let Some(key) = placement.keys.get(tile.index) else {
                continue;
            };
            let y = top.saturating_add(tile.y);
            if y >= bottom {
                continue;
            }
            // A block scrolled half off the bottom paints only what fits.
            let height = tile.height.min(bottom - y);
            let x = inner.x.saturating_add(GUTTER).saturating_add(tile.x);
            let right = inner.x.saturating_add(inner.width);
            if height == 0 || x >= right {
                continue;
            }
            let width = tile.width.min(right - x);
            if width == 0 {
                continue;
            }
            // One picture keeps its own shape; a row of them fills identical
            // tiles, or the row looks ragged.
            let fill = placement.tiles.len() > 1;
            media.render(frame, Rect { x, y, width, height }, key, fill);
        }
    }
}

/// Chooses the first visible line.
///
/// Following the live edge anchors to the bottom. Otherwise the viewport
/// starts at the scrolled-to message but is nudged so the selection stays on
/// screen -- a cursor you cannot see is worse than a viewport that moved.
fn viewport_top(app: &App, lines: &[PaneLine], height: usize) -> usize {
    let total = lines.len();
    if app.follow_latest {
        return total.saturating_sub(height);
    }

    let first_line_of = |message_index: usize| {
        lines
            .iter()
            .position(|entry| entry.message_index == Some(message_index))
    };
    let last_line_of = |message_index: usize| {
        lines
            .iter()
            .rposition(|entry| entry.message_index == Some(message_index))
    };

    let mut top = first_line_of(app.message_scroll)
        .unwrap_or(0)
        .min(total.saturating_sub(1));
    // A message with no rows of its own -- one folded into a picture block --
    // must leave the viewport where it is rather than snapping to the top.
    if let (Some(cursor_start), Some(cursor_end)) =
        (first_line_of(app.message_cursor), last_line_of(app.message_cursor))
    {
        if cursor_start < top {
            top = cursor_start;
        } else if cursor_end >= top + height {
            top = cursor_end.saturating_sub(height.saturating_sub(1));
        }
    }
    top.min(total.saturating_sub(height.min(total)))
}

fn build_message_lines(
    app: &App,
    theme: &Theme,
    media: &Media,
    width: usize,
    now_ms: i64,
) -> Vec<PaneLine> {
    let palette = syntax_palette(theme);
    let messages = app.messages();
    let unread_at = app.unread_boundary();
    let mut out: Vec<PaneLine> = Vec::new();

    let runs = album_runs(&messages);
    let superseded = superseded_placeholders(&messages);
    for (index, message) in messages.iter().enumerate() {
        // The agent's "working on it" has served its purpose once the answer
        // is under it.
        if superseded[index] {
            continue;
        }
        // A picture that belongs to the block started earlier draws nothing
        // of its own; its rows are inside that block.
        if runs[index].0 != index {
            continue;
        }
        let run_len = runs[index].1;
        let previous = index.checked_sub(1).and_then(|i| messages.get(i)).copied();

        if starts_new_day(message, previous, app.utc_offset_ms) {
            out.push(divider(
                &format!("  {}  ", day_label(message.sent_at_ms() + app.utc_offset_ms, now_ms + app.utc_offset_ms)),
                theme.style(HG::DateDivider),
                width,
                index,
            ));
        }
        if unread_at == Some(index) {
            out.push(divider(
                "  以下为未读  ",
                theme.style(HG::UnreadDivider),
                width,
                index,
            ));
        }

        let show_header = starts_author_group(message, previous);
        if show_header && index > 0 {
            out.push(content_line(Vec::new(), Some((index, run_len)), Some(index), false));
        }

        let addressed = message.mentions.iter().any(|m| m.notifies_me);
        let run = &messages[index..index + run_len];
        let (lines, preview) = render_message(
            run,
            theme,
            width,
            show_header,
            palette,
            media,
            app.utc_offset_ms,
        );
        let block_rows = preview.as_ref().map(block_height).unwrap_or(0) as usize;
        let first_block_row = lines.len() - block_rows;
        for (offset, line) in lines.into_iter().enumerate() {
            // Rows inside a block belong to the pictures they cover, so the
            // cursor can still land on each message of the run.
            let owner = match offset.checked_sub(first_block_row) {
                Some(within) if block_rows > 0 => index + within.min(run_len - 1),
                _ => index,
            };
            let mut entry = content_line(line, Some((index, run_len)), Some(owner), addressed);
            if offset == first_block_row && block_rows > 0 {
                entry.album = preview.clone();
            }
            out.push(entry);
        }
    }
    out
}

/// Which placeholder messages have been answered already.
///
/// A placeholder says "I am working on this"; once its sender has said
/// something else in the same conversation, it is only clutter. Hiding it
/// here rather than withdrawing it on the server keeps the decision local
/// and reversible, and avoids a revoke that cannot reliably name its target.
fn superseded_placeholders(messages: &[&Message]) -> Vec<bool> {
    let mut hidden = vec![false; messages.len()];
    let mut spoken_since: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (index, message) in messages.iter().enumerate().rev() {
        if message.transient && spoken_since.contains(message.sender.0.as_str()) {
            hidden[index] = true;
        } else {
            spoken_since.insert(message.sender.0.as_str());
        }
    }
    hidden
}

/// For each message, which message starts its picture block and how long that
/// block is. A message that is not part of one is a block of its own.
///
/// Sending four pictures is four messages, because OpenIM has no message that
/// carries more than one. Drawing them as four separate blocks would bury the
/// conversation, so consecutive pictures from one person, sent close
/// together, are drawn as a single block.
fn album_runs(messages: &[&Message]) -> Vec<(usize, usize)> {
    /// Pictures further apart than this were two thoughts, not one batch.
    const GROUP_MS: i64 = 60_000;
    let groupable = |m: &Message| {
        m.attachment().is_some_and(|a| a.kind == im_model::AttachmentKind::Image)
            && m.text().is_empty()
            && m.quote.is_none()
            && m.reactions.is_empty()
            && m.visibility == Visibility::Broadcast
    };

    let mut runs = vec![(0usize, 1usize); messages.len()];
    let mut index = 0;
    while index < messages.len() {
        let mut end = index + 1;
        if groupable(messages[index]) {
            while end < messages.len()
                && groupable(messages[end])
                && messages[end].sender == messages[index].sender
                && messages[end].sent_at_ms() - messages[end - 1].sent_at_ms() <= GROUP_MS
            {
                end += 1;
            }
        }
        for member in index..end {
            runs[member] = (index, end - index);
        }
        index = end;
    }
    runs
}

/// Whether the cursor sits inside the run this row belongs to.
fn row_selected(app: &App, entry: &PaneLine) -> bool {
    app.pane == Pane::Messages
        && entry
            .run
            .is_some_and(|(start, len)| (start..start + len).contains(&app.message_cursor))
}

fn content_line(
    spans: Vec<TuiSpan<'static>>,
    run: Option<(usize, usize)>,
    message_index: Option<usize>,
    mentions_me: bool,
) -> PaneLine {
    PaneLine {
        message_index,
        mentions_me,
        run,
        line: TuiLine::from(spans),
        album: None,
    }
}

/// Prepends the gutter. Two columns either way, so the content width never
/// changes and nothing re-wraps when the cursor moves.
///
/// The same two columns carry two different facts: where the cursor is, and
/// which messages are addressed to you. Selection wins when both apply --
/// you can see the mention in the text, but only the gutter says where you
/// are.
fn with_gutter(entry: &PaneLine, selected: bool, theme: &Theme) -> TuiLine<'static> {
    let mut spans = Vec::with_capacity(entry.line.spans.len() + 1);
    spans.push(match (selected, entry.mentions_me) {
        (true, _) => TuiSpan::styled(SELECTED_MARK, theme.style(HG::MessageSelectedBorder)),
        (false, true) => TuiSpan::styled(MENTION_MARK, theme.style(HG::MentionSelf)),
        (false, false) => TuiSpan::styled(UNSELECTED_MARK, theme.style(HG::MessageSelectedBorder)),
    });
    spans.extend(entry.line.spans.iter().cloned());
    TuiLine::from(spans)
}

/// A divider belongs to the message it introduces, not to nothing. Tagging it
/// `None` would make scrolling to that message land *below* its own divider.
/// A rule with its label centred, the way a chat window separates one day
/// from the next: the eye should read the date, not hunt for it at one end.
fn divider(label: &str, style: Style, width: usize, message_index: usize) -> PaneLine {
    let rule = width.saturating_sub(label.width());
    let left = rule / 2;
    let right = rule - left;
    PaneLine {
        message_index: Some(message_index),
        mentions_me: false,
        run: None,
        line: TuiLine::from(vec![
            TuiSpan::styled("─".repeat(left), style),
            TuiSpan::styled(label.to_owned(), style),
            TuiSpan::styled("─".repeat(right), style),
        ]),
        album: None,
    }
}

/// Renders one message to spans-per-line, without the gutter.
fn render_message(
    run: &[&Message],
    theme: &Theme,
    width: usize,
    show_header: bool,
    palette: syntax::Palette,
    media: &Media,
    utc_offset_ms: i64,
) -> (Vec<Vec<TuiSpan<'static>>>, Option<Placement>) {
    let message = run[0];
    let mut out = Vec::new();
    let mut preview: Option<Placement> = None;

    if message.is_system() {
        out.push(vec![TuiSpan::styled(
            format!("— {} —", message.text()),
            theme.style(HG::MessageSecondary),
        )]);
        return (out, None);
    }

    let ghost = message.visibility == Visibility::Ghost;

    if show_header {
        let mut header = Vec::new();
        if ghost {
            header.push(TuiSpan::styled("◇ ", theme.style(HG::GhostMessage)));
        }
        header.push(TuiSpan::styled(
            message.sender_name.clone(),
            if ghost {
                theme.style(HG::GhostMessage)
            } else {
                theme.style(author_colour(&message.sender.0))
            },
        ));
        header.push(TuiSpan::styled(
            format!("  {}", format_time(message.sent_at_ms() + utc_offset_ms)),
            theme.style(HG::MessageTimestamp),
        ));
        if message.edited {
            header.push(TuiSpan::styled(" (已编辑)", theme.style(HG::Edited)));
        }
        match message.send_state {
            SendState::Sending => {
                header.push(TuiSpan::styled(" 发送中…", theme.style(HG::SendPending)));
            }
            SendState::Failed => {
                header.push(TuiSpan::styled(" 发送失败", theme.style(HG::SendFailed)));
            }
            SendState::Sent => {}
        }
        if let Some(read_by) = message.read_by {
            header.push(TuiSpan::styled(
                format!("  已读 {read_by}"),
                theme.style(HG::ReadReceipt),
            ));
        }
        if ghost {
            header.push(TuiSpan::styled("  仅自己可见", theme.style(HG::GhostMessage)));
        }
        out.push(header);
    }

    if let Some(quote) = &message.quote {
        let summary = format!("{}: {}", quote.sender_name, quote.summary);
        for line in wrap(&RichText::plain(truncate(&summary, width.saturating_sub(2))), width) {
            out.push(vec![
                TuiSpan::styled("▏", theme.style(HG::MarkdownQuote)),
                TuiSpan::styled(line.text, theme.style(HG::QuotePreview)),
            ]);
        }
    }

    // Markdown is how people write in a chat window, whatever content type
    // the message arrived as -- backticks and fences come from bots and from
    // anyone pasting a snippet. Mentions are the exception: they are byte
    // ranges into the raw text, and parsing moves the text out from under
    // them, so a message that names somebody stays literal.
    let markdown_source = match &message.body {
        Body::Markdown(source) => Some(source.as_str()),
        Body::Text(text) if message.mentions.is_empty() && looks_like_markdown(text) => Some(text.as_str()),
        _ => None,
    };
    let rich_lines = match markdown_source {
        Some(source) => {
            let mut blocks = parse_markdown(source);
            // Syntect needs the whole block in order, so highlighting happens
            // before layout rather than per rendered line.
            for block in &mut blocks {
                if let tui_richtext::Block::Code {
                    language,
                    lines,
                    highlights,
                } = block
                    && syntax::supported(language.as_deref())
                {
                    *highlights = syntax::highlight(language.as_deref(), lines, palette);
                }
            }
            layout_blocks(&blocks, width)
        }
        None => {
            let mut rich = RichText::plain(message.text());
            for mention in &message.mentions {
                rich.push_span(Span::new(
                    mention.start,
                    mention.end,
                    if mention.mentions_everyone() {
                        SpanKind::MentionAll
                    } else if mention.notifies_me {
                        SpanKind::MentionSelf
                    } else {
                        SpanKind::MentionOther
                    },
                ));
            }
            rich.normalize();
            wrap(&rich, width)
        }
    };
    for line in &rich_lines {
        out.push(rich_line_to_spans(line, theme, ghost));
    }

    let attachments: Vec<&im_model::Attachment> =
        run.iter().filter_map(|m| m.attachment()).collect();
    if !attachments.is_empty() {
        out.push(attachment_label(&attachments, media, theme));
        if let Some(placement) = album_placement(&attachments, media, width) {
            for _ in 0..block_height(&placement) {
                out.push(Vec::new());
            }
            preview = Some(placement);
        }
    }

    if !message.reactions.is_empty() {
        let mut spans = Vec::new();
        for reaction in &message.reactions {
            spans.push(TuiSpan::styled(
                format!("{} {}  ", reaction.emoji, reaction.count),
                theme.style(if reaction.by_me {
                    HG::SelfReaction
                } else {
                    HG::Reaction
                }),
            ));
        }
        out.push(spans);
    }

    (out, preview)
}

/// Splits a wrapped line into styled runs: one base style per line, inline
/// spans layered on top. Two layers, never three.
fn rich_line_to_spans(line: &RichLine, theme: &Theme, ghost: bool) -> Vec<TuiSpan<'static>> {
    let base_group = match line.style {
        LineStyle::Body => {
            if ghost {
                HG::GhostMessage
            } else {
                HG::MessageBody
            }
        }
        LineStyle::Heading(1) => HG::MarkdownHeading1,
        LineStyle::Heading(2) => HG::MarkdownHeading2,
        LineStyle::Heading(_) => HG::MarkdownHeading3,
        LineStyle::Quote => HG::MarkdownQuote,
        LineStyle::Code => HG::CodeBlockText,
        LineStyle::CodeBorder => HG::CodeBlockBorder,
    };
    let base = theme.style(base_group);

    let mut spans = Vec::new();
    if let Some(prefix) = &line.prefix {
        let group = match prefix.kind {
            PrefixKind::Quote => HG::MarkdownQuote,
            PrefixKind::Bullet | PrefixKind::Indent => HG::MarkdownMarker,
            PrefixKind::CodeBorder | PrefixKind::CodeBody => HG::CodeBlockBorder,
        };
        spans.push(TuiSpan::styled(prefix.text.clone(), theme.style(group)));
    }

    let suffix = line.suffix.as_ref().map(|edge| {
        let group = match edge.kind {
            PrefixKind::Quote => HG::MarkdownQuote,
            PrefixKind::Bullet | PrefixKind::Indent => HG::MarkdownMarker,
            PrefixKind::CodeBorder | PrefixKind::CodeBody => HG::CodeBlockBorder,
        };
        TuiSpan::styled(edge.text.clone(), theme.style(group))
    });

    if line.spans.is_empty() {
        spans.push(TuiSpan::styled(line.text.clone(), base));
        spans.extend(suffix);
        return spans;
    }

    // Cut the line at every span edge, then compute each segment's style by
    // layering the spans that cover it. Overlapping spans (bold containing
    // italic) compose instead of fighting.
    let mut edges = vec![0usize, line.text.len()];
    for span in &line.spans {
        edges.push(span.start);
        edges.push(span.end);
    }
    edges.sort_unstable();
    edges.dedup();

    for pair in edges.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end || !line.text.is_char_boundary(start) || !line.text.is_char_boundary(end) {
            continue;
        }
        let mut style = base;
        for span in line.spans.iter().filter(|s| s.start <= start && s.end >= end) {
            style = style.patch(span_style(span.kind, theme));
        }
        spans.push(TuiSpan::styled(line.text[start..end].to_owned(), style));
    }
    spans.extend(suffix);
    spans
}

fn span_style(kind: SpanKind, theme: &Theme) -> Style {
    match kind {
        SpanKind::MentionSelf => theme.style(HG::MentionSelf),
        SpanKind::MentionOther => theme.style(HG::MentionOther),
        SpanKind::MentionAll => theme.style(HG::MentionAll),
        SpanKind::Url => theme.style(HG::MessageLink),
        SpanKind::InlineCode => theme.style(HG::InlineCode),
        SpanKind::Bold => theme.style(HG::Strong),
        SpanKind::Italic => theme.style(HG::Emphasis),
        SpanKind::Strikethrough => Style::new().add_modifier(Modifier::CROSSED_OUT),
        SpanKind::Timestamp => theme.style(HG::MessageTimestamp),
        SpanKind::Syntax(rgb) => Style::new().fg(ratatui::style::Color::Rgb(
            (rgb >> 16) as u8,
            (rgb >> 8) as u8,
            rgb as u8,
        )),
    }
}

/// Picks the syntect palette from the terminal's own background, so code does
/// not come out dark-on-dark or light-on-light.
fn syntax_palette(theme: &Theme) -> syntax::Palette {
    match theme.style(HG::Normal).bg {
        Some(ratatui::style::Color::Rgb(r, g, b)) => {
            // Rec. 601 luma is close enough to decide light from dark.
            if (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000 > 128 {
                syntax::Palette::Light
            } else {
                syntax::Palette::Dark
            }
        }
        // A terminal-default background is unknown to us; dark is the common
        // case for terminals and the safer guess for a light-on-dark theme.
        _ => syntax::Palette::Dark,
    }
}

// ----------------------------------------------------------------- utils ---

fn starts_author_group(message: &Message, previous: Option<&Message>) -> bool {
    let Some(previous) = previous else {
        return !message.is_system();
    };
    if message.is_system() || previous.is_system() {
        return !message.is_system();
    }
    previous.sender != message.sender
        || message.sent_at_ms() - previous.sent_at_ms() > AUTHOR_GROUP_MS
        || previous.visibility != message.visibility
}

/// "今天" and "昨天" read faster than a date, and a date is only useful
/// further back than that.
fn day_label(when_ms: i64, now_ms: i64) -> String {
    match day_index(now_ms) - day_index(when_ms) {
        0 => "今天".to_owned(),
        1 => "昨天".to_owned(),
        _ => format_date(when_ms),
    }
}

fn starts_new_day(message: &Message, previous: Option<&Message>, offset_ms: i64) -> bool {
    match previous {
        None => true,
        Some(previous) => {
            day_index(message.sent_at_ms() + offset_ms) != day_index(previous.sent_at_ms() + offset_ms)
        }
    }
}

fn day_index(unix_ms: i64) -> i64 {
    unix_ms.div_euclid(86_400_000)
}

/// Formats a wall-clock time. Callers pass a timestamp already shifted by
/// `App::utc_offset_ms`, so this stays a pure function of its input.
fn format_time(unix_ms: i64) -> String {
    let seconds = unix_ms.div_euclid(1000);
    let minute_of_day = seconds.rem_euclid(86_400) / 60;
    format!("{:02}:{:02}", minute_of_day / 60, minute_of_day % 60)
}

fn format_date(unix_ms: i64) -> String {
    // Days since the Unix epoch, converted through the civil-from-days
    // algorithm so month lengths and leap years are right.
    let days = day_index(unix_ms) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year}-{month:02}-{day:02}")
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Truncates from the left, keeping the end. For a path, the end is the part
/// that says which directory this is.
fn truncate_left(value: &str, width: usize) -> String {
    if value.width() <= width || width < 2 {
        return value.to_owned();
    }
    let mut kept: Vec<char> = Vec::new();
    let mut used = 1; // the leading ellipsis
    for c in value.chars().rev() {
        let cell = c.to_string().width().max(1);
        if used + cell > width {
            break;
        }
        kept.push(c);
        used += cell;
    }
    kept.reverse();
    format!("…{}", kept.into_iter().collect::<String>())
}

/// Truncates to a display width, counting CJK as two cells.
/// A stable colour for one person, derived from their id.
///
/// Names carry most of the work of scanning a busy channel, and a colour that
/// belongs to somebody makes them findable without reading. Derived rather
/// than assigned, so it survives restarts and agrees between clients.
fn author_colour(user_id: &str) -> HG {
    const PALETTE: [HG; 6] = [
        HG::Author1,
        HG::Author2,
        HG::Author3,
        HG::Author4,
        HG::Author5,
        HG::Author6,
    ];
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in user_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    PALETTE[(hash % PALETTE.len() as u64) as usize]
}

/// Whether a message is worth handing to the markdown parser.
///
/// A cheap check first, because most chat lines contain none of this and
/// parsing them would only risk turning a stray asterisk into emphasis.
fn looks_like_markdown(text: &str) -> bool {
    text.contains('`')
        || text.contains("**")
        || text.contains("\n- ")
        || text.contains("\n# ")
        || text.starts_with("- ")
        || text.starts_with("# ")
}

/// A one-line stand-in for a message, for the reply strip.
///
/// The first non-empty line, not every line run together: a quoted code
/// block joined end to end is a wall of backticks that nobody recognises,
/// whereas its opening sentence is exactly what the reader remembers.
fn one_line(value: &str) -> String {
    value
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .filter(|line| !line.is_empty())
        .unwrap_or("（无文字内容）")
        .to_owned()
}

fn truncate(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.to_owned();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for grapheme in value.chars() {
        let cell = grapheme.to_string().width().max(1);
        if used + cell > width.saturating_sub(1) {
            break;
        }
        out.push(grapheme);
        used += cell;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use im_model::AttachmentKind;

    #[test]
    fn recent_days_are_named_rather_than_dated() {
        let day = 86_400_000i64;
        let now = im_model::mock::BASE_MS;
        assert_eq!(super::day_label(now, now), "今天");
        assert_eq!(super::day_label(now - day, now), "昨天");
        assert_eq!(super::day_label(now - 5 * day, now), super::format_date(now - 5 * day));
    }

    #[test]
    fn one_person_always_gets_the_same_colour() {
        let once = super::author_colour("u_lina");
        assert_eq!(once, super::author_colour("u_lina"));
        // Not a hard requirement that any two differ, but a palette that
        // collapsed everyone onto one colour would be useless.
        let spread: std::collections::HashSet<_> = ["a", "b", "c", "d", "e", "f", "g", "h"]
            .iter()
            .map(|id| super::author_colour(id))
            .collect();
        assert!(spread.len() > 1, "every id landed on one colour");
    }

    #[test]
    fn moving_the_cursor_does_not_relay_out_the_messages() {
        // The whole point of the cache: parsing, highlighting and wrapping
        // are what make scrolling heavy, and none of them depend on where
        // the cursor is.
        use crate::app::App;
        let mut app = App::new(im_model::mock::snapshot());
        let theme = super::Theme::default();
        let media = crate::media::Media::disabled();
        let mut cache = super::Cache::default();

        let first = cache.rows(&app, &theme, &media, 60).len();
        let before = cache.lines.as_ptr();
        app.message_cursor = 0;
        app.pane = crate::app::Pane::Conversations;
        let again = cache.rows(&app, &theme, &media, 60).len();
        assert_eq!(first, again);
        assert_eq!(before, cache.lines.as_ptr(), "the rows were rebuilt");
    }

    #[test]
    fn a_new_message_or_a_new_width_does_relay_them_out() {
        use crate::app::App;
        let mut app = App::new(im_model::mock::snapshot());
        let theme = super::Theme::default();
        let media = crate::media::Media::disabled();
        let mut cache = super::Cache::default();

        cache.rows(&app, &theme, &media, 60);
        let key = cache.key.clone();
        cache.rows(&app, &theme, &media, 40);
        assert_ne!(cache.key, key, "a different width needs different wrapping");

        let key = cache.key.clone();
        let mut extra = app.snapshot.messages[0].clone();
        extra.id = im_model::MessageId::new(u64::MAX / 2);
        app.snapshot.upsert_message(extra);
        cache.rows(&app, &theme, &media, 40);
        assert_ne!(cache.key, key, "a new message must appear");
    }

    #[test]
    fn every_attachment_glyph_measures_two_cells() {
        // A glyph the width table calls narrow and the terminal draws wide
        // shifts the whole line, and the border with it.
        for kind in [
            AttachmentKind::Image,
            AttachmentKind::File,
            AttachmentKind::Video,
            AttachmentKind::Audio,
        ] {
            let glyph = kind.glyph();
            assert_eq!(
                super::UnicodeWidthStr::width(glyph),
                2,
                "{kind:?} renders as {glyph:?}"
            );
        }
    }

    use super::*;
    use im_model::mock;

    fn app() -> App {
        App::new(mock::snapshot())
    }

    #[test]
    fn author_grouping_merges_a_run_and_breaks_on_sender_change() {
        let app = app();
        let messages = app.messages();
        let mut headers = 0;
        for (index, message) in messages.iter().enumerate() {
            let previous = index.checked_sub(1).and_then(|i| messages.get(i)).copied();
            if starts_author_group(message, previous) {
                headers += 1;
            }
        }
        assert!(headers < messages.len(), "a same-sender run must share a header");
        assert!(headers > 1, "different senders must not be merged");
    }

    #[test]
    fn a_ghost_reply_never_shares_a_header_with_a_real_message() {
        let app = app();
        let messages = app.messages();
        for (index, message) in messages.iter().enumerate() {
            if message.visibility != Visibility::Ghost {
                continue;
            }
            let previous = index.checked_sub(1).and_then(|i| messages.get(i)).copied();
            assert!(
                starts_author_group(message, previous),
                "a ghost message must be visually separated"
            );
        }
    }

    #[test]
    fn the_gutter_keeps_content_width_identical_whether_selected_or_not() {
        assert_eq!(SELECTED_MARK.width(), UNSELECTED_MARK.width());
        assert_eq!(SELECTED_MARK.width(), GUTTER as usize);
    }

    #[test]
    fn selecting_a_message_does_not_change_how_many_lines_it_occupies() {
        let mut app = app();
        let theme = Theme::default();
        let width = 44;
        let baseline = build_message_lines(&app, &theme, &Media::disabled(), width, app.now_ms).len();
        for cursor in 0..app.messages().len() {
            app.message_cursor = cursor;
            assert_eq!(
                build_message_lines(&app, &theme, &Media::disabled(), width, app.now_ms).len(),
                baseline,
                "cursor {cursor} changed the rendered height"
            );
        }
    }

    #[test]
    fn every_line_fits_the_content_width_at_any_size() {
        let app = app();
        let theme = Theme::default();
        for width in [30usize, 44, 60, 100] {
            for entry in build_message_lines(&app, &theme, &Media::disabled(), width, app.now_ms) {
                let rendered: String = entry
                    .line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                assert!(
                    rendered.width() <= width + GUTTER as usize,
                    "width {width}: {:?} is {} cells",
                    rendered,
                    rendered.width()
                );
            }
        }
    }

    #[test]
    fn both_dividers_are_emitted_once() {
        let app = app();
        let theme = Theme::default();
        let lines = build_message_lines(&app, &theme, &Media::disabled(), 60, app.now_ms);
        let text = |entry: &PaneLine| {
            entry
                .line
                .spans
                .iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        };
        assert_eq!(
            lines.iter().filter(|l| text(l).contains("以下为未读")).count(),
            1
        );
        // Match the rule around the label, not the word: a message that says
        // "今天能出个修订版吗" is not a divider.
        assert_eq!(
            lines.iter().filter(|l| text(l).contains("─  今天  ─")).count(),
            1,
            "one date divider for a single-day fixture"
        );
    }

    #[test]
    fn scrolling_to_a_message_keeps_its_own_divider_in_view() {
        let mut app = app();
        app.follow_latest = false;
        app.message_scroll = 0;
        app.message_cursor = 0;
        let theme = Theme::default();
        let lines = build_message_lines(&app, &theme, &Media::disabled(), 60, app.now_ms);
        let top = viewport_top(&app, &lines, 12);
        let first = lines[top]
            .line
            .spans
            .iter()
            .map(|span| span.content.to_string())
            .collect::<String>();
        assert!(
            first.contains("今天"),
            "the date divider must scroll with its message, got {first:?}"
        );
    }

    #[test]
    fn following_the_live_edge_anchors_the_viewport_to_the_bottom() {
        let app = app();
        let theme = Theme::default();
        let lines = build_message_lines(&app, &theme, &Media::disabled(), 60, app.now_ms);
        let height = 10;
        assert_eq!(viewport_top(&app, &lines, height), lines.len() - height);
    }

    #[test]
    fn the_viewport_follows_the_cursor_when_not_at_the_live_edge() {
        let mut app = app();
        app.follow_latest = false;
        app.message_scroll = 0;
        app.message_cursor = app.messages().len() - 1;
        let theme = Theme::default();
        let lines = build_message_lines(&app, &theme, &Media::disabled(), 60, app.now_ms);
        let height = 6;
        let top = viewport_top(&app, &lines, height);
        let cursor_line = lines
            .iter()
            .rposition(|entry| entry.message_index == Some(app.message_cursor))
            .expect("cursor is rendered");
        assert!(
            (top..top + height).contains(&cursor_line),
            "cursor at line {cursor_line} is outside {top}..{}",
            top + height
        );
    }

    #[test]
    fn byte_sizes_read_as_people_expect() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1_258_291), "1.2 MB");
    }

    #[test]
    fn the_civil_calendar_conversion_is_right_on_known_dates() {
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(1_788_847_200_000), "2026-09-08");
        // 2024 was a leap year: this is the day after 2024-02-28.
        assert_eq!(format_date(1_709_164_800_000), "2024-02-29");
    }

    #[test]
    fn truncation_counts_cjk_as_two_cells() {
        assert_eq!(truncate("排期讨论", 10), "排期讨论");
        assert!(truncate("排期讨论今天出稿", 8).width() <= 8);
    }
}
