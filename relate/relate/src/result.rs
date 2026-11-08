//! The engine's public query-result type and its construction-time
//! metadata stamps: the resolved namespace (ADR-0117 §7), the plan-derived
//! query type (ADR-0162), the wall-clock elapsed (ADR-0221) with its
//! always-on PARSE / PLAN / EXECUTE phase split (LABS-832) and its fine
//! sub-split plus the plan-cache outcome (LABS-1120), and the per-column
//! type names (ADR-0224).

use std::time::Duration;

use relate_executor::{QueryResult as ExecResult, QueryStats, StatementEffect};
use relate_model::wire::Notification;
use relate_planner::QueryType;

use crate::phase_timing::{PhaseTimings, TimingStamp};
use crate::{ExecutionReceipt, Namespace, PlanCacheOutcome, Value};

/// Duration → wall-clock milliseconds (ADR-0221): the single conversion
/// behind [`QueryResult::elapsed_ms`] and the phase accessors
/// ([`QueryResult::parse_ms`], [`QueryResult::plan_ms`],
/// [`QueryResult::execute_ms`]), also used by engine-internal
/// renderers (PROFILE's `_profile_time` cell) that hold a bare `Duration`.
pub(crate) fn duration_ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// The result metadata every statement announces, finished or failed
/// (ADR-0319): one home for the vocabulary both [`QueryResult`] and
/// [`ExecutionFailure`] carry, so the two can never drift apart.
///
/// [`ExecutionFailure`]: crate::ExecutionFailure
#[derive(Debug)]
pub struct ResultHeader {
    /// The result's columns: the RETURN aliases / generated names.
    pub columns: Vec<String>,
    /// Per-column result types (ADR-0224): one of the 18 canonical type
    /// names (`STRING`, `INTEGER`, `FLOAT`, `BOOLEAN`, `NODE`,
    /// `RELATIONSHIP`, `PATH`, `LIST`, `MAP`, `POINT`, `DATETIME`, `DATE`,
    /// `TIME`, `DURATION`, `VECTOR`, `UUID`, `BINARY`, `ANY`) per column —
    /// same length and order as [`ResultHeader::columns`] — derived from the
    /// FIRST row. A column whose first-row value is Null, or a result with
    /// no rows, stamps `ANY`; the name describes the first row only.
    pub column_types: Vec<String>,
    /// The statement's query type (ADR-0162): the Neo4j Query API v2
    /// `queryType` classification — [`QueryType::Read`] (`"r"`),
    /// [`QueryType::Write`] (`"w"`), [`QueryType::ReadWrite`] (`"rw"`) or
    /// [`QueryType::Schema`] (`"s"`) — derived from the logical plan
    /// before execution. A write splits on its returned columns
    /// (ADR-0332): no returned columns → `"w"` (`MATCH … SET` without
    /// RETURN), returned columns → `"rw"` (`CREATE (n) RETURN n`). Plain
    /// runs, EXPLAIN and PROFILE alike carry it; EXPLAIN and PROFILE
    /// report the wrapped statement's type.
    pub query_type: QueryType,
    /// The namespace the statement ran in (ADR-0117 §7): the statement's
    /// resolved `USE` target, or the session namespace when it carries no
    /// `USE` (ADR-0116 §3).
    pub namespace: Namespace,
    /// The statement's wall-clock elapsed time (ADR-0221): parse, plan,
    /// gates and dispatch (execution or EXPLAIN/PROFILE rendering),
    /// measured once around `Connection::run_query` — never around row
    /// serialization or surface-side output. Stamped at construction;
    /// there is no placeholder value.
    pub elapsed: Duration,
    /// The statement's per-phase timings (LABS-832, ADR-0491): the
    /// always-on PARSE / PLAN / EXECUTE split of
    /// [`ResultHeader::elapsed`] — the three stages' disjoint
    /// sub-windows, so their sum never exceeds the total. Read publicly
    /// through [`QueryResult::parse_ms`], [`QueryResult::plan_ms`] and
    /// [`QueryResult::execute_ms`]; the field itself stays `pub(crate)`,
    /// so the accessors are the one public path (never a `pub` field
    /// beside them).
    ///
    /// Stamped at every header construction — there is no placeholder
    /// value, the same construction-time stamp as the total.
    pub(crate) phases: PhaseTimings,
    /// The statement's plan-cache outcome (LABS-1120): which ADR-0503
    /// stage served the plan — the fact that explains a hit's near-zero
    /// parse window and zero fine parse sub-windows. Stamped with the
    /// timing it explains; read publicly through
    /// [`QueryResult::plan_cache`].
    pub(crate) plan_cache: crate::PlanCacheOutcome,
}

