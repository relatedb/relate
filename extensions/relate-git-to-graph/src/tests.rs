//! The importer's lib-level tests (the dir-module pattern the CLI tree's
//! importers used, LABS-421): the committed manifest's contract, the
//! envelope's validation and keys, the person-name fold through real git
//! fixtures. The catalog↔code ties (the starter queries and the ADR-0377
//! numbers) are the CLI's e2e catalog suite's: a first-party child never
//! reads a surface's files.

use std::path::Path;

use serde_json::Value;

use super::*;
use crate::git_log;
use crate::plan;
use relate_test_support::git::{history_fixture, Repo, JAN_1, JAN_3};

/// The persons phase's row for `email`, as the fold's answer for that
/// person's name — the projection the store's `MATCH (p:Person)` query
/// used to read before the importer stopped touching stores.
fn person_name(built: &plan::Plan, email: &str) -> String {
    built
        .phases()
        .into_iter()
        .find(|(statement, _)| *statement == plan::WRITE_PERSONS)
        .expect("the plan has a persons phase")
        .1
        .find(|row| row["email"] == email)
        .unwrap_or_else(|| panic!("no person row for {email}"))["name"]
        .as_str()
        .expect("the person row's name is a string")
        .to_string()
}

/// Read and plan `dir`'s whole history under the shipped caps.
fn plan_for(dir: &Path) -> plan::Plan {
    let history = git_log::read_history(dir, &Opts::default(), Caps::ADR.log_bytes).unwrap();
    plan::build(dir, history, DEFAULT_MAX_CHANGESET, Caps::ADR.pairs).unwrap()
}

#[test]
fn the_committed_manifest_declares_the_contract_this_child_speaks() {
    let manifest: Value = serde_json::from_str(include_str!("../package/extension.json")).unwrap();
    assert_eq!(manifest["kind"], "command");
    assert_eq!(manifest["name"], "git-to-graph");
    // The protocol names the MCP revision the mount serves, and the
    // retired dual-wire selector is gone: a package declares the one wire
    // ADR-0447 leaves.
    assert_eq!(manifest["protocol"], relate_wire::mcp::MCP_PROTOCOL_VERSION);
    assert!(
        manifest.get("wire").is_none(),
        "the manifest carries no wire field: the mount speaks MCP stdio only"
    );
    assert_eq!(manifest["commit"], "per-batch");
    assert_eq!(manifest["positional"]["name"], "DIR");
    assert!(!manifest["requires-fresh-store"].as_bool().unwrap());
    let flags: Vec<(String, &str, Value)> = manifest["flags"]
        .as_array()
        .expect("the flag table is an array")
        .iter()
        .map(|flag| {
            (
                flag["name"].as_str().unwrap().to_string(),
                flag["kind"].as_str().unwrap(),
                flag["default"].clone(),
            )
        })
        .collect();
    // One row per flag — name, kind and default in one spelling: the
    // defaults the child lowers are `0` max-commits is all history, an
    // empty `since` or `until` is no bound (the envelope contract), and
    // the changeset default is the lib's own constant.
    assert_eq!(
        flags,
        [
            ("max-commits".to_string(), "integer", serde_json::json!(0)),
            (
                "max-changeset".to_string(),
                "integer",
                serde_json::json!(DEFAULT_MAX_CHANGESET)
            ),
            ("since".to_string(), "string", serde_json::json!("")),
            ("until".to_string(), "string", serde_json::json!("")),
        ]
    );
}

/// The bin's envelope reads the flag names the manifest declares — the
/// two spell one contract, pinned through the kit's one shared
/// assertion (the same helper the fs child's test calls) so a manifest
/// edit that breaks the child's envelope fails in this crate, not
/// mid-import.
#[test]
fn the_envelope_keys_match_the_manifest_flag_names() {
    let example = serde_json::to_value(Envelope {
        dir: Some(".".to_string()),
        max_commits: 0,
        max_changeset: DEFAULT_MAX_CHANGESET as i64,
        since: String::new(),
        until: String::new(),
    })
    .unwrap();
    let manifest: Value = serde_json::from_str(include_str!("../package/extension.json")).unwrap();
    relate_test_support::envelope::assert_envelope_keys(&example, &manifest);
}

