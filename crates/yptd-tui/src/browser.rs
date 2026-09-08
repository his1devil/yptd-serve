//! Picking pictures off the disk without leaving the keyboard.
//!
//! Dragging a file in from a file manager is quicker when you are sitting at
//! the machine, and it is the only gesture most people already know. This is
//! for everything else: an ssh session, a keyboard-only workflow, or simply
//! not knowing the path by heart. It shows directories and pictures and
//! nothing else, because it exists to answer one question.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyModifiers};

/// Extensions the client can decode and show. Anything else would upload
/// fine but draw as a filename, which is not what this list is for.
const IMAGE_EXTENSIONS: [&str; 8] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "avif", "ico"];

/// A guard against listing something like `/nix/store`. Well past any
/// directory a person keeps screenshots in.
const MAX_ENTRIES: usize = 5_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    /// The `..` row.
    Parent,
    Directory,
    Image,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub kind: Kind,
    pub bytes: u64,
}

#[derive(Clone)]
pub struct Browser {
    dir: PathBuf,
    entries: Vec<Entry>,
    pub filter: String,
    pub cursor: usize,
    pub selected: BTreeSet<PathBuf>,
    /// Why the last directory could not be read, shown in place of the list.
    pub error: Option<String>,
}

pub enum Verdict {
    Continue,
    Cancel,
    Confirm(Vec<PathBuf>),
}

