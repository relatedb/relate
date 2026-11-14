// The sealed adjacency tier (ADR-0551): compressed chunks, versioned
// tombstones, and the delta merge beside the live store.
mod adjacency;
mod catalog_meta;
mod csv;
mod ctx;
mod metrics;
// The one tiered distinct set under every DISTINCT site (LABS-852).
mod distinct_set;
#[cfg(test)]
mod distinct_set_tests;
mod effect;
mod eval;
// The eval-side pattern-walk engine (pattern comprehensions, pattern
// predicates, expression shortest paths): a pattern-walk client like the
// exec walkers, hoisted out of the eval tree so `eval` stays the
// expression core strictly below `pattern_walk` — private module,
// `pub(crate)` items, one public path per item at `crate::eval_walk::…`.
mod eval_walk;
mod exec;
// The frontier rung's decline vocabulary (ADR-0478, ADR-0516): private
// module, root re-exports only (one public path).
mod frontier_decline;
mod function_registry;
mod graph;
mod graph_type;
mod graph_type_visualize;
mod group_key;
#[cfg(test)]
mod group_key_tests;
// The hasher seam: the Fx alias and the one process-wide seeded state.
mod hash_seed;
mod memory;
mod overlay;
mod props;
// The pattern-walk vocabulary both walk engines share (LABS-668): private
// module, `pub(crate)` items — nothing re-exported at the root.
mod pattern_walk;
// The diagnostic pattern-engine switch (ADR-0519): private module with a
// root re-export only (one public path) — the store's service bundle
// (`graph/services`) holds the field, the `relate` crate's open path
// resolves the variable and installs it, and the kernel rungs read it
// back through `Graph::pattern_engine`.
mod pattern_engine;
mod query_engine;
// Private module with a root re-export only (one public path): internal
// callers use `crate::procedure_gate::…`, external consumers use the root
// re-export below.
mod procedure_gate;
mod workspace;
// Private module with root re-exports only (one public path): internal
// callers already use `crate::procedure_registry::…`, external consumers
// use the root re-exports below.
mod procedure_registry;
mod pull_identity;
mod redo;
// The compact row schema (LABS-668's hoist, finished at the crate root):
// private module, `pub`/`pub(crate)` items — every consumer, both walk
// engines included, imports downward from `crate::row`, the one public
// path per item.
mod row;
// The relationship-uniqueness set in its boundary form (LABS-1116, the
// set-type migration LABS-822 deferred): the type threaded through every
// used-set signature — `Matched`, the walk-entry and probe-predicate
// parameters, the GPM state carry. Hoisted to the crate root beside
// `row` (LABS-668's precedent): the workspace meter and both walk
// engines import downward from here, no module importing up. Its hasher
// policy (engine-generated ids, so the id policy) is `hash_seed`'s.
pub(crate) type UsedSet = hash_seed::IdSet<relate_core::RelId>;
mod shortest;
mod show_catalog;
mod spatial;
mod store;
#[cfg(test)]
mod test_support;
// The tiered word-set utility (ADR-0522): the tier mechanics ADR-0507
// built for the distinct set — one key class's words, inline then
// promoted — freed of DISTINCT semantics so raw-key consumers (the
// label-union planner work) can hold it; `distinct_set` delegates its
// word storage to it. Private module, `pub(crate)` items.
mod tiered_set;
#[cfg(test)]
mod tiered_set_tests;
// The stored vector-index option schema (stellar round 3, std-6): the
// key table, the storable-metric narrowing, and the one parse the CREATE
// validation and the stored read share — split from the similarity
// kernels in `vector_similarity` so the two concerns stop sharing a file.
mod vector_config;
mod vector_similarity;

