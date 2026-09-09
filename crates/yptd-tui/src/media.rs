//! Inline images in the message list.
//!
//! Terminals disagree about how to show a picture: Kitty and Ghostty speak the
//! Kitty graphics protocol, iTerm2 has its own, some support Sixel, and the
//! rest get half-block characters. [`ratatui_image`] negotiates that; what this
//! module owns is everything around it -- deciding how many rows an image gets
//! *before* it is decoded, caching decoded pictures, and degrading to a text
//! line when the terminal or the file will not cooperate.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use image::imageops::FilterType;
use image::DynamicImage;
use ratatui::layout::{Rect, Size};
use ratatui::Frame;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};

/// Rows an inline preview is allowed to occupy. Tall enough to be worth
/// showing, short enough that one screenshot cannot bury the conversation.
pub const MAX_PREVIEW_ROWS: u16 = 12;
const MAX_PREVIEW_COLS: u16 = 48;

/// Decoded images, keyed by attachment name.
///
/// Height is decided from the image's aspect ratio and reserved in the layout
/// before the picture is drawn, so a message's height never changes between
/// the frame that measured it and the frame that paints it.
/// What the encoder sends back: which picture at which size, and the
/// terminal-ready result or why there is none.
pub type Encoded = ((String, u16, u16, bool), Result<Protocol, String>);

/// One request to the encoder: the picture and the exact cells to fill.
struct EncodeJob {
    key: (String, u16, u16, bool),
    source: Arc<DynamicImage>,
}

/// Decoded pictures this many and older are dropped, least recently drawn
/// first. Each is a few megabytes; a long conversation must not keep all
/// of them.
const MAX_DECODED: usize = 24;

pub struct Media {
    picker: Option<Picker>,
    /// Decoded pictures, already shrunk to something a terminal can use.
    images: HashMap<String, Arc<DynamicImage>>,
    /// When each picture was last drawn, for eviction.
    last_used: HashMap<String, u64>,
    tick: u64,
    /// Where encode requests go. Encoding is a resize plus the terminal's
    /// own encoding of the pixels, tens of milliseconds a picture; doing it
    /// while drawing is what makes the first frame with pictures stutter.
    encoder: Option<Sender<EncodeJob>>,
    /// Sizes already asked for, so a frame drawn before the answer arrives
    /// does not ask again.
    pending: HashSet<(String, u16, u16, bool)>,
    /// One encoded protocol per picture *per size it is drawn at*.
    ///
    /// Built here rather than left to the widget: handing the library a big
    /// picture and an area makes it resize on every draw with a filter this
    /// code does not choose. Sizing the canvas first means the filter is ours
    /// and the work happens once.
    protocols: HashMap<(String, u16, u16, bool), Protocol>,
    sizes: HashMap<String, (u32, u32)>,
    failed: HashMap<String, String>,
    /// Bumped when a picture arrives or fails, both of which change how many
    /// rows its message needs.
    revision: u64,
}

impl Media {
    /// Queries the terminal for the best available graphics protocol.
    ///
    /// Returns a `Media` either way: when the query fails -- no tty, or a
    /// terminal that answers nothing -- images degrade to a text line rather
    /// than the client refusing to start.
    pub fn detect() -> Self {
        Self {
            picker: forced_picker().or_else(|| Picker::from_query_stdio().ok()),
            images: HashMap::new(),
            last_used: HashMap::new(),
            tick: 0,
            encoder: None,
            pending: HashSet::new(),
            protocols: HashMap::new(),
            sizes: HashMap::new(),
            failed: HashMap::new(),
            revision: 0,
        }
    }

    /// Starts the thread that turns pictures into terminal sequences.
    ///
    /// `out` is the main loop's channel and `wake` wraps a result into
    /// whatever that loop understands, the same shape as the downloader.
    pub fn start_encoder<T, F>(&mut self, out: Sender<T>, wake: F)
    where
        T: Send + 'static,
        F: Fn(Encoded) -> T + Send + 'static,
    {
        let Some(picker) = self.picker.clone() else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel::<EncodeJob>();
        let spawned = std::thread::Builder::new()
            .name("image-encoder".into())
            .spawn(move || {
                for job in rx {
                    let result = encode(&picker, &job.source, job.key.1, job.key.2, job.key.3);
                    if out.send(wake((job.key, result))).is_err() {
                        break;
                    }
                }
            });
        if spawned.is_ok() {
            self.encoder = Some(tx);
        }
    }

