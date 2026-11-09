//! Text-file sprites: one character per pixel, one line per row. The parser
//! and both renderers (24-bit half-block ANSI, plain shade characters) live
//! here; the legend is the single [`TONES`] table, so a later variant that
//! adds an eye, smile, node or string tone adds one row and nothing else.

use std::fmt;

/// One 24-bit colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// The three Archie tone colours, the brand's logomark values (`--mark-hl`
/// #4c99a4, `--mark-md` #0a6190, `--mark-sh` #014063). The one constant that
/// names a colour; nothing else does.
pub const BRAND_PALETTE: Palette = Palette {
    hl: Rgb(0x4c, 0x99, 0xa4),
    md: Rgb(0x0a, 0x61, 0x90),
    sh: Rgb(0x01, 0x40, 0x63),
};

/// A highlight / mid / shadow colour triple.
pub struct Palette {
    pub hl: Rgb,
    pub md: Rgb,
    pub sh: Rgb,
}

/// One legend row: the file symbol, its true colour, and its plain-text
/// shade glyph.
#[derive(Debug)]
pub struct Tone {
    pub symbol: char,
    pub rgb: Rgb,
    pub shade: char,
}

/// The legend: every non-empty symbol a sprite file may use. Empty cells are
/// [`EMPTY_SYMBOLS`]. Later variants extend this table (`e` eye, `k` smile,
/// `n` node, `w` string).
pub const TONES: &[Tone] = &[
    Tone {
        symbol: 'h',
        rgb: BRAND_PALETTE.hl,
        shade: '\u{2593}',
    },
    Tone {
        symbol: 'm',
        rgb: BRAND_PALETTE.md,
        shade: '\u{2592}',
    },
    Tone {
        symbol: 's',
        rgb: BRAND_PALETTE.sh,
        shade: '\u{2591}',
    },
];

/// Symbols that mean "no pixel".
const EMPTY_SYMBOLS: [char; 2] = [' ', '.'];

type Cell = Option<&'static Tone>;

/// A parsed, rectangular pixel grid.
#[derive(Debug)]
pub struct Sprite {
    rows: Vec<Vec<Cell>>,
}

/// Why a sprite file failed to parse.
#[derive(Debug, PartialEq, Eq)]
pub enum SpriteError {
    NoRows,
    Ragged {
        row: usize,
        width: usize,
        expected: usize,
    },
    UnknownSymbol {
        row: usize,
        col: usize,
        symbol: char,
    },
}

impl fmt::Display for SpriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpriteError::NoRows => write!(f, "sprite has no pixel rows"),
            SpriteError::Ragged {
                row,
                width,
                expected,
            } => write!(f, "row {row} is {width} wide, expected {expected}"),
            SpriteError::UnknownSymbol { row, col, symbol } => {
                write!(f, "unknown symbol {symbol:?} at row {row}, column {col}")
            }
        }
    }
}

fn cell_for(symbol: char) -> Option<Cell> {
    if EMPTY_SYMBOLS.contains(&symbol) {
        return Some(None);
    }
    TONES.iter().find(|t| t.symbol == symbol).map(Some)
}

