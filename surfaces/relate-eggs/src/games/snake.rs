//! `.snake`: Archie is the snake. Walls kill, apples grow; the head and
//! body fade through the Archie palette. `.snake demo` lets an autopilot
//! (breadth-first shortest path to the apple) play.
//! Keys: arrows/WASD steer, r restart (when over), q quit.

use std::collections::VecDeque;
use std::time::Duration;

use super::term::{self, fit, lerp, Flow, Game, Key, Style};
use crate::rng::Rng;
use crate::sprite::{Rgb, BRAND_PALETTE};
use crate::{Ctx, Refusal};

const USAGE: &str = "Usage: .snake [demo]";
const W: i32 = 28;
const H: i32 = 16;
/// Two columns per cell, walls included; a status row, the walls.
const MIN_SIZE: (u16, u16) = (((W + 2) * 2) as u16, (H + 3) as u16);
const APPLE: Rgb = Rgb(0xdc, 0x3c, 0x3c);
const WALL: Rgb = Rgb(0x58, 0x58, 0x58);
/// Ticks the demo shows a dead snake before starting over.
const DEMO_RESTART_TICKS: u32 = 15;

type Pos = (i32, i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    const ALL: [Dir; 4] = [Dir::Up, Dir::Down, Dir::Left, Dir::Right];

    fn delta(self) -> Pos {
        match self {
            Dir::Up => (0, -1),
            Dir::Down => (0, 1),
            Dir::Left => (-1, 0),
            Dir::Right => (1, 0),
        }
    }

    fn opposite(self) -> Dir {
        match self {
            Dir::Up => Dir::Down,
            Dir::Down => Dir::Up,
            Dir::Left => Dir::Right,
            Dir::Right => Dir::Left,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Playing,
    Dead,
    /// The snake fills the board.
    Won,
}

/// The rules: a snake on a walled `W` by `H` board.
pub(crate) struct Board {
    /// Head first.
    body: VecDeque<Pos>,
    dir: Dir,
    queued: Option<Dir>,
    apple: Option<Pos>,
    score: u32,
    state: State,
    rng: Rng,
}

fn in_bounds((x, y): Pos) -> bool {
    (0..W).contains(&x) && (0..H).contains(&y)
}

impl Board {
    pub fn new(rng: Rng) -> Board {
        let mut b = Board {
            body: VecDeque::from([(W / 2, H / 2), (W / 2 - 1, H / 2), (W / 2 - 2, H / 2)]),
            dir: Dir::Right,
            queued: None,
            apple: None,
            score: 0,
            state: State::Playing,
            rng,
        };
        b.place_apple();
        b
    }

    fn place_apple(&mut self) {
        let free: Vec<Pos> = (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .filter(|p| !self.body.contains(p))
            .collect();
        if free.is_empty() {
            self.apple = None;
            self.state = State::Won;
        } else {
            self.apple = Some(free[self.rng.below(free.len())]);
        }
    }

    /// Turn at the next step. A reversal into the neck is ignored.
    pub fn steer(&mut self, dir: Dir) {
        if dir != self.dir.opposite() {
            self.queued = Some(dir);
        }
    }

    /// Advance one cell: wall or self collision kills, an apple grows.
    pub fn step(&mut self) {
        if self.state != State::Playing {
            return;
        }
        self.dir = self.queued.take().unwrap_or(self.dir);
        let (dx, dy) = self.dir.delta();
        let head = self.body[0];
        let next = (head.0 + dx, head.1 + dy);
        let eating = self.apple == Some(next);
        if !eating {
            // The tail moves away this tick, so its cell is safe to enter.
            self.body.pop_back();
        }
        if !in_bounds(next) || self.body.contains(&next) {
            self.state = State::Dead;
            return;
        }
        self.body.push_front(next);
        if eating {
            self.score += 1;
            self.place_apple();
        }
    }

    /// The autopilot's move: the first step of a shortest path to the apple
    /// around walls and the body (the tail counts as free, it will have
    /// moved), else any safe neighbour, else straight on.
    pub fn demo_dir(&self) -> Dir {
        let start = self.body[0];
        let idx = |(x, y): Pos| (y * W + x) as usize;
        let mut blocked = vec![false; (W * H) as usize];
        let keep = self.body.len() - 1;
        for &p in self.body.iter().take(keep) {
            blocked[idx(p)] = true;
        }
        let safe = |p: Pos| in_bounds(p) && !blocked[idx(p)];
        let step = |p: Pos, d: Dir| (p.0 + d.delta().0, p.1 + d.delta().1);
        if let Some(apple) = self.apple {
            // First move that began each reached cell's shortest path.
            let mut first: Vec<Option<Dir>> = vec![None; (W * H) as usize];
            let mut seen = vec![false; (W * H) as usize];
            seen[idx(start)] = true;
            let mut queue = VecDeque::from([start]);
            while let Some(p) = queue.pop_front() {
                for d in Dir::ALL {
                    let n = step(p, d);
                    if !safe(n) || seen[idx(n)] || (p == start && d == self.dir.opposite()) {
                        continue;
                    }
                    seen[idx(n)] = true;
                    first[idx(n)] = if p == start { Some(d) } else { first[idx(p)] };
                    if n == apple {
                        return first[idx(n)].expect("a reached cell has a first move");
                    }
                    queue.push_back(n);
                }
            }
        }
        Dir::ALL
            .into_iter()
            .find(|&d| d != self.dir.opposite() && safe(step(start, d)))
            .unwrap_or(self.dir)
    }
}

struct Snake {
    board: Board,
    best: u32,
    demo: bool,
    dead_ticks: u32,
    style: Style,
}

impl Snake {
    fn new(demo: bool, style: Style) -> Snake {
        Snake {
            board: Board::new(Rng::from_clock()),
            best: 0,
            demo,
            dead_ticks: 0,
            style,
        }
    }

    fn restart(&mut self) {
        self.best = self.best.max(self.board.score);
        self.board = Board::new(Rng::from_clock());
        self.dead_ticks = 0;
    }

    fn cell_at(&self, p: Pos) -> String {
        let s = &self.style;
        let body = &self.board.body;
        if let Some(i) = body.iter().position(|&b| b == p) {
            let rgb = if i == 0 {
                BRAND_PALETTE.hl
            } else {
                lerp(
                    BRAND_PALETTE.md,
                    BRAND_PALETTE.sh,
                    i - 1,
                    body.len().max(2) - 1,
                )
            };
            return s.cell(rgb, if i == 0 { "@@" } else { "oo" });
        }
        if self.board.apple == Some(p) {
            return s.cell(APPLE, "()");
        }
        "  ".to_string()
    }
}

impl Game for Snake {
    fn min_size(&self) -> (u16, u16) {
        MIN_SIZE
    }

    fn interval(&self) -> Duration {
        let faster = self.board.score.min(20) as u64 * 4;
        Duration::from_millis(if self.demo { 60 } else { 140 - faster })
    }

    fn tick(&mut self) {
        if self.demo {
            if self.board.state == State::Playing {
                let d = self.board.demo_dir();
                self.board.steer(d);
            } else {
                self.dead_ticks += 1;
                if self.dead_ticks >= DEMO_RESTART_TICKS {
                    self.restart();
                }
            }
        }
        self.board.step();
    }

    fn key(&mut self, key: Key) -> Flow {
        match key {
            Key::Char('q') | Key::Esc => return Flow::Quit,
            Key::Char('r') if self.board.state != State::Playing && !self.demo => self.restart(),
            _ if self.demo => {}
            Key::Up => self.board.steer(Dir::Up),
            Key::Down => self.board.steer(Dir::Down),
            Key::Left => self.board.steer(Dir::Left),
            Key::Right => self.board.steer(Dir::Right),
            _ => {}
        }
        Flow::Continue
    }

    fn render(&self) -> Vec<String> {
        let b = &self.board;
        let note = match (b.state, self.demo) {
            (State::Dead, false) => "  GAME OVER - r restart, q quit",
            (State::Won, false) => "  YOU WIN - r restart, q quit",
            (_, true) => "  demo - q quit",
            (State::Playing, false) => "  arrows/wasd steer, q quit",
        };
        let status = format!(
            " ARCHIE  score {}  best {}{note}",
            b.score,
            self.best.max(b.score)
        );
        let wall = self.style.cell(WALL, "##");
        let mut lines = vec![fit(&status, MIN_SIZE.0 as usize)];
        lines.push(wall.repeat(W as usize + 2));
        for y in 0..H {
            let row: String = (0..W).map(|x| self.cell_at((x, y))).collect();
            lines.push(format!("{wall}{row}{wall}"));
        }
        lines.push(wall.repeat(W as usize + 2));
        lines
    }
}

/// The `.snake` handler.
pub(crate) fn run(ctx: &mut Ctx<'_>, args: &[&str]) -> Result<(), Refusal> {
    let demo = match args {
        [] => false,
        ["demo"] => true,
        _ => return Err(Refusal::Usage(USAGE)),
    };
    term::terminal_size(MIN_SIZE)?;
    let mut game = Snake::new(demo, Style::from_caps(ctx.caps));
    term::play(ctx, &mut game)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board_with(body: &[Pos], dir: Dir, apple: Option<Pos>) -> Board {
        Board {
            body: body.iter().copied().collect(),
            dir,
            queued: None,
            apple,
            score: 0,
            state: State::Playing,
            rng: Rng::seeded(5),
        }
    }

    #[test]
    fn it_moves_one_cell_per_step_keeping_its_length() {
        let mut b = board_with(&[(5, 5), (4, 5), (3, 5)], Dir::Right, Some((0, 0)));
        b.step();
        assert_eq!(b.body, VecDeque::from([(6, 5), (5, 5), (4, 5)]));
    }

    #[test]
    fn eating_grows_scores_and_moves_the_apple() {
        let mut b = board_with(&[(5, 5), (4, 5), (3, 5)], Dir::Right, Some((6, 5)));
        b.step();
        assert_eq!((b.body.len(), b.score), (4, 1));
        assert_ne!(b.apple, Some((6, 5)));
        assert!(!b.body.contains(&b.apple.expect("an apple")));
    }

    #[test]
    fn walls_kill_on_every_side() {
        for (head, dir) in [
            ((0, 5), Dir::Left),
            ((W - 1, 5), Dir::Right),
            ((5, 0), Dir::Up),
            ((5, H - 1), Dir::Down),
        ] {
            let (dx, dy) = dir.opposite().delta();
            let mut b = board_with(&[head, (head.0 + dx, head.1 + dy)], dir, None);
            b.step();
            assert_eq!(b.state, State::Dead, "{dir:?} at {head:?}");
        }
    }

    #[test]
    fn running_into_itself_kills() {
        let ring = [(5, 5), (5, 6), (4, 6), (4, 5), (4, 4), (5, 4)];
        let mut b = board_with(&ring, Dir::Up, None);
        b.steer(Dir::Left);
        b.step();
        assert_eq!(b.state, State::Dead);
    }

    #[test]
    fn the_tail_cell_is_safe_to_enter() {
        // A 2x2 loop: the head moves into the cell the tail leaves.
        let mut b = board_with(&[(5, 5), (5, 6), (4, 6), (4, 5)], Dir::Left, None);
        b.steer(Dir::Left);
        b.step();
        assert_eq!(b.state, State::Playing);
    }

    #[test]
    fn reversing_into_the_neck_is_ignored() {
        let mut b = board_with(&[(5, 5), (4, 5), (3, 5)], Dir::Right, None);
        b.steer(Dir::Left);
        b.step();
        assert_eq!(b.body[0], (6, 5));
        // Up then Left within one tick must not reverse either.
        b.steer(Dir::Up);
        b.steer(Dir::Left);
        b.step();
        assert_eq!(b.state, State::Playing);
    }

    #[test]
    fn a_full_board_is_a_win() {
        let mut b = board_with(&[(0, 0), (1, 0)], Dir::Right, None);
        b.body = (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).collect();
        b.place_apple();
        assert_eq!((b.state, b.apple), (State::Won, None));
    }

    #[test]
    fn the_autopilot_heads_for_the_apple() {
        let b = board_with(&[(5, 5), (4, 5), (3, 5)], Dir::Right, Some((5, 9)));
        assert_eq!(b.demo_dir(), Dir::Down);
        let b = board_with(&[(5, 5), (4, 5), (3, 5)], Dir::Right, Some((9, 5)));
        assert_eq!(b.demo_dir(), Dir::Right);
    }

    #[test]
    fn the_autopilot_routes_around_its_own_body() {
        // The apple is behind the neck: the way there goes over or under.
        let b = board_with(&[(5, 5), (4, 5), (3, 5)], Dir::Right, Some((2, 5)));
        assert!(matches!(b.demo_dir(), Dir::Up | Dir::Down));
    }

    #[test]
    fn without_an_apple_the_autopilot_takes_any_safe_move() {
        let b = board_with(&[(5, 0), (6, 0), (7, 0)], Dir::Left, None);
        // Up is the wall and Right is the neck: Down is the first safe move.
        assert_eq!(b.demo_dir(), Dir::Down);
    }

    #[test]
    fn the_autopilot_plays_a_long_game_without_dying_early() {
        let mut b = Board::new(Rng::seeded(11));
        for _ in 0..400 {
            if b.state != State::Playing {
                break;
            }
            let d = b.demo_dir();
            b.steer(d);
            b.step();
        }
        assert!(b.score >= 5, "score {} state {:?}", b.score, b.state);
    }

    #[test]
    fn the_frame_fits_the_declared_size() {
        let style = Style { color: true };
        let g = Snake::new(false, style);
        let lines = g.render();
        assert_eq!(lines.len(), MIN_SIZE.1 as usize);
        let plain = Snake::new(false, Style { color: false });
        let lines = plain.render();
        assert!(lines
            .iter()
            .all(|l| l.chars().count() <= MIN_SIZE.0 as usize));
        assert!(lines[1].starts_with("##"));
    }

    #[test]
    fn keys_steer_restart_and_quit() {
        let style = Style { color: false };
        let mut g = Snake::new(false, style);
        assert_eq!(g.key(Key::Up), Flow::Continue);
        g.tick();
        assert_eq!(g.board.body[0].1, H / 2 - 1);
        g.board.state = State::Dead;
        g.key(Key::Char('r'));
        assert_eq!(g.board.state, State::Playing);
        assert_eq!(g.key(Key::Char('q')), Flow::Quit);
    }

    #[test]
    fn the_demo_ignores_steering_and_restarts_after_dying() {
        let style = Style { color: false };
        let mut g = Snake::new(true, style);
        g.key(Key::Left);
        assert!(g.board.queued.is_none());
        g.board.state = State::Dead;
        for _ in 0..DEMO_RESTART_TICKS {
            g.tick();
        }
        assert_eq!(g.board.state, State::Playing);
    }
}
