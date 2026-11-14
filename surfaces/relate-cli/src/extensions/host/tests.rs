//! The extension host's behavior tests, in the dir-module pattern the
//! `lab` tree and the wire's own protocol module use (LABS-421): the
//! argv parse, the help rendering, the pinned refusal texts, and the
//! driver that executes admitted batches against a host-owned store —
//! with POSIX `sh` fixture children where the wire is exercised, gated
//! to unix lanes like the protocol suite's own. The fixtures come from
//! the extension tree's shared kit (`extensions::testkit`).

use super::*;
use crate::args;
use crate::extensions::protocol::{Caps, ProtocolError};
use crate::extensions::testkit::{
    extension, installed, manifest_for, registry_with, EVERY_KIND_FLAGS,
};
use crate::extensions::write;
use crate::parse_error::REMOVED_DB_SPELLING;

// ---------------------------------------------------------------------
// The argv parse (every platform)
// ---------------------------------------------------------------------

#[test]
fn parses_every_flag_kind_with_its_value_and_keeps_the_envelope_host_free() {
    let (registry, _tmp) = registry_with(
        "greet",
        EVERY_KIND_FLAGS,
        r#","positional": {"name": "DIR"}"#,
    );
    let entry = extension(&registry, "greet");
    let run = parse_action_args(
        entry,
        &args(&[
            "DIR",
            "--message",
            "hi",
            "--count",
            "7",
            "--shout",
            "--tag",
            "x",
            "--tag",
            "y",
        ]),
    )
    .unwrap();
    // The child's envelope: the resolved values, keyed by the manifest's
    // own names; the positional under its declared name.
    assert_eq!(
        run.envelope,
        serde_json::json!({
            "message": "hi",
            "count": 7,
            "shout": true,
            "tag": ["a", "b", "x", "y"],
            "DIR": "DIR",
        })
        .as_object()
        .unwrap()
        .clone()
    );
    // The host resolves the store target itself: no argv spelling of it
    // reaches the envelope (ADR-0430 §6).
    assert!(run.db.is_none());
}

#[test]
fn absent_flags_resolve_to_their_typed_defaults() {
    let (registry, _tmp) = registry_with(
        "greet",
        EVERY_KIND_FLAGS,
        r#","positional": {"name": "DIR"}"#,
    );
    let entry = extension(&registry, "greet");
    let run = parse_action_args(entry, &[]).unwrap();
    assert_eq!(
        run.envelope,
        serde_json::json!({
            "message": "hello",
            "count": 3,
            "shout": false,
            "tag": ["a", "b"],
        })
        .as_object()
        .unwrap()
        .clone()
    );
}

#[test]
fn the_store_target_resolves_host_side_under_both_spellings() {
    let (registry, _tmp) = registry_with("greet", EVERY_KIND_FLAGS, "");
    let entry = extension(&registry, "greet");
    for flag in ["--database", "-d"] {
        let run = parse_action_args(entry, &args(&[flag, "./graph"])).unwrap();
        assert_eq!(run.db.as_deref(), Some("./graph"), "{flag}");
        // The store path never reaches the child (ADR-0430 §6).
        assert!(!run.envelope.contains_key("database"));
    }
}

#[test]
fn the_removed_db_spelling_keeps_its_no_hint_rejection() {
    let (registry, _tmp) = registry_with("greet", EVERY_KIND_FLAGS, "");
    let entry = extension(&registry, "greet");
    let err = parse_action_args(entry, &args(&[REMOVED_DB_SPELLING, "./graph"])).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("unexpected argument to run greet: {REMOVED_DB_SPELLING}")
    );
    let candidates = flag_candidates(entry);
    let refs: Vec<&str> = candidates.iter().map(|c| c.as_str()).collect();
    assert_eq!(
        err.suggestion(&refs, &[]),
        None,
        "the removed spelling must not hint at its replacement"
    );
}

