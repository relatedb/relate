//! Recordings: the canonical text form of a script's result (ADR-0132).
//!
//! A recording is the text a host keeps for a step's result, as a
//! ```` ```table ```` or ```` ```gram ```` block: the relate-web cookbook
//! records each recipe step this way. Hosts render with this module through
//! the WASM `renderResult` export (ADR-0131), so a recording reads the same
//! wherever it is made. It must lose nothing the wire envelope says.
//!
//! It renders wire JSON (`relate_model::wire`), not engine values:
//! numbers stay exactly as the wire spells them and strings are
//! double-quoted. [`crate::cypher_literal`] is the Cypher-literal form of
//! engine values, for `.mode cypher`; both escape identifiers and strings the
//! same way.
//!
//! - A script's result is its last statement's envelope, with mutation
//!   counters summed over every statement and listed in canonical order
//!   when non-zero (`nodes created: 3, …`).
//! - An error is a table recording `error: <message>`; a result without
//!   columns is `(no rows)` plus the counters.
//! - A result is **gram** when any row value holds a node, relationship, or
//!   path, and a **table** otherwise.
//!
//! Table: `| col |` header, a dashed rule, one line per row (every row, no
//! clipping), then `N rows` plus the counters. Values are Cypher literals:
//! strings double-quoted and escaped, numbers exactly as the wire spells them
//! (`1.0` stays `1.0`), lists `[a, b]`, maps `{k: v}` with keys in wire
//! order, points `point({…})`, vectors `vector([…], dim, TYPE)`.
//!
//! Gram: a `// N rows` comment (plus counters), then one line per row. A row
//! that is a single graph value is that pattern; any other row is
//! `[{scalar columns} | graph values]`, a list of graph values being
//! `[ | a, b]`. Nodes are `(n<id>:Label {props})` and relationships
//! `-[r<id>:TYPE {props}]->` at first occurrence, `(n<id>)` / `-[r<id>]->`
//! after. Values gram cannot hold (empty or nested lists, maps of maps,
//! vectors, exponent-form numbers, strings with a character gram has no
//! escape for) are exact Cypher literals in a `cypher` tagged string:
//! ``cypher`[]` ``. Gram quotes names and strings with its own escapes
//! (`gram.rs`), not Cypher's.

mod decode;
mod gram;
mod literal;

use relate_model::wire::STAT_COUNTERS;
use serde_json::{json, Map, Value};

use decode::{graph_of, Envelope, Wire};
use gram::Gram;

/// Which block a recording is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Table,
    Gram,
}

impl Kind {
    /// The block's fence info string.
    pub fn info(self) -> &'static str {
        match self {
            Kind::Table => "table",
            Kind::Gram => "gram",
        }
    }

    /// The kind whose fence info string is `info`.
    pub fn from_info(info: &str) -> Option<Kind> {
        match info {
            "table" => Some(Kind::Table),
            "gram" => Some(Kind::Gram),
            _ => None,
        }
    }
}

/// A table recording's cells, for hosts that lay it out themselves (a web
/// page can draw it as an HTML table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub columns: Vec<String>,
    /// Each row's cells as Cypher literals, in column order.
    pub rows: Vec<Vec<String>>,
    /// Per column: every value is a number or null.
    pub numeric: Vec<bool>,
    /// The closing line: the row count and the counters.
    pub summary: String,
}

/// A gram recording's nodes and relationships, each once, as the wire has
/// them: what a graph view draws.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<Value>,
    pub rels: Vec<Value>,
}

/// What a recording holds beyond its text.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    /// A table recording, with its cells when it has rows.
    Table(Option<Table>),
    /// A gram recording, with its graph.
    Gram(Graph),
}

/// A rendered result.
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    /// The recording's text, as the recipe keeps it.
    pub text: String,
    pub view: View,
    /// The mutation counters summed over the script, in canonical order.
    pub stats: Vec<(&'static str, u64)>,
}

impl Recording {
    pub fn kind(&self) -> Kind {
        match self.view {
            View::Table(_) => Kind::Table,
            View::Gram(_) => Kind::Gram,
        }
    }

    /// `{kind, text, table, graph, stats}` as JSON, the WASM `renderResult`
    /// result: `table` and `graph` are null unless the view has them, and
    /// `stats` holds every counter.
    pub fn to_json(&self) -> Value {
        let (table, graph) = match &self.view {
            View::Table(table) => (
                table.as_ref().map(|t| {
                    json!({
                        "columns": t.columns,
                        "rows": t.rows,
                        "numeric": t.numeric,
                        "summary": t.summary,
                    })
                }),
                None,
            ),
            View::Gram(g) => (None, Some(json!({"nodes": g.nodes, "rels": g.rels}))),
        };
        let stats: Map<String, Value> = self
            .stats
            .iter()
            .map(|(k, n)| ((*k).to_string(), (*n).into()))
            .collect();
        json!({
            "kind": self.kind().info(),
            "text": self.text,
            "table": table,
            "graph": graph,
            "stats": stats,
        })
    }
}

