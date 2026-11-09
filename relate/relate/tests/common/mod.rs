//! Helpers shared across the `relate` integration-test crates (each
//! file directly under `tests/` is its own crate; this module is the
//! single source for the helpers they share): general-purpose helpers,
//! plus the pinned Neo4j CE error texts for the fixed-namespace tests
//! (testdata/ce-errors/neo4j-ce.json), the constraint-violation error
//! texts, the error-text extractor, the `db.ontology.*` document helpers
//! (`set_doc`/`get_doc`), the WAL segment-magic readers, the WAL frame
//! encoder, the crash-seeded store fixtures
//! (`store_vfs`/`active_segment`/`seed_store`), the deterministic
//! test-store open policy (`opts`), and the ADR-0120 checkpoint
//! format-marker assertion (`assert_checkpoint_format` plus its
//! `ExpectedOntology`), the `ontology` preview opt-in (`preview_ontology`,
//! ADR-0378), and the shared closed-side preview assertion
//! (`assert_preview_closed`).
//!
//! Each including test crate uses a different subset of these helpers, so
//! the ones it does not use would warn as dead code — allow it module-wide.
#![allow(dead_code)]

use std::collections::BTreeMap;

/// CE 42N17 suffix (ADR-0116 §4; testdata/ce-errors/neo4j-ce.json
/// `system_data_clause`).
pub const SYSTEM_CLAUSES_SUFFIX: &str = "The system database supports a restricted set of Cypher clauses. The supported clause structure for procedure calls is: CALL, YIELD, RETURN. YIELD and RETURN clauses are optional. The order of the clauses is fixed and each can only occur once.";
/// CE 42N18 text (ADR-0116 §4/§9; CE appends "on this Neo4j instance." to
/// the ADR wording — testdata/ce-errors/neo4j-ce.json `read_only_write`).
/// Still the recorded CE text for read-only databases; the
/// `system_ontologies` gate no longer uses it (ADR-0229 §2).
pub const READ_ONLY_SUFFIX: &str =
    "No write operations are allowed on this database. The database is in read-only mode on this Neo4j instance.";
/// The authored 42N17 text every non-administration statement routed to
/// `system_ontologies` fails with (ADR-0229 §2: reads included,
/// EXPLAIN-wrapped included). Not CE-pinned — live CE allows reads on the
/// namespace, so the fixture carries no case for it.
pub const NOT_QUERYABLE_SUFFIX: &str =
    "This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead.";
/// CE 2026.09's composability text for a `WITH` around a
/// database-administration command on the `system` namespace, live-pinned
/// 2026-11-04 (ADR-0116 §4 completion; testdata/ce-errors/neo4j-ce.json
/// `use_system_show_databases_with`; wire 42001 /
/// Neo.ClientError.Statement.SemanticError, cause chain 42N17 → 42NA9).
pub const WITH_ON_SYSTEM_SUFFIX: &str =
    "The following clauses are not allowed on a system database: WITH.";
/// CE 2026.09's 42N79 auto-routing text, live-pinned 2026-11-04 (ADR-0116
/// §2 amendment; testdata/ce-errors/neo4j-ce.json
/// `use_neo4j_show_databases`; Neo.ClientError.Statement.SyntaxError).
pub const USE_WITH_ADMIN_SUFFIX: &str =
    "The `USE` clause is not required for administration commands. \
     Retry your query without the `USE` clause, and it will be routed automatically.";

pub fn err_text<T>(result: relate::Result<T>) -> String {
    // `err().expect` over `unwrap_err`: no `T: Debug` bound, so callers
    // never force-fit their Ok type.
    result.err().expect("expected an error").to_string()
}

/// Opt this test process into the `ontology` preview feature (ADR-0378):
/// the `db.ontology.*` procedures are gated behind
/// `RELATE_PREVIEW=ontology`, and `Database::open` reads the variable
/// once per open. `Once` because a test binary runs its tests on threads
/// sharing one environment; every test in the enabled-gate ontology crates
/// calls this before its first open, and no test ever toggles the variable
/// back.
pub fn preview_ontology() {
    set_preview_once("ontology");
}

