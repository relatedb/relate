//! The `integration` integration-test binary: every test file that can
//! share a process with its siblings, compiled and linked once instead
//! of once per file.
//! A file stays a binary of its own directly under `tests/` when sharing a
//! process would change what it tests: it reads or sets the process-wide
//! preview opt-in; it mutates the process environment; it samples process-wide
//! memory; it re-executes its own binary; it owns the test crate root; or it is
//! listed in KEEP_ALWAYS (scripts/merge-test-binaries.py, the one table).
//! Add a new shareable test file here as a `mod` line; the
//! `every_file_is_declared` test fails on a file left out.

#[path = "../common/mod.rs"]
mod common;

mod aggregate_grouping_key_through_scalar_call;
mod aggregate_postfix;
mod aggregate_transparent_positions;
mod argument_error_classes;
mod bar_function;
mod batch_consumer;
mod bytes_value;
mod call_in_transactions_batch_pass;
mod cancellation;
mod canonical_registries;
mod case_expression;
mod checked_int_ops;
mod cip_240_search_filters;
mod cip_subqueries;
mod cip_value_operators;
mod civil_zoned_datetimes;
mod clause_projection_streams;
mod clone;
mod collect_list_concat;
mod comparison_semantics;
mod compile_time_error_classes;
mod comprehension_label_filter_map_expr;
mod comprehension_pipe_preference;
mod constraint_numeric_equivalence;
mod count_subquery_declaration;
mod cypher25_dialect;
mod cypher_version_preamble;
mod dbms_procedures;
mod ddl_error_classes;
mod ddl_result_shape;
mod distinct_bounded;
mod distinct_differential;
mod distinct_order_by_base_variable;
mod duration_fraction_truncation;
mod dynamic_operand_validation;
mod equality_postings_serve_labs1731;
mod explain_profile;
mod foreach_create_shadow;
mod fulltext_cjk;
mod fulltext_folding;
mod fulltext_query_strictness;
mod fulltext_search_semantics;
mod generator_batch_tail;
mod index_constraint_coexistence;
mod inf_nan_keywords;
mod integer_argument_error_classes;
mod invalid_unicode_character;
mod json_snapshot_recovery;
mod keyword_aliases;
mod keyword_label_names;
mod keyword_symbolic_names;
mod label_disjunction_seek_labs1083;
mod labs156_list_carry;
mod labs157_pattern_anchor;
mod labs1605_comprehension_map_projection;
mod labs1606_comprehension_type_predicate;
mod labs193_pattern_exists_probe;
mod labs246_pattern_exists_limit1;
mod labs249_pattern_probe_anonymous_endpoint;
mod labs250_in_list_node_seek;
mod labs255_258_expansion_order;
mod labs273_stats_orientation;
mod labs286_engine_elapsed;
mod labs290_column_types;
mod labs366_seek_right_property_in;
mod labs367_path_fold_pred_row_scope;
mod labs418_envelope_plan;
mod labs610_traversal_depth;
mod labs646_corpus_engine_pins;
mod labs680_safe_mode;
mod labs801_reltype_ids;
mod labs832_phase_envelope;
mod load_csv;
mod markdown_vault_e2e;
mod match_mode_bindings;
mod match_shape_validation;
mod misc_tail_forms;
mod not_parenthesized_operand;
mod notifications;
mod order_by_id_scan_order;
mod order_by_top_k_bounds;
mod parameter_missing;
mod parquet_wide_format_facade;
mod parser_syntax_gap_regressions;
mod pattern_predicate_param_map;
mod pattern_predicates;
mod pattern_prop_policy;
mod pipelined_write_batch;
mod point_index;
mod pokec_fixture;
mod predicate_list_coercion;
mod procedure_validation;
mod profile_frontier_metrics;
mod profile_kernel_rung;
mod profile_metrics;
mod profile_warnings;
mod projection_paren_transparency;
mod property_index_numeric_equivalence;
mod publish_race;
mod read_only;
mod recovery_fails_closed;
mod rel8_2030_value_ordering;
mod rel8_2076_row_bound_rel_fold;
mod return_item_column_names;
mod return_sharing;
mod round_precision;
mod run_script;
mod scalar_function_arity;
mod scalar_function_live_parity;
mod schema_procedures;
mod sealed_adjacency_reopen;
mod search_complex_patterns;
mod search_window;
mod shortest_path_end_predicate;
mod show_catalog_shape;
mod show_commands;
mod show_indexes_create_statement;
mod slice_bound_saturation;
mod source_unicode_escapes;
mod spaced_arrowhead;
mod spatial_argument_error_classes;
mod statement_notifications;
mod statement_notifications_dynamic_property;
mod statement_notifications_hints;
mod statement_notifications_optional;
mod statement_notifications_parameters;
mod statement_notifications_shapes;
mod statement_timeout;
mod string_arg_checks;
mod string_index_lazy;
mod subclause_aggregation;
mod temporal_constructor_arity;
mod temporal_identity_split;
mod temporal_map_field_policy;
mod temporal_pattern_safety;
mod to_integer_error_classes;
mod to_string_path_display;
mod transaction_clock;
mod tx_created_delete_stats;
mod type_error_classes;
mod typed_ir_exprs;
mod undefined_variable;
mod unicode_identifiers;
mod unwind_drain_charge;
mod unwind_source_memory_bounds;
mod user_alias_double_underscore;
mod uuid_label_predicates;
mod vector_quantization_type;
mod vector_search_candidates;
mod wal_payload_limit;
mod with_map_projection;
mod with_where_after_page;
mod write_barrier_charge;
mod write_effect;
mod write_validation;

/// A file under this directory that no `mod` line declares would
/// silently never compile or run.
#[test]
fn every_file_is_declared() {
    relate_test_support::test_binaries::assert_every_file_is_declared(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        "integration",
    );
}
