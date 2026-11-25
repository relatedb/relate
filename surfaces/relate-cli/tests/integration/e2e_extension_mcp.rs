//! The extension mount end-to-end (ADR-0442, ADR-0447): the compiled
//! `mcp-extension-fixture` child and the shell fixtures installed as
//! packages and driven through the real binary, under both commit policies.
//! The compiled fixture performs the whole surface in order (initialize and
//! its confirmation, update_schema, write_cypher with rows, read_cypher,
//! batch_cypher, report_output), so a green run proves the handshake, the
//! flag envelope, every tool including the multi-statement one, the commit
//! framing, the summary rendering, and the persisted store.
//!
//! The shell fixtures inline the bash MCP kit
//! (`docs/api/extensions/examples/mcp-client.sh`): `mcp_init` opens the
//! session and `mcp_call` round-trips a tool. A test that provokes a refusal sends the
//! offending line raw — a malformed line or a missing id cannot go through
//! a well-formed helper — and asserts the host's answer.
//!
//! The failure e2e drives a child that completes its tool calls and then
//! exits non-zero: the session's exit is classified inside the framed
//! import, so the run fails and a `whole-import` store stays empty — the
//! ordering the unit suite pins over scripted pipes.
//!
//! The unit suite (`src/extensions/mcp/tests.rs`) pins every refusal's
//! exact wording; this suite proves the real child and the real pipes.

#![cfg(feature = "extensions")]

use crate::support;

use std::path::Path;

use relate_wire::MAX_ROWS_PER_STATEMENT;

use support::extensions::{
    install_child_file, install_package, install_sh_child, mcp_child_body,
    mcp_extension_fixture_bin, xdg_root,
};
use support::{node_count, run_cli, tempdir, CliRunOptions as Opts};

/// The fixture package manifest: one integer flag driving the child's
/// row count, the MCP revision, and the given commit policy.
fn manifest(name: &str, executable: &str, commit: &str) -> String {
    format!(
        r#"{{"kind":"command","name":"{name}","version":"0.1.0","protocol":"2025-06-18","flags":[{{"kind":"integer","name":"count","default":3}}],"commit":"{commit}","executable":"{executable}"}}"#
    )
}

/// Install the compiled MCP fixture under `xdg` as the package `dir_name`
/// with the given mount name and commit policy.
fn install(xdg: &Path, dir_name: &str, name: &str, commit: &str) {
    let (bin, executable) = mcp_extension_fixture_bin();
    let package = install_package(xdg, dir_name, &manifest(name, &executable, commit));
    install_child_file(&package, &bin);
}

/// The stdout the fixture's `report_output` renders: the summary line, then
/// each starter indented two spaces.
fn reported(count: i64) -> String {
    format!("imported {count} nodes\n  MATCH (n:Greet) RETURN n.name AS name ORDER BY name\n")
}

/// Run the installed package `name` with `count` and return the CLI's
/// outcome plus the store path.
fn run_installed(
    package: &str,
    count: &str,
    database: &Path,
    xdg: &str,
    cwd: &Path,
) -> (std::process::ExitStatus, String, String) {
    run_cli(
        Opts::new(&[
            "run",
            package,
            "--count",
            count,
            "--database",
            database.to_str().unwrap(),
        ])
        .with_envs(&[("XDG_CONFIG_HOME", xdg)])
        .in_dir(cwd),
    )
}

/// Install a shell fixture package whose child is the bash kit plus `body`.
#[cfg(unix)]
fn install_shell_fixture(xdg: &Path, name: &str, commit: &str, body: &str) {
    let package = install_package(xdg, name, &manifest(name, "run.sh", commit));
    install_sh_child(&package, &mcp_child_body(name, body));
}

#[test]
fn the_mcp_child_runs_every_tool_under_each_commit_policy() {
    // One run per manifest commit policy: the same fixture, the same tools.
    // Per-batch commits each writing call; whole-import commits the
    // session's one transaction at its end — both persist the rows.
    for (policy, count) in [("per-batch", 4), ("whole-import", 2)] {
        let (tmp, xdg) = xdg_root();
        let name = format!("mcp-greet-{policy}");
        install(tmp.path(), &name, &name, policy);
        let cwd = tempdir();
        let db = cwd.path().join("store");
        let (status, stdout, stderr) =
            run_installed(&name, &count.to_string(), &db, &xdg, cwd.path());
        assert_eq!(status.code(), Some(0), "{policy}: {stderr}");
        // The fixture failed loudly on stderr if any tool round-trip broke;
        // a clean exit also means its own counts assertions held.
        assert_eq!(stdout, reported(count), "{policy}");
        // The fixture's three writing calls: the row batch plus the two
        // `batch_cypher` statements.
        assert_eq!(
            node_count(&db),
            count as u64 + 2,
            "{policy}: the rows and the batch persisted"
        );
    }
}

