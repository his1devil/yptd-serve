//! yptd terminal client.
//!
//! `yptd login` registers this machine with an invitation code and stores a
//! device credential. Every later `yptd` exchanges that credential for an
//! OpenIM token, starts the sidecar, and drives the UI from live data. The
//! `--mock` flag and the capture modes keep the renderer testable without a
//! server.

mod app;
mod auth;
mod config;
mod layout;
mod media;
mod mouse;
mod render;
mod session;
mod syntax;
mod ui;

use std::error::Error;
use std::io::Write;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use im_model::{ConversationId, Snapshot};
use tui_theme::Theme;

use crate::app::{App, Mode, Pane};
use crate::config::Paths;
use crate::render::{Palette, Scene};
use crate::session::Session;

type Fallible<T> = Result<T, Box<dyn Error>>;

const USAGE: &str = "yptd — 终端 IM 客户端

  yptd                     启动（需先 login）
  yptd login               用邀请码登录这台机器
  yptd logout              清除本机凭据
  yptd doctor [文本]       不进界面：登录、起边车、拉会话，可选发一条消息
  yptd doctor --frame WxH  同上，但把真实数据渲染成一帧文本输出
  yptd --mock              用内置示例数据启动，不连服务器
  yptd --snapshot WxH [scene] [palette]   离屏出帧（文本）
  yptd --ansi     WxH [scene]             离屏出帧（ANSI）
  yptd --html     WxH [scene] [palette]   离屏出帧（HTML）

文件都在 ~/.yptd/（或 $YPTD_HOME）。边车二进制通过 $YPTD_SIDECAR、
yptd 同目录或 PATH 查找。
";

fn main() -> Fallible<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--snapshot") | Some("--ansi") | Some("--html") => capture(&args),
        Some("login") => cmd_login(),
        Some("logout") => cmd_logout(),
        Some("doctor") => match args.get(1).map(String::as_str) {
            Some("--frame") => cmd_doctor_frame(args.get(2).map(String::as_str).unwrap_or("120x32")),
            text => cmd_doctor(text),
        },
        Some("--mock") => run(App::new(im_model::mock::snapshot()), None),
        Some("--help" | "-h") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("未知参数 {other}\n{USAGE}").into()),
        None => cmd_start(),
    }
}

// ------------------------------------------------------------- commands ---

fn cmd_login() -> Fallible<()> {
    let paths = Paths::discover();
    let config = paths.load_config();
    if let Some(existing) = paths.load_credentials() {
        println!(
            "本机已经以 {}（{}）登录。要换账号先 yptd logout。",
            existing.nickname, existing.user_id
        );
        return Ok(());
    }

    println!("yptd 登录 · 服务端 {}", config.server);
    let invite = prompt("邀请码: ")?;
    let nickname = prompt("昵称:   ")?;
    let user_id = prompt("用户名（留空自动生成）: ")?;
    let user_id = (!user_id.is_empty()).then_some(user_id.as_str());

    print!("正在注册… ");
    std::io::stdout().flush()?;
    let login = auth::register(&config, &invite, &nickname, user_id)?;
    paths.save_credentials(&login.credentials)?;
    paths.save_config(&config)?;
    println!("完成。");
    println!();
    println!("  用户名  {}", login.credentials.user_id);
    println!("  昵称    {}", login.credentials.nickname);
    println!("  凭据    {}", paths.credentials().display());
    println!();
    println!("以后直接运行 yptd 即可。");
    Ok(())
}

fn cmd_logout() -> Fallible<()> {
    let paths = Paths::discover();
    match paths.load_credentials() {
        Some(c) => {
            paths.clear_credentials()?;
            println!("已清除 {} 的本机凭据。服务端上的账号不受影响。", c.user_id);
        }
        None => println!("本机没有凭据。"),
    }
    Ok(())
}

fn cmd_start() -> Fallible<()> {
    let (session, events, snapshot) = connect()?;
    run(live_app(snapshot), Some((session, events)))
}

/// An `App` over real data: the same as `App::new`, plus the machine's time
/// zone so timestamps read as the clock on the wall.
fn live_app(snapshot: Snapshot) -> App {
    let mut app = App::new(snapshot);
    app.utc_offset_ms = local_utc_offset_ms();
    app
}

