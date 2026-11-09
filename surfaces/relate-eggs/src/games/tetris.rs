//! `.tetris`: the seven standard pieces on a 10 by 20 well, a shuffled bag
//! for the sequence, line clears, score and levels.
//! Keys: left/right (a/d) move, up (w) rotate, down (s) soft drop, space
//! hard drop, p pause, r restart (when over), q quit.

use std::time::Duration;

use super::term::{self, fit, Flow, Game, Key, Style};
use crate::rng::Rng;
use crate::sprite::Rgb;
use crate::{Ctx, Refusal};

const USAGE: &str = "Usage: .tetris";
const BW: usize = 10;
const BH: usize = 20;
/// Well with walls (two columns per cell) plus the side panel; the well's
/// rows plus its floor.
const MIN_SIZE: (u16, u16) = (42, BH as u16 + 1);
const WALL: Rgb = Rgb(0x58, 0x58, 0x58);
/// Points for 0 to 4 lines at once, times the level plus one.
const LINE_POINTS: [u32; 5] = [0, 100, 300, 500, 800];

/// One piece: its cells in an `n` by `n` box, and its colour.
struct Piece {
    cells: [(i32, i32); 4],
    n: i32,
    rgb: Rgb,
}

/// I, O, T, S, Z, J, L.
const PIECES: [Piece; 7] = [
    Piece {
        cells: [(0, 1), (1, 1), (2, 1), (3, 1)],
        n: 4,
        rgb: Rgb(0, 190, 200),
    },
    Piece {
        cells: [(0, 0), (1, 0), (0, 1), (1, 1)],
        n: 2,
        rgb: Rgb(220, 200, 0),
    },
    Piece {
        cells: [(1, 0), (0, 1), (1, 1), (2, 1)],
        n: 3,
        rgb: Rgb(160, 60, 200),
    },
    Piece {
        cells: [(1, 0), (2, 0), (0, 1), (1, 1)],
        n: 3,
        rgb: Rgb(60, 190, 70),
    },
    Piece {
        cells: [(0, 0), (1, 0), (1, 1), (2, 1)],
        n: 3,
        rgb: Rgb(210, 50, 50),
    },
    Piece {
        cells: [(0, 0), (0, 1), (1, 1), (2, 1)],
        n: 3,
        rgb: Rgb(50, 80, 220),
    },
    Piece {
        cells: [(2, 0), (0, 1), (1, 1), (2, 1)],
        n: 3,
        rgb: Rgb(230, 130, 30),
    },
];

/// A falling piece: kind, clockwise quarter turns, box position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Active {
    kind: usize,
    rot: u8,
    x: i32,
    y: i32,
}

impl Active {
    fn spawn(kind: usize) -> Active {
        Active {
            kind,
            rot: 0,
            x: (BW as i32 - PIECES[kind].n) / 2,
            y: 0,
        }
    }

