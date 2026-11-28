//! The `extension check` verb's stages (ADR-0429 §6, ADR-0430 §5,
//! ADR-0447): one fixture package per failure class — a bad manifest, a
//! missing executable, a non-executable file, a silent child, a child of
//! another MCP revision, an unknown tool — each asserting its stage line
//! and exit code, plus the clean package that passes all six stages.
//!
//! The fixture packages are plain directories under a tempdir (the verb
//! takes a directory, not an installed package), written with the
//! extension tree's one kit ([`crate::extensions::testkit`]) so a fixture
//! manifest and child cannot drift from the shipped ones.

use super::*;
use crate::extensions::testkit::manifest_for;

/// The check's read deadline in the suites: short enough that a stalled
/// fixture fails fast, long enough that a spawned shell answers first.
const TEST_DEADLINE: Duration = Duration::from_secs(5);

/// A stage's line: the report's line at `stage`'s position.
fn line(report: &Report, stage: Stage) -> &str {
    report
        .lines
        .iter()
        .find(|line| line.starts_with(&format!("{}:", stage.as_str())))
        .unwrap_or_else(|| panic!("no {} line in {:?}", stage.as_str(), report.lines))
}

/// The failing report's last line, asserting the stage and exit code —
/// every failure names its stage and stops the run there.
fn assert_failure(report: &Report, stage: Stage, expected_exit: i32) {
    assert_eq!(report.failure, Some(stage), "{:?}", report.lines);
    assert_eq!(report.exit_code(), expected_exit, "{:?}", report.lines);
    assert_eq!(
        line(report, stage),
        report.lines.last().expect("a failure ends the report"),
        "the failing stage's line is the report's last: {:?}",
        report.lines
    );
}

// ---------------------------------------------------------------------
// Stages 1-2: the package itself (no child runs)
// ---------------------------------------------------------------------

mod package {
    use super::*;

