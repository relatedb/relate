//! The `extension` verb's argument-grammar tests (ADR-0431 §3): the
//! closed action vocabulary, the one-positional rule, the source-flag
//! mutual exclusion, and the per-build `--available` and not-included
//! shapes. The flow behaviors (install, list, remove) are pinned in
//! `extensions::install`'s tests and end to end in `tests/e2e_extensions.rs`.

use super::*;
use crate::args;
use crate::extensions::source::GithubPin;

fn err_of(xs: &[&str]) -> String {
    parse_extension_args(&args(xs)).unwrap_err().to_string()
}

#[test]
fn the_grammar_reads_each_action() {
    // `init` parses in every build that carries `extensions` — it is
    // dependency-free (ADR-0137: the grammar is ungated within the command).
    let parsed = parse_extension_args(&args(&["init", "greet"])).unwrap();
    let ExtensionAction::Init(spec) = &parsed else {
        panic!("the init action");
    };
    assert_eq!(spec.name, "greet");
    assert_eq!(
        spec.language,
        crate::extension_init::templates::Language::Py
    );
    assert_eq!(parsed.as_str(), "init");
    #[cfg(feature = "extension-install")]
    {
        let parsed = parse_extension_args(&args(&["install", "greet"])).unwrap();
        assert!(matches!(parsed, ExtensionAction::Install(_)));
        assert_eq!(parsed.as_str(), "install");
    }
    let parsed = parse_extension_args(&args(&["list"])).unwrap();
    assert_eq!(parsed, ExtensionAction::List(ListMode::Installed));
    assert_eq!(parsed.as_str(), "list");
    let parsed = parse_extension_args(&args(&["remove", "greet"])).unwrap();
    assert_eq!(
        parsed,
        ExtensionAction::Remove {
            name: "greet".into(),
            forget_pin: false
        }
    );
    assert_eq!(parsed.as_str(), "remove");
    let parsed = parse_extension_args(&args(&["check", "./my-extension"])).unwrap();
    assert_eq!(
        parsed,
        ExtensionAction::Check {
            dir: "./my-extension".into()
        }
    );
    assert_eq!(parsed.as_str(), "check");
}

#[test]
fn a_missing_or_unknown_action_is_a_usage_error() {
    assert_eq!(
        err_of(&[]),
        "extension requires an action (init, install, list, remove or check)"
    );
    assert_eq!(
        err_of(&["upgrade"]),
        "unexpected argument to extension: upgrade"
    );
}

#[test]
fn install_reads_its_flags_in_any_order() {
    let parsed =
        parse_extension_args(&args(&["install", "greet", "--from", "/pkg", "--force"])).unwrap();
    let ExtensionAction::Install(spec) = &parsed else {
        panic!("the install action");
    };
    assert_eq!(spec.name, "greet");
    assert_eq!(spec.source, InstallSource::From("/pkg".to_string()));
    assert!(spec.force);
    let parsed = parse_extension_args(&args(&["install", "greet", "--version", "1.2.3"])).unwrap();
    let ExtensionAction::Install(spec) = &parsed else {
        panic!("the install action");
    };
    assert_eq!(
        spec.source,
        InstallSource::Channel {
            version: Some("1.2.3".to_string())
        }
    );
    // No source flags at all is the channel source, newest listed — the
    // typed variant the parse produced, never a run-time derivation.
    let parsed = parse_extension_args(&args(&["install", "greet"])).unwrap();
    let ExtensionAction::Install(spec) = &parsed else {
        panic!("the install action");
    };
    assert_eq!(spec.source, InstallSource::Channel { version: None });
    assert!(!spec.force);
}

#[cfg(feature = "extension-install")]
#[test]
fn install_rejects_each_grammar_violation() {
    for (case, argv, needle) in [
        (
            "no name",
            &["install"][..],
            "extension install requires a package name",
        ),
        (
            "two positionals",
            &["install", "a", "b"],
            "unexpected argument to extension install: b",
        ),
        (
            "an unknown flag",
            &["install", "greet", "--bogus"],
            "unexpected argument to extension install: --bogus",
        ),
        (
            "--url and --from together",
            &["install", "greet", "--url", "u", "--from", "d"],
            "--url and --from are mutually exclusive (they both name the install source)",
        ),
        (
            "--version beside --url",
            &["install", "greet", "--url", "u", "--version", "1"],
            "--version is not valid with --url, --from or --github (the named source \
             already determines the version)",
        ),
        (
            "--version beside --from",
            &["install", "greet", "--from", "d", "--version", "1"],
            "--version is not valid with --url, --from or --github (the named source \
             already determines the version)",
        ),
        (
            "a missing --url value",
            &["install", "greet", "--url"],
            "--url requires a value (the package tarball URL)",
        ),
        (
            "a repeated --url",
            &["install", "greet", "--url", "u", "--url", "v"],
            "extension install --url may be given only once",
        ),
        (
            "a repeated --force",
            &["install", "greet", "--force", "--force"],
            "extension install --force may be given only once",
        ),
    ] {
        let err = err_of(argv);
        assert!(err.contains(needle), "case {case}: {err}");
    }
}

