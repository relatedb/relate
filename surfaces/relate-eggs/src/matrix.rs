//! `.matrix [text]`: digital rain that decrypts a short line into place.
//!
//! Plain ANSI, no alternate screen and no hidden cursor, so an interrupt
//! (Ctrl-C ends the process) can never leave the terminal in a bad mode:
//! every frame ends with a style reset, and the animation lives in a small
//! block of lines below the prompt that the finish clears. The run is
//! bounded: [`FRAMES`] frames at [`FRAME_MS`] each.

use std::io::Write;
use std::time::Duration;

use crate::rng::Rng;
use crate::{Ctx, Refusal};

const DEFAULT_TEXT: &str = "Relate: the graph database that lives in your process.";
const MAX_TEXT_CHARS: usize = 60;
const FRAMES: usize = 110;
const FRAME_MS: u64 = 40;
/// Frame at which the first character locks in; the last locks in 60
/// frames later, leaving a short hold at the end.
const REVEAL_START: usize = 30;
const REVEAL_SPAN: usize = 60;
const DEFAULT_SIZE: (usize, usize) = (80, 24);
const MAX_WIDTH: usize = 100;
const MAX_HEIGHT: usize = 12;

const KATAKANA: &str = "ｦｧｨｩｪｫｬｭｮｯｰｱｲｳｴｵｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾅﾆﾇﾈﾉﾊﾋﾌﾍﾎﾏﾐﾑﾒﾓﾔﾕﾖﾗﾘﾙﾚﾛﾜﾝ";
const ASCII_GLYPHS: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ:+=*<>";

/// Terminal columns and rows from `COLUMNS`/`LINES`, with a default where a
/// variable is missing or unusable (shells often do not export them).
fn term_size(env: &dyn Fn(&str) -> Option<String>) -> (usize, usize) {
    let get = |k: &str, default: usize| {
        env(k)
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(default)
    };
    (get("COLUMNS", DEFAULT_SIZE.0), get("LINES", DEFAULT_SIZE.1))
}

/// The animation block for a terminal of `cols` x `rows`: leaves room for
/// the prompt, and at least a text row with rain above and below it.
fn block_size(cols: usize, rows: usize) -> (usize, usize) {
    (
        cols.clamp(20, MAX_WIDTH),
        rows.saturating_sub(3).clamp(5, MAX_HEIGHT),
    )
}

struct Drop {
    head: i64,
    len: i64,
    period: usize,
}

impl Drop {
    fn spawn(rng: &mut Rng, height: usize) -> Drop {
        Drop {
            head: -(rng.below(height * 2) as i64),
            len: 4 + rng.below(height + 3) as i64,
            period: 1 + rng.below(2),
        }
    }
}

struct Rain {
    height: usize,
    drops: Vec<Drop>,
}

impl Rain {
    fn new(width: usize, height: usize, rng: &mut Rng) -> Rain {
        Rain {
            height,
            drops: (0..width).map(|_| Drop::spawn(rng, height)).collect(),
        }
    }

    fn step(&mut self, frame: usize, rng: &mut Rng) {
        for d in &mut self.drops {
            if frame % d.period == 0 {
                d.head += 1;
            }
            if d.head - d.len > self.height as i64 {
                *d = Drop::spawn(rng, self.height);
            }
        }
    }
}

/// Brightness of a rain cell, 0 = head (white) through 3 = dimmest.
fn shade(dist: i64) -> u8 {
    match dist {
        0 => 0,
        1..=2 => 1,
        3..=5 => 2,
        _ => 3,
    }
}

/// How a cell is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    Blank,
    Rain(u8),
    Locked,
}

const fn sgr(style: Style) -> &'static str {
    match style {
        Style::Blank => "\x1b[0m",
        Style::Rain(0) => "\x1b[0;97m",
        Style::Rain(1) => "\x1b[0;38;5;46m",
        Style::Rain(2) => "\x1b[0;38;5;34m",
        Style::Rain(_) => "\x1b[0;38;5;22m",
        Style::Locked => "\x1b[1;97m",
    }
}