/// The shared opt-in of the gated-connection fixtures: the preview list is
/// set exactly once per test process (`Once` because a test binary runs
/// its tests on threads sharing one environment -- a per-connection
/// `set_var` races a sibling thread's in-flight `Database::open`, the
/// round-13 stellar F-13-4 hazard), and no test ever toggles the
/// variable back.
pub fn set_preview_once(features: &str) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| std::env::set_var("RELATE_PREVIEW", features));
}

/// Assert that `query` fails closed with the authored preview-gate error for
/// `feature` (ADR-0371) — the one closed-side assertion every gated-feature
/// test binary shares. The expected text comes from
/// [`relate_core::preview_feature_disabled_text`], the engine's single
/// source of the message, so no fixture re-spells it.
pub fn assert_preview_closed(feature: relate_core::PreviewFeature, query: &str) {
    let expected = relate_core::preview_feature_disabled_text(feature);
    let error = connection()
        .query(query)
        .expect_err("a gated feature must fail closed without the opt-in");
    let rendered = error.to_string();
    assert!(
        rendered.contains(&expected),
        "{query}: expected the authored preview error {expected:?}, got {rendered}"
    );
}

/// A fresh connection on a fresh in-memory store — the shared starting
/// point of the fixed-namespace integration tests.
pub fn connection() -> relate::Connection {
    relate::Database::open(":memory:")
        .unwrap()
        .connect()
        .unwrap()
}

/// The first column of every result row as a `String` — the shape the
/// title/name/note assertions of the SEARCH and procedure test crates all
/// spelled inline.
pub fn first_column_strings(result: &relate::QueryResult) -> Vec<String> {
    result
        .rows
        .iter()
        .map(|row| match &row[0] {
            relate::Value::String(value) => value.to_string(),
            other => panic!("expected a string column, got {other:?}"),
        })
        .collect()
}

/// Opens a fresh in-memory store with `RELATE_PATTERN_ENGINE` forced to
/// `engine` (ADR-0519). The switch is read once at open and the variable
/// is process-global, so the lock keeps the `set_var` + open pair atomic
/// against sibling tests: a sibling's open must never read this leg's
/// value.
pub fn open_with_engine(engine: &str) -> relate::Connection {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::env::set_var("RELATE_PATTERN_ENGINE", engine);
    relate::Database::open(":memory:")
        .unwrap()
        .connect()
        .unwrap()
}

/// Runs `body` once per forced pattern engine (the walker and the frontier
/// NFA), each on its own fresh store from [`open_with_engine`].
pub fn for_each_engine(mut body: impl FnMut(&str, &mut relate::Connection)) {
    for engine in ["walk", "nfa"] {
        let mut conn = open_with_engine(engine);
        body(engine, &mut conn);
    }
}

/// A fresh connection whose store carries one `IS UNIQUE` constraint:
/// `CREATE CONSTRAINT <constraint> FOR (n:<label>) REQUIRE n.<prop> IS UNIQUE`
/// — the shared scaffold `temporal_identity_split.rs` and
/// `constraint_numeric_equivalence.rs` each hand-rolled (F-4-18).
pub fn unique_constrained(constraint: &str, label: &str, prop: &str) -> relate::Connection {
    let mut c = connection();
    c.query(&format!(
        "CREATE CONSTRAINT {constraint} FOR (n:{label}) REQUIRE n.{prop} IS UNIQUE"
    ))
    .unwrap();
    c
}

/// One `:Person` and one `:City` — the two-label store the notification
/// tests' disjoint multi-pattern cartesian statements name.
pub fn two_label_store() -> relate::Connection {
    let mut conn = connection();
    conn.query("CREATE (:Person)").unwrap();
    conn.query("CREATE (:City)").unwrap();
    conn
}
/// The plan tree an `EXPLAIN`/`PROFILE` statement stamps on its result —
/// the shared guarded accessor: the per-test files never re-spell
/// `plan_root().expect(…)`.
pub fn plan(result: &relate::QueryResult) -> &relate::PlanNode {
    result.plan_root().expect("EXPLAIN/PROFILE stamps the plan")
}