#[test]
fn the_install_action_parses_in_every_build() {
    // ADR-0137: the grammar is ungated — the flags parse identically in
    // every build; the capability (the install itself) is what the
    // feature gates, failing closed at run time in a build without it.
    let parsed =
        parse_extension_args(&args(&["install", "greet", "--url", "x", "--force"])).unwrap();
    assert_eq!(
        parsed,
        ExtensionAction::Install(InstallArgs {
            name: "greet".into(),
            source: InstallSource::Url("x".into()),
            force: true,
            re_approve: false,
            approve: None,
        })
    );
    assert_eq!(parsed.as_str(), "install");
}

#[test]
fn list_reads_available_once() {
    // `--available` parses in every build (ADR-0137); a build without
    // `extension-install` answers it at run time with the not-included
    // error — pinned end to end in `tests/e2e_bundles.rs`.
    let parsed = parse_extension_args(&args(&["list", "--available"])).unwrap();
    assert_eq!(parsed, ExtensionAction::List(ListMode::Available));
    assert_eq!(
        err_of(&["list", "--available", "--available"]),
        "extension list --available may be given only once"
    );
}

#[test]
fn list_and_remove_reject_extras() {
    assert_eq!(
        err_of(&["list", "extra"]),
        "unexpected argument to extension list: extra"
    );
    assert_eq!(
        err_of(&["remove"]),
        "extension remove requires a package name"
    );
    // `remove` carries no --force (ADR-0431 §3).
    assert_eq!(
        err_of(&["remove", "greet", "--force"]),
        "unexpected argument to extension remove: --force"
    );
}

/// `check` takes one positional and no flags (ADR-0137's ungated grammar
/// like every action): the directory is the whole invocation, and there is
/// no store flag — the verb opens no store, so `--database` is an
/// unexpected argument, never a target to check a package against.
#[test]
fn check_reads_one_directory_and_no_flags() {
    assert_eq!(
        err_of(&["check"]),
        "extension check requires a package directory"
    );
    assert_eq!(
        err_of(&["check", "a", "b"]),
        "unexpected argument to extension check: b"
    );
    for flag in ["--database", "-d", "--db", "--force"] {
        assert_eq!(
            err_of(&["check", flag, "/tmp/store"]),
            format!("unexpected argument to extension check: {flag}"),
            "{flag}"
        );
    }
    // A directory whose name starts with a dash stays reachable the way
    // every verb's does — through a `./` prefix — so the flag rule costs
    // no path.
    assert_eq!(
        parse_extension_args(&args(&["check", "./-odd"])).unwrap(),
        ExtensionAction::Check {
            dir: "./-odd".into()
        }
    );
}

/// The list row's package-authored fragments render sanitized — the
/// `print_error` doctrine applied to the listing: a manifest whose name
/// and version carry control characters lists one honest row, never a
/// forged second line or ANSI.
#[test]
fn the_list_row_sanitizes_its_package_authored_fragments() {
    use crate::extensions::registry::ActionRegistry;
    use crate::extensions::testkit::install_package;
    use crate::telemetry::env_of;

    let tmp = tempfile::tempdir().unwrap();
    install_package(
        &tmp,
        "hostile",
        r#"{"kind":"command","name":"gr\neet","version":"1.2\u001b","protocol":"2025-06-18","flags":[],"commit":"per-batch","executable":"run.sh"}"#,
    );
    let xdg = tmp.path().to_string_lossy().into_owned();
    let registry = ActionRegistry::load_with(&env_of(&[("XDG_CONFIG_HOME", xdg.as_str())]))
        .expect("the hostile package loads");
    let entry = registry.find("gr\neet").expect("the mount name resolves");
    // The newline renders as a space, the escape is dropped: one row,
    // the package's own words tamed.
    assert_eq!(list_row(entry).unwrap(), "gr eet 1.2 unmarked");
}