impl Browser {
    /// Opens at `start`, falling back to somewhere sensible when it is not a
    /// readable directory.
    pub fn open(start: Option<&Path>) -> Self {
        let dir = start
            .map(Path::to_path_buf)
            .filter(|p| p.is_dir())
            .or_else(default_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let mut browser = Self {
            dir,
            entries: Vec::new(),
            filter: String::new(),
            cursor: 0,
            selected: BTreeSet::new(),
            error: None,
        };
        browser.reload();
        browser
    }

    /// A browser over a fixed list, for the off-screen captures and for tests
    /// that should not depend on what happens to be on this disk.
    pub fn synthetic(dir: impl Into<PathBuf>, entries: Vec<Entry>) -> Self {
        Self {
            dir: dir.into(),
            entries,
            filter: String::new(),
            cursor: 0,
            selected: BTreeSet::new(),
            error: None,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The directory as it should read in the title: `~` for the home prefix,
    /// because the absolute path is usually too long for the frame.
    pub fn dir_label(&self) -> String {
        let full = self.dir.to_string_lossy().into_owned();
        match dirs::home_dir().map(|h| h.to_string_lossy().into_owned()) {
            Some(home) if full == home => "~".to_owned(),
            Some(home) if full.starts_with(&format!("{home}/")) => {
                format!("~{}", &full[home.len()..])
            }
            _ => full,
        }
    }

    /// Rows matching the filter, in display order.
    pub fn visible(&self) -> Vec<&Entry> {
        let needle = self.filter.to_lowercase();
        self.entries
            .iter()
            .filter(|entry| {
                entry.kind == Kind::Parent
                    || needle.is_empty()
                    || entry.name.to_lowercase().contains(&needle)
            })
            .collect()
    }

    pub fn current(&self) -> Option<&Entry> {
        self.visible().get(self.cursor).copied()
    }

    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    pub fn is_selected(&self, entry: &Entry) -> bool {
        self.selected.contains(&entry.path)
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let count = self.visible().len();
        if count == 0 {
            self.cursor = 0;
            return;
        }
        let next = self.cursor as isize + delta;
        self.cursor = next.clamp(0, count as isize - 1) as usize;
    }

    /// Enters a directory, keeping whatever has already been ticked: picking
    /// two shots from one folder and a third from another is normal.
    pub fn enter(&mut self, path: &Path) {
        self.dir = path.to_path_buf();
        self.filter.clear();
        self.cursor = 0;
        self.reload();
    }

    pub fn go_up(&mut self) {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            let leaving = self.dir.clone();
            self.enter(&parent);
            // Land on the directory just left, so going up and back down is
            // not a hunt through a long list.
            if let Some(index) = self.visible().iter().position(|e| e.path == leaving) {
                self.cursor = index;
            }
        }
    }

    pub fn toggle(&mut self) {
        let Some(entry) = self.current().cloned() else {
            return;
        };
        if entry.kind != Kind::Image {
            return;
        }
        if !self.selected.remove(&entry.path) {
            self.selected.insert(entry.path);
        }
    }

    /// What confirming would send: everything ticked, or the picture under
    /// the cursor when nothing is.
    pub fn chosen(&self) -> Vec<PathBuf> {
        if !self.selected.is_empty() {
            return self.selected.iter().cloned().collect();
        }
        match self.current() {
            Some(entry) if entry.kind == Kind::Image => vec![entry.path.clone()],
            _ => Vec::new(),
        }
    }

    fn reload(&mut self) {
        self.entries.clear();
        self.error = None;
        if let Some(parent) = self.dir.parent() {
            self.entries.push(Entry {
                path: parent.to_path_buf(),
                name: "..".to_owned(),
                kind: Kind::Parent,
                bytes: 0,
            });
        }
        let reader = match std::fs::read_dir(&self.dir) {
            Ok(reader) => reader,
            Err(e) => {
                self.error = Some(format!("读不了这个目录: {e}"));
                return;
            }
        };

        let mut directories = Vec::new();
        let mut images = Vec::new();
        for entry in reader.flatten().take(MAX_ENTRIES) {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                directories.push(Entry { path, name, kind: Kind::Directory, bytes: 0 });
            } else if meta.is_file() && is_image(&path) {
                images.push(Entry { path, name, kind: Kind::Image, bytes: meta.len() });
            }
        }
        // Directories first, then pictures, each by name. Case-insensitive so
        // `IMG_1.png` and `img_2.png` do not end up in separate runs.
        directories.sort_by_key(|e| e.name.to_lowercase());
        images.sort_by_key(|e| e.name.to_lowercase());
        self.entries.extend(directories);
        self.entries.extend(images);
        self.cursor = self.cursor.min(self.visible().len().saturating_sub(1));
    }

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Verdict {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Esc => return Verdict::Cancel,
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('p' | 'k') if ctrl => self.move_cursor(-1),
            KeyCode::Char('n' | 'j') if ctrl => self.move_cursor(1),
            KeyCode::Left => self.go_up(),
            KeyCode::Right => {
                if let Some(entry) = self.current().cloned()
                    && entry.kind != Kind::Image
                {
                    self.enter(&entry.path);
                }
            }
            KeyCode::Enter => {
                match self.current().cloned() {
                    // A directory row means "go here", never "send this".
                    Some(entry) if entry.kind != Kind::Image => self.enter(&entry.path),
                    _ => {
                        let chosen = self.chosen();
                        if !chosen.is_empty() {
                            return Verdict::Confirm(chosen);
                        }
                    }
                }
            }
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Tab => self.toggle(),
            KeyCode::Char('u') if ctrl => {
                self.filter.clear();
                self.cursor = 0;
            }
            // Backspace edits the filter, and once it is empty it means
            // "back out of this directory" -- the same key doing the same
            // thing at two levels.
            KeyCode::Backspace => {
                if self.filter.pop().is_none() {
                    self.go_up();
                } else {
                    self.cursor = 0;
                }
            }
            KeyCode::Char(value) if !ctrl => {
                self.filter.push(value);
                self.cursor = 0;
            }
            _ => {}
        }
        Verdict::Continue
    }
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.as_str()))
}