/// Seconds east of UTC for the current moment, via `localtime_r`, which
/// honours `TZ` and daylight saving. Zero where the platform has no
/// `tm_gmtoff`.
fn local_utc_offset_ms() -> i64 {
    #[cfg(unix)]
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as libc::time_t)
            .unwrap_or(0);
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: both pointers are valid for the call; localtime_r writes
        // only into `tm`.
        if unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
            return 0;
        }
        tm.tm_gmtoff as i64 * 1000
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// The same path as a normal start, minus the terminal UI, so a broken
/// setup can be diagnosed over ssh or in a script. With a text argument it
/// also sends one message to the most recent conversation.
fn cmd_doctor(text: Option<&str>) -> Fallible<()> {
    let (mut session, events, mut snapshot) = connect()?;
    println!("会话 {} 个", snapshot.conversations.len());
    for c in snapshot.conversations.iter().take(10) {
        println!("  {:<28} {:<6} 未读 {:>3}  成员 {:>3}", c.id.0, c.name, c.unread, c.member_count);
    }
    let Some(first) = snapshot.conversations.first().map(|c| c.id.clone()) else {
        println!("没有会话；先在服务端建群或让人发消息。");
        return Ok(());
    };
    session.ensure_history(&first, &mut snapshot)?;
    session.ensure_members(&first, &mut snapshot)?;
    let msgs = snapshot.messages_in(&first);
    println!("{} 最近 {} 条，成员 {} 人", first.0, msgs.len(), snapshot.members.len());
    for m in msgs.iter().rev().take(5).rev() {
        println!("  [{}] {}: {}", m.sent_at_ms(), m.sender_name, m.text().lines().next().unwrap_or(""));
    }
    if let Some(text) = text {
        session.send_text(&first, text, &mut snapshot)?;
        println!("已发送: {text}");
    }
    // Keep listening a while so pushes from other people show up too.
    let started = Instant::now();
    let mut seen = 0;
    let window = Duration::from_secs(8);
    while let Ok(ev) = events.recv_timeout(window.saturating_sub(started.elapsed()).max(Duration::from_millis(1))) {
        seen += 1;
        let changed = session.apply(&ev, &mut snapshot);
        if changed.messages
            && let Some(m) = snapshot.messages_in(&first).last()
        {
            println!("  ← {}: {}", m.sender_name, m.text().lines().next().unwrap_or(""));
        } else if seen <= 8 {
            println!("  事件 {}", ev.name);
        }
        if started.elapsed() > window {
            break;
        }
    }
    println!("事件 {seen} 个，连接 {}，会话现在 {} 个", if snapshot.connected { "正常" } else { "断开" }, snapshot.conversations.len());
    Ok(())
}

/// A live frame as text: the real renderer over the real snapshot, without a
/// terminal. Handy for "what does it actually look like" over ssh.
fn cmd_doctor_frame(size: &str) -> Fallible<()> {
    let (mut session, _events, snapshot) = connect()?;
    let mut app = live_app(snapshot);
    let open = app.open.clone();
    session.ensure_history(&open, &mut app.snapshot)?;
    session.ensure_members(&open, &mut app.snapshot)?;
    app.jump_to_latest();
    let (width, height) = parse_size(size);
    print!("{}", render::to_text(&render::capture_app(width, height, &app)?));
    Ok(())
}

/// Credential → token → sidecar → first snapshot.
fn connect() -> Fallible<(Session, mpsc::Receiver<im_sidecar::Event>, Snapshot)> {
    let paths = Paths::discover();
    let config = paths.load_config();
    let Some(creds) = paths.load_credentials() else {
        return Err("还没登录。先运行 yptd login，或用 yptd --mock 看示例数据。".into());
    };

    eprint!("登录 {}… ", creds.user_id);
    let im_token = auth::login(&config, &creds)
        .map_err(|e| format!("{e}\n如果凭据已被吊销，运行 yptd logout 后重新 yptd login。"))?;
    eprint!("启动边车… ");
    let (mut session, events) = Session::start(&paths, &config, &creds.user_id, &im_token)?;
    eprint!("同步会话… ");
    let mut snapshot = Snapshot::default();
    let synced = session.wait_for_sync(&events, &mut snapshot, Duration::from_secs(20));
    session.bootstrap(&mut snapshot)?;
    eprintln!("{}", if synced { "好。" } else { "未等到同步完成，先用本地数据。" });
    Ok((session, events, snapshot))
}

