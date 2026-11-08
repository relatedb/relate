//! `.lolwut`: useless, fast and safe, after Redis. Draws generative braille
//! art that varies with the major version, then prints the Relate version.
//! Arguments are ignored: nothing a user types reaches the art.

use crate::canvas::Canvas;
use crate::layout::{layout, Rng};
use crate::{Ctx, Refusal};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const COLS: usize = 44;
const ROWS: usize = 10;

fn major(version: &str) -> u64 {
    version
        .split('.')
        .next()
        .and_then(|m| m.parse().ok())
        .expect("CARGO_PKG_VERSION starts with a numeric major")
}

/// The art for a major version: a small random graph, laid out by force and
/// drawn as dots and lines, with a few edges' worth of chords. The seed is
/// the major version, so a release keeps its own picture.
fn art(major: u64) -> String {
    let mut rng = Rng::new(major.wrapping_add(0x5EED));
    let n = 7 + (major % 6) as usize;
    let mut edges: Vec<(usize, usize)> = (1..n)
        .map(|i| ((rng.next_u64() % i as u64) as usize, i))
        .collect();
    for _ in 0..n / 2 {
        edges.push((
            (rng.next_u64() % n as u64) as usize,
            (rng.next_u64() % n as u64) as usize,
        ));
    }
    let pos = layout(n, &edges, major);
    let mut canvas = Canvas::new(COLS, ROWS);
    let (w, h) = (canvas.width() - 7, canvas.height() - 7);
    let px = |p: (f64, f64)| {
        (
            3 + (p.0 * f64::from(w)) as i32,
            3 + (p.1 * f64::from(h)) as i32,
        )
    };
    for &(a, b) in &edges {
        let ((x0, y0), (x1, y1)) = (px(pos[a]), px(pos[b]));
        canvas.line(x0, y0, x1, y1);
    }
    for p in &pos {
        let (x, y) = px(*p);
        canvas.circle(x, y, 2);
        canvas.circle(x, y, 1);
        canvas.set(x, y);
    }
    canvas.render()
}

pub(crate) fn run(ctx: &mut Ctx<'_>, _args: &[&str]) -> Result<(), Refusal> {
    if !ctx.caps.utf8 {
        return Err(Refusal::NotUtf8);
    }
    let text = format!("{}\nRelate ver. {VERSION}\n", art(major(VERSION)));
    ctx.out
        .write_all(text.as_bytes())
        .map_err(|e| Refusal::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_is_deterministic_per_major_and_differs_across_majors() {
        assert_eq!(art(0), art(0));
        assert_ne!(art(0), art(1));
        let a = art(3);
        assert_eq!(a.lines().count(), ROWS);
        assert!(a.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)));
    }

    #[test]
    fn every_major_draws_without_panicking() {
        for m in 0..64 {
            assert!(!art(m).trim().is_empty());
        }
        assert_eq!(major("0.16.1"), 0);
        assert_eq!(major("12.0.0"), 12);
    }
}