/// The notification kinds of one result, in list order — the shared
/// helper of the notification test files.
pub fn kinds(result: &relate::QueryResult) -> Vec<relate_model::wire::NotificationKind> {
    result
        .notifications()
        .iter()
        .map(relate::Notification::kind)
        .collect()
}

/// The result's notifications of `kind`, in list order — the one
/// filter-by-kind spelling the notification test files share (Relate may
/// warn more than Neo4j, so tests assert on their target kind, never on
/// the whole list).
pub fn notifications_of_kind(
    result: &relate::QueryResult,
    kind: relate_model::wire::NotificationKind,
) -> Vec<&relate::Notification> {
    result
        .notifications()
        .iter()
        .filter(|notification| notification.kind() == kind)
        .collect()
}

/// Whether the result carries a notification of `kind` — the typed predicate
/// the notification test files share. The vocabulary is a typed enum
/// (ADR-0363), so tests discriminate by kind rather than by wire code.
pub fn has_notification(
    result: &relate::QueryResult,
    kind: relate_model::wire::NotificationKind,
) -> bool {
    !notifications_of_kind(result, kind).is_empty()
}

/// The first node with the given operator name, pre-order — the shared
/// fixture for pinning an affected operator in a plan tree (ADR-0359:
/// the engine emits the tree only, so assertions target operators, not
/// rendered lines).
/// The first (pre-order) node with the given operator name — the
/// early-return view of [`find_nodes`], one recursion for the one
/// matching policy.
pub fn find_node<'a>(node: &'a relate::PlanNode, operator: &str) -> Option<&'a relate::PlanNode> {
    find_nodes(node, operator).into_iter().next()
}

/// Every node with the given operator name, pre-order — the shared
/// fixture for counting a statement's occurrences of an operator.
pub fn find_nodes<'a>(node: &'a relate::PlanNode, operator: &str) -> Vec<&'a relate::PlanNode> {
    let mut collected = Vec::new();
    collect_nodes(node, operator, &mut collected);
    collected
}

fn collect_nodes<'a>(
    node: &'a relate::PlanNode,
    operator: &str,
    collected: &mut Vec<&'a relate::PlanNode>,
) {
    if node.operator == operator {
        collected.push(node);
    }
    for child in &node.children {
        collect_nodes(child, operator, collected);
    }
}

/// `Value::List` from a Rust vec (F-1-4: one definition — the three test
/// files that each copied it now share this one).
pub fn list(values: Vec<relate::Value>) -> relate::Value {
    relate::Value::List(values.into())
}

/// The `BTreeMap` parameter map the integration tests pass to
/// `query_with_params`, from `(name, value)` pairs (the hoisted twin of the
/// `params` helper `bytes_value.rs` and `parameter_missing.rs` each carried;
/// ADR-0335's round-1 stellar nit).
pub fn params(entries: &[(&str, relate::Value)]) -> BTreeMap<String, relate::Value> {
    entries
        .iter()
        .map(|(name, value)| ((*name).to_string(), value.clone()))
        .collect()
}

/// A Cypher MAP `Value` from `(key, value)` pairs — the value-side twin of
/// [`params`], shared by the DDL and value-operator tests that assert on
/// map results or pass maps as parameters.
pub fn map(entries: &[(&str, relate::Value)]) -> relate::Value {
    relate::Value::Map(
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect::<BTreeMap<_, _>>()
            .into(),
    )
}

/// The empty Cypher MAP: `map(&[])`.
pub fn empty_map() -> relate::Value {
    map(&[])
}

/// The openCypher TCK error *class* the language bindings expose for
/// `statement`'s failure (the `tck_class` slot of
/// [`relate::binding_error_metadata`]; the finer `kind` is not
/// returned). The one spelling the DDL class tables read, so a class
/// assertion never re-implements the empty-params/with-params fork.
pub fn error_class(
    conn: &mut relate::Connection,
    statement: &str,
    params: BTreeMap<String, relate::Value>,
) -> &'static str {
    let result = if params.is_empty() {
        conn.query(statement)
    } else {
        conn.query_with_params(statement, params)
    };
    let error = result
        .err()
        .unwrap_or_else(|| panic!("`{statement}` unexpectedly succeeded"));
    relate::binding_error_metadata(&error).1
}