impl ResultHeader {
    /// The one construction every result-metadata site builds: `columns` and
    /// the result's own `rows` go in, the ADR-0224 per-column stamp derives
    /// from the first row here, so `QueryResult::from_exec`,
    /// `explain_result` and `ExecutionFailure::staged` cannot hand-spell the
    /// derivation (and its `column_types` length) apart. `rows` is `&[]`
    /// when the statement has none — a staged failure — which stamps every
    /// column `ANY`. `timing` produces the statement's measured timing as
    /// one stamp — the ADR-0221 total, its always-on PARSE / PLAN /
    /// EXECUTE split with the fine sub-split, and the plan-cache outcome
    /// (LABS-832 + LABS-1120). It is taken *after* the per-column stamp
    /// below, because that derivation is this construction's work: a
    /// caller passing a window's elapsed evaluates it here, so the
    /// window covers the result build it documents instead of closing
    /// before it (LABS-1120).
    pub(crate) fn new(
        columns: Vec<String>,
        rows: &[Vec<Value>],
        query_type: QueryType,
        namespace: Namespace,
        timing: impl FnOnce() -> TimingStamp,
    ) -> Self {
        let column_types = stamp_column_types(columns.len(), rows);
        let timing = timing();
        Self {
            columns,
            column_types,
            query_type,
            namespace,
            elapsed: timing.elapsed,
            phases: timing.phases,
            plan_cache: timing.plan_cache,
        }
    }
}

