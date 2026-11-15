//! Rows as gram (https://github.com/gram-data/tree-sitter-gram): a pattern for
//! a row that is one graph value, `[{scalar columns} | graph values]` for any
//! other. Values gram cannot hold are Cypher literals in a `cypher` tagged
//! string.
//!
//! Gram quotes names and strings its own way ([`gram_quoted`]), not as Cypher
//! does: a backtick in a name takes a backslash where Cypher doubles it
//! (`cypher_identifier`), and there is no `\u` escape (`quoted_literal`).

use std::collections::HashSet;

use super::decode::{entries, Node, Rel, Wire};
use super::literal::{arrow, props};
use relate_core::is_unsafe_output_char;

use crate::is_bare_identifier;

fn is_gram_integer(s: &str) -> bool {
    let d = s.strip_prefix('-').unwrap_or(s);
    d == "0"
        || (d.starts_with(|c: char| ('1'..='9').contains(&c))
            && d.chars().all(|c| c.is_ascii_digit()))
}

fn is_gram_decimal(s: &str) -> bool {
    match s.split_once('.') {
        Some((int, frac)) => {
            is_gram_integer(int) && !frac.is_empty() && frac.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// `s` between `quote`s as gram writes a quoted name or string: the quote, a
/// backslash, and `\b \f \n \r \t` take a backslash, and every other
/// character is written as it is.
fn gram_quoted(s: &str, quote: char) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Whether gram can write `s` as a string: every character a recording must
/// not carry raw ([`is_unsafe_output_char`]) has a gram escape.
fn gram_writes(s: &str) -> bool {
    !s.chars()
        .any(|c| is_unsafe_output_char(c) && !matches!(c, '\n' | '\r' | '\t' | '\u{8}' | '\u{c}'))
}

fn tagged(text: &str) -> String {
    format!("cypher{}", gram_quoted(text, '`'))
}

fn key(k: &str) -> String {
    if is_bare_identifier(k) {
        k.to_string()
    } else {
        gram_quoted(k, '"')
    }
}

/// A label or relationship type: bare, or a backticked gram name.
fn gram_name(name: &str) -> String {
    if is_bare_identifier(name) {
        name.to_string()
    } else {
        gram_quoted(name, '`')
    }
}

/// A scalar gram value (allowed inside arrays and maps), if `v` is one.
fn scalar(v: &Wire) -> Option<String> {
    match v {
        Wire::Null => Some("null".into()),
        Wire::Bool(b) => Some(b.to_string()),
        Wire::Number(n) => {
            let text = n.to_string();
            Some(if is_gram_integer(&text) || is_gram_decimal(&text) {
                text
            } else {
                tagged(&text)
            })
        }
        // A string gram cannot write is its exact Cypher literal, tagged.
        Wire::Str(s) => Some(if gram_writes(s) {
            gram_quoted(s, '"')
        } else {
            tagged(&v.cypher())
        }),
        _ => None,
    }
}

/// A gram record of scalar values, if every value is one.
fn scalar_record(entries: &[(&str, Wire)]) -> Option<String> {
    let parts = entries
        .iter()
        .map(|(k, v)| scalar(v).map(|s| format!("{}: {s}", key(k))))
        .collect::<Option<Vec<_>>>()?;
    Some(format!("{{{}}}", parts.join(", ")))
}

/// A gram value in record position: scalars, non-empty scalar arrays, and
/// maps of scalars (points included); everything else is a tagged literal.
fn value(v: &Wire) -> String {
    if let Some(s) = scalar(v) {
        return s;
    }
    let plain = match v {
        Wire::List(items) if !items.is_empty() => items
            .iter()
            .map(scalar)
            .collect::<Option<Vec<_>>>()
            .map(|parts| format!("[{}]", parts.join(", "))),
        Wire::Map(entries) => scalar_record(entries),
        Wire::Point(m) => scalar_record(&entries(m)),
        _ => None,
    };
    plain.unwrap_or_else(|| tagged(&v.cypher()))
}

fn record(entries: &[(&str, Wire)]) -> String {
    let parts: Vec<String> = entries
        .iter()
        .map(|(k, v)| format!("{}: {}", key(k), value(v)))
        .collect();
    format!("{{{}}}", parts.join(", "))
}

/// Gram patterns for the rows of one result: a node or relationship is
/// written in full once, then referenced by its identifier.
#[derive(Default)]
pub(super) struct Gram {
    nodes: HashSet<String>,
    rels: HashSet<String>,
}

impl Gram {
    fn node(&mut self, n: &Node) -> String {
        let id = n.id.to_string();
        if !self.nodes.insert(id.clone()) {
            return format!("(n{id})");
        }
        let labels: String = n
            .labels
            .iter()
            .map(|l| format!(":{}", gram_name(l)))
            .collect();
        format!("(n{id}{labels}{})", props(&n.props, record))
    }

    fn rel(&mut self, r: &Rel) -> String {
        let id = r.id.to_string();
        if !self.rels.insert(id.clone()) {
            return format!("[r{id}]");
        }
        format!("[r{id}:{}{}]", gram_name(r.ty), props(&r.props, record))
    }

    fn pattern(&mut self, v: &Wire) -> String {
        match v {
            Wire::Node(n) => self.node(n),
            Wire::Rel(r) => {
                let rel = self.rel(r);
                format!("(n{})-{rel}->(n{})", r.start, r.end)
            }
            Wire::Path(p) => {
                let mut out = self.node(&p.first);
                for (rel, node, forward) in &p.steps {
                    let rel = self.rel(rel);
                    out.push_str(&arrow(&rel, *forward));
                    out.push_str(&self.node(node));
                }
                out
            }
            _ => tagged(&v.cypher()),
        }
    }

    /// One row, `values` in `columns` order.
    pub(super) fn row(&mut self, columns: &[&str], values: &[Wire]) -> String {
        if let [only] = values {
            if only.is_graph() {
                return self.pattern(only);
            }
        }
        let mut scalars = Vec::new();
        let mut elements = Vec::new();
        for (col, v) in columns.iter().zip(values) {
            match v {
                _ if v.is_graph() => elements.push(self.pattern(v)),
                Wire::List(items) if !items.is_empty() && items.iter().all(Wire::is_graph) => {
                    let inner: Vec<String> = items.iter().map(|i| self.pattern(i)).collect();
                    elements.push(format!("[ | {}]", inner.join(", ")));
                }
                _ => scalars.push(format!("{}: {}", key(col), value(v))),
            }
        }
        let record = if scalars.is_empty() {
            String::new()
        } else {
            format!("{{{}}}", scalars.join(", "))
        };
        match (record.is_empty(), elements.is_empty()) {
            (_, true) => format!("[{record}]"),
            (true, false) => format!("[ | {}]", elements.join(", ")),
            (false, false) => format!("[{record} | {}]", elements.join(", ")),
        }
    }
}
