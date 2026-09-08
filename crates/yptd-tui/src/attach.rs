//! Turning a paste into attachments.
//!
//! Dragging files from a file manager into a terminal types their paths as if
//! somebody had entered them, shell-escaped and space-separated. So does
//! pasting a file copied in Finder or Nautilus. Recognising that is the whole
//! of "pick an attachment" for anyone sitting at the machine, and it costs
//! only the parsing below.
//!
//! A paste is only treated as attachments when every word in it is a file
//! that exists. Pasting a sentence has to stay a sentence.

use std::path::PathBuf;

/// Markers some desktops put on the clipboard alongside the paths.
const NOISE: [&str; 3] = ["copy", "cut", "x-special/gnome-copied-files"];

/// The paths a paste refers to, or `None` when it is ordinary text.
pub fn parse_paths(text: &str) -> Option<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || NOISE.contains(&line) {
            continue;
        }
        // A path with unescaped spaces is a whole line on its own; try that
        // before splitting, or `~/My Pictures/a.png` becomes two words.
        let whole = expand(unescape(line));
        if whole.is_file() {
            paths.push(whole);
            continue;
        }
        for word in split_words(line)? {
            let path = expand(unescape(&word));
            if !path.is_file() {
                return None;
            }
            paths.push(path);
        }
    }
    (!paths.is_empty()).then_some(paths)
}

/// Splits a line the way a shell would: whitespace separates words, quotes
/// and backslashes hold them together. Returns `None` for an unterminated
/// quote, which means the text was never a path list.
fn split_words(line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for c in line.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        match (c, quote) {
            ('\\', Some('\'')) => current.push(c),
            ('\\', _) => {
                escaped = true;
                current.push(c);
            }
            ('\'' | '"', None) => quote = Some(c),
            (c, Some(q)) if c == q => quote = None,
            (c, None) if c.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }
    if quote.is_some() || escaped {
        return None;
    }
    if !current.is_empty() {
        words.push(current);
    }
    (!words.is_empty()).then_some(words)
}

/// Removes one level of shell escaping and the `file://` wrapper a desktop
/// clipboard may add.
pub fn unescape(word: &str) -> String {
    let word = word.trim().trim_matches('\'').trim_matches('"');
    if let Some(rest) = word.strip_prefix("file://") {
        // A file URI is percent-encoded and may carry an empty host.
        return percent_decode(rest.strip_prefix("localhost").unwrap_or(rest));
    }
    let mut out = String::with_capacity(word.len());
    let mut chars = word.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Expands a leading `~/` and makes the path absolute against the working
/// directory, so a relative path typed by hand behaves like one in a shell.
pub fn expand(value: impl Into<String>) -> PathBuf {
    let value = value.into();
    let path = match value.strip_prefix("~/") {
        Some(rest) => match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => PathBuf::from(&value),
        },
        None => PathBuf::from(&value),
    };
    if path.is_absolute() {
        return path;
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sandbox(PathBuf);

    impl Sandbox {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("yptd-attach-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("sandbox");
            for file in ["one.png", "two.png", "有 空格.png"] {
                std::fs::write(root.join(file), b"x").expect("file");
            }
            Self(root)
        }
        fn path(&self, name: &str) -> String {
            self.0.join(name).to_string_lossy().into_owned()
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_single_dragged_path_becomes_one_attachment() {
        let s = Sandbox::new("single");
        let paths = parse_paths(&s.path("one.png")).expect("a path");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].file_name().unwrap(), "one.png");
    }

    #[test]
    fn dragging_several_files_gives_several_attachments() {
        // What a terminal inserts when more than one file is dropped at once.
        let s = Sandbox::new("many");
        let line = format!("{} {}", s.path("one.png"), s.path("two.png"));
        let paths = parse_paths(&line).expect("two paths");
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn a_backslash_escaped_space_is_one_path_not_two() {
        let s = Sandbox::new("escaped");
        let escaped = s.path("有 空格.png").replace(' ', "\\ ");
        let paths = parse_paths(&escaped).expect("one path");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].file_name().unwrap(), "有 空格.png");
    }

    #[test]
    fn a_quoted_path_with_spaces_survives() {
        let s = Sandbox::new("quoted");
        let quoted = format!("\"{}\" '{}'", s.path("有 空格.png"), s.path("one.png"));
        let paths = parse_paths(&quoted).expect("two paths");
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn an_unescaped_path_with_spaces_is_taken_whole() {
        let s = Sandbox::new("bare");
        let paths = parse_paths(&s.path("有 空格.png")).expect("one path");
        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn one_path_per_line_works_and_desktop_markers_are_ignored() {
        let s = Sandbox::new("lines");
        let text = format!(
            "x-special/gnome-copied-files\ncopy\n{}\n{}\n",
            s.path("one.png"),
            s.path("two.png")
        );
        assert_eq!(parse_paths(&text).expect("two paths").len(), 2);
    }

    #[test]
    fn a_file_uri_is_decoded() {
        let s = Sandbox::new("uri");
        let uri = format!("file://{}", s.path("有 空格.png").replace(' ', "%20"));
        let paths = parse_paths(&uri).expect("one path");
        assert_eq!(paths[0].file_name().unwrap(), "有 空格.png");
    }

    #[test]
    fn pasting_ordinary_text_stays_text() {
        assert!(parse_paths("这是一句话，不是路径").is_none());
        assert!(parse_paths("看看 /etc/passwd 这个文件").is_none(), "只有一部分是路径也不算");
        assert!(parse_paths("").is_none());
        assert!(parse_paths("   \n  \n").is_none());
    }

    #[test]
    fn a_path_that_does_not_exist_is_not_an_attachment() {
        assert!(parse_paths("/definitely/not/here.png").is_none());
    }

    #[test]
    fn an_unterminated_quote_is_not_a_path_list() {
        assert!(split_words("\"unclosed").is_none());
        assert!(split_words("trailing\\").is_none());
    }

    #[test]
    fn expanding_leaves_an_absolute_path_alone() {
        assert_eq!(expand("/tmp/a.png"), PathBuf::from("/tmp/a.png"));
        assert!(expand("a.png").is_absolute(), "relative paths gain the working directory");
    }
}