#[derive(Debug)]
pub struct QueryResult {
    /// The statement's result metadata (ADR-0319).
    pub header: ResultHeader,
    pub rows: Vec<Vec<Value>>,
    pub stats: QueryStats,
    /// The engine's truthful write effect (ADR-0402 Decision 2): whether a
    /// mutation operator reached execution and whether the engine applied a
    /// change. An execution fact of the same class as [`Self::receipt`] — not
    /// announced result metadata ([`ResultHeader`]) and not carried by any
    /// wire envelope: a surface that wants to report it reads it here. The
    /// CLI's batch zero-effect write note is the one consumer.
    ///
    /// ```
    /// use relate::Database;
    ///
    /// let db = Database::open(":memory:")?;
    /// let mut conn = db.connect()?;
    ///
    /// // A read ran no mutation operator: nothing to report.
    /// assert!(!conn.query("MATCH (n:Missing) RETURN n")?.effect.changed_nothing());
    ///
    /// // A write whose row pipeline was empty applied nothing.
    /// let noop = conn.query("MATCH (n:Missing) DELETE n")?;
    /// assert!(noop.effect.attempted());
    /// assert!(!noop.effect.applied());
    /// assert!(noop.effect.changed_nothing());
    ///
    /// // An effective write, and a catalog change that leaves every
    /// // mutation counter at zero, both applied a change.
    /// assert!(conn.query("CREATE (:Kept)")?.effect.applied());
    /// assert!(conn.query("ALTER DATABASE neo4j SET DEFAULT LANGUAGE CYPHER 5")?.effect.applied());
    /// # Ok::<(), relate::Error>(())
    /// ```
    pub effect: StatementEffect,
    /// The statement's commit bookmark (ADR-0502 §7): the ADR-0310 string
    /// form `relate:<n>` of the commit timestamp the WAL boundary
    /// assigned this statement's auto-commit write. `n` is the store's
    /// commit sequence — monotonically increasing across every connection
    /// and surface — so a later commit carries a greater `n`. Compare the
    /// parsed `n`, never the strings: `relate:10` sorts before
    /// `relate:9` lexicographically. `None` on every statement that committed nothing:
    /// a read, a write whose row pipeline was empty, an `EXPLAIN` (nothing
    /// executes), and every statement inside an explicit transaction —
    /// those commit at [`crate::Connection::commit`], which returns the
    /// bookmark. An execution fact of the same class as [`Self::effect`],
    /// not announced result metadata ([`ResultHeader`]).
    ///
    /// ```
    /// use relate::Database;
    ///
    /// let db = Database::open(":memory:")?;
    /// let mut conn = db.connect()?;
    ///
    /// // Two sequential auto-commit writes: strictly increasing bookmarks.
    /// let first = conn.query("CREATE (:Kept)")?;
    /// let second = conn.query("CREATE (:Kept)")?;
    /// let first = first.bookmark.as_deref().expect("a write committed");
    /// let second = second.bookmark.as_deref().expect("a write committed");
    /// assert!(first.starts_with("relate:"));
    /// let seq = |b: &str| -> u64 { b["relate:".len()..].parse().expect("relate:<n>") };
    /// assert!(seq(second) > seq(first), "the commit sequence is monotonic");
    ///
    /// // A read-only statement committed nothing: no bookmark.
    /// assert!(conn.query("MATCH (n:Kept) RETURN n")?.bookmark.is_none());
    /// # Ok::<(), relate::Error>(())
    /// ```
    pub bookmark: Option<String>,
    /// The plan tree and the query notifications derived with it, as one
    /// artifact (ADR-0344/0345, ADR-0363): `Some` when the statement was
    /// `EXPLAIN` or `PROFILE`, `None` on every plain run — so a
    /// non-explained query never constructs a [`crate::PlanNode`] (ADR-0344) and
    /// a result cannot carry notifications without the tree they derive
    /// from. A `PROFILE` whose post-run estimates read is refused (the
    /// store was poisoned while the statement ran) also leaves it `None`:
    /// the executed rows survive that refusal un-enriched, never as an
    /// error. Boxed: the artifact is heavy and rarely populated, and
    /// surfaces buffer whole `QueryResult`s (the Bolt row cap) — `None`
    /// stays pointer-sized.
    ///
    /// ```
    /// use relate::{Database, NotificationKind};
    ///
    /// let db = Database::open(":memory:")?;
    /// let mut conn = db.connect()?;
    /// conn.query("CREATE (:Person)")?;
    /// conn.query("CREATE (:City)")?;
    ///
    /// let explained = conn.query("EXPLAIN MATCH (a:Person), (b:City) RETURN a, b")?;
    /// let [notification] = explained.notifications() else {
    ///     panic!("the disconnected MATCH raises exactly one notification");
    /// };
    /// assert_eq!(notification.kind(), NotificationKind::CartesianProduct);
    /// assert_eq!(notification.gql_status(), "03N90");
    /// assert_eq!(notification.severity().as_str(), "INFORMATION");
    /// assert_eq!(notification.category().as_str(), "PERFORMANCE");
    /// assert!(notification.operator_id().is_some());
    /// assert!(notification.remedy().contains("join anchor"));
    ///
    /// // A plain run builds no plan artifact: no tree, so no
    /// // notifications, never a re-derivation of the EXPLAIN list.
    /// let plain = conn.query("MATCH (a:Person), (b:City) RETURN a, b")?;
    /// assert!(plain.plan.is_none());
    /// assert!(plain.notifications().is_empty());
    /// # Ok::<(), relate::Error>(())
    /// ```
    pub plan: Option<Box<crate::PlanArtifacts>>,
    /// Peak logical workspace, in bytes, when admission is on. `None` when
    /// it is off, and on `EXPLAIN` (the peak rides
    /// [`crate::PlanNode::token_estimate`]).
    pub receipt: Option<ExecutionReceipt>,
    /// Keeps the result's peak-token reservation until the buffered rows are
    /// released by the consuming surface. Boxed so a retained ledger does not
    /// enlarge every `Executed::Rows` value.
    #[doc(hidden)]
    pub _retained_ledger: Option<Box<relate_core::tokens::TokenLedger>>,
}

