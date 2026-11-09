//! The shared terminal and game loop for the full-screen eggs (ADR-0547).
//!
//! [`play`] puts the terminal in raw mode on the alternate screen behind a
//! guard that restores it on every exit path: normal return, error, Ctrl-C
//! (raw mode delivers it as a key, so the loop quits cleanly) and a panic
//! in the game (caught, the terminal restored, then reported as an error).
//! Drawing is a full redraw of the frame the game renders; work and output
//! per frame are bounded by the terminal size.

use std::io::{IsTerminal, Write};
use std::panic::{self, AssertUnwindSafe};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{cursor, execute};

use crate::sprite::Rgb;
use crate::{Caps, Ctx, Refusal};

/// A key press, with WASD folded into the arrows and letters lower-cased.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Up,
    Down,
    Left,
    Right,
    Esc,
    Enter,
    Char(char),
}

/// What a game answers to a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flow {
    Continue,
    Quit,
}

/// One game, driven by [`play`]: a clock tick, keys, and a frame.
pub(crate) trait Game {
    /// Columns and rows the frame needs; below this the loop shows a notice
    /// instead of the frame and stops ticking.
    fn min_size(&self) -> (u16, u16);
    /// Time until the next tick (may change as the game speeds up).
    fn interval(&self) -> Duration;
    fn tick(&mut self);
    fn key(&mut self, key: Key) -> Flow;
    /// One string per terminal row, each at most `min_size().0` columns.
    fn render(&self) -> Vec<String>;
}

/// How games colour a two-column cell: 24-bit background when the terminal
/// offers it, an ASCII stand-in otherwise (`NO_COLOR`, dumb terminals).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Style {
    pub color: bool,
}

impl Style {
    pub fn from_caps(caps: Caps) -> Style {
        Style {
            color: caps.truecolor,
        }
    }

    /// A two-column cell: coloured blanks, or `mono` (two ASCII columns).
    pub fn cell(&self, rgb: Rgb, mono: &str) -> String {
        if self.color {
            format!("\x1b[48;2;{};{};{}m  \x1b[0m", rgb.0, rgb.1, rgb.2)
        } else {
            mono.to_string()
        }
    }
}

/// Linear blend of two colours; `t` in `0..=num` out of `den` (`den > 0`).
pub(crate) fn lerp(a: Rgb, b: Rgb, num: usize, den: usize) -> Rgb {
    let mix = |x: u8, y: u8| {
        let (x, y, n, d) = (x as i64, y as i64, num as i64, den as i64);
        (x + (y - x) * n / d) as u8
    };
    Rgb(mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

/// Cut a line to at most `cols` characters.
pub(crate) fn fit(s: &str, cols: usize) -> String {
    s.chars().take(cols).collect()
}

/// The terminal's size, once stdin and stdout are known to be terminals
/// and the window is at least `min` (columns, rows).
pub(crate) fn terminal_size(min: (u16, u16)) -> Result<(u16, u16), Refusal> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(Refusal::NotATerminal);
    }
    let have = crossterm::terminal::size().map_err(|e| Refusal::Io(e.to_string()))?;
    if have.0 < min.0 || have.1 < min.1 {
        return Err(Refusal::TooSmall { need: min, have });
    }
    Ok(have)
}

/// Restores the terminal when dropped. Restoring twice is harmless.
struct TermGuard;