pub use catalog_meta::{DeprecatedBy, DeprecationForm};
pub use csv::CsvPolicy;
pub use ctx::BatchCommit;
pub use effect::StatementEffect;
pub use exec::bulk::{
    analyze_bulk_write, BulkCursor, BulkTargets, BulkWrite, DEFAULT_BATCH_BUDGET,
    DEFAULT_BATCH_THRESHOLD,
};
pub use exec::graph_stats::GraphStats;
pub use exec::procedures::{check_explicit_arity, check_implicit_passing_mode};
pub use exec::{
    catalog_create_statements, ddl_notifications::ddl_notifications, execute, execute_read_at,
    value_matches_proc_type, ExecRequest, QueryResult, ReadAdmission,
};
pub use frontier_decline::{FrontierDecline, FrontierDeclineReason, FrontierRunDecline};
pub use function_registry::builtin_function_deprecation;
pub use graph::constraint_keys::admission::{ConstraintAdmission, SlotPosition};
pub use graph::{
    CheckpointTheater, ContainsCount, DatabaseEntry, Graph, Procedure, QueryStats, SchemaSnapshot,
    SnapshotPin,
};
pub use metrics::{
    ClauseMetricsRecord, FrontierCounters, KernelRung, PhaseTimes, PlanMetricsSink, StartReason,
    StoreReads, WalkStart,
};
// One public path (the graph module stays private): the whole-graph
// index-rebuild count the facade's reopen tests read to pin the
// empty-replay rebuild skip (LABS-1112). Test seam behind `test-hooks`,
// never in a shipped build.
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub use graph::take_index_rebuilds;
pub use query_engine::InMemoryQueryEngine;
// Research-only (speed-of-light kernels), quarantined behind the
// non-default `research` cargo feature (ADR-0476): the re-export (and the
// modules themselves) compile only under that feature.
#[cfg(feature = "research")]
#[doc(hidden)]
pub use graph::sol;
// Research-only heap-census harness (same quarantine, ADR-0476): the
// census example in relate-query-latency names `HeapCensus`; the nested
// types are re-exported so every public census field has a public type.
#[cfg(feature = "research")]
#[doc(hidden)]
pub use graph::census::{AdjacencyCensus, DeclaredCensus, HeapCensus};
pub use graph_type::GraphTypeSpec;
#[cfg(feature = "research")]
#[doc(hidden)]
pub use memory::ChainCensus;
pub use overlay::{ExecGraph, WriteScratch};
// The diagnostic switch's typed value (ADR-0519): re-exported for the
// `relate` crate's open path (the resolver and the installer below),
// the one public path — deliberately not advertised API beyond it.
pub use pattern_engine::PatternEngine;
// The ADR-0395 budget constant: workspace-internal (test-support's
// single-sourced assertion reads it), deliberately not advertised API —
// the budget is a fixed contract, not a knob (ADR-0395: "never ambient
// configuration").
#[doc(hidden)]
pub use overlay::NESTED_EVAL_BUDGET;
// The re-arrival rule's work budget (the ADR-0395 fixed-constant family,
// ADR-0516): workspace-internal (the PROFILE fixtures calibrate the budget
// fraction against it), deliberately not advertised API — a fixed
// contract, not a knob.
#[doc(hidden)]
pub use metrics::REARRIVAL_WORK_BUDGET;
// The ADR-0463 default per-allocation expression cap: workspace-internal
// (tests and the engine-default resolver read it), deliberately not
// advertised API. The operator knob lives on the open options, not here.
#[doc(hidden)]
pub use memory::EXPRESSION_BUDGET_BYTES;
pub use procedure_gate::procedure_preview_feature;
pub use procedure_registry::{
    builtin_procedure_deprecation, builtin_procedure_signature, call_is_write,
    is_ontology_procedure, is_ontology_store_op, is_ontology_write_procedure, is_void_procedure,
    ProcSignature,
};
pub use redo::{
    legacy_property_encoding, props_from_json, split_legacy_property_encoding, IndexDetailSpec,
    IndexFacts, IndexKindSpec, OntologyOp, PullKind, RedoOp, MAX_ONTOLOGY_SPEC_BYTES,
};
#[doc(hidden)]
pub use workspace::result_rows_bytes;
