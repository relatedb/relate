//! Connection-scoped query path and explicit transactions (ADR-0016,
//! ADR-0028, ADR-0259). Extracted from the crate root to keep `lib.rs` under
//! the repository's file-size budget; the query-worker cluster lives in
//! [`worker`] and the transaction cluster in [`txn`], both under the budget.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{ce_messages, Namespace};
use relate_engine_api::{RecordType, TxChannelState};
use relate_executor::ExecGraph;

use crate::query_worker::Heartbeat;
use crate::{CancelToken, PullKind, QueryResult, Result, Shared, Value};

mod admission;
mod cache;
mod columns;
mod outcome;
mod render;
mod staged;
mod txn;
mod worker;

use cache::CacheUse;
pub use outcome::{Executed, ExecutionFailure};
use txn::OpenTxn;

/// Test hook for the processing-memory load test (ADR-0333). Unset in
/// production, so admission does not sleep. A positive value holds the
/// active-query slot before the statement runs, long enough for a second
/// session to overlap it.
fn admit_hold() {
    crate::token_budget::query_admit_hold();
}

/// Test hook for the processing-memory load test. Unset in production.
/// After a successful statement the process logs `logical-bytes <n>`, the
/// live graph meter ([`crate::Database::memory_usage`]).
fn log_logical_bytes_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("RELATE_LOG_LOGICAL_BYTES").ok().as_deref() == Some("1"))
}

fn log_logical_bytes(shared: &Shared) {
    if !log_logical_bytes_enabled() {
        return;
    }
    eprintln!("logical-bytes {}", shared.total_used_bytes());
}

/// A connection sharing the database graph.
///
/// Reads pin `commit_ts` and capture schema under a short publication guard (ADR-0094). Writes execute on a
/// private overlay (ADR-0016); `commit_ts` is assigned at WAL append (ADR-0015).
/// Independent waiters share one `sync_all` (ADR-0019). Explicit transactions
/// (ADR-0028) pin one snapshot and hold the overlay until `commit` / `rollback`.
///
/// Each connection has a session namespace (ADR-0117 §2): the namespace its
/// statements run in when they carry no `USE` clause (ADR-0116 §3).
pub struct Connection {
    pub(crate) inner: Arc<Shared>,
    pub(crate) txn: Option<OpenTxn>,
    /// The connection's per-transaction metadata cell (ADR-0610): the one
    /// state every metadata channel writes — the three untyped channels
    /// (`CALL tx.setMetaData`, a Bolt BEGIN's `tx_metadata` extra, and
    /// [`Self::set_tx_metadata`]), which hard-set the provenance record
    /// family as they merge, and the typed [`Self::set_tx_metadata_of_type`],
    /// which names another family. A `RefCell` because the executor's
    /// statement views hold it as shared per-admission state and the
    /// procedure arm merges through it mid-statement (the connection is
    /// `!Sync`, so the cell is single-threaded). Taken whole — map and
    /// record family together — into the commit's record at the next WAL
    /// boundary; a rollback discards it. Empty — and never written — on a
    /// store that never opted in.
    pub(crate) tx_metadata: RefCell<TxChannelState>,
    /// ADR-0273: the authenticated principal a surface verified for this
    /// connection (the Bolt server sets it after a validated HELLO).
    /// Display-only: read by `dbms.showCurrentUser`; it confers no
    /// authorization. `None` on every surface with no notion of identity.
    pub(crate) principal: Option<String>,
    pub(crate) namespace: Namespace,
    /// ADR-0259: one 64 MiB worker for every query on this connection.
    /// `None` until the first query. Dropped (and joined) with the connection.
    #[cfg(not(target_arch = "wasm32"))]
    query_worker: Option<crate::query_worker::QueryWorker>,
    /// ADR-0504: the caller asserted this connection's calling thread
    /// carries the query worker's own stack budget
    /// ([`Self::assume_worker_stack_on_caller_thread`]), so statements take
    /// the connection-thread fast path: bounded-work plans run inline on
    /// the calling thread, and everything else escalates to the worker.
    /// wasm32 has no worker to bypass — every statement already runs on
    /// the caller's thread there — so the field does not exist there and
    /// the assertion method records nothing.
    #[cfg(not(target_arch = "wasm32"))]
    caller_stack_asserted: bool,
    /// How many times this connection has spawned its worker. One across
    /// every later statement, including after a caught worker panic.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    worker_spawns: u32,
    /// How many statements the cache-hit inline route (ADR-0663) served
    /// on the calling thread — the route's own observable, so tests pin
    /// which statements it takes and which it leaves the worker.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    hit_inline_statements: u32,
}

mod dispatch;

use outcome::QueryOutcome;