    /// A package directory holding `manifest`, and — when `child` is
    /// given — the `run.sh` executable the manifest declares.
    fn package(manifest: &str, child: Option<&str>) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pkg");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("extension.json"), manifest).unwrap();
        if let Some(body) = child {
            crate::extensions::testkit::sh_fixture(&dir, "run.sh", body);
        }
        tmp
    }

    fn checked(tmp: &tempfile::TempDir) -> Report {
        check(&tmp.path().join("pkg"), TEST_DEADLINE)
    }

    #[test]
    fn a_malformed_manifest_fails_the_manifest_stage() {
        let tmp = package("{ nope", None);
        let report = checked(&tmp);
        assert_failure(&report, Stage::Manifest, 2);
        assert!(
            line(&report, Stage::Manifest)
                .starts_with("manifest: failed: extension 'pkg': invalid manifest: "),
            "{:?}",
            report.lines
        );
    }

    #[test]
    fn an_unknown_kind_fails_the_manifest_stage() {
        let tmp = package(
            &manifest_for("greet", "", "").replacen("\"command\"", "\"callables\"", 1),
            None,
        );
        let report = checked(&tmp);
        assert_failure(&report, Stage::Manifest, 2);
        let failed = line(&report, Stage::Manifest);
        assert!(
            failed.contains("unknown variant") && failed.contains("callables"),
            "{failed}"
        );
    }

    /// The closed vocabularies are the loader's own refusals, surfaced by
    /// the check unchanged: an unknown flag kind and an unknown commit
    /// policy fail the first stage naming the value.
    #[test]
    fn the_closed_flag_and_commit_vocabularies_fail_the_manifest_stage() {
        for (manifest, needle) in [
            (
                manifest_for(
                    "greet",
                    r#"{"kind":"float","name":"ratio","default":1.5}"#,
                    "",
                ),
                "float",
            ),
            (
                manifest_for("greet", "", "").replacen("\"per-batch\"", "\"partial\"", 1),
                "unknown commit policy",
            ),
        ] {
            let tmp = package(&manifest, None);
            let report = checked(&tmp);
            assert_failure(&report, Stage::Manifest, 2);
            let failed = line(&report, Stage::Manifest);
            assert!(
                failed.starts_with("manifest: failed: extension 'pkg': invalid manifest: ")
                    && failed.contains(needle),
                "{needle}: {failed}"
            );
        }
    }

    #[test]
    fn a_reserved_command_word_fails_the_manifest_stage() {
        let tmp = package(&manifest_for("run", "", ""), None);
        let report = checked(&tmp);
        assert_failure(&report, Stage::Manifest, 2);
        assert_eq!(
            line(&report, Stage::Manifest),
            "manifest: failed: extension name 'run' collides with the reserved command word 'run'"
        );
    }

    #[test]
    fn an_escaping_executable_fails_the_manifest_stage() {
        let tmp = package(
            &manifest_for("greet", "", "").replacen("\"run.sh\"", "\"../evil.sh\"", 1),
            None,
        );
        let report = checked(&tmp);
        assert_failure(&report, Stage::Manifest, 2);
        assert_eq!(
            line(&report, Stage::Manifest),
            "manifest: failed: extension 'pkg': executable path '../evil.sh' escapes the package directory"
        );
    }

    /// The manifest's protocol is the mount's one revision (ADR-0447): a
    /// package that declares another is refused before anything runs, with
    /// the `run` grammar's own wording.
    #[test]
    fn a_declared_protocol_this_host_does_not_speak_fails_the_manifest_stage() {
        let tmp = package(
            &manifest_for("greet", "", "").replacen("\"2025-06-18\"", "\"1999-01-01\"", 1),
            None,
        );
        let report = checked(&tmp);
        assert_failure(&report, Stage::Manifest, 2);
        assert_eq!(
            line(&report, Stage::Manifest),
            "manifest: failed: extension 'greet' declares protocol '1999-01-01'; this host speaks \
             '2025-06-18'"
        );
    }

    /// A flag table the mount cannot parse is a manifest problem too: the
    /// same refusal `run` renders.
    #[test]
    fn a_duplicate_flag_name_fails_the_manifest_stage() {
        let tmp = package(
            &manifest_for(
                "greet",
                r#"{"kind":"string","name":"tag","default":"a"},
                   {"kind":"string","name":"tag","default":"b"}"#,
                "",
            ),
            None,
        );
        let report = checked(&tmp);
        assert_failure(&report, Stage::Manifest, 2);
        assert_eq!(
            line(&report, Stage::Manifest),
            "manifest: failed: extension 'greet' declares the flag 'tag' twice"
        );
    }

    #[test]
    fn a_missing_executable_fails_the_executable_stage() {
        let tmp = package(&manifest_for("greet", "", ""), None);
        let report = checked(&tmp);
        assert_failure(&report, Stage::Executable, 2);
        let failed = line(&report, Stage::Executable);
        assert!(
            failed.starts_with("executable: failed: the extension executable ")
                && failed.ends_with("run.sh does not exist"),
            "{failed}"
        );
    }

    /// A file that is not executable never spawns: the check reports the
    /// exec bit rather than a spawn failure (the mount would say
    /// `NotExecutable`), because the package's contents are the usage class.
    #[cfg(unix)]
    #[test]
    fn a_non_executable_file_fails_the_executable_stage() {
        let tmp = package(&manifest_for("greet", "", ""), None);
        // Written, not made executable: the plain write's 0644 mode.
        std::fs::write(tmp.path().join("pkg").join("run.sh"), "#!/bin/sh\n").unwrap();
        let report = checked(&tmp);
        assert_failure(&report, Stage::Executable, 2);
        let failed = line(&report, Stage::Executable);
        assert!(
            failed.starts_with("executable: failed: the extension executable ")
                && failed.ends_with("run.sh is not executable"),
            "{failed}"
        );
    }
}

// ---------------------------------------------------------------------
// Stages 3-6: the running package (unix lanes: the fixture children, like
// the mount's own spawn suites)
// ---------------------------------------------------------------------

#[cfg(unix)]
mod running {
    use super::*;
    use crate::extensions::kit::mcp_kit;
    use crate::extensions::testkit::{mcp_child_body, sh_fixture};

    /// The check's deadline for a stalled fixture: short, so the stall
    /// tests cost milliseconds.
    const STALL_DEADLINE: Duration = Duration::from_millis(200);

