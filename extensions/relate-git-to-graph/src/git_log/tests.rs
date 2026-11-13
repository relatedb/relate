//! The git-to-graph history reader's behavior tests, in the dir-module
//! pattern the importer's own `tests/` directory uses (LABS-421): the
//! reader's file stays under the repo's 1k-line budget with its ~15 test
//! functions and helpers beside it.

use std::path::Path;
use std::process::{Command, Stdio};

use super::*;
use crate::Opts;
use relate_test_support::git::{Repo, AUTHOR_EMAIL, AUTHOR_NAME, JAN_1, JAN_2, JAN_3};

/// Build one raw git record the way the pinned format emits it.
fn record(
    sha: &str,
    name: &str,
    email: &str,
    date: &str,
    subject: &str,
    paths: &[&str],
) -> Vec<u8> {
    let mut bytes = format!("\x1e{sha}\x1f{name}\x1f{email}\x1f{date}\x1f{subject}\0").into_bytes();
    if !paths.is_empty() {
        bytes.push(b'\n');
        for path in paths {
            bytes.extend_from_slice(path.as_bytes());
            bytes.push(0);
        }
    }
    bytes
}

#[test]
fn argv_pins_the_adr_invocation_and_appends_optional_filters() {
    let args = log_args(
        Path::new("/repo"),
        &Opts {
            max_commits: Some(5),
            since: Some("-x".to_string()),
            until: Some("-y".to_string()),
            ..Opts::default()
        },
    );
    let strings: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    for pinned in [
        "-C",
        "/repo",
        "-c",
        "log.showSignature=false",
        "--no-pager",
        "log",
        "--no-merges",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--relative",
        "--name-only",
        "-z",
        "--format=%x1e%H%x1f%aN%x1f%aE%x1f%aI%x1f%s",
    ] {
        assert!(
            strings.contains(&pinned.to_string()),
            "{pinned:?}: {strings:?}"
        );
    }
    assert!(
        strings.contains(&"--max-count=5".to_string()),
        "{strings:?}"
    );
    assert!(strings.contains(&"--since=-x".to_string()), "{strings:?}");
    assert!(strings.contains(&"--until=-y".to_string()), "{strings:?}");
    assert_eq!(&strings[strings.len() - 2..], ["--", "."]);
}

#[test]
fn argv_omits_the_optional_filters_when_absent() {
    let args = log_args(Path::new("."), &Opts::default());
    let strings: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(!strings.iter().any(|arg| arg.starts_with("--max-count")));
    assert!(!strings.iter().any(|arg| arg.starts_with("--since=")));
    assert!(!strings.iter().any(|arg| arg.starts_with("--until=")));
    assert_eq!(&strings[strings.len() - 2..], ["--", "."]);
}

#[test]
fn parser_reads_two_commits_with_files() {
    let stdout = [
        record(
            "a".repeat(40).as_str(),
            "Ada Lovelace",
            "ada@example.com",
            "2026-01-02T03:04:05Z",
            "first: \"quoted\": subject",
            &["src/a.txt", "src/with space.txt", "src/å.txt"],
        ),
        record(
            "b".repeat(40).as_str(),
            "Grace Hopper",
            "grace@example.com",
            "2026-01-03T03:04:05Z",
            "second",
            &["src/b.txt"],
        ),
    ]
    .concat();
    let commits = parse_log(&stdout).unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].sha, "a".repeat(40));
    assert_eq!(commits[0].author_name, "Ada Lovelace");
    assert_eq!(commits[0].author_email, "ada@example.com");
    assert_eq!(commits[0].authored_at, "2026-01-02T03:04:05Z");
    assert_eq!(commits[0].subject, "first: \"quoted\": subject");
    assert_eq!(
        commits[0].paths,
        ["src/a.txt", "src/with space.txt", "src/å.txt"]
    );
    assert_eq!(commits[1].paths, ["src/b.txt"]);
}

#[test]
fn parser_reads_a_commit_with_zero_files() {
    let stdout = record(
        "c".repeat(40).as_str(),
        "Ada Lovelace",
        "ada@example.com",
        "2026-01-02T03:04:05Z",
        "empty",
        &[],
    );
    let commits = parse_log(&stdout).unwrap();
    assert_eq!(commits.len(), 1);
    assert!(commits[0].paths.is_empty());
    assert_eq!(commits[0].subject, "empty");
}

#[test]
fn parser_accepts_empty_output_as_an_empty_history() {
    assert!(parse_log(b"").unwrap().is_empty());
}