impl QueryResult {
    /// Wrap an executor result, stamped with the namespace the statement
    /// ran in, the statement's plan-derived query type, and the
    /// statement's measured elapsed (ADR-0221). Construction is the
    /// stamp: a `QueryResult` always carries its resolved namespace
    /// (ADR-0117 §7), its query type (ADR-0162) and its plan-plus-run
    /// elapsed — there is no placeholder value. `timing` produces the
    /// elapsed with its always-on PARSE / PLAN / EXECUTE split
    /// (LABS-832) as one stamp; it is taken at the header's construction,
    /// after the per-column stamp, so a lazily-handed window covers the
    /// build.
    pub(crate) fn from_exec(
        r: ExecResult,
        namespace: Namespace,
        query_type: QueryType,
        timing: impl FnOnce() -> TimingStamp,
    ) -> Self {
        let header = ResultHeader::new(r.columns, &r.rows, query_type, namespace, timing);
        Self {
            header,
            rows: r.rows,
            stats: r.stats,
            effect: r.effect,
            bookmark: None,
            plan: None,
            receipt: None,
            _retained_ledger: None,
        }
    }

    /// A result with no execution behind it — `EXPLAIN`'s plan-only output:
    /// no rows, no stats, no receipt, and the zero effect, because no mutation
    /// operator ran (ADR-0402 Decision 2's plan-producing exclusion). The one
    /// construction for a plan-producing result, so the explain path spells
    /// neither an empty result nor a placeholder effect at a construction site;
    /// the plan artifact attaches with [`Self::with_plan`].
    pub(crate) fn not_executed(header: ResultHeader) -> Self {
        Self {
            header,
            rows: Vec::new(),
            stats: QueryStats::default(),
            effect: StatementEffect::None,
            bookmark: None,
            plan: None,
            receipt: None,
            _retained_ledger: None,
        }
    }

    /// Stamp the auto-commit write's commit sequence (ADR-0502 §7) as the
    /// result's bookmark — the one construction of a bookmark-carrying
    /// result, so the `relate:<n>` form (ADR-0310) is never
    /// hand-spelled at a call site. `None` — a read, a statement that
    /// committed nothing, an in-transaction statement — leaves the result
    /// bookmark-free.
    pub(crate) fn with_commit_sequence(mut self, sequence: Option<u64>) -> Self {
        self.bookmark = sequence.map(crate::commit::bookmark_string);
        self
    }

    /// Stamp the plan tree and the notifications derived with it
    /// (ADR-0344/0345, ADR-0363) as one artifact. The only construction of
    /// a plan-carrying result, so the tree and its list are always attached
    /// together — `EXPLAIN` and `PROFILE` both call it, a plain run never
    /// does.
    pub(crate) fn with_plan(
        mut self,
        root: crate::PlanNode,
        notifications: Vec<Notification>,
    ) -> Self {
        self.plan = Some(Box::new(crate::PlanArtifacts {
            root,
            notifications,
        }));
        self
    }

    /// The structured plan tree, `None` on a plain run — the read half of
    /// [`QueryResult::plan`] for every surface that renders a tree.
    pub fn plan_root(&self) -> Option<&crate::PlanNode> {
        self.plan.as_deref().map(|artifacts| &artifacts.root)
    }

    /// The query notifications derived with the plan tree (ADR-0363): the
    /// operator-scoped entries in the tree's pre-order — each carrying its
    /// operator's [`crate::PlanNode::id`] — then the statement-scoped entries by
    /// category, code and description. Identical `(kind, description)`
    /// pairs collapse to the first. Empty on a plain run, because no tree
    /// is built there (ADR-0363 decision 3); each entry's code, GQLSTATUS,
    /// title, severity, category and canonical remedy come from its
    /// [`crate::NotificationKind`].
    pub fn notifications(&self) -> &[Notification] {
        self.plan
            .as_deref()
            .map_or(&[], |artifacts| &artifacts.notifications)
    }