#[test]
fn an_unknown_flag_refuses_in_the_actions_scope_and_suggests_from_its_own_table() {
    let (registry, _tmp) = registry_with("greet", EVERY_KIND_FLAGS, "");
    let entry = extension(&registry, "greet");
    let err = parse_action_args(entry, &args(&["--mesage", "hi"])).unwrap_err();
    assert_eq!(
        err.to_string(),
        "unexpected argument to run greet: --mesage"
    );
    // The did-you-mean candidates are the same flag list the grammar
    // accepts (ADR-0230's derive-from-the-one-table rule): the manifest's
    // own flags plus the host's store spellings.
    let candidates = flag_candidates(entry);
    assert_eq!(
        candidates,
        [
            "--message",
            "--count",
            "--shout",
            "--tag",
            "-d",
            "--database"
        ]
    );
    let refs: Vec<&str> = candidates.iter().map(|c| c.as_str()).collect();
    assert_eq!(err.suggestion(&refs, &[]), Some("--message"));
}

#[test]
fn a_value_that_does_not_parse_as_its_kind_names_the_flag_and_the_value() {
    let (registry, _tmp) = registry_with("greet", EVERY_KIND_FLAGS, "");
    let entry = extension(&registry, "greet");
    let err = parse_action_args(entry, &args(&["--count", "x"])).unwrap_err();
    assert_eq!(err.to_string(), "invalid --count: 'x' is not an integer");
    // A missing value is the shared requires text.
    let missing = parse_action_args(entry, &args(&["--count"])).unwrap_err();
    assert_eq!(missing.to_string(), "--count requires a number");
}

#[test]
fn the_positional_is_one_at_most_and_only_when_declared() {
    let (registry, _tmp) = registry_with(
        "greet",
        EVERY_KIND_FLAGS,
        r#","positional": {"name": "DIR"}"#,
    );
    let entry = extension(&registry, "greet");
    let two = parse_action_args(entry, &args(&["one", "two"])).unwrap_err();
    assert_eq!(two.to_string(), "unexpected argument to run greet: two");
    let (registry_none, _tmp) = registry_with("greet", EVERY_KIND_FLAGS, "");
    let takes_none = extension(&registry_none, "greet");
    let stray = parse_action_args(takes_none, &args(&["one"])).unwrap_err();
    assert_eq!(stray.to_string(), "unexpected argument to run greet: one");
}

#[test]
fn a_manifest_protocol_this_host_does_not_speak_refuses_naming_both() {
    let (registry, _tmp) = installed(
        &manifest_for("greet", "", "").replacen(
            r#""protocol":"2025-06-18""#,
            r#""protocol":"1999-01-01""#,
            1,
        ),
        "",
    );
    let entry = registry.find("greet").expect("the fixture must resolve");
    let err = parse_action_args(entry, &[]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "extension 'greet' declares protocol '1999-01-01'; this host speaks '2025-06-18'"
    );
}

#[test]
fn a_duplicate_flag_name_refuses_the_parse() {
    let duplicate = r#"{"kind": "string", "name": "dup", "default": "x"},
        {"kind": "integer", "name": "dup", "default": 1}"#;
    let (registry, _tmp) = registry_with("greet", duplicate, "");
    let entry = extension(&registry, "greet");
    let err = parse_action_args(entry, &[]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "extension 'greet' declares the flag 'dup' twice"
    );
}

