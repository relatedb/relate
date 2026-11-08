//! The staged statement entry point (ADR-0319): a surface that reports
//! planning failures and execution failures at different points (the Bolt
//! server's RUN and PULL) learns the phase from the engine, and a surface
//! that must keep its client connection alive during a long statement gets
//! a callback while it waits (ADR-0312, whose `Heartbeat` lives in
//! `query_worker.rs` beside the wait loop that arms it).

use std::collections::BTreeMap;

use super::Executed;
use crate::{CancelToken, Connection, Heartbeat, Result, Value};

impl Connection {
    /// Run a statement and report which phase a failure came from
    /// (ADR-0319). `Err` is a failure before execution: parse, semantic
    /// validation, planning and the compile-time gates. `Ok(Executed::Rows)`
    /// is the finished result; `Ok(Executed::Failed)` is a failure while
    /// executing the planned statement, with its planned columns.
    ///
    /// `token` cancels the statement from another thread (ADR-0111), as in
    /// [`Self::query_with_params_cancellable`]. `heartbeat`, when given,
    /// ticks on this thread while the statement runs on the connection's
    /// query worker (ADR-0312) — on a caller-stack-asserted connection
    /// (ADR-0504) a bounded read runs inline and has no wait, so its
    /// heartbeat is never due, while an escalating statement keeps the
    /// worker wait and ticks exactly as on an unasserted connection. In
    /// an explicit transaction an execution
    /// failure fails the transaction, as every query error does (ADR-0028).
    ///
    /// ```
    /// use std::collections::BTreeMap;
    /// use std::time::Duration;
    /// use relate::{CancelToken, Database, Executed, Heartbeat, Value};
    ///
    /// let db = Database::open(":memory:").unwrap();
    /// let mut conn = db.connect().unwrap();
    /// // A runtime error is staged with the planned columns.
    /// let staged = conn
    ///     .query_staged("UNWIND [1, 0] AS x RETURN 10 / x AS v", BTreeMap::new(), CancelToken::new(), None)
    ///     .unwrap();
    /// let Executed::Failed(failure) = staged else { panic!("10 / 0 fails while executing") };
    /// assert_eq!(failure.header.columns, ["v"]);
    /// assert_eq!(failure.header.column_types, ["ANY"]);
    /// // A syntax error fails before execution.
    /// assert!(conn.query_staged("RETURN", BTreeMap::new(), CancelToken::new(), None).is_err());
    /// // A heartbeat that is never due does not tick.
    /// let mut ticks = 0;
    /// let mut tick = || ticks += 1;
    /// let heartbeat = Heartbeat::every(Duration::from_secs(60), &mut tick).unwrap();
    /// let staged = conn
    ///     .query_staged("RETURN 1 AS n", BTreeMap::new(), CancelToken::new(), Some(heartbeat))
    ///     .unwrap();
    /// assert_eq!(staged.into_result().unwrap().rows, vec![vec![Value::Int(1)]]);
    /// assert_eq!(ticks, 0);
    /// ```
    pub fn query_staged(
        &mut self,
        cypher: &str,
        params: BTreeMap<String, Value>,
        token: CancelToken,
        heartbeat: Option<Heartbeat<'_>>,
    ) -> Result<Executed> {
        self.run_cancellable(cypher, params, &token, heartbeat)
    }

