//! Syntax highlighting for fenced code blocks.
//!
//! This is the one place a span carries a literal color instead of a theme
//! group name. Syntect owns code coloring: its themes encode relationships
//! between token classes that a dozen highlight groups could not express, and
//! remapping them onto our vocabulary would lose more than it gained. Every
//! other color in the client still comes from a named group.

use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use tui_richtext::{Span, SpanKind};

struct Assets {
    syntaxes: SyntaxSet,
    dark: Theme,
    light: Theme,
}

fn assets() -> &'static Assets {
    static ASSETS: OnceLock<Assets> = OnceLock::new();
    ASSETS.get_or_init(|| {
        let themes = ThemeSet::load_defaults();
        Assets {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            dark: themes.themes["base16-ocean.dark"].clone(),
            light: themes.themes["InspiredGitHub"].clone(),
        }
    })
}

/// Which syntect theme to use. The terminal's own background decides: a dark
/// theme's colors are unreadable on a light ground and the reverse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Palette {
    Dark,
    Light,
}

/// Whether a language tag maps to a syntax we can actually highlight, so the
/// caller can decide to render a plain block rather than pretending.
pub fn supported(language: Option<&str>) -> bool {
    resolve(language).is_some()
}

/// Fence labels syntect's default set does not know, mapped to the closest
/// syntax it does.
///
/// TypeScript highlights acceptably as JavaScript -- the token classes that
/// differ (type annotations, `interface`) are a small share of a chat snippet,
/// and plain grey is worse. Labels with no honest neighbour (`toml`,
/// `dockerfile`, `swift`, `kotlin`) are deliberately absent: rendering them as
/// something else would color keywords that are not keywords.
fn alias(token: &str) -> Option<&'static str> {
    Some(match token {
        "ts" | "tsx" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "sh" | "shell" | "console" | "terminal" => "Bourne Again Shell (bash)",
        "yml" => "YAML",
        "golang" => "Go",
        "py3" | "python3" => "Python",
        "text" | "txt" | "plain" | "log" => "Plain Text",
        _ => return None,
    })
}

fn resolve(language: Option<&str>) -> Option<&'static syntect::parsing::SyntaxReference> {
    let a = assets();
    let token = language?.trim();
    if token.is_empty() {
        return None;
    }
    let lower = token.to_ascii_lowercase();
    // Try the fence label as a token ("rs", "sh"), then as a name ("Rust"),
    // then as an extension, then through the alias table.
    a.syntaxes
        .find_syntax_by_token(token)
        .or_else(|| a.syntaxes.find_syntax_by_name(token))
        .or_else(|| a.syntaxes.find_syntax_by_extension(token))
        .or_else(|| alias(&lower).and_then(|name| a.syntaxes.find_syntax_by_name(name)))
}