/// The child's script for the exit-1 regression: the handshake, one
/// `write_cypher` and `report_output`, then exit 1 — a clean request stream
/// followed by a failed exit.
#[cfg(unix)]
fn exit_after_tools() -> String {
    r#"mcp_call write_cypher '{"statement":"CREATE (n:Exit)"}' >/dev/null
mcp_call report_output '{"text":"about to fail"}' >/dev/null
exit 1"#
        .to_string()
}

/// A shell MCP child that provokes each pinned per-call refusal in turn
/// and asserts the host's answer: the exact JSON-RPC code and message
/// wording for a malformed line, an unknown method, an unknown tool, and a
/// cap exceeded. It exits 0 only when all four pairs matched, and 4 with
/// the offending response on stderr otherwise — the run's exit code is the
/// assertion, over the real pipes.
#[cfg(unix)]
fn pinned_refusals() -> String {
    format!(
        r#"expect() {{
  case "$1" in
    *"$2"*) ;;
    *) echo "the $3 refusal changed: $1" >&2; exit 4 ;;
  esac
}}
# 1. A line that is not JSON: a parse error, and the session continues.
printf 'this is not json\n'
IFS= read -r malformed
expect "$malformed" '"code":-32700' malformed-line
expect "$malformed" 'malformed JSON-RPC message: ' malformed-line
# 2. A method the mount does not serve.
MCP_ID=$((MCP_ID + 1))
printf '{{"jsonrpc":"2.0","id":%d,"method":"resources/list","params":{{}}}}\n' "$MCP_ID" >&3
IFS= read -r method
expect "$method" '"code":-32601' unknown-method
expect "$method" "unknown method 'resources/list'" unknown-method
# 3. A tool that is not one of the five.
MCP_ID=$((MCP_ID + 1))
printf '{{"jsonrpc":"2.0","id":%d,"method":"tools/call","params":{{"name":"drop_everything","arguments":{{}}}}}}\n' "$MCP_ID" >&3
IFS= read -r tool
expect "$tool" '"code":-32602' unknown-tool
expect "$tool" "unknown tool 'drop_everything'" unknown-tool
# 4. One row over the rows cap: refused before the engine.
rows=""
i=1
while [ "$i" -le {over} ]; do
  rows="$rows{{\"id\":$i}},"
  i=$((i + 1))
done
rows=${{rows%,}}
MCP_ID=$((MCP_ID + 1))
printf '{{"jsonrpc":"2.0","id":%d,"method":"tools/call","params":{{"name":"write_cypher","arguments":{{"statement":"UNWIND $rows AS row CREATE (n:Cap {{id: row.id}})","parameters":{{"rows":[%s]}}}}}}}}\n' "$MCP_ID" "$rows" >&3
IFS= read -r capped
expect "$capped" '"code":-32602' rows-cap
expect "$capped" 'the call carries {over} rows, over the {limit}-row cap' rows-cap
"#,
        over = MAX_ROWS_PER_STATEMENT + 1,
        limit = MAX_ROWS_PER_STATEMENT,
    )
}

/// The four pinned per-call refusals, asserted over the real binary and the
/// real pipes: a shell child sends one offending request per refusal and
/// checks the host's code and wording. The unit suite pins the same texts
/// against scripted pipes; this proves they survive the process boundary.
#[cfg(unix)]
#[test]
fn the_pinned_refusals_are_answered_over_the_real_pipes() {
    let (tmp, xdg) = xdg_root();
    install_shell_fixture(tmp.path(), "mcp-refusals", "per-batch", &pinned_refusals());
    let cwd = tempdir();
    let db = cwd.path().join("store");
    let (status, stdout, stderr) = run_installed("mcp-refusals", "0", &db, &xdg, cwd.path());
    assert_eq!(
        status.code(),
        Some(0),
        "the child exits 4 naming the refusal that drifted: {stderr}"
    );
    assert_eq!(stdout, "", "a child without a summary prints nothing");
    assert_eq!(node_count(&db), 0, "no refused call reached the graph");
}

/// A shell MCP child that sends one `batch_cypher` call whose second
/// statement fails: `CREATE (n:Second {id: $missing})` cannot bind
/// `$missing`. It exits 0 when the host answered the pinned refusal and 3
/// when it did not, so a run's exit code says whether the per-batch session
/// kept its per-call promise.
#[cfg(unix)]
fn failing_batch() -> String {
    r#"MCP_ID=$((MCP_ID + 1))
printf '{"jsonrpc":"2.0","id":%d,"method":"tools/call","params":{"name":"batch_cypher","arguments":{"statements":[{"statement":"CREATE (n:First)"},{"statement":"CREATE (n:Second {id: $missing})"}]}}}\n' "$MCP_ID" >&3
IFS= read -r batch_response
case "$batch_response" in
  *'"code":-32602'*) exit 0 ;;
  *) exit 3 ;;