    /// [`Self::commit`], ticking `heartbeat` on this thread while the
    /// flush runs (ADR-0312) — including its return: the committed
    /// transaction's bookmark (ADR-0502 §7), `None` when the transaction
    /// committed nothing. One shape for every connection (ADR-0504): the
    /// commit prepares on the connection thread — the preconditions
    /// first (a failed transaction errors before anything else runs,
    /// ADR-0028), then the drained redo, the commit's own routing fact
    /// (`None`: the no-op; `Some`: the flush).
    /// An empty drain is the no-op, flushed inline with nothing waiting
    /// (a no-op cannot outlive any interval, so no wait shape is lost).
    /// A redo drain may be large: it flushes on the connection's worker —
    /// the running one, or one spawned for the flush — keeping the
    /// heartbeat's wait shape. A worker-spawn failure never drops the
    /// prepared work: the flush completes inline instead, the commit's
    /// data safety taking precedence over the one lost wait shape (the
    /// no-worker inline flush is the pre-fast-path behavior, and the
    /// flush is not the input-proportional recursion ADR-0259 provisions
    /// the worker against). The degraded path logs one line to stderr.
    ///
    /// ```
    /// use std::time::Duration;
    /// use relate::{Database, Heartbeat};
    ///
    /// let db = Database::open(":memory:").unwrap();
    /// let mut conn = db.connect().unwrap();
    /// conn.begin().unwrap();
    /// conn.query("CREATE (:N)").unwrap();
    /// let mut tick = || {};
    /// let heartbeat = Heartbeat::every(Duration::from_secs(60), &mut tick).unwrap();
    /// let bookmark = conn.commit_with_heartbeat(heartbeat).unwrap();
    /// assert!(bookmark.is_some());
    /// assert!(!conn.in_transaction());
    /// ```
    pub fn commit_with_heartbeat(&mut self, heartbeat: Heartbeat<'_>) -> Result<Option<String>> {
        // wasm32 has no worker: the commit runs inline on this thread, so
        // nothing waits and the heartbeat is never due.
        #[cfg(target_arch = "wasm32")]
        {
            drop(heartbeat);
            return self.commit();
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Prepare on the connection thread: the preconditions (a
            // failed transaction errors here, before any worker is
            // spawned for a doomed flush), then the drained work — the
            // variant is the routing decision, the commit's own, not a
            // second derivation of it.
            let Some(redo) = self.prepare_commit()? else {
                // The no-op commit: nothing to flush, nothing waits — and
                // no bookmark (ADR-0502 §7: none is fabricated for a
                // commit that crossed no WAL boundary).
                return Ok(None);
            };
            // A redo flush may be large: the worker keeps the wait shape.
            // A spawn failure must not drop the drained redo — the point
            // of no return is behind us — so the flush completes inline
            // instead (the doc above states the boundary).
            match self.worker_slot() {
                Ok(slot) => slot
                    .run_with_heartbeat(|| self.flush_commit(redo), Some(heartbeat))?
                    .map(Some),
                Err(err) => {
                    // Observable degradation: the inline flush is untimed
                    // and unticked, so leave a line to correlate with a
                    // client timing out during the commit.
                    eprintln!(
                        "relate: commit worker unavailable, flushing inline without heartbeat: {err}"
                    );
                    self.flush_commit(redo).map(Some)
                }
            }
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use crate::{CancelToken, Database, Executed, Heartbeat};

    /// ADR-0312: a statement that outlives the interval ticks on the
    /// waiting thread, once per elapsed interval.
    #[test]
    fn a_long_statement_ticks_the_heartbeat() {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        let mut ticks = 0u32;
        let mut tick = || ticks += 1;
        let heartbeat = Heartbeat::every(Duration::from_millis(5), &mut tick).unwrap();
        // ADR-0463 is per-allocation, not cumulative: every stage's range
        // and UNWIND buffer is well under the cap, so the full 30-stage
        // statement runs unchanged. This fixture is the regression test for
        // the false-positive class a cumulative meter produced.
        let stage = "UNWIND range(1, 100000) AS x WITH count(x) AS c ";
        let long_query = format!("{}RETURN c", stage.repeat(30));
        let staged = conn
            .query_staged(
                &long_query,
                BTreeMap::new(),
                CancelToken::new(),
                Some(heartbeat),
            )
            .unwrap();
        assert!(matches!(staged, Executed::Rows(_)));
        assert!(ticks > 0, "a statement longer than the interval must tick");
    }

    /// ADR-0319: an execution-phase failure inside an explicit transaction
    /// fails the transaction, like any query error (ADR-0028).
    #[test]
    fn a_staged_execution_failure_fails_the_transaction() {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.begin().unwrap();
        let staged = conn
            .query_staged(
                "RETURN 1 / 0 AS v",
                BTreeMap::new(),
                CancelToken::new(),
                None,
            )
            .unwrap();
        assert!(matches!(staged, Executed::Failed(_)));
        let err = conn.commit().unwrap_err();
        assert!(err.to_string().contains("rollback required"), "{err}");
    }

    /// ADR-0504: on a caller-stack-asserted connection the keep-alive
    /// contract keeps exactly its ADR-0312 shape — an escalating statement
    /// (unbounded row potential) still runs on the worker, so a statement
    /// that outlives the interval ticks; a bounded-work statement runs
    /// inline, where there is no wait and the heartbeat is never due.
    #[test]
    fn an_asserted_connection_ticks_for_escalating_statements_only() {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.assume_worker_stack_on_caller_thread();

        // Inline: a bounded-work statement under an aggressive interval
        // finishes before the first tick can be due — no wait exists.
        let mut ticks = 0u32;
        let mut tick = || ticks += 1;
        let heartbeat = Heartbeat::every(Duration::from_millis(1), &mut tick).unwrap();
        let staged = conn
            .query_staged(
                "RETURN 1 AS n",
                BTreeMap::new(),
                CancelToken::new(),
                Some(heartbeat),
            )
            .unwrap();
        assert!(matches!(staged, Executed::Rows(_)));
        assert_eq!(ticks, 0, "an inline statement has no wait to tick");

        // Escalated: the long statement still runs on the worker, so the
        // waiting connection thread ticks — ADR-0312 unchanged.
        let stage = "UNWIND range(1, 100000) AS x WITH count(x) AS c ";
        let long_query = format!("{}RETURN c", stage.repeat(30));
        let mut ticks = 0u32;
        let mut tick = || ticks += 1;
        let heartbeat = Heartbeat::every(Duration::from_millis(5), &mut tick).unwrap();
        let staged = conn
            .query_staged(
                &long_query,
                BTreeMap::new(),
                CancelToken::new(),
                Some(heartbeat),
            )
            .unwrap();
        assert!(matches!(staged, Executed::Rows(_)));
        assert!(
            ticks > 0,
            "an escalating statement must keep the wait shape"
        );
    }

    /// ADR-0504: a heartbeat commit routes on the prepared work. A redo
    /// flush keeps the worker wait shape — the write statements that made
    /// the redo escalated (every write does), so the worker exists and
    /// the commit reuses it; a read-only transaction's no-op commit on a
    /// fresh asserted connection stays inline, spawning nothing; and a
    /// failed transaction's commit errors in the preconditions — before
    /// any worker is spawned for a doomed flush.
    #[test]
    fn an_asserted_heartbeat_commit_routes_on_the_prepared_work() {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.assume_worker_stack_on_caller_thread();
        // The write that makes the redo escalated (the reads-only
        // boundary), so the worker exists before COMMIT.
        conn.begin().unwrap();
        conn.query("CREATE (:N)").unwrap();
        assert_eq!(
            conn.query_worker_spawns(),
            1,
            "the redo-making write escalated to the worker"
        );
        let mut tick = || {};
        let heartbeat = Heartbeat::every(Duration::from_secs(60), &mut tick).unwrap();
        conn.commit_with_heartbeat(heartbeat).unwrap();
        assert!(!conn.in_transaction());
        assert_eq!(
            conn.query_worker_spawns(),
            1,
            "the redo flush reuses the worker — no new spawn"
        );
        // A read-only transaction on a fresh asserted connection: the
        // no-op commit stays inline — no worker spawned for a commit
        // that waits for nothing.
        let mut conn = db.connect().unwrap();
        conn.assume_worker_stack_on_caller_thread();
        conn.begin().unwrap();
        conn.query("RETURN 1 AS n").unwrap();
        let mut tick = || {};
        let heartbeat = Heartbeat::every(Duration::from_secs(60), &mut tick).unwrap();
        conn.commit_with_heartbeat(heartbeat).unwrap();
        assert_eq!(
            conn.query_worker_spawns(),
            0,
            "a no-op heartbeat commit with no worker must stay inline"
        );
        // A failed transaction that still holds redo: the preconditions
        // error before the flush routing, so the commit spawns nothing
        // for a doomed flush (the worker count is whatever the statements
        // left it at).
        let mut conn = db.connect().unwrap();
        conn.assume_worker_stack_on_caller_thread();
        conn.begin().unwrap();
        conn.query("CREATE (:N)").unwrap();
        conn.query("RETURN 1 / 0 AS x").unwrap_err();
        assert!(conn.in_transaction());
        let spawns = conn.query_worker_spawns();
        let mut tick = || {};
        let heartbeat = Heartbeat::every(Duration::from_secs(60), &mut tick).unwrap();
        let err = conn.commit_with_heartbeat(heartbeat).unwrap_err();
        assert!(err.to_string().contains("rollback required"), "{err}");
        assert_eq!(
            conn.query_worker_spawns(),
            spawns,
            "a failed transaction's commit must error before any spawn"
        );
        conn.rollback().unwrap();
    }
}