/// Highlights a whole code block, returning one span list per input line.
///
/// The block is processed as a unit rather than line by line, because syntect
/// carries parser state across lines: a string literal or block comment that
/// opens on one line and closes three lines later only colors correctly when
/// the lines are fed in order.
pub fn highlight(language: Option<&str>, lines: &[String], palette: Palette) -> Vec<Vec<Span>> {
    let Some(syntax) = resolve(language) else {
        return vec![Vec::new(); lines.len()];
    };
    let a = assets();
    let theme = match palette {
        Palette::Dark => &a.dark,
        Palette::Light => &a.light,
    };

    let source = lines.join("\n") + "\n";
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut out = Vec::with_capacity(lines.len());

    for line in LinesWithEndings::from(&source) {
        let Ok(regions) = highlighter.highlight_line(line, &a.syntaxes) else {
            out.push(Vec::new());
            continue;
        };
        let mut spans = Vec::new();
        let mut offset = 0usize;
        for (style, text) in regions {
            let end = offset + text.len();
            // The trailing newline is not part of the rendered line.
            let visible_end = end.min(line.trim_end_matches('\n').len());
            if offset < visible_end {
                let c = style.foreground;
                let rgb = (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
                spans.push(Span::new(offset, visible_end, SpanKind::Syntax(rgb)));
            }
            offset = end;
        }
        out.push(spans);
        if out.len() == lines.len() {
            break;
        }
    }
    out.resize(lines.len(), Vec::new());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(source: &str) -> Vec<String> {
        source.lines().map(str::to_owned).collect()
    }

    #[test]
    fn common_fence_labels_resolve() {
        for lang in [
            "rust", "rs", "bash", "sh", "zsh", "python", "py", "go", "golang", "json", "yaml",
            "yml", "js", "ts", "tsx", "html", "css", "sql", "java", "c", "cpp", "ruby", "php",
            "xml", "markdown", "md", "diff", "makefile", "lua",
        ] {
            assert!(supported(Some(lang)), "{lang} should resolve");
        }
    }

    #[test]
    fn labels_with_no_honest_neighbour_render_plain() {
        // syntect's default set has no syntax for these, and mapping them to
        // something adjacent would color the wrong tokens. Plain is correct.
        for lang in ["toml", "dockerfile", "ini", "swift", "kotlin"] {
            assert!(!supported(Some(lang)), "{lang} should not be faked");
        }
    }

    #[test]
    fn typescript_borrows_the_javascript_syntax() {
        let src = lines("const x: number = 42;");
        let spans = highlight(Some("ts"), &src, Palette::Dark);
        assert!(!spans[0].is_empty(), "ts fell through to plain");
    }

    #[test]
    fn an_unknown_or_missing_language_is_reported_not_guessed() {
        assert!(!supported(None));
        assert!(!supported(Some("")));
        assert!(!supported(Some("definitely-not-a-language")));
    }

    #[test]
    fn highlighting_returns_one_span_list_per_line() {
        let src = lines("fn main() {\n    println!(\"hi\");\n}");
        let spans = highlight(Some("rust"), &src, Palette::Dark);
        assert_eq!(spans.len(), src.len());
        assert!(spans.iter().any(|s| !s.is_empty()), "nothing was highlighted");
    }

    #[test]
    fn spans_stay_inside_their_line_and_on_character_boundaries() {
        let src = lines("let s = \"中文字符串\";\nlet n = 42; // 注释");
        for (index, spans) in highlight(Some("rust"), &src, Palette::Dark).iter().enumerate() {
            let line = &src[index];
            for span in spans {
                assert!(span.end <= line.len(), "line {index}: {span:?} past end");
                assert!(span.start < span.end, "line {index}: empty {span:?}");
                assert!(
                    line.is_char_boundary(span.start) && line.is_char_boundary(span.end),
                    "line {index}: {span:?} splits a character in {line:?}"
                );
            }
        }
    }

    #[test]
    fn parser_state_carries_across_lines() {
        // The string opens on line 0 and closes on line 2; the middle line is
        // only colored as string content if state was carried.
        let src = lines("let s = \"\nstill inside\n\";");
        let spans = highlight(Some("rust"), &src, Palette::Dark);
        assert!(
            !spans[1].is_empty(),
            "the continuation line lost its highlighting"
        );
    }

    #[test]
    fn an_unknown_language_yields_empty_spans_of_the_right_shape() {
        let src = lines("a\nb\nc");
        let spans = highlight(Some("nope"), &src, Palette::Dark);
        assert_eq!(spans.len(), 3);
        assert!(spans.iter().all(Vec::is_empty));
    }

    #[test]
    fn the_two_palettes_differ() {
        let src = lines("fn main() {}");
        let dark = highlight(Some("rust"), &src, Palette::Dark);
        let light = highlight(Some("rust"), &src, Palette::Light);
        assert_ne!(dark, light, "both palettes produced identical colors");
    }

    #[test]
    fn an_empty_block_does_not_panic() {
        assert!(highlight(Some("rust"), &[], Palette::Dark).is_empty());
    }
}