    /// The engine-reported statement elapsed in wall-clock milliseconds
    /// (ADR-0221) — the one Duration→ms conversion every surface uses, so
    /// the unit convention lives here and nowhere else.
    pub fn elapsed_ms(&self) -> f64 {
        duration_ms(self.header.elapsed)
    }

    /// The statement's PARSE stage in wall-clock milliseconds (ADR-0221
    /// total, split per ADR-0491): the front-end parse and lowering of
    /// the statement text, measured always-on — no PROFILE opt-in, no
    /// feature flag. One of the three stages that partition the window
    /// [`QueryResult::elapsed_ms`] spans, so
    /// `parse_ms() + plan_ms() + execute_ms() <= elapsed_ms()`; the
    /// inter-stage gaps (the post-EXPLAIN gates, token reservation) stay
    /// in the total alone.
    pub fn parse_ms(&self) -> f64 {
        duration_ms(self.header.phases.parse())
    }

    /// The statement's PLAN stage in wall-clock milliseconds (ADR-0221
    /// total, split per ADR-0491): binding, logical planning and the
    /// admission classification, measured always-on. One of the three
    /// stages that partition the window [`QueryResult::elapsed_ms`]
    /// spans, so `parse_ms() + plan_ms() + execute_ms() <= elapsed_ms()`.
    pub fn plan_ms(&self) -> f64 {
        duration_ms(self.header.phases.plan())
    }

    /// The statement's EXECUTE stage in wall-clock milliseconds (ADR-0221
    /// total, split per ADR-0491): the dispatch — a transaction's overlay
    /// run, an auto-commit write, or a snapshot read. For `EXPLAIN` this
    /// is the plan render: the wrapped statement never executes, so the
    /// render is that path's execute-equivalent stage. One of the three
    /// stages that partition the window [`QueryResult::elapsed_ms`]
    /// spans, so `parse_ms() + plan_ms() + execute_ms() <= elapsed_ms()`.
    pub fn execute_ms(&self) -> f64 {
        duration_ms(self.header.phases.execute())
    }

    /// The PARSE stage's lexing-plus-AST-build sub-window in wall-clock
    /// milliseconds (LABS-1120, the fine split of [`QueryResult::parse_ms`]):
    /// tokenize, the AST build and the preview-gate walk — the lexer and
    /// the parser are fused at the parser's one tokenizing constructor, so
    /// this is one window, never two. Zero on a plan-cache hit: nothing was
    /// parsed, and [`QueryResult::plan_cache`] says why.
    pub fn lex_parse_ms(&self) -> f64 {
        duration_ms(self.header.phases.parse_stages().lex_parse)
    }

    /// The PARSE stage's rewrite sub-window in wall-clock milliseconds
    /// (LABS-1120): the CIP-231 `DEFINE`-block resolver pass with its
    /// nesting-depth guard. Zero on a plan-cache hit.
    pub fn rewrite_ms(&self) -> f64 {
        duration_ms(self.header.phases.parse_stages().rewrite)
    }

    /// The PARSE stage's validation-and-lowering sub-window in wall-clock
    /// milliseconds (LABS-1120): the lowering battery — validation fused
    /// into the lowering walk, so this is one window. Zero on a
    /// plan-cache hit.
    pub fn lower_ms(&self) -> f64 {
        duration_ms(self.header.phases.parse_stages().lower)
    }

    /// The EXECUTE stage's result-generation sub-window in wall-clock
    /// milliseconds (LABS-1120, carved out of [`QueryResult::execute_ms`]):
    /// the buffered executor result's conversion into this result — the
    /// column-type stamp, the header, a staged failure's derived columns.
    /// Zero on the `EXPLAIN` path, whose execute-equivalent stage (the
    /// render) is itself its result generation.
    pub fn result_generation_ms(&self) -> f64 {
        duration_ms(self.header.phases.result_generation())
    }