/// The class of `statement` run without parameters (the literal
/// spellings) — the no-params half of [`error_class`], so the class-table
/// files share one default instead of each wrapping it again.
pub fn error_class_literal(conn: &mut relate::Connection, statement: &str) -> &'static str {
    error_class(conn, statement, BTreeMap::new())
}

/// Assert `error` carries the `ArgumentError` class (Neo4j's
/// `Neo.ClientError.General.InvalidArguments`).
pub fn assert_argument_error(error: &relate::Error) {
    assert_eq!(
        relate::binding_error_metadata(error).1,
        "ArgumentError",
        "{error}"
    );
}

/// Assert `error` carries the `SyntaxError` class (Neo4j's
/// `Neo.ClientError.Statement.SyntaxError`).
pub fn assert_syntax_error(error: &relate::Error) {
    assert_eq!(
        relate::binding_error_metadata(error).1,
        "SyntaxError",
        "{error}"
    );
}

/// Builds a string value for row assertions.
pub fn string(value: &str) -> relate::Value {
    relate::Value::String(value.into())
}

/// A `Value::List` of `Value::String` — the one canonical spelling of a
/// list-of-strings expectation (the hoisted twin of `uuid_label_predicates`'
/// `labels` and `schema_procedures`' `strings`; F-1-53).
pub fn strings(values: &[&str]) -> relate::Value {
    list(values.iter().map(|value| string(value)).collect())
}

/// The first cell of a one-row query, as the STRING it must be — the
/// one toString-extraction helper the temporal integration tests share.
pub fn text(conn: &mut relate::Connection, query: &str) -> String {
    match scalar_one(conn, query) {
        relate::Value::String(value) => value.to_string(),
        other => panic!("{query}: expected a STRING, got {other:?}"),
    }
}

/// Runs `query`, which must succeed.
pub fn run(connection: &mut relate::Connection, query: &str) -> relate::QueryResult {
    connection
        .query(query)
        .unwrap_or_else(|e| panic!("query failed: {e}\n{query}"))
}

/// The rows of `query`, which must succeed.
pub fn rows(connection: &mut relate::Connection, query: &str) -> Vec<Vec<relate::Value>> {
    run(connection, query).rows
}

/// The first cell of the first row of `query`, e.g. a `count(…)`.
pub fn scalar(connection: &mut relate::Connection, query: &str) -> relate::Value {
    rows(connection, query).remove(0).remove(0)
}

/// [`scalar`], but asserts the query returns exactly one row.
pub fn scalar_one(connection: &mut relate::Connection, query: &str) -> relate::Value {
    let mut rs = rows(connection, query);
    assert_eq!(rs.len(), 1, "{query}: expected a single row");
    rs.remove(0).remove(0)
}

/// Every column of the exactly-one row (the all-columns twin of
/// `scalar_one`) — the shared extractor the comparison and temporal test
/// files had each copied (F-1-11, F-1-12 and F-1-50).
pub fn row(connection: &mut relate::Connection, query: &str) -> Vec<relate::Value> {
    let mut rows = rows(connection, query);
    assert_eq!(rows.len(), 1, "{query}: expected one row");
    rows.remove(0)
}

/// Run one comparison-case table: each tuple is `(a, b, eq, lt, gt,
/// distinct)`, and the one query shape asserts all four answers at once
/// (hoisted from `comparison_semantics.rs`; F-1-54).
pub fn assert_comparison_cases(
    c: &mut relate::Connection,
    cases: &[(&str, &str, bool, bool, bool, i64)],
) {
    for &(a, b, eq, lt, gt, distinct) in cases {
        let query = format!(
            "WITH {a} AS a, {b} AS b UNWIND [a, b] AS x \
             RETURN a = b AS eq, a < b AS lt, a > b AS gt, count(DISTINCT x) AS dc"
        );
        assert_eq!(
            row(c, &query),
            vec![
                relate::Value::Bool(eq),
                relate::Value::Bool(lt),
                relate::Value::Bool(gt),
                relate::Value::Int(distinct)
            ],
            "{a} vs {b}"
        );
    }
}