/// A positional name that collides with a flag name refuses at manifest
/// check time, shaped like the duplicate-flag text: the envelope keys
/// the positional and the flags by name, so a shared name would shadow
/// one with the other.
#[test]
fn a_positional_name_colliding_with_a_flag_name_refuses_the_parse() {
    let flags = r#"{"kind": "string", "name": "DIR", "default": "x"}"#;
    let (registry, _tmp) = registry_with("greet", flags, r#","positional": {"name": "DIR"}"#);
    let entry = extension(&registry, "greet");
    let err = parse_action_args(entry, &[]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "extension 'greet' declares 'DIR' twice (as its positional and as a flag)"
    );
}

#[test]
fn a_flag_spelling_the_host_owns_refuses_the_parse() {
    for owned in ["database", "db"] {
        let flags = format!(r#"{{"kind": "string", "name": "{owned}", "default": "x"}}"#);
        let (registry, _tmp) = registry_with("greet", &flags, "");
        let entry = extension(&registry, "greet");
        let err = parse_action_args(entry, &[]).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "extension 'greet' declares the flag '{owned}', which collides \
                 with the host's own store flag (-d/--database)"
            )
        );
    }
}

#[test]
fn a_hidden_extensions_flags_never_reach_did_you_mean() {
    let (registry, _tmp) = registry_with("greet", EVERY_KIND_FLAGS, r#","hidden": true"#);
    let entry = extension(&registry, "greet");
    // The action still parses and runs (ADR-0429 §2) — only its listing
    // and its flag suggestions hide.
    assert!(parse_action_args(entry, &args(&["--message", "hi"])).is_ok());
    assert_eq!(
        flag_candidates(entry),
        ["-d".to_string(), "--database".to_string()]
    );
}

// ---------------------------------------------------------------------
// The help text (every platform)
// ---------------------------------------------------------------------

#[test]
fn the_usage_line_renders_from_the_table() {
    let (registry, _tmp) = registry_with(
        "greet",
        EVERY_KIND_FLAGS,
        r#","positional": {"name": "DIR"}"#,
    );
    let entry = extension(&registry, "greet");
    assert_eq!(
        action_usage(entry),
        "Usage: relate run greet [DIR] [--message VALUE] [--count N] \
         [--shout] [--tag VALUE]"
    );
}

#[test]
fn the_help_page_documents_the_whole_grammar_and_the_shared_store_block() {
    let (registry, _tmp) = registry_with(
        "greet",
        EVERY_KIND_FLAGS,
        r#","positional": {"name": "DIR"}"#,
    );
    let entry = extension(&registry, "greet");
    let page = action_help(entry);
    // The synopsis opens the page (one Usage: line overall).
    assert!(page.starts_with(&action_usage(entry)), "{page}");
    assert_eq!(page.matches("Usage: relate").count(), 1, "{page}");
    // The shared -d/--database block (the one store-path fact every
    // import grammar documents) renders on the extension's page too.
    for line in crate::help::DATABASE_FLAG_HELP {
        assert!(page.contains(line), "{line:?} missing:\n{page}");
    }
    // Every flag the grammar accepts is documented with its kind and its
    // default.
    for (token, description) in [
        ("--message VALUE", "A string flag (default 'hello')"),
        ("--count N", "An integer flag (default 3)"),
        ("--shout", "A boolean flag (default false)"),
        ("--tag VALUE", "A repeatable string flag (default 'a', 'b')"),
    ] {
        assert!(page.contains(token), "{token} missing:\n{page}");
        assert!(page.contains(description), "{description} missing:\n{page}");
    }
    assert!(page.contains("The action's positional"), "{page}");
}

#[test]
fn a_manifest_fragment_cannot_forge_structure_into_the_page() {
    // A default carrying control characters renders flattened — the
    // sanitizer's one-line rule applied to the page's fragments, so a
    // package author cannot forge lines, ANSI, or control characters
    // into the host's output (ADR-0430 §5).
    let flags = r#"{"kind": "string", "name": "banner", "default": "a\tb"}"#;
    let (registry, _tmp) = registry_with("noisy", flags, "");
    let entry = extension(&registry, "noisy");
    let page = action_help(entry);
    assert!(
        !page.contains('\t') && !page.contains('\u{1b}'),
        "control characters reached the page raw:\n{page:?}"
    );
    assert!(
        page.contains("a b"),
        "the tab flattened to a space:\n{page}"
    );
}

#[test]
fn requires_fresh_store_is_stated_on_the_page() {
    let (registry, _tmp) = registry_with("greet", "", r#","requires-fresh-store": true"#);
    let entry = extension(&registry, "greet");
    let page = action_help(entry);
    assert!(
        page.contains("requires a fresh store"),
        "the fresh-store requirement must be stated:\n{page}"
    );
}

// ---------------------------------------------------------------------
// Pinned refusal texts (every platform)
// ---------------------------------------------------------------------

#[test]
fn every_host_refusal_renders_its_pinned_text() {
    assert_eq!(
        HostError::Wire(ProtocolError::ExitFailed {
            code: 1,
            stderr: "boom".to_string()
        })
        .to_string(),
        "the extension child exited with 1: boom"
    );
    assert_eq!(
        HostError::FreshStoreRequired {
            name: "greet".into(),
            nodes: 3,
        }
        .to_string(),
        "the extension 'greet' requires a fresh store, but the target holds 3 nodes"
    );
    assert_eq!(
        HostError::Mcp(crate::extensions::mcp::McpError::InitializeMissing).to_string(),
        "the extension child ended its MCP session without an initialize request"
    );
}

/// The engine's typed text rides the Engine carry byte for byte (the
/// `ImportError::Engine` rule, ADR-0392) — whatever the engine renders,
/// unreframed.
#[test]
fn an_engine_error_rides_the_engine_carry_byte_for_byte() {
    let engine = relate::Error::capacity(100, 64);
    assert_eq!(
        HostError::Engine {
            error: engine.clone()
        }
        .to_string(),
        engine.to_string()
    );
}

/// The manifest commit policy is the wire kit's one vocabulary (ADR-0447
/// §3): the serde decode accepts exactly its two spellings, and the
/// `initialize` result publishes the same words.
#[test]
fn the_commit_policy_is_the_kits_commit_facts() {
    use relate_wire::mcp::CommitFacts;
    for (spelling, facts) in [
        (relate_wire::PER_BATCH, CommitFacts::PerBatch),
        (relate_wire::WHOLE_IMPORT, CommitFacts::WholeImport),
    ] {
        let decoded: CommitFacts = serde_json::from_str(&format!("\"{spelling}\"")).unwrap();
        assert_eq!(decoded, facts);
        assert_eq!(facts.spelling(), spelling);
    }
}

// ---------------------------------------------------------------------
// The driver: one MCP session against a host-owned store
// (unix lanes: the fixture children, like the protocol suite's own)
// ---------------------------------------------------------------------

#[cfg(unix)]
mod driver {
    use super::*;
    use crate::extensions::mcp::McpError;
    use crate::extensions::testkit::mcp_child_body;

    /// The manifest with `commit`'s policy instead of the default.
    fn manifest_under(commit: &str) -> String {
        manifest_for("greet", "", "").replacen(
            "\"commit\":\"per-batch\"",
            &format!("\"commit\":\"{commit}\""),
            1,
        )
    }

    /// Drive `body`'s child through the real mount for the
    /// manifest-declared policy, returning the session's outcome and the
    /// store's connection for the assertions. The child runs pinned to the
    /// fixture tempdir (the spawn-cwd rule: no transient process-cwd swap
    /// reaches the child).
    fn drive_body(
        manifest_text: &str,
        body: &str,
    ) -> (Result<Option<RunSummary>, HostError>, relate::Connection) {
        let (registry, tmp) = installed(manifest_text, &mcp_child_body("fixture-pkg", body));
        let entry = registry.find("greet").expect("the fixture must resolve");
        let db = relate::Database::open(":memory:").unwrap();
        let mut conn = db.connect_typed(relate::Namespace::Neo4j).unwrap();
        let exe = entry.package_dir.join("run.sh");
        let process = crate::extensions::mcp::process::spawn(
            &exe,
            &serde_json::Map::new(),
            Caps::MOUNT,
            entry.command(),
            Some(tmp.path()),
        )
        .expect("the fixture child spawns");
        let outcome = process.drive(&db, &mut conn);
        (outcome, conn)
    }

    /// The store's node count, read the way the freshness read reads it.
    fn nodes(conn: &mut relate::Connection) -> i64 {
        write::node_count(conn).unwrap()
    }

    /// One `write_cypher` call writing `ids`' `:N` nodes.
    fn write_nodes(ids: &[i64]) -> String {
        let rows: Vec<String> = ids.iter().map(|id| format!("{{\"id\": {id}}}")).collect();
        format!(
            "mcp_call write_cypher '{{\"statement\":\"UNWIND $rows AS row CREATE (n:N {{id: row.id}})\",\"parameters\":{{\"rows\":[{}]}}}}' >/dev/null",
            rows.join(",")
        )
    }

    #[test]
    fn a_clean_session_writes_the_calls_and_returns_the_summary() {
        let (outcome, mut conn) = drive_body(
            &manifest_for("greet", "", ""),
            &format!(
                "{}\n{}\nmcp_call report_output '{{\"text\":\"imported\",\"starters\":[\"MATCH (n:N) RETURN count(n)\"]}}' >/dev/null",
                write_nodes(&[1, 2]),
                write_nodes(&[3]),
            ),
        );
        // Every call lands (3 nodes), and the summary is what
        // `report_output` carried.
        assert_eq!(nodes(&mut conn), 3);
        let summary = outcome.unwrap().expect("the child reported a summary");
        assert_eq!(summary.text, "imported");
        assert_eq!(summary.starters, ["MATCH (n:N) RETURN count(n)"]);
    }

    #[test]
    fn a_session_without_a_report_succeeds_with_no_summary() {
        let (outcome, mut conn) = drive_body(&manifest_for("greet", "", ""), &write_nodes(&[5]));
        assert_eq!(nodes(&mut conn), 1);
        assert_eq!(outcome.unwrap(), None);
    }

    #[test]
    fn per_batch_commits_the_calls_before_a_failure() {
        // ADR-0392: every call before a failure stays committed. The
        // child's non-zero exit ends the run after one good call.
        let (outcome, mut conn) = drive_body(
            &manifest_for("greet", "", ""),
            &format!("{}\nexit 1", write_nodes(&[1, 2])),
        );
        assert_eq!(
            outcome.unwrap_err(),
            HostError::Wire(ProtocolError::ExitFailed {
                code: 1,
                stderr: String::new(),
            })
        );
        // The first call stayed committed.
        assert_eq!(nodes(&mut conn), 2);
    }

    #[test]
    fn whole_import_rolls_everything_back_on_a_failure() {
        // ADR-0157: one explicit transaction around the whole import — the
        // child's failed exit after a good call rolls it all back.
        let (outcome, mut conn) = drive_body(
            &manifest_under("whole-import"),
            &format!("{}\nexit 1", write_nodes(&[1, 2])),
        );
        assert!(matches!(outcome.unwrap_err(), HostError::Wire(_)));
        assert_eq!(nodes(&mut conn), 0, "the whole import must roll back");
    }

    #[test]
    fn a_clean_whole_import_commits_with_the_summary() {
        let (outcome, mut conn) = drive_body(
            &manifest_under("whole-import"),
            &format!(
                "{}\nmcp_call report_output '{{\"text\":\"done\"}}' >/dev/null",
                write_nodes(&[1, 2, 3]),
            ),
        );
        assert!(outcome.unwrap().is_some());
        assert_eq!(nodes(&mut conn), 3);
    }

    #[test]
    fn a_update_schema_runs_at_the_point_the_child_placed_it() {
        // A `update_schema` before the first writing call runs before it (the
        // range-index shape), and the index it created is queryable after.
        let (outcome, mut conn) = drive_body(
            &manifest_for("greet", "", ""),
            &format!(
                "mcp_call update_schema '{{\"statement\":\"CREATE INDEX n_id IF NOT EXISTS FOR (n:N) ON (n.id)\"}}' >/dev/null\n{}\nmcp_call report_output '{{\"text\":\"indexed\"}}' >/dev/null",
                write_nodes(&[1]),
            ),
        );
        assert!(outcome.unwrap().is_some(), "the session ran clean");
        let created = conn
            .query("SHOW INDEXES YIELD name")
            .expect("the store lists its indexes");
        assert!(
            !created.rows.is_empty(),
            "the index the child created must exist"
        );
    }

    #[test]
    fn a_update_schema_the_engine_refuses_is_a_per_call_refusal() {
        // A malformed DDL is answered as the mount's per-call engine
        // refusal: the child reads `-32602` and the session can continue —
        // the fixture exits 0 only when the refusal arrived, so the run's
        // exit code is the assertion.
        let (outcome, _conn) = drive_body(
            &manifest_for("greet", "", ""),
            r#"MCP_ID=$((MCP_ID + 1))
printf '{"jsonrpc":"2.0","id":%d,"method":"tools/call","params":{"name":"update_schema","arguments":{"statement":"CREATE INDEX nope FOR"}}}\n' "$MCP_ID" >&3
IFS= read -r response
case $response in
  *'"code":-32602'*) ;;
  *) exit 4 ;;
esac"#,
        );
        assert!(outcome.is_ok(), "the refused call is answered, not fatal");
        assert_eq!(
            relate_mcp::tools::ToolError::SchemaOnly.to_string(),
            "Only DDL queries are allowed for update-schema"
        );
    }

    #[test]
    fn a_request_without_an_id_is_the_mounts_fatal_refusal() {
        // A `tools/call` with no id is a request the host can never answer:
        // the session fails instead of hanging the child (ADR-0442 §3).
        let (outcome, _conn) = drive_body(
            &manifest_for("greet", "", ""),
            r#"printf '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"report_output","arguments":{"text":"x"}}}\n' >&3"#,
        );
        assert_eq!(
            outcome.unwrap_err(),
            HostError::Mcp(McpError::RequestWithoutId {
                method: "tools/call".to_string()
            })
        );
    }

    #[test]
    fn a_malformed_line_is_answered_and_the_session_continues() {
        // A line that is not JSON is a `-32700` answer, never a fatal
        // refusal: the child can correct itself (the fixture asserts the
        // code and then reports a summary, which the host prints).
        let (outcome, _conn) = drive_body(
            &manifest_for("greet", "", ""),
            r#"printf 'this is not json\n'
IFS= read -r malformed
case $malformed in
  *'"code":-32700'*) ;;
  *) exit 4 ;;
esac
mcp_call report_output '{"text":"recovered"}' >/dev/null"#,
        );
        let summary = outcome.unwrap().expect("the session reached its summary");
        assert_eq!(summary.text, "recovered");
    }

    #[test]
    fn a_row_that_does_not_decode_refuses_before_the_write() {
        // A parameter row carrying an integer past i64's range does not
        // decode as a parameter value, so the call is refused as invalid
        // params before the store write it would have fed.
        let (outcome, mut conn) = drive_body(
            &manifest_for("greet", "", ""),
            r#"MCP_ID=$((MCP_ID + 1))
printf '{"jsonrpc":"2.0","id":%d,"method":"tools/call","params":{"name":"write_cypher","arguments":{"statement":"UNWIND $rows AS row CREATE (n:N {id: row.id})","parameters":{"rows":[{"id":18446744073709551615}]}}}}\n' "$MCP_ID" >&3
IFS= read -r response
case $response in
  *'"code":-32602'*) ;;
  *) exit 4 ;;
