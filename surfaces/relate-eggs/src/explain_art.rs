//! `.explain-art <query>`: the query plan as a text flame graph.
//!
//! The statement runs once under `PROFILE` (read-only statements only; the
//! check is an `EXPLAIN` first, which executes nothing) and the profiled
//! operator tree is drawn as an icicle: the root operator on top, each
//! operator's children beneath it, every bar as wide as the operator's
//! share of the metric. The engine reports per-operator elapsed time, so
//! width is time; a statement too quick to register falls back to db hits,
//! then rows, then plain leaf count, and the header names what the width
//! means. Operator times are inclusive; children that sum past their parent
//! (shared work counted in both arms) are scaled down to fit under it.

use crate::sprite::{Rgb, TONES};
use crate::{Caps, Ctx, Refusal};

const USAGE: &str = "Usage: .explain-art <read-only query>";

/// Narrowest and widest chart drawn, in terminal columns.
const MIN_WIDTH: usize = 20;
const MAX_WIDTH: usize = 200;
/// Width when the terminal cannot be asked.
const DEFAULT_WIDTH: usize = 80;
/// Deepest operator row drawn; deeper operators are cut, never wrapped.
const MAX_ROWS: usize = 24;

/// One operator, reduced to what the flame needs.
#[derive(Debug, Clone)]
struct Frame {
    label: String,
    rows: u64,
    db_hits: u64,
    time_ns: u128,
    children: Vec<Frame>,
}

impl Frame {
    /// Reduce the engine's profiled tree; no plan parsing happens here.
    /// The synthetic nodes (`Union`, `Next`, ...) carry no metrics of
    /// their own and count as zero; the caller checks the tree is profiled.
    fn from_plan(node: &relate::PlanNode) -> Frame {
        let (rows, db_hits, time_ns) = node
            .metrics
            .map_or((0, 0, 0), |m| (m.rows, m.db_hits, m.time_ns));
        Frame {
            label: node.operator.clone(),
            rows,
            db_hits,
            time_ns,
            children: node.children.iter().map(Frame::from_plan).collect(),
        }
    }

    fn leaves(&self) -> u128 {
        if self.children.is_empty() {
            1
        } else {
            self.children.iter().map(Frame::leaves).sum()
        }
    }
}

/// What bar width stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Metric {
    Time,
    DbHits,
    Rows,
    Leaves,
}

impl Metric {
    /// Time when the root registered any, else the first metric that did.
    fn pick(root: &Frame) -> Metric {
        if root.time_ns > 0 {
            Metric::Time
        } else if root.db_hits > 0 {
            Metric::DbHits
        } else if root.rows > 0 {
            Metric::Rows
        } else {
            Metric::Leaves
        }
    }

    fn value(self, f: &Frame) -> u128 {
        match self {
            Metric::Time => f.time_ns,
            Metric::DbHits => u128::from(f.db_hits),
            Metric::Rows => u128::from(f.rows),
            Metric::Leaves => f.leaves(),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Metric::Time => "time",
            Metric::DbHits => "db hits",
            Metric::Rows => "rows",
            Metric::Leaves => "operator count",
        }
    }

    fn show(self, v: u128, utf8: bool) -> String {
        match self {
            Metric::Time => fmt_ns(v, utf8),
            _ => v.to_string(),
        }
    }
}

fn fmt_ns(ns: u128, utf8: bool) -> String {
    let micro = if utf8 { "\u{b5}s" } else { "us" };
    if ns < 1_000 {
        return format!("{ns}ns");
    }
    // Round to the printed precision first, then pick the unit, so a value
    // that rounds up to 1000.0 moves to the next unit.
    for (div, unit, prec) in [(1e3, micro, 1), (1e6, "ms", 1), (1e9, "s", 2)] {
        let v = ns as f64 / div;
        let scale = 10f64.powi(prec);
        if (v * scale).round() / scale < 1000.0 || unit == "s" {
            return format!("{v:.prec$}{unit}", prec = prec as usize);
        }
    }
    unreachable!("the seconds arm always returns")
}

/// One drawn bar: the half-open column range `[start, end)` on a row.
#[derive(Debug, PartialEq, Eq)]
struct Bar {
    depth: usize,
    start: usize,
    end: usize,
    label: String,
    tone: usize,
}

/// Lay the tree out over `width` columns. A bar narrower than one column
/// is dropped with its subtree, so the bar count is bounded by the width.
fn layout(root: &Frame, metric: Metric, width: usize) -> Vec<Bar> {
    let mut bars = Vec::new();
    place(root, metric, (0, width), 0, 0, &mut bars);
    bars
}