/// The frame at which character `i` of `n` locks in.
fn reveal_frame(i: usize, n: usize) -> usize {
    REVEAL_START + i * REVEAL_SPAN / n
}

struct Look {
    colour: bool,
    glyphs: Vec<char>,
}

/// Render one frame as `rain.height` lines of exactly `rain.drops.len()`
/// visible cells, with `text` decrypting on the middle row.
fn render(rain: &Rain, frame: usize, text: &[char], look: &Look, rng: &mut Rng) -> Vec<String> {
    let (w, h) = (rain.drops.len(), rain.height);
    let mid = h / 2;
    let start = (w - text.len().min(w)) / 2;
    let mut lines = Vec::with_capacity(h);
    for y in 0..h {
        let mut line = String::new();
        let mut prev = None;
        for (x, drop) in rain.drops.iter().enumerate() {
            let dist = drop.head - y as i64;
            let on_text = y == mid && x >= start && x - start < text.len();
            let (ch, style) = if on_text {
                let i = x - start;
                if frame >= reveal_frame(i, text.len()) {
                    (text[i], Style::Locked)
                } else {
                    (look.glyphs[rng.below(look.glyphs.len())], Style::Rain(1))
                }
            } else if (0..drop.len).contains(&dist) {
                (
                    look.glyphs[rng.below(look.glyphs.len())],
                    Style::Rain(shade(dist)),
                )
            } else {
                (' ', Style::Blank)
            };
            if look.colour && prev != Some(style) {
                line.push_str(sgr(style));
                prev = Some(style);
            }
            line.push(ch);
        }
        if look.colour {
            line.push_str("\x1b[0m");
        }
        lines.push(line);
    }
    lines
}

/// The text to decrypt: control characters dropped, cut to the cap.
fn clean_text(args: &[&str]) -> Vec<char> {
    let joined = args.join(" ");
    let text = if joined.trim().is_empty() {
        DEFAULT_TEXT
    } else {
        joined.trim()
    };
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT_CHARS)
        .collect()
}

fn animate(
    out: &mut dyn Write,
    text: &[char],
    size: (usize, usize),
    look: &Look,
    pace: Duration,
) -> std::io::Result<()> {
    let (w, h) = size;
    let text = &text[..text.len().min(w)];
    let mut rng = Rng::from_clock();
    let mut rain = Rain::new(w, h, &mut rng);
    // Reserve the block, then redraw it in place by moving up over it.
    out.write_all("\n".repeat(h).as_bytes())?;
    for frame in 0..FRAMES {
        rain.step(frame, &mut rng);
        let mut buf = format!("\x1b[{h}A");
        for line in render(&rain, frame, text, look, &mut rng) {
            buf.push_str(&format!("\r{line}\n"));
        }
        out.write_all(buf.as_bytes())?;
        out.flush()?;
        std::thread::sleep(pace);
    }
    Ok(())
}