/// Render the envelopes of a script's statements, in order.
///
/// No envelopes render as `(no rows)`. An envelope that is not a wire
/// envelope (`{columns, rows, stats, elapsed_ms, error}`) is an error
/// naming it, as is a malformed `stats` object: a result envelope must
/// carry every counter as an unsigned integer, and an error envelope none.
pub fn render(envelopes: &[Value]) -> Result<Recording, String> {
    let envelopes = envelopes
        .iter()
        .enumerate()
        .map(|(i, e)| Envelope::decode(e).map_err(|why| format!("envelope {i}: {why}")))
        .collect::<Result<Vec<_>, _>>()?;
    let stats = summed_stats(&envelopes);
    let text_only = |text: String| Recording {
        text,
        view: View::Table(None),
        stats: stats.clone(),
    };
    let Some(last) = envelopes.last() else {
        return Ok(text_only("(no rows)".into()));
    };
    if let Some(err) = last.error {
        return Ok(text_only(format!("error: {err}")));
    }
    let counters = stats_text(&stats);
    let with_stats = |head: String| {
        if counters.is_empty() {
            head
        } else {
            format!("{head} · {counters}")
        }
    };
    if last.columns.is_empty() {
        return Ok(text_only(with_stats("(no rows)".into())));
    }
    let rows = last
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| last.cells(row).map_err(|why| format!("row {i}: {why}")))
        .collect::<Result<Vec<Vec<Wire>>, _>>()?;
    let n = rows.len();
    let count = with_stats(format!("{n} row{}", if n == 1 { "" } else { "s" }));

    if rows.iter().flatten().any(Wire::contains_graph) {
        let mut g = Gram::default();
        let mut out = vec![format!("// {count}")];
        out.extend(rows.iter().map(|row| g.row(&last.columns, row)));
        return Ok(Recording {
            text: out.join("\n"),
            view: View::Gram(graph_of(rows.iter().flatten())),
            stats,
        });
    }

    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(Wire::cypher).collect())
        .collect();
    let widths: Vec<usize> = (0..last.columns.len())
        .map(|i| {
            cells
                .iter()
                .map(|r| r[i].chars().count())
                .chain([last.columns[i].chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: &[&str]| {
        let padded: Vec<String> = cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c}{}", " ".repeat(w - c.chars().count())))
            .collect();
        format!("| {} |", padded.join(" | "))
    };
    let mut out = vec![line(&last.columns)];
    out.push(format!(
        "|{}|",
        widths
            .iter()
            .map(|w| "-".repeat(w + 2))
            .collect::<Vec<_>>()
            .join("|")
    ));
    out.extend(
        cells
            .iter()
            .map(|r| line(&r.iter().map(String::as_str).collect::<Vec<_>>())),
    );
    out.push(count.clone());
    let table = (n > 0).then(|| Table {
        columns: last.columns.iter().map(|c| c.to_string()).collect(),
        numeric: (0..last.columns.len())
            .map(|i| {
                rows.iter()
                    .all(|r| matches!(r[i], Wire::Null | Wire::Number(_)))
            })
            .collect(),
        rows: cells,
        summary: count,
    });
    Ok(Recording {
        text: out.join("\n"),
        view: View::Table(table),
        stats,
    })
}

/// Every counter, summed over `envelopes`, in canonical order.
///
/// [`Envelope::decode`] has already decoded each result envelope's `stats`
/// object into the typed snapshot — a result envelope carries every
/// canonical counter, an error envelope carries none — so this only sums
/// the snapshots. An error envelope contributes nothing.
fn summed_stats(envelopes: &[Envelope]) -> Vec<(&'static str, u64)> {
    STAT_COUNTERS
        .iter()
        .map(|counter| {
            let total = envelopes
                .iter()
                .filter_map(|envelope| envelope.stats.as_ref())
                .map(|stats| (counter.count)(stats))
                .sum();
            (counter.key, total)
        })
        .collect()
}

/// The non-zero counters as the recording lists them.
fn stats_text(stats: &[(&str, u64)]) -> String {
    stats
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(key, n)| format!("{}: {n}", key.replace('_', " ")))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests;