/// A connection with no open transaction and no query worker yet.
pub(crate) fn fresh(inner: Arc<Shared>, namespace: Namespace) -> Connection {
    Connection {
        inner,
        txn: None,
        tx_metadata: RefCell::new(TxChannelState::default()),
        principal: None,
        namespace,
        #[cfg(not(target_arch = "wasm32"))]
        query_worker: None,
        #[cfg(not(target_arch = "wasm32"))]
        caller_stack_asserted: false,
        #[cfg(all(test, not(target_arch = "wasm32")))]
        worker_spawns: 0,
        #[cfg(all(test, not(target_arch = "wasm32")))]
        hit_inline_statements: 0,
    }
}

/// Parse a session namespace name (ADR-0117 §2). Names match
/// case-insensitively; an unknown name fails with `DatabaseNotFound` and
/// CE's live text `Graph not found: <name>` (ADR-0146) — the one path both
/// `Database::connect_namespace` and `Connection::set_namespace` use.
pub(crate) fn parse_session_namespace(name: &str) -> Result<Namespace> {
    Namespace::parse(name).ok_or_else(|| ce_messages::database_does_not_exist(name))
}

/// One interpreted `@txMetadata` annotation (ADR-0610): the metadata map
/// with the optional `type` key consumed, beside the record family it
/// named ([`RecordType::Provenance`] — the home family — when the key is
/// absent or spells the home name).
struct InterpretedAnnotation {
    record_type: RecordType,
    map: BTreeMap<String, Value>,
}

/// The `type` key's spelling (ADR-0610): consumed from the spelled map
/// into the record type, never merged into the metadata itself.
const TX_METADATA_TYPE_KEY: &str = "type";

/// Interpret one captured `@txMetadata` annotation (ADR-0610): spell its
/// parameters as one map — a single positional map literal, or
/// all-named parameters — then consume the `type` key into the record
/// family, its serialized spelling decoded by the one rule
/// ([`RecordType::of`]: the home name is the home family). The
/// rejections are the channel's value-domain errors
/// (`InvalidArgumentValue`): a positional parameter that is not a map
/// literal, and a `type` value that is not a string; the type *name*'s
/// kebab-case policy is the canonical `validate_record_type`, applied
/// where the channel applies the record family
/// ([`Connection::apply_tx_metadata_annotations`]).
fn interpret_tx_metadata_annotation(
    capture: &relate_parser::TxMetadataAnnotation,
) -> Result<InterpretedAnnotation> {
    let pairs: Vec<(String, &relate_parser::Expr)> = match capture {
        relate_parser::TxMetadataAnnotation::Positional(expr) => match expr {
            relate_parser::Expr::Map(pairs) => pairs
                .iter()
                .map(|pair| (pair.key.to_string(), &pair.value))
                .collect(),
            _ => {
                return Err(crate::Error::invalid_argument_value(
                    "@txMetadata's positional parameter must be a map literal \
                     (or spell the map as all-named parameters)",
                ));
            }
        },
        relate_parser::TxMetadataAnnotation::Named(pairs) => pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect(),
    };
    let mut map = BTreeMap::new();
    for (key, value) in pairs {
        map.insert(key, annotation_literal_value(value)?);
    }
    // The optional type key names the record family; it is consumed from
    // the map, never merged into the metadata.
    let record_type = match map.remove(TX_METADATA_TYPE_KEY) {
        Some(Value::String(name)) => RecordType::of(&name),
        Some(other) => {
            return Err(crate::Error::invalid_argument_value(format!(
                "@txMetadata's type key must be a string, not {}",
                relate_core::value_type_name(&other)
            )));
        }
        None => RecordType::Provenance,
    };
    Ok(InterpretedAnnotation { record_type, map })
}

/// The CIP-281 annotation-parameter literal vocabulary (ADR-0610's
/// channel rides it): a boolean, integer, float or string literal, a
/// signed integer or float, a list of those, or a map of those — the
/// closed set the parser's annotation checks already guarantee, so the
/// unreachable arm is a stated invariant, not a fabricated default. A
/// conversion of its own rather than the front's
/// `static_value` (constant folding's policy: query parameters are
/// values, map literals are not) — different vocabulary, one purpose.
fn annotation_literal_value(expr: &relate_parser::Expr) -> Result<Value> {
    use relate_parser::{Expr, LiteralValue};
    match expr {
        Expr::Literal(LiteralValue::Bool(value)) => Ok(Value::Bool(*value)),
        Expr::Literal(LiteralValue::Integer(value)) => Ok(Value::Int(*value)),
        Expr::Literal(LiteralValue::Float(value)) => Ok(Value::Float(*value)),
        Expr::Literal(LiteralValue::String(value)) => Ok(Value::String(value.as_str().into())),
        // A sign over an integer or float literal: the parser's
        // annotation checks admit exactly that unary shape, so the
        // fall-through arm is the stated escape invariant, not a
        // fabricated default. The negation guard covers the one literal
        // the parser's own i64 parse cannot build, so it is the same
        // invariant in guarded form.
        Expr::Unary(unary) => {
            let literal = match &unary.expr {
                Expr::Literal(LiteralValue::Integer(value)) => Value::Int(*value),
                Expr::Literal(LiteralValue::Float(value)) => Value::Float(*value),
                _ => return escaped_annotation_literal(),
            };
            match unary.op {
                "-" => match literal {
                    Value::Int(value) => value.checked_neg().map(Value::Int).ok_or_else(|| {
                        crate::Error::internal(
                            "a captured @txMetadata parameter escaped the annotation literal checks",
                        )
                    }),
                    Value::Float(value) => Ok(Value::Float(-value)),
                    _ => escaped_annotation_literal(),
                },
                "+" => Ok(literal),
                _ => escaped_annotation_literal(),
            }
        }
        Expr::List(items) => {
            let values = items
                .iter()
                .map(annotation_literal_value)
                .collect::<Result<Vec<_>>>()?;
            Ok(Value::List(values.into()))
        }
        Expr::Map(pairs) => {
            let mut map = BTreeMap::new();
            for pair in pairs {
                map.insert(pair.key.to_string(), annotation_literal_value(&pair.value)?);
            }
            Ok(Value::Map(map.into()))
        }
        // Same invariant as the unary arm: the parser's annotation checks
        // admit exactly the literal vocabulary above, so a capture
        // carrying anything else escaped them — loud, never a default.
        _ => escaped_annotation_literal(),
    }
}