pub(crate) fn run(ctx: &mut Ctx<'_>, args: &[&str]) -> Result<(), Refusal> {
    let text = clean_text(args);
    if !ctx.caps.utf8 && !text.iter().all(char::is_ascii) {
        return Err(Refusal::NotUtf8);
    }
    let env = |k: &str| std::env::var(k).ok();
    let (cols, rows) = term_size(&env);
    let size = block_size(cols, rows);
    // The same clipped text the animation decrypts, so the final line
    // never wraps.
    let text = &text[..text.len().min(size.0)];
    let look = Look {
        colour: ctx.caps.truecolor,
        glyphs: if ctx.caps.utf8 {
            KATAKANA
        } else {
            ASCII_GLYPHS
        }
        .chars()
        .collect(),
    };
    let result = animate(ctx.out, text, size, &look, Duration::from_millis(FRAME_MS));
    // Whatever happened, leave the cursor on a fresh line below the art
    // with no style active, then replace the block by the decrypted line.
    let finish = format!(
        "\x1b[0m\x1b[{h}A{clear}\x1b[{h}A{line}\n",
        h = size.1,
        clear = "\x1b[2K\n".repeat(size.1),
        line = text.iter().collect::<String>(),
    );
    let tail = ctx
        .out
        .write_all(finish.as_bytes())
        .and_then(|()| ctx.out.flush());
    result.and(tail).map_err(|e| Refusal::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible(line: &str) -> String {
        let mut out = String::new();
        let mut esc = false;
        for c in line.chars() {
            match (esc, c) {
                (false, '\x1b') => esc = true,
                (true, 'm') => esc = false,
                (false, c) => out.push(c),
                _ => {}
            }
        }
        out
    }

    fn look(colour: bool) -> Look {
        Look {
            colour,
            glyphs: ASCII_GLYPHS.chars().collect(),
        }
    }

    #[test]
    fn size_comes_from_env_with_a_sane_default() {
        let env = |k: &str| match k {
            "COLUMNS" => Some("120".to_string()),
            "LINES" => Some("junk".to_string()),
            _ => None,
        };
        assert_eq!(term_size(&env), (120, DEFAULT_SIZE.1));
        assert_eq!(term_size(&|_| None), DEFAULT_SIZE);
        assert_eq!(term_size(&|_| Some("0".to_string())), DEFAULT_SIZE);
    }

    #[test]
    fn block_is_bounded_whatever_the_terminal() {
        assert_eq!(block_size(10_000, 10_000), (MAX_WIDTH, MAX_HEIGHT));
        assert_eq!(block_size(1, 1), (20, 5));
    }

    #[test]
    fn frames_have_fixed_geometry_and_no_escapes_without_colour() {
        let mut rng = Rng::seeded(7);
        let rain = Rain::new(30, 7, &mut rng);
        let text: Vec<char> = "hello".chars().collect();
        for colour in [false, true] {
            let lines = render(&rain, 0, &text, &look(colour), &mut rng);
            assert_eq!(lines.len(), 7);
            assert!(lines.iter().all(|l| visible(l).chars().count() == 30));
            assert_eq!(colour, lines.iter().any(|l| l.contains('\x1b')));
            if colour {
                assert!(lines.iter().all(|l| l.ends_with("\x1b[0m")));
            }
        }
    }

    #[test]
    fn text_decrypts_left_to_right_and_is_whole_at_the_end() {
        let mut rng = Rng::seeded(3);
        let rain = Rain::new(40, 7, &mut rng);
        let text: Vec<char> = "hello".chars().collect();
        let mut row = |frame| visible(&render(&rain, frame, &text, &look(false), &mut rng)[3]);
        assert!(!row(0).contains("hello"));
        assert!(row(FRAMES - 1).contains("hello"));
        let revealed = |frame: usize| {
            (0..text.len())
                .filter(|&i| frame >= reveal_frame(i, text.len()))
                .count()
        };
        assert!(revealed(REVEAL_START) < revealed(REVEAL_START + 30));
        assert_eq!(revealed(FRAMES - 1), text.len());
    }

    #[test]
    fn rain_recycles_drops_and_stays_in_bounds() {
        let mut rng = Rng::seeded(1);
        let mut rain = Rain::new(10, 6, &mut rng);
        for f in 0..500 {
            rain.step(f, &mut rng);
            assert!(rain.drops.iter().all(|d| d.head - d.len <= 6));
        }
    }

    #[test]
    fn text_is_cleaned_capped_and_defaulted() {
        assert_eq!(clean_text(&[]).iter().collect::<String>(), DEFAULT_TEXT);
        assert_eq!(
            clean_text(&["a\x1b[0m", "b"]).iter().collect::<String>(),
            "a[0m b"
        );
        assert_eq!(clean_text(&[&"z".repeat(500)]).len(), MAX_TEXT_CHARS);
    }

    #[test]
    fn animation_is_bounded_and_ends_with_a_reset_line() {
        let mut out = Vec::new();
        let text: Vec<char> = "hi".chars().collect();
        animate(&mut out, &text, (20, 5), &look(true), Duration::ZERO).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.matches("\x1b[5A").count() == FRAMES);
        assert!(s.len() < 1_000_000);
    }
}
