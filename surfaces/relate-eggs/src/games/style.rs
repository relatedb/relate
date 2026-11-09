//! How the games colour a frame: the [`Style`] cell, the colour blend, and
//! the wall colour the walled games share. Colour is the games' concern,
//! not the terminal loop's, so it lives apart from [`super::term`].

use crate::sprite::{bg, Rgb};
use crate::{Caps, Refusal};

/// The wall colour of the walled games (`.snake`, `.tetris`).
pub(super) const WALL: Rgb = Rgb(0x58, 0x58, 0x58);

/// How games colour a two-column cell: 24-bit background when the terminal
/// offers it, an ASCII stand-in otherwise (`NO_COLOR`, dumb terminals).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Style {
    pub color: bool,
}

/// The glyph repertoire a game draws with: the one input to the games'
/// plain-glyph/locale policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Glyphs {
    /// ASCII text and coloured blanks only: runs in any locale.
    Ascii,
    /// Non-ASCII glyphs (braille): needs a UTF-8 locale.
    Utf8,
}

impl Style {
    /// The style for a game drawing with `glyphs` under `caps`: the one
    /// place the games decide colour versus ASCII stand-in, and refuse a
    /// locale that cannot show the glyphs.
    pub fn for_game(caps: Caps, glyphs: Glyphs) -> Result<Style, Refusal> {
        if glyphs == Glyphs::Utf8 && !caps.utf8 {
            return Err(Refusal::NotUtf8);
        }
        Ok(Style {
            color: caps.truecolor,
        })
    }

    /// A two-column cell: coloured blanks, or `mono` (two ASCII columns).
    pub fn cell(&self, rgb: Rgb, mono: &str) -> String {
        if self.color {
            format!("{}  \x1b[0m", bg(rgb))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_hits_both_ends() {
        let (a, b) = (Rgb(0, 100, 200), Rgb(100, 0, 100));
        assert_eq!(lerp(a, b, 0, 4), a);
        assert_eq!(lerp(a, b, 4, 4), b);
        assert_eq!(lerp(a, b, 2, 4), Rgb(50, 50, 150));
    }

    #[test]
    fn only_utf8_games_refuse_a_non_utf8_locale() {
        let caps = Caps {
            utf8: false,
            truecolor: true,
        };
        assert!(Style::for_game(caps, Glyphs::Ascii).unwrap().color);
        assert_eq!(
            Style::for_game(caps, Glyphs::Utf8).unwrap_err(),
            Refusal::NotUtf8
        );
        let utf8 = Caps { utf8: true, ..caps };
        assert!(Style::for_game(utf8, Glyphs::Utf8).is_ok());
    }

    #[test]
    fn cells_fall_back_to_ascii_without_colour() {
        let mono = Style { color: false };
        assert_eq!(mono.cell(Rgb(1, 2, 3), "[]"), "[]");
        let colour = Style { color: true };
        assert!(colour.cell(Rgb(1, 2, 3), "[]").contains("48;2;1;2;3"));
    }
}