/// Where to start when nothing better is known. Screenshots land on the
/// desktop on macOS and in Pictures on most Linux setups.
fn default_dir() -> Option<PathBuf> {
    let home = dirs::home_dir();
    let candidates = [
        dirs::desktop_dir(),
        dirs::picture_dir(),
        dirs::download_dir(),
        home.clone(),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.is_dir())
        .or_else(|| std::env::current_dir().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sandbox(PathBuf);

    impl Sandbox {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("yptd-browse-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("子目录")).expect("sandbox");
            for file in ["b.png", "a.PNG", "notes.txt", "clip.jpeg", ".hidden.png"] {
                std::fs::write(root.join(file), b"x").expect("file");
            }
            std::fs::write(root.join("子目录/inner.png"), b"x").expect("file");
            Self(root)
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn names(browser: &Browser) -> Vec<String> {
        browser.visible().iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn only_directories_and_pictures_are_listed_and_hidden_files_are_not() {
        let sandbox = Sandbox::new("listing");
        let browser = Browser::open(Some(&sandbox.0));
        assert_eq!(names(&browser), vec!["..", "子目录", "a.PNG", "b.png", "clip.jpeg"]);
    }

    #[test]
    fn typing_filters_and_the_parent_row_always_stays() {
        let mut sandbox = Browser::open(Some(&Sandbox::new("filter").0));
        sandbox.handle_key(KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(names(&sandbox), vec!["..", "clip.jpeg"]);
        sandbox.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        assert!(names(&sandbox).len() > 2, "backspace restored the list");
    }

    #[test]
    fn enter_on_a_directory_descends_rather_than_sending() {
        let sandbox = Sandbox::new("descend");
        let mut browser = Browser::open(Some(&sandbox.0));
        browser.cursor = 1; // 子目录
        assert!(matches!(
            browser.handle_key(KeyCode::Enter, KeyModifiers::NONE),
            Verdict::Continue
        ));
        assert_eq!(browser.dir().file_name().unwrap(), "子目录");
        assert_eq!(names(&browser), vec!["..", "inner.png"]);
    }

    #[test]
    fn backspace_on_an_empty_filter_backs_out_and_lands_on_where_it_was() {
        let sandbox = Sandbox::new("updown");
        let mut browser = Browser::open(Some(&sandbox.0));
        browser.cursor = 1;
        browser.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(browser.dir().file_name().unwrap(), "子目录");
        browser.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(browser.dir(), sandbox.0);
        assert_eq!(
            browser.current().map(|e| e.name.as_str()),
            Some("子目录"),
            "coming back up lands on the directory just left"
        );
    }

    #[test]
    fn selection_survives_changing_directory() {
        let sandbox = Sandbox::new("carry");
        let mut browser = Browser::open(Some(&sandbox.0));
        browser.cursor = 2; // a.PNG
        browser.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(browser.selected_count(), 1);
        browser.cursor = 1; // 子目录
        browser.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        browser.cursor = 1; // inner.png
        browser.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        match browser.handle_key(KeyCode::Enter, KeyModifiers::NONE) {
            Verdict::Confirm(paths) => assert_eq!(paths.len(), 2, "one from each directory"),
            _ => panic!("enter with ticks must confirm"),
        }
    }

    #[test]
    fn a_directory_cannot_be_ticked() {
        let mut browser = Browser::open(Some(&Sandbox::new("nodirs").0));
        browser.cursor = 1; // 子目录
        browser.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(browser.selected_count(), 0);
    }

    #[test]
    fn enter_with_nothing_ticked_sends_the_picture_under_the_cursor() {
        let sandbox = Sandbox::new("cursoronly");
        let mut browser = Browser::open(Some(&sandbox.0));
        browser.cursor = 2; // a.PNG
        match browser.handle_key(KeyCode::Enter, KeyModifiers::NONE) {
            Verdict::Confirm(paths) => {
                assert_eq!(paths.len(), 1);
                assert_eq!(paths[0].file_name().unwrap(), "a.PNG");
            }
            _ => panic!("enter on a picture must confirm"),
        }
    }

    #[test]
    fn an_unreadable_directory_reports_itself_instead_of_panicking() {
        let missing = std::env::temp_dir().join("yptd-browse-does-not-exist");
        let _ = std::fs::remove_dir_all(&missing);
        // `open` falls back when the start is not a directory, so aim the
        // failure at `enter`, which is what a stale row would do.
        let mut browser = Browser::open(None);
        browser.enter(&missing);
        assert!(browser.error.is_some());
        assert!(browser.chosen().is_empty());
        assert!(matches!(
            browser.handle_key(KeyCode::Enter, KeyModifiers::NONE),
            Verdict::Continue
        ));
    }

    #[test]
    fn the_home_prefix_is_shortened_for_the_title() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let browser = Browser::open(Some(&home));
        assert_eq!(browser.dir_label(), "~");
    }

    #[test]
    fn the_cursor_clamps_instead_of_running_off_the_list() {
        let mut browser = Browser::open(Some(&Sandbox::new("clamp").0));
        browser.move_cursor(-9);
        assert_eq!(browser.cursor, 0);
        browser.move_cursor(99);
        assert_eq!(browser.cursor, browser.visible().len() - 1);
    }
}
