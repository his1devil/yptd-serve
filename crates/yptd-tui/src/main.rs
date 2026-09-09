//! yptd terminal client.
//!
//! `yptd login` registers this machine with an invitation code and stores a
//! device credential. Every later `yptd` exchanges that credential for an
//! OpenIM token, starts the sidecar, and drives the UI from live data. The
//! `--mock` flag and the capture modes keep the renderer testable without a
//! server.

mod album;
mod app;
mod attach;
mod auth;
mod browser;
mod config;
mod downloads;
mod embedded;
mod layout;
mod media;
mod mouse;
mod picker;
mod render;
mod session;
mod syntax;
mod ui;

use std::collections::HashSet;
use std::error::Error;
use std::io::Write;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use im_model::{Conversation, ConversationId, ConversationKind, Member, Role, Snapshot, UserId};
use tui_theme::Theme;

use crate::app::{App, DraftMention, HistoryState, Mode, Pane};
use crate::config::{Config, Credentials, Paths};
use crate::downloads::Downloads;
use crate::picker::{PickItem, Picker, Purpose, Verdict};
use crate::render::{Palette, Scene};
use crate::browser::{Browser, Verdict as BrowseVerdict};
use crate::session::{Pager, Session, Uploads};

type Fallible<T> = Result<T, Box<dyn Error>>;

const USAGE: &str = "yptd — 终端 IM 客户端

  yptd                     启动（需先 login）
  yptd login               用邀请码登录这台机器
  yptd logout              清除本机凭据
  yptd doctor [文本]       不进界面：登录、起边车、拉会话，可选发一条消息
  yptd doctor --frame WxH  同上，但把真实数据渲染成一帧文本输出
  yptd doctor --new 群名 [用户…]   建群并拉人，打印结果
  yptd doctor --reply 用户 文本     引用最新一条并 @ 这个人
  yptd doctor --img 路径            发一张图片，打印回显里的 URL
  yptd doctor --media               终端的图形能力与图片实际渲染尺寸
  yptd --mock              用内置示例数据启动，不连服务器
  yptd --snapshot WxH [scene] [palette]   离屏出帧（文本）
  yptd --ansi     WxH [scene]             离屏出帧（ANSI）
  yptd --html     WxH [scene] [palette]   离屏出帧（HTML）

界面里：r 引用选中的消息，输入时 @ 提及群成员，把图片文件拖进窗口即可附带，
: 输命令：:new 群名 · :invite [用户…] · :dm [用户] · :img [路径] · :help · :q

文件都在 ~/.yptd/（或 $YPTD_HOME）。边车二进制通过 $YPTD_SIDECAR、
yptd 同目录或 PATH 查找。
";

const HELP_LINE: &str =
    ":new 群名 · :invite [用户…] · :dm [用户] · :img [路径] 选图 · :avatar 路径 · :q 退出";

/// Shown wherever an action needs a conversation and there is none. Names the
/// two commands that create one, because a new account starts here.
const NO_CONVERSATION: &str = "还没有会话。按 : 然后 :new 群名 建群，或 :dm 找人私聊";

fn main() -> Fallible<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--snapshot") | Some("--ansi") | Some("--html") => capture(&args),
        Some("login") => cmd_login(),
        Some("logout") => cmd_logout(),
        Some("doctor") => match args.get(1).map(String::as_str) {
            Some("--frame") => cmd_doctor_frame(args.get(2).map(String::as_str).unwrap_or("120x32")),
            Some("--new") => cmd_doctor_new(args.get(2).map(String::as_str), args.get(3..).unwrap_or(&[])),
            Some("--reply") => cmd_doctor_reply(args.get(2).map(String::as_str), args.get(3..).unwrap_or(&[])),
            Some("--img") => cmd_doctor_image(args.get(2).map(String::as_str)),
            Some("--media") => cmd_doctor_media(),
            Some("--avatar") => cmd_doctor_avatar(args.get(2).map(String::as_str)),
            text => cmd_doctor(text),
        },
        Some("--mock") => run(App::new(im_model::mock::snapshot()), false),
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
    // The one thing worth failing on before the interface comes up: without
    // credentials there is nothing to connect to, and the hint belongs on a
    // plain terminal.
    if Paths::discover().load_credentials().is_none() {
        return Err(NOT_LOGGED_IN.into());
    }
    let mut app = live_app(Snapshot::default());
    app.connecting = true;
    run(app, true)
}

const NOT_LOGGED_IN: &str = "还没登录。先运行 yptd login，或用 yptd --mock 看示例数据。";

