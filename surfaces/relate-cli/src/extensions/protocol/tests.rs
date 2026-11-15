//! The mount's process-half tests, in the dir-module pattern the extension
//! tree's modules use (LABS-421): the pinned refusal texts, the caps, the
//! spawn and the exit classification — the file stays under the repo's
//! 1k-line budget with the module beside its tests.
//!
//! The refusal texts run on every platform; the child-process tests spawn a
//! fixture script through the extension tree's shared kit
//! (`testkit::sh_fixture`) and are gated to unix lanes, like every
//! process-driving suite. The record-level refusals the retired wire v1
//! carried (unknown record kind, wrong `hello`, a record after the summary)
//! are ported onto the MCP mount: `extensions::mcp::tests` pins the JSON-RPC
//! decode, the handshake ordering and every per-call refusal's exact
//! wording, and those texts keep their meaning there.

use super::*;

// ---------------------------------------------------------------------
// Pinned authored texts (every platform)
// ---------------------------------------------------------------------

#[test]
fn every_refusal_renders_its_pinned_text() {
    assert_eq!(
        ProtocolError::NotExecutable {
            exe: "/no/such/exe".to_string()
        }
        .to_string(),
        "the extension executable /no/such/exe was not found or is not executable"
    );
    assert_eq!(
        ProtocolError::SpawnFailed("no memory".to_string()).to_string(),
        "failed to spawn the extension child: no memory"
    );
    assert_eq!(
        ProtocolError::EnvelopeWriteFailed("broken pipe".to_string()).to_string(),
        "failed to send the request envelope to the extension child: broken pipe"
    );
    assert_eq!(
        ProtocolError::WaitFailed("no child".to_string()).to_string(),
        "failed to wait for the extension child: no child"
    );
    assert_eq!(
        ProtocolError::StderrJoinFailed.to_string(),
        "failed to collect the extension child's stderr"
    );
    assert_eq!(
        ProtocolError::ExitFailed {
            code: 3,
            stderr: "boom".to_string()
        }
        .to_string(),
        "the extension child exited with 3: boom"
    );
    assert_eq!(
        ProtocolError::ExitFailed {
            code: 4,
            stderr: String::new()
        }
        .to_string(),
        "the extension child exited with 4"
    );
    assert_eq!(
        ProtocolError::KilledBySignal {
            signal: Some(9),
            stderr: String::new()
        }
        .to_string(),
        "the extension child was killed by signal 9 before finishing its MCP session"
    );
    assert_eq!(
        ProtocolError::KilledBySignal {
            signal: Some(9),
            stderr: "dying gasp".to_string()
        }
        .to_string(),
        "the extension child was killed by signal 9 before finishing its MCP session: dying gasp"
    );
    assert_eq!(
        ProtocolError::KilledBySignal {
            signal: None,
            stderr: String::new()
        }
        .to_string(),
        "the extension child was killed before finishing its MCP session"
    );
}

#[test]
fn the_shipped_caps_are_the_adr_0377_scale() {
    // One place states the shipped numbers as literals: 8 MiB per statement
    // (and per response line), 1000 rows per statement, 4096 statements per
    // call, the 128 MiB stream bound ADR-0377 caps `git log` stdout at.
    assert_eq!(Caps::MOUNT.statement_bytes, 8_388_608);
    assert_eq!(Caps::MOUNT.rows_per_statement, 1000);
    assert_eq!(Caps::MOUNT.statements, 4096);
    assert_eq!(Caps::MOUNT.stream_bytes, 134_217_728);
    // The published facts are the caps, and the framing bound is the kit's
    // one derivation from the statement cap.
    let facts = Caps::MOUNT.mcp_facts();
    assert_eq!(facts.max_statement_bytes, Caps::MOUNT.statement_bytes);
    assert_eq!(facts.max_rows_per_statement, Caps::MOUNT.rows_per_statement);
    assert_eq!(facts.max_statements, Caps::MOUNT.statements);
    assert_eq!(facts.max_stream_bytes, Caps::MOUNT.stream_bytes);
    assert_eq!(
        facts.max_request_bytes,
        relate_wire::mcp::request_bytes_cap(Caps::MOUNT.statement_bytes)
    );
    assert_eq!(
        Caps::MOUNT.mcp_response_bytes(),
        Caps::MOUNT.statement_bytes
    );
}

// ---------------------------------------------------------------------
// The spawned child (unix lanes: the fixture script)
// ---------------------------------------------------------------------

#[cfg(unix)]
mod child {
    // The fixture children the mount spawns, written by the extension
    // tree's shared kit (`testkit::sh_fixture`): the direct exec of a
    // shebang script, no shell on the host side (ADR-0430's
    // language-agnosticism proof).
    use std::path::Path;

    use super::super::*;
    use crate::extensions::testkit::{mcp_child_body, sh_fixture};