/// The error text of a `unique` or `key` violation of constraint `name`
/// (ADR-0172).
pub fn uniqueness(name: &str) -> String {
    format!("semantic error: ConstraintValidationFailed: `{name}` uniqueness")
}

/// The error text of a missing `key` or `exists` property of constraint
/// `name` (ADR-0172).
pub fn requires(name: &str) -> String {
    format!("semantic error: ConstraintValidationFailed: `{name}` requires properties")
}

/// The error text of a `propertyType` violation of constraint `name`
/// (ADR-0172).
pub fn property_type(name: &str) -> String {
    format!("semantic error: ConstraintValidationFailed: `{name}` property type")
}

/// A `YIELD` of a column the procedure does not produce must fail
/// validation with an `UnknownProcedureOutput` error naming the column.
pub fn assert_unknown_output_fails(query: &str, unknown_column: &str) {
    let mut connection = connection();
    let error = connection.query(query).unwrap_err();
    assert!(
        error.to_string().contains("UnknownProcedureOutput"),
        "error should be an unknown-output validation error, got: {error}"
    );
    assert!(
        error.to_string().contains(unknown_column),
        "error should name the unknown column `{unknown_column}`, got: {error}"
    );
}

/// The Relate-authored Ontology Graph Spec 1.1.0 example
/// (`relate-ontology/schema/example.json`, the published shape
/// ADR-0487 §1 pins) — the shared valid document the `db.ontology.*`
/// fixtures pass as `$ontology`. `set` validates it; `apply` refuses the
/// constructs the derived graph cannot express, which the fixtures that
/// need `apply` assert on rather than rely on.
pub const ONTOLOGY_DOC: &str = include_str!("../../../relate-ontology/schema/example.json");

/// `CALL db.ontology.set($ontology)` with `doc` — Result-returning, so a
/// test can assert on the error text of a refused document as well as on
/// a successful set.
pub fn set_doc(conn: &mut relate::Connection, doc: &str) -> relate::Result<relate::QueryResult> {
    conn.query_with_params(
        "CALL db.ontology.set($ontology)",
        std::collections::BTreeMap::from([(
            "ontology".to_string(),
            relate::Value::String(doc.into()),
        )]),
    )
}

/// The single `ontology` cell of a `db.ontology.get` query, which must
/// succeed with exactly one row.
pub fn get_doc(conn: &mut relate::Connection, cypher: &str) -> relate::Value {
    let result = conn.query(cypher).unwrap();
    assert_eq!(result.header.columns, ["ontology"]);
    assert_eq!(result.rows.len(), 1);
    result.rows[0][0].clone()
}

/// `(segment name, magic)` of every WAL segment under `dir`, sorted by
/// name — the last entry is the active segment. The per-codec WAL
/// directory comes from [`relate_storage::SnapshotCodecKind::wal_dir`]
/// (ADR-0406 §3 for the markdown store).
pub fn segment_magics(
    dir: &std::path::Path,
    codec: relate_storage::SnapshotCodecKind,
) -> Vec<(String, [u8; 8])> {
    let mut segments: Vec<(String, [u8; 8])> = std::fs::read_dir(dir.join(codec.wal_dir()))
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension().and_then(|e| e.to_str()) == Some("log")).then(|| {
                let bytes = std::fs::read(&path).unwrap();
                (
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    bytes[..8].try_into().unwrap(),
                )
            })
        })
        .collect();
    segments.sort();
    segments
}

/// Just the magics of [`segment_magics`], in segment-name order.
pub fn magics(dir: &std::path::Path, codec: relate_storage::SnapshotCodecKind) -> Vec<[u8; 8]> {
    segment_magics(dir, codec)
        .into_iter()
        .map(|(_, magic)| magic)
        .collect()
}

/// Open a store's VFS by the storage crate's own path rule: the bundle VFS
/// for a `.rl` file, the Native VFS for a directory. A `BundleVfs` holds
/// the host file's exclusive flock, so the handle must be dropped before
/// `Database::open` on the same store.
pub fn store_vfs(root: &std::path::Path) -> Box<dyn relate_storage::Vfs> {
    if relate_storage::is_bundle_path(root) {
        Box::new(relate_storage::BundleVfs::open_or_create(root).unwrap())
    } else {
        Box::new(relate_storage::NativeVfs::new(root))
    }
}