esac"#,
        );
        assert!(outcome.is_ok(), "the bad row is a per-call refusal");
        assert_eq!(nodes(&mut conn), 0, "the bad row's call never writes");
    }
}

// ---------------------------------------------------------------------
// The run lifecycle (unix: a real child through `run_action`)
// ---------------------------------------------------------------------

#[cfg(unix)]
mod lifecycle {
    use super::*;
    use crate::extensions::testkit::mcp_child_body;

    /// The full `run <name>` end-to-end: parse `argv`, run the action
    /// against the parsed target, and return the exit code with the
    /// target the host left behind.
    fn run_end_to_end(manifest_text: &str, body: &str, argv: &[&str]) -> (i32, Option<String>) {
        let (registry, _tmp) = installed(manifest_text, &mcp_child_body("fixture-pkg", body));
        let entry = registry.find("greet").expect("the fixture must resolve");
        let run = parse_action_args(entry, &args(argv)).expect("the fixture argv parses");
        let target = run.db.clone();
        let mut telemetry = crate::telemetry::Telemetry::new();
        let code = run_action(entry, run, &mut telemetry);
        (code, target)
    }

    /// The store's node count, freshly opened the way a caller opens the
    /// target the import left behind.
    fn nodes(target: &str) -> i64 {
        let db = relate::Database::open(target).unwrap();
        let mut conn = db.connect_typed(relate::Namespace::Neo4j).unwrap();
        write::node_count(&mut conn).unwrap()
    }