    /// The request envelope the tests resolve for the child: two flag
    /// values, the shape the manifest flag table produces.
    fn envelope() -> Map<String, Value> {
        let mut envelope = Map::new();
        envelope.insert("dir".to_string(), serde_json::json!("."));
        envelope.insert("limit".to_string(), serde_json::json!(10));
        envelope
    }

    /// Spawn the fixture and write its envelope, then close stdin — the
    /// child reads one envelope line and sees end-of-input, which ends its
    /// session before it sends a request. Returns the outcome of the read
    /// side plus the reaped exit classification.
    fn run_in(exe: &Path, cwd: &Path) -> Result<(), ProtocolError> {
        let mut piped = spawn_piped(exe, Some(cwd))?;
        write_envelope_line(&mut piped.stdin, &envelope())?;
        drop(piped.stdin);
        // The child said nothing: the read side sees the end of its stream,
        // and the classification is the exit's (ADR-0430 §5).
        let mut line = String::new();
        let _ = std::io::BufRead::read_line(&mut piped.stdout, &mut line);
        wait_and_reap(&mut piped.child, piped.stderr_drain)
    }

    /// Write `body` to a fixture script and run it.
    fn run(body: &str) -> Result<(), ProtocolError> {
        let dir = tempfile::tempdir().unwrap();
        let exe = sh_fixture(dir.path(), "child.sh", body);
        run_in(&exe, dir.path())
    }

    #[test]
    fn the_child_receives_exactly_one_envelope_line_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let capture = dir.path().join("captured");
        let exe = sh_fixture(
            dir.path(),
            "child.sh",
            &format!("cat > '{}'", capture.display()),
        );
        // The child's whole stdin is the envelope line: the capture is
        // compared after the run.
        run_in(&exe, dir.path()).expect("the fixture exits zero");
        let captured = std::fs::read_to_string(&capture).unwrap();
        assert_eq!(
            captured,
            format!("{}\n", serde_json::to_string(&envelope()).unwrap())
        );
    }

    #[test]
    fn no_argv_reaches_the_child() {
        // The child exits 7 on any argument at all: a clean exit proves the
        // spawn handed it no argv (ADR-0430 §6).
        run("if [ \"$#\" -gt 0 ]; then exit 7; fi").expect("no argv arrives");
    }

    #[test]
    fn a_clean_exit_classifies_as_success() {
        run("exit 0").expect("a clean exit is a clean run");
    }

    #[test]
    fn a_killed_child_is_a_runtime_failure_naming_the_signal() {
        let err = run("kill -9 $$").unwrap_err();
        assert_eq!(
            err,
            ProtocolError::KilledBySignal {
                signal: Some(9),
                stderr: String::new(),
            }
        );
        assert_eq!(
            err.to_string(),
            "the extension child was killed by signal 9 before finishing its MCP session"
        );
    }

    #[test]
    fn a_non_zero_exit_carries_the_childs_stderr() {
        let err = run("echo 'boom' >&2\nexit 3").unwrap_err();
        assert_eq!(
            err,
            ProtocolError::ExitFailed {
                code: 3,
                stderr: "boom".to_string(),
            }
        );
        assert_eq!(err.to_string(), "the extension child exited with 3: boom");
    }

    #[test]
    fn a_silent_non_zero_exit_names_the_code_alone() {
        let err = run("exit 4").unwrap_err();
        assert_eq!(err.to_string(), "the extension child exited with 4");
    }

    /// A compose'd kit child's failure rides its stderr: the kit's own
    /// `mcp:` text is what the host renders under its one `Error:` line —
    /// the child-failure channel ADR-0447 replaced the `error` record with.
    #[test]
    fn the_bash_kit_childs_failure_rides_its_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let exe = sh_fixture(dir.path(), "child.sh", &mcp_child_body("fixture", "exit 0"));
        // The host here writes the envelope and closes stdin, so the kit's
        // handshake reads the closed session instead of an answer.
        let err = run_in(&exe, dir.path()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the extension child exited with 1: mcp: the host closed the session \
             before the initialize response"
        );
    }

    #[test]
    fn a_missing_executable_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-child");
        let err = run_in(&missing, dir.path()).unwrap_err();
        assert!(
            matches!(err, ProtocolError::NotExecutable { .. }),
            "{err:?}"
        );
        assert_eq!(
            err.to_string(),
            format!(
                "the extension executable {} was not found or is not executable",
                missing.display()
            )
        );
    }

    #[test]
    fn a_non_executable_file_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain.sh");
        std::fs::write(&plain, "#!/bin/sh\n").unwrap();
        // Mode 644: present but not executable — the same typed refusal.
        let err = run_in(&plain, dir.path()).unwrap_err();
        assert!(
            matches!(err, ProtocolError::NotExecutable { .. }),
            "{err:?}"
        );
    }
}
