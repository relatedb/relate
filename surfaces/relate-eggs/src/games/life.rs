//! `.life`: Conway's Game of Life on a toroidal grid, drawn in braille
//! (two columns by four rows of cells per character).
//! Keys: space pause, n/right step, r randomize, c clear, +/- speed, q quit.

use std::time::Duration;

use super::style::{Glyphs, Style};
use super::term::{self, fit, Flow, Game, Key};
use crate::canvas::DOT_BITS;
use crate::rng::Rng;
use crate::sprite::{fg, BRAND_PALETTE};
use crate::{Ctx, Refusal};

const USAGE: &str = "Usage: .life";
/// Smallest window, and the cap on the grid (characters), which bounds the
/// per-tick work.
const MIN_SIZE: (u16, u16) = (20, 8);
const MAX_COLS: u16 = 160;
const MAX_ROWS: u16 = 48;
/// Tick intervals, slowest to fastest, in milliseconds.
const SPEEDS_MS: [u64; 5] = [250, 150, 90, 50, 25];

/// A toroidal grid of cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Grid {
    w: usize,
    h: usize,
    cells: Vec<bool>,
}

impl Grid {
    pub fn new(w: usize, h: usize) -> Grid {
        assert!(w > 0 && h > 0, "empty grid");
        Grid {
            w,
            h,
            cells: vec![false; w * h],
        }
    }

    pub fn get(&self, x: usize, y: usize) -> bool {
        self.cells[y * self.w + x]
    }

    pub fn set(&mut self, x: usize, y: usize, alive: bool) {
        self.cells[y * self.w + x] = alive;
    }

    pub fn population(&self) -> usize {
        self.cells.iter().filter(|&&c| c).count()
    }

    pub fn clear(&mut self) {
        self.cells.fill(false);
    }

    /// Fill at random, about three cells in ten alive.
    pub fn randomize(&mut self, rng: &mut Rng) {
        for c in &mut self.cells {
            *c = rng.below(10) < 3;
        }
    }

    fn neighbours(&self, x: usize, y: usize) -> usize {
        let mut n = 0;
        for dy in [self.h - 1, 0, 1] {
            for dx in [self.w - 1, 0, 1] {
                if (dx, dy) != (0, 0) && self.get((x + dx) % self.w, (y + dy) % self.h) {
                    n += 1;
                }
            }
        }
        n
    }

    /// The next generation: a live cell with two or three live neighbours
    /// lives, a dead one with exactly three is born; the edges wrap.
    pub fn step(&self) -> Grid {
        let mut next = Grid::new(self.w, self.h);
        for y in 0..self.h {
            for x in 0..self.w {
                let n = self.neighbours(x, y);
                next.set(x, y, matches!((self.get(x, y), n), (true, 2) | (_, 3)));
            }
        }
        next
    }

    /// The braille character for the 2x4 block of cells at character
    /// position (`cx`, `cy`); cells beyond the grid read as dead.
    pub fn braille(&self, cx: usize, cy: usize) -> char {
        let mut mask = 0;
        for (dx, col) in DOT_BITS.iter().enumerate() {
            for (dy, bit) in col.iter().enumerate() {
                let (x, y) = (cx * 2 + dx, cy * 4 + dy);
                if x < self.w && y < self.h && self.get(x, y) {
                    mask |= u32::from(*bit);
                }
            }
        }
        char::from_u32(0x2800 + mask).expect("braille block is in range")
    }
}

struct Life {
    grid: Grid,
    rng: Rng,
    gen: u64,
    paused: bool,
    speed: usize,
    style: Style,
    cols: u16,
    rows: u16,
}

impl Life {
    fn new(cols: u16, rows: u16, style: Style, mut rng: Rng) -> Life {
        let cols = cols.min(MAX_COLS);
        let rows = rows.min(MAX_ROWS);
        // One row is the status line.
        let mut grid = Grid::new(cols as usize * 2, (rows as usize - 1) * 4);
        grid.randomize(&mut rng);
        Life {
            grid,
            rng,
            gen: 0,
            paused: false,
            speed: 2,
            style,
            cols,
            rows,
        }
    }

    fn advance(&mut self) {
        self.grid = self.grid.step();
        self.gen += 1;
    }
}

impl Game for Life {
    fn min_size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(SPEEDS_MS[self.speed])
    }

    fn tick(&mut self) {
        if !self.paused {
            self.advance();
        }
    }

    fn key(&mut self, key: Key) -> Flow {
        match key {
            Key::Char('q') | Key::Esc => return Flow::Quit,
            Key::Char(' ') => self.paused = !self.paused,
            Key::Char('n') | Key::Right => {
                self.paused = true;
                self.advance();
            }
            Key::Char('r') => {
                self.grid.randomize(&mut self.rng);
                self.gen = 0;
            }
            Key::Char('c') => {
                self.grid.clear();
                self.gen = 0;
            }
            Key::Char('+') | Key::Char('=') => {
                self.speed = (self.speed + 1).min(SPEEDS_MS.len() - 1)
            }
            Key::Char('-') => self.speed = self.speed.saturating_sub(1),
            _ => {}
        }
        Flow::Continue
    }

    fn render(&self) -> Vec<String> {
        let status = format!(
            " LIFE gen {} pop {}{}  space pause  n step  r random  c clear  +/- speed  q quit",
            self.gen,
            self.grid.population(),
            if self.paused { " (paused)" } else { "" },
        );
        let (on, off) = if self.style.color {
            let c = BRAND_PALETTE.hl;
            (fg(c), "\x1b[0m")
        } else {
            (String::new(), "")
        };
        let mut lines = vec![fit(&status, self.cols as usize)];
        for cy in 0..self.rows as usize - 1 {
            let row: String = (0..self.cols as usize)
                .map(|cx| self.grid.braille(cx, cy))
                .collect();
            lines.push(format!("{on}{row}{off}"));
        }
        lines
    }
}

