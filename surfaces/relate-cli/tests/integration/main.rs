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

#[path = "../support/mod.rs"]
mod support;

mod e2e_agent_context;
mod e2e_agent_mode;
mod e2e_bundles;
mod e2e_cli;
mod e2e_clone;
mod e2e_cold_start;
mod e2e_command_suggestion;
mod e2e_commit_cap;
mod e2e_durability;
mod e2e_eggs;
mod e2e_eggs_graph;
mod e2e_envelope;
mod e2e_errors;
mod e2e_exit_open_transaction;
mod e2e_extension_check;
mod e2e_extension_fixtures;
mod e2e_extension_init;
mod e2e_extension_mcp;
mod e2e_extensions;
mod e2e_features;
mod e2e_feedback;
mod e2e_fetch_params;
mod e2e_flags;
mod e2e_fs_to_graph;
mod e2e_git_to_graph;
mod e2e_help;
mod e2e_importers_catalog;
mod e2e_info;
mod e2e_jsonl;
mod e2e_lab_retired;
mod e2e_load;
mod e2e_load_csv;
mod e2e_notifications;
mod e2e_output_safety;
mod e2e_profile_memo;
mod e2e_pull;
mod e2e_push;
mod e2e_repl;
mod e2e_row_cap;
mod e2e_run;
mod e2e_run_from_url;
mod e2e_serve;
mod e2e_serve_bolt;
mod e2e_serve_durability;
mod e2e_serve_read_only;
mod e2e_skill;
mod e2e_speedscope;
mod e2e_statement_progress;
mod e2e_syntax_position;
mod e2e_telemetry;
mod e2e_timeout;
mod e2e_upgrade;
mod e2e_upgrade_route;
mod params;
mod wire_golden;

/// A file under this directory that no `mod` line declares would
/// silently never compile or run.
#[test]
fn every_file_is_declared() {
    relate_test_support::test_binaries::assert_every_file_is_declared(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        "integration",
    );
}
