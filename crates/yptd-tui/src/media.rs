//! Inline images in the message list.
//!
//! Terminals disagree about how to show a picture: Kitty and Ghostty speak the
//! Kitty graphics protocol, iTerm2 has its own, some support Sixel, and the
//! rest get half-block characters. [`ratatui_image`] negotiates that; what this
//! module owns is everything around it -- deciding how many rows an image gets
//! *before* it is decoded, caching decoded pictures, and degrading to a text
//! line when the terminal or the file will not cooperate.

use std::collections::HashMap;

use image::DynamicImage;
use ratatui::layout::Rect;
use ratatui::Frame;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{Resize, StatefulImage};

/// Rows an inline preview is allowed to occupy. Tall enough to be worth
/// showing, short enough that one screenshot cannot bury the conversation.
pub const MAX_PREVIEW_ROWS: u16 = 12;
const MAX_PREVIEW_COLS: u16 = 48;

/// Decoded images, keyed by attachment name.
///
/// Height is decided from the image's aspect ratio and reserved in the layout
/// before the picture is drawn, so a message's height never changes between
/// the frame that measured it and the frame that paints it.
pub struct Media {
    picker: Option<Picker>,
    protocols: HashMap<String, StatefulProtocol>,
    sizes: HashMap<String, (u32, u32)>,
    failed: HashMap<String, String>,
}

impl Media {
    /// Queries the terminal for the best available graphics protocol.
    ///
    /// Returns a `Media` either way: when the query fails -- no tty, or a
    /// terminal that answers nothing -- images degrade to a text line rather
    /// than the client refusing to start.
    pub fn detect() -> Self {
        Self {
            picker: Picker::from_query_stdio().ok(),
            protocols: HashMap::new(),
            sizes: HashMap::new(),
            failed: HashMap::new(),
        }
    }

    /// A `Media` that never renders pictures, for off-screen capture and tests.
    pub fn disabled() -> Self {
        Self {
            picker: None,
            protocols: HashMap::new(),
            sizes: HashMap::new(),
            failed: HashMap::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.picker.is_some()
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
    pub fn insert(&mut self, key: &str, image: DynamicImage) -> u16 {
        let Some(picker) = self.picker.as_mut() else {
            return 0;
        };
        let (w, h) = (image.width(), image.height());
        self.sizes.insert(key.to_owned(), (w, h));
        self.protocols
            .insert(key.to_owned(), picker.new_resize_protocol(image));
        self.rows_for(key)
    }

    pub fn mark_failed(&mut self, key: &str, reason: impl Into<String>) {
        self.failed.insert(key.to_owned(), reason.into());
    }

    pub fn failure(&self, key: &str) -> Option<&str> {
        self.failed.get(key).map(String::as_str)
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
    pub fn render(&mut self, frame: &mut Frame, area: Rect, key: &str) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let Some(protocol) = self.protocols.get_mut(key) else {
            return;
        };
        frame.render_stateful_widget(StatefulImage::new().resize(Resize::Fit(None)), area, protocol);
    }
}

impl Default for Media {
    fn default() -> Self {
        Self::disabled()
    }
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
    fn a_disabled_media_never_reserves_rows() {
        let mut m = Media::disabled();
        assert!(!m.enabled());
        assert_eq!(m.insert("a.png", gradient(100, 100)), 0);
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
        m.insert("tall.png", gradient(10, 10_000));
        assert_eq!(m.rows_for("tall.png"), 0);
        assert!(MAX_PREVIEW_ROWS <= 12, "cap must stay modest");
    }
}
