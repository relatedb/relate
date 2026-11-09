//! `.graph [MATCH ...]`: draw the open store, or the nodes and
//! relationships a read-only query returns, as a force-directed picture on
//! the braille canvas. Capped at [`MAX_NODES`] nodes and [`MAX_RELS`]
//! relationships; the footer always says "showing N of M".

use std::collections::{HashMap, HashSet};

use relate::{Connection, Node, QueryType, Relationship, Value};

use crate::canvas::Canvas;
use crate::explain_art::statement;
use crate::layout::layout;
use crate::{Ctx, Refusal};

const MAX_NODES: usize = 50;
const MAX_RELS: usize = 100;
/// Rows of a user query scanned for graph values.
const MAX_ROWS: usize = 5_000;
/// Nesting followed inside lists, maps and paths.
const MAX_DEPTH: usize = 8;
const LABEL_CHARS: usize = 10;
const MIN_COLS: usize = 40;
const DEFAULT_COLS: usize = 80;
const DEFAULT_ROWS: usize = 24;
const USAGE: &str = "Usage: .graph [MATCH ... query]";
/// Properties tried, in order, for a node's short label.
const NAME_KEYS: [&str; 4] = ["name", "title", "id", "label"];

/// What gets drawn, plus how much of the whole it is.
#[derive(Debug, Default, PartialEq)]
struct GraphData {
    labels: Vec<String>,
    edges: Vec<(usize, usize)>,
    total_nodes: usize,
    total_rels: usize,
}

/// Collects nodes and relationships up to the caps, counting the rest.
#[derive(Default)]
struct Collector {
    index: HashMap<i64, usize>,
    labels: Vec<String>,
    seen_nodes: HashSet<i64>,
    rels: Vec<(i64, i64)>,
    seen_rels: HashSet<i64>,
}

impl Collector {
    fn node(&mut self, n: &Node) {
        self.seen_nodes.insert(n.id);
        if self.labels.len() < MAX_NODES && !self.index.contains_key(&n.id) {
            self.index.insert(n.id, self.labels.len());
            self.labels.push(short_label(n));
        }
    }

    fn rel(&mut self, r: &Relationship) {
        // An endpoint a bare relationship names still counts as a node.
        self.seen_nodes.insert(r.start);
        self.seen_nodes.insert(r.end);
        if self.seen_rels.insert(r.id) {
            self.rels.push((r.start, r.end));
        }
    }

    fn value(&mut self, v: &Value, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        match v {
            Value::Node(n) => self.node(n),
            Value::Relationship(r) => self.rel(r),
            Value::Path(p) => {
                p.nodes.iter().for_each(|n| self.node(n));
                p.rels.iter().for_each(|r| self.rel(r));
            }
            Value::List(items) => items.iter().for_each(|v| self.value(v, depth + 1)),
            Value::Map(m) => m.values().for_each(|v| self.value(v, depth + 1)),
            _ => {}
        }
    }

    /// Edges between kept nodes only, capped; totals are what was seen.
    fn finish(self) -> GraphData {
        let total_rels = self.seen_rels.len();
        let edges = self
            .rels
            .iter()
            .filter_map(|(a, b)| Some((*self.index.get(a)?, *self.index.get(b)?)))
            .take(MAX_RELS)
            .collect();
        GraphData {
            labels: self.labels,
            edges,
            total_nodes: self.seen_nodes.len(),
            total_rels,
        }
    }
}

/// A short, terminal-safe label: the first of the usual name properties
/// that is a string or number, else the first label, else the id.
fn short_label(n: &Node) -> String {
    let named = NAME_KEYS.iter().find_map(|k| match n.properties.get(k)? {
        Value::String(s) => Some(s.to_string()),
        Value::Int(i) => Some(i.to_string()),
        _ => None,
    });
    let raw = named
        .or_else(|| n.labels.first().cloned())
        .unwrap_or_else(|| format!("#{}", n.id));
    let clean: String = raw.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    if clean.is_empty() {
        return format!("#{}", n.id);
    }
    clean.chars().take(LABEL_CHARS).collect()
}

fn io_err(e: impl std::fmt::Display) -> Refusal {
    Refusal::Io(e.to_string())
}

fn scalar_count(conn: &mut Connection, cypher: &str) -> Result<usize, Refusal> {
    let result = conn.query(cypher).map_err(io_err)?;
    match result.rows.first().and_then(|r| r.first()) {
        Some(Value::Int(i)) => {
            usize::try_from(*i).map_err(|_| io_err(format!("count out of range: {i}")))
        }
        other => Err(io_err(format!("unexpected count result {other:?}"))),
    }
}

