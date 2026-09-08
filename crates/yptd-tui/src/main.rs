//! yptd terminal client.
//!
//! Backed by a mock snapshot for now: the OpenIM sidecar has not landed, and
//! the renderer is better developed against a fixture that always contains the
//! awkward cases than against whatever happens to be in a dev server.

mod app;
mod layout;
mod media;
mod mouse;
mod render;
mod syntax;
mod ui;

use std::error::Error;

use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
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
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let mut clicks = mouse::Clicks::default();

    // Protocol negotiation talks to the terminal, so it happens once at
    // startup rather than on the first message that carries a picture.
    let mut media = media::Media::detect();
    load_fixture_images(&mut media, &app);

    let result = loop {
        if let Err(error) = terminal.draw(|frame| ui::draw(frame, &mut app, &mut media, &theme)) {
            break Err(error);
        }
        let area = terminal.size().map(|s| ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: s.width,
            height: s.height,
        })?;

        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                if handle_key(&mut app, key.code, key.modifiers) {
                    break Ok(());
                }
            }
            Ok(Event::Mouse(m)) => {
                let lines = app.composer.lines().count().max(1);
                let out = mouse::handle(&mut app, m, area, lines, &mut clicks);
                if out.activate {
                    app.open_selected();
                }
            }
            Ok(_) => {}
            Err(error) => break Err(error),
        }
    };

    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result.map_err(Into::into)
}

/// Registers the mock's image attachments.
///
/// The fixture ships no binary assets: the picture is generated so the repo
/// stays text-only and the rendering path still gets exercised end to end.
fn load_fixture_images(media: &mut media::Media, app: &App) {
    if !media.enabled() {
        return;
    }
    for message in &app.snapshot.messages {
        let Some(attachment) = message.attachment() else {
            continue;
        };
        if attachment.kind != im_model::AttachmentKind::Image {
            continue;
        }
        // Encode then decode, so the fixture exercises the same path a real
        // attachment will take rather than a shortcut around it.
        let decoded = media::demo_image_png(&attachment.name).and_then(|b| media::decode(&b));
        match decoded {
            Ok(image) => {
                media.insert(&attachment.name, image);
            }
            Err(reason) => media.mark_failed(&attachment.name, reason),
        }
    }
}

/// Returns true when the app should quit.
fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);

    match app.mode {
        Mode::Insert | Mode::Command => match code {
            KeyCode::Esc => {
                if app.mode == Mode::Command {
                    app.composer.clear();
                }
                app.mode = Mode::Normal;
            }
            KeyCode::Enter if app.mode == Mode::Command => {
                app.composer.clear();
                app.mode = Mode::Normal;
            }
            // Shift+Enter inserts a newline; plain Enter is reserved for send.
            KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) => app.composer.newline(),
            KeyCode::Enter => { /* send: wired up with the backend */ }
            KeyCode::Backspace if ctrl || alt => {
                app.composer.delete_word_before();
            }
            KeyCode::Backspace => {
                app.composer.backspace();
            }
            KeyCode::Delete if ctrl || alt => {
                app.composer.delete_word_after();
            }
            KeyCode::Delete => {
                app.composer.delete();
            }
            KeyCode::Left if ctrl || alt => app.composer.move_word_left(),
            KeyCode::Right if ctrl || alt => app.composer.move_word_right(),
            KeyCode::Left => app.composer.move_left(),
            KeyCode::Right => app.composer.move_right(),
            KeyCode::Up => {
                app.composer.move_vertical(-1);
            }
            KeyCode::Down => {
                app.composer.move_vertical(1);
            }
            KeyCode::Home => app.composer.move_line_start(),
            KeyCode::End => app.composer.move_line_end(),
            KeyCode::Char('a') if ctrl => app.composer.move_line_start(),
            KeyCode::Char('e') if ctrl => app.composer.move_line_end(),
            KeyCode::Char('u') if ctrl => {
                app.composer.delete_to_line_start();
            }
            KeyCode::Char('k') if ctrl => {
                app.composer.delete_to_line_end();
            }
            KeyCode::Char('w') if ctrl => {
                app.composer.delete_word_before();
            }
            KeyCode::Char(value) => app.composer.insert_char(value),
            _ => {}
        },
        Mode::Normal => match code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('i') => app.mode = Mode::Insert,
            KeyCode::Char(':') => {
                app.composer.set_text(":");
                app.mode = Mode::Command;
            }
            KeyCode::Char('1') => app.focus(Pane::Conversations),
            KeyCode::Char('2') => app.focus(Pane::Messages),
            KeyCode::Char('3') => app.focus(Pane::Members),
            KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right => app.cycle_focus(true),
            KeyCode::BackTab | KeyCode::Char('h') | KeyCode::Left => app.cycle_focus(false),
            KeyCode::Char('j') | KeyCode::Down => app.select(1),
            KeyCode::Char('k') | KeyCode::Up => app.select(-1),
            KeyCode::Char('n') if ctrl => app.select(1),
            KeyCode::Char('p') if ctrl => app.select(-1),
            KeyCode::Char('J') => app.scroll(1),
            KeyCode::Char('K') => app.scroll(-1),
            KeyCode::Char('d') if ctrl => app.scroll(5),
            KeyCode::Char('u') if ctrl => app.scroll(-5),
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
        assert_eq!(app.composer.text(), "q");
        handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn colon_opens_the_command_line_separately_from_the_composer() {
        let mut app = App::new(im_model::mock::snapshot());
        handle_key(&mut app, KeyCode::Char(':'), KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Command);
        assert_eq!(app.composer.text(), ":");
        handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.composer.is_empty());
    }
}