    /// Board coordinates of the four cells.
    fn cells(&self) -> [(i32, i32); 4] {
        let p = &PIECES[self.kind];
        p.cells.map(|(mut cx, mut cy)| {
            for _ in 0..self.rot % 4 {
                (cx, cy) = (p.n - 1 - cy, cx);
            }
            (self.x + cx, self.y + cy)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Playing,
    Paused,
    Over,
}

/// The rules.
pub(crate) struct Well {
    /// 0 is empty, otherwise piece kind + 1.
    rows: [[u8; BW]; BH],
    active: Active,
    next: usize,
    bag: Vec<usize>,
    rng: Rng,
    score: u32,
    lines: u32,
    state: State,
}

impl Well {
    pub fn new(mut rng: Rng) -> Well {
        let mut bag = Vec::new();
        let first = draw(&mut bag, &mut rng);
        let next = draw(&mut bag, &mut rng);
        Well {
            rows: [[0; BW]; BH],
            active: Active::spawn(first),
            next,
            bag,
            rng,
            score: 0,
            lines: 0,
            state: State::Playing,
        }
    }

    pub fn level(&self) -> u32 {
        self.lines / 10
    }

    fn fits(&self, a: &Active) -> bool {
        a.cells().iter().all(|&(x, y)| {
            (0..BW as i32).contains(&x)
                && y < BH as i32
                && (y < 0 || self.rows[y as usize][x as usize] == 0)
        })
    }

    fn try_move(&mut self, dx: i32, dy: i32) -> bool {
        let moved = Active {
            x: self.active.x + dx,
            y: self.active.y + dy,
            ..self.active
        };
        let ok = self.fits(&moved);
        if ok {
            self.active = moved;
        }
        ok
    }

    pub fn left(&mut self) {
        self.try_move(-1, 0);
    }

    pub fn right(&mut self) {
        self.try_move(1, 0);
    }

    /// Rotate clockwise, nudging sideways off a wall or a stack if needed.
    pub fn rotate(&mut self) {
        for kick in [0, -1, 1, -2, 2] {
            let turned = Active {
                rot: (self.active.rot + 1) % 4,
                x: self.active.x + kick,
                ..self.active
            };
            if self.fits(&turned) {
                self.active = turned;
                return;
            }
        }
    }

    /// One row down for a point, or lock the piece if it cannot.
    pub fn soft_drop(&mut self) {
        if self.try_move(0, 1) {
            self.score += 1;
        } else {
            self.lock();
        }
    }

    /// Drop to the floor (two points a row) and lock.
    pub fn hard_drop(&mut self) {
        while self.try_move(0, 1) {
            self.score += 2;
        }
        self.lock();
    }

    /// Gravity: one row down, or lock.
    pub fn gravity(&mut self) {
        if !self.try_move(0, 1) {
            self.lock();
        }
    }

    fn lock(&mut self) {
        stamp(&mut self.rows, &self.active);
        let before = self.level();
        let kept: Vec<[u8; BW]> = self
            .rows
            .iter()
            .copied()
            .filter(|r| r.contains(&0))
            .collect();
        let cleared = BH - kept.len();
        let mut rows = [[0; BW]; BH];
        rows[cleared..].copy_from_slice(&kept);
        self.rows = rows;
        self.lines += cleared as u32;
        self.score += LINE_POINTS[cleared] * (before + 1);
        self.active = Active::spawn(self.next);
        self.next = draw(&mut self.bag, &mut self.rng);
        if !self.fits(&self.active) {
            self.state = State::Over;
        }
    }

    /// Milliseconds per gravity step: faster each level, floor 100.
    pub fn gravity_ms(&self) -> u64 {
        700u64.saturating_sub(self.level() as u64 * 60).max(100)
    }

    /// The well with the falling piece drawn in (kind + 1 per cell).
    fn with_active(&self) -> [[u8; BW]; BH] {
        let mut rows = self.rows;
        stamp(&mut rows, &self.active);
        rows
    }
}

/// Write the piece's in-well cells into `rows` (kind + 1 per cell).
fn stamp(rows: &mut [[u8; BW]; BH], a: &Active) {
    for (x, y) in a.cells() {
        if (0..BH as i32).contains(&y) {
            rows[y as usize][x as usize] = a.kind as u8 + 1;
        }
    }
}

/// The next kind from a shuffled bag of all seven (refilled when empty).
fn draw(bag: &mut Vec<usize>, rng: &mut Rng) -> usize {
    if bag.is_empty() {
        *bag = (0..PIECES.len()).collect();
        for i in (1..bag.len()).rev() {
            bag.swap(i, rng.below(i + 1));
        }
    }
    bag.pop().expect("the bag was just refilled")
}

struct Tetris {
    well: Well,
    style: Style,
}

impl Tetris {
    fn cell(&self, v: u8) -> String {
        match v {
            0 => "  ".to_string(),
            k => self.style.cell(PIECES[k as usize - 1].rgb, "[]"),
        }
    }

    fn preview(&self, row: i32) -> String {
        let p = &PIECES[self.well.next];
        (0..4)
            .map(|x| {
                if p.cells.contains(&(x, row)) {
                    self.style.cell(p.rgb, "[]")
                } else {
                    "  ".to_string()
                }
            })
            .collect()
    }
}

impl Game for Tetris {
    fn min_size(&self) -> (u16, u16) {
        MIN_SIZE
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(self.well.gravity_ms())
    }

    fn tick(&mut self) {
        if self.well.state == State::Playing {
            self.well.gravity();
        }
    }

    fn key(&mut self, key: Key) -> Flow {
        let w = &mut self.well;
        match (w.state, key) {
            (_, Key::Char('q') | Key::Esc) => return Flow::Quit,
            (State::Over, Key::Char('r')) => w.restart(),
            (State::Playing, Key::Left) => w.left(),
            (State::Playing, Key::Right) => w.right(),
            (State::Playing, Key::Up) => w.rotate(),
            (State::Playing, Key::Down) => w.soft_drop(),
            (State::Playing, Key::Char(' ')) => w.hard_drop(),
            (State::Playing, Key::Char('p')) => w.state = State::Paused,
            (State::Paused, Key::Char('p')) => w.state = State::Playing,
            _ => {}
        }
        Flow::Continue
    }

    fn render(&self) -> Vec<String> {
        let w = &self.well;
        let wall = self.style.cell(WALL, "##");
        let grid = w.with_active();
        let panel = |i: usize| -> String {
            match i {
                1 => " TETRIS".to_string(),
                3 => format!(" score {}", w.score),
                4 => format!(" lines {}", w.lines),
                5 => format!(" level {}", w.level()),
                7 => " next".to_string(),
                8 => format!(" {}", self.preview(0)),
                9 => format!(" {}", self.preview(1)),
                12 => match w.state {
                    State::Over => " GAME OVER".to_string(),
                    State::Paused => " paused".to_string(),
                    State::Playing => String::new(),
                },
                13 if w.state == State::Over => " r restart".to_string(),
                15 => " a/d move w turn".to_string(),
                16 => " s down space drop".to_string(),
                17 => " p pause  q quit".to_string(),
                _ => String::new(),
            }
        };
        let mut lines = Vec::with_capacity(BH + 1);
        for (y, row) in grid.iter().enumerate() {
            let cells: String = row.iter().map(|&v| self.cell(v)).collect();
            // The two preview rows carry colour escapes, which `fit` would
            // count as columns; they are a fixed short width and need no cut.
            let side = if matches!(y, 8 | 9) {
                panel(y)
            } else {
                fit(&panel(y), MIN_SIZE.0 as usize - (BW + 2) * 2)
            };
            lines.push(format!("{wall}{cells}{wall}{side}"));
        }
        lines.push(wall.repeat(BW + 2));
        lines
    }
}

impl Well {
    /// A fresh game; the generator carries on.
    fn restart(&mut self) {
        let seed = self.rng.next_u64();
        *self = Well::new(Rng::seeded(seed));
    }
}

/// The `.tetris` handler.
pub(crate) fn run(ctx: &mut Ctx<'_>, args: &[&str]) -> Result<(), Refusal> {
    if !args.is_empty() {
        return Err(Refusal::Usage(USAGE));
    }
    term::terminal_size(MIN_SIZE)?;
    let mut game = Tetris {
        well: Well::new(Rng::from_clock()),
        style: Style::from_caps(ctx.caps),
    };
    term::play(ctx, &mut game)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn well() -> Well {
        Well::new(Rng::seeded(9))
    }

    fn put(w: &mut Well, kind: usize) {
        w.active = Active::spawn(kind);
    }

    fn fill_row(w: &mut Well, y: usize, gap: Option<usize>) {
        for x in 0..BW {
            w.rows[y][x] = if Some(x) == gap { 0 } else { 1 };
        }
    }

    #[test]
    fn every_piece_has_four_distinct_cells_in_every_rotation() {
        for (kind, piece) in PIECES.iter().enumerate() {
            for rot in 0..4 {
                let a = Active {
                    kind,
                    rot,
                    x: 3,
                    y: 3,
                };
                let mut cells = a.cells().to_vec();
                cells.sort_unstable();
                cells.dedup();
                assert_eq!(cells.len(), 4, "piece {kind} rot {rot}");
                let n = piece.n;
                assert!(cells
                    .iter()
                    .all(|&(x, y)| (3..3 + n).contains(&x) && (3..3 + n).contains(&y)));
            }
        }
    }

    #[test]
    fn four_turns_come_home_and_the_o_never_changes() {
        let a = Active::spawn(2);
        let mut b = a;
        b.rot = 4;
        assert_eq!(a.cells(), b.cells());
        let o = Active::spawn(1);
        let mut o2 = o;
        o2.rot = 1;
        let (mut x, mut y) = (o.cells().to_vec(), o2.cells().to_vec());
        x.sort_unstable();
        y.sort_unstable();
        assert_eq!(x, y);
    }

    #[test]
    fn the_i_piece_turns_upright() {
        let mut w = well();
        put(&mut w, 0);
        w.active.y = 5;
        w.rotate();
        let xs: Vec<i32> = w.active.cells().iter().map(|c| c.0).collect();
        assert!(xs.iter().all(|&x| x == xs[0]), "vertical: {xs:?}");
    }

    #[test]
    fn walls_and_the_floor_stop_moves() {
        let mut w = well();
        put(&mut w, 1);
        for _ in 0..20 {
            w.left();
        }
        assert_eq!(w.active.x, 0);
        for _ in 0..20 {
            w.right();
        }
        assert_eq!(w.active.x, BW as i32 - 2);
        while w.try_move(0, 1) {}
        assert_eq!(w.active.y, BH as i32 - 2);
    }

    #[test]
    fn a_stacked_cell_blocks_the_piece() {
        let mut w = well();
        put(&mut w, 1);
        w.rows[5][4] = 3;
        w.active.y = 3;
        assert!(!w.try_move(0, 1), "O at y=3 would overlap (4,5)");
        assert!(w.try_move(1, 0));
    }

    #[test]
    fn rotating_beside_a_wall_kicks_off_it() {
        let mut w = well();
        put(&mut w, 0);
        w.active.rot = 1;
        w.active.x = -2; // vertical I in column 0 of its box 2 -> x = 0
        w.active.y = 4;
        assert!(w.fits(&w.active));
        w.rotate();
        assert!(w.fits(&w.active), "kicked into the well");
    }

    #[test]
    fn full_rows_clear_and_the_rest_falls() {
        let mut w = well();
        fill_row(&mut w, BH - 1, Some(0));
        fill_row(&mut w, BH - 2, Some(0));
        fill_row(&mut w, BH - 3, Some(0));
        w.rows[BH - 3][5] = 0;
        // A vertical I into column 0 completes the two bottom rows.
        w.active = Active {
            kind: 0,
            rot: 1,
            x: -2,
            y: 0,
        };
        w.hard_drop();
        assert_eq!(w.lines, 2);
        assert!(
            w.rows[BH - 1].contains(&0),
            "the partial row fell to the floor"
        );
        assert_eq!(w.rows[BH - 1].iter().filter(|&&c| c != 0).count(), BW - 1);
        // 2 lines * 300 base is 300 at level 0, plus the drop points.
        assert!(w.score >= 300);
    }

    #[test]
    fn four_lines_score_eight_hundred_times_the_level() {
        let mut w = well();
        w.lines = 10; // level 1
        for y in BH - 4..BH {
            fill_row(&mut w, y, Some(9));
        }
        w.active = Active {
            kind: 0,
            rot: 1,
            x: 7,
            y: 0,
        };
        let before = w.score;
        w.hard_drop();
        assert_eq!(w.lines, 14);
        assert_eq!(w.score - before - 2 * 16, 800 * 2);
        assert!(w.rows.iter().all(|r| r.iter().all(|&c| c == 0)));
    }

    #[test]
    fn the_game_ends_when_a_new_piece_cannot_enter() {
        let mut w = well();
        for y in 0..4 {
            fill_row(&mut w, y, Some(0));
        }
        w.hard_drop();
        assert_eq!(w.state, State::Over);
    }

    #[test]
    fn the_bag_deals_each_kind_once_per_seven() {
        let mut bag = Vec::new();
        let mut rng = Rng::seeded(4);
        for _ in 0..3 {
            let mut seven: Vec<usize> = (0..7).map(|_| draw(&mut bag, &mut rng)).collect();
            seven.sort_unstable();
            assert_eq!(seven, vec![0, 1, 2, 3, 4, 5, 6]);
        }
    }

    #[test]
    fn gravity_speeds_up_with_the_level_to_a_floor() {
        let mut w = well();
        assert_eq!(w.gravity_ms(), 700);
        w.lines = 20;
        assert_eq!(w.gravity_ms(), 580);
        w.lines = 1000;
        assert_eq!(w.gravity_ms(), 100);
    }

    fn game() -> Tetris {
        Tetris {
            well: well(),
            style: Style { color: false },
        }
    }

    #[test]
    fn the_coloured_next_preview_is_not_cut_mid_escape() {
        let mut g = game();
        g.style = Style { color: true };
        let preview = g.preview(0) + &g.preview(1);
        let lines = g.render();
        let shown = lines.iter().filter(|l| l.contains("\x1b[48;2;")).count();
        assert!(preview.contains("\x1b[48;2;"));
        assert!(shown >= 2, "the preview rows keep their colour cells");
        assert!(lines
            .iter()
            .all(|l| l.matches("\x1b[0m").count() >= l.matches("\x1b[48;2;").count()));
    }

    #[test]
    fn keys_move_pause_restart_and_quit() {
        let mut g = game();
        let x = g.well.active.x;
        g.key(Key::Left);
        assert_eq!(g.well.active.x, x - 1);
        g.key(Key::Char('p'));
        g.key(Key::Left);
        assert_eq!(g.well.active.x, x - 1, "paused ignores moves");
        g.tick();
        g.key(Key::Char('p'));
        assert_eq!(g.well.state, State::Playing);
        g.well.state = State::Over;
        g.well.score = 50;
        g.key(Key::Char('r'));
        assert_eq!((g.well.state, g.well.score), (State::Playing, 0));
        assert_eq!(g.key(Key::Char('q')), Flow::Quit);
    }

    #[test]
    fn the_frame_fits_the_declared_size() {
        let lines = game().render();
        assert_eq!(lines.len(), MIN_SIZE.1 as usize);
        assert!(lines
            .iter()
            .all(|l| l.chars().count() <= MIN_SIZE.0 as usize));
        assert!(lines.iter().any(|l| l.contains("score 0")));
        assert!(lines.iter().any(|l| l.contains("[]")), "the piece is drawn");
    }
}
