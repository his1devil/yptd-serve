//! yptd terminal client.
//!
//! Backed by a mock snapshot for now: the OpenIM sidecar has not landed, and
//! the renderer is better developed against a fixture that always contains the
//! awkward cases than against whatever happens to be in a dev server.

mod app;
mod render;
mod ui;

use std::error::Error;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use tui_theme::Theme;

use crate::app::{App, Mode, Pane};
use crate::render::{Palette, Scene};

type Fallible<T> = Result<T, Box<dyn Error>>;

fn main() -> Fallible<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = args.first().map(String::as_str);
    match flag {
        Some("--snapshot") | Some("--ansi") | Some("--html") => {
            let size = args.get(1).map(String::as_str).unwrap_or("120x32");
            let scene = args
                .get(2)
                .and_then(|name| Scene::parse(name))
                .unwrap_or(Scene::Live);
            let palette = args
                .get(3)
                .and_then(|name| Palette::parse(name))
                .unwrap_or(render::DARK);
            let (width, height) = parse_size(size);
            let buffer = render::capture(width, height, scene)?;
            print!(
                "{}",
                match flag {
                    Some("--ansi") => render::to_ansi(&buffer),
                    Some("--html") => render::to_html(&buffer, palette, size),
                    _ => render::to_text(&buffer),
                }
            );
            Ok(())
        }
        Some("--help" | "-h") => {
            println!("yptd [--snapshot|--ansi|--html WxH [live|top|nav|insert|code] [dark|light]]");
            Ok(())
        }
        _ => run(),
    }
}

fn parse_size(value: &str) -> (u16, u16) {
    value
        .split_once('x')
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or((120, 32))
}

fn run() -> Fallible<()> {
    let theme = Theme::default();
    let mut app = App::new(im_model::mock::snapshot());
    let mut terminal = ratatui::init();

    let result = loop {
        if let Err(error) = terminal.draw(|frame| ui::draw(frame, &app, &theme)) {
            break Err(error);
        }
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                if handle_key(&mut app, key.code, key.modifiers) {
                    break Ok(());
                }
            }
            Ok(_) => {}
            Err(error) => break Err(error),
        }
    };

    ratatui::restore();
    result.map_err(Into::into)
}

/// Returns true when the app should quit.
fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    match app.mode {
        Mode::Insert => match code {
            KeyCode::Esc => app.mode = Mode::Normal,
            KeyCode::Backspace => {
                app.composer.pop();
            }
            KeyCode::Char(value) => app.composer.push(value),
            _ => {}
        },
        Mode::Command => match code {
            KeyCode::Esc | KeyCode::Enter => {
                app.composer.clear();
                app.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                app.composer.pop();
            }
            KeyCode::Char(value) => app.composer.push(value),
            _ => {}
        },
        Mode::Normal => match code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('i') => app.mode = Mode::Insert,
            KeyCode::Char(':') => {
                app.composer = ":".to_owned();
                app.mode = Mode::Command;
            }
            KeyCode::Char('1') => app.focus(Pane::Conversations),
            KeyCode::Char('2') => app.focus(Pane::Messages),
            KeyCode::Char('3') => app.focus(Pane::Members),
            KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right => app.cycle_focus(true),
            KeyCode::BackTab | KeyCode::Char('h') | KeyCode::Left => app.cycle_focus(false),
            KeyCode::Char('j') | KeyCode::Down => app.select(1),
            KeyCode::Char('k') | KeyCode::Up => app.select(-1),
            KeyCode::Char('n') if modifiers.contains(KeyModifiers::CONTROL) => app.select(1),
            KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => app.select(-1),
            KeyCode::Char('J') => app.scroll(1),
            KeyCode::Char('K') => app.scroll(-1),
            KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => app.scroll(5),
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => app.scroll(-5),
            KeyCode::Char('G') => app.jump_to_latest(),
            KeyCode::Char('g') => app.jump_to_top(),
            KeyCode::Char('z') => app.toggle_collapsed(),
            KeyCode::Enter => app.open_selected(),
            _ => {}
        },
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(size: &str) -> Fallible<String> {
        let (width, height) = parse_size(size);
        Ok(render::to_text(&render::capture(width, height, Scene::Live)?))
    }

    #[test]
    fn a_snapshot_renders_at_the_requested_size() {
        let out = frame("100x24").expect("render");
        assert_eq!(out.lines().count(), 24);
        assert!(out.contains("排期讨论"), "the open conversation is titled");
        assert!(out.contains("以下为未读"), "the unread divider is drawn");
    }

    #[test]
    fn a_narrow_terminal_drops_the_side_panes_before_the_message_pane() {
        // Match the pane *titles*, not bare words: "会话" also occurs inside
        // the fixture's message text, which would make this pass by accident.
        let wide = frame("120x24").expect("render");
        assert!(wide.contains("1 会话"), "conversation pane at 120 columns");
        assert!(wide.contains("3 成员"), "members pane at 120 columns");

        let medium = frame("90x24").expect("render");
        assert!(medium.contains("1 会话"), "conversation pane survives to 90");
        assert!(!medium.contains("3 成员"), "members pane drops first");

        let narrow = frame("70x24").expect("render");
        assert!(!narrow.contains("1 会话"), "conversation pane hidden at 70");
        assert!(narrow.contains("2 #排期讨论"), "the message pane always survives");
    }

    #[test]
    fn no_rendered_row_overflows_the_terminal_width() {
        for size in ["70x24", "100x24", "160x40"] {
            let width: usize = size.split_once('x').expect("size").0.parse().expect("width");
            for line in frame(size).expect("render").lines() {
                assert!(
                    unicode_width::UnicodeWidthStr::width(line) <= width,
                    "{size}: {line:?}"
                );
            }
        }
    }

    #[test]
    fn insert_mode_takes_letters_as_text_not_as_commands() {
        let mut app = App::new(im_model::mock::snapshot());
        handle_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Insert);
        let quit = handle_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(!quit, "q must not quit while typing");
        assert_eq!(app.composer, "q");
        handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn colon_opens_the_command_line_separately_from_the_composer() {
        let mut app = App::new(im_model::mock::snapshot());
        handle_key(&mut app, KeyCode::Char(':'), KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Command);
        assert_eq!(app.composer, ":");
        handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.composer.is_empty());
    }
}
