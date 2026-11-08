//! `.cowsay [text]`: Archie says the text in a speech bubble. The sprite is
//! rendered by the shared sprite kit; the bubble is plain ASCII.

use crate::sprite::Sprite;
use crate::{Ctx, Refusal};

const ARCHIE: &str = include_str!("../assets/cowsay-archie.txt");

const DEFAULT_TEXT: &str = "Hello from Relate!";
/// Widest text column inside the bubble, in characters.
const BUBBLE_WIDTH: usize = 40;
/// Input cap: longer text is cut, so output stays bounded.
const MAX_CHARS: usize = 400;
const INDENT: &str = "    ";

/// Greedy word wrap to `width` characters; words longer than a line are
/// split. Width is counted in `char`s (wide glyphs may overshoot).
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0;
    for word in text.split_whitespace() {
        let mut chars: Vec<char> = word.chars().collect();
        while !chars.is_empty() {
            let sep = usize::from(cur_len > 0);
            if cur_len + sep + chars.len() <= width {
                if sep == 1 {
                    cur.push(' ');
                }
                cur_len += sep + chars.len();
                cur.extend(chars.drain(..));
            } else if cur_len > 0 {
                lines.push(std::mem::take(&mut cur));
                cur_len = 0;
            } else {
                let rest = chars.split_off(width);
                lines.push(chars.iter().collect());
                chars = rest;
            }
        }
    }
    if cur_len > 0 {
        lines.push(cur);
    }
    lines
}

/// The speech bubble with its two-line tail.
fn bubble(lines: &[String]) -> Vec<String> {
    let Some(last) = lines.len().checked_sub(1) else {
        return Vec::new();
    };
    let w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let mut out = vec![format!(" {}", "_".repeat(w + 2))];
    for (i, l) in lines.iter().enumerate() {
        let (open, close) = match (lines.len(), i) {
            (1, _) => ('<', '>'),
            (_, 0) => ('/', '\\'),
            (_, i) if i == last => ('\\', '/'),
            _ => ('|', '|'),
        };
        let pad = " ".repeat(w - l.chars().count());
        out.push(format!("{open} {l}{pad} {close}"));
    }
    out.push(format!(" {}", "-".repeat(w + 2)));
    out.push("  \\".to_string());
    out.push("   \\".to_string());
    out
}

/// Everything `.cowsay` prints for `args`, as lines.
fn compose(args: &[&str], caps: crate::Caps) -> Result<Vec<String>, Refusal> {
    if !caps.utf8 {
        return Err(Refusal::NotUtf8);
    }
    let cleaned: String = args
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_CHARS)
        .collect();
    let text = if cleaned.trim().is_empty() {
        DEFAULT_TEXT
    } else {
        &cleaned
    };
    let sprite = Sprite::parse(ARCHIE).expect("embedded cowsay sprite parses");
    let art = if caps.truecolor {
        sprite.render_truecolor()
    } else {
        sprite.render_plain()
    };
    let mut out = bubble(&wrap(text, BUBBLE_WIDTH));
    out.extend(art.into_iter().map(|l| format!("{INDENT}{}", l.trim_end())));
    Ok(out)
}

pub(crate) fn run(ctx: &mut Ctx<'_>, args: &[&str]) -> Result<(), Refusal> {
    let mut text = compose(args, ctx.caps)?.join("\n");
    text.push('\n');
    ctx.out
        .write_all(text.as_bytes())
        .map_err(|e| Refusal::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Caps;

    #[test]
    fn wrap_breaks_on_words_and_splits_long_ones() {
        assert_eq!(wrap("aa bb cc", 5), vec!["aa bb", "cc"]);
        assert_eq!(wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
        assert_eq!(wrap("ab abcdefg", 4), vec!["ab", "abcd", "efg"]);
        assert!(wrap("   ", 5).is_empty());
    }

    #[test]
    fn wrap_counts_chars_not_bytes() {
        let lines = wrap("ééééé ññññ", 5);
        assert_eq!(lines, vec!["ééééé", "ññññ"]);
    }

    #[test]
    fn empty_bubble_is_empty_not_a_panic() {
        assert!(bubble(&[]).is_empty());
    }

    #[test]
    fn single_line_bubble_uses_angle_brackets() {
        let b = bubble(&["hi".to_string()]);
        assert_eq!(b[0], " ____");
        assert_eq!(b[1], "< hi >");
        assert_eq!(b[2], " ----");
    }

    #[test]
    fn multi_line_bubble_pads_and_frames() {
        let b = bubble(&["aaa".to_string(), "b".to_string(), "cc".to_string()]);
        assert_eq!(b[1], "/ aaa \\");
        assert_eq!(b[2], "| b   |");
        assert_eq!(b[3], "\\ cc  /");
    }

    #[test]
    fn long_text_is_bounded_and_control_chars_dropped() {
        let caps = Caps {
            utf8: true,
            truecolor: false,
        };
        let long = "x".repeat(10_000);
        let out = compose(&[&long, "\x1b[31m"], caps).unwrap();
        // 400 chars at 40 per line is 10 lines, plus frame, tail and art.
        assert!(out.len() < 40, "{}", out.len());
        assert!(out.iter().all(|l| !l.contains('\x1b')));
    }

    #[test]
    fn no_text_says_the_default_and_non_utf8_refuses() {
        let utf8 = Caps {
            utf8: true,
            truecolor: false,
        };
        assert!(compose(&[], utf8).unwrap()[1].contains(DEFAULT_TEXT));
        let ascii = Caps {
            utf8: false,
            truecolor: false,
        };
        assert_eq!(compose(&[], ascii).unwrap_err(), Refusal::NotUtf8);
    }
}