#[test]
fn parser_names_a_malformed_record() {
    let malformed = b"\x1esha-only\x1ftoo-few-fields\0";
    let err = parse_log(malformed).unwrap_err();
    assert!(err.contains("record 1"), "{err}");
    assert!(err.contains("expected 5 header fields"), "{err}");

    let no_newline = b"\x1esha\x1fname\x1fmail\x1fdate\x1fsubject\0path\0";
    let err = parse_log(no_newline).unwrap_err();
    assert!(err.contains("record 1"), "{err}");
    assert!(err.contains("newline"), "{err}");

    let unterminated = b"\x1esha\x1fname\x1fmail\x1fdate\x1fsubject\0\npath";
    let err = parse_log(unterminated).unwrap_err();
    assert!(err.contains("record 1"), "{err}");
    assert!(err.contains("NUL-terminated"), "{err}");

    let empty_path = b"\x1esha\x1fname\x1fmail\x1fdate\x1fsubject\0\n\0";
    let err = parse_log(empty_path).unwrap_err();
    assert!(err.contains("record 1"), "{err}");
    assert!(err.contains("path 1 is empty"), "{err}");
}

#[test]
fn parser_rejects_output_that_does_not_start_with_a_record_separator() {
    let err = parse_log(b"sha\x1fname\x1fmail\x1fdate\x1fsubject\0").unwrap_err();
    assert!(err.contains("missing record separator"), "{err}");
}

/// One commit carrying only an author date, for the instant tests.
fn dated(value: &str) -> GitCommit {
    GitCommit {
        sha: "a".repeat(40),
        author_name: "Ada Lovelace".to_string(),
        author_email: "ada@example.com".to_string(),
        authored_at: value.to_string(),
        subject: "subject".to_string(),
        paths: vec![],
    }
}

#[test]
fn authored_instant_applies_the_offset() {
    let instant = |value: &str| dated(value).authored_instant();
    let utc = instant("2026-01-02T03:04:05Z").unwrap();
    assert_eq!(instant("2026-01-02T05:04:05+02:00").unwrap(), utc);
    assert_eq!(instant("2026-01-02T02:04:05-01:00").unwrap(), utc);
    assert_eq!(instant("2026-01-01T22:04:05-05:00").unwrap(), utc);
    assert_eq!(instant("1970-01-01T00:00:00Z").unwrap(), 0);
    assert_eq!(instant("2024-02-29T00:00:00Z").unwrap(), 1_709_164_800);
    assert!(
        instant("2026-01-02T03:04:06Z").unwrap() > utc,
        "instants must order by the moment they name"
    );
}

#[test]
fn authored_instant_orders_by_instant_not_by_local_spelling() {
    // 2026-01-02T01:00:00+02:00 is 2026-01-01T23:00:00Z; the later
    // instant is the +00:00 one, whose local spelling sorts first.
    let offset = "2026-01-02T01:00:00+02:00";
    let utc = "2026-01-01T23:30:00+00:00";
    assert!(utc < offset, "the local spellings must sort the other way");
    let offset = dated(offset).authored_instant().unwrap();
    let utc = dated(utc).authored_instant().unwrap();
    assert!(utc > offset, "{utc} must be later than {offset}");
    assert_eq!(utc - offset, 1_800);
}

#[test]
fn authored_instant_rejects_malformed_dates() {
    for value in [
        "",
        "2026-01-02",
        "2026-01-02T03:04:05",
        "2026-13-02T03:04:05Z",
        "2026-02-30T03:04:05Z",
        "2026-01-02T24:04:05Z",
        "2026-01-02T03:04:60Z",
        "2026-01-02T03:04:05+2:00",
        "2026-01-02T03:04:05+24:00",
        "2026-01-02T03:04:05Z ",
    ] {
        let err = dated(value).authored_instant().unwrap_err();
        assert_eq!(
            err,
            format!("git returned an invalid author date: {value}"),
            "{value:?} must be rejected by name"
        );
    }
}

#[test]
fn a_missing_git_program_is_the_typed_refusal() {
    // The authored refusal carries its own variant (LABS-421), the way the
    // index failure does: asserted by variant, with the exact wording
    // pinned once here for the `Display` arm that renders it.
    let mut command = Command::new("relate-no-such-git-program");
    let err = run_command(&mut command, u64::MAX).unwrap_err();
    assert_eq!(err, ImportError::GitNotInstalled);
    assert_eq!(
        err.to_string(),
        "git was not found on PATH; install git and retry"
    );
}

#[test]
fn a_non_zero_exit_carries_trimmed_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-directory");
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(&missing)
        .arg("status")
        .stdin(Stdio::null());
    let err = run_command(&mut command, u64::MAX).unwrap_err().to_string();
    assert!(err.contains(&missing.display().to_string()), "{err}");
    assert!(!err.ends_with('\n'), "stderr must be trimmed: {err:?}");
}

