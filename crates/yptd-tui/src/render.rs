//! Off-screen frame capture: plain text, ANSI, or HTML.
//!
//! The default theme is built from the sixteen ANSI names on purpose, so the
//! client borrows whatever palette the user's terminal already has. That makes
//! a screenshot ambiguous unless the palette is stated, which is why capture
//! takes one explicitly.

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Terminal;
use tui_theme::Theme;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Mode, Pane};
use crate::ui;

/// A named starting state, so each capture shows a different part of the UI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scene {
    /// Following the live edge, message pane focused.
    Live,
    /// Scrolled to the top of the conversation: date divider, system notice,
    /// mention highlight, reactions, read receipt.
    Top,
    /// Conversation pane focused with one category collapsed.
    Nav,
    /// Typing, with the composer active.
    Insert,
    /// Cursor on the markdown message, so the fenced code block is in view.
    Code,
}

impl Scene {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "live" => Self::Live,
            "top" => Self::Top,
            "nav" => Self::Nav,
            "insert" => Self::Insert,
            "code" => Self::Code,
            _ => return None,
        })
    }

    fn apply(self, app: &mut App) {
        match self {
            Self::Live => {}
            Self::Top => {
                app.jump_to_top();
                app.message_cursor = 3;
            }
            Self::Nav => {
                app.focus(Pane::Conversations);
                app.nav_cursor = 4;
                app.collapsed.push("研发".to_owned());
            }
            Self::Insert => {
                app.mode = Mode::Insert;
                app.composer.set_text("好的，我把回滚脚本先合了 @陈明");
            }
            Self::Code => {
                app.jump_to_top();
                app.message_cursor = 5;
                app.message_scroll = 4;
                app.follow_latest = false;
            }
        }
    }
}

/// The sixteen ANSI slots plus a default foreground and background.
#[derive(Clone, Copy)]
pub struct Palette {
    pub name: &'static str,
    ansi: [u32; 16],
    pub background: u32,
    pub foreground: u32,
}

/// Tomorrow Night -- a common, unremarkable dark terminal palette. Chosen
/// because it is not this project's palette: the point of a screenshot is to
/// show the client adopting whatever the terminal already uses.
pub const DARK: Palette = Palette {
    name: "dark",
    ansi: [
        0x1D1F21, 0xCC6666, 0xB5BD68, 0xF0C674, 0x81A2BE, 0xB294BB, 0x8ABEB7, 0xC5C8C6,
        0x969896, 0xCC6666, 0xB5BD68, 0xF0C674, 0x81A2BE, 0xB294BB, 0x8ABEB7, 0xFFFFFF,
    ],
    background: 0x1D1F21,
    foreground: 0xC5C8C6,
};

/// The same scheme's light variant, to show the theme following the terminal
/// rather than fighting it.
pub const LIGHT: Palette = Palette {
    name: "light",
    ansi: [
        0x1D1F21, 0xC82829, 0x718C00, 0xEAB700, 0x4271AE, 0x8959A8, 0x3E999F, 0x4D4D4C,
        0x8E908C, 0xC82829, 0x718C00, 0xEAB700, 0x4271AE, 0x8959A8, 0x3E999F, 0x1D1F21,
    ],
    background: 0xFFFFFF,
    foreground: 0x4D4D4C,
};

impl Palette {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "dark" => Some(DARK),
            "light" => Some(LIGHT),
            _ => None,
        }
    }

    fn resolve(self, color: Color, fallback: u32) -> u32 {
        match color {
            Color::Reset => fallback,
            Color::Black => self.ansi[0],
            Color::Red => self.ansi[1],
            Color::Green => self.ansi[2],
            Color::Yellow => self.ansi[3],
            Color::Blue => self.ansi[4],
            Color::Magenta => self.ansi[5],
            Color::Cyan => self.ansi[6],
            Color::Gray => self.ansi[7],
            Color::DarkGray => self.ansi[8],
            Color::LightRed => self.ansi[9],
            Color::LightGreen => self.ansi[10],
            Color::LightYellow => self.ansi[11],
            Color::LightBlue => self.ansi[12],
            Color::LightMagenta => self.ansi[13],
            Color::LightCyan => self.ansi[14],
            Color::White => self.ansi[15],
            Color::Rgb(r, g, b) => u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
            Color::Indexed(index) => self
                .ansi
                .get(usize::from(index))
                .copied()
                .unwrap_or(fallback),
        }
    }
}

/// Renders one frame into a buffer.
pub fn capture(
    width: u16,
    height: u16,
    scene: Scene,
) -> Result<Buffer, Box<dyn std::error::Error>> {
    let mut app = App::new(im_model::mock::snapshot());
    scene.apply(&mut app);
    capture_app(width, height, &app)
}

