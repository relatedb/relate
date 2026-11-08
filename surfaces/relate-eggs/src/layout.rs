//! Force-directed graph layout (Fruchterman-Reingold): deterministic, with
//! a fixed iteration count, so the same graph always draws the same.

const ITERATIONS: usize = 300;
const EPS: f64 = 1e-4;

/// Tiny xorshift generator for the seed jitter (no `rand` dependency).
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform in `[0, 1)`.
    pub(crate) fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Positions in the unit square for `n` nodes joined by `edges`. Self
/// loops and edges naming a node `>= n` are ignored. Every coordinate is
/// finite and within `0.0..=1.0`; the layout is stretched to fill it.
pub(crate) fn layout(n: usize, edges: &[(usize, usize)], seed: u64) -> Vec<(f64, f64)> {
    match n {
        0 => return Vec::new(),
        1 => return vec![(0.5, 0.5)],
        _ => {}
    }
    let mut rng = Rng::new(seed);
    let mut pos: Vec<(f64, f64)> = (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            (
                0.5 + 0.4 * a.cos() + (rng.unit() - 0.5) * 0.04,
                0.5 + 0.4 * a.sin() + (rng.unit() - 0.5) * 0.04,
            )
        })
        .collect();
    let edges: Vec<(usize, usize)> = edges
        .iter()
        .copied()
        .filter(|&(a, b)| a != b && a < n && b < n)
        .collect();
    let k = (1.0 / n as f64).sqrt();
    for it in 0..ITERATIONS {
        let temp = 0.1 * (1.0 - it as f64 / ITERATIONS as f64);
        let mut disp = vec![(0.0f64, 0.0f64); n];
        for i in 0..n {
            for j in (i + 1)..n {
                let (dx, dy) = (pos[i].0 - pos[j].0, pos[i].1 - pos[j].1);
                let d = (dx * dx + dy * dy).sqrt().max(EPS);
                let f = k * k / d / d;
                disp[i].0 += dx * f;
                disp[i].1 += dy * f;
                disp[j].0 -= dx * f;
                disp[j].1 -= dy * f;
            }
        }
        for &(a, b) in &edges {
            let (dx, dy) = (pos[a].0 - pos[b].0, pos[a].1 - pos[b].1);
            let d = (dx * dx + dy * dy).sqrt().max(EPS);
            let f = d / k;
            disp[a].0 -= dx * f;
            disp[a].1 -= dy * f;
            disp[b].0 += dx * f;
            disp[b].1 += dy * f;
        }
        for (p, d) in pos.iter_mut().zip(&disp) {
            let m = (d.0 * d.0 + d.1 * d.1).sqrt().max(EPS);
            let step = m.min(temp) / m;
            p.0 = (p.0 + d.0 * step).clamp(0.0, 1.0);
            p.1 = (p.1 + d.1 * step).clamp(0.0, 1.0);
        }
    }
    stretch(&mut pos);
    pos
}

/// Rescale each axis to span `0..=1` (a degenerate axis centres at 0.5).
fn stretch(pos: &mut [(f64, f64)]) {
    let range = |f: fn(&(f64, f64)) -> f64| {
        let lo = pos.iter().map(f).fold(f64::INFINITY, f64::min);
        let hi = pos.iter().map(f).fold(f64::NEG_INFINITY, f64::max);
        (lo, hi)
    };
    let (x, y) = (range(|p| p.0), range(|p| p.1));
    let norm = |v: f64, (lo, hi): (f64, f64)| {
        if hi - lo < EPS {
            0.5
        } else {
            ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
        }
    };
    for p in pos.iter_mut() {
        *p = (norm(p.0, x), norm(p.1, y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_sane(pos: &[(f64, f64)]) {
        for &(x, y) in pos {
            assert!(x.is_finite() && y.is_finite());
            assert!(
                (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y),
                "{x} {y}"
            );
        }
    }

    #[test]
    fn same_input_same_layout() {
        let edges = [(0, 1), (1, 2), (2, 3), (3, 0), (0, 2)];
        assert_eq!(layout(4, &edges, 7), layout(4, &edges, 7));
        assert_ne!(layout(4, &edges, 7), layout(4, &edges, 8));
    }

    #[test]
    fn degenerate_inputs_stay_in_bounds() {
        assert!(layout(0, &[], 1).is_empty());
        assert_eq!(layout(1, &[(0, 0)], 1), vec![(0.5, 0.5)]);
        // Self loop, duplicate edges and out-of-range endpoints.
        assert_sane(&layout(3, &[(0, 0), (0, 1), (0, 1), (1, 9), (9, 9)], 1));
        assert_sane(&layout(2, &[], 1));
    }

    #[test]
    fn dense_and_disconnected_graphs_have_no_nan() {
        let n = 50;
        let complete: Vec<_> = (0..n)
            .flat_map(|a| ((a + 1)..n).map(move |b| (a, b)))
            .collect();
        assert_sane(&layout(n, &complete, 3));
        assert_sane(&layout(n, &[], 3));
    }

    #[test]
    fn connected_nodes_end_up_closer_than_strangers() {
        let pos = layout(4, &[(0, 1), (2, 3)], 5);
        let d = |a: usize, b: usize| {
            ((pos[a].0 - pos[b].0).powi(2) + (pos[a].1 - pos[b].1).powi(2)).sqrt()
        };
        assert!(d(0, 1) < d(0, 2) && d(2, 3) < d(1, 3));
    }
}
