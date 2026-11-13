//! `shortestPath` / `allShortestPaths` in expression form: one value per
//! input row. The front end lowers the expression's pattern to the
//! selector form the `MATCH` clause uses (ADR-0568), so it runs on the one
//! shortest route: each (start, end) partition's rows fold into the
//! global shortest, no row retained but the winners (ADR-0555). A shape
//! the route declines enumerates and folds the shortest (ADR-0025, ADR-0555).

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::PathPattern;

use crate::ctx::ExecCtx;
use crate::eval_walk::fold_pattern_rows;
use crate::exec::gpm::fold_shortest_route;
use crate::overlay::ExecGraph;
use crate::row::Row;

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

    /// The answer in the shape `want` asks: the path (or list of paths),
    /// or its length (the list's count for `allShortestPaths`); null when
    /// no path was seen. Decided by the tracked `all` and the path's own
    /// relationship count, never by sniffing the built value.
    fn into_value(mut self, want: ShortestWant) -> Value {
        if self.paths.is_empty() {
            return Value::Null;
        }
        match (want, self.all) {
            (ShortestWant::Path, true) => Value::List(self.paths.into()),
            (ShortestWant::Path, false) => self.paths.swap_remove(0),
            (ShortestWant::Length, true) => Value::Int(self.paths.len() as i64),
            (ShortestWant::Length, false) => Value::Int(self.len as i64),
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
) -> Result<Value> {
    let (name, path) = named_path(path);
    let mut best = Shortest::new(all);
    let routed = fold_shortest_route(ExecCtx::new(graph, params), &path, row, &mut |r| {
        best.offer_row(&name, r);
        Ok(())
    })?;
    if !routed {
        // The fallback for every shape the route declines (a minimum above
        // one, a multi-relationship or quantified body, a predicate the
        // search cannot decide): enumerate the pattern's rows and keep the
        // shortest, into the same accumulator.
        fold_pattern_rows(graph, row, &path, params, &mut |r| {
            best.offer_row(&name, r);
            Ok(())
        })?;
    }
    Ok(best.into_value(want))
}
