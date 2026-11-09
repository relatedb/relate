//! `.dash`: Archie runs, jumps and ducks past obstacles. Pure rules
//! ([`Run`]: jump table, obstacles, collisions) plus a renderer and the
//! `.dash` handler. `.dash demo` plays itself. The best score lasts for the
//! session only; nothing is written to disk.

use std::time::Duration;

use super::style::{Glyphs, Style};
use super::term::{self, fit, Flow, Game, Key};
use crate::rng::Rng;
use crate::sprite::{Rgb, Sprite};
use crate::{Ctx, Refusal};

const USAGE: &str = "Usage: .dash [demo]";
/// Playfield width in pixels (two terminal columns each) and height in rows.
const W: i32 = 32;
const H: i32 = 10;
const MIN_SIZE: (u16, u16) = ((W * 2) as u16, (H + 2) as u16);
/// Archie's left edge (pixels) and sprite width.
const AX: i32 = 3;
const AW: i32 = 6;
/// Height above the ground per tick of a jump; the jump ends after the last.
const JUMP: [i32; 12] = [1, 3, 4, 5, 6, 6, 6, 6, 5, 4, 3, 1];
const DUCK_TICKS: u32 = 10;
const DEMO_RESTART_TICKS: u32 = 15;
/// Flying hazards appear after this many ticks of running.
const FLYERS_FROM: u32 = 150;
const NODE: Rgb = Rgb(0xe0, 0x8a, 0x2e);
const REL: Rgb = Rgb(0xdc, 0x3c, 0x3c);
const GROUND: Rgb = Rgb(0x58, 0x58, 0x58);

const RUN1: &str = include_str!("../../assets/archie-xs/run1.txt");
const RUN2: &str = include_str!("../../assets/archie-xs/run2.txt");
const JUMP_POSE: &str = include_str!("../../assets/archie-xs/jump.txt");
const DUCK_POSE: &str = include_str!("../../assets/archie-xs/duck.txt");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Playing,
    Dead,
}

/// A hazard: a node on the ground, or a relationship flying at chest height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Obstacle {
    x: i32,
    w: i32,
    /// Height of its lowest row above the ground, and its height in rows.
    base: i32,
    h: i32,
    flying: bool,
}

struct Poses {
    run: [Sprite; 2],
    jump: Sprite,
    duck: Sprite,
}

impl Poses {
    fn load() -> Poses {
        let parse = |t: &str| Sprite::parse(t).expect("the shipped dash sprites parse");
        Poses {
            run: [parse(RUN1), parse(RUN2)],
            jump: parse(JUMP_POSE),
            duck: parse(DUCK_POSE),
        }
    }
}

struct Run {
    obstacles: Vec<Obstacle>,
    /// Index into [`JUMP`] while airborne.
    jump: Option<usize>,
    duck: u32,
    dist: u32,
    gap: i32,
    state: State,
    rng: Rng,
}

impl Run {
    fn new(rng: Rng) -> Run {
        Run {
            obstacles: Vec::new(),
            jump: None,
            duck: 0,
            dist: 0,
            gap: 10,
            state: State::Playing,
            rng,
        }
    }

    fn score(&self) -> u32 {
        self.dist / 5
    }

    fn height(&self) -> i32 {
        self.jump.map_or(0, |i| JUMP[i])
    }

    fn ducking(&self) -> bool {
        self.duck > 0 && self.jump.is_none()
    }

    fn sprite_rows(&self) -> i32 {
        if self.ducking() {
            2
        } else {
            4
        }
    }

    fn press_jump(&mut self) {
        if self.jump.is_none() && self.state == State::Playing {
            self.duck = 0;
            self.jump = Some(0);
        }
    }

    fn press_duck(&mut self) {
        if self.jump.is_none() && self.state == State::Playing {
            self.duck = DUCK_TICKS;
        }
    }

    fn hits(&self, o: &Obstacle) -> bool {
        let (lo, hi) = (self.height(), self.height() + self.sprite_rows());
        o.x < AX + AW && o.x + o.w > AX && o.base < hi && o.base + o.h > lo
    }

    fn spawn(&mut self) {
        let last_right = self.obstacles.last().map_or(0, |o| o.x + o.w);
        if W - last_right < self.gap {
            return;
        }
        let flying = self.dist >= FLYERS_FROM && self.rng.below(3) == 0;
        self.obstacles.push(if flying {
            Obstacle {
                x: W,
                w: 3,
                base: 2,
                h: 1,
                flying,
            }
        } else {
            Obstacle {
                x: W,
                w: 2,
                base: 0,
                h: 2 + self.rng.below(2) as i32,
                flying,
            }
        });
        self.gap = 14 + self.rng.below(12) as i32;
    }