/// Renders whatever `app` holds -- mock or live -- into an off-screen buffer.
pub fn capture_app(
    width: u16,
    height: u16,
    app: &App,
) -> Result<Buffer, Box<dyn std::error::Error>> {
    let theme = Theme::default();
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    terminal.draw(|frame| ui::draw_static(frame, app, &theme))?;
    Ok(terminal.backend().buffer().clone())
}

/// One run of cells that share a style, plus how many columns it spans.
struct Run {
    text: String,
    cells: usize,
    style: Style,
}

/// Walks a row, coalescing equal-styled cells and skipping the filler cell a
/// double-width grapheme leaves behind.
fn row_runs(buffer: &Buffer, y: u16) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut skip = 0usize;

    for x in 0..buffer.area.width {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        let cell = &buffer[(x, y)];
        let symbol = cell.symbol();
        let cells = symbol.width().max(1);
        skip = cells - 1;

        let style = cell_style(cell);
        match runs.last_mut() {
            Some(run) if run.style == style => {
                run.text.push_str(symbol);
                run.cells += cells;
            }
            _ => runs.push(Run {
                text: symbol.to_owned(),
                cells,
                style,
            }),
        }
    }
    runs
}

fn cell_style(cell: &ratatui::buffer::Cell) -> Style {
    Style::new()
        .fg(cell.fg)
        .bg(cell.bg)
        .add_modifier(cell.modifier)
}