esac"#
        .to_string()
}

#[cfg(unix)]
#[test]
fn a_clean_stream_then_a_failed_exit_fails_the_run_and_rolls_back_whole_import() {
    // The child completes its tool calls (the writes are inside the
    // session's one transaction) and then exits 1. The exit is classified
    // inside that framing, so the run fails and nothing is committed: a
    // committed store behind a failed run would be a silent success.
    let (tmp, xdg) = xdg_root();
    install_shell_fixture(tmp.path(), "mcp-exit", "whole-import", &exit_after_tools());
    let cwd = tempdir();
    let db = cwd.path().join("store");
    let (status, _stdout, stderr) = run_installed("mcp-exit", "0", &db, &xdg, cwd.path());
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("Error: the extension child exited with 1"),
        "{stderr}"
    );
    assert_eq!(
        node_count(&db),
        0,
        "whole-import must roll the session back"
    );
}

/// Under per-batch a failing statement rolls the whole call back and the
/// refusal is answered: the session finishes cleanly, the first statement of
/// the call is gone with it, and the child saw the pinned code.
#[cfg(unix)]
#[test]
fn a_failed_batch_rolls_back_alone_under_per_batch() {
    let (tmp, xdg) = xdg_root();
    install_shell_fixture(
        tmp.path(),
        "mcp-batch-per-batch",
        "per-batch",
        &failing_batch(),
    );
    let cwd = tempdir();
    let db = cwd.path().join("store");
    let (status, _stdout, stderr) =
        run_installed("mcp-batch-per-batch", "0", &db, &xdg, cwd.path());
    assert_eq!(
        status.code(),
        Some(0),
        "the child exits 3 when the -32602 refusal never arrived: {stderr}"
    );
    assert_eq!(node_count(&db), 0, "the call's first statement rolled back");
}

/// Under whole-import the session's one transaction cannot roll back a
/// single call, so the failing batch ends the run and nothing commits — the
/// host never answers a continuation the framing cannot keep.
#[cfg(unix)]
#[test]
fn a_failed_batch_ends_the_whole_import_run() {
    let (tmp, xdg) = xdg_root();
    install_shell_fixture(
        tmp.path(),
        "mcp-batch-whole",
        "whole-import",
        &failing_batch(),
    );
    let cwd = tempdir();
    let db = cwd.path().join("store");
    let (status, _stdout, stderr) = run_installed("mcp-batch-whole", "0", &db, &xdg, cwd.path());
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(
            "Error: the extension MCP batch failed inside the whole-import transaction: "
        ),
        "{stderr}"
    );
    assert_eq!(node_count(&db), 0, "the whole import rolled back");
}

/// A child that takes its envelope and then writes nothing: the mount's read
/// deadline names the bound in a typed refusal instead of hanging the verb.
/// The child `exec`s into the sleep, so the run's kill reaps the very process
/// the deadline convicted.
#[cfg(unix)]
#[test]
fn a_silent_child_fails_the_run_on_the_read_deadline() {
    let (tmp, xdg) = xdg_root();
    let package = install_package(
        tmp.path(),
        "stalled-mcp",
        &manifest("stalled-mcp", "run.sh", "per-batch"),
    );
    install_sh_child(&package, "read line\nexec sleep 60\n");
    let cwd = tempdir();
    let db = cwd.path().join("store");
    let (status, _stdout, stderr) = run_installed("stalled-mcp", "0", &db, &xdg, cwd.path());
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("Error: the extension wrote no MCP request within 10s"),
        "{stderr}"
    );
    assert_eq!(node_count(&db), 0, "nothing was imported");
}

#[cfg(unix)]
#[test]
fn an_mcp_child_without_a_handshake_fails_closed() {
    // A package whose executable emits nothing and exits: the session never
    // initializes, and the host refuses with the pinned text instead of
    // treating an empty stream as success. The child must not read stdin to
    // end-of-input — the host keeps it open for responses, so a `cat`
    // fixture would wait forever (the stall the request stream's end
    // classification exists for).
    let (tmp, xdg) = xdg_root();
    let package = install_package(
        tmp.path(),
        "silent-mcp",
        &manifest("silent-mcp", "run.sh", "per-batch"),
    );
    // It reads the one envelope line (so the host's write cannot race its
    // exit) and emits nothing.
    install_sh_child(&package, "read line\nexit 0");
    let cwd = tempdir();
    let db = cwd.path().join("store");
    let (status, _stdout, stderr) = run_installed("silent-mcp", "0", &db, &xdg, cwd.path());
    assert_eq!(status.code(), Some(1));
    assert!(
        stderr.contains(
            "Error: the extension child ended its MCP session without an initialize request"
        ),
        "{stderr}"
    );
}