impl Sprite {
    /// Parse a sprite file: `#` lines are comments, every other line is a
    /// row, and all rows must be the same width.
    pub fn parse(text: &str) -> Result<Sprite, SpriteError> {
        let mut rows: Vec<Vec<Cell>> = Vec::new();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let row = rows.len() + 1;
            let cells = line
                .chars()
                .enumerate()
                .map(|(col, symbol)| {
                    cell_for(symbol).ok_or(SpriteError::UnknownSymbol {
                        row,
                        col: col + 1,
                        symbol,
                    })
                })
                .collect::<Result<Vec<Cell>, _>>()?;
            if let Some(first) = rows.first() {
                if first.len() != cells.len() {
                    return Err(SpriteError::Ragged {
                        row,
                        width: cells.len(),
                        expected: first.len(),
                    });
                }
            }
            rows.push(cells);
        }
        if rows.is_empty() {
            return Err(SpriteError::NoRows);
        }
        Ok(Sprite { rows })
    }

    /// The tone at pixel (`x`, `y`), `None` for an empty or out-of-range pixel.
    pub(crate) fn tone(&self, x: usize, y: usize) -> Option<&'static Tone> {
        self.rows.get(y).and_then(|r| r.get(x)).copied().flatten()
    }

    /// Pixel columns.
    pub fn width(&self) -> usize {
        self.rows[0].len()
    }

    /// Pixel rows.
    pub fn height(&self) -> usize {
        self.rows.len()
    }

    /// 24-bit half-block rendering: two pixel rows per text line. Every
    /// line ends with a reset, so no style leaks past the art.
    pub fn render_truecolor(&self) -> Vec<String> {
        let width = self.width();
        self.rows
            .chunks(2)
            .map(|pair| {
                let mut line = String::new();
                for x in 0..width {
                    let top = pair[0][x];
                    let bottom = pair.get(1).and_then(|r| r[x]);
                    line.push_str(&half_block(top, bottom));
                }
                line.push_str("\x1b[0m");
                line
            })
            .collect()
    }

    /// Plain rendering: shade characters, doubled for a roughly square
    /// pixel, no escapes.
    pub fn render_plain(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|c| c.map_or(' ', |t| t.shade))
                    .flat_map(|c| [c, c])
                    .collect()
            })
            .collect()
    }
}

fn fg(c: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2)
}

fn bg(c: Rgb) -> String {
    format!("\x1b[48;2;{};{};{}m", c.0, c.1, c.2)
}

/// One text cell for a (top, bottom) pixel pair.
fn half_block(top: Cell, bottom: Cell) -> String {
    match (top, bottom) {
        (None, None) => "\x1b[0m ".to_string(),
        (Some(t), None) => format!("\x1b[0m{}\u{2580}", fg(t.rgb)),
        (None, Some(b)) => format!("\x1b[0m{}\u{2584}", fg(b.rgb)),
        (Some(t), Some(b)) => format!("\x1b[0m{}{}\u{2580}", fg(t.rgb), bg(b.rgb)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rows_and_skips_comments() {
        let s = Sprite::parse("# note\nhm\n s\n").unwrap();
        assert_eq!(s.height(), 2);
    }

    #[test]
    fn dot_and_space_are_both_empty() {
        let a = Sprite::parse("h.\n").unwrap().render_plain();
        let b = Sprite::parse("h \n").unwrap().render_plain();
        assert_eq!(a, b);
    }

    #[test]
    fn ragged_rows_are_rejected() {
        assert_eq!(
            Sprite::parse("hh\nh\n").unwrap_err(),
            SpriteError::Ragged {
                row: 2,
                width: 1,
                expected: 2
            }
        );
    }

    #[test]
    fn unknown_symbols_and_empty_files_are_rejected() {
        assert_eq!(
            Sprite::parse("hx\n").unwrap_err(),
            SpriteError::UnknownSymbol {
                row: 1,
                col: 2,
                symbol: 'x'
            }
        );
        assert_eq!(Sprite::parse("# only\n").unwrap_err(), SpriteError::NoRows);
    }

    #[test]
    fn plain_render_doubles_width_with_shade_glyphs_and_no_escapes() {
        let lines = Sprite::parse("hms\n").unwrap().render_plain();
        assert_eq!(
            lines,
            vec!["\u{2593}\u{2593}\u{2592}\u{2592}\u{2591}\u{2591}"]
        );
    }

    #[test]
    fn truecolor_render_packs_two_pixel_rows_per_line() {
        let lines = Sprite::parse("h\nm\ns\n").unwrap().render_truecolor();
        // Three pixel rows: one full pair plus a lone top half.
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains('\u{2580}'));
        assert!(lines[0].contains("38;2;") && lines[0].contains("48;2;"));
        assert!(lines.iter().all(|l| l.ends_with("\x1b[0m")));
    }

    #[test]
    fn lone_bottom_pixel_uses_the_lower_half_block() {
        let lines = Sprite::parse(".\nh\n").unwrap().render_truecolor();
        assert!(lines[0].contains('\u{2584}'));
    }
}