// --- the GitHub pin source (ADR-0449) ---

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn github_parses_a_tag_or_a_commit_and_carries_a_re_approval() {
    let parsed = parse_extension_args(&args(&[
        "install",
        "greet",
        "--github",
        "owner/repo@v1.2.3",
    ]))
    .unwrap();
    let ExtensionAction::Install(spec) = &parsed else {
        panic!("the install action");
    };
    assert_eq!(
        spec.source,
        InstallSource::Github(GithubSpec {
            repo: "owner/repo".to_string(),
            pin: GithubPin::Tag("v1.2.3".to_string()),
        })
    );
    assert!(!spec.re_approve && spec.approve.is_none());

    let parsed = parse_extension_args(&args(&[
        "install",
        "greet",
        "--github",
        &format!("owner/repo@{COMMIT}"),
        "--approve",
        &COMMIT.to_ascii_uppercase(),
    ]))
    .unwrap();
    let ExtensionAction::Install(spec) = &parsed else {
        panic!("the install action");
    };
    assert_eq!(
        spec.source,
        InstallSource::Github(GithubSpec {
            repo: "owner/repo".to_string(),
            pin: GithubPin::Commit(COMMIT.to_string()),
        })
    );
    // `--approve` normalizes the commit spelling.
    assert_eq!(spec.approve.as_deref(), Some(COMMIT));

    let parsed = parse_extension_args(&args(&[
        "install",
        "greet",
        "--github",
        "owner/repo@v1",
        "--re-approve",
    ]))
    .unwrap();
    let ExtensionAction::Install(spec) = &parsed else {
        panic!("the install action");
    };
    assert!(spec.re_approve);
    assert!(spec.approve.is_none());
}

#[test]
fn github_grammar_violations_are_usage_errors() {
    for (case, argv, needle) in [
        (
            "an untagged repository",
            &["install", "greet", "--github", "owner/repo"][..],
            "an untagged repository is a moving target",
        ),
        (
            "a malformed slug",
            &["install", "greet", "--github", "owner@v1"],
            "is not an owner/repo repository slug",
        ),
        (
            "a missing pin value",
            &["install", "greet", "--github"],
            "--github requires a value (owner/repo@tag or owner/repo@<full-commit-sha>)",
        ),
        (
            "a repeated --github",
            &[
                "install", "greet", "--github", "o/r@v1", "--github", "o/r@v2",
            ],
            "extension install --github may be given only once",
        ),
        (
            "--github beside --url",
            &["install", "greet", "--github", "o/r@v1", "--url", "u"],
            "--github is mutually exclusive with --url and --from (each names the install source)",
        ),
        (
            "--github beside --version",
            &["install", "greet", "--github", "o/r@v1", "--version", "1"],
            "--version is not valid with --url, --from or --github",
        ),
        (
            "--re-approve without --github",
            &["install", "greet", "--from", "d", "--re-approve"],
            "--re-approve and --approve <commit-sha> only apply to --github installs",
        ),
        (
            "--approve without --github",
            &["install", "greet", "--url", "u", "--approve", COMMIT],
            "--re-approve and --approve <commit-sha> only apply to --github installs",
        ),
        (
            "--re-approve beside --approve",
            &[
                "install",
                "greet",
                "--github",
                "o/r@v1",
                "--re-approve",
                "--approve",
                COMMIT,
            ],
            "--re-approve and --approve are mutually exclusive (both re-approve a resolved pin)",
        ),
        (
            "a short --approve value",
            &[
                "install",
                "greet",
                "--github",
                "o/r@v1",
                "--approve",
                "abc123",
            ],
            "--approve requires a full 40-hex commit sha",
        ),
    ] {
        let err = err_of(argv);
        assert!(err.contains(needle), "case {case}: {err}");
    }
}

#[test]
fn list_trust_and_remove_forget_pin_parse_in_every_build() {
    assert_eq!(
        parse_extension_args(&args(&["list", "--trust"])).unwrap(),
        ExtensionAction::List(ListMode::Trust)
    );
    assert_eq!(
        err_of(&["list", "--trust", "--available"]),
        "extension list: --trust and --available ask different questions (recorded pins vs \
         the channel catalog)"
    );
    assert_eq!(
        parse_extension_args(&args(&["remove", "greet", "--forget-pin"])).unwrap(),
        ExtensionAction::Remove {
            name: "greet".into(),
            forget_pin: true
        }
    );
}