/// The whole store: the first nodes, the relationships among them, and the
/// store-wide totals.
fn gather_store(conn: &mut Connection) -> Result<GraphData, Refusal> {
    let total_nodes = scalar_count(conn, "MATCH (n) RETURN count(n)")?;
    let total_rels = scalar_count(conn, "MATCH ()-[r]->() RETURN count(r)")?;
    let mut c = Collector::default();
    let nodes = conn
        .query(&format!("MATCH (n) RETURN n LIMIT {MAX_NODES}"))
        .map_err(io_err)?;
    for row in &nodes.rows {
        row.iter().for_each(|v| c.value(v, 0));
    }
    let ids: Vec<String> = c.index.keys().map(i64::to_string).collect();
    if !ids.is_empty() {
        let ids = ids.join(",");
        let rels = conn
            .query(&format!(
                "MATCH (a)-[r]->(b) WHERE id(a) IN [{ids}] AND id(b) IN [{ids}] \
                 RETURN r LIMIT {MAX_RELS}"
            ))
            .map_err(io_err)?;
        for row in &rels.rows {
            row.iter().for_each(|v| c.value(v, 0));
        }
    }
    let mut data = c.finish();
    data.total_nodes = total_nodes;
    data.total_rels = total_rels;
    Ok(data)
}

/// The graph values a read-only query returns. `EXPLAIN` first, so a write
/// is refused before it can run.
fn gather_query(conn: &mut Connection, query: &str) -> Result<GraphData, Refusal> {
    let plan = conn.query(&format!("EXPLAIN {query}")).map_err(io_err)?;
    if plan.header.query_type != QueryType::Read {
        return Err(io_err(".graph only draws read-only queries"));
    }
    let result = conn.query(query).map_err(io_err)?;
    let mut c = Collector::default();
    for row in result.rows.iter().take(MAX_ROWS) {
        row.iter().for_each(|v| c.value(v, 0));
    }
    Ok(c.finish())
}

/// Terminal size: the width from the CLI's one probe ([`Ctx::columns`],
/// 80 when it cannot be asked), the rows from `LINES` (24 when missing or
/// unusable). A probed width under [`MIN_COLS`] is refused.
fn terminal_size(
    columns: Option<usize>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<(usize, usize), Refusal> {
    let cols = columns.unwrap_or(DEFAULT_COLS);
    if cols < MIN_COLS {
        return Err(Refusal::Usage(
            "terminal too narrow for .graph (need 40 columns)",
        ));
    }
    let rows = env("LINES")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_ROWS);
    Ok((cols, rows))
}

/// Draw `data` on a canvas sized to the terminal.
fn draw(data: &GraphData, cols: usize, rows: usize) -> String {
    let mut canvas = Canvas::new(cols.min(100), rows.saturating_sub(4).clamp(8, 40));
    let pos = layout(data.labels.len(), &data.edges, 0x6772_6170);
    let label_cols = data
        .labels
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let (w, h) = (canvas.width(), canvas.height());
    let x_room = (w - 8 - 2 * label_cols as i32).max(1);
    let y_room = (h - 8).max(1);
    let px: Vec<(i32, i32)> = pos
        .iter()
        .map(|p| {
            (
                4 + (p.0 * f64::from(x_room)) as i32,
                4 + (p.1 * f64::from(y_room)) as i32,
            )
        })
        .collect();
    for &(a, b) in &data.edges {
        canvas.line(px[a].0, px[a].1, px[b].0, px[b].1);
    }
    for (&(x, y), label) in px.iter().zip(&data.labels) {
        canvas.circle(x, y, 2);
        canvas.circle(x, y, 1);
        canvas.set(x, y);
        canvas.put_text((x / 2) as usize + 2, (y / 4) as usize, label);
    }
    canvas.render()
}

