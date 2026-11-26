//! End-to-end tests for `relate extension check <dir>` (ADR-0429 §6,
//! ADR-0430 §5, ADR-0447): the package check through the real binary —
//! one report line per stage, the six stages of a clean package, and one
//! fixture per failure class with its stage line and exit code.
//!
//! The `sh` children are the extension suites' one kit
//! (`support::extensions`), so a fixture cannot forget the handshake or
//! drift from the shipped `mcp-client.sh`. The check needs no
//! `$XDG_CONFIG_HOME` at all — it reads the directory it is handed, never
//! the machine's installed set — which is itself part of the contract
//! pinned here: no store flag, no store, nothing written.

#![cfg(all(feature = "extensions", unix))]

use crate::support;

use std::path::{Path, PathBuf};

use support::extensions::{install_sh_child, mcp_child_body, mcp_kit};
use support::{run_cli, tempdir, CliRunOptions as Opts};

/// The manifest every fixture installs: one string flag, `per-batch`, the
/// `run.sh` executable the kit writes.
fn manifest() -> String {
    r#"{"kind":"command","name":"greet","version":"0.1.0","protocol":"2025-06-18","flags":[{"kind":"string","name":"message","default":"hello"}],"commit":"per-batch","executable":"run.sh"}"#
        .to_string()
}

/// A package directory outside any extensions dir — what the verb takes —
/// with `manifest` and (when given) the `sh` child `body`.
fn package(manifest: &str, body: Option<&str>) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempdir();
    let dir = tmp.path().join("pkg");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("extension.json"), manifest).unwrap();
    if let Some(body) = body {
        install_sh_child(&dir, body);
    }
    (tmp, dir)
}

/// `relate extension check <dir>` through the real binary.
fn check(dir: &Path) -> (std::process::ExitStatus, String, String) {
    let dir = dir.to_str().expect("the fixture path is UTF-8");
    run_cli(Opts::new(&["extension", "check", dir]))
}

#[test]
fn a_clean_package_prints_one_line_per_stage_and_exits_zero() {
    let (_tmp, dir) = package(
        &manifest(),
        Some(&mcp_child_body(
            "greet",
            "mcp_call report_output '{\"text\":\"ok\"}' >/dev/null",
        )),
    );
    let (status, stdout, stderr) = check(&dir);
    assert!(status.success(), "stderr: {stderr}\nstdout: {stdout}");
    let lines: Vec<&str> = stdout.lines().collect();
    // The six stages, in the mount's order, one line each, then the
    // advisory pin line (the fixture package carries no marker).
    let labels: Vec<&str> = lines
        .iter()
        .map(|line| line.split(':').next().expect("a stage label"))
        .collect();
    assert_eq!(
        labels,
        [
            "manifest",
            "executable",
            "spawn",
            "initialize",
            "notifications/initialized",
            "tools/list",
            "pin"
        ],
        "{stdout}"
    );
    assert_eq!(
        lines.last().copied(),
        Some("pin: not from a GitHub source (unmarked)"),
        "{stdout}"
    );
    assert!(
        lines.iter().take(5).all(|line| line.contains(": ok")),
        "{stdout}"
    );
    assert_eq!(stderr, "", "a clean check says nothing on stderr");
}

/// The declaration class exits 2: a package whose file is not executable
/// never spawns, and the stage line names the file.
#[test]
fn a_non_executable_package_exits_two_naming_the_stage() {
    let (_tmp, dir) = package(&manifest(), None);
    std::fs::write(dir.join("run.sh"), "#!/bin/sh\n").unwrap();
    let (status, stdout, _stderr) = check(&dir);
    assert_eq!(status.code(), Some(2), "{stdout}");
    assert!(
        stdout.contains("executable: failed: ") && stdout.contains("is not executable"),
        "{stdout}"
    );
    // The check stops at the first failing stage.
    assert_eq!(stdout.lines().count(), 2, "{stdout}");
}

/// The runtime class exits 1: a manifest that loads but a child of another
/// MCP revision fails the `initialize` stage.
#[test]
fn a_child_of_another_revision_exits_one() {
    let body = format!(
        "{}\nMCP_PROTOCOL_VERSION=1999-01-01\nmcp_init greet 0.1.0\n",
        mcp_kit()
    );
    let (_tmp, dir) = package(&manifest(), Some(&body));
    let (status, stdout, _stderr) = check(&dir);
    assert_eq!(status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains(
            "initialize: failed: the extension names MCP revision '1999-01-01'; this host serves \
             '2025-06-18'"
        ),
        "{stdout}"
    );
}

/// A tool the mount does not serve fails the tool-table stage.
#[test]
fn an_unknown_tool_exits_one_at_the_tool_table_stage() {
    let (_tmp, dir) = package(
        &manifest(),
        Some(&mcp_child_body(
            "greet",
            "mcp_call delete_everything '{}' >/dev/null",
        )),
    );
    let (status, stdout, _stderr) = check(&dir);
    assert_eq!(status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains(
            "tools/list: failed: the extension calls 'delete_everything'; this mount serves: \
             write_cypher, read_cypher, update_schema, batch_cypher, report_output"
        ),
        "{stdout}"
    );
}

