//! The `lab` retirement's observable contract (ADR-0431 §12, the
//! 2026-10-28 user direction): a clean break — no stub, no compat,
//! nothing recognizes `lab` afterward. The word answers exactly as a
//! never-existing word does: a bare invocation falls into the
//! path-first form and fails as a Cypher parse error, and an action
//! tail fails as the unexpected-argument usage error. The suite runs
//! in every bundle — `lab` left `COMMAND_WORDS`, the one list shared
//! by every build — and the other half of the retirement, `run
//! <importer>` performing the same import through the extension
//! mount, is the importer suites' (`e2e_fs_to_graph.rs`,
//! `e2e_git_to_graph.rs`, ADR-0431 §11).

use crate::support;

use support::{run_cli, tempdir, CliRunOptions as Opts};

/// A bare first positional the CLI has never known falls into the
/// path-first form and fails as a Cypher parse error (exit 6). The
/// control word proves `lab` gets the standard never-existing-word
/// treatment, not a retirement special case, and the absence of a
/// did-you-mean line pins that nothing steers the retired word
/// anywhere — no stub, no migration hint.
#[test]
fn a_bare_lab_fails_as_the_plain_never_word_parse_error() {
    let home = tempdir();
    for word in ["lab", "wombat"] {
        let cwd = tempdir();
        let (status, _stdout, stderr) =
            run_cli(Opts::new(&[word]).in_home(home.path()).in_dir(cwd.path()));
        assert_eq!(status.code(), Some(6), "{word}: {stderr}");
        assert!(
            stderr.starts_with(&format!(
                "syntax error: parse error: unexpected token \"{word}\""
            )),
            "{word}: {stderr}"
        );
        assert!(
            !stderr.contains("did you mean"),
            "{word}: nothing recognizes the retired word: {stderr}"
        );
        assert_eq!(
            std::fs::read_dir(cwd.path()).unwrap().count(),
            0,
            "{word} must not create anything"
        );
    }
}

/// An action tail after `lab` fails as the unexpected-argument usage
/// error (exit 2): the first positional takes the query slot, so the
/// action word is the unexpected argument. Every action the surface
/// carried answers the same way — the importers and the hidden `neo`
/// egg alike (the egg is deleted entirely, ADR-0431 §11) — and the
/// invocation opens no database.
#[test]
fn a_lab_action_tail_fails_as_the_unexpected_argument() {
    let home = tempdir();
    for action in ["fs-to-graph", "git-to-graph", "neo"] {
        let cwd = tempdir();
        let (status, stdout, stderr) = run_cli(
            Opts::new(&["lab", action, "."])
                .in_home(home.path())
                .in_dir(cwd.path()),
        );
        assert_eq!(status.code(), Some(2), "lab {action}: {stderr}");
        assert!(stdout.is_empty(), "lab {action}: stdout: {stdout}");
        assert!(
            stderr.starts_with(&format!("Error: unexpected argument: {action}")),
            "lab {action}: {stderr}"
        );
        assert!(
            stderr.contains("Usage: relate [OPTIONS] [--database PATH] [QUERY]"),
            "lab {action}: {stderr}"
        );
        assert_eq!(
            std::fs::read_dir(cwd.path()).unwrap().count(),
            0,
            "lab {action} must not create a database"
        );
    }
}
