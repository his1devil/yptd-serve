//! Rendering. Every color comes from a named highlight group -- there is not a
//! single `Color::` literal below this line.

use im_model::{Body, Message, SendState, Visibility};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as TuiLine, Span as TuiSpan};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_richtext::{
    Line as RichLine, LineStyle, PrefixKind, RichText, Span, SpanKind, layout_blocks,
    parse_markdown, wrap,
};
use tui_theme::{BorderSurface, HighlightGroup as HG, Theme};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, NavRow, Pane};

/// Columns reserved on the left of every message for the selection marker.
/// Always reserved, never conditional: if selecting a message changed the
/// content width, every cursor move would re-wrap the text and the viewport
/// would jump under the reader.
const GUTTER: u16 = 2;
const SELECTED_MARK: &str = "▌ ";
const UNSELECTED_MARK: &str = "  ";

/// Consecutive messages from one sender inside this window share a header.
const AUTHOR_GROUP_MS: i64 = 5 * 60_000;

pub fn draw(frame: &mut Frame, app: &App, theme: &Theme) {
    let [status, body, composer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    let nav_width = if app.show_conversations && body.width >= 72 { 24 } else { 0 };
    let member_width = if app.show_members && body.width >= 100 { 18 } else { 0 };
    let [nav, messages, members] = Layout::horizontal([
        Constraint::Length(nav_width),
        Constraint::Min(40),
        Constraint::Length(member_width),
    ])
    .areas(body);

    draw_status(frame, status, app, theme);
    if nav_width > 0 {
        draw_conversations(frame, nav, app, theme);
    }
    draw_messages(frame, messages, app, theme);
    if member_width > 0 {
        draw_members(frame, members, app, theme);
    }
    draw_composer(frame, composer, app, theme);
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

fn draw_status(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let mut left = vec![
        TuiSpan::styled("yptd", theme.style(HG::StatusTitle)),
        TuiSpan::styled("  ", theme.style(HG::StatusLabel)),
    ];
    if let Some(me) = app.snapshot.me.as_ref() {
        let name = app
            .snapshot
            .members
            .iter()
            .find(|member| &member.id == me)
            .map(|member| member.name.as_str())
            .unwrap_or("未知");
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

    let unread = app.snapshot.total_unread();
    let mentions = app.snapshot.total_mentions();
    let right = TuiLine::from(vec![
        TuiSpan::styled("未读 ", theme.style(HG::StatusLabel)),
        TuiSpan::styled(unread.to_string(), theme.style(HG::UnreadBadge)),
        TuiSpan::styled("  提及 ", theme.style(HG::StatusLabel)),
        TuiSpan::styled(mentions.to_string(), theme.style(HG::MentionBadge)),
        TuiSpan::styled(" ", theme.style(HG::StatusLabel)),
    ])
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
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(theme.border_type(BorderSurface::Composer))
        .border_style(theme.style(if active {
            HG::ActiveComposerBorder
        } else {
            HG::ComposerBorder
        }))
        .title(TuiLine::from(TuiSpan::styled(
            format!(" {} ", app.mode.label()),
            theme.style(HG::ComposerTitle),
        )))
        .title(
            TuiLine::from(TuiSpan::styled(
                format!(" {}/2000 ", app.composer.chars().count()),
                theme.style(HG::MessageSecondary),
            ))
            .right_aligned(),
        );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let body = if app.composer.is_empty() && !active {
        TuiLine::from(TuiSpan::styled(
            "按 i 输入消息，: 进入命令，Space 打开快捷键",
            theme.style(HG::Placeholder),
        ))
    } else {
        TuiLine::from(TuiSpan::styled(app.composer.clone(), theme.style(HG::Normal)))
    };
    frame.render_widget(Paragraph::new(body), inner);
}

// -------------------------------------------------------------- messages ---

/// One flattened output line plus the message it belongs to, so the gutter and
/// the scroll arithmetic can both work in line space.
struct PaneLine {
    message_index: Option<usize>,
    line: TuiLine<'static>,
}

fn draw_messages(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
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

    let lines = build_message_lines(app, theme, content_width);
    let top = viewport_top(app, &lines, inner.height as usize);
    let visible: Vec<TuiLine<'static>> = lines
        .into_iter()
        .skip(top)
        .take(inner.height as usize)
        .map(|entry| entry.line)
        .collect();

    frame.render_widget(Paragraph::new(visible), inner);
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
            .unwrap_or(0)
    };
    let last_line_of = |message_index: usize| {
        lines
            .iter()
            .rposition(|entry| entry.message_index == Some(message_index))
            .unwrap_or(0)
    };

    let mut top = first_line_of(app.message_scroll).min(total.saturating_sub(1));
    let cursor_start = first_line_of(app.message_cursor);
    let cursor_end = last_line_of(app.message_cursor);
    if cursor_start < top {
        top = cursor_start;
    } else if cursor_end >= top + height {
        top = cursor_end.saturating_sub(height.saturating_sub(1));
    }
    top.min(total.saturating_sub(height.min(total)))
}

fn build_message_lines(app: &App, theme: &Theme, width: usize) -> Vec<PaneLine> {
    let messages = app.messages();
    let unread_at = app.unread_boundary();
    let mut out: Vec<PaneLine> = Vec::new();

    for (index, message) in messages.iter().enumerate() {
        let previous = index.checked_sub(1).and_then(|i| messages.get(i)).copied();

        if starts_new_day(message, previous) {
            out.push(divider(
                &format!(" {} ", format_date(message.sent_at_ms())),
                theme.style(HG::DateDivider),
                width,
                index,
            ));
        }
        if unread_at == Some(index) {
            out.push(divider(
                " 以下为未读 ",
                theme.style(HG::UnreadDivider),
                width,
                index,
            ));
        }

        let selected = app.pane == Pane::Messages && index == app.message_cursor;
        let show_header = starts_author_group(message, previous);
        if show_header && index > 0 {
            out.push(gutter_line(Vec::new(), selected, theme, Some(index)));
        }

        for line in render_message(message, theme, width, show_header) {
            out.push(gutter_line(line, selected, theme, Some(index)));
        }
    }
    out
}

fn gutter_line(
    mut spans: Vec<TuiSpan<'static>>,
    selected: bool,
    theme: &Theme,
    message_index: Option<usize>,
) -> PaneLine {
    let mark = TuiSpan::styled(
        if selected { SELECTED_MARK } else { UNSELECTED_MARK },
        theme.style(HG::MessageSelectedBorder),
    );
    spans.insert(0, mark);
    PaneLine {
        message_index,
        line: TuiLine::from(spans),
    }
}

/// A divider belongs to the message it introduces, not to nothing. Tagging it
/// `None` would make scrolling to that message land *below* its own divider.
fn divider(label: &str, style: Style, width: usize, message_index: usize) -> PaneLine {
    let rule = width.saturating_sub(label.width() + 2);
    PaneLine {
        message_index: Some(message_index),
        line: TuiLine::from(vec![
            TuiSpan::styled("  ".to_owned(), style),
            TuiSpan::styled(format!("──{label}{}", "─".repeat(rule)), style),
        ]),
    }
}

/// Renders one message to spans-per-line, without the gutter.
fn render_message(
    message: &Message,
    theme: &Theme,
    width: usize,
    show_header: bool,
) -> Vec<Vec<TuiSpan<'static>>> {
    let mut out = Vec::new();

    if message.is_system() {
        out.push(vec![TuiSpan::styled(
            format!("— {} —", message.text()),
            theme.style(HG::MessageSecondary),
        )]);
        return out;
    }

    let ghost = message.visibility == Visibility::Ghost;

    if show_header {
        let mut header = Vec::new();
        if ghost {
            header.push(TuiSpan::styled("◇ ", theme.style(HG::GhostMessage)));
        }
        header.push(TuiSpan::styled(
            message.sender_name.clone(),
            theme.style(if ghost { HG::GhostMessage } else { HG::MessageAuthor }),
        ));
        header.push(TuiSpan::styled(
            format!("  {}", format_time(message.sent_at_ms())),
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
                header.push(TuiSpan::styled(" 发送失败 · r 重试", theme.style(HG::SendFailed)));
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

    let rich_lines = match &message.body {
        Body::Markdown(source) => layout_blocks(&parse_markdown(source), width),
        _ => {
            let mut rich = RichText::plain(message.text());
            for mention in &message.mentions {
                rich.push_span(Span::new(
                    mention.start,
                    mention.end,
                    if mention.notifies_me {
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

    if let Some(attachment) = message.attachment() {
        out.push(vec![TuiSpan::styled(
            format!(
                "{} {}  {}",
                attachment.kind.glyph(),
                attachment.name,
                format_bytes(attachment.bytes)
            ),
            theme.style(HG::MessageAttachment),
        )]);
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

    out
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

fn starts_new_day(message: &Message, previous: Option<&Message>) -> bool {
    match previous {
        None => true,
        Some(previous) => day_index(message.sent_at_ms()) != day_index(previous.sent_at_ms()),
    }
}

fn day_index(unix_ms: i64) -> i64 {
    unix_ms.div_euclid(86_400_000)
}

/// UTC formatting, deliberately dependency-free for now. Local-zone handling
/// arrives with the real backend, where the offset has to come from config
/// anyway.
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

/// Truncates to a display width, counting CJK as two cells.
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
        let baseline = build_message_lines(&app, &theme, width).len();
        for cursor in 0..app.messages().len() {
            app.message_cursor = cursor;
            assert_eq!(
                build_message_lines(&app, &theme, width).len(),
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
            for entry in build_message_lines(&app, &theme, width) {
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
        let lines = build_message_lines(&app, &theme, 60);
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
        assert_eq!(
            lines.iter().filter(|l| text(l).contains("2026-")).count(),
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
        let lines = build_message_lines(&app, &theme, 60);
        let top = viewport_top(&app, &lines, 12);
        let first = lines[top]
            .line
            .spans
            .iter()
            .map(|span| span.content.to_string())
            .collect::<String>();
        assert!(
            first.contains("2026-"),
            "the date divider must scroll with its message, got {first:?}"
        );
    }

    #[test]
    fn following_the_live_edge_anchors_the_viewport_to_the_bottom() {
        let app = app();
        let theme = Theme::default();
        let lines = build_message_lines(&app, &theme, 60);
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
        let lines = build_message_lines(&app, &theme, 60);
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