/// The store-relative name of the active (highest-numbered) WAL segment,
/// e.g. `wal/00000001.log`.
pub fn active_segment(vfs: &dyn relate_storage::Vfs) -> String {
    let mut segments: Vec<String> = vfs
        .list_dir("wal")
        .unwrap()
        .into_iter()
        .filter(|name| name.ends_with(".log"))
        .collect();
    segments.sort();
    format!("wal/{}", segments.last().expect("a WAL segment must exist"))
}

/// The deterministic-test-store open policy: zero checkpoint interval
/// (every durability boundary is an explicit `db.checkpoint()`, the timer
/// never fires on its own) plus the chosen snapshot codec — the shared
/// fixture the integration tests open stores with, so no test re-spells
/// the `OpenOptions` record.
pub fn opts(codec: relate_storage::SnapshotCodecKind) -> relate::OpenOptions {
    relate::OpenOptions {
        checkpoint_interval: Some(std::time::Duration::ZERO),
        snapshot_codec: Some(codec),
        ..relate::OpenOptions::default()
    }
}

/// Create a store through the public `Database` API under `codec`, commit
/// once, checkpoint, commit again, then close with
/// `leak_without_checkpoint` (a crash after the WAL fsync) so the last
/// commit lives only in the active WAL segment.
pub fn seed_store(root: &std::path::Path, codec: relate_storage::SnapshotCodecKind) {
    seed_store_with(root, codec, &[]);
}

/// [`seed_store`] plus `pre_checkpoint` statements applied before the
/// checkpoint — the parametrized fixture the constraint-replay seeding
/// shares, where the DDL must live in the checkpoint and the appended WAL
/// tail carries only the duplicate writes.
pub fn seed_store_with(
    root: &std::path::Path,
    codec: relate_storage::SnapshotCodecKind,
    pre_checkpoint: &[&str],
) {
    let db = relate::Database::open_with(root.to_str().unwrap(), opts(codec)).unwrap();
    let mut conn = db.connect().unwrap();
    conn.query("CREATE (:T {name: 'checkpointed'})").unwrap();
    for statement in pre_checkpoint {
        run(&mut conn, statement);
    }
    db.checkpoint().unwrap();
    conn.query("CREATE (:T {name: 'tailed'})").unwrap();
    drop(conn);
    db.leak_without_checkpoint();
}

/// The one WAL frame encoder the crash-planting fixtures share: CRC-32 of
/// the payload, then the payload length as a little-endian `u32`, then the
/// payload verbatim (ADR-0120 §3). Each fixture builds its own payload.
pub fn wal_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// A complete, CRC-valid WAL frame carrying one `OntologySet` op with `spec`
/// verbatim (ADR-0120 §3/§4) — the raw path a pre-bump binary's store carries
/// a document the public `set` would now refuse. `spec` is JSON-escaped by
/// the serializer, so any document syntax can be planted. The receiving
/// segment must carry the `MDBWAL02` ontology magic (ADR-0120 §4): the
/// scanner rejects an ontology op in an `MDBWAL01` segment.
pub fn ontology_set_frame(lsn: u64, commit_ts: u64, spec: &str) -> Vec<u8> {
    let payload = serde_json::json!({
        "lsn": lsn,
        "commit_ts": commit_ts,
        "ops": [{"op": "OntologySet", "database": "neo4j", "spec": spec}],
    })
    .to_string();
    wal_frame(payload.as_bytes())
}