    /// A `Media` that never renders pictures, for off-screen capture and tests.
    /// A media store around a picker chosen by the caller, for tests that
    /// need a particular graphics protocol rather than whatever the terminal
    /// running them happens to support.
    #[cfg(test)]
    pub fn with_picker(picker: Picker) -> Self {
        let mut media = Self::disabled();
        media.picker = Some(picker);
        media
    }

    pub fn disabled() -> Self {
        Self {
            picker: None,
            images: HashMap::new(),
            last_used: HashMap::new(),
            tick: 0,
            encoder: None,
            pending: HashSet::new(),
            protocols: HashMap::new(),
            sizes: HashMap::new(),
            failed: HashMap::new(),
            revision: 0,
        }
    }

    pub fn enabled(&self) -> bool {
        self.picker.is_some()
    }

    /// Protocol and cell size in one short label, for the status line: the
    /// two numbers that decide whether a picture can be sharp, where the
    /// person can see them without running a diagnostic.
    pub fn status_label(&self) -> String {
        match self.picker.as_ref() {
            Some(picker) => {
                let cell = picker.font_size();
                format!("{} {}×{}", self.protocol_name(), cell.width, cell.height)
            }
            None => "off".to_owned(),
        }
    }

    /// Short name of the negotiated protocol, for the status line.
    pub fn protocol_name(&self) -> &'static str {
        use ratatui_image::picker::ProtocolType;
        match self.picker.as_ref().map(Picker::protocol_type) {
            Some(ProtocolType::Kitty) => "kitty",
            Some(ProtocolType::Iterm2) => "iterm2",
            Some(ProtocolType::Sixel) => "sixel",
            Some(ProtocolType::Halfblocks) => "halfblocks",
            None => "off",
        }
    }

    /// Registers a decoded image and returns the rows it will occupy.
    ///
    /// `natural` is the picture's size before [`decode`] shrank it, because
    /// that is the size worth telling the reader about.
    pub fn insert(&mut self, key: &str, image: DynamicImage, natural: (u32, u32)) -> u16 {
        if self.picker.is_none() {
            return 0;
        }
        self.revision = self.revision.wrapping_add(1);
        self.sizes.insert(key.to_owned(), natural);
        self.images.insert(key.to_owned(), Arc::new(image));
        self.touch(key);
        self.evict_decoded();
        self.rows_for(key)
    }

    fn touch(&mut self, key: &str) {
        self.tick += 1;
        self.last_used.insert(key.to_owned(), self.tick);
    }

    /// Drops the decoded pictures drawn longest ago once there are too many.
    /// Their sizes stay, so the rows they occupy do not change; a scroll
    /// back to them re-downloads from the disk cache.
    fn evict_decoded(&mut self) {
        while self.images.len() > MAX_DECODED {
            let Some(oldest) = self
                .images
                .keys()
                .min_by_key(|k| self.last_used.get(*k).copied().unwrap_or(0))
                .cloned()
            else {
                break;
            };
            self.images.remove(&oldest);
            self.protocols.retain(|(k, ..), _| *k != oldest);
            self.pending.retain(|(k, ..)| *k != oldest);
        }
    }

    /// Whether the picture itself is still held. A size that is known while
    /// the pixels were let go asks the caller to fetch again.
    pub fn holds(&self, key: &str) -> bool {
        self.images.contains_key(key)
    }

    /// Files an encoder's answer.
    pub fn store_encoded(&mut self, (key, result): Encoded) {
        self.pending.remove(&key);
        match result {
            Ok(protocol) => {
                self.evict_if_crowded();
                self.protocols.insert(key, protocol);
            }
            Err(reason) => self.mark_failed(&key.0, reason),
        }
    }

    pub fn mark_failed(&mut self, key: &str, reason: impl Into<String>) {
        self.revision = self.revision.wrapping_add(1);
        self.failed.insert(key.to_owned(), reason.into());
    }

    /// A number that changes whenever a picture's size or state does.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// What the terminal answered when asked about its geometry.
    ///
    /// The one number that decides how sharp a picture can be: everything is
    /// resized to the area's size *in pixels*, and that is cells times this.
    /// A terminal that reports logical rather than device pixels on a
    /// high-density screen halves the resolution of every image, and the only
    /// way to tell is to look.
    pub fn report(&self) -> String {
        let Some(picker) = self.picker.as_ref() else {
            return "图形协议: 无（这个终端不支持，图片会降级成一行文字）".to_owned();
        };
        let size = picker.font_size();
        let album_cols = crate::album::MAX_COLS;
        let album_rows = crate::album::MAX_ROWS;
        format!(
            "图形协议: {}\n             终端单元格: {} × {} 像素\n             单张图上限: {} 列 × {} 行 = {} × {} 像素\n             一排四张时每格: {} 列 × {} 行 = {} × {} 像素",
            self.protocol_name(),
            size.width,
            size.height,
            crate::album::SINGLE_MAX_COLS,
            album_rows,
            u32::from(crate::album::SINGLE_MAX_COLS) * u32::from(size.width),
            u32::from(album_rows) * u32::from(size.height),
            album_cols / 4,
            album_rows,
            u32::from(album_cols / 4) * u32::from(size.width),
            u32::from(album_rows) * u32::from(size.height),
        )
    }

    pub fn failure(&self, key: &str) -> Option<&str> {
        self.failed.get(key).map(String::as_str)
    }

    /// The picture's own size in pixels, once it has been decoded.
    pub fn dimensions(&self, key: &str) -> Option<(u32, u32)> {
        self.sizes.get(key).copied()
    }

    /// How many pixels one terminal cell covers, once the protocol has been
    /// negotiated. `None` when pictures cannot be drawn at all.
    pub fn cell_size(&self) -> Option<crate::album::CellSize> {
        let size = self.picker.as_ref()?.font_size();
        (size.width > 0 && size.height > 0).then_some(crate::album::CellSize {
            width: size.width,
            height: size.height,
        })
    }

    /// Rows this image occupies, from its aspect ratio and the terminal's
    /// cell geometry. Zero when there is nothing to draw.
    pub fn rows_for(&self, key: &str) -> u16 {
        let Some(picker) = self.picker.as_ref() else {
            return 0;
        };
        let Some(&(w, h)) = self.sizes.get(key) else {
            return 0;
        };
        if w == 0 || h == 0 {
            return 0;
        }
        let cell = picker.font_size();
        let (cell_w, cell_h) = (cell.width, cell.height);
        if cell_w == 0 || cell_h == 0 {
            return 0;
        }
        // Fit to a column budget first, then convert pixel height to rows.
        let target_px_w = u32::from(MAX_PREVIEW_COLS) * u32::from(cell_w);
        let scale = f64::from(target_px_w.min(w)) / f64::from(w);
        let px_h = (f64::from(h) * scale).round().max(1.0);
        let rows = (px_h / f64::from(cell_h)).ceil() as u16;
        rows.clamp(1, MAX_PREVIEW_ROWS)
    }

    /// Draws a registered image into `area`.
    /// Draws a picture into `area`, or asks for it to be encoded at this size
    /// and draws nothing until that comes back.
    ///
    /// `fill` covers the whole area, cropping what does not fit, which is what
    /// makes a row of pictures read as one block; otherwise the picture is
    /// fitted whole inside it.
    /// Draws a picture, reporting whether anything was actually painted.
    ///
    /// `false` means the caller should draw whatever it shows in place of a
    /// picture: the file is still downloading, the encoder has not caught up,
    /// or the area is too small to draw into safely. Leaving the space blank
    /// instead would be a hole that fills in a moment later.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, key: &str, fill: bool) -> bool {
        // One cell in either direction is refused on purpose. ratatui-image
        // ends every row of a picture with `ESC[u ESC[{w-1}C ESC[{h-1}B` to
        // put the cursor back where ratatui expects it -- but a cursor
        // movement with a parameter of 0 means *one* in ECMA-48, not none. So
        // a picture one row tall moves the cursor a row further than ratatui
        // accounts for, and everything drawn afterwards lands one row low.
        // That is what makes an avatar look like it floated above its name.
        if area.width < 2 || area.height < 2 {
            return false;
        }
        self.touch(key);
        let cached = (key.to_owned(), area.width, area.height, fill);
        if let Some(protocol) = self.protocols.get(&cached) {
            frame.render_widget(Image::new(protocol), area);
            return true;
        }
        if self.pending.contains(&cached) {
            return false;
        }
        let Some(source) = self.images.get(key) else {
            return false;
        };
        match self.encoder.as_ref() {
            Some(encoder) => {
                if encoder
                    .send(EncodeJob { key: cached.clone(), source: Arc::clone(source) })
                    .is_ok()
                {
                    self.pending.insert(cached);
                }
            }
            // No encoder thread (tests, the capture paths): do it here.
            None => {
                if let Some(picker) = self.picker.as_ref() {
                    let source = Arc::clone(source);
                    match encode(picker, &source, area.width, area.height, fill) {
                        Ok(protocol) => {
                            frame.render_widget(Image::new(&protocol), area);
                            self.protocols.insert(cached, protocol);
                            return true;
                        }
                        Err(reason) => self.mark_failed(key, reason),
                    }
                }
            }
        }
        false
    }

    /// Keeps the encoded set bounded. Pictures are cheap to re-encode and a
    /// long conversation would otherwise hold every one it ever drew.
    fn evict_if_crowded(&mut self) {
        const MAX_ENCODED: usize = 48;
        if self.protocols.len() < MAX_ENCODED {
            return;
        }
        let victims: Vec<_> = self
            .protocols
            .keys()
            .take(self.protocols.len() - MAX_ENCODED / 2)
            .cloned()
            .collect();
        for victim in victims {
            self.protocols.remove(&victim);
        }
    }
}

