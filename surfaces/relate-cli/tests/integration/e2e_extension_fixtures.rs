//! Fixture extensions end-to-end (ADR-0429's Consequences, ADR-0447,
//! LABS-470): the spawn/isolate/assert harness that runs real installed
//! extensions through the real binary — the language-agnosticism pair. A
//! compiled Rust child (the `mcp-extension-fixture` bin of this package) and
//! a shell child (the bash MCP kit, unix lanes, like every
//! process-driving suite) install as packages under a tempdir
//! `$XDG_CONFIG_HOME` and drive the same manifest grammar through the same
//! wire: `run <name>` imports end-to-end, the flag values the host resolved
//! reach the child in its envelope and drive its rows, an explicit
//! `--database` target persists the store and the run exits, and the
//! collision refusals pin their texts through the real process.
//!
//! The REPL handoff half of the `:memory:` contract is the shared ADR-0286
//! machinery (`session::run_import_interactive`) the lab PTY suites already
//! drive on a real terminal; under this harness — a piped, non-TTY spawn —
//! the asserted branch is the non-TTY exit path: the run prints the child's
//! summary and exits 0, leaving no store behind.

#![cfg(feature = "extensions")]

use crate::support;

use std::path::Path;

use support::extensions::{
    install_child_file, install_package, install_sh_child, mcp_child_body,
    mcp_extension_fixture_bin, run_in_xdg, xdg_root, RUN_HINT, RUN_USAGE,
};
use support::{node_count, run_cli, tempdir, CliRunOptions as Opts};

/// The fixture manifest both languages install — identical except the
/// `executable` value, which is the pair's one variable: the
/// language-agnosticism proof. The `count` flag drives the compiled child's
/// row count — the envelope proof — while the shell twin emits a fixed two;
/// `name` is the mount name, `greet` for the runs and a colliding word for
/// the load refusals.
fn command_manifest(name: &str, executable: &str) -> String {
    format!(
        r#"{{"kind":"command","name":"{name}","version":"0.1.0","protocol":"2025-06-18","flags":[{{"kind":"integer","name":"count","default":3}}],"commit":"per-batch","executable":"{executable}"}}"#
    )
}

/// The compiled fixture's summary line and its starter: the text the
/// `mcp-extension-fixture` reports through `report_output`.
fn reported(count: i64) -> String {
    format!("imported {count} nodes\n  MATCH (n:Greet) RETURN n.name AS name ORDER BY name\n")
}

/// Install the compiled `greet` fixture — the `mcp-extension-fixture` bin —
/// under `xdg` as the package `rust-greet`: the Rust half of the pair,
/// running on every platform a compiled child can.
fn install_rust_greet(xdg: &Path) {
    let (bin, executable) = mcp_extension_fixture_bin();
    let package = install_package(xdg, "rust-greet", &command_manifest("greet", &executable));
    install_child_file(&package, &bin);
}

/// Install the shell `greet` fixture with `child` as its body under `xdg` as
/// the package `sh-greet`: the other half of the pair, gated to unix lanes.
#[cfg(unix)]
fn install_sh_greet(xdg: &Path, child: &str) {
    let package = install_package(xdg, "sh-greet", &command_manifest("greet", "run.sh"));
    install_sh_child(&package, &mcp_child_body("greet", child));
}

/// The shell child's clean session: the same handshake and the same `:Greet`
/// statement the compiled child opens with, two fixed rows, and the summary
/// with a starter — the shell language drives the identical store shape.
#[cfg(unix)]
const GREET_CHILD: &str = r#"mcp_call write_cypher '{"statement":"UNWIND $rows AS row CREATE (n:Greet {id: row.id})","parameters":{"rows":[{"id": 1}, {"id": 2}]}}' >/dev/null
mcp_call report_output '{"text":"imported 2 nodes","starters":["MATCH (n:Greet) RETURN count(n) AS n"]}' >/dev/null"#;

// --- the compiled Rust child (every platform) ---

/// `run <fixture>` on the `:memory:` default: the child's rows import
/// end-to-end and the non-TTY run exits after the summary — no shell,
/// no store. `--count 5` (the manifest's default is 3) proves the
/// flag value reached the child's envelope and drove its walk: only
/// the envelope explains five.
#[test]
fn the_rust_child_imports_rows_on_the_memory_default_and_exits() {
    let (tmp, xdg) = xdg_root();
    install_rust_greet(tmp.path());
    let cwd = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["run", "greet", "--count", "5"])
            .with_envs(&[("XDG_CONFIG_HOME", xdg.as_str())])
            .in_dir(cwd.path()),
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    // The summary and its starter — the whole stdout — then the exit.
    assert_eq!(stdout, reported(5));
    assert!(
        std::fs::read_dir(cwd.path()).unwrap().count() == 0,
        "the in-memory run must leave no store behind"
    );
}