#[test]
fn read_history_maps_the_byte_past_the_cap_to_log_too_large() {
    // One byte under the real output's length: the read stops at that
    // byte, the child is killed and reaped, and the ADR-0377 error names
    // the cap unchanged.
    let dir = tempfile::tempdir().unwrap();
    let repo = Repo::init(dir.path());
    repo.write("a.txt", b"a");
    repo.commit("first");
    repo.write("b.txt", b"b");
    repo.commit("second");
    let mut command = Command::new("git");
    command.args(log_args(dir.path(), &Opts::default()));
    let full = command.output().unwrap();
    assert!(full.status.success());
    let cap = full.stdout.len() as u64 - 1;

    let err = read_history(dir.path(), &Opts::default(), cap).unwrap_err();
    assert_eq!(err, ImportError::LogTooLarge { limit: cap });
    assert_eq!(
        err.to_string(),
        format!(
            "git log output exceeds the {cap}-byte import limit ({} MiB); \
             pass --max-commits, --since or --until to read a smaller window",
            cap / (1024 * 1024)
        )
    );
}

#[test]
fn run_command_reads_output_exactly_at_the_cap() {
    let version = Command::new("git").arg("--version").output().unwrap();
    assert!(version.status.success());
    let mut command = Command::new("git");
    command.arg("--version").stdin(Stdio::null());
    let stdout = run_command(&mut command, version.stdout.len() as u64).unwrap();
    assert_eq!(stdout, version.stdout);
}

#[test]
fn read_history_uses_real_git_output() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repo::init(dir.path());
    repo.write("with space.txt", b"one");
    let first = repo.commit("first: \"quoted\"");
    repo.write("å.txt", b"two");
    repo.write("with space.txt", b"changed");
    let second = repo.commit("second");

    let commits = read_history(dir.path(), &Opts::default(), u64::MAX).unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].sha, second);
    assert_eq!(commits[0].paths, ["with space.txt", "å.txt"]);
    assert_eq!(commits[0].author_name, "Ada Lovelace");
    assert_eq!(commits[1].sha, first);
    assert_eq!(commits[1].paths, ["with space.txt"]);
    assert_eq!(commits[1].subject, "first: \"quoted\"");
}

/// The three-commit dated ladder the window tests read: oldest, middle
/// and newest at JAN_1, JAN_2 and JAN_3, one file changed per commit, all
/// by one author, returning the newest sha. `commit_as` sets author and
/// committer date alike, and the committer date is what `--since` and
/// `--until` filter, so the ladder orders the way the windows cut it.
fn dated_ladder(repo: &Repo) -> String {
    repo.write("a.txt", b"a");
    repo.commit_as(AUTHOR_NAME, AUTHOR_EMAIL, JAN_1, "oldest");
    repo.write("a.txt", b"b");
    repo.commit_as(AUTHOR_NAME, AUTHOR_EMAIL, JAN_2, "middle");
    repo.write("a.txt", b"c");
    repo.commit_as(AUTHOR_NAME, AUTHOR_EMAIL, JAN_3, "newest")
}

/// The history's subjects in log order — the projection every window
/// test asserts, spelled once.
fn subjects(history: &[GitCommit]) -> Vec<&str> {
    history
        .iter()
        .map(|commit| commit.subject.as_str())
        .collect()
}

#[test]
fn read_history_applies_max_commits_and_since() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repo::init(dir.path());
    let newest = dated_ladder(&repo);

    let one = read_history(
        dir.path(),
        &Opts {
            max_commits: Some(1),
            ..Opts::default()
        },
        u64::MAX,
    )
    .unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].sha, newest);

    let recent = read_history(
        dir.path(),
        &Opts {
            since: Some("2026-01-02T00:00:00Z".to_string()),
            ..Opts::default()
        },
        u64::MAX,
    )
    .unwrap();
    assert_eq!(subjects(&recent), ["newest", "middle"]);
}

#[test]
fn read_history_applies_until_for_an_era_and_a_closed_range() {
    // The ADR-0597 shapes: `--until` alone reads an era that need not end
    // at HEAD, and `--since A --until B` is a closed, reproducible range —
    // both filter committer date, exactly as `--since` does.
    let dir = tempfile::tempdir().unwrap();
    let repo = Repo::init(dir.path());
    dated_ladder(&repo);

    let era = read_history(
        dir.path(),
        &Opts {
            until: Some("2026-01-02T00:00:00Z".to_string()),
            ..Opts::default()
        },
        u64::MAX,
    )
    .unwrap();
    assert_eq!(
        subjects(&era),
        ["middle", "oldest"],
        "an --until window reads an era that stops before HEAD"
    );

    let closed = read_history(
        dir.path(),
        &Opts {
            since: Some("2026-01-01T00:00:01Z".to_string()),
            until: Some("2026-01-02T00:00:01Z".to_string()),
            ..Opts::default()
        },
        u64::MAX,
    )
    .unwrap();
    assert_eq!(
        subjects(&closed),
        ["middle"],
        "a closed --since/--until range reads exactly the middle commit"
    );
}