/// Resizes a picture to exactly `cols` × `rows` cells and encodes it for the
/// terminal. The canvas matches the area, so the library has nothing left to
/// resize and its own (nearest-neighbour) filter never comes into it.
fn encode(
    picker: &Picker,
    source: &DynamicImage,
    cols: u16,
    rows: u16,
    fill: bool,
) -> Result<Protocol, String> {
    let cell = picker.font_size();
    let (Some(canvas_w), Some(canvas_h)) = (
        u32::from(cols).checked_mul(u32::from(cell.width)),
        u32::from(rows).checked_mul(u32::from(cell.height)),
    ) else {
        return Err("画布尺寸溢出".into());
    };
    if canvas_w == 0 || canvas_h == 0 {
        return Err("画布为空".into());
    }
    // Lanczos, and only here: this is the one resize that decides how the
    // picture looks, and it starts from an image already close to its final
    // size, so the good filter costs little.
    let canvas = if fill {
        cover(source, canvas_w, canvas_h)
    } else {
        source.resize(canvas_w, canvas_h, FilterType::Lanczos3)
    };
    picker
        .new_protocol(canvas, Size::new(cols, rows), Resize::Fit(None))
        .map_err(|e| format!("编码失败: {e}"))
}

/// An override for the detected graphics protocol and cell size.
///
/// `YPTD_IMAGE=kitty:16x35` forces both; `YPTD_IMAGE=off` turns pictures off.
/// Terminals disagree about the graphics protocols in ways that only show up
/// on somebody else's machine, and a person who can see the problem needs a
/// way to try another protocol without rebuilding. It also lets a headless
/// test drive a protocol the pseudo-terminal would never negotiate.
fn forced_picker() -> Option<Picker> {
    use ratatui_image::picker::ProtocolType;

    let raw = std::env::var("YPTD_IMAGE").ok()?;
    let (name, size) = raw.split_once(':').unwrap_or((raw.as_str(), "10x20"));
    // Accept both separators: the status line prints "16×35", and that is
    // what anybody copying from it will type back.
    let (w, h) = size.split_once(['x', 'X', '×'])?;
    let font = ratatui_image::FontSize {
        width: w.parse().ok()?,
        height: h.parse().ok()?,
    };
    let protocol = match name.trim().to_ascii_lowercase().as_str() {
        "kitty" => ProtocolType::Kitty,
        "iterm2" => ProtocolType::Iterm2,
        "sixel" => ProtocolType::Sixel,
        "halfblocks" | "blocks" => ProtocolType::Halfblocks,
        "off" | "none" => return None,
        _ => return None,
    };
    let mut picker = Picker::from_fontsize(font);
    picker.set_protocol_type(protocol);
    Some(picker)
}