/// What a checkpoint's ontology publication must look like — the one
/// typed expectation [`assert_checkpoint_format`] asserts on, so every
/// shape asserts something (no held-but-unchecked hole).
pub enum ExpectedOntology {
    /// The plain format: the codec's ontology field is absent.
    Absent,
    /// The ontology is held: the codec's ontology field is present (a
    /// Parquet manifest publishes by reference — `ontology_path`,
    /// ADR-0120 §5 — so presence is the most it can assert).
    Held,
    /// The ontology is held and published verbatim: the field's value is
    /// exactly `document`.
    Document(&'static str),
}

/// Assert the ADR-0120 §4/§8 checkpoint format markers of a Native store
/// directory, per snapshot codec, against one typed ontology expectation.
/// The ontology state is the format-marker state: the plain format
/// ([`ExpectedOntology::Absent`]) is a Parquet `format_version` 5 (the
/// wide pair, ADR-0553) / a JSON `value_encoding` 2 with no ontology
/// published anywhere, and a held ontology is `format_version` 6 /
/// `value_encoding` 3; the experimental codec writes the legacy pair
/// (ADR-0537), 3/4. Each codec arm
/// asserts on one typed read of its own checkpoint image
/// (`SnapshotCodecKind::checkpoint_image`): the Parquet manifest's
/// `ontology_path` (by reference, ADR-0120 §5), the JSON `graph.json`'s
/// inline `ontology` field, and the markdown sidecar manifest
/// (ADR-0406 §3), whose envelope names the codec and whose inline
/// ontology follows the JSON codec's rules.
pub fn assert_checkpoint_format(
    dir: &std::path::Path,
    codec: relate_storage::SnapshotCodecKind,
    expected: ExpectedOntology,
) {
    // One typed read of the checkpoint image, shared by every arm below.
    let image: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join(codec.checkpoint_image())).unwrap())
            .unwrap();
    // The ontology state is the format-marker state (ADR-0120 §4/§8).
    let held = !matches!(expected, ExpectedOntology::Absent);
    match codec {
        relate_storage::SnapshotCodecKind::Parquet => {
            assert_eq!(image["format_version"], if held { 6 } else { 5 });
            // A Parquet manifest publishes the ontology by reference
            // (`ontology_path`) — the referenced object's bytes are the
            // document, asserted verbatim, never just presence.
            let published = image.get("ontology_path").map(|value| {
                let path = value.as_str().expect("ontology_path is a path");
                relate_storage::read_to_vec(&*store_vfs(dir), path)
                    .unwrap_or_else(|e| panic!("ontology object {path} of {dir:?} reads: {e}"))
            });
            assert_ontology(published, expected);
        }
        relate_storage::SnapshotCodecKind::Json => {
            assert_eq!(image["value_encoding"], if held { 3 } else { 2 });
            assert_ontology(inline_ontology(&image), expected);
        }
        relate_storage::SnapshotCodecKind::Markdown => {
            assert_eq!(image["codec"], "markdown");
            assert_ontology(inline_ontology(&image), expected);
        }
        // The experimental codec's image is the plain codec manifest: the
        // format marker (classifying against the manifest's own ontology
        // flag, ADR-0120 §8) plus the checkpoint theater. The ontology
        // document rides the codec payload, so the image never carries it.
        relate_storage::SnapshotCodecKind::Rkyv => {
            assert_eq!(image["format_version"], if held { 4 } else { 3 });
            assert_eq!(image["has_ontology"], held);
            assert_ontology(None, ExpectedOntology::Absent);
        }
    }
}

/// The inline ontology text of a checkpoint image (`graph.json` or the
/// markdown sidecar manifest), as published-document bytes.
fn inline_ontology(image: &serde_json::Value) -> Option<Vec<u8>> {
    image
        .get("ontology")
        .and_then(|value| value.as_str())
        .map(str::as_bytes)
        .map(<[u8]>::to_vec)
}

/// The ontology-publication rule every codec arm of
/// [`assert_checkpoint_format`] shares, over the codec's own
/// published-document accessor — the inline field's text, or the
/// referenced object's bytes for a Parquet manifest: absent, present,
/// or the published document verbatim.
fn assert_ontology(published: Option<Vec<u8>>, expected: ExpectedOntology) {
    match expected {
        ExpectedOntology::Absent => {
            assert!(
                published.is_none(),
                "the checkpoint image publishes no ontology: {published:?}"
            );
        }
        ExpectedOntology::Held => {
            assert!(published.is_some(), "the checkpoint publishes its ontology");
        }
        ExpectedOntology::Document(document) => {
            assert_eq!(
                published.as_deref(),
                Some(document.as_bytes()),
                "the published ontology is the document"
            );
        }
    }
}