/// The annotation-literal escape invariant (ADR-0610): the parser's
/// CIP-281 checks guarantee every captured `@txMetadata` parameter is one
/// of the literal forms [`annotation_literal_value`] matches, so any other
/// shape reaching the conversion means a capture escaped those checks —
/// a loud internal error, never a fabricated default.
fn escaped_annotation_literal() -> Result<Value> {
    Err(crate::Error::internal(
        "a captured @txMetadata parameter escaped the annotation literal checks",
    ))
}

impl Connection {
    /// Set (or clear, with `None`) the authenticated principal for this
    /// connection (ADR-0273).
    ///
    /// The principal is **display-only**: it is reported as the `username`
    /// column of `dbms.showCurrentUser()` and confers no authorization —
    /// Relate has no engine-level access control. A surface should set
    /// it only with an identity it has itself verified (the Bolt server
    /// sets it after a validated HELLO); surfaces with no notion of
    /// identity never call it, and `dbms.showCurrentUser()` then reports
    /// the documented placeholder `'relate'`.
    ///
    /// ```
    /// use relate::{Database, Value};
    ///
    /// let db = Database::open(":memory:").unwrap();
    /// let mut conn = db.connect().unwrap();
    /// // No principal: the documented placeholder.
    /// let result = conn.query("CALL dbms.showCurrentUser()").unwrap();
    /// assert_eq!(result.rows[0][0], Value::String("relate".into()));
    /// // A verified principal is reported as `username`.
    /// conn.set_principal(Some("alice"));
    /// assert_eq!(conn.principal(), Some("alice"));
    /// let result = conn.query("CALL dbms.showCurrentUser()").unwrap();
    /// assert_eq!(result.rows[0][0], Value::String("alice".into()));
    /// // Clearing restores the placeholder.
    /// conn.set_principal(None);
    /// let result = conn.query("CALL dbms.showCurrentUser()").unwrap();
    /// assert_eq!(result.rows[0][0], Value::String("relate".into()));
    /// ```
    pub fn set_principal(&mut self, principal: Option<&str>) {
        self.principal = principal.map(str::to_string);
    }