/// The `.life` handler.
pub(crate) fn run(ctx: &mut Ctx<'_>, args: &[&str]) -> Result<(), Refusal> {
    if !args.is_empty() {
        return Err(Refusal::Usage(USAGE));
    }
    let style = Style::for_game(ctx.caps, Glyphs::Utf8)?;
    let (cols, rows) = term::terminal_size(MIN_SIZE)?;
    let mut game = Life::new(cols, rows, style, Rng::from_clock());
    term::play(ctx, &mut game)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_rows(rows: &[&str]) -> Grid {
        let mut g = Grid::new(rows[0].len(), rows.len());
        for (y, r) in rows.iter().enumerate() {
            for (x, c) in r.chars().enumerate() {
                g.set(x, y, c == '#');
            }
        }
        g
    }

    #[test]
    fn a_block_is_a_still_life() {
        let g = from_rows(&["......", ".##...", ".##...", "......", "......"]);
        assert_eq!(g.step(), g);
    }

    #[test]
    fn a_blinker_oscillates_with_period_two() {
        let a = from_rows(&[".....", ".....", ".###.", ".....", "....."]);
        let b = from_rows(&[".....", "..#..", "..#..", "..#..", "....."]);
        assert_eq!(a.step(), b);
        assert_eq!(b.step(), a);
    }

    #[test]
    fn a_lonely_cell_dies_and_a_crowded_one_dies() {
        let lone = from_rows(&["...", ".#.", "..."]);
        assert_eq!(lone.step().population(), 0);
        // Five neighbours: the centre dies of overcrowding.
        let crowd = from_rows(&["###", "###", "..."]);
        assert!(!crowd.step().get(1, 1));
    }

    #[test]
    fn a_glider_travels_diagonally_and_wraps_the_edges() {
        let glider = [
            "..#.....", "#.#.....", ".##.....", "........", "........", "........",
        ];
        let start = from_rows(&glider);
        let mut g = start.clone();
        for _ in 0..4 {
            g = g.step();
        }
        // Four generations move it one cell down and one right.
        let mut moved = Grid::new(8, 6);
        for y in 0..6 {
            for x in 0..8 {
                if start.get(x, y) {
                    moved.set(x + 1, y + 1, true);
                }
            }
        }
        assert_eq!(g, moved);
        // One cell per four generations: the torus is 8 wide and 6 high, so
        // the glider is home again after lcm(8, 6) * 4 = 96 generations.
        let mut h = start.clone();
        for _ in 0..96 {
            h = h.step();
        }
        assert_eq!(h, start);
    }

    #[test]
    fn the_edges_are_neighbours() {
        // Three cells in a row across the left/right seam are a blinker.
        let a = from_rows(&["#...##", "......", "......"]);
        let a1 = a.step();
        assert_eq!(a1.population(), 3);
        assert!(a1.get(5, 1) && a1.get(5, 0) && a1.get(5, 2));
    }

    #[test]
    fn braille_maps_the_eight_dots() {
        let mut g = Grid::new(2, 4);
        assert_eq!(g.braille(0, 0), '\u{2800}');
        g.set(0, 0, true);
        assert_eq!(g.braille(0, 0), '\u{2801}');
        g.set(1, 3, true);
        assert_eq!(g.braille(0, 0), '\u{2881}');
        for y in 0..4 {
            g.set(0, y, true);
            g.set(1, y, true);
        }
        assert_eq!(g.braille(0, 0), '\u{28ff}');
        // Past the grid reads as dead.
        assert_eq!(g.braille(1, 0), '\u{2800}');
    }

    #[test]
    fn randomize_fills_roughly_a_third_and_is_seeded() {
        let mut a = Grid::new(40, 40);
        a.randomize(&mut Rng::seeded(7));
        let pop = a.population();
        assert!(pop > 300 && pop < 700, "{pop}");
        let mut b = Grid::new(40, 40);
        b.randomize(&mut Rng::seeded(7));
        assert_eq!(a, b);
    }

    fn game() -> Life {
        let style = Style { color: false };
        Life::new(30, 10, style, Rng::seeded(3))
    }

    #[test]
    fn keys_pause_step_clear_and_quit() {
        let mut g = game();
        assert_eq!(g.key(Key::Char(' ')), Flow::Continue);
        assert!(g.paused);
        let gen = g.gen;
        g.tick();
        assert_eq!(g.gen, gen, "paused: no tick");
        g.key(Key::Char('n'));
        assert_eq!(g.gen, gen + 1);
        g.key(Key::Char('c'));
        assert_eq!((g.grid.population(), g.gen), (0, 0));
        assert_eq!(g.key(Key::Char('q')), Flow::Quit);
    }

    #[test]
    fn speed_is_clamped_to_the_table() {
        let mut g = game();
        for _ in 0..10 {
            g.key(Key::Char('+'));
        }
        assert_eq!(g.speed, SPEEDS_MS.len() - 1);
        for _ in 0..10 {
            g.key(Key::Char('-'));
        }
        assert_eq!(g.speed, 0);
    }

    #[test]
    fn the_frame_fits_its_declared_size() {
        let g = game();
        let lines = g.render();
        assert_eq!(lines.len(), g.min_size().1 as usize);
        assert!(lines
            .iter()
            .all(|l| l.chars().count() <= g.min_size().0 as usize));
    }

    #[test]
    fn the_grid_is_capped_on_huge_windows() {
        let style = Style { color: false };
        let g = Life::new(500, 500, style, Rng::seeded(1));
        assert_eq!(g.min_size(), (MAX_COLS, MAX_ROWS));
    }
}