pub(crate) fn run(ctx: &mut Ctx<'_>, args: &[&str]) -> Result<(), Refusal> {
    if !ctx.caps.utf8 {
        return Err(Refusal::NotUtf8);
    }
    let (cols, rows) = terminal_size(ctx.columns, &|k| std::env::var(k).ok())?;
    // The raw tail, never the re-joined words: whitespace inside a string
    // literal is data.
    let query = statement(ctx.raw_args);
    let data = if query.is_empty() {
        gather_store(ctx.conn)?
    } else if args[0].eq_ignore_ascii_case("match") {
        gather_query(ctx.conn, query)?
    } else {
        return Err(Refusal::Usage(USAGE));
    };
    let mut text = if data.labels.is_empty() {
        "(no nodes to draw)\n".to_string()
    } else {
        draw(&data, cols, rows)
    };
    text.push_str(&format!(
        "showing {} of {} nodes, {} of {} relationships\n",
        data.labels.len(),
        data.total_nodes.max(data.labels.len()),
        data.edges.len(),
        data.total_rels.max(data.edges.len()),
    ));
    ctx.out.write_all(text.as_bytes()).map_err(io_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> (relate::Database, Connection) {
        let db = relate::Database::open(":memory:").expect("open");
        let conn = db.connect().expect("connect");
        (db, conn)
    }

    fn caps() -> crate::Caps {
        crate::Caps {
            utf8: true,
            truecolor: false,
        }
    }

    fn run_in(
        conn: &mut Connection,
        db: &relate::Database,
        args: &[&str],
    ) -> (Result<(), Refusal>, String) {
        let mut buf = Vec::new();
        let r = run(
            &mut Ctx {
                db,
                conn,
                out: &mut buf,
                caps: caps(),
                raw_args: &args.join(" "),
                columns: None,
            },
            args,
        );
        (r, String::from_utf8(buf).expect("utf8"))
    }

    #[test]
    fn store_graph_draws_labels_and_totals() {
        let (db, mut c) = conn();
        c.query("CREATE (:P {name:'Ada'})-[:KNOWS]->(:P {name:'Grace'})")
            .unwrap();
        let (r, out) = run_in(&mut c, &db, &[]);
        r.unwrap();
        assert!(out.contains("Ada") && out.contains("Grace"), "{out}");
        assert!(
            out.contains("showing 2 of 2 nodes, 1 of 1 relationships"),
            "{out}"
        );
    }

    #[test]
    fn caps_report_showing_n_of_m() {
        let (db, mut c) = conn();
        c.query("UNWIND range(1,60) AS i CREATE (:N {id:i})")
            .unwrap();
        let (r, out) = run_in(&mut c, &db, &[]);
        r.unwrap();
        assert!(out.contains("showing 50 of 60 nodes"), "{out}");
    }

    #[test]
    fn query_form_draws_only_returned_graph_and_refuses_writes() {
        let (db, mut c) = conn();
        c.query("CREATE (:A {name:'x'})-[:R]->(:B {name:'y'}), (:C {name:'z'})")
            .unwrap();
        let (r, out) = run_in(&mut c, &db, &["MATCH", "p=(:A)-[:R]->(:B)", "RETURN", "p"]);
        r.unwrap();
        assert!(out.contains("showing 2 of 2 nodes, 1 of 1"), "{out}");
        assert!(!out.contains('z'));
        let (r, _) = run_in(&mut c, &db, &["MATCH", "(n)", "DETACH", "DELETE", "n"]);
        assert!(r.is_err());
        let (_, rows) = (0, c.query("MATCH (n) RETURN count(n)").unwrap().rows);
        assert_eq!(rows[0][0], Value::Int(3));
    }

    #[test]
    fn odd_arguments_and_empty_graphs_are_handled() {
        let (db, mut c) = conn();
        let (r, out) = run_in(&mut c, &db, &[]);
        r.unwrap();
        assert!(out.contains("(no nodes to draw)") && out.contains("showing 0 of 0"));
        let (r, _) = run_in(&mut c, &db, &["CREATE", "(n)"]);
        assert_eq!(r, Err(Refusal::Usage(USAGE)));
        let (r, _) = run_in(&mut c, &db, &["MATCH", "nonsense"]);
        assert!(matches!(r, Err(Refusal::Io(_))));
    }

    #[test]
    fn labels_are_short_and_free_of_control_characters() {
        let (db, mut c) = conn();
        c.query("CREATE (:T {name:'\\u001b[31mevil-and-very-long-name'})")
            .unwrap();
        let (r, out) = run_in(&mut c, &db, &[]);
        r.unwrap();
        assert!(!out.contains('\x1b'), "{out:?}");
        assert!(!out.contains("very-long-name"));
    }

    #[test]
    fn terminal_size_refuses_unusable_widths() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(terminal_size(None, &env(&[])).unwrap(), (80, 24));
        assert_eq!(
            terminal_size(Some(120), &env(&[("LINES", "50")])).unwrap(),
            (120, 50)
        );
        assert_eq!(
            terminal_size(Some(40), &env(&[("LINES", "junk")])).unwrap(),
            (40, 24)
        );
        assert!(terminal_size(Some(20), &env(&[])).is_err());
    }

    #[test]
    fn the_raw_tail_keeps_whitespace_inside_literals() {
        let (db, mut c) = conn();
        c.query("CREATE (:T {name:'a   b'})").unwrap();
        let raw = "MATCH (n:T) WHERE n.name = 'a   b' RETURN n;";
        let words: Vec<&str> = raw.split_whitespace().collect();
        let mut buf = Vec::new();
        let r = run(
            &mut Ctx {
                db: &db,
                conn: &mut c,
                out: &mut buf,
                caps: caps(),
                raw_args: raw,
                columns: None,
            },
            &words,
        );
        r.unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("showing 1 of 1 nodes"), "{out}");
    }

    #[test]
    fn non_utf8_locale_is_refused() {
        let (db, mut c) = conn();
        let mut buf = Vec::new();
        let r = run(
            &mut Ctx {
                db: &db,
                conn: &mut c,
                out: &mut buf,
                caps: crate::Caps {
                    utf8: false,
                    truecolor: false,
                },
                raw_args: "",
                columns: None,
            },
            &[],
        );
        assert_eq!(r, Err(Refusal::NotUtf8));
    }

    #[test]
    fn draw_stays_within_the_terminal_width() {
        let data = GraphData {
            labels: (0..50).map(|i| format!("node-{i}")).collect(),
            edges: (1..50).map(|i| (0, i)).collect(),
            total_nodes: 50,
            total_rels: 49,
        };
        let text = draw(&data, 60, 24);
        assert!(text.lines().all(|l| l.chars().count() <= 60));
        assert!(text.lines().count() <= 24);
    }
}