/// The verb opens no store, so it takes no store flag: `--database` is an
/// unexpected argument (exit 2), and nothing is created at the path.
#[test]
fn the_check_refuses_a_store_flag_and_writes_nothing() {
    let (tmp, dir) = package(&manifest(), None);
    let store = tmp.path().join("store");
    let dir_arg = dir.to_str().expect("the fixture path is UTF-8");
    let store_arg = store.to_str().expect("the fixture path is UTF-8");
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "extension",
        "check",
        dir_arg,
        "--database",
        store_arg,
    ]));
    assert_eq!(status.code(), Some(2), "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.contains("unexpected argument to extension check: --database"),
        "{stderr}"
    );
    assert!(!store.exists(), "the check created the store it was handed");
}

/// The missing directory is a manifest-stage refusal, not a panic.
#[test]
fn a_missing_package_directory_exits_two() {
    let tmp = tempdir();
    let missing = tmp.path().join("no-such-package");
    let (status, stdout, _stderr) = check(&missing);
    assert_eq!(status.code(), Some(2), "{stdout}");
    assert!(
        stdout.contains("no extension.json in the package directory"),
        "{stdout}"
    );
}

/// A github-pin provenance marker recording `commit` for `greet`
/// (ADR-0449) — the file an installed `--github` package carries.
fn github_marker(commit: &str) -> String {
    format!(
        "{{\"tier\":\"github-pin\",\"source\":\"owner/repo@v1.2.3\",\
         \"pin\":{{\"repo\":\"owner/repo\",\"tag\":\"v1.2.3\",\"commit\":\"{commit}\",\
         \"assetSha256\":\"{}\"}}}}\n",
        "a".repeat(64)
    )
}

const COMMIT_A: &str = "1111111111111111111111111111111111111111";
const COMMIT_B: &str = "2222222222222222222222222222222222222222";

/// The per-user trust store under `xdg` (`<xdg>/relate/extensions-trust.json`,
/// ADR-0449) recording `greet`'s approved pin at `commit`, when given.
fn trust_store(xdg: &Path, commit: Option<&str>) {
    let dir = xdg.join("relate");
    std::fs::create_dir_all(&dir).unwrap();
    let entries = match commit {
        Some(commit) => format!(
            "{{\"greet\":{{\"repo\":\"owner/repo\",\"tag\":\"v1.2.3\",\"commit\":\"{commit}\",\
             \"assetSha256\":\"{}\"}}}}\n",
            "a".repeat(64)
        ),
        None => "{}\n".to_string(),
    };
    std::fs::write(dir.join("extensions-trust.json"), entries).unwrap();
}

/// A clean github-pin package whose marker matches the trust store: the
/// closing line reports `approved`, and the check exits 0.
#[test]
fn an_approved_pin_is_reported_and_exits_zero() {
    let (tmp, dir) = package(&manifest(), Some(&mcp_child_body("greet", "exit 0")));
    std::fs::write(dir.join(".relate-provenance.json"), github_marker(COMMIT_A)).unwrap();
    let xdg = tmp.path().join("xdg");
    let xdg_arg = xdg.to_str().expect("the fixture path is UTF-8");
    trust_store(&xdg, Some(COMMIT_A));
    let dir_arg = dir.to_str().expect("the fixture path is UTF-8");
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["extension", "check", dir_arg]).with_envs(&[("XDG_CONFIG_HOME", xdg_arg)]),
    );
    assert!(status.success(), "stderr: {stderr}\nstdout: {stdout}");
    assert_eq!(
        stdout.lines().last(),
        Some(format!("pin: approved (owner/repo@v1.2.3 commit {COMMIT_A})").as_str()),
        "{stdout}"
    );
}

/// A package whose recorded pin no longer matches the approval: the line
/// says `drifted` with the remedy, and the check still exits 0 — drift does
/// not stop a package mounting (the install path is where it refuses).
#[test]
fn a_drifted_pin_is_reported_and_does_not_change_the_exit_code() {
    let (tmp, dir) = package(&manifest(), Some(&mcp_child_body("greet", "exit 0")));
    std::fs::write(dir.join(".relate-provenance.json"), github_marker(COMMIT_A)).unwrap();
    let xdg = tmp.path().join("xdg");
    let xdg_arg = xdg.to_str().expect("the fixture path is UTF-8");
    trust_store(&xdg, Some(COMMIT_B));
    let dir_arg = dir.to_str().expect("the fixture path is UTF-8");
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["extension", "check", dir_arg]).with_envs(&[("XDG_CONFIG_HOME", xdg_arg)]),
    );
    assert_eq!(status.code(), Some(0), "stderr: {stderr}\nstdout: {stdout}");
    assert!(
        stdout.lines().last().is_some_and(|line| line.starts_with(
            format!("pin: drifted (installed owner/repo@v1.2.3 commit {COMMIT_A};").as_str()
        )),
        "{stdout}"
    );
    assert!(stdout.contains("--re-approve"), "{stdout}");
}

/// A `--from` install (the local tier) reports that it is not a GitHub
/// source — the marker's tier, not a missing pin.
#[test]
fn a_local_package_reports_not_from_a_github_source() {
    let (_tmp, dir) = package(&manifest(), Some(&mcp_child_body("greet", "exit 0")));
    std::fs::write(
        dir.join(".relate-provenance.json"),
        "{\"tier\":\"local\",\"source\":\"/pkgs/greet\"}\n",
    )
    .unwrap();
    let (status, stdout, _stderr) = check(&dir);
    assert!(status.success(), "{stdout}");
    assert_eq!(
        stdout.lines().last(),
        Some("pin: not from a GitHub source (local)"),
        "{stdout}"
    );
}