    /// Seed this connection's per-transaction metadata map (ADR-0610) —
    /// the embedded Rust channel of the metadata inputs, the others
    /// being `CALL tx.setMetaData` and a Bolt `BEGIN`'s `tx_metadata`
    /// extra. The map merges with every earlier seed (later keys win) and
    /// is serialized into the commit's WAL record at the next transaction
    /// boundary — an auto-commit write's, or an explicit transaction's
    /// `commit` — then cleared; a `rollback` discards it. The merged map
    /// is bounded by the per-object envelope (16 MiB): an over-bound seed
    /// is `Error::Storage` and leaves the current map unchanged. The
    /// channel hard-sets the **provenance** record type: the transaction's
    /// record belongs to the provenance family, and any earlier typed
    /// seed's family ([`Self::set_tx_metadata_of_type`]) returns to it —
    /// the last channel call owns the transaction's type.
    ///
    /// The whole machinery is the `value-provenance` preview feature's
    /// (ADR-0371/ADR-0610): on a store opened without the opt-in the call
    /// fails with the authored preview error instead of silently dropping
    /// the map — the embedded caller's contract, where the quiet-ignore of
    /// the transport channel would hide a mistake. The variable is read
    /// once, at open.
    ///
    /// ```
    /// # use relate::{Database, Value};
    /// # relate::test_opts::assert_preview_unset();
    /// use std::collections::BTreeMap;
    ///
    /// let db = Database::open(":memory:").unwrap();
    /// let mut conn = db.connect().unwrap();
    /// let mut metadata = BTreeMap::new();
    /// metadata.insert("by".to_string(), Value::String("app".into()));
    /// // Without the opt-in the seed is the authored preview error.
    /// let error = conn.set_tx_metadata(metadata).expect_err("gated");
    /// assert_eq!(
    ///     error.to_string(),
    ///     "semantic error: SemanticError: preview feature 'value-provenance' is not enabled: \
    ///      set RELATE_PREVIEW=value-provenance"
    /// );
    /// # Ok::<(), relate::Error>(())
    /// ```
    pub fn set_tx_metadata(&mut self, metadata: BTreeMap<String, Value>) -> Result<()> {
        // The `GraphCell` deref contract: the preview read is under
        // `read_guard`, so a Follow-mode reload's `replace` cannot swap
        // the graph out from under the deref. The guard is scoped to
        // the read — the cell mutation below needs `&mut self`.
        {
            let _read = self.inner.read_guard()?;
            self.inner
                .graph
                .preview()
                .require(relate_core::PreviewFeature::ValueProvenance)?;
        }
        // ADR-0610: the untyped channel hard-sets the provenance family
        // as it merges — the last channel call owns the transaction's
        // family. The merge runs first: a refused seed (the over-bound
        // map) leaves the whole cell untouched, family included.
        self.merge_tx_metadata(metadata)?;
        self.tx_metadata.get_mut().record_type = RecordType::Provenance;
        Ok(())
    }

    /// Seed this connection's per-transaction metadata map with an
    /// explicit **record type** (ADR-0610) — [`Self::set_tx_metadata`]'s
    /// typed twin: the map merges exactly the same way (later keys win,
    /// the 16 MiB envelope, taken at the next transaction boundary and
    /// cleared after), and the transaction's record carries `record_type`
    /// instead of the provenance family's own. The commit joins the
    /// retained table as a record of its own family: `db.transactions`
    /// lists it with its `type` and **null** `changedKeys` (changed keys
    /// derive only for provenance records), and the provenance queries
    /// (`provenance()`, `history()`) never read it — a mixed-type store's
    /// provenance timeline holds only its own family's commits.
    ///
    /// The name must be a non-empty kebab-case name (lowercase segments
    /// of ASCII letters and digits joined by single hyphens), and the
    /// provenance family's own `"provenance"` is reserved — the untyped
    /// channels seed it, so a typed seed naming it is the typed
    /// `InvalidArgumentValue` rejection, never a silently-accepted
    /// provenance record. An empty map seeds nothing (the boundary then
    /// writes no record of the type), exactly like the untyped channel's.
    /// Gated like the untyped channel: the authored preview error on a
    /// store without the opt-in.
    ///
    /// ```
    /// # use relate::{Database, Value};
    /// # relate::test_opts::assert_preview_unset();
    /// # use std::collections::BTreeMap;
    /// let db = Database::open(":memory:").unwrap();
    /// let mut conn = db.connect().unwrap();
    /// let error = conn
    ///     .set_tx_metadata_of_type("audit-trail", BTreeMap::new())
    ///     .expect_err("the typed channel is gated like the untyped one");
    /// assert_eq!(
    ///     error.to_string(),
    ///     "semantic error: SemanticError: preview feature 'value-provenance' is not enabled: \
    ///      set RELATE_PREVIEW=value-provenance"
    /// );
    /// # Ok::<(), relate::Error>(())
    /// ```
    pub fn set_tx_metadata_of_type(
        &mut self,
        record_type: &str,
        metadata: BTreeMap<String, Value>,
    ) -> Result<()> {
        // The `GraphCell` deref contract: the preview read is under
        // `read_guard`, so a Follow-mode reload's `replace` cannot swap
        // the graph out from under the deref. The guard is scoped to
        // the read — the cell mutation below needs `&mut self`.
        {
            let _read = self.inner.read_guard()?;
            self.inner
                .graph
                .preview()
                .require(relate_core::PreviewFeature::ValueProvenance)?;
        }
        relate_engine_api::validate_record_type(record_type)?;
        // ADR-0610: the typed twin names the family — the merge runs
        // first, so a refused seed (the over-bound map) leaves the whole
        // cell untouched, family included.
        self.merge_tx_metadata(metadata)?;
        self.tx_metadata.get_mut().record_type = RecordType::Typed(record_type.to_owned());
        Ok(())
    }