/// The same fixture with `--database`: the child's rows land in a
/// persistent store (the manifest's default `count` resolving into the
/// envelope) and the run exits — an explicit target never opens the
/// shell (ADR-0286).
#[test]
fn the_rust_child_writes_a_persistent_store_and_the_run_exits() {
    let (tmp, xdg) = xdg_root();
    install_rust_greet(tmp.path());
    let cwd = tempdir();
    let db = cwd.path().join("graph");
    let db_path = db.to_str().expect("the store path is UTF-8");
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["run", "greet", "-d", db_path])
            .with_envs(&[("XDG_CONFIG_HOME", xdg.as_str())])
            .in_dir(cwd.path()),
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(stdout, reported(3));
    // The fixture's row call plus its two `batch_cypher` statements.
    assert_eq!(node_count(&db), 5, "the host wrote the child's calls");
}

// --- the shell twin (unix lanes) ---

/// The shell half of the pair on the `:memory:` default: the same grammar,
/// the same wire, the same exit — a shell script that speaks the bash kit
/// drives the host exactly like the compiled child does.
#[cfg(unix)]
#[test]
fn the_sh_child_imports_rows_on_the_memory_default_and_exits() {
    let (tmp, xdg) = xdg_root();
    install_sh_greet(tmp.path(), GREET_CHILD);
    let cwd = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["run", "greet"])
            .with_envs(&[("XDG_CONFIG_HOME", xdg.as_str())])
            .in_dir(cwd.path()),
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(
        stdout,
        "imported 2 nodes\n  MATCH (n:Greet) RETURN count(n) AS n\n"
    );
    assert!(
        std::fs::read_dir(cwd.path()).unwrap().count() == 0,
        "the in-memory run must leave no store behind"
    );
}

/// The shell half with `--database`: the fixed two rows persist and the
/// run exits, exactly like the compiled child's leg.
#[cfg(unix)]
#[test]
fn the_sh_child_writes_a_persistent_store_and_the_run_exits() {
    let (tmp, xdg) = xdg_root();
    install_sh_greet(tmp.path(), GREET_CHILD);
    let cwd = tempdir();
    let db = cwd.path().join("graph");
    let db_path = db.to_str().expect("the store path is UTF-8");
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["run", "greet", "--database", db_path])
            .with_envs(&[("XDG_CONFIG_HOME", xdg.as_str())])
            .in_dir(cwd.path()),
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(
        stdout,
        "imported 2 nodes\n  MATCH (n:Greet) RETURN count(n) AS n\n"
    );
    assert_eq!(node_count(&db), 2, "the host wrote the child's rows");
}

// --- the load-time collisions (every platform: the refusal costs no child) ---

/// A mount name installed twice refuses the whole load, naming both
/// sides — never first-wins (ADR-0429 §6). The sorted walk makes the
/// refusal deterministic: `beta-greet`'s duplicate names the package
/// that already installed it, and the verb fails closed, help included.
#[test]
fn a_name_installed_twice_refuses_the_load_with_the_pinned_text() {
    let (tmp, xdg) = xdg_root();
    install_package(
        tmp.path(),
        "alpha-greet",
        &command_manifest("greet", "run.sh"),
    );
    install_package(
        tmp.path(),
        "beta-greet",
        &command_manifest("greet", "run.sh"),
    );
    let (status, stdout, stderr) = run_in_xdg(&["run"], &xdg);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(
        stderr,
        format!(
            "Error: extension name 'greet' collides with the already-installed extension \
              (package 'alpha-greet')\n{RUN_USAGE}\n{RUN_HINT}\n"
        )
    );
    // The grammar `run` accepts is machine state: the refusal answers a
    // help invocation too, never a partial page.
    let (status, stdout, stderr) = run_in_xdg(&["run", "greet", "--help"], &xdg);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(
        stderr.contains("collides with the already-installed extension"),
        "{stderr}"
    );
}

/// A mount name spelling a reserved command word refuses the load with
/// the pinned text (ADR-0429 §6) — an extension can never shadow the
/// CLI's own grammar.
#[test]
fn a_name_spelling_a_reserved_command_word_refuses_the_load() {
    let (tmp, xdg) = xdg_root();
    install_package(tmp.path(), "helper", &command_manifest("serve", "run.sh"));
    let (status, stdout, stderr) = run_in_xdg(&["run"], &xdg);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(
        stderr,
        format!(
            "Error: extension name 'serve' collides with the reserved command word 'serve'\n\
              {RUN_USAGE}\n{RUN_HINT}\n"
        )
    );
}

// --- the manifest protocol gate (every platform: the refusal costs no child) ---

/// A package declaring another protocol refuses the `run` dispatch with the
/// action's usage class (exit 2): the manifest declares, the host enforces,
/// and the refusal names both revisions (ADR-0447).
#[test]
fn a_manifest_naming_another_protocol_refuses_the_run_with_the_pinned_text() {
    let (tmp, xdg) = xdg_root();
    install_package(
        tmp.path(),
        "stale-greet",
        &command_manifest("greet", "run.sh").replacen(
            r#""protocol":"2025-06-18""#,
            r#""protocol":"1""#,
            1,
        ),
    );
    let (status, stdout, stderr) = run_in_xdg(&["run", "greet"], &xdg);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(
        stderr,
        "Error: extension 'greet' declares protocol '1'; this host speaks '2025-06-18'\n\
         Usage: relate run greet [--count N]\n\
         Run 'relate run greet --help' for more information.\n"
    );
}
