//! A braille canvas: every terminal cell is a 2x4 grid of subpixels, so a
//! 80x24 terminal draws on 160x96 dots. Shared by the eggs that draw
//! (`.lolwut`, `.graph`); drawing is clipped, never panics and its work is
//! bounded by [`COORD_LIMIT`] whatever coordinates a caller passes.

/// Largest canvas, in cells, per axis.
pub const MAX_CELLS: usize = 200;

/// Coordinates are clamped to this magnitude before any stepping, so a
/// stray huge value costs a bounded loop, not a hang.
const COORD_LIMIT: i32 = 4096;

const BRAILLE_BASE: u32 = 0x2800;

/// Dot bit for subpixel (x % 2, y % 4), as laid out by Unicode braille.
pub(crate) const DOT_BITS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

pub struct Canvas {
    cols: usize,
    rows: usize,
    dots: Vec<u8>,
    text: Vec<Option<char>>,
}

impl Canvas {
    /// A blank canvas of `cols` x `rows` cells, each clamped to
    /// `1..=MAX_CELLS`.
    pub fn new(cols: usize, rows: usize) -> Canvas {
        let cols = cols.clamp(1, MAX_CELLS);
        let rows = rows.clamp(1, MAX_CELLS);
        Canvas {
            cols,
            rows,
            dots: vec![0; cols * rows],
            text: vec![None; cols * rows],
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Width in subpixels.
    pub fn width(&self) -> i32 {
        (self.cols * 2) as i32
    }

    /// Height in subpixels.
    pub fn height(&self) -> i32 {
        (self.rows * 4) as i32
    }

    /// Light one subpixel; outside the canvas is a no-op.
    pub fn set(&mut self, x: i32, y: i32) {
        if x < 0 || y < 0 || x >= self.width() || y >= self.height() {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        self.dots[(y / 4) * self.cols + x / 2] |= DOT_BITS[x % 2][y % 4];
    }

    /// Whether the subpixel is lit; outside the canvas is `false`.
    pub fn is_set(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x >= self.width() || y >= self.height() {
            return false;
        }
        let (x, y) = (x as usize, y as usize);
        self.dots[(y / 4) * self.cols + x / 2] & DOT_BITS[x % 2][y % 4] != 0
    }

    /// Bresenham line, clipped to the canvas.
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        let c = |v: i32| v.clamp(-COORD_LIMIT, COORD_LIMIT);
        let (mut x, mut y, x1, y1) = (c(x0), c(y0), c(x1), c(y1));
        let (dx, dy) = ((x1 - x).abs(), -(y1 - y).abs());
        let (sx, sy) = (if x < x1 { 1 } else { -1 }, if y < y1 { 1 } else { -1 });
        let mut err = dx + dy;
        loop {
            self.set(x, y);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// Midpoint circle outline of radius `r` (negative counts as 0).
    pub fn circle(&mut self, cx: i32, cy: i32, r: i32) {
        let r = r.clamp(0, COORD_LIMIT);
        let (cx, cy) = (
            cx.clamp(-COORD_LIMIT, COORD_LIMIT),
            cy.clamp(-COORD_LIMIT, COORD_LIMIT),
        );
        let (mut x, mut y, mut err) = (r, 0, 1 - r);
        while x >= y {
            for (px, py) in [
                (x, y),
                (y, x),
                (-y, x),
                (-x, y),
                (-x, -y),
                (-y, -x),
                (y, -x),
                (x, -y),
            ] {
                self.set(cx + px, cy + py);
            }
            y += 1;
            if err < 0 {
                err += 2 * y + 1;
            } else {
                x -= 1;
                err += 2 * (y - x) + 1;
            }
        }
    }

    /// Overlay `s` on the cells starting at (`col`, `row`); the glyphs
    /// replace the braille under them. Control characters are dropped and
    /// anything past the canvas edge is clipped.
    pub fn put_text(&mut self, col: usize, row: usize, s: &str) {
        if row >= self.rows {
            return;
        }
        let glyphs = s.chars().filter(|c| !c.is_control());
        for (i, ch) in glyphs.enumerate() {
            let c = col.saturating_add(i);
            if c >= self.cols {
                break;
            }
            self.text[row * self.cols + c] = Some(ch);
        }
    }

    /// The canvas as text, one line per cell row, trailing blanks trimmed.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(self.rows * (self.cols * 3 + 1));
        for r in 0..self.rows {
            let start = out.len();
            for c in 0..self.cols {
                let i = r * self.cols + c;
                out.push(match (self.text[i], self.dots[i]) {
                    (Some(ch), _) => ch,
                    (None, 0) => ' ',
                    (None, bits) => char::from_u32(BRAILLE_BASE + u32::from(bits))
                        .expect("0x2800..=0x28FF are braille patterns"),
                });
            }
            out.truncate(start + out[start..].trim_end().len());
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_dot_maps_to_its_braille_bit() {
        let mut c = Canvas::new(2, 1);
        c.set(0, 0);
        c.set(1, 3);
        assert_eq!(c.render(), "\u{2881}\n");
        c.set(2, 0);
        assert_eq!(c.render(), "\u{2881}\u{2801}\n");
    }

    #[test]
    fn out_of_range_is_clipped_and_size_is_clamped() {
        let mut c = Canvas::new(0, 10_000);
        assert_eq!((c.cols(), c.rows()), (1, MAX_CELLS));
        c.set(-1, 0);
        c.set(i32::MAX, i32::MIN);
        assert!(c.render().trim().is_empty());
    }

    #[test]
    fn lines_hit_both_endpoints_and_survive_huge_coordinates() {
        let mut c = Canvas::new(10, 5);
        c.line(0, 0, 19, 19);
        assert!(c.is_set(0, 0) && c.is_set(19, 19));
        c.line(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
        c.circle(i32::MAX, 0, i32::MAX);
    }

    #[test]
    fn circle_is_symmetric_and_hollow() {
        let mut c = Canvas::new(10, 5);
        c.circle(10, 10, 5);
        assert!(c.is_set(15, 10) && c.is_set(5, 10) && c.is_set(10, 15) && c.is_set(10, 5));
        assert!(!c.is_set(10, 10));
    }

    #[test]
    fn text_overlays_clips_and_drops_control_characters() {
        let mut c = Canvas::new(4, 1);
        c.line(0, 0, 7, 0);
        c.put_text(1, 0, "a\x1b[31mbcdef");
        assert_eq!(c.render(), "\u{2809}a[3\n");
        c.put_text(0, 5, "x");
    }
}