    /// The transport channel's seed (ADR-0610): a Bolt `BEGIN`'s
    /// `tx_metadata` extra, applied at transaction open. Quiet where
    /// [`Self::set_tx_metadata`] is loud: a transport's extra is compat
    /// traffic, so on a store without the opt-in the map is ignored —
    /// exactly the pre-feature behavior a driver never notices — rather
    /// than failing the BEGIN. While the feature is on the map seeds the
    /// same per-transaction map (an over-bound map is the merge's typed
    /// `Error`, which the BEGIN surfaces as its failure); the channel
    /// hard-sets the provenance record type like every untyped channel,
    /// and a driver that sends both a BEGIN extra and `CALL
    /// tx.setMetaData` merges the two in call order.
    #[doc(hidden)]
    pub fn seed_tx_metadata(&mut self, metadata: BTreeMap<String, Value>) -> Result<()> {
        // The `GraphCell` deref contract: the preview read is under
        // `read_guard`, so a Follow-mode reload's `replace` cannot swap
        // the graph out from under the deref. The guard is scoped to
        // the read — the cell mutation below needs `&mut self`.
        {
            let _read = self.inner.read_guard()?;
            if !self
                .inner
                .graph
                .preview()
                .enabled(relate_core::PreviewFeature::ValueProvenance)
            {
                return Ok(());
            }
        }
        // ADR-0610: the transport channel hard-sets the provenance family
        // like every untyped channel — the merge runs first, so a refused
        // seed (the over-bound map) leaves the whole cell untouched,
        // family included.
        self.merge_tx_metadata(metadata)?;
        self.tx_metadata.get_mut().record_type = RecordType::Provenance;
        Ok(())
    }

    /// The one merge core both channels share (ADR-0610) — the canonical
    /// [`relate_engine_api::merge_tx_metadata`]: later keys win, the
    /// merged map bounded by the per-object envelope, the merge atomic.
    fn merge_tx_metadata(&mut self, metadata: BTreeMap<String, Value>) -> Result<()> {
        if metadata.is_empty() {
            return Ok(());
        }
        relate_engine_api::merge_tx_metadata(&mut self.tx_metadata.get_mut().map, metadata)
    }

    /// The in-statement channel (ADR-0610): apply a statement's captured
    /// `@txMetadata` annotations to the per-transaction cell, in the
    /// clause order the parser captured them — the fourth metadata input,
    /// seeded from the statement's own text before its first clause runs
    /// (the dispatch site calls this after the EXPLAIN early return and
    /// the admission gates, so a plan-only or refused statement seeds
    /// nothing). The seeding is one atomic step: every capture is
    /// interpreted, typed and merged into a local [`TxChannelState`]
    /// seeded from the cell, and the cell is assigned once, after the
    /// whole list is proven — a list whose second annotation refuses
    /// leaves the connection exactly as though none ran, so nothing from
    /// a refused statement rides the next commit. Each annotation is
    /// otherwise one channel call exactly like the others: its
    /// parameters spell one map — a single positional map literal, or
    /// all-named parameters — with an optional `type` string key
    /// consumed from that map into the record family (absent, the
    /// channel's home family: `provenance`, the default every untyped
    /// channel hard-sets; the home name spelled explicitly is admitted
    /// as the default, where the embedded typed twin reserves it —
    /// naming another family is that twin's whole contract; any other
    /// name goes through the canonical
    /// [`validate_record_type`](relate_engine_api::validate_record_type)),
    /// and the map itself merges through the canonical
    /// [`merge_tx_metadata`](relate_engine_api::merge_tx_metadata) with
    /// every earlier channel call — later keys winning, so the last
    /// annotation owns any shared key and the transaction's family.
    ///
    /// The captures arrive already gated (the front drops them while
    /// `value-provenance` is off, exactly as if the annotation had been
    /// any unrecognized CIP-281 name): this method runs only behind the
    /// opt-in, and its rejections are the channel's value-domain errors,
    /// never a silent drop.
    pub(super) fn apply_tx_metadata_annotations(
        &mut self,
        captures: &[relate_parser::TxMetadataAnnotation],
    ) -> Result<()> {
        // The local state the whole list is proven against before the
        // cell is touched — seeded from the cell so the merges compose
        // with every earlier channel call.
        let mut seeded = self.tx_metadata.get_mut().clone();
        for capture in captures {
            let seed = interpret_tx_metadata_annotation(capture)?;
            match &seed.record_type {
                // The home family: the default, spelled or not — the same
                // hard-set every untyped channel performs.
                RecordType::Provenance => seeded.record_type = RecordType::Provenance,
                RecordType::Typed(name) => {
                    relate_engine_api::validate_record_type(name)?;
                    seeded.record_type = RecordType::Typed(name.clone());
                }
            }
            relate_engine_api::merge_tx_metadata(&mut seeded.map, seed.map)?;
        }
        *self.tx_metadata.get_mut() = seeded;
        Ok(())
    }