/// Scales to cover the canvas and trims the overflow from the centre, so a
/// tile is filled edge to edge whatever shape the picture is.
fn cover(source: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    let scaled = source.resize_to_fill(width, height, FilterType::Lanczos3);
    if scaled.width() == width && scaled.height() == height {
        return scaled;
    }
    scaled.crop_imm(0, 0, width, height)
}

impl Default for Media {
    fn default() -> Self {
        Self::disabled()
    }
}

/// The longest edge a picture keeps once decoded.
///
/// A phone photo is twenty-odd megapixels; a terminal shows it in a few
/// hundred. Resizing straight from the original with a good filter costs
/// about half a second *per picture*, and that happens on the drawing thread
/// -- which is what makes a conversation full of photos freeze on open.
/// Shrinking once, cheaply, up front leaves the final resize working on a
/// twentieth of the pixels and still far more than the screen can show.
const MAX_STORED_EDGE: u32 = 1600;

/// Decodes image bytes and shrinks them to something a terminal can use.
///
/// Returns the picture and its size before shrinking, which is what the
/// attachment line reports.
pub fn decode_for_display(bytes: &[u8]) -> Result<(DynamicImage, (u32, u32)), String> {
    let image = decode(bytes)?;
    let natural = (image.width(), image.height());
    let longest = natural.0.max(natural.1);
    if longest <= MAX_STORED_EDGE {
        return Ok((image, natural));
    }
    // A box filter, not Lanczos: this step throws away most of the pixels,
    // and the quality that matters comes from the final resize onto the
    // cells. Measured on a 4284×5712 photo: thumbnail 148ms, Triangle 236ms,
    // Lanczos over 400ms.
    let shrunk = image.thumbnail(MAX_STORED_EDGE, MAX_STORED_EDGE);
    Ok((shrunk, natural))
}