fn place(
    f: &Frame,
    metric: Metric,
    (x0, x1): (usize, usize),
    depth: usize,
    sibling: usize,
    bars: &mut Vec<Bar>,
) {
    if x1 <= x0 || depth >= MAX_ROWS {
        return;
    }
    bars.push(Bar {
        depth,
        start: x0,
        end: x1,
        label: label_of(f, metric),
        tone: (depth + sibling) % TONES.len(),
    });
    let sum: u128 = f.children.iter().map(|c| metric.value(c)).sum();
    // Scale down only: a parent that out-weighs its children keeps a gap.
    let den = sum.max(metric.value(f));
    if den == 0 {
        return;
    }
    let avail = (x1 - x0) as u128;
    let mut cum = 0u128;
    for (i, c) in f.children.iter().enumerate() {
        let v = metric.value(c);
        let s = x0 + (cum * avail / den) as usize;
        let e = x0 + ((cum + v) * avail / den) as usize;
        cum += v;
        place(c, metric, (s, e), depth + 1, i, bars);
    }
}

/// `Operator 5 rows 1.2ms`, control and non-ASCII characters replaced so a
/// label can never move the cursor or widen a cell.
fn label_of(f: &Frame, metric: Metric) -> String {
    let mut s = format!("{} {} rows", f.label, f.rows);
    if metric == Metric::Time {
        s.push(' ');
        s.push_str(&fmt_ns(f.time_ns, false));
    }
    s.chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .collect()
}

/// The text inside a bar of `span` columns: a leading pad cell, then the
/// label cut to fit (with an ellipsis where the locale has one).
fn fit(label: &str, span: usize, utf8: bool) -> Vec<char> {
    let room = span.saturating_sub(1);
    let chars: Vec<char> = label.chars().collect();
    if chars.len() <= room {
        return chars;
    }
    match (room, utf8) {
        (0, _) => Vec::new(),
        (_, true) => chars[..room - 1]
            .iter()
            .copied()
            .chain(['\u{2026}'])
            .collect(),
        (_, false) => chars[..room].to_vec(),
    }
}

/// The plain fill for a tone: the brand shade glyphs, or ASCII off UTF-8.
fn fill_char(tone: usize, utf8: bool) -> char {
    if utf8 {
        TONES[tone].shade
    } else {
        ['#', '=', '-'][tone % 3]
    }
}

#[derive(Clone, Copy)]
struct Cell {
    ch: char,
    tone: Option<usize>,
}

/// Draw the bars as text lines: truecolour backgrounds with the Archie
/// tones, or shade glyphs otherwise. Every coloured line ends in a reset.
fn render(bars: &[Bar], width: usize, caps: Caps) -> Vec<String> {
    let depth = bars.iter().map(|b| b.depth + 1).max().unwrap_or(0);
    let mut grid = vec![
        vec![
            Cell {
                ch: ' ',
                tone: None
            };
            width
        ];
        depth
    ];
    for b in bars {
        let row = &mut grid[b.depth];
        let text = fit(&b.label, b.end - b.start, caps.utf8);
        for (i, cell) in row[b.start..b.end].iter_mut().enumerate() {
            let ch = match i.checked_sub(1).and_then(|k| text.get(k)) {
                Some(&c) => c,
                None if caps.truecolor => ' ',
                None => fill_char(b.tone, caps.utf8),
            };
            *cell = Cell {
                ch,
                tone: Some(b.tone),
            };
        }
    }
    grid.iter().map(|row| row_text(row, caps)).collect()
}

fn row_text(row: &[Cell], caps: Caps) -> String {
    let mut out = String::new();
    let mut current: Option<usize> = None;
    for c in row {
        if caps.truecolor && c.tone != current {
            match c.tone {
                Some(t) => {
                    let Rgb(r, g, b) = TONES[t].rgb;
                    out.push_str(&format!("\x1b[48;2;{r};{g};{b}m\x1b[38;2;255;255;255m"));
                }
                None => out.push_str("\x1b[0m"),
            }
            current = c.tone;
        }
        out.push(c.ch);
    }
    if caps.truecolor {
        out.push_str("\x1b[0m");
        out
    } else {
        out.trim_end().to_string()
    }
}