    /// The clean-run body: one writing call of two rows, then the summary.
    fn clean_body() -> String {
        r#"mcp_call write_cypher '{"statement":"UNWIND $rows AS row CREATE (n:N {id: row.id})","parameters":{"rows":[{"id":1},{"id":2}]}}' >/dev/null
mcp_call report_output '{"text":"imported 2"}' >/dev/null"#
            .to_string()
    }

    #[test]
    fn a_persistent_target_is_written_and_the_run_exits_zero() {
        let store = tempfile::tempdir().unwrap();
        let target = store.path().join("graph").display().to_string();
        let (code, written) = run_end_to_end(
            &manifest_for("greet", "", ""),
            &clean_body(),
            &["--database", &target],
        );
        assert_eq!(code, 0);
        assert_eq!(written.as_deref(), Some(target.as_str()));
        // The store the host left behind: the target a caller opens.
        assert_eq!(nodes(&target), 2);
    }

    #[test]
    fn an_omitted_target_is_the_memory_default() {
        let (code, target) = run_end_to_end(&manifest_for("greet", "", ""), &clean_body(), &[]);
        assert_eq!(code, 0);
        assert!(target.is_none(), "the default is :memory:, never a path");
    }

    #[test]
    fn a_missing_executable_is_the_spawns_typed_refusal() {
        // The registry's load-time check is lexical; existence is
        // spawn-time (ADR-0429 §2) — the child that never was, typed.
        let (registry, _tmp) = installed(&manifest_for("greet", "", ""), "");
        let entry = registry.find("greet").expect("the fixture must resolve");
        let run = parse_action_args(entry, &[]).unwrap();
        let mut telemetry = crate::telemetry::Telemetry::new();
        assert_eq!(run_action(entry, run, &mut telemetry), 1);
    }

    #[test]
    fn requires_fresh_store_refuses_a_populated_target_and_costs_no_child() {
        let store = tempfile::tempdir().unwrap();
        let target = store.path().join("graph").display().to_string();
        let manifest = manifest_for("greet", "", r#","requires-fresh-store": true"#);
        // A fresh target runs (ADR-0394's read agrees): the import lands.
        let (code, _) = run_end_to_end(&manifest, &clean_body(), &["--database", &target]);
        assert_eq!(code, 0);
        assert_eq!(nodes(&target), 2);
        // The same target again: the refusal, before the child spawns —
        // the child's call would have grown the store, so an unchanged
        // count is the proof nothing ran.
        let (code, _) = run_end_to_end(&manifest, &clean_body(), &["--database", &target]);
        assert_eq!(code, 1);
        assert_eq!(nodes(&target), 2, "the non-fresh target must be refused");
    }
}