    /// One tick: the world moves one pixel left.
    fn step(&mut self) {
        if self.state != State::Playing {
            return;
        }
        self.dist += 1;
        self.jump = self
            .jump
            .and_then(|i| (i + 1 < JUMP.len()).then_some(i + 1));
        self.duck = self.duck.saturating_sub(1);
        for o in &mut self.obstacles {
            o.x -= 1;
        }
        self.obstacles.retain(|o| o.x + o.w > 0);
        self.spawn();
        if self.obstacles.iter().any(|o| self.hits(o)) {
            self.state = State::Dead;
        }
    }

    /// The demo's hands: act when the next hazard is a fixed distance away.
    fn autopilot(&mut self) {
        let front = AX + AW;
        let Some(next) = self.obstacles.iter().find(|o| o.x + o.w > AX) else {
            return;
        };
        let dx = next.x - front;
        if next.flying && dx == 0 {
            self.press_duck();
        } else if !next.flying && dx == 2 {
            self.press_jump();
        }
    }
}

struct Dash {
    run: Run,
    poses: Poses,
    style: Style,
    demo: bool,
    best: u32,
    dead_ticks: u32,
}

impl Dash {
    fn new(demo: bool, style: Style) -> Dash {
        Dash {
            run: Run::new(Rng::from_clock()),
            poses: Poses::load(),
            style,
            demo,
            best: 0,
            dead_ticks: 0,
        }
    }

    fn restart(&mut self) {
        self.best = self.best.max(self.run.score());
        self.run = Run::new(Rng::from_clock());
        self.dead_ticks = 0;
    }

    fn pose(&self) -> &Sprite {
        if self.run.ducking() {
            &self.poses.duck
        } else if self.run.jump.is_some() {
            &self.poses.jump
        } else {
            &self.poses.run[(self.run.dist / 3 % 2) as usize]
        }
    }

    /// The pixel at column `x`, `height` rows above the ground.
    fn pixel(&self, x: i32, height: i32) -> String {
        let pose = self.pose();
        let row = self.run.height() + self.run.sprite_rows() - 1 - height;
        if (AX..AX + AW).contains(&x) && (0..self.run.sprite_rows()).contains(&row) {
            if let Some(tone) = pose.tone((x - AX) as usize, row as usize) {
                let mono: String = [tone.shade, tone.shade].iter().collect();
                return self.style.cell(tone.rgb, &mono);
            }
        }
        for o in &self.run.obstacles {
            if (o.x..o.x + o.w).contains(&x) && (o.base..o.base + o.h).contains(&height) {
                return if o.flying {
                    self.style.cell(REL, "<>")
                } else {
                    self.style.cell(NODE, "[]")
                };
            }
        }
        "  ".to_string()
    }
}

impl Game for Dash {
    fn min_size(&self) -> (u16, u16) {
        MIN_SIZE
    }

    fn interval(&self) -> Duration {
        let faster = (self.run.dist / 20).min(25) as u64;
        Duration::from_millis(60 - faster)
    }

    fn tick(&mut self) {
        if self.demo {
            if self.run.state == State::Playing {
                self.run.autopilot();
            } else {
                self.dead_ticks += 1;
                if self.dead_ticks >= DEMO_RESTART_TICKS {
                    self.restart();
                }
            }
        }
        self.run.step();
    }

    fn key(&mut self, key: Key) -> Flow {
        match key {
            Key::Char('q') | Key::Esc => return Flow::Quit,
            Key::Char('r') if self.run.state == State::Dead && !self.demo => self.restart(),
            _ if self.demo => {}
            Key::Up | Key::Char(' ') | Key::Char('w') => self.run.press_jump(),
            Key::Down | Key::Char('s') => self.run.press_duck(),
            _ => {}
        }
        Flow::Continue
    }

    fn render(&self) -> Vec<String> {
        let note = match (self.run.state, self.demo) {
            (State::Dead, false) => "  GAME OVER - r restart, q quit",
            (_, true) => "  demo - q quit",
            (State::Playing, false) => "  up/space jump, down duck, q quit",
        };
        let status = format!(
            " ARCHIE DASH  score {}  best {}{note}",
            self.run.score(),
            self.best.max(self.run.score())
        );
        let mut lines = vec![fit(&status, MIN_SIZE.0 as usize)];
        for y in 0..H {
            let height = H - 1 - y;
            lines.push((0..W).map(|x| self.pixel(x, height)).collect());
        }
        lines.push(self.style.cell(GROUND, "==").repeat(W as usize));
        lines
    }
}