    /// The statement's plan-cache outcome (LABS-1120): which ADR-0503
    /// stage served the plan — `HitTemplate` / `HitRawFastPath` /
    /// `ReplanGenerationMismatch` / `Miss` / `Bypassed`,
    /// rendered on every machine-readable surface through
    /// [`PlanCacheOutcome::as_str`]. The fact that explains a hit's
    /// near-zero [`QueryResult::parse_ms`] and zero fine parse
    /// sub-windows.
    pub fn plan_cache(&self) -> PlanCacheOutcome {
        self.header.plan_cache
    }
}

/// The ADR-0224 per-column stamp behind [`ResultHeader::new`] — the one
/// derivation, never re-spelled at a construction site: `column_count` names
/// derived from the first row, `ANY` for a Null cell or a result with no
/// rows. The name describes the first row only — Cypher columns are
/// heterogeneous. The classification is the single-sourced ADR-0224 kind set
/// (`relate_core::column_type_name`); the `&'static str` names are
/// copied exactly once at the `Vec<String>` boundary below.
///
/// Engine rows are rectangular by contract — one value per column, in
/// column order (the CLI's exec path asserts the same) — so a first row
/// short of `column_count` is a contract break and fails loudly here,
/// not a column stamped `ANY`.
fn stamp_column_types(column_count: usize, rows: &[Vec<Value>]) -> Vec<String> {
    let first = rows.first();
    (0..column_count)
        .map(|i| {
            let cell = match first {
                Some(row) => row
                    .get(i)
                    .expect("engine rows are rectangular: one value per column"),
                None => &Value::Null,
            };
            relate_core::column_type_name(cell)
        })
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use std::cell::Cell;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    use relate_planner::QueryType;

    use super::{duration_ms, QueryResult, ResultHeader};
    use crate::{Database, Namespace, Value};

    /// The result-generation window covers the result build instead of
    /// closing ahead of it (LABS-1120): the header's timing stamp is taken
    /// *after* the per-column stamp, so a build that fails (a row short of
    /// its column count is `stamp_column_types`' documented contract break)
    /// fails before the window closes. The closure panics instead of
    /// producing a stamp, so a window taken ahead of the build trips the
    /// `taken` flag and fails this test.
    #[test]
    fn the_timing_stamp_is_taken_after_the_column_stamp() {
        let taken = Cell::new(false);
        let ragged = vec![vec![Value::Int(1)]];
        let build = catch_unwind(AssertUnwindSafe(|| {
            ResultHeader::new(
                vec!["a".to_owned(), "b".to_owned()],
                &ragged,
                QueryType::Read,
                Namespace::Neo4j,
                || {
                    taken.set(true);
                    panic!("the timing stamp ran before the column stamp")
                },
            )
        }));
        assert!(
            build.is_err(),
            "a row short of its column count is the column stamp's contract break",
        );
        assert!(
            !taken.get(),
            "the result-generation window must close after the build it documents",
        );
    }

    /// A first-row `Value::Uuid` stamps `STRING`, not the reserved `UUID`
    /// name (ADR-0224: the wire taxonomy cannot distinguish a UUID from a
    /// string — `value_to_cypher` crosses it as `Str`). Pinned end to end
    /// through a uuid-producing query: the engine stamp and the wire
    /// envelope agree.
    #[test]
    fn uuid_first_row_stamps_string_and_matches_the_wire_envelope() {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        let result = conn.query("RETURN uuid() AS id").unwrap();
        assert_eq!(result.header.columns, ["id"]);
        assert_eq!(result.header.column_types, ["STRING"]);

        let envelope = crate::query_result_to_wire(&result);
        assert_eq!(envelope["columnTypes"], serde_json::json!(["STRING"]));
    }

    /// Each phase accessor re-sources its header phase through the one
    /// Duration→ms conversion (the `elapsed_ms` policy, ADR-0491): the
    /// value is the header's own `Duration` for that stage converted by
    /// `duration_ms` — bit-exact, like the wire envelope's `elapsed_ms`
    /// re-sourcing check — never a second conversion site or a different
    /// stage's duration.
    #[test]
    fn phase_accessors_re_source_the_header_phases_in_milliseconds() {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.query("CREATE (:Person {name: 'a'})").unwrap();
        let result = conn
            .query("MATCH (n:Person) WHERE n.name = 'a' RETURN n.name AS name")
            .unwrap();
        assert_eq!(
            result.parse_ms().to_bits(),
            duration_ms(result.header.phases.parse()).to_bits()
        );
        assert_eq!(
            result.plan_ms().to_bits(),
            duration_ms(result.header.phases.plan()).to_bits()
        );
        assert_eq!(
            result.execute_ms().to_bits(),
            duration_ms(result.header.phases.execute()).to_bits()
        );
    }

    /// Invariant through the public accessors: the three phases partition
    /// the measured window [`QueryResult::elapsed_ms`] spans (ADR-0491),
    /// so their sum never exceeds the total. Pinned over sample
    /// statements covering every phase-carrying path: a plain read, a
    /// write, an aggregate, an `EXPLAIN` (render as the EXECUTE stage)
    /// and a `PROFILE` (normal dispatch plus the epilogue).
    #[test]
    fn phase_accessors_sum_never_exceeds_elapsed_ms_for_sample_statements() {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.query("CREATE (:Person {name: 'a'})").unwrap();
        for statement in crate::phase_timing::SAMPLE_STATEMENTS {
            let result = conn.query(statement).unwrap();
            assert_own_window(statement, &result);
        }
    }

    /// LABS-1120: the fine sub-split partitions its coarse phase through
    /// the public accessors — `lex_parse + rewrite + lower <= parse` and
    /// `result_generation <= execute` — over the same sample corpus, so a
    /// fine window that escapes its phase fails here, not in a surface's
    /// rendering.
    #[test]
    fn fine_split_accessors_partition_their_coarse_phases() {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.query("CREATE (:Person {name: 'a'})").unwrap();
        for statement in crate::phase_timing::SAMPLE_STATEMENTS {
            let result = conn.query(statement).unwrap();
            let fine = result.lex_parse_ms() + result.rewrite_ms() + result.lower_ms();
            assert!(
                fine <= result.parse_ms(),
                "{statement}: fine parse {fine} exceeds parse {}",
                result.parse_ms()
            );
            assert!(
                result.result_generation_ms() <= result.execute_ms(),
                "{statement}: result generation {} exceeds execute {}",
                result.result_generation_ms(),
                result.execute_ms()
            );
        }
    }

    /// LABS-1120: the plan-cache outcome distinguishes the cache's real
    /// arms — a value that would read the same whether or not the cache
    /// served the statement is worthless here. The sequence walks every
    /// reachable outcome in one session: the first run of a template
    /// misses; the same text again hits the raw fast path (stage 1); a
    /// different literal on the same template hits the template key
    /// (stage 2); a write bumps the stats generation, so the next read
    /// re-plans from the carried AST; and the plan-tree path (EXPLAIN)
    /// bypasses the cache entirely.
    #[test]
    fn plan_cache_outcome_distinguishes_miss_raw_template_replan_and_bypass() {
        use crate::PlanCacheOutcome;
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.query("CREATE (:Person {name: 'a', age: 30})").unwrap();
        let read = |conn: &mut crate::Connection, name: &str| {
            let statement = format!("MATCH (n:Person {{name: '{name}'}}) RETURN n.age");
            let result = conn.query(&statement).unwrap();
            (result.plan_cache(), result.parse_ms())
        };
        // No entry yet: the first run of the template compiles and inserts.
        let (first, first_parse) = read(&mut conn, "a");
        assert_eq!(first, PlanCacheOutcome::Miss);
        // The same text again: stage 1, the raw fast path — no normalizer.
        let (again, _) = read(&mut conn, "a");
        assert_eq!(again, PlanCacheOutcome::HitRawFastPath);
        // A different literal, same template: stage 2 — and the parse
        // window collapses to the lookup (the fine parse sub-windows are
        // zero), which is exactly what the outcome explains.
        let (templated, templated_parse) = read(&mut conn, "zzz-not-there");
        assert_eq!(templated, PlanCacheOutcome::HitTemplate);
        assert_eq!(
            conn.query("MATCH (n:Person {name: 'a'}) RETURN n.age")
                .unwrap()
                .lex_parse_ms(),
            0.0,
            "a hit parses nothing: the fine parse sub-windows are zero"
        );
        assert!(
            templated_parse < first_parse,
            "a hit's parse window ({templated_parse}) must be smaller than the first \
             compile's ({first_parse}) — otherwise the cache never served it"
        );
        // A write bumps the stats generation: the next hit re-plans from
        // the entry's carried AST under the fresh generation.
        conn.query("CREATE (:Person {name: 'b', age: 1})").unwrap();
        let (replanned, _) = read(&mut conn, "a");
        assert_eq!(replanned, PlanCacheOutcome::ReplanGenerationMismatch);
        // A caller parameter map carrying the reserved `__AUTO_*__` prefix
        // disables the cache for the statement entirely — no stage, not
        // even the raw lookup — while the statement itself runs normally.
        let mut reserved = std::collections::BTreeMap::new();
        reserved.insert("name".to_string(), crate::Value::String("a".into()));
        reserved.insert("__AUTO_INT_0__".to_string(), crate::Value::Int(1));
        let bypassed = conn
            .query_with_params("MATCH (n:Person {name: $name}) RETURN n.age", reserved)
            .unwrap();
        assert_eq!(bypassed.plan_cache(), PlanCacheOutcome::Bypassed);
    }

    /// Multi-statement semantics: phases are per statement, exactly like
    /// `elapsed` — a session (and an explicit transaction) running several
    /// statements reports each statement's own split, never an accumulated
    /// one. That is the verified tree-wide behavior of `elapsed` itself:
    /// every multi-statement site (`run_script_wire`, the transaction
    /// statements, every surface's session) hands out per-statement
    /// results, and the one `QueryStats::merge` call is the per-batch
    /// merge inside a single ADR-0209 batched bulk write — a statement
    /// whose phases are stamped once at the statement level. So the
    /// per-statement pin here IS the "match elapsed exactly" contract; a
    /// merged-result surface that ever needs accumulation re-adds the
    /// `PhaseTimings` merge arm in its own change, with its production
    /// caller.
    #[test]
    fn phases_are_per_statement_across_a_session_like_elapsed() {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();

        // A plain session: two statements, each its own window.
        let first = conn.query("CREATE (:Person {name: 'a'})").unwrap();
        let second = conn
            .query("MATCH (n:Person) RETURN count(n) AS people")
            .unwrap();
        assert_own_window("first", &first);
        assert_own_window("second", &second);

        // An explicit multi-statement transaction (ADR-0028): same shape —
        // every in-transaction result carries its own statement's split.
        conn.begin().unwrap();
        let txn_first = conn.query("CREATE (:Person {name: 'b'})").unwrap();
        let txn_second = conn
            .query("MATCH (n:Person) RETURN count(n) AS people")
            .unwrap();
        conn.commit().unwrap();
        assert_own_window("txn_first", &txn_first);
        assert_own_window("txn_second", &txn_second);
    }

    /// One statement's own timing window, the module's one spelling of the
    /// partition predicate for every per-statement result it pins: the
    /// phase split partitions that statement's `elapsed` (the sum never
    /// exceeds it), and the elapsed is one statement's plan+run — bounded
    /// well under a session-length total, so an accumulated split could
    /// never pass as per-statement. The 60 s bound is what pins
    /// non-accumulation at the session and transaction call sites; for
    /// the single quick sample statements it holds vacuously. Session
    /// statements and in-transaction
    /// statements get the same pair of asserts.
    fn assert_own_window(label: &str, result: &QueryResult) {
        let sum = result.parse_ms() + result.plan_ms() + result.execute_ms();
        assert!(
            sum <= result.elapsed_ms(),
            "{label}: parse {} + plan {} + execute {} = {sum} exceeds elapsed {}",
            result.parse_ms(),
            result.plan_ms(),
            result.execute_ms(),
            result.elapsed_ms()
        );
        assert!(
            result.elapsed_ms() < duration_ms(Duration::from_secs(60)),
            "{label}: elapsed is one statement's plan+run, not a session total"
        );
    }
}
