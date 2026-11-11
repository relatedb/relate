//! `shortestPath` / `allShortestPaths` in expression form: one value per
//! input row. The front end lowers the expression's pattern to the
//! selector form the `MATCH` clause uses (ADR-0568), so it runs on the one
//! shortest route: each (start, end) partition's rows fold into the
//! global shortest, no row retained but the winners (ADR-0555). A shape
//! the route declines enumerates and keeps the shortest (ADR-0025).

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::PathPattern;

use crate::ctx::ExecCtx;
use crate::exec::gpm::fold_shortest_route;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::pattern::collect_pattern_rows;

#[derive(Clone, Copy)]
pub(crate) enum ShortestWant {
    Path,
    Length,
}

/// The internal name an anonymous expression pattern binds its path under:
/// both engines bind only a named path, and the fold reads it back.
const ANONYMOUS_PATH: &str = "__shortest_path";

/// `path` with a name to bind, and that name — the one spelling both the
/// route and the enumeration fallback read the path value through. A
/// pattern the user named keeps its own name (and is not cloned).
fn named_path(path: &PathPattern) -> (String, std::borrow::Cow<'_, PathPattern>) {
    match &path.variable {
        // The carried path name renders once (a row bind feed).
        Some(n) => (n.to_string(), std::borrow::Cow::Borrowed(path)),
        None => (
            ANONYMOUS_PATH.to_string(),
            std::borrow::Cow::Owned(PathPattern {
                variable: Some(ANONYMOUS_PATH.into()),
                subpath_variable: None,
                ..path.clone()
            }),
        ),
    }
}

/// The shortest paths seen so far: the one winner for `shortestPath`
/// (witness among equals unspecified), every path of the minimum length
/// for `allShortestPaths`.
struct Shortest {
    all: bool,
    len: usize,
    paths: Vec<Value>,
}

impl Shortest {
    fn new(all: bool) -> Self {
        Self {
            all,
            len: 0,
            paths: Vec::new(),
        }
    }

    fn offer(&mut self, path: Value, len: usize) {
        if self.paths.is_empty() || len < self.len {
            self.len = len;
            self.paths.clear();
            self.paths.push(path);
        } else if self.all && len == self.len {
            self.paths.push(path);
        }
    }

    /// Offer the path `row` binds under `name`, if it binds one.
    fn offer_row(&mut self, name: &str, mut row: Row) {
        if let Some(Value::Path(p)) = row.swap_remove(name) {
            let len = p.rels.len();
            self.offer(Value::Path(p), len);
        }
    }

    fn into_value(mut self) -> Value {
        if self.paths.is_empty() {
            Value::Null
        } else if self.all {
            Value::List(self.paths.into())
        } else {
            self.paths.swap_remove(0)
        }
    }
}

pub(crate) fn eval_shortest(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    all: bool,
    path: &PathPattern,
    want: ShortestWant,
    cypher_version: Option<u32>,
) -> Result<Value> {
    let (name, path) = named_path(path);
    let mut best = Shortest::new(all);
    let routed = fold_shortest_route(
        ExecCtx::new(graph, params, cypher_version),
        &path,
        row,
        &mut |r| {
            best.offer_row(&name, r);
            Ok(())
        },
    )?;
    let value = if routed {
        best.into_value()
    } else {
        enumerate(graph, row, params, all, &name, &path, cypher_version)?
    };
    Ok(shortest_from_enum(value, want))
}

fn shortest_from_enum(v: Value, want: ShortestWant) -> Value {
    match want {
        ShortestWant::Path => v,
        ShortestWant::Length => match v {
            Value::Path(p) => Value::Int(p.rels.len() as i64),
            Value::List(xs) => Value::Int(xs.len() as i64),
            other => other,
        },
    }
}

/// The fallback for every shape the route declines (a minimum above one, a
/// multi-relationship or quantified body, a predicate the search cannot
/// decide): enumerate the pattern's rows and keep the shortest.
fn enumerate(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    all: bool,
    name: &str,
    path: &PathPattern,
    cypher_version: Option<u32>,
) -> Result<Value> {
    let rows = collect_pattern_rows(graph, row, path, params, cypher_version)?;
    let mut best = Shortest::new(all);
    for r in rows {
        best.offer_row(name, r);
    }
    Ok(best.into_value())
}