fn prompt(label: &str) -> Fallible<String> {
    print!("{label}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

// ------------------------------------------------------------- capture ---

fn capture(args: &[String]) -> Fallible<()> {
    let flag = args.first().map(String::as_str);
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

fn parse_size(value: &str) -> (u16, u16) {
    value
        .split_once('x')
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or((120, 32))
}

// ----------------------------------------------------------- main loop ---

/// Everything the loop can be woken by, merged onto one channel so the body
/// is a plain `recv()` with no polling.
enum Input {
    Terminal(Event),
    Sidecar(im_sidecar::Event),
    /// A source closed; the loop decides whether that is fatal.
    Closed(&'static str),
}

/// How long background traffic waits before a frame, so a burst of events
/// paints once rather than once per event.
const COALESCE: Duration = Duration::from_millis(40);

fn run(mut app: App, live: Option<(Session, mpsc::Receiver<im_sidecar::Event>)>) -> Fallible<()> {
    let theme = Theme::default();
    let (tx, rx) = mpsc::channel::<Input>();

    // Terminal input on its own thread: `event::read` blocks, and the loop
    // must also wake for sidecar traffic.
    {
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("terminal-input".into())
            .spawn(move || loop {
                match event::read() {
                    Ok(ev) => {
                        if tx.send(Input::Terminal(ev)).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = tx.send(Input::Closed("terminal"));
                        break;
                    }
                }
            })?;
    }

    let mut session = match live {
        Some((session, events)) => {
            let tx = tx.clone();
            std::thread::Builder::new()
                .name("sidecar-events".into())
                .spawn(move || {
                    for ev in events {
                        if tx.send(Input::Sidecar(ev)).is_err() {
                            break;
                        }
                    }
                    let _ = tx.send(Input::Closed("sidecar"));
                })?;
            Some(session)
        }
        None => None,
    };

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let mut clicks = mouse::Clicks::default();
    // Protocol negotiation talks to the terminal, so it happens once at
    // startup rather than on the first message that carries a picture.
    let mut media = media::Media::detect();
    if session.is_none() {
        load_fixture_images(&mut media, &app);
    }

    let mut last_open: Option<ConversationId> = None;
    let mut dirty = true;
    let mut deadline: Option<Instant> = None;

    let result = loop {
        // Opening a conversation pulls its history and members lazily, once.
        if let Some(s) = session.as_mut()
            && last_open.as_ref() != Some(&app.open)
        {
            let open = app.open.clone();
            if let Err(e) = s.ensure_history(&open, &mut app.snapshot) {
                app.notice = Some(format!("加载历史失败: {e}"));
            }
            let _ = s.ensure_members(&open, &mut app.snapshot);
            app.jump_to_latest();
            last_open = Some(open);
            dirty = true;
        }

        if dirty {
            if let Err(error) = terminal.draw(|frame| ui::draw(frame, &mut app, &mut media, &theme)) {
                break Err(error.into());
            }
            dirty = false;
            deadline = None;
        }

        let area = terminal.size().map(|s| ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: s.width,
            height: s.height,
        })?;

        // Wait for input, or for a coalesced background redraw to come due.
        let input = match deadline {
            Some(when) => match rx.recv_timeout(when.saturating_duration_since(Instant::now())) {
                Ok(input) => input,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    dirty = true;
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
            },
            None => match rx.recv() {
                Ok(input) => input,
                Err(_) => break Ok(()),
            },
        };

        match input {
            Input::Terminal(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                match handle_key(&mut app, key.code, key.modifiers) {
                    KeyAction::Quit => break Ok(()),
                    KeyAction::Send => send_composer(&mut app, session.as_mut()),
                    KeyAction::None => {}
                }
                // Foreground input always paints at once; latency here is
                // what a person feels as "sluggish".
                dirty = true;
            }
            Input::Terminal(Event::Mouse(m)) => {
                let lines = app.composer.lines().count().max(1);
                let out = mouse::handle(&mut app, m, area, lines, &mut clicks);
                if out.activate {
                    app.open_selected();
                }
                dirty = out.redraw;
            }
            Input::Terminal(Event::Resize(_, _)) => dirty = true,
            Input::Terminal(_) => {}
            Input::Sidecar(ev) => {
                if let Some(s) = session.as_mut() {
                    let mut changed = s.apply(&ev, &mut app.snapshot);
                    if changed.resync {
                        match s.resync(&mut app.snapshot) {
                            Ok(()) => {
                                // Force the open conversation to refetch.
                                last_open = None;
                                changed.conversations = true;
                            }
                            Err(e) => app.notice = Some(format!("同步失败: {e}")),
                        }
                    }
                    if changed.messages && app.follow_latest {
                        app.jump_to_latest();
                    }
                    // Only schedule a frame when something on screen moved: a
                    // message in the open conversation, the sidebar, or the
                    // connection state in the status line.
                    let visible = changed.connection
                        || changed.conversations
                        || (changed.messages && event_touches_open(&ev, &app.open));
                    if visible && deadline.is_none() {
                        deadline = Some(Instant::now() + COALESCE);
                    }
                }
            }
            Input::Closed("sidecar") => {
                app.notice = Some("边车已退出，重启 yptd 重连".into());
                app.snapshot.connected = false;
                session = None;
                dirty = true;
            }
            Input::Closed(_) => break Ok(()),
        }
    };

    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

/// Sends what is in the composer, or explains why it cannot.
fn send_composer(app: &mut App, session: Option<&mut Session>) {
    let text = app.composer.text().to_owned();
    if text.trim().is_empty() {
        return;
    }
    let Some(s) = session else {
        app.notice = Some("示例模式下不能发送".into());
        return;
    };
    match s.send_text(&app.open, &text, &mut app.snapshot) {
        Ok(()) => {
            app.composer.clear();
            app.jump_to_latest();
        }
        Err(e) => app.notice = Some(format!("发送失败: {e}")),
    }
}

/// Whether an incoming-message event is for the conversation on screen. Off-
/// screen traffic still updates the model but need not repaint.
fn event_touches_open(ev: &im_sidecar::Event, open: &ConversationId) -> bool {
    let group = ev.data.get("groupID").and_then(|v| v.as_str()).unwrap_or("");
    if !group.is_empty() {
        return open.0 == format!("sg_{group}");
    }
    // Direct messages and batches: cheap to just repaint.
    true
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyAction {
    None,
    Send,
    Quit,
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> KeyAction {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);
    app.notice = None;

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
            // Shift+Enter inserts a newline; plain Enter sends.
            KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) => app.composer.newline(),
            KeyCode::Enter => return KeyAction::Send,
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
            KeyCode::Char('q') => return KeyAction::Quit,
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
    KeyAction::None
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
        let action = handle_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(action, KeyAction::None, "q must not quit while typing");
        assert_eq!(app.composer.text(), "q");
        handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn enter_in_insert_mode_asks_to_send_and_shift_enter_does_not() {
        let mut app = App::new(im_model::mock::snapshot());
        app.mode = Mode::Insert;
        app.composer.insert_str("hi");
        assert_eq!(
            handle_key(&mut app, KeyCode::Enter, KeyModifiers::SHIFT),
            KeyAction::None
        );
        assert_eq!(app.composer.text(), "hi\n", "shift-enter inserted a newline");
        assert_eq!(
            handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            KeyAction::Send
        );
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

    #[test]
    fn sending_in_mock_mode_keeps_the_draft_and_explains() {
        let mut app = App::new(im_model::mock::snapshot());
        app.composer.insert_str("hello");
        send_composer(&mut app, None);
        assert_eq!(app.composer.text(), "hello", "draft must survive a failed send");
        assert!(app.notice.is_some());
    }

    #[test]
    fn group_events_only_touch_their_own_conversation() {
        let ev = im_sidecar::Event {
            name: "OnRecvNewMessage".into(),
            data: serde_json::json!({"groupID": "42"}),
        };
        assert!(event_touches_open(&ev, &ConversationId("sg_42".into())));
        assert!(!event_touches_open(&ev, &ConversationId("sg_99".into())));
    }
}