    /// A package directory with `manifest` and the `run.sh` child `body`.
    fn package(manifest: &str, body: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pkg");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("extension.json"), manifest).unwrap();
        sh_fixture(&dir, "run.sh", body);
        tmp
    }

    fn checked(tmp: &tempfile::TempDir, deadline: Duration) -> Report {
        check(&tmp.path().join("pkg"), deadline)
    }

    /// The whole contract on one clean package: the six stage lines in the
    /// stages' order, the advisory pin line, and exit 0.
    #[test]
    fn a_clean_package_passes_every_stage() {
        let tmp = package(
            &manifest_for("greet", "", ""),
            &mcp_child_body(
                "greet",
                "mcp_call report_output '{\"text\":\"ok\"}' >/dev/null",
            ),
        );
        let report = checked(&tmp, TEST_DEADLINE);
        assert_eq!(report.failure, None, "{:?}", report.lines);
        assert_eq!(report.exit_code(), 0);
        let mut labels: Vec<&str> = [
            Stage::Manifest,
            Stage::Executable,
            Stage::Spawn,
            Stage::Initialize,
            Stage::Initialized,
            Stage::ToolTable,
        ]
        .iter()
        .map(|stage| stage.as_str())
        .collect();
        let printed: Vec<&str> = report
            .lines
            .iter()
            .map(|line| line.split(':').next().expect("a stage label"))
            .collect();
        // The closing status line is not a stage: `pin` follows the six.
        labels.push("pin");
        assert_eq!(printed, labels, "{:?}", report.lines);
        assert_eq!(
            report.lines.last().map(String::as_str),
            Some("pin: not from a GitHub source (unmarked)")
        );
        assert_eq!(
            line(&report, Stage::Manifest),
            "manifest: ok (greet 0.1.0, protocol 2025-06-18)"
        );
        assert_eq!(line(&report, Stage::Executable), "executable: ok (run.sh)");
        assert_eq!(line(&report, Stage::Spawn), "spawn: ok");
        assert_eq!(
            line(&report, Stage::Initialize),
            format!(
                "initialize: ok (MCP {MCP_PROTOCOL_VERSION}, server relate {})",
                env!("CARGO_PKG_VERSION")
            )
        );
        assert_eq!(
            line(&report, Stage::Initialized),
            "notifications/initialized: ok"
        );
        assert_eq!(
            line(&report, Stage::ToolTable),
            "tools/list: ok (the shared tool table: 5 tools; the extension calls 'report_output')"
        );
    }

    /// A child that handshakes and then ends (it asked for nothing) is
    /// clean: the tool table stage served a table nobody needed.
    #[test]
    fn a_child_that_ends_after_the_handshake_passes() {
        let tmp = package(
            &manifest_for("greet", "", ""),
            &mcp_child_body("greet", "exit 0"),
        );
        let report = checked(&tmp, TEST_DEADLINE);
        assert_eq!(report.failure, None, "{:?}", report.lines);
        assert_eq!(
            line(&report, Stage::ToolTable),
            "tools/list: ok (the shared tool table: 5 tools)"
        );
    }

    /// The silent child: it takes its envelope and writes nothing. The
    /// check's deadline names the stage instead of hanging (the mount's
    /// deferred gap 7 is a diagnostic's must).
    #[test]
    fn a_silent_child_fails_the_initialize_stage_on_the_deadline() {
        let tmp = package(
            &manifest_for("greet", "", ""),
            "read -r envelope\nread -r more\n",
        );
        let report = checked(&tmp, STALL_DEADLINE);
        assert_failure(&report, Stage::Initialize, 1);
        assert_eq!(
            line(&report, Stage::Initialize),
            "initialize: failed: the extension wrote no request within 200ms (the initialize \
             request was expected)"
        );
    }