    /// The principal set on this connection, if any (ADR-0273). See
    /// [`Self::set_principal`].
    pub fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
    }

    /// Run this connection's statements on the calling thread when their
    /// plan is bounded work (ADR-0504, the follow-up to ADR-0233's
    /// connection-thread-waits-on-the-worker shape).
    ///
    /// **Precondition — the caller owns this guarantee:** every later call
    /// into this connection must happen on a thread whose stack carries
    /// the engine's own query-worker provision (64 MiB, ADR-0259), for the
    /// connection's remaining lifetime. Parse, lowering, planning and
    /// execution recurse proportionally to input nesting; on an asserted
    /// thread that recursion is exactly as safe as on the worker because
    /// it is the same stack budget with the same ADR-0257 depth-500
    /// margin. A caller that asserts from a smaller stack reintroduces the
    /// pre-ADR-0259 exposure for its own process — the engine cannot check
    /// a thread's stack size. A server spawning its connection threads
    /// with an explicit 64 MiB stack (the Bolt surface, ADR-0233) asserts
    /// once per session at connect; hosts that cannot promise the budget
    /// simply do not assert, and every statement runs on the worker as
    /// before.
    ///
    /// With the assertion set, a statement whose plan is bounded READ
    /// work — point and joined reads, catalog/admin commands, plain
    /// projections (the planner's `plan_is_bounded_work` classification,
    /// ADR-0504) — runs
    /// inline: no worker handoff, no context switches. Anything with
    /// data-proportional or unbounded-row potential (variable-length and
    /// quantified paths, `UNWIND`, `CALL {}`, procedures, `LOAD CSV`,
    /// pattern expressions) escalates to the worker, whose wait keeps a
    /// [`Heartbeat`](crate::Heartbeat) ticking for the statements that can
    /// outlive its interval (ADR-0312). A panicking statement is contained
    /// to a typed internal error on both paths, and cancellation behaves
    /// identically (ADR-0259's guarantees hold unchanged).
    ///
    /// On wasm32 there is no worker — statements already run on the
    /// caller's thread — so the assertion changes nothing there.
    ///
    /// ```
    /// use relate::{Database, Value};
    ///
    /// // The assertion is made from a thread that honestly carries the
    /// // budget — a spawned thread stacked with the engine's exported
    /// // constant, exactly how a serving connection thread is stacked.
    /// let db = Database::open(":memory:").unwrap();
    /// let out = std::thread::Builder::new()
    ///     .stack_size(relate::QUERY_WORKER_STACK_BYTES)
    ///     .spawn(move || {
    ///         let mut conn = db.connect().unwrap();
    ///         conn.assume_worker_stack_on_caller_thread();
    ///         // A bounded read runs on this very thread and answers
    ///         // exactly as it does on the worker.
    ///         let result = conn.query("RETURN 1 AS one").unwrap();
    ///         assert_eq!(result.rows[0][0], Value::Int(1));
    ///         // Escalating statements still answer — on the worker: the
    ///         // unbounded-row UNWIND, and every write.
    ///         let result = conn.query("UNWIND [1, 2] AS x RETURN x").unwrap();
    ///         assert_eq!(result.rows.len(), 2);
    ///         conn.query("CREATE (:N)").unwrap();
    ///     })
    ///     .unwrap();
    /// out.join().unwrap();
    /// ```
    pub fn assume_worker_stack_on_caller_thread(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.caller_stack_asserted = true;
        }
        // wasm32: no worker exists to bypass — every statement already
        // runs on the caller's thread, so nothing is recorded.
    }

    pub fn query(&mut self, cypher: &str) -> Result<QueryResult> {
        self.query_with_params(cypher, BTreeMap::new())
    }

    pub fn execute(&mut self, cypher: &str) -> Result<QueryResult> {
        self.query(cypher)
    }

    /// The session namespace this connection's statements run in when they
    /// carry no `USE` clause (ADR-0117 §2), typed (ADR-0147):
    /// `Namespace::Neo4j`, `Namespace::System` or
    /// `Namespace::SystemOntologies`.
    pub fn namespace(&self) -> Namespace {
        self.namespace
    }

    /// Change the session namespace (ADR-0117 §2). Names match
    /// case-insensitively; an unknown name fails with `DatabaseNotFound`
    /// (`Graph not found: <name>`, ADR-0146), and a namespace hidden by
    /// the preview set (ADR-0593: `system_ontologies` while the
    /// `ontology` opt-in is off) fails the same way. A session
    /// namespace changes only between transactions (ADR-0116 §5): while an
    /// explicit transaction is open this fails with `Cannot change the
    /// namespace while a transaction is open.`
    ///
    /// ```
    /// # use relate::{Database, Namespace};
    /// let db = Database::open(":memory:").unwrap();
    /// let mut conn = db.connect().unwrap();
    /// assert_eq!(conn.namespace(), Namespace::Neo4j);
    /// conn.set_namespace("SYSTEM").unwrap();
    /// assert_eq!(conn.namespace(), Namespace::System);
    /// conn.begin().unwrap();
    /// assert!(conn.set_namespace("neo4j").is_err());
    /// conn.rollback().unwrap();
    /// conn.set_namespace("neo4j").unwrap();
    /// ```
    pub fn set_namespace(&mut self, name: &str) -> Result<()> {
        if self.txn.is_some() {
            return Err(ce_messages::namespace_change_in_transaction());
        }
        let namespace = parse_session_namespace(name)?;
        // ADR-0593: a namespace hidden by the preview set is refused here
        // too, with the same ordinary rejection the parse above produces
        // for an unknown name — through the one guarded spelling both
        // connect paths share.
        self.inner.require_namespace_visible(namespace)?;
        self.namespace = namespace;
        Ok(())
    }

    /// Local id for a pulled remote element if it still exists (ADR-0080).
    ///
    /// A poisoned/fenced handle is a typed error, distinct from a missing
    /// mapping (ADR-0099).
    pub fn pull_lookup(
        &self,
        source: &str,
        kind: PullKind,
        remote_id: &str,
    ) -> Result<Option<i64>> {
        let _read = self.inner.read_guard()?;
        let Some(local) = self.inner.graph.pull_identity_get(source, kind, remote_id) else {
            return Ok(None);
        };
        Ok(self.pull_local_alive(kind, local).then_some(local))
    }

    fn pull_local_alive(&self, kind: PullKind, id: i64) -> bool {
        if let Some(txn) = &self.txn {
            let view = ExecGraph::write(&self.inner.graph, &txn.scratch);
            return match kind {
                PullKind::Node => view.node_exists(id),
                PullKind::Rel => view.rel_exists(id),
            };
        }
        match kind {
            PullKind::Node => self.inner.graph.node_exists(id),
            PullKind::Rel => self.inner.graph.rel_exists(id),
        }
    }

    /// Record a pull identity mapping in the in-memory index — load-scoped
    /// state, never persisted (ADR-0158).
    pub fn pull_bind(
        &mut self,
        source: String,
        kind: PullKind,
        remote_id: String,
        local_id: i64,
    ) -> Result<()> {
        // The `GraphCell` deref contract: every read of the cell is under
        // `read_guard` (or a pin taken while it was held), so a Follow-mode
        // reload's `replace` cannot swap the graph out from under this
        // deref.
        let _read = self.inner.read_guard()?;
        // Writes the shared in-memory index outside any transaction: the
        // map is load-scoped metadata (ADR-0158), and pull_lookup filters
        // dead local ids on read (pull_local_alive), so a rolled-back
        // transaction can never be aliased through it.
        self.inner
            .graph
            .pull_identity_put(source, kind, remote_id, local_id);
        Ok(())
    }

    /// Plan a statement and return its plan tree (ADR-0344) without
    /// executing it — the `EXPLAIN` plan as a structured `PlanNode`, for
    /// callers that render it themselves: the CLI's terminal-width
    /// rendering, tooling, and the later `PROFILE` phases. Admission runs
    /// exactly as every statement's does — the health and
    /// failed-transaction gates, parse, lower, plan, and the compile-time
    /// gates of [`Self::admit_statement`], on the connection's 64 MiB
    /// worker (ADR-0259), never the caller's stack — so a statement that
    /// cannot be planned fails with the same typed error `EXPLAIN`
    /// reports, and a planned write leaves the graph untouched. No
    /// parameters are bound: a statement referencing one fails with the
    /// same typed error as a plain [`Self::query`] without parameters.
    pub fn explain_plan(&mut self, cypher: &str) -> Result<crate::PlanNode> {
        let result = self.dispatch_plan(cypher);
        if result.is_err() {
            // Same epilogue as `run_cancellable`: a panicking worker may
            // have left an explicit transaction's scratch in an unknown
            // state; fail it like any other query error.
            self.fail_txn();
        }
        // Planning admits no read, so the health check always applies.
        self.inner.check_healthy()?;
        result
    }

    /// Spawn the connection worker on first use, then plan there (the
    /// `dispatch_query` shape: parse/lower/plan recurse proportionally to
    /// input nesting, so they run on the 64 MiB worker).
    #[cfg(not(target_arch = "wasm32"))]
    fn dispatch_plan(&mut self, cypher: &str) -> Result<crate::PlanNode> {
        // Planning admits no read and reports no progress: the wait is
        // untimed (ADR-0312), so no heartbeat is armed.
        Self::run_on_worker(self.worker_slot(), || self.plan_body(cypher), None)?
    }

    /// wasm32 has no threads. The host stack is whatever the environment gave us.
    #[cfg(target_arch = "wasm32")]
    fn dispatch_plan(&mut self, cypher: &str) -> Result<crate::PlanNode> {
        self.plan_body(cypher)
    }

    /// wasm32 has no worker to dispatch to: the statement runs on this
    /// thread through the one `run_query` body, so there is nothing to
    /// wait for and a heartbeat can never be due (ADR-0312) — the twin of
    /// `dispatch_plan`'s wasm arm, and the target the mode-free
    /// `run_cancellable` calls on every architecture.
    #[cfg(target_arch = "wasm32")]
    fn dispatch_query(
        &mut self,
        cypher: &str,
        params: BTreeMap<String, Value>,
        token: &CancelToken,
        heartbeat: Option<Heartbeat<'_>>,
    ) -> QueryOutcome {
        drop(heartbeat);
        self.run_query(cypher, params, token)
    }

    /// The plan body, run on the connection's worker thread (ADR-0259):
    /// the one admission sequence, then the tree build — the estimates
    /// (ADR-0218) read from the store under a read guard, like the
    /// admission's own planning above. ADR-0503: this path bypasses the
    /// plan cache (no lookup, no insert) so the tree keeps spelling the
    /// statement as written — a cached, auto-parameterized plan would
    /// render `$__AUTO_*__` in its seek lines.
    fn plan_body(&mut self, cypher: &str) -> Result<crate::PlanNode> {
        let params = BTreeMap::new();
        let admission =
            self.admit_statement(cypher, &params, CacheUse::Bypass, web_time::Instant::now())?;
        let _read = self.inner.read_guard()?;
        let stats = crate::stats::GraphPatternStats::new(&self.inner.graph, &params);
        Ok(crate::plan_node::plan_tree(&admission.logical, &stats).root)
    }

    pub fn query_with_params(
        &mut self,
        cypher: &str,
        params: BTreeMap<String, Value>,
    ) -> Result<QueryResult> {
        self.run_cancellable(cypher, params, &CancelToken::new(), None)
            .and_then(Executed::into_result)
    }

    /// Like [`Self::query_with_params`], but the query can be stopped by
    /// another thread through `token` (ADR-0111).
    ///
    /// The token is checked at row-production points and inside the
    /// executor's long loops. Once cancelled the query returns
    /// `Error::Cancelled`, whose message is `"query cancelled"`, and the
    /// connection remains usable: a later query on it runs normally. `query`
    /// and `query_with_params` keep their signatures and run with a token
    /// that is never cancelled.
    ///
    /// ```
    /// use std::collections::BTreeMap;
    /// use relate::{CancelToken, Database};
    ///
    /// let db = Database::open(":memory:").unwrap();
    /// let mut conn = db.connect().unwrap();
    /// let token = CancelToken::new();
    /// // An uncancelled token lets the query run to completion.
    /// let result = conn
    ///     .query_with_params_cancellable("RETURN 1 AS one", BTreeMap::new(), token.clone())
    ///     .unwrap();
    /// assert_eq!(result.rows[0][0], relate::Value::Int(1));
    /// // `cancel()` flips the flag shared by every clone.
    /// token.cancel();
    /// assert!(token.is_cancelled());
    /// ```
    pub fn query_with_params_cancellable(
        &mut self,
        cypher: &str,
        params: BTreeMap<String, Value>,
        token: CancelToken,
    ) -> Result<QueryResult> {
        self.run_cancellable(cypher, params, &token, None)
            .and_then(Executed::into_result)
    }

    /// Shared worker-thread wrapper for every query entry point: the
    /// cancellable and non-cancellable ones, and the staged one
    /// (`Connection::query_staged`, ADR-0319) that also takes the waiting
    /// caller's heartbeat (ADR-0312).
    pub(crate) fn run_cancellable(
        &mut self,
        cypher: &str,
        params: BTreeMap<String, Value>,
        token: &CancelToken,
        heartbeat: Option<Heartbeat<'_>>,
    ) -> Result<Executed> {
        // ADR-0259: parse, front-end lowering/validation, planning, and
        // execution all recurse proportionally to input nesting, so the
        // whole query runs on this connection's 64 MiB worker. The worker
        // is spawned on the first query and reused after that. The calling
        // thread's stack size is irrelevant; a worker panic or a spawn
        // failure surfaces as a typed internal error — never an abort,
        // never a fallback to the caller's stack.
        //
        // ADR-0504: the dispatch entry (`dispatch_query`) picks the path —
        // the fast path for a connection whose caller asserted the
        // worker's own stack budget, the worker body for every other — so
        // this shared epilogue stays mode-free.
        let outcome = self.dispatch_query(cypher, params, token, heartbeat);
        if matches!(outcome.result, Ok(Executed::Rows(_))) {
            log_logical_bytes(&self.inner);
        }
        // A retryable admission refusal never ran the statement, so an
        // explicit transaction stays usable (ADR-0333). Every other error,
        // including a worker panic that may have left the scratch unknown,
        // fails the transaction so a rollback is required. Ordinary query
        // errors already failed it inside `run_query`; this is then a no-op.
        let retryable_refusal = matches!(
            outcome.result,
            Err(crate::Error::ResourceLimit {
                retryable: true,
                ..
            })
        );
        if !matches!(outcome.result, Ok(Executed::Rows(_))) && !retryable_refusal {
            self.fail_txn();
        }
        // An already admitted read owns a pin to the last safe snapshot and
        // may finish even if a later physical publication poisons the handle.
        if !outcome.admitted {
            self.inner.check_healthy()?;
        }
        outcome.result
    }
}