impl TermGuard {
    fn enter() -> std::io::Result<TermGuard> {
        enable_raw_mode()?;
        let guard = TermGuard;
        execute!(std::io::stdout(), EnterAlternateScreen, cursor::Hide)?;
        Ok(guard)
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        // Keys mashed after the game ended must not leak into the shell.
        while event::poll(Duration::ZERO).unwrap_or(false) {
            if event::read().is_err() {
                break;
            }
        }
        let _ = execute!(std::io::stdout(), cursor::Show, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// Translate a key event; `None` for keys no game uses.
fn map_key(ev: KeyEvent) -> Option<Key> {
    Some(match ev.code {
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Esc => Key::Esc,
        KeyCode::Enter => Key::Enter,
        KeyCode::Char(c) => match c.to_ascii_lowercase() {
            'w' => Key::Up,
            'a' => Key::Left,
            's' => Key::Down,
            'd' => Key::Right,
            c => Key::Char(c),
        },
        _ => return None,
    })
}

fn is_interrupt(ev: &KeyEvent) -> bool {
    ev.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(ev.code, KeyCode::Char('c') | KeyCode::Char('C'))
}

/// One frame as a single write: home, each row cleared to its end, the rest
/// of the screen cleared. `clear_all` wipes the screen first (after resize).
fn frame_bytes(lines: &[String], rows: u16, clear_all: bool) -> String {
    let mut buf = String::new();
    if clear_all {
        buf.push_str("\x1b[2J");
    }
    buf.push_str("\x1b[H");
    for (i, line) in lines.iter().take(rows as usize).enumerate() {
        if i > 0 {
            buf.push_str("\r\n");
        }
        buf.push_str(line);
        buf.push_str("\x1b[0m\x1b[K");
    }
    buf.push_str("\x1b[J");
    buf
}

fn too_small_lines(min: (u16, u16)) -> Vec<String> {
    vec![format!(
        "window too small: need {}x{} (q quits)",
        min.0, min.1
    )]
}

fn run_loop(out: &mut dyn Write, game: &mut dyn Game) -> std::io::Result<()> {
    let (mut cols, mut rows) = crossterm::terminal::size()?;
    let mut clear_all = true;
    let mut dirty = true;
    let mut next = Instant::now() + game.interval();
    loop {
        let min = game.min_size();
        let small = cols < min.0 || rows < min.1;
        if dirty {
            let lines = if small {
                too_small_lines(min)
            } else {
                game.render()
            };
            out.write_all(frame_bytes(&lines, rows, clear_all).as_bytes())?;
            out.flush()?;
            clear_all = false;
            dirty = false;
        }
        if event::poll(next.saturating_duration_since(Instant::now()))? {
            match event::read()? {
                Event::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                    if is_interrupt(&k) {
                        return Ok(());
                    }
                    if let Some(key) = map_key(k) {
                        if game.key(key) == Flow::Quit {
                            return Ok(());
                        }
                        dirty = true;
                    }
                }
                Event::Resize(c, r) => {
                    (cols, rows) = (c, r);
                    clear_all = true;
                    dirty = true;
                }
                _ => {}
            }
        }
        if Instant::now() >= next {
            if !small {
                game.tick();
                dirty = true;
            }
            next = Instant::now() + game.interval();
        }
    }
}

/// Run `game` full-screen until it quits. The terminal is restored before
/// this returns, whatever happened; a panic in the game becomes an error.
pub(crate) fn play(ctx: &mut Ctx<'_>, game: &mut dyn Game) -> Result<(), Refusal> {
    terminal_size(game.min_size())?;
    let io_err = |e: std::io::Error| Refusal::Io(e.to_string());
    let guard = TermGuard::enter().map_err(io_err)?;
    // The default hook would print into the alternate screen, where the
    // restore wipes it: stay quiet while the game runs, report afterwards.
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let result = panic::catch_unwind(AssertUnwindSafe(|| run_loop(&mut *ctx.out, game)));
    drop(guard);
    panic::set_hook(hook);
    match result {
        Ok(r) => r.map_err(io_err),
        Err(payload) => {
            let why = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
            Err(Refusal::Io(match why {
                Some(msg) => format!("the game crashed: {msg}"),
                None => "the game crashed".to_string(),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn wasd_fold_into_arrows_and_letters_lower_case() {
        assert_eq!(map_key(press(KeyCode::Char('W'))), Some(Key::Up));
        assert_eq!(map_key(press(KeyCode::Char('a'))), Some(Key::Left));
        assert_eq!(map_key(press(KeyCode::Char('Q'))), Some(Key::Char('q')));
        assert_eq!(map_key(press(KeyCode::Down)), Some(Key::Down));
        assert_eq!(map_key(press(KeyCode::F(1))), None);
    }

    #[test]
    fn ctrl_c_is_an_interrupt() {
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(is_interrupt(&ev));
        assert!(!is_interrupt(&press(KeyCode::Char('c'))));
    }

    #[test]
    fn frames_home_clear_and_stay_within_the_rows() {
        let lines: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
        let f = frame_bytes(&lines, 2, false);
        assert!(f.starts_with("\x1b[H"));
        assert!(f.contains('a') && f.contains('b') && !f.contains('c'));
        assert!(f.ends_with("\x1b[J"));
        assert!(frame_bytes(&lines, 3, true).starts_with("\x1b[2J"));
    }

    #[test]
    fn lerp_hits_both_ends() {
        let (a, b) = (Rgb(0, 100, 200), Rgb(100, 0, 100));
        assert_eq!(lerp(a, b, 0, 4), a);
        assert_eq!(lerp(a, b, 4, 4), b);
        assert_eq!(lerp(a, b, 2, 4), Rgb(50, 50, 150));
    }

    #[test]
    fn cells_fall_back_to_ascii_without_colour() {
        let mono = Style { color: false };
        assert_eq!(mono.cell(Rgb(1, 2, 3), "[]"), "[]");
        let colour = Style { color: true };
        assert!(colour.cell(Rgb(1, 2, 3), "[]").contains("48;2;1;2;3"));
    }

    #[test]
    fn fit_cuts_by_characters() {
        assert_eq!(fit("héllo", 3), "hél");
        assert_eq!(fit("ab", 5), "ab");
    }
}