    /// A child whose stream ends before `initialize` is the mount's own
    /// refusal, verbatim.
    #[test]
    fn a_child_that_ends_before_initializing_fails_the_initialize_stage() {
        let tmp = package(&manifest_for("greet", "", ""), "exit 0\n");
        let report = checked(&tmp, TEST_DEADLINE);
        assert_failure(&report, Stage::Initialize, 1);
        assert_eq!(
            line(&report, Stage::Initialize),
            "initialize: failed: the extension child ended its MCP session without an initialize \
             request"
        );
    }

    /// The mount refuses a first request that is not `initialize`; so does
    /// the check, in the mount's words.
    #[test]
    fn a_first_request_that_is_not_initialize_fails_the_initialize_stage() {
        let tmp = package(
            &manifest_for("greet", "", ""),
            "read -r envelope\nprintf '%s\\n' \
             '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}'\nread -r answer\n",
        );
        let report = checked(&tmp, TEST_DEADLINE);
        assert_failure(&report, Stage::Initialize, 1);
        assert_eq!(
            line(&report, Stage::Initialize),
            "initialize: failed: the first MCP request must be initialize, got 'tools/list'"
        );
    }

    /// A child of another MCP revision cannot mount: the check names both
    /// revisions instead of answering and letting the child fail.
    #[test]
    fn a_child_of_another_revision_fails_the_initialize_stage() {
        let body = format!(
            "{}\nMCP_PROTOCOL_VERSION=1999-01-01\nmcp_init greet 0.1.0\n",
            mcp_kit()
        );
        let tmp = package(&manifest_for("greet", "", ""), &body);
        let report = checked(&tmp, TEST_DEADLINE);
        assert_failure(&report, Stage::Initialize, 1);
        assert_eq!(
            line(&report, Stage::Initialize),
            format!(
                "initialize: failed: the extension names MCP revision '1999-01-01'; this host \
                 serves '{MCP_PROTOCOL_VERSION}'"
            )
        );
    }