/// The envelope's value validation, with the importer's own wording: a
/// negative commit window and a changeset outside `1..=1000` are
/// child-side refusals (the host's kind-checked parse cannot see value
/// ranges, ADR-0429 §5), and the optional-with-no-bound flags ride their
/// defaults — `0` commits is all history, an empty `since` or `until` no
/// bound.
#[test]
fn the_envelope_refuses_values_the_flag_kinds_cannot() {
    let base = Envelope {
        dir: None,
        max_commits: 0,
        max_changeset: DEFAULT_MAX_CHANGESET as i64,
        since: String::new(),
        until: String::new(),
    };
    assert_eq!(base.opts().unwrap(), Opts::default());
    let no_bound = Envelope {
        max_commits: 5,
        since: "2026-01-01T00:00:00Z".to_string(),
        ..base.clone()
    };
    assert_eq!(
        no_bound.opts().unwrap(),
        Opts {
            max_commits: Some(5),
            max_changeset: DEFAULT_MAX_CHANGESET,
            since: Some("2026-01-01T00:00:00Z".to_string()),
            until: None,
        }
    );
    // The closed range the ADR-0597 window exists for: both bounds lower
    // together, and either alone is still a bound.
    let closed = Envelope {
        since: "2018-01-01".to_string(),
        until: "2020-01-01".to_string(),
        ..base.clone()
    };
    assert_eq!(
        closed.opts().unwrap(),
        Opts {
            max_commits: None,
            max_changeset: DEFAULT_MAX_CHANGESET,
            since: Some("2018-01-01".to_string()),
            until: Some("2020-01-01".to_string()),
        }
    );
    let until_only = Envelope {
        until: "2026-01-01T00:00:00Z".to_string(),
        ..base.clone()
    };
    assert_eq!(
        until_only.opts().unwrap(),
        Opts {
            max_commits: None,
            max_changeset: DEFAULT_MAX_CHANGESET,
            since: None,
            until: Some("2026-01-01T00:00:00Z".to_string()),
        }
    );
    let negative = Envelope {
        max_commits: -1,
        ..base.clone()
    };
    assert_eq!(
        negative.opts().unwrap_err(),
        "invalid --max-commits: '-1' is not a positive integer"
    );
    for (envelope, expected) in [
        (
            Envelope {
                max_changeset: 0,
                ..base.clone()
            },
            "invalid --max-changeset: '0' is not a positive integer".to_string(),
        ),
        (
            Envelope {
                max_changeset: MAX_CHANGESET as i64 + 1,
                ..base
            },
            format!(
                "invalid --max-changeset: '{}' is above the maximum {MAX_CHANGESET}",
                MAX_CHANGESET + 1
            ),
        ),
    ] {
        assert_eq!(envelope.opts().unwrap_err(), expected);
    }
}

#[test]
fn person_name_is_the_greatest_author_date_across_a_rebase() {
    // The rebase/cherry-pick shape: `--date` keeps an older author date
    // while the committer date stays newer, and git log is committer-date
    // order, so the older author date is the first record. ADR-0376 pins
    // the name to the greatest authored_at, not to the log's first name.
    let dir = tempfile::tempdir().unwrap();
    let repo = Repo::init(dir.path());
    repo.write("a.txt", b"a");
    repo.commit_as_author("Jan Third", "ada@example.com", JAN_3, "original");
    repo.write("b.txt", b"b");
    repo.commit_as_author(
        "Jan First",
        "ada@example.com",
        JAN_1,
        "rebased with an older author date",
    );

    let history = git_log::read_history(dir.path(), &Opts::default(), MAX_LOG_BYTES).unwrap();
    assert_eq!(
        history
            .iter()
            .map(|commit| commit.author_name.as_str())
            .collect::<Vec<_>>(),
        ["Jan First", "Jan Third"],
        "the newer committer date sorts first"
    );
    assert!(history[0].authored_instant().unwrap() < history[1].authored_instant().unwrap());

    assert_eq!(
        person_name(&plan_for(dir.path()), "ada@example.com"),
        "Jan Third"
    );
}

#[test]
fn person_name_compares_instants_across_offset_author_dates() {
    // `2026-01-02T01:00:00+02:00` is 2026-01-01T23:00Z, so its local date
    // string sorts after `2026-01-01T23:30:00+00:00` while its instant is
    // earlier. The child commit's string is the log's first record too, so
    // only comparing parsed instants gives ADR-0376's greatest-instant name.
    let dir = tempfile::tempdir().unwrap();
    let repo = Repo::init(dir.path());
    repo.write("a.txt", b"a");
    repo.commit_as_author(
        "Instant Greater",
        "ada@example.com",
        "2026-01-01T23:30:00+00:00",
        "the greater instant",
    );
    repo.write("b.txt", b"b");
    repo.commit_as_author(
        "Local Date Later",
        "ada@example.com",
        "2026-01-02T01:00:00+02:00",
        "the later local date string",
    );

    let history = git_log::read_history(dir.path(), &Opts::default(), MAX_LOG_BYTES).unwrap();
    assert_eq!(
        history
            .iter()
            .map(|commit| commit.author_name.as_str())
            .collect::<Vec<_>>(),
        ["Local Date Later", "Instant Greater"],
        "the child commit is the log's first record"
    );
    assert!(
        history[0].authored_at > history[1].authored_at,
        "the local date strings sort opposite to their instants"
    );
    assert_eq!(history[0].authored_instant().unwrap(), 1_767_308_400);
    assert_eq!(history[1].authored_instant().unwrap(), 1_767_310_200);
    assert!(history[0].authored_instant().unwrap() < history[1].authored_instant().unwrap());

    assert_eq!(
        person_name(&plan_for(dir.path()), "ada@example.com"),
        "Instant Greater"
    );
}

/// The stats the canonical history fixture plans — the counts the e2e
/// suites pin through the real `run` mount, asserted here at the fold so
/// the fixture's shape and the plan's derivation cannot drift apart.
#[test]
fn the_canonical_history_fixture_plans_the_documented_counts() {
    let (dir, _repo) = history_fixture();
    let built = plan_for(dir.path());
    assert_eq!(
        built.stats(),
        Stats {
            commits: 3,
            people: 2,
            files: 4,
            touched: 6,
            co_changed: 3,
        }
    );
}