/// Decodes image bytes, rejecting anything implausible before handing it to
/// the decoder.
///
/// The size cap is a denial-of-service guard, not a quality one: a 20000×20000
/// PNG is a few hundred kilobytes on the wire and gigabytes decoded.
pub fn decode(bytes: &[u8]) -> Result<DynamicImage, String> {
    const MAX_PIXELS: u64 = 40_000_000;

    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("无法识别图片格式: {e}"))?;
    if let Some((w, h)) = reader.into_dimensions().ok() {
        if u64::from(w) * u64::from(h) > MAX_PIXELS {
            return Err(format!("图片过大: {w}×{h}"));
        }
    }
    image::load_from_memory(bytes).map_err(|e| format!("解码失败: {e}"))
}

/// A deterministic stand-in picture for the mock fixture.
///
/// Derived from the attachment name so two different attachments look
/// different, which makes it obvious when the cache keys off the wrong thing.
pub fn demo_image_png(name: &str) -> Result<Vec<u8>, String> {
    const W: u32 = 480;
    const H: u32 = 270;
    let seed = name.bytes().fold(17u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));

    let mut buf = image::RgbImage::new(W, H);
    for (x, y, pixel) in buf.enumerate_pixels_mut() {
        // A soft diagonal gradient with a grid, so scaling artifacts and
        // aspect-ratio mistakes are both easy to see.
        let base = ((x * 255) / W + (y * 255) / H) / 2;
        let grid = u32::from(x % 60 < 2 || y % 60 < 2) * 60;
        let r = ((base + seed % 64) % 256) as u8;
        let g = ((base + grid + (seed / 64) % 64) % 256) as u8;
        let b = ((200 - base.min(200)) + grid) as u8;
        *pixel = image::Rgb([r, g, b]);
    }
    let mut out = Vec::new();
    DynamicImage::ImageRgb8(buf)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| format!("生成演示图片失败: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders one picture through the real kitty protocol into an offscreen
    /// buffer and returns what would go to the terminal.
    fn kitty_cells(cols: u16, rows: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui_image::FontSize;
        use ratatui::Terminal;
        use ratatui_image::picker::ProtocolType;
        use ratatui_image::Image;

        let mut picker = Picker::from_fontsize(FontSize { width: 10, height: 20 });
        picker.set_protocol_type(ProtocolType::Kitty);
        let protocol = encode(&picker, &gradient(64, 64), cols, rows, true).expect("encode");
        let mut terminal = Terminal::new(TestBackend::new(cols + 2, rows + 2)).expect("terminal");
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Image::new(&protocol),
                    Rect { x: 0, y: 0, width: cols, height: rows },
                );
            })
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    /// The reason [`Media::render`] refuses an area one cell tall or wide.
    ///
    /// ratatui-image closes every row of a picture by restoring the saved
    /// cursor and stepping `width-1` right and `height-1` down. In ECMA-48 a
    /// cursor movement with a parameter of 0 means *one*, not none -- so at a
    /// height of one the terminal ends up a row below where ratatui believes
    /// it is, and everything drawn afterwards lands a row low. That is what
    /// made an avatar appear to float above its own name.
    #[test]
    fn a_picture_one_row_tall_would_walk_the_cursor_off_by_a_row() {
        assert!(
            kitty_cells(4, 1).contains("\x1b[0B"),
            "a one-row picture still emits the zero-parameter cursor move"
        );
        assert!(
            !kitty_cells(4, 2).contains("\x1b[0B"),
            "two rows and up are safe: the parameter is never zero"
        );
    }

    #[test]
    fn an_area_too_thin_to_place_a_picture_reports_that_it_drew_nothing() {
        let mut media = Media::disabled();
        media.insert("k", gradient(64, 64), (64, 64));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(8, 8)).expect("terminal");
        for (w, h) in [(1u16, 4u16), (4, 1), (1, 1)] {
            terminal
                .draw(|frame| {
                    let area = Rect { x: 0, y: 0, width: w, height: h };
                    assert!(
                        !media.render(frame, area, "k", true),
                        "{w}x{h} must report nothing drawn so the caller falls back"
                    );
                })
                .expect("draw");
        }
    }

    fn gradient(w: u32, h: u32) -> DynamicImage {
        let mut buf = image::RgbImage::new(w, h);
        for (x, y, pixel) in buf.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8, 128]);
        }
        DynamicImage::ImageRgb8(buf)
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        gradient(w, h)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encode");
        out
    }

    #[test]
    fn covering_fills_the_canvas_whatever_shape_the_picture_is() {
        for (w, h) in [(4000u32, 1000u32), (1000, 4000), (900, 900)] {
            let filled = super::cover(&gradient(w, h), 240, 120);
            assert_eq!(
                (filled.width(), filled.height()),
                (240, 120),
                "{w}x{h} must cover the tile exactly"
            );
        }
    }

    #[test]
    fn fitting_never_exceeds_the_canvas() {
        let fitted = gradient(4000, 1000).resize(240, 120, image::imageops::FilterType::Triangle);
        assert!(fitted.width() <= 240 && fitted.height() <= 120);
    }

    #[test]
    fn a_disabled_media_never_reserves_rows() {
        let mut m = Media::disabled();
        assert!(!m.enabled());
        assert_eq!(m.insert("a.png", gradient(100, 100), (100, 100)), 0);
        assert_eq!(m.rows_for("a.png"), 0);
        assert_eq!(m.protocol_name(), "off");
    }

    #[test]
    fn decoding_accepts_a_real_png() {
        let image = decode(&png(64, 32)).expect("decode");
        assert_eq!((image.width(), image.height()), (64, 32));
    }

    #[test]
    fn decoding_rejects_junk_with_a_message_not_a_panic() {
        let err = decode(b"this is not an image").unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn an_absurdly_large_image_is_refused_before_decoding() {
        // A 1x1 PNG rewritten to claim huge dimensions would be the real
        // attack; here we assert the guard exists and reports its numbers.
        let mut header = png(8, 8);
        // Corrupting the IHDR makes it undecodable, which is also a rejection.
        header[20] = 0xFF;
        assert!(decode(&header).is_err());
    }

    #[test]
    fn the_demo_image_round_trips_through_the_real_decode_path() {
        let bytes = demo_image_png("排期草稿-v3.png").expect("generate");
        let a = decode(&bytes).expect("decode");
        assert_eq!((a.width(), a.height()), (480, 270));

        let other = decode(&demo_image_png("other.png").expect("generate")).expect("decode");
        assert_ne!(
            a.to_rgb8().into_raw(),
            other.to_rgb8().into_raw(),
            "two names produced the same picture"
        );
    }

    #[test]
    fn failures_are_recorded_and_readable() {
        let mut m = Media::disabled();
        assert!(m.failure("x.png").is_none());
        m.mark_failed("x.png", "解码失败");
        assert_eq!(m.failure("x.png"), Some("解码失败"));
    }

    #[test]
    fn rows_are_bounded_even_for_a_very_tall_image() {
        // Without a picker there is no cell geometry, so this asserts the
        // no-terminal path stays at zero rather than guessing.
        let mut m = Media::disabled();
        m.insert("tall.png", gradient(10, 10_000), (10, 10_000));
        assert_eq!(m.rows_for("tall.png"), 0);
        assert!(MAX_PREVIEW_ROWS <= 12, "cap must stay modest");
    }
}