/// The whole chart: a header line, then the rows.
fn chart(root: &Frame, width: usize, caps: Caps) -> String {
    let metric = Metric::pick(root);
    let total = metric.value(root);
    let mut out = format!(
        "plan flame: width = {}, total {}\n",
        metric.name(),
        metric.show(total, caps.utf8)
    );
    for line in render(&layout(root, metric, width), width, caps) {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The terminal's column count, clamped; `COLUMNS` then 80 when unasked.
fn terminal_width() -> usize {
    #[cfg(unix)]
    {
        // SAFETY: a TIOCGWINSZ probe on stdout with a valid out-parameter;
        // the kernel fills it or fails.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0
            && ws.ws_col > 0
        {
            return usize::from(ws.ws_col).clamp(MIN_WIDTH, MAX_WIDTH);
        }
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_WIDTH)
        .clamp(MIN_WIDTH, MAX_WIDTH)
}

/// `.explain-art <query>`.
pub(crate) fn run(ctx: &mut Ctx<'_>, args: &[&str]) -> Result<(), Refusal> {
    let joined = args.join(" ");
    let stmt = joined.trim().trim_end_matches(';').trim();
    if stmt.is_empty() {
        return Err(Refusal::Usage(USAGE));
    }
    let engine = |e: relate::Error| Refusal::Declined(e.to_string());
    // EXPLAIN executes nothing, and reports the wrapped statement's type.
    let planned = ctx.conn.query(&format!("EXPLAIN {stmt}")).map_err(engine)?;
    let kind = match planned.header.query_type {
        relate::QueryType::Read => None,
        relate::QueryType::Write => Some("write"),
        relate::QueryType::ReadWrite => Some("read-write"),
        relate::QueryType::Schema => Some("schema"),
    };
    if let Some(kind) = kind {
        return Err(Refusal::Declined(format!(
            ".explain-art runs the query to time it, so it takes read-only \
             statements; refusing a {kind} statement"
        )));
    }
    let profiled = ctx.conn.query(&format!("PROFILE {stmt}")).map_err(engine)?;
    let root = profiled
        .plan_root()
        .ok_or_else(|| Refusal::Declined("the engine returned no plan tree".into()))?;
    if !root.is_profiled() {
        return Err(Refusal::Declined(
            "the plan carries no PROFILE metrics to draw".into(),
        ));
    }
    let text = chart(&Frame::from_plan(root), terminal_width(), ctx.caps);
    ctx.out
        .write_all(text.as_bytes())
        .and_then(|()| ctx.out.flush())
        .map_err(|e| Refusal::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(label: &str, time_ns: u128, children: Vec<Frame>) -> Frame {
        Frame {
            label: label.into(),
            rows: 3,
            db_hits: 0,
            time_ns,
            children,
        }
    }

    fn plan() -> Frame {
        f(
            "ProduceResults",
            1000,
            vec![f(
                "Expand",
                800,
                vec![f("NodeByLabelScan", 600, vec![]), f("Filter", 100, vec![])],
            )],
        )
    }

    const PLAIN: Caps = Caps {
        utf8: true,
        truecolor: false,
    };
    const TRUE: Caps = Caps {
        utf8: true,
        truecolor: true,
    };

    #[test]
    fn child_widths_are_proportional_and_nested() {
        let bars = layout(&plan(), Metric::Time, 100);
        let by = |d: usize| bars.iter().filter(|b| b.depth == d).collect::<Vec<_>>();
        assert_eq!((by(0)[0].start, by(0)[0].end), (0, 100));
        assert_eq!((by(1)[0].start, by(1)[0].end), (0, 80));
        let leaves = by(2);
        assert_eq!((leaves[0].start, leaves[0].end), (0, 60));
        assert_eq!((leaves[1].start, leaves[1].end), (60, 70));
        for b in &bars {
            assert!(b.end <= 100 && b.start < b.end);
        }
    }

    #[test]
    fn overweight_children_are_scaled_under_the_parent() {
        let p = f("A", 100, vec![f("B", 100, vec![]), f("C", 100, vec![])]);
        let bars = layout(&p, Metric::Time, 40);
        assert_eq!((bars[1].start, bars[1].end), (0, 20));
        assert_eq!((bars[2].start, bars[2].end), (20, 40));
    }

    #[test]
    fn slivers_are_dropped_and_depth_is_bounded() {
        let p = f("A", 1000, vec![f("tiny", 1, vec![]), f("big", 999, vec![])]);
        let bars = layout(&p, Metric::Time, 20);
        assert!(bars.iter().all(|b| b.label != "tiny 3 rows 1ns"));
        let mut deep = f("leaf", 10, vec![]);
        for _ in 0..100 {
            deep = f("n", 10, vec![deep]);
        }
        assert_eq!(layout(&deep, Metric::Time, 30).len(), MAX_ROWS);
    }

    #[test]
    fn metric_falls_back_when_time_is_zero() {
        let mut p = f("A", 0, vec![]);
        assert_eq!(Metric::pick(&p), Metric::Rows);
        p.db_hits = 4;
        assert_eq!(Metric::pick(&p), Metric::DbHits);
        p.rows = 0;
        p.db_hits = 0;
        assert_eq!(Metric::pick(&p), Metric::Leaves);
        assert_eq!(Metric::pick(&plan()), Metric::Time);
    }

    #[test]
    fn plain_render_fits_the_width_and_uses_shades() {
        let text = chart(&plan(), 60, PLAIN);
        let mut lines = text.lines();
        assert!(lines
            .next()
            .unwrap()
            .starts_with("plan flame: width = time"));
        let body: Vec<&str> = lines.collect();
        assert_eq!(body.len(), 3);
        assert!(body.iter().all(|l| l.chars().count() <= 60));
        assert!(body[0].contains("ProduceResults"));
        assert!(body[0].contains('\u{2593}') || body[0].contains('\u{2592}'));
        assert!(!text.contains('\x1b'));
    }

    #[test]
    fn truecolor_render_resets_every_line() {
        let text = chart(&plan(), 60, TRUE);
        for line in text.lines().skip(1) {
            assert!(line.contains("\x1b[48;2;"), "{line:?}");
            assert!(line.ends_with("\x1b[0m"), "{line:?}");
        }
    }

    #[test]
    fn narrow_bars_truncate_labels_safely() {
        let p = f("A\u{1b}[31mVeryLongOperatorName\n", 10, vec![]);
        for w in [MIN_WIDTH, 8, 1] {
            let text = chart(&p, w, PLAIN);
            let body = text.lines().nth(1).unwrap();
            assert!(body.chars().count() <= w, "{body:?}");
            assert!(!text.contains('\x1b') && !body.contains('\n'));
        }
        let ascii = chart(
            &p,
            12,
            Caps {
                utf8: false,
                truecolor: false,
            },
        );
        assert!(ascii.is_ascii());
    }

    #[test]
    fn times_format_in_their_unit() {
        assert_eq!(fmt_ns(999, true), "999ns");
        assert_eq!(fmt_ns(1_500, false), "1.5us");
        assert_eq!(fmt_ns(2_500_000, true), "2.5ms");
        assert_eq!(fmt_ns(3_000_000_000, true), "3.00s");
        assert_eq!(fmt_ns(999_960, false), "1.0ms");
        assert_eq!(fmt_ns(999_960_000, true), "1.00s");
    }

    fn session() -> (relate::Database, relate::Connection) {
        let db = relate::Database::open(":memory:").unwrap();
        let conn = db.connect().unwrap();
        (db, conn)
    }

    fn drive(
        conn: &mut relate::Connection,
        db: &relate::Database,
        args: &[&str],
    ) -> (Result<(), Refusal>, String) {
        let mut out = Vec::new();
        let r = run(
            &mut Ctx {
                db,
                conn,
                out: &mut out,
                caps: PLAIN,
            },
            args,
        );
        (r, String::from_utf8(out).unwrap())
    }

    #[test]
    fn profiles_a_real_read_query() {
        let (db, mut conn) = session();
        conn.query("CREATE (:P {n: 1}), (:P {n: 2})").unwrap();
        let (r, out) = drive(&mut conn, &db, &["MATCH", "(n:P)", "RETURN", "n.n;"]);
        r.unwrap();
        assert!(out.starts_with("plan flame:"), "{out}");
        assert!(out.contains("ProduceResults"), "{out}");
    }

    #[test]
    fn refuses_writes_without_running_them() {
        let (db, mut conn) = session();
        for q in ["CREATE (:X)", "CREATE (n:X) RETURN n"] {
            let words: Vec<&str> = q.split(' ').collect();
            let (r, out) = drive(&mut conn, &db, &words);
            assert!(matches!(r, Err(Refusal::Declined(_))), "{q}: {r:?}");
            assert!(out.is_empty());
        }
        let n = conn.query("MATCH (n:X) RETURN count(n) AS c").unwrap();
        assert_eq!(
            format!("{:?}", n.rows[0][0]),
            format!("{:?}", relate::Value::Int(0))
        );
    }

    #[test]
    fn union_queries_draw_despite_metricless_synthetic_nodes() {
        let (db, mut conn) = session();
        let (r, out) = drive(
            &mut conn,
            &db,
            &["RETURN", "1", "AS", "x", "UNION", "RETURN", "2", "AS", "x"],
        );
        r.unwrap();
        assert!(out.starts_with("plan flame:"), "{out}");
    }

    #[test]
    fn empty_and_broken_input_are_typed_refusals() {
        let (db, mut conn) = session();
        assert!(matches!(
            drive(&mut conn, &db, &[]).0,
            Err(Refusal::Usage(_))
        ));
        assert!(matches!(
            drive(&mut conn, &db, &[";"]).0,
            Err(Refusal::Usage(_))
        ));
        assert!(matches!(
            drive(&mut conn, &db, &["MATCH", "("]).0,
            Err(Refusal::Declined(_))
        ));
    }
}
