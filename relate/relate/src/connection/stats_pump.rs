//! The statement-completion seam of the per-key statistics (ADR-0696 §3).
//!
//! A statistics consult never builds: it records a request and answers
//! `None`. The build is maintenance the graph's owner runs, and this is the
//! owner's one seam: every statement path (an auto-commit read or write, a
//! statement inside an explicit transaction, the streaming read) funnels
//! through `Connection::dispatch`, which calls [`Connection::serve_key_statistics`]
//! once the arm has returned.
//!
//! The seam serves **one** requested label per statement, so a statement
//! inherits at most one label's scan and later statements serve the rest. It
//! runs only after a statement that succeeded, and its scan stops at the next
//! chunk boundary when the statement's cancel token fires (the label stays
//! queued). It takes its own short-lived snapshot pin, after the statement's
//! pin is dropped: the pin keeps the graph cell alive across a follow reload
//! (ADR-0137), and a build needs nothing else from the store.

use super::Connection;
use crate::CancelToken;

/// Labels one statement's completion serves.
const LABELS_PER_STATEMENT: usize = 1;

impl Connection {
    /// Serve at most [`LABELS_PER_STATEMENT`] requested key-statistics
    /// builds on the `neo4j` graph, the one graph statistics are planned
    /// over (`GraphPatternStats` is built over it for every namespace).
    /// Maintenance is best effort: a refused admission or pin skips the
    /// pump, and the recorded requests wait for the next statement.
    pub(super) fn serve_key_statistics(&self, token: &CancelToken) {
        if token.is_cancelled() {
            return;
        }
        // The cell is dereferenced only under the publication read guard
        // (`GraphCell`'s contract: a follow reload may replace it otherwise),
        // and the pin is taken, under that guard, only when a label waits:
        // a statement costs one guard acquisition and one atomic load here
        // when no consult asked for statistics.
        let admit = || -> crate::Result<_> {
            let _read = self.inner.read_guard()?;
            let target: &crate::engines::LiveQuery = &self.inner.graph;
            let graph: &relate_executor::Graph = target;
            if !graph.has_pending_key_stats() {
                return Ok(None);
            }
            let pin = target.pin_now()?;
            Ok(Some((target, pin)))
        };
        let Ok(Some((target, pin))) = admit() else {
            return;
        };
        let graph: &relate_executor::Graph = target;
        graph.build_pending_key_stats(LABELS_PER_STATEMENT, || !token.is_cancelled());
        drop(pin);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use relate_names::SmallName;
    use relate_planner::PatternStats;

    use crate::{CancelToken, Connection, Database};

    fn connection() -> Connection {
        Database::open(":memory:").unwrap().connect().unwrap()
    }

    /// Whether a plan-time consult of `label.k` is measured; a consult of
    /// an unmeasured label records the build request, as the planner's does.
    fn measured(conn: &Connection, label: &str) -> bool {
        let _read = conn.inner.read_guard().unwrap();
        let target: &crate::engines::LiveQuery = &conn.inner.graph;
        let graph: &relate_executor::Graph = target;
        let params = BTreeMap::new();
        let stats = crate::stats::GraphPatternStats::new(graph, &params);
        let named = |name: &str| relate_names::unresolved(SmallName::from_name(name));
        stats.key_stats(&named(label), &named("k")).is_some()
    }

    fn load(conn: &mut Connection, labels: &[&str]) {
        for label in labels {
            conn.query(&format!(
                "UNWIND range(1, 30) AS i CREATE (:{label} {{k: i % 5}})"
            ))
            .unwrap();
        }
    }

    #[test]
    fn an_autocommit_read_serves_a_recorded_request() {
        let mut conn = connection();
        load(&mut conn, &["A"]);
        assert!(!measured(&conn, "A"), "the consult records the request");
        conn.query("MATCH (n:A) RETURN count(n)").unwrap();
        assert!(measured(&conn, "A"));
    }

    #[test]
    fn a_statement_inside_an_explicit_transaction_serves_it() {
        let mut conn = connection();
        load(&mut conn, &["A"]);
        assert!(!measured(&conn, "A"));
        conn.begin().unwrap();
        conn.query("MATCH (n:A) RETURN count(n)").unwrap();
        assert!(measured(&conn, "A"), "served before the commit");
        conn.commit().unwrap();
    }

    #[test]
    fn a_write_statement_serves_it() {
        let mut conn = connection();
        load(&mut conn, &["A"]);
        assert!(!measured(&conn, "A"));
        conn.query("CREATE (:Other)").unwrap();
        assert!(measured(&conn, "A"));
    }

    #[test]
    fn a_failed_statement_serves_nothing() {
        let mut conn = connection();
        load(&mut conn, &["A"]);
        assert!(!measured(&conn, "A"));
        assert!(conn.query("RETURN 1 / 0").is_err());
        assert!(!measured(&conn, "A"), "a failed statement does not pump");
        conn.query("RETURN 1").unwrap();
        assert!(measured(&conn, "A"), "the next successful one does");
    }

    #[test]
    fn a_statement_serves_one_label_and_the_next_serves_the_rest() {
        let mut conn = connection();
        load(&mut conn, &["A", "B"]);
        assert!(!measured(&conn, "A"));
        assert!(!measured(&conn, "B"));
        conn.query("RETURN 1").unwrap();
        let after_one = [measured(&conn, "A"), measured(&conn, "B")];
        assert_eq!(after_one.iter().filter(|m| **m).count(), 1, "one label");
        conn.query("RETURN 1").unwrap();
        assert!(measured(&conn, "A") && measured(&conn, "B"));
    }

    #[test]
    fn a_cancelled_statement_token_serves_nothing() {
        let mut conn = connection();
        load(&mut conn, &["A"]);
        assert!(!measured(&conn, "A"));
        let token = CancelToken::new();
        token.cancel();
        conn.serve_key_statistics(&token);
        assert!(!measured(&conn, "A"));
        conn.serve_key_statistics(&CancelToken::new());
        assert!(measured(&conn, "A"));
    }
}