pub fn to_text(buffer: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let row: String = row_runs(buffer, y).into_iter().map(|run| run.text).collect();
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

/// Emits real escape sequences, so the frame can be piped straight into a
/// terminal and checked against what the app actually draws.
pub fn to_ansi(buffer: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for run in row_runs(buffer, y) {
            let mut codes: Vec<String> = vec!["0".to_owned()];
            if run.style.add_modifier.contains(Modifier::BOLD) {
                codes.push("1".to_owned());
            }
            if run.style.add_modifier.contains(Modifier::DIM) {
                codes.push("2".to_owned());
            }
            if run.style.add_modifier.contains(Modifier::ITALIC) {
                codes.push("3".to_owned());
            }
            if run.style.add_modifier.contains(Modifier::UNDERLINED) {
                codes.push("4".to_owned());
            }
            if run.style.add_modifier.contains(Modifier::CROSSED_OUT) {
                codes.push("9".to_owned());
            }
            if let Some(color) = run.style.fg {
                codes.push(sgr_color(color, false));
            }
            if let Some(color) = run.style.bg {
                codes.push(sgr_color(color, true));
            }
            out.push_str(&format!("\x1b[{}m{}", codes.join(";"), run.text));
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

fn sgr_color(color: Color, background: bool) -> String {
    let base = if background { 40 } else { 30 };
    let bright = if background { 100 } else { 90 };
    match color {
        Color::Reset => format!("{}", base + 9),
        Color::Black => format!("{base}"),
        Color::Red => format!("{}", base + 1),
        Color::Green => format!("{}", base + 2),
        Color::Yellow => format!("{}", base + 3),
        Color::Blue => format!("{}", base + 4),
        Color::Magenta => format!("{}", base + 5),
        Color::Cyan => format!("{}", base + 6),
        Color::Gray => format!("{}", base + 7),
        Color::DarkGray => format!("{bright}"),
        Color::LightRed => format!("{}", bright + 1),
        Color::LightGreen => format!("{}", bright + 2),
        Color::LightYellow => format!("{}", bright + 3),
        Color::LightBlue => format!("{}", bright + 4),
        Color::LightMagenta => format!("{}", bright + 5),
        Color::LightCyan => format!("{}", bright + 6),
        Color::White => format!("{}", bright + 7),
        Color::Rgb(r, g, b) => format!("{};2;{r};{g};{b}", base + 8),
        Color::Indexed(index) => format!("{};5;{index}", base + 8),
    }
}

/// A standalone page showing the frame at a fixed character grid.
///
/// Every run is an `inline-block` sized in `ch`, which pins each column even
/// when a CJK glyph and a box-drawing character come from different fallback
/// fonts. Without that, one substituted font is enough to shear the frame.
pub fn to_html(buffer: &Buffer, palette: Palette, label: &str) -> String {
    let mut body = String::new();
    for y in 0..buffer.area.height {
        body.push_str("<div class=\"r\">");
        let mut skip = 0usize;
        for x in 0..buffer.area.width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let cell = &buffer[(x, y)];
            let symbol = cell.symbol();
            let cells = symbol.width().max(1);
            skip = cells - 1;

            let style = cell_style(cell);
            let fg = palette.resolve(style.fg.unwrap_or(Color::Reset), palette.foreground);
            let mut css = format!("width:calc(var(--cw)*{cells});color:#{fg:06X}");
            if let Some(bg) = style.bg.filter(|color| *color != Color::Reset) {
                css.push_str(&format!(
                    ";background:#{:06X}",
                    palette.resolve(bg, palette.background)
                ));
            }
            for (modifier, declaration) in [
                (Modifier::BOLD, ";font-weight:700"),
                (Modifier::DIM, ";opacity:.55"),
                (Modifier::ITALIC, ";font-style:italic"),
                (Modifier::UNDERLINED, ";text-decoration:underline"),
                (Modifier::CROSSED_OUT, ";text-decoration:line-through"),
            ] {
                if style.add_modifier.contains(modifier) {
                    css.push_str(declaration);
                }
            }
            body.push_str(&format!("<i style=\"{css}\">{}</i>", escape(symbol)));
        }
        body.push_str("</div>");
    }

    format!(
        r#"<!doctype html><meta charset="utf-8"><style>
:root {{ color-scheme: {scheme}; --cw: 9.6px; --ch: 21px; }}
* {{ box-sizing: border-box; }}
body {{ margin:0; padding:22px; background:#{page:06X};
  font:16px/var(--ch) "Menlo","SF Mono","Sarasa Mono SC","Noto Sans Mono CJK SC",monospace; }}
.term {{ display:inline-block; padding:14px 16px; border-radius:8px;
  background:#{bg:06X}; color:#{fg:06X}; box-shadow:0 12px 34px rgba(0,0,0,.28); }}
.r {{ height:var(--ch); white-space:nowrap; }}
/* One box per terminal cell. Pinning every cell -- not every run -- is what
   keeps a CJK glyph from drifting off the grid when the browser substitutes
   a font whose advance is not exactly twice the Latin one. */
.r i {{ display:inline-block; width:var(--cw); height:var(--ch);
  overflow:hidden; font-style:normal; text-align:center; vertical-align:top; }}
.cap {{ margin-top:12px; font-size:12px; color:#{dim:06X}; letter-spacing:.02em; }}
</style><div class="term">{body}</div><div class="cap">{caption}</div>"#,
        scheme = if palette.background < 0x808080 { "dark" } else { "light" },
        page = if palette.background < 0x808080 { 0x101114u32 } else { 0xF2F3F5u32 },
        bg = palette.background,
        fg = palette.foreground,
        dim = palette.ansi[8],
        body = body,
        caption = escape(&format!("{label} · {} palette", palette.name)),
    )
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scene_renders_without_panicking() {
        for scene in [Scene::Live, Scene::Top, Scene::Nav, Scene::Insert, Scene::Code] {
            let buffer = capture(110, 26, scene).expect("render");
            assert!(!to_text(&buffer).trim().is_empty(), "{scene:?} drew nothing");
        }
    }

    #[test]
    fn the_top_scene_shows_what_the_live_scene_scrolled_past() {
        let top = to_text(&capture(110, 26, Scene::Top).expect("render"));
        assert!(top.contains("加入了群聊"), "system notification");
        assert!(top.contains("已读 8"), "read receipt");
        assert!(top.contains("2026-09-08"), "date divider");
    }

    #[test]
    fn the_insert_scene_activates_the_composer() {
        let frame = to_text(&capture(110, 26, Scene::Insert).expect("render"));
        assert!(frame.contains("INSERT"));
        assert!(frame.contains("回滚脚本先合"));
    }

    #[test]
    fn collapsing_a_category_in_the_nav_scene_hides_its_rows() {
        let frame = to_text(&capture(110, 26, Scene::Nav).expect("render"));
        assert!(frame.contains("▸ 研发"), "collapsed marker");
        assert!(!frame.contains("#infra"), "its conversations are folded away");
    }

    #[test]
    fn html_runs_declare_a_column_width_for_every_cell() {
        let buffer = capture(60, 6, Scene::Live).expect("render");
        for y in 0..buffer.area.height {
            let columns: usize = row_runs(&buffer, y).iter().map(|run| run.cells).sum();
            assert_eq!(columns, buffer.area.width as usize, "row {y} lost columns");
        }
        assert!(to_html(&buffer, DARK, "t").contains("width:"));
    }

    #[test]
    fn ansi_output_resets_at_the_end_of_every_row() {
        let ansi = to_ansi(&capture(40, 4, Scene::Live).expect("render"));
        assert_eq!(ansi.matches("\x1b[0m\n").count(), 4);
    }
}