    /// MCP's handshake ends with the client's confirmation: a child that
    /// goes straight to its first request skips it, and the check says so.
    #[test]
    fn a_child_that_skips_the_initialized_notification_fails_that_stage() {
        let tmp = package(
            &manifest_for("greet", "", ""),
            "read -r envelope\nprintf '%s\\n' \
             '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":\
             {\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\
             \"clientInfo\":{\"name\":\"greet\",\"version\":\"0.1.0\"}}}'\n\
             read -r answer\nprintf '%s\\n' \
             '{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":\
             {\"name\":\"report_output\",\"arguments\":{\"text\":\"x\"}}}'\nread -r answer\n",
        );
        let report = checked(&tmp, TEST_DEADLINE);
        assert_failure(&report, Stage::Initialized, 1);
        assert_eq!(
            line(&report, Stage::Initialized),
            "notifications/initialized: failed: the extension sent 'tools/call' before \
             notifications/initialized"
        );
    }

    /// A tool the mount does not serve is the tool-table stage's refusal,
    /// naming what the mount does serve — read off the shared table.
    #[test]
    fn an_unknown_tool_fails_the_tool_table_stage() {
        let tmp = package(
            &manifest_for("greet", "", ""),
            &mcp_child_body("greet", "mcp_call delete_everything '{}' >/dev/null"),
        );
        let report = checked(&tmp, TEST_DEADLINE);
        assert_failure(&report, Stage::ToolTable, 1);
        assert_eq!(
            line(&report, Stage::ToolTable),
            "tools/list: failed: the extension calls 'delete_everything'; this mount serves: \
             write_cypher, read_cypher, update_schema, batch_cypher, report_output"
        );
    }

    /// `tools/list` is answered with the wire kit's one table: the fixture
    /// reads the answer and calls a tool only when the table carried it, so
    /// the check's last line proves what the child received.
    #[test]
    fn a_tools_list_request_is_answered_with_the_shared_table() {
        let body = "read -r envelope\n\
             printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":\
             {\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\
             \"clientInfo\":{\"name\":\"greet\",\"version\":\"0.1.0\"}}}'\n\
             read -r answer\n\
             printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}'\n\
             printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}'\n\
             read -r table\n\
             case $table in\n\
               *'\"name\":\"report_output\"'*)\n\
                 printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":\
             {\"name\":\"report_output\",\"arguments\":{\"text\":\"x\"}}}'\n\
                 read -r answer ;;\n\
               *) exit 1 ;;\n\
             esac\n";
        let tmp = package(&manifest_for("greet", "", ""), body);
        let report = checked(&tmp, TEST_DEADLINE);
        assert_eq!(report.failure, None, "{:?}", report.lines);
        assert_eq!(
            line(&report, Stage::ToolTable),
            "tools/list: ok (the shared tool table: 5 tools; the extension calls 'report_output')"
        );
    }

    /// The check is safe against any store: no store path exists for it to
    /// touch, and the stage it reports is the same whatever the working
    /// directory holds. The verb's own grammar test pins that `--database`
    /// is not accepted (`extension_cmd::tests`).
    #[test]
    fn the_check_writes_nothing_beside_the_package() {
        let tmp = package(
            &manifest_for("greet", "", ""),
            &mcp_child_body("greet", "exit 0"),
        );
        let before = std::fs::read_dir(tmp.path()).unwrap().count();
        let report = checked(&tmp, TEST_DEADLINE);
        assert_eq!(report.failure, None, "{:?}", report.lines);
        let after = std::fs::read_dir(tmp.path()).unwrap().count();
        assert_eq!(
            before, after,
            "the check created a file: {:?}",
            report.lines
        );
    }
}

// ---------------------------------------------------------------------
// The closing pin line (ADR-0449): a status report, never a stage
// ---------------------------------------------------------------------

mod pin_report {
    use super::*;
    use crate::extensions::provenance::{Pin, MARKER_FILE};
    use crate::extensions::trust::{trust_path, TrustStore};

    const COMMIT_A: &str = "1111111111111111111111111111111111111111";
    const COMMIT_B: &str = "2222222222222222222222222222222222222222";

    /// A package directory holding `marker` (when given) under the
    /// `greet` manifest — the directory the pin line reads.
    fn package(marker: Option<&str>, body: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pkg");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("extension.json"), manifest_for("greet", "", "")).unwrap();
        crate::extensions::testkit::sh_fixture(&dir, "run.sh", body);
        if let Some(marker) = marker {
            std::fs::write(dir.join(MARKER_FILE), marker).unwrap();
        }
        tmp
    }

    /// A github-pin marker recording `commit`.
    fn github_marker(commit: &str) -> String {
        format!(
            "{{\"tier\":\"github-pin\",\"source\":\"owner/repo@v1.2.3\",\
             \"pin\":{{\"repo\":\"owner/repo\",\"tag\":\"v1.2.3\",\"commit\":\"{commit}\",\
             \"assetSha256\":\"{}\"}}}}\n",
            "a".repeat(64)
        )
    }

    fn pin(tag: &str, commit: &str) -> Pin {
        Pin {
            repo: "owner/repo".to_string(),
            tag: tag.to_string(),
            commit: commit.to_string(),
            asset_sha256: "a".repeat(64),
        }
    }

    /// The trust store under `root` with `entry` recorded for `name` — the
    /// real store file, written through its own writer (`record` persists
    /// the entry: the install's own commit step).
    fn trust_store(root: &Path, name: &str, entry: Pin) -> PathBuf {
        let path = trust_path(root);
        let mut store = TrustStore::load(&path).unwrap();
        store.record(name.to_string(), entry).unwrap();
        path
    }

    fn line_for(tmp: &tempfile::TempDir, trust: Option<&Path>) -> String {
        pin_line("greet", &tmp.path().join("pkg"), trust)
    }

    #[test]
    fn an_approved_pin_reports_approved() {
        let tmp = package(Some(&github_marker(COMMIT_A)), "exit 0\n");
        let trust = trust_store(tmp.path(), "greet", pin("v1.2.3", COMMIT_A));
        assert_eq!(
            line_for(&tmp, Some(&trust)),
            format!("pin: approved (owner/repo@v1.2.3 commit {COMMIT_A})")
        );
    }

    #[test]
    fn a_drifted_pin_reports_drifted_with_the_remedy() {
        let tmp = package(Some(&github_marker(COMMIT_A)), "exit 0\n");
        let trust = trust_store(tmp.path(), "greet", pin("v1.0.0", COMMIT_B));
        assert_eq!(
            line_for(&tmp, Some(&trust)),
            format!(
                "pin: drifted (installed owner/repo@v1.2.3 commit {COMMIT_A}; the trust store \
                 holds owner/repo@v1.0.0 commit {COMMIT_B} — re-approve with 'relate \
                 extension install greet --github owner/repo@v1.2.3 --re-approve')"
            )
        );
    }

    /// The store's other entries do not approve this package: the lookup is
    /// by the manifest's mount name.
    #[test]
    fn a_github_package_with_no_store_entry_reports_drifted() {
        let tmp = package(Some(&github_marker(COMMIT_A)), "exit 0\n");
        let trust = trust_store(tmp.path(), "other", pin("v1.2.3", COMMIT_A));
        assert_eq!(
            line_for(&tmp, Some(&trust)),
            format!(
                "pin: drifted (the trust store holds no pin for 'greet'; the package carries \
                 owner/repo@v1.2.3 commit {COMMIT_A})"
            )
        );
    }

    #[test]
    fn a_non_github_package_reports_its_source() {
        for (marker, state) in [
            (
                Some("{\"tier\":\"local\",\"source\":\"/pkgs/greet\"}\n"),
                "local",
            ),
            (
                Some("{\"tier\":\"third-party\",\"source\":\"https://x.test/p.tar.gz\"}\n"),
                "third-party",
            ),
            (
                Some("{\"tier\":\"first-party\",\"source\":\"relate-extensions-greet-1.0.0\"}\n"),
                "first-party",
            ),
            (None, "unmarked"),
        ] {
            let tmp = package(marker, "exit 0\n");
            assert_eq!(
                line_for(&tmp, None),
                format!("pin: not from a GitHub source ({state})"),
                "marker: {marker:?}"
            );
        }
    }

    /// Corruption is reported, never worn as one of the three states.
    #[test]
    fn a_corrupt_marker_reports_unknown() {
        let tmp = package(Some("{ nope"), "exit 0\n");
        let line = line_for(&tmp, None);
        assert!(
            line.starts_with("pin: unknown (invalid provenance marker "),
            "{line}"
        );
    }

    /// A github-pin marker with no pin field cannot be compared: `unknown`,
    /// not a silent approval.
    #[test]
    fn a_github_marker_without_a_pin_reports_unknown() {
        let tmp = package(
            Some("{\"tier\":\"github-pin\",\"source\":\"owner/repo@v1.2.3\"}\n"),
            "exit 0\n",
        );
        assert_eq!(
            line_for(&tmp, None),
            "pin: unknown (the provenance marker records the github-pin tier without a pin)"
        );
    }

    /// A corrupt trust store is a refusal naming the file, surfaced by the
    /// line rather than hidden as drift.
    #[test]
    fn a_corrupt_trust_store_reports_unknown() {
        let tmp = package(Some(&github_marker(COMMIT_A)), "exit 0\n");
        let trust = tmp.path().join("trust.json");
        std::fs::write(&trust, "{ nope").unwrap();
        let line = line_for(&tmp, Some(&trust));
        assert!(
            line.starts_with("pin: unknown (invalid extension trust store "),
            "{line}"
        );
    }

    /// Drift is reported, never fatal: the package still mounts, so the
    /// check still exits 0 — the install path is where a moved pin refuses.
    #[cfg(unix)]
    #[test]
    fn a_drifted_pin_does_not_change_the_exit_code() {
        let tmp = package(
            Some(&github_marker(COMMIT_A)),
            &crate::extensions::testkit::mcp_child_body("greet", "exit 0"),
        );
        let trust = trust_store(tmp.path(), "greet", pin("v1.0.0", COMMIT_B));
        let report = check_with(&tmp.path().join("pkg"), TEST_DEADLINE, Some(&trust));
        assert_eq!(report.failure, None, "{:?}", report.lines);
        assert_eq!(report.exit_code(), 0);
        assert!(
            report
                .lines
                .last()
                .is_some_and(|line| line.starts_with("pin: drifted (")),
            "{:?}",
            report.lines
        );
    }
}