/// The `.dash` handler.
pub(crate) fn run(ctx: &mut Ctx<'_>, args: &[&str]) -> Result<(), Refusal> {
    let demo = match args {
        [] => false,
        ["demo"] => true,
        _ => return Err(Refusal::Usage(USAGE)),
    };
    let mut game = Dash::new(demo, Style::for_game(ctx.caps, Glyphs::Ascii)?);
    term::play(ctx, &mut game)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_with(obstacles: &[Obstacle]) -> Run {
        let mut r = Run::new(Rng::seeded(7));
        r.obstacles = obstacles.to_vec();
        r.gap = 1000;
        r
    }

    fn node(x: i32) -> Obstacle {
        Obstacle {
            x,
            w: 2,
            base: 0,
            h: 3,
            flying: false,
        }
    }

    fn flyer(x: i32) -> Obstacle {
        Obstacle {
            x,
            w: 3,
            base: 2,
            h: 1,
            flying: true,
        }
    }

    #[test]
    fn the_shipped_sprites_parse_with_the_declared_size() {
        let p = Poses::load();
        for s in [&p.run[0], &p.run[1], &p.jump] {
            assert_eq!((s.width() as i32, s.height()), (AW, 4));
        }
        assert_eq!((p.duck.width() as i32, p.duck.height()), (AW, 2));
    }

    #[test]
    fn a_jump_rises_and_lands_after_the_table() {
        let mut r = run_with(&[]);
        r.press_jump();
        assert_eq!(r.height(), 1);
        let mut peak = 0;
        for _ in 0..JUMP.len() {
            r.step();
            peak = peak.max(r.height());
        }
        assert_eq!((peak, r.height(), r.jump), (6, 0, None));
    }

    #[test]
    fn a_node_on_the_ground_kills_a_runner() {
        let mut r = run_with(&[node(AX + AW)]);
        r.step();
        assert_eq!(r.state, State::Dead);
    }

    #[test]
    fn jumping_clears_a_node_and_ducking_clears_a_flyer() {
        let mut r = run_with(&[node(AX + AW + 2)]);
        r.step();
        r.press_jump();
        for _ in 0..16 {
            r.step();
        }
        assert_eq!(r.state, State::Playing);
        let mut r = run_with(&[flyer(AX + AW)]);
        r.press_duck();
        for _ in 0..10 {
            r.step();
        }
        assert_eq!(r.state, State::Playing);
        let mut r = run_with(&[flyer(AX + AW)]);
        r.step();
        assert_eq!(r.state, State::Dead, "standing up into a flyer hurts");
    }

    #[test]
    fn jumping_cancels_a_duck_and_the_air_blocks_both() {
        let mut r = run_with(&[]);
        r.press_duck();
        assert!(r.ducking());
        r.press_jump();
        assert!(!r.ducking());
        r.press_duck();
        r.press_jump();
        assert_eq!(r.duck, 0);
    }

    #[test]
    fn obstacles_scroll_off_and_new_ones_keep_a_playable_gap() {
        let mut r = Run::new(Rng::seeded(3));
        let mut saw_flyer = false;
        for _ in 0..400 {
            r.step();
            r.state = State::Playing; // immortal: this tests spawning only
            for pair in r.obstacles.windows(2) {
                assert!(pair[1].x - (pair[0].x + pair[0].w) >= 14);
            }
            assert!(r.obstacles.iter().all(|o| o.x + o.w > 0));
            saw_flyer |= r.obstacles.iter().any(|o| o.flying);
        }
        assert!(saw_flyer, "flyers appear once the run is long enough");
    }

    #[test]
    fn the_demo_survives_a_long_run() {
        for seed in 1..6u64 {
            let mut g = Dash::new(true, Style { color: false });
            g.run = Run::new(Rng::seeded(seed));
            for _ in 0..3000 {
                g.tick();
                assert_eq!(g.run.state, State::Playing, "seed {seed} died");
            }
        }
    }

    #[test]
    fn keys_jump_duck_restart_and_quit() {
        let mut g = Dash::new(false, Style { color: false });
        g.run = run_with(&[]);
        assert_eq!(g.key(Key::Char(' ')), Flow::Continue);
        assert!(g.run.jump.is_some());
        g.run.jump = None;
        g.key(Key::Down);
        assert!(g.run.ducking());
        g.run.state = State::Dead;
        g.run.dist = 100;
        g.key(Key::Char('r'));
        assert_eq!((g.run.state, g.run.dist, g.best), (State::Playing, 0, 20));
        assert_eq!(g.key(Key::Char('q')), Flow::Quit);
    }

    #[test]
    fn the_demo_ignores_steering_and_the_frame_fits() {
        let mut g = Dash::new(true, Style { color: false });
        g.key(Key::Up);
        assert!(g.run.jump.is_none());
        for color in [false, true] {
            g.style = Style { color };
            let lines = g.render();
            assert_eq!(lines.len(), MIN_SIZE.1 as usize);
            assert!(lines.iter().any(|l| l.contains("score 0")));
        }
        g.style = Style { color: false };
        assert!(g
            .render()
            .iter()
            .all(|l| l.chars().count() <= MIN_SIZE.0 as usize));
    }
}