/// The same path as a normal start, minus the terminal UI, so a broken
/// setup can be diagnosed over ssh or in a script. With a text argument it
/// also sends one message to the most recent conversation.
fn cmd_doctor(text: Option<&str>) -> Fallible<()> {
    println!(
        "边车 {}",
        if embedded::is_embedded() {
            "内置，首次运行解到 ~/.yptd/bin"
        } else {
            "外置，在同目录或 PATH 上找"
        }
    );
    let Live { mut backend, events, mut snapshot } = connect()?;
    let session = &mut backend.session;
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
    session.mark_read(&first, &mut snapshot);
    let msgs = snapshot.messages_in(&first);
    println!("{} 最近 {} 条，成员 {} 人", first.0, msgs.len(), snapshot.members.len());
    for m in msgs.iter().rev().take(5).rev() {
        println!("  [{}] {}: {}", m.sent_at_ms(), m.sender_name, summarize(m));
    }
    if let Some(text) = text {
        session.send_text(&first, text, None, &[], &mut snapshot)?;
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

/// Sets this account's avatar and reads back what the server now reports, so
/// the round trip is visible without opening the interface.
fn cmd_doctor_avatar(path: Option<&str>) -> Fallible<()> {
    let Some(path) = path.filter(|p| !p.is_empty()) else {
        return Err("用法: yptd doctor --avatar 图片路径".into());
    };
    let path = resolve_path(path)?;
    let Live { mut backend, mut snapshot, .. } = connect()?;
    let url = backend.session.set_avatar(&path.to_string_lossy())?;
    println!("已上传 {}", path.display());
    println!("头像地址 {url}");

    // Read it back from a group's member list, which is where every client
    // draws avatars from.
    let group = snapshot
        .conversations
        .iter()
        .map(|c| c.id.clone())
        .find(|id| id.0.starts_with("sg_"));
    if let Some(group) = group {
        backend.session.forget_members();
        backend.session.ensure_members(&group, &mut snapshot)?;
        for m in snapshot.members.iter() {
            println!(
                "  {:<16} {}",
                m.name,
                m.avatar.as_deref().unwrap_or("（没有头像）")
            );
        }
    }
    Ok(())
}

/// A live frame as text: the real renderer over the real snapshot, without a
/// terminal. Handy for "what does it actually look like" over ssh.
fn cmd_doctor_frame(size: &str) -> Fallible<()> {
    let Live { mut backend, snapshot, events: _events } = connect()?;
    let mut app = live_app(snapshot);
    if app.has_open_conversation() {
        let open = app.open.clone();
        backend.session.ensure_history(&open, &mut app.snapshot)?;
        backend.session.ensure_members(&open, &mut app.snapshot)?;
        app.jump_to_latest();
        if !backend.session.has_older(&open) {
            app.history = HistoryState::Exhausted;
        }
    }
    let (width, height) = parse_size(size);
    print!("{}", render::to_text(&render::capture_app(width, height, &app)?));
    Ok(())
}

/// Creates a group and invites people to it, the way `:new` does in the UI,
/// then reads the membership back so the round trip is visible.
fn cmd_doctor_new(name: Option<&str>, invitees: &[String]) -> Fallible<()> {
    let Some(name) = name.filter(|n| !n.trim().is_empty()) else {
        return Err("用法: yptd doctor --new 群名 [用户…]".into());
    };
    let Live { mut backend, mut snapshot, events } = connect()?;
    let conv = backend.session.create_group(name, &mut snapshot)?;
    println!("已建群 {name} → {}", conv.0);
    let group_id = conv.0.trim_start_matches("sg_").to_owned();
    if !invitees.is_empty() {
        backend.session.invite(&group_id, invitees)?;
        println!("已邀请 {}", invitees.join("、"));
    }
    let roster = auth::users(&backend.config, &backend.creds)?;
    let listed: Vec<String> = roster.iter().map(|u| format!("{}({})", u.nickname, u.user_id)).collect();
    println!("花名册 {} 人：{}", roster.len(), listed.join(" "));
    // Membership lands as SDK events after the call returns; show them, then
    // read the list back.
    let started = Instant::now();
    while let Ok(ev) = events.recv_timeout(Duration::from_millis(1200)) {
        println!("  事件 {}", ev.name);
        backend.session.apply(&ev, &mut snapshot);
        if started.elapsed() > Duration::from_secs(4) {
            break;
        }
    }
    backend.session.ensure_members(&conv, &mut snapshot)?;
    let names: Vec<String> = snapshot.members.iter().map(|m| format!("{}[{}]", m.name, m.role.label())).collect();
    println!("成员 {} 人：{}", names.len(), names.join(" "));
    Ok(())
}

/// Quotes the newest message in the newest conversation and mentions
/// somebody, which is the one path that exercises at-text, the nested quote
/// and the message lookup all at once.
fn cmd_doctor_reply(user_id: Option<&str>, words: &[String]) -> Fallible<()> {
    let Some(user_id) = user_id.filter(|u| !u.is_empty()) else {
        return Err("用法: yptd doctor --reply 用户 文本".into());
    };
    let Live { mut backend, mut snapshot, .. } = connect()?;
    // Newest first, but a group where nobody has said anything yet has only
    // system notices, and those cannot be quoted.
    let ids: Vec<ConversationId> = snapshot.conversations.iter().map(|c| c.id.clone()).collect();
    let mut found = None;
    for id in ids {
        backend.session.ensure_history(&id, &mut snapshot)?;
        if let Some(m) = snapshot.messages_in(&id).into_iter().rev().find(|m| !m.is_system()) {
            found = Some((id.clone(), m.id, m.sender_name.clone(), m.text().to_owned()));
            break;
        }
    }
    let Some((conv, target_id, target_sender, target_text)) = found else {
        return Err("没有可引用的消息".into());
    };
    backend.session.ensure_members(&conv, &mut snapshot)?;

    // Mirror what the `@` list puts in the text, including the reserved
    // "everyone" id, which has no member row to read a name from.
    let nickname = if user_id == backend.session.at_all_tag() {
        "全体成员".to_owned()
    } else {
        snapshot
            .members
            .iter()
            .find(|m| m.id.0 == user_id)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| user_id.to_owned())
    };
    let body = words.join(" ");
    let text = format!("@{nickname} {}", if body.is_empty() { "看下这条" } else { &body });
    let mentions = [DraftMention { user_id: user_id.to_owned(), nickname }];

    println!("在 {} 里引用 {} 的「{}」", conv.0, target_sender, target_text.lines().next().unwrap_or(""));
    backend
        .session
        .send_text(&conv, &text, Some(target_id), &mentions, &mut snapshot)?;
    println!("已发送: {text}");

    let sent = snapshot
        .messages_in(&conv)
        .into_iter()
        .next_back()
        .ok_or("发出去的消息没有回到快照里")?;
    println!("回显: {}", sent.text());
    match &sent.quote {
        Some(q) => println!("  引用 → {}: {}", q.sender_name, q.summary),
        None => println!("  ✗ 回显里没有引用"),
    }
    if sent.mentions.is_empty() {
        println!("  ✗ 回显里没有提及");
    }
    for m in &sent.mentions {
        println!("  提及 → {} [{}..{}] 通知我={}", m.user.0, m.start, m.end, m.notifies_me);
    }
    Ok(())
}

/// Sends one picture and prints what came back, so the upload and the URL
/// the object store hands out can be checked without a terminal.
fn cmd_doctor_image(path: Option<&str>) -> Fallible<()> {
    let Some(path) = path.filter(|p| !p.is_empty()) else {
        return Err("用法: yptd doctor --img 图片路径".into());
    };
    let path = resolve_path(path)?;
    let Live { mut backend, mut snapshot, .. } = connect()?;
    let Some(conv) = snapshot.conversations.first().map(|c| c.id.clone()) else {
        return Err("没有会话可发".into());
    };
    println!("发送 {} 到 {}", path.display(), conv.0);
    backend.session.send_image(&conv, &path, &mut snapshot)?;
    let sent = snapshot
        .messages_in(&conv)
        .into_iter()
        .next_back()
        .ok_or("发出去的图片没有回到快照里")?;
    let Some(a) = sent.attachment() else {
        println!("  ✗ 回显不是附件消息");
        return Ok(());
    };
    println!("回显: {} {} 字节", a.name, a.bytes);
    if a.url.is_empty() {
        println!("  ✗ 回显里没有 URL，别人下不到这张图");
        return Ok(());
    }
    println!("  URL: {}", a.url);

    // Fetch it back the way another client would, so the round trip -- not
    // just the upload -- is what gets checked.
    let cache = Paths::discover().cache_dir();
    let bytes = downloads::fetch(&cache, &a.url)?;
    let (image, natural) = media::decode_for_display(&bytes)?;
    println!(
        "  取回 {} 字节，原图 {}x{}，存为 {}x{}",
        bytes.len(),
        natural.0,
        natural.1,
        image.width(),
        image.height()
    );
    println!("  缓存在 {}", cache.display());
    Ok(())
}

/// What this terminal can do with pictures, and how many pixels one actually
/// gets. Needs a real terminal: the numbers come from asking it.
fn cmd_doctor_media() -> Fallible<()> {
    let media = media::Media::detect();
    println!("{}", media.report());
    if !media.enabled() {
        println!();
        println!("Kitty、Ghostty、iTerm2、WezTerm 支持；Terminal.app 不支持。");
        return Ok(());
    }
    println!();
    println!("像素数越大越清晰。如果这些数字明显小于你屏幕上那块区域的实际像素，");
    println!("说明终端报的是逻辑像素而不是设备像素，图会被拉伸。");
    Ok(())
}

/// The SDK session plus the server credentials, which the roster endpoint
/// wants. Everything the live loop needs besides the UI.
struct Backend {
    session: Session,
    config: Config,
    creds: Credentials,
}

struct Live {
    backend: Backend,
    events: mpsc::Receiver<im_sidecar::Event>,
    snapshot: Snapshot,
}

/// Credential → token → sidecar → first snapshot, with a progress line on
/// stderr for the diagnostic commands that run on a plain terminal.
fn connect() -> Fallible<Live> {
    connect_with(true)
}

fn connect_with(progress: bool) -> Fallible<Live> {
    let paths = Paths::discover();
    let config = paths.load_config();
    let Some(creds) = paths.load_credentials() else {
        return Err(NOT_LOGGED_IN.into());
    };

    // One line, not a running commentary: which parts of the machinery are
    // starting is the client's business, not the reader's.
    if progress {
        eprint!("连接中…");
    }
    let im_token = auth::login(&config, &creds)
        .map_err(|e| format!("{e}\n如果凭据已被吊销，运行 yptd logout 后重新 yptd login。"))?;
    let (mut session, events) =
        Session::start(&paths, &config, &creds.user_id, &creds.nickname, &im_token)?;
    let mut snapshot = Snapshot::default();
    // A session taken over from an earlier run has already synced; waiting
    // would only stall the start for a sync that is never going to be
    // announced again. A fresh one gets a short wait for the conversation
    // list: whatever is still in flight after that is folded in when its
    // event lands, rather than holding the first frame for it.
    if !session.resumed() {
        session.wait_for_sync(&events, &mut snapshot, Duration::from_secs(3));
    }
    session.bootstrap(&mut snapshot)?;
    if progress {
        // Erase the line so the terminal is clean when the interface takes over.
        eprint!("\r          \r");
    }
    Ok(Live {
        backend: Backend { session, config, creds },
        events,
        snapshot,
    })
}

/// An `App` over real data: the same as `App::new`, plus the machine's time
/// zone so timestamps read as the clock on the wall.
fn live_app(snapshot: Snapshot) -> App {
    let mut app = App::new(snapshot);
    app.utc_offset_ms = local_utc_offset_ms();
    app.now_ms = now_ms();
    app
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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
    /// A picture finished downloading: its cache key, and the bytes or the
    /// reason there are none.
    Image(downloads::Fetched),
    /// A picture finished encoding for the terminal at one size.
    Encoded(media::Encoded),
    /// A page of older history arrived.
    History(session::Page),
    /// The session came up behind the interface, or could not.
    Connected(Result<Live, String>),
    /// A staged picture finished uploading, carrying the SDK's echo of the
    /// message it became.
    Sent(Result<serde_json::Value, im_sidecar::Error>),
    /// A source closed; the loop decides whether that is fatal.
    Closed(&'static str),
}

/// How long background traffic waits before a frame, so a burst of events
/// paints once rather than once per event.
const COALESCE: Duration = Duration::from_millis(40);

fn run(mut app: App, connect: bool) -> Fallible<()> {
    let theme = Theme::default();
    let (tx, rx) = mpsc::channel::<Input>();
    let cache = Paths::discover().cache_dir();

    let mut terminal = ratatui::init();
    // Bracketed paste is what turns a dragged file into one event instead of
    // a burst of keystrokes that would land in the composer character by
    // character.
    execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;

    // Ask the terminal what it can do BEFORE anything else reads stdin. The
    // query is a round trip on the same stream the input thread reads; start
    // that thread first and it swallows the terminal's answer, the picker
    // times out, and every picture degrades to half-block characters drawn
    // with a guessed cell size -- which is what "blurry and laggy" looks
    // like from the outside.
    let mut media = media::Media::detect();
    if media.enabled() {
        media.start_encoder(tx.clone(), Input::Encoded);
    }
    // Syntax highlighting loads its grammars on first use, a few hundred
    // milliseconds; take that hit here instead of on the first frame that
    // has a code block in it.
    let _ = std::thread::Builder::new()
        .name("syntax-warm".into())
        .spawn(syntax::warm);

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

    // Logging in, starting the sidecar and syncing take a second or two;
    // the interface comes up at once and says "connecting" until then.
    if connect {
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("connect".into())
            .spawn(move || {
                let outcome = connect_with(false).map_err(|e| e.to_string());
                let _ = tx.send(Input::Connected(outcome));
            })?;
    } else {
        load_fixture_images(&mut media, &app);
    }
    let mut backend: Option<Backend> = None;

    let mut clicks = mouse::Clicks::default();
    let mut layout_cache = ui::Cache::default();
    // The workers start with the session; until then there is nothing to
    // fetch, send or page.
    let mut downloads = Downloads::disabled();
    let mut uploads = Uploads::disabled();
    let mut pager = Pager::disabled();

    let mut last_open: Option<ConversationId> = None;
    let mut dirty = true;
    let mut deadline: Option<Instant> = None;

    let result = loop {
        // Opening a conversation pulls its history and members lazily, once.
        if let Some(b) = backend.as_mut()
            && last_open.as_ref() != Some(&app.open)
            // A fresh account has no conversation open, and asking the SDK
            // for the history of nothing only produces an error toast.
            && app.has_open_conversation()
        {
            let open = app.open.clone();
            if let Err(e) = b.session.ensure_history(&open, &mut app.snapshot) {
                app.notice = Some(format!("加载历史失败: {e}"));
            }
            let _ = b.session.ensure_members(&open, &mut app.snapshot);
            b.session.mark_read(&open, &mut app.snapshot);
            app.jump_to_latest();
            last_open = Some(open);
            dirty = true;
        }

        // Ask for any picture in this conversation we have not fetched yet.
        // Doing it here rather than on arrival covers history too, and the
        // request set makes repeats free.
        // Somebody in the member list who has not spoken has no message to
        // carry their picture, and the roster draws them all.
        for url in app
            .snapshot
            .members
            .iter()
            .filter_map(|m| m.avatar.clone())
            .collect::<Vec<_>>()
        {
            if media.failure(&url).is_none() && !media.holds(&url) {
                downloads.request(&url, &url);
            }
        }
        for message in app.messages() {
            // The sender's picture, if they have set one. Same worker and the
            // same cache as attachments -- an avatar is just a small image.
            if let Some(url) = message.sender_avatar.as_deref()
                && media.failure(url).is_none()
                && !media.holds(url)
            {
                downloads.request(url, url);
            }
            if let Some(a) = message.attachment()
                && a.kind == im_model::AttachmentKind::Image
                && (media.rows_for(a.key()) == 0 || !media.holds(a.key()))
                && media.failure(a.key()).is_none()
            {
                downloads.request(a.key(), &a.url);
            }
        }

        if dirty {
            // Kept current so a conversation left open across midnight starts
            // calling yesterday "昨天" rather than "今天".
            app.now_ms = now_ms();
            if let Err(error) =
                terminal.draw(|frame| ui::draw(frame, &mut app, &mut media, &theme, &mut layout_cache))
            {
                break Err(error.into());
            }
            dirty = false;
            deadline = None;

            // Reaching the top is the signal to fetch the page before it, off
            // the drawing thread. The notice row at the top changes to say so.
            if let Some(b) = backend.as_mut() {
                let open = app.open.clone();
                let state = if !b.session.has_older(&open) {
                    HistoryState::Exhausted
                } else if pager.is_loading(&open) {
                    HistoryState::Loading
                } else if app.at_oldest()
                    && let Some(oldest) = b.session.oldest_client_id(&open, &app.snapshot)
                    && pager.request(&open, oldest)
                {
                    HistoryState::Loading
                } else {
                    HistoryState::MayHaveMore
                };
                if app.history != state {
                    app.history = state;
                    dirty = true;
                }
            }
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
                let flow = on_key(
                    &mut app,
                    backend.as_mut(),
                    &mut uploads,
                    key.code,
                    key.modifiers,
                );
                if let Flow::Quit = flow {
                    break Ok(());
                }
                // Foreground input always paints at once; latency here is
                // what a person feels as "sluggish".
                dirty = true;
            }
            Input::Terminal(Event::Mouse(m)) => {
                // A modal owns the screen; clicks underneath it do nothing.
                if app.picker.is_none() && app.browser.is_none() {
                    let rows = app.composer_rows();
                    let out = mouse::handle(&mut app, m, area, rows, &mut clicks);
                    if out.activate {
                        app.open_selected();
                    }
                    dirty = out.redraw;
                }
            }
            Input::Terminal(Event::Paste(text)) => {
                on_paste(&mut app, &text);
                dirty = true;
            }
            Input::Terminal(Event::Resize(_, _)) => {
                dirty = true;
            }
            Input::Terminal(_) => {}
            Input::Sent(outcome) => {
                let left = uploads.finished();
                match outcome {
                    Ok(sent) => {
                        if let Some(b) = backend.as_mut() {
                            b.session.absorb(sent, &mut app.snapshot);
                        }
                        app.notice = (left > 0).then(|| format!("还有 {left} 张在发"));
                        if app.follow_latest {
                            app.jump_to_latest();
                        }
                    }
                    Err(e) => app.notice = Some(format!("图片发送失败: {e}")),
                }
                dirty = true;
            }
            Input::Sidecar(ev) => {
                if let Some(b) = backend.as_mut() {
                    let mut changed = b.session.apply(&ev, &mut app.snapshot);
                    if changed.resync {
                        match b.session.resync(&mut app.snapshot) {
                            Ok(()) => {
                                // Force the open conversation to refetch.
                                last_open = None;
                                changed.conversations = true;
                            }
                            Err(e) => app.notice = Some(format!("同步失败: {e}")),
                        }
                    }
                    if changed.members {
                        let _ = b.session.ensure_members(&app.open, &mut app.snapshot);
                        changed.conversations = true;
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
            Input::Connected(Err(e)) => break Err(e.into()),
            Input::Connected(Ok(live)) => {
                let Live { backend: b, events, snapshot } = live;
                {
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
                }
                // Nothing to show without a graphics protocol, so the
                // download worker never starts without one.
                if media.enabled() {
                    downloads = Downloads::start(cache.clone(), tx.clone(), Input::Image);
                }
                uploads = b.session.uploads(tx.clone(), Input::Sent);
                pager = b.session.pager(tx.clone(), Input::History);
                backend = Some(b);
                app = live_app(snapshot);
                last_open = None;
                dirty = true;
            }
            Input::History(page) => {
                match &page.reply {
                    Ok(_) => pager.finished(&page.conversation),
                    Err(e) => {
                        pager.failed(&page.conversation);
                        app.notice = Some(format!("加载更早的消息失败: {e}"));
                    }
                }
                if let Some(b) = backend.as_mut() {
                    // Older messages go in above; every index shifts. Keep the
                    // viewport and the selection on the messages they were on
                    // by remembering which ones those were.
                    let anchor = app.messages().get(app.message_scroll).map(|m| m.id);
                    let selected = app.selected_message().map(|m| m.id);
                    let added = b.session.absorb_page(&page, &mut app.snapshot);
                    if added > 0 && page.conversation == app.open {
                        let ids: Vec<_> = app.messages().iter().map(|m| m.id).collect();
                        let find = |id| ids.iter().position(|m| *m == id);
                        if let Some(index) = anchor.and_then(find) {
                            app.message_scroll = index;
                        }
                        if let Some(index) = selected.and_then(find) {
                            app.message_cursor = index;
                            app.shown_cursor = Some(index);
                        }
                    }
                }
                dirty = true;
            }
            Input::Encoded(done) => {
                media.store_encoded(done);
                if deadline.is_none() {
                    deadline = Some(Instant::now() + COALESCE);
                }
            }
            Input::Image((key, outcome)) => {
                downloads.delivered(&key);
                match outcome {
                    Ok((image, natural)) => {
                        media.insert(&key, image, natural);
                    }
                    Err(reason) => media.mark_failed(&key, reason),
                }
                // A picture changes how tall its message is, so this is a
                // layout change, not just a repaint.
                if app.follow_latest {
                    app.jump_to_latest();
                }
                if deadline.is_none() {
                    deadline = Some(Instant::now() + COALESCE);
                }
            }
            Input::Closed("sidecar") => {
                app.notice = Some("边车已退出，重启 yptd 重连".into());
                app.snapshot.connected = false;
                backend = None;
                dirty = true;
            }
            Input::Closed(_) => break Ok(()),
        }
    };

    let _ = execute!(std::io::stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
    result
}

/// One line describing a message for the diagnostic output. An attachment
/// has no text of its own, and an empty line tells the reader nothing.
fn summarize(message: &im_model::Message) -> String {
    match message.attachment() {
        Some(a) if message.text().is_empty() => format!("{} {}", a.kind.glyph(), a.name),
        _ => message.text().lines().next().unwrap_or("").to_owned(),
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
        let decoded =
            media::demo_image_png(&attachment.name).and_then(|b| media::decode_for_display(&b));
        match decoded {
            Ok((image, natural)) => {
                media.insert(&attachment.name, image, natural);
            }
            Err(reason) => media.mark_failed(&attachment.name, reason),
        }
    }
}

// ---------------------------------------------------------------- keys ---

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flow {
    Continue,
    Quit,
}

/// One keypress, routed: the modal first if one is open, then the mode map.
fn on_key(
    app: &mut App,
    mut backend: Option<&mut Backend>,
    uploads: &mut Uploads,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Flow {
    if let Some(browser) = app.browser.as_mut() {
        match browser.handle_key(code, modifiers) {
            BrowseVerdict::Continue => {}
            BrowseVerdict::Cancel => {
                app.last_dir = Some(browser.dir().to_path_buf());
                app.browser = None;
            }
            BrowseVerdict::Confirm(paths) => {
                app.last_dir = Some(browser.dir().to_path_buf());
                app.browser = None;
                let added = app.attach(paths);
                app.mode = Mode::Insert;
                app.notice = Some(format!("已选 {added} 张，Enter 发送，Esc 取消"));
            }
        }
        return Flow::Continue;
    }
    if let Some(picker) = app.picker.as_mut() {
        match picker.handle_key(code, modifiers) {
            Verdict::Continue => {}
            Verdict::Cancel => app.picker = None,
            Verdict::Confirm(ids) => {
                let picker = app.picker.take().expect("picker is open");
                complete_pick(app, backend, picker, ids);
            }
        }
        return Flow::Continue;
    }
    match handle_key(app, code, modifiers) {
        KeyAction::Quit => Flow::Quit,
        KeyAction::Send => {
            send_composer(app, backend.map(|b| &mut b.session), uploads);
            Flow::Continue
        }
        KeyAction::Command(line) => execute_command(app, backend, &line),
        KeyAction::Mention => {
            let tag = backend
                .as_deref_mut()
                .map(|b| b.session.at_all_tag().to_owned())
                .unwrap_or_else(|| im_model::AT_ALL_TAG.to_owned());
            open_mention_picker(app, &tag);
            Flow::Continue
        }
        KeyAction::Emoji => {
            app.picker = Some(Picker::new(
                "表情",
                Purpose::Emoji,
                picker::emoji_rows(),
                false,
            ));
            Flow::Continue
        }
        KeyAction::Reply => {
            if !app.begin_reply() {
                app.notice = Some("这条消息不能引用".into());
            }
            Flow::Continue
        }
        KeyAction::None => Flow::Continue,
    }
}

/// Sends what is in the composer, or explains why it cannot.
///
/// Text first, then the staged pictures: OpenIM has no message that carries
/// both, and a caption reads as a caption only when it arrives before what it
/// captions.
fn send_composer(app: &mut App, session: Option<&mut Session>, uploads: &mut Uploads) {
    let text = app.composer.text().to_owned();
    let has_text = !text.trim().is_empty();
    if !has_text && app.pending.is_empty() {
        return;
    }
    if !app.has_open_conversation() {
        app.notice = Some(NO_CONVERSATION.into());
        return;
    }
    let Some(s) = session else {
        app.notice = Some("示例模式下不能发送".into());
        return;
    };

    if has_text {
        let mentions = app::live_mentions(&text, &app.draft_mentions);
        if let Err(e) = s.send_text(&app.open, &text, app.reply_to, &mentions, &mut app.snapshot) {
            app.notice = Some(format!("发送失败: {e}"));
            return;
        }
        app.composer.clear();
    }

    let staged = std::mem::take(&mut app.pending);
    let count = staged.len();
    for path in staged {
        if !uploads.queue(&app.open, path) {
            app.notice = Some("发不了图片：上传队列没起来".into());
            break;
        }
    }
    if count > 0 {
        app.notice = Some(format!("正在发送 {count} 张图片…"));
    }
    app.reply_to = None;
    app.draft_mentions.clear();
    app.jump_to_latest();
}

/// A paste is either files to attach or text to type.
fn on_paste(app: &mut App, text: &str) {
    app.notice = None;
    if let Some(paths) = attach::parse_paths(text) {
        let images = paths.len();
        let added = app.attach(paths);
        app.mode = Mode::Insert;
        app.notice = Some(if added == images {
            format!("已附带 {added} 个文件，Enter 发送")
        } else {
            format!("已附带 {added} 个，其余重复或超过上限（共 {images} 个）")
        });
        return;
    }
    if app.mode == Mode::Normal {
        return;
    }
    // Ordinary text goes in at the cursor, newlines and all.
    app.composer.insert_str(text);
}

/// Opens the `@` list: the people in this conversation, plus everyone at once
/// when it is a group. The roster is deliberately not used here -- mentioning
/// somebody who cannot see the message notifies nobody.
fn open_mention_picker(app: &mut App, at_all_tag: &str) {
    let me = me_id(app);
    let mut items: Vec<PickItem> = Vec::new();
    if app.conversation().map(|c| c.kind) == Some(ConversationKind::Group)
        && app.snapshot.members.len() > 1
    {
        items.push(PickItem {
            id: at_all_tag.to_owned(),
            label: "全体成员".into(),
            detail: "提醒所有人".into(),
        });
    }
    items.extend(
        app.snapshot
            .members
            .iter()
            .filter(|m| m.id.0 != me)
            .map(|m| PickItem {
                id: m.id.0.clone(),
                label: m.name.clone(),
                detail: m.id.0.clone(),
            }),
    );
    if items.is_empty() {
        app.notice = Some("这个会话里没有别人可以提及".into());
        return;
    }
    // The at-all row is first by intent, not by id, so keep the order given.
    app.picker = Some(Picker::ordered("提及谁", Purpose::Mention, items, false));
}

/// Finishes an `@`: the `@` is already in the text, so only the name and a
/// separating space go in.
fn insert_mention(app: &mut App, id: &str, nickname: &str) {
    app.composer.insert_str(nickname);
    app.composer.insert_char(' ');
    app.draft_mentions.push(DraftMention {
        user_id: id.to_owned(),
        nickname: nickname.to_owned(),
    });
}

// ------------------------------------------------------------ commands ---

/// Runs one `:` command line. Verbs are short and few on purpose: a person
/// types these while talking, not while configuring.
fn execute_command(app: &mut App, mut backend: Option<&mut Backend>, line: &str) -> Flow {
    let mut words = line.split_whitespace();
    let Some(verb) = words.next() else {
        return Flow::Continue;
    };
    let rest: Vec<String> = words.map(str::to_owned).collect();
    match verb {
        "q" | "quit" => return Flow::Quit,
        "new" | "group" => {
            let name = rest.join(" ");
            if name.is_empty() {
                app.notice = Some("用法: :new 群名".into());
                return Flow::Continue;
            }
            match backend {
                Some(b) => match b.session.create_group(&name, &mut app.snapshot) {
                    Ok(conv) => {
                        app.open_conversation(conv);
                        open_invite_picker(app, Some(b));
                    }
                    Err(e) => app.notice = Some(format!("建群失败: {e}")),
                },
                None => {
                    let conv = mock_group(app, &name);
                    app.open_conversation(conv);
                    open_invite_picker(app, None);
                }
            }
        }
        "invite" => {
            if rest.is_empty() {
                open_invite_picker(app, backend);
            } else {
                let Some(group_id) = group_of(&app.open) else {
                    app.notice = Some("当前不是群聊，先 :new 建一个".into());
                    return Flow::Continue;
                };
                let names = rest.clone();
                invite_users(app, backend, &group_id, rest, names);
            }
        }
        "dm" => match rest.first() {
            None => open_dm_picker(app, backend),
            Some(user_id) => {
                let nickname = roster(backend.as_deref_mut())
                    .ok()
                    .and_then(|r| r.into_iter().find(|i| &i.id == user_id).map(|i| i.label))
                    .unwrap_or_default();
                open_direct(app, backend, user_id, &nickname);
            }
        },
        "img" | "image" => {
            if rest.is_empty() {
                open_browser(app);
            } else {
                stage_path(app, &rest.join(" "));
            }
        }
        "avatar" => {
            let raw = rest.join(" ");
            if raw.is_empty() {
                app.notice = Some("用法: :avatar 图片路径".into());
                return Flow::Continue;
            }
            match (resolve_path(&raw), backend) {
                (Err(e), _) => app.notice = Some(e),
                (Ok(_), None) => app.notice = Some("示例数据里改不了头像".into()),
                (Ok(path), Some(b)) => {
                    // Blocking: setting an avatar is a rare, deliberate act,
                    // and the picture is small. Worth a moment's pause to
                    // keep the two steps together and report one outcome.
                    match b.session.set_avatar(&path.to_string_lossy()) {
                        Ok(_) => {
                            // The member list carries what everyone sees, so
                            // pull it again rather than guess.
                            b.session.forget_members();
                            let _ = b.session.ensure_members(&app.open, &mut app.snapshot);
                            app.notice = Some("头像已更新".into());
                        }
                        Err(e) => app.notice = Some(format!("设置头像失败: {e}")),
                    }
                }
            }
        }
        "help" | "h" => app.notice = Some(HELP_LINE.into()),
        other => app.notice = Some(format!("未知命令 :{other}，:help 看列表")),
    }
    Flow::Continue
}

/// Stages a picture named on the command line. Like a drag, it waits for
/// Enter rather than going out immediately, so several can go together.
fn stage_path(app: &mut App, raw: &str) {
    match resolve_path(raw) {
        Ok(path) => {
            let name = file_label(&path);
            if app.attach([path]) == 0 {
                app.notice = Some(format!("{name} 已经在待发列表里"));
            } else {
                app.mode = Mode::Insert;
                app.notice = Some(format!("已附带 {name}，Enter 发送"));
            }
        }
        Err(e) => app.notice = Some(e),
    }
}

/// Opens the file browser where it was last left.
fn open_browser(app: &mut App) {
    app.browser = Some(Browser::open(app.last_dir.as_deref()));
}

/// Turns what somebody typed into a path the SDK will accept: `~` expanded,
/// relative paths resolved, and the file confirmed to exist before the send
/// so the failure names the path instead of an SDK error code.
fn resolve_path(raw: &str) -> Result<std::path::PathBuf, String> {
    let trimmed = raw.trim().trim_matches('\'').trim_matches('"');
    let expanded = match trimmed.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .ok_or_else(|| "找不到 home 目录".to_owned())?
            .join(rest),
        None => std::path::PathBuf::from(trimmed),
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .map_err(|e| format!("取当前目录失败: {e}"))?
            .join(expanded)
    };
    if !absolute.is_file() {
        return Err(format!("找不到文件: {}", absolute.display()));
    }
    Ok(absolute)
}

fn file_label(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Whether the cursor sits where a new word would start.
fn at_word_start(app: &App) -> bool {
    let text = app.composer.text();
    let cursor = app.composer.cursor();
    text[..cursor]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace)
}

fn group_of(conv: &ConversationId) -> Option<String> {
    conv.0.strip_prefix("sg_").map(str::to_owned)
}

/// The server's roster as picker rows, or the mock's when offline.
fn roster(backend: Option<&mut Backend>) -> Result<Vec<PickItem>, String> {
    let Some(b) = backend else {
        return Ok(picker::mock_roster());
    };
    let users = auth::users(&b.config, &b.creds).map_err(|e| format!("取名单失败: {e}"))?;
    Ok(users
        .into_iter()
        .map(|u| {
            let label = if u.nickname.is_empty() { u.user_id.clone() } else { u.nickname };
            PickItem { id: u.user_id.clone(), label, detail: u.user_id }
        })
        .collect())
}

fn me_id(app: &App) -> String {
    app.snapshot.me.as_ref().map(|m| m.0.clone()).unwrap_or_default()
}

fn open_invite_picker(app: &mut App, backend: Option<&mut Backend>) {
    let Some(group_id) = group_of(&app.open) else {
        app.notice = Some("当前不是群聊，先 :new 建一个".into());
        return;
    };
    let items = match roster(backend) {
        Ok(items) => items,
        Err(e) => {
            app.notice = Some(e);
            return;
        }
    };
    let me = me_id(app);
    let present: HashSet<String> = app.snapshot.members.iter().map(|m| m.id.0.clone()).collect();
    let items: Vec<PickItem> = items
        .into_iter()
        .filter(|i| i.id != me && !present.contains(&i.id))
        .collect();
    if items.is_empty() {
        app.notice = Some("没有可邀请的人：大家都在群里了".into());
        return;
    }
    let title = format!("邀请到 #{}", app.conversation().map(|c| c.name.clone()).unwrap_or_default());
    app.picker = Some(Picker::new(
        title,
        Purpose::Invite { group_id, conversation: app.open.clone() },
        items,
        true,
    ));
}

fn open_dm_picker(app: &mut App, backend: Option<&mut Backend>) {
    let items = match roster(backend) {
        Ok(items) => items,
        Err(e) => {
            app.notice = Some(e);
            return;
        }
    };
    let me = me_id(app);
    let items: Vec<PickItem> = items.into_iter().filter(|i| i.id != me).collect();
    if items.is_empty() {
        app.notice = Some("服务器上还没有别人".into());
        return;
    }
    app.picker = Some(Picker::new("私聊谁", Purpose::DirectMessage, items, false));
}

fn complete_pick(app: &mut App, backend: Option<&mut Backend>, picker: Picker, ids: Vec<String>) {
    match picker.purpose.clone() {
        Purpose::Invite { group_id, .. } => {
            let names: Vec<String> = ids.iter().map(|id| picker.label_of(id)).collect();
            invite_users(app, backend, &group_id, ids, names);
        }
        Purpose::DirectMessage => {
            if let Some(id) = ids.first() {
                let name = picker.label_of(id);
                open_direct(app, backend, id, &name);
            }
        }
        Purpose::Mention => {
            if let Some(id) = ids.first() {
                let name = picker.label_of(id);
                insert_mention(app, id, &name);
            }
        }
        Purpose::Emoji => {
            if let Some(emoji) = ids.first() {
                // The colon that opened the list is not part of the message.
                app.composer.backspace();
                app.composer.insert_str(emoji);
            }
        }
    }
}

fn invite_users(app: &mut App, backend: Option<&mut Backend>, group_id: &str, ids: Vec<String>, names: Vec<String>) {
    match backend {
        Some(b) => match b.session.invite(group_id, &ids) {
            Ok(()) => app.notice = Some(format!("已邀请 {}", names.join("、"))),
            Err(e) => app.notice = Some(format!("邀请失败: {e}")),
        },
        None => {
            // Mock: put them straight into the member list so the pane reacts.
            for (id, name) in ids.iter().zip(&names) {
                if !app.snapshot.members.iter().any(|m| m.id.0 == *id) {
                    app.snapshot.members.push(Member {
                        id: UserId(id.clone()),
                        name: name.clone(),
                        avatar: None,
                        role: Role::Member,
                        online: false,
                        is_bot: false,
                    });
                }
            }
            let open = app.open.clone();
            let count = app.snapshot.members.len().max(1) as u32;
            if let Some(c) = app.snapshot.conversation_mut(&open) {
                c.member_count = count;
            }
            app.notice = Some(format!("示例模式：已邀请 {}", names.join("、")));
        }
    }
}

fn open_direct(app: &mut App, backend: Option<&mut Backend>, user_id: &str, nickname: &str) {
    let conv = match backend {
        Some(b) => b.session.open_direct(user_id, nickname, &mut app.snapshot),
        None => {
            let me = me_id(app);
            session::local_direct(&mut app.snapshot, &me, user_id, nickname)
        }
    };
    app.open_conversation(conv);
}

/// A group that exists only in this process, for `--mock`.
fn mock_group(app: &mut App, name: &str) -> ConversationId {
    let id = ConversationId(format!("sg_local_{}", app.snapshot.conversations.len() + 1));
    let newest = app.snapshot.conversations.iter().map(|c| c.last_activity_ms).max().unwrap_or(0);
    app.snapshot.upsert_conversation(Conversation {
        id: id.clone(),
        name: name.to_owned(),
        kind: ConversationKind::Group,
        category: Some(im_model::translate::category_for(ConversationKind::Group).to_owned()),
        unread: 0,
        mentions: 0,
        muted: false,
        member_count: 1,
        last_activity_ms: newest + 60_000,
        read_up_to: None,
    });
    let me = me_id(app);
    let my_name = app
        .snapshot
        .members
        .iter()
        .find(|m| m.id.0 == me)
        .map(|m| m.name.clone())
        .unwrap_or_else(|| "我".to_owned());
    app.snapshot.set_members(vec![Member {
        id: UserId(me),
        name: my_name,
        avatar: None,
        role: Role::Owner,
        online: true,
        is_bot: false,
    }]);
    id
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum KeyAction {
    None,
    Send,
    Quit,
    /// A `:` line, without the colon.
    Command(String),
    /// `@` was typed; offer the people in this conversation.
    Mention,
    /// `:` was typed at a word boundary; offer emoji.
    Emoji,
    /// Quote the selected message.
    Reply,
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> KeyAction {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);
    app.notice = None;

    match app.mode {
        Mode::Insert | Mode::Command => match code {
            KeyCode::Esc => {
                // Escape peels one layer at a time: the reply first, then
                // insert mode. Dropping both at once loses a quote the person
                // may have picked several keystrokes ago.
                if app.mode == Mode::Insert && app.has_draft_context() {
                    app.clear_draft_context();
                } else {
                    if app.mode == Mode::Command {
                        app.composer.clear();
                    }
                    app.mode = Mode::Normal;
                }
            }
            KeyCode::Enter if app.mode == Mode::Command => {
                let line = app.composer.text().trim_start_matches(':').trim().to_owned();
                app.composer.clear();
                app.mode = Mode::Normal;
                return KeyAction::Command(line);
            }
            // Shift+Enter inserts a newline; plain Enter sends.
            KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) => app.composer.newline(),
            KeyCode::Enter => return KeyAction::Send,
            KeyCode::Backspace if ctrl || alt => {
                app.composer.delete_word_before();
            }
            KeyCode::Backspace => {
                // Deleting the colon leaves the command line; there is nothing
                // to type into any more.
                if app.mode == Mode::Command && app.composer.text().len() <= 1 {
                    app.composer.clear();
                    app.mode = Mode::Normal;
                } else {
                    app.composer.backspace();
                }
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
            KeyCode::Char('@') if app.mode == Mode::Insert => {
                app.composer.insert_char('@');
                return KeyAction::Mention;
            }
            // Only at a word boundary: a colon inside "10:30" or a URL is
            // punctuation, not the start of an emoji.
            KeyCode::Char(':') if app.mode == Mode::Insert && at_word_start(app) => {
                app.composer.insert_char(':');
                return KeyAction::Emoji;
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
            KeyCode::Char('d') if ctrl => app.scroll_half_page(1),
            KeyCode::Char('u') if ctrl => app.scroll_half_page(-1),
            KeyCode::Char('G') => app.jump_to_latest(),
            KeyCode::Char('g') => app.jump_to_top(),
            KeyCode::Char('z') => app.toggle_collapsed(),
            KeyCode::Char('r') => return KeyAction::Reply,
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

    fn mock_app() -> App {
        App::new(im_model::mock::snapshot())
    }

    fn press(app: &mut App, code: KeyCode) -> Flow {
        on_key(app, None, &mut Uploads::disabled(), code, KeyModifiers::NONE)
    }

    fn type_command(app: &mut App, line: &str) -> Flow {
        press(app, KeyCode::Char(':'));
        for c in line.chars() {
            press(app, KeyCode::Char(c));
        }
        press(app, KeyCode::Enter)
    }

    #[test]
    fn a_snapshot_renders_at_the_requested_size() {
        let out = frame("100x24").expect("render");
        assert_eq!(out.lines().count(), 24);
        assert!(out.contains("排期讨论"), "the open conversation is titled");
        // Tall enough to hold the whole fixture, so the divider is on screen
        // wherever the scene happens to be anchored.
        let tall = frame("100x48").expect("render");
        assert!(tall.contains("以下为未读"), "the unread divider is drawn");
        assert!(tall.contains("─  今天  ─"), "the day divider is centred");
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
    fn the_picker_scene_draws_the_modal_over_the_panes() {
        let (width, height) = parse_size("110x30");
        let out = render::to_text(&render::capture(width, height, Scene::Picker).expect("render"));
        assert!(out.contains("邀请到 #排期讨论"), "modal title");
        assert!(out.contains("[x] 孙丽"), "a ticked row");
        assert!(out.contains("Enter 确定"), "the hint line");
        for line in out.lines() {
            assert!(unicode_width::UnicodeWidthStr::width(line) <= 110, "{line:?}");
        }
    }

    #[test]
    fn insert_mode_takes_letters_as_text_not_as_commands() {
        let mut app = mock_app();
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
        let mut app = mock_app();
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
    fn colon_enter_runs_the_line_and_returns_to_normal() {
        let mut app = mock_app();
        handle_key(&mut app, KeyCode::Char(':'), KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Command);
        for c in "help".chars() {
            handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(
            handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            KeyAction::Command("help".into())
        );
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.composer.is_empty());
    }

    #[test]
    fn backspacing_the_colon_leaves_command_mode() {
        let mut app = mock_app();
        handle_key(&mut app, KeyCode::Char(':'), KeyModifiers::NONE);
        handle_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.composer.is_empty());
    }

    #[test]
    fn sending_in_mock_mode_keeps_the_draft_and_explains() {
        let mut app = mock_app();
        app.composer.insert_str("hello");
        send_composer(&mut app, None, &mut Uploads::disabled());
        assert_eq!(app.composer.text(), "hello", "draft must survive a failed send");
        assert!(app.notice.is_some());
    }

    #[test]
    fn new_creates_a_group_opens_it_and_offers_the_invite_picker() {
        let mut app = mock_app();
        assert_eq!(type_command(&mut app, "new 周末爬山"), Flow::Continue);
        assert!(app.open.0.starts_with("sg_local_"), "the new group is open: {}", app.open.0);
        assert_eq!(app.conversation().map(|c| c.name.as_str()), Some("周末爬山"));
        assert_eq!(app.snapshot.conversations.first().map(|c| c.name.as_str()), Some("周末爬山"), "newest first");
        let picker = app.picker.as_ref().expect("invite picker opens");
        assert!(picker.multi);
        assert!(picker.title.contains("周末爬山"));
        assert!(!picker.items.iter().any(|i| i.id == me_id(&app)), "never offer to invite myself");
    }

    #[test]
    fn keys_go_to_the_picker_while_it_is_open() {
        let mut app = mock_app();
        type_command(&mut app, "new 周末爬山");
        assert_eq!(press(&mut app, KeyCode::Char('q')), Flow::Continue, "q filters, it does not quit");
        assert_eq!(app.picker.as_ref().map(|p| p.filter.as_str()), Some("q"));
        press(&mut app, KeyCode::Esc);
        assert!(app.picker.is_none(), "escape closes the modal");
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn confirming_the_invite_picker_in_mock_mode_adds_the_members() {
        let mut app = mock_app();
        type_command(&mut app, "new 周末爬山");
        let before = app.snapshot.members.len();
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Enter);
        assert!(app.picker.is_none());
        assert_eq!(app.snapshot.members.len(), before + 2);
        assert!(app.notice.as_deref().is_some_and(|n| n.contains("已邀请")), "{:?}", app.notice);
        assert_eq!(app.conversation().map(|c| c.member_count), Some(before as u32 + 2));
    }

    #[test]
    fn dm_opens_a_direct_conversation_on_the_sdk_derived_id() {
        let mut app = mock_app();
        let me = me_id(&app);
        assert!(!me.is_empty(), "the mock knows who I am");
        type_command(&mut app, "dm lina");
        let expected = im_model::translate::direct_conversation_id(&me, "lina");
        assert_eq!(app.open, expected);
        assert_eq!(app.conversation().map(|c| c.kind), Some(ConversationKind::Direct));
        assert_eq!(app.conversation().map(|c| c.name.as_str()), Some("李娜"), "named from the roster");
        // Opening it twice must not duplicate the row.
        let count = app.snapshot.conversations.len();
        type_command(&mut app, "dm lina");
        assert_eq!(app.snapshot.conversations.len(), count);
    }

    #[test]
    fn q_quits_and_unknown_verbs_explain_themselves() {
        let mut app = mock_app();
        assert_eq!(type_command(&mut app, "q"), Flow::Quit);
        assert_eq!(type_command(&mut app, "frobnicate"), Flow::Continue);
        assert!(app.notice.as_deref().is_some_and(|n| n.contains("frobnicate")));
        type_command(&mut app, "new");
        assert!(app.notice.as_deref().is_some_and(|n| n.contains("用法")));
        assert!(app.picker.is_none());
    }

    #[test]
    fn the_reply_scene_shows_what_is_being_answered_above_the_input() {
        let (width, height) = parse_size("110x30");
        let out = render::to_text(&render::capture(width, height, Scene::Reply).expect("render"));
        assert!(out.contains("↩ 陈明:"), "the quote strip names the author: {out}");
        assert!(out.contains("@李娜 我按这个跑一遍再合"), "the draft is visible");
        for line in out.lines() {
            assert!(unicode_width::UnicodeWidthStr::width(line) <= 110, "{line:?}");
        }
    }

    #[test]
    fn r_starts_a_reply_and_escape_drops_it_before_leaving_insert() {
        let mut app = mock_app();
        app.focus(Pane::Messages);
        press(&mut app, KeyCode::Char('r'));
        assert!(app.reply_to.is_some(), "r quotes the selected message");
        assert_eq!(app.mode, Mode::Insert, "and puts the cursor in the composer");

        press(&mut app, KeyCode::Esc);
        assert!(app.reply_to.is_none(), "the first escape drops the quote");
        assert_eq!(app.mode, Mode::Insert, "but stays in insert");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Normal, "the second escape leaves insert");
    }

    #[test]
    fn at_types_the_sign_then_offers_the_people_in_this_conversation() {
        let mut app = mock_app();
        app.mode = Mode::Insert;
        press(&mut app, KeyCode::Char('@'));
        assert_eq!(app.composer.text(), "@", "the sign is typed either way");
        let picker = app.picker.as_ref().expect("the list opens");
        assert!(!picker.multi, "one mention at a time");
        assert_eq!(
            picker.items.first().map(|i| i.label.as_str()),
            Some("全体成员"),
            "everyone is offered first in a group"
        );
        assert!(!picker.items.iter().any(|i| i.id == me_id(&app)), "never offer to @ myself");

        // Cancelling leaves the sign behind as ordinary text.
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.composer.text(), "@");
        assert!(app.draft_mentions.is_empty());
    }

    #[test]
    fn picking_a_name_writes_it_into_the_draft_and_records_who_to_notify() {
        let mut app = mock_app();
        app.mode = Mode::Insert;
        press(&mut app, KeyCode::Char('@'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(app.picker.is_none());
        let recorded = app.draft_mentions.first().expect("one mention recorded");
        assert_eq!(app.composer.text(), format!("@{} ", recorded.nickname));
        assert_eq!(
            app::live_mentions(app.composer.text(), &app.draft_mentions).len(),
            1,
            "the name is still in the text, so it still notifies"
        );
    }

    #[test]
    fn a_direct_conversation_offers_no_mention_of_everyone() {
        let mut app = mock_app();
        let direct = app
            .snapshot
            .conversations
            .iter()
            .find(|c| c.kind == ConversationKind::Direct)
            .map(|c| c.id.clone())
            .expect("the fixture has a direct conversation");
        app.open_conversation(direct);
        app.mode = Mode::Insert;
        press(&mut app, KeyCode::Char('@'));
        let offered = app.picker.as_ref().map(|p| {
            p.items.iter().any(|i| i.id == im_model::AT_ALL_TAG)
        });
        assert_eq!(offered, Some(false), "there is no 'everyone' in a two-person chat");
    }

    #[test]
    fn a_dragged_file_is_staged_rather_than_sent_at_once() {
        let dir = std::env::temp_dir().join(format!("yptd-drag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let one = dir.join("a.png");
        let two = dir.join("b b.png");
        std::fs::write(&one, b"x").expect("file");
        std::fs::write(&two, b"x").expect("file");

        let mut app = mock_app();
        // What a terminal inserts when two files are dropped at once.
        let pasted = format!(
            "{} {}",
            one.display(),
            two.display().to_string().replace(' ', "\\ ")
        );
        on_paste(&mut app, &pasted);
        assert_eq!(app.pending.len(), 2, "both files staged: {:?}", app.pending);
        assert_eq!(app.mode, Mode::Insert, "ready for a caption");
        assert!(app.composer.is_empty(), "the paths must not land in the text");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pasting_prose_types_it_instead_of_attaching_it() {
        let mut app = mock_app();
        app.mode = Mode::Insert;
        on_paste(&mut app, "这是一段普通文字");
        assert!(app.pending.is_empty());
        assert_eq!(app.composer.text(), "这是一段普通文字");
    }

    #[test]
    fn escape_drops_staged_pictures_before_leaving_insert() {
        let mut app = mock_app();
        app.mode = Mode::Insert;
        app.pending.push(std::path::PathBuf::from("/tmp/a.png"));
        press(&mut app, KeyCode::Esc);
        assert!(app.pending.is_empty());
        assert_eq!(app.mode, Mode::Insert);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn staging_the_same_picture_twice_keeps_one() {
        let mut app = mock_app();
        assert_eq!(app.attach([std::path::PathBuf::from("/tmp/a.png")]), 1);
        assert_eq!(app.attach([std::path::PathBuf::from("/tmp/a.png")]), 0);
        assert_eq!(app.pending.len(), 1);
    }

    #[test]
    fn img_without_a_path_opens_the_browser_and_keys_go_to_it() {
        let mut app = mock_app();
        type_command(&mut app, "img");
        assert!(app.browser.is_some(), "the browser opens");
        assert_eq!(press(&mut app, KeyCode::Char('q')), Flow::Continue, "q filters here too");
        assert_eq!(app.browser.as_ref().map(|b| b.filter.as_str()), Some("q"));
        press(&mut app, KeyCode::Esc);
        assert!(app.browser.is_none());
        assert!(app.last_dir.is_some(), "reopening remembers where it was");
    }

    #[test]
    fn sending_with_nothing_typed_but_pictures_staged_still_sends() {
        let mut app = mock_app();
        app.pending.push(std::path::PathBuf::from("/tmp/a.png"));
        // No text, no live session: the mock path reports why rather than
        // silently doing nothing, which is what an empty composer would do.
        send_composer(&mut app, None, &mut Uploads::disabled());
        assert!(app.notice.is_some());
        assert_eq!(app.pending.len(), 1, "staged pictures survive a failed send");
    }

    #[test]
    fn the_browse_scene_shows_the_list_over_the_conversation() {
        let (width, height) = parse_size("100x24");
        let out = render::to_text(&render::capture(width, height, Scene::Browse).expect("render"));
        assert!(out.contains("选择图片"), "modal title");
        assert!(out.contains("[x] 现场-1.png"), "a ticked picture");
        assert!(out.contains("📁 上周归档"), "a directory row");
        assert!(out.contains("已选 2 张"), "the hint counts them");
    }

    #[test]
    fn the_attach_scene_shows_what_is_staged_above_the_input() {
        let (width, height) = parse_size("100x20");
        let out = render::to_text(&render::capture(width, height, Scene::Attach).expect("render"));
        assert!(out.contains("3 个待发"), "the strip counts them: {out}");
        assert!(out.contains("现场-2.png"));
    }

    #[test]
    fn consecutive_pictures_from_one_person_are_labelled_as_a_group() {
        let (width, height) = parse_size("100x26");
        let out = render::to_text(&render::capture(width, height, Scene::Live).expect("render"));
        assert!(out.contains("🖼️ 3 张图片"), "the fixture's run collapses: {out}");
    }

    #[test]
    fn a_colon_at_a_word_boundary_offers_emoji() {
        let mut app = mock_app();
        app.mode = Mode::Insert;
        press(&mut app, KeyCode::Char(':'));
        assert_eq!(app.composer.text(), ":");
        let picker = app.picker.as_ref().expect("the emoji list opens");
        assert!(picker.items.len() > 500, "the whole set is offered");
        // Filtering is by shortcode, which is what people already type.
        let mut filtered = picker.clone();
        for c in "smile".chars() {
            filtered.push_char(c);
        }
        assert!(
            filtered.visible().iter().any(|&i| filtered.items[i].label.contains(":smile:")),
            "typing a shortcode finds it"
        );
    }

    #[test]
    fn a_colon_inside_a_word_stays_punctuation() {
        // "10:30" and "https://…" are not emoji.
        let mut app = mock_app();
        app.mode = Mode::Insert;
        app.composer.insert_str("10");
        press(&mut app, KeyCode::Char(':'));
        assert!(app.picker.is_none(), "no list for a colon mid-word");
        assert_eq!(app.composer.text(), "10:");
    }

    #[test]
    fn picking_an_emoji_replaces_the_colon_that_opened_the_list() {
        let mut app = mock_app();
        app.mode = Mode::Insert;
        app.composer.insert_str("好的 ");
        press(&mut app, KeyCode::Char(':'));
        assert!(app.picker.is_some());
        press(&mut app, KeyCode::Enter);
        assert!(app.picker.is_none());
        let text = app.composer.text();
        assert!(text.starts_with("好的 "), "{text:?}");
        assert!(!text.contains(':'), "the colon must not survive: {text:?}");
        assert!(text.chars().count() > 3, "an emoji went in: {text:?}");
    }

    #[test]
    fn escaping_the_emoji_list_leaves_the_colon_as_typed() {
        let mut app = mock_app();
        app.mode = Mode::Insert;
        press(&mut app, KeyCode::Char(':'));
        press(&mut app, KeyCode::Esc);
        assert!(app.picker.is_none());
        assert_eq!(app.composer.text(), ":");
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
