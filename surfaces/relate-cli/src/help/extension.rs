//! The `extension` command help page (ADR-0431 §3, ADR-0449): the shared
//! synopsis, its actions and flags, and the trust tiers — rendered from one
//! per-action descriptor table, the `serve` page's surface-table pattern
//! (LABS-405).
//!
//! One descriptor per action states its synopsis line, its did-you-mean
//! flags, its help rows and its example, so the usage, the flag list, the
//! examples and the page body cannot drift apart; the per-build difference is
//! one table entry, not a cfg pair per view. `install` rides the
//! `extension-install` feature (ADR-0431 §3 — network dependencies gate the
//! install verbs only; a build with `extensions` but without it keeps `init`,
//! `list`, `remove` and `check` and answers `install` with the not-included
//! error for the action).

use crate::examples::Example;

/// One `extension` action's facts, stated once.
struct ExtensionAction {
    /// The action's synopsis line, without the `Usage:` prefix or the
    /// continuation indent. `None` for an action that parses but cannot run
    /// in this build — it gets a page row and no synopsis segment.
    line: Option<&'static str>,
    /// The flags this action contributes to the did-you-mean vocabulary — the
    /// same set the action's rows print.
    flags: &'static [&'static str],
    /// The page's rows for the action and its flags, columns indented as the
    /// page prints them, no trailing newline.
    rows: &'static str,
    /// The one example this action contributes, when it contributes one.
    example: Option<Example>,
}

/// The author's first verb, and the one flag it takes: `init` leads every
/// build's block — scaffolding needs no network and no store.
const INIT: ExtensionAction = ExtensionAction {
    line: Some("relate extension init <name> [--lang sh|py|ts|rust]"),
    flags: &["--lang"],
    rows: "  init <name>     Scaffold a package directory — extension.json, an executable
                  that is a generated MCP client (the glue is inlined; nothing to
                  install), and a README quickstart; then `extension install <name>
                  --from <name>/`
  --lang L       Template language: sh, py (default), ts or rust",
    example: Some((
        "Scaffold a Python extension package",
        "relate extension init my-ext",
    )),
};

/// The install action — in the build that carries the capability.
#[cfg(feature = "extension-install")]
const INSTALL: ExtensionAction = ExtensionAction {
    line: Some(
        "relate extension install <name> [--github REPO@TAG|@SHA] [--url URL] \
         [--from DIR] [--version V] [--force] [--re-approve | --approve SHA]",
    ),
    flags: &[
        "--github",
        "--url",
        "--from",
        "--version",
        "--force",
        "--re-approve",
        "--approve",
    ],
    rows: "  install <name>  Install a package: from the signed first-party channel
                  by default; --github REPO@TAG|@SHA is a GitHub release pinned to a commit;
                  --url URL is the explicit third-party trust act; --from DIR the
                  local-directory one (no network, no signature). The tarball is
                  verified (channel: signature + SHA256; GitHub: the resolved asset SHA256),
                  unpacked under the closed contract, and load-checked before it lands —
                  never a silent fetch
  --github R@T   Install the package from a GitHub release: R is owner/repo, T a release
                  tag or a full 40-hex commit sha. Resolution records the tag's commit
                  and the platform asset's SHA256; a moved tag or changed asset refuses
                  before any fetch unless re-approved
  --url URL      Install the tarball at URL, https only (third-party: you named the
                  source; a non-https URL refuses before any fetch)
  --from DIR     Install the unpacked package directory (local: the directory is the
                  trust act, like pull --from; the executable must resolve
                  inside it)
  --version V    Install channel version V (default: the newest listed)
  --force        Clobber a target directory without the install marker (never a
                  name collision or a tier's own reinstall path)
  --re-approve   With --github: re-approve whatever the pin resolves to now, replacing a
                  recorded pin that moved (the new asset must verify first)
  --approve SHA  With --github: re-approve only the commit SHA the pin resolves to;
                  any other resolution refuses",
    example: Some((
        "Install a first-party extension from the signed channel",
        "relate extension install git-to-graph",
    )),
};

/// The install action in the build without the capability: it parses (ADR-0137)
/// and answers the not-included error, so the page keeps its row and says why
/// it cannot run — and takes no synopsis segment.
#[cfg(not(feature = "extension-install"))]
const INSTALL_UNAVAILABLE: ExtensionAction = ExtensionAction {
    line: None,
    flags: &[],
    rows: "  install         not included in this build (enable the \"extension-install\" feature);
                  init, list, remove and check are the dependency-free actions",
    example: None,
};

/// The listing action, dependency-free in every build: the installed set, the
/// channel's catalog, or the recorded GitHub pins (the fetch fails closed
/// offline; every flag parses everywhere — ADR-0137, ADR-0449).
const LIST: ExtensionAction = ExtensionAction {
    line: Some("relate extension list [--available | --trust]"),
    flags: &["--available", "--trust"],
    rows: "  list          Print the installed set — name, version, tier, source; offline,
                  dependency-free, never a fetch
  list --available
                  Print the channel's catalog (the signed extensions.json index): one row
                  per package — name, newest version, protocol, installed — hidden
                  packages omitted. Offline it fails closed naming the index URL
  list --trust
                  Print the recorded GitHub pins — name, repo, tag, commit, asset SHA256 —
                  from $XDG_CONFIG_HOME/relate/extensions-trust.json; offline",
    example: None,
};

/// The remove action, dependency-free in every build.
const REMOVE: ExtensionAction = ExtensionAction {
    line: Some("relate extension remove <name> [--forget-pin]"),
    flags: &["--forget-pin"],
    rows: "  remove <name>  Remove the installed package (the provenance marker is required;
                  a foreign or unknown directory is a refusal, and there is no --force)
  remove --forget-pin
                  Also clear the name's recorded GitHub pin. Without it the pin outlives
                  the package, so a reinstall is still compared against it",
    example: None,
};

/// The check action — dependency-free like `list` and `remove`: it opens no
/// store, fetches nothing and runs no tool.
const CHECK: ExtensionAction = ExtensionAction {
    line: Some("relate extension check <dir>"),
    flags: &[],
    rows: "  check <dir>   Run a package directory through the mount's six stages — manifest,
                  executable, spawn, initialize, notifications/initialized, tools/list —
                  printing one line per stage, then one on the recorded GitHub pin
                  (approved / drifted / not from a GitHub source; the pin line never
                  changes the exit code). It opens no store, fetches nothing and runs no
                  tool, so it is safe before installing: exit 2 for a manifest or
                  packaging problem, 1 for what the package does when run, 0 when it
                  would mount",
    example: Some((
        "Check a package directory against the mount's contract before installing it",
        "relate extension check ./my-extension",
    )),
};

/// Every action this build carries, in help order: `init` leads (the author's
/// first verb), then install (or its not-included row), then the listing
/// actions, with `check` last.
const ACTIONS: &[ExtensionAction] = &[
    INIT,
    #[cfg(feature = "extension-install")]
    INSTALL,
    #[cfg(not(feature = "extension-install"))]
    INSTALL_UNAVAILABLE,
    LIST,
    REMOVE,
    CHECK,
];

/// The `extension` synopsis, shared by its help page and its usage errors:
/// every running action's line, the continuation lines at the lab/serve
/// pages' seven-space indent.
pub(super) fn extension_usage() -> String {
    let mut usage = String::from("Usage: ");
    let mut lines = ACTIONS.iter().filter_map(|action| action.line);
    if let Some(first) = lines.next() {
        usage.push_str(first);
    }
    for line in lines {
        usage.push_str("\n       ");
        usage.push_str(line);
    }
    usage
}

/// The page's flag vocabulary for did-you-mean suggestions: every compiled-in
/// action's flags — the closed flag set of this build.
pub(super) fn extension_flags() -> Vec<&'static str> {
    ACTIONS
        .iter()
        .flat_map(|action| action.flags)
        .copied()
        .collect()
}

/// An action's example as a `const` value — the table's one optional read.
const fn example_of(action: &ExtensionAction) -> Example {
    match action.example {
        Some(example) => example,
        None => panic!("this action carries an example"),
    }
}

/// The page's examples, from the table: `init` in every build, then the
/// per-build second example (`install` where it runs, `check` otherwise) —
/// `help::tests` pins the one-or-two range.
pub(super) fn examples() -> &'static [Example] {
    const EXAMPLES: &[Example] = &[
        example_of(&INIT),
        #[cfg(feature = "extension-install")]
        example_of(&INSTALL),
        #[cfg(not(feature = "extension-install"))]
        example_of(&CHECK),
    ];
    EXAMPLES
}

/// The `extension` help page: the shared synopsis, the actions, the trust
/// tiers, and the offline story (ADR-0431, ADR-0449).
///
/// The rows come from the table; the fixed prose is assembled from
/// plain-string pieces rather than one `format!` literal, because a
/// `\`-continued literal strips the next line's leading whitespace and would
/// flatten the action table's two-column layout.
pub(super) fn extension_help() -> String {
    let mut page = extension_usage();
    page.push_str(
        "\n\
         \n\
         extension manages the command-kind extensions that mount as `relate run <name>`.\n\
         Packages live under $XDG_CONFIG_HOME/relate/extensions/<name>/.\n\
         \n\
         Actions:\n",
    );
    for action in ACTIONS {
        page.push_str(action.rows);
        page.push('\n');
    }
    page.push_str(
        "\n\
         Trust tiers: first-party (the signed channel, verified against the release\n\
         signing key upgrade verifies), github-pin (--github: the release's tag, commit\n\
         and asset SHA256 are recorded on first install — the invocation is the approval;\n\
         a moved tag or changed asset refuses without --re-approve/--approve), third-party\n\
         (--url: the invocation is the trust act, the install line names what landed),\n\
         local (--from: whoever provisions the directory owns what is in it).\n",
    );
    page
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::help::{command_help, HelpTopic};

    /// One page per build: the synopsis the page opens with is the per-build
    /// usage, exactly one `Usage:` line pair, and every action's rows and
    /// flags are the page's.
    #[test]
    fn extension_help_lists_the_per_build_synopsis_and_its_actions() {
        let help = command_help(HelpTopic::Extension);
        crate::help::assert_single_usage(&help, &extension_usage());
        for needle in [
            "init <name>",
            "--lang",
            "list",
            "remove",
            "check <dir>",
            "Trust tiers",
            "never a fetch",
            "github-pin",
            "--trust",
            "--forget-pin",
        ] {
            assert!(help.contains(needle), "{needle:?} missing:\n{help}");
        }
        for action in ACTIONS {
            assert!(help.contains(action.rows), "rows missing:\n{help}");
        }
        for flag in extension_flags() {
            assert!(help.contains(flag), "{flag:?} missing:\n{help}");
        }
        // `init` leads every build's block: scaffolding is dependency-free,
        // so it sits above the per-build install row.
        assert!(
            help.find("init <name>").unwrap() < help.find("install").unwrap_or(usize::MAX),
            "init leads the actions in every build:\n{help}"
        );
        #[cfg(not(feature = "extension-install"))]
        {
            assert!(help.contains("not included in this build"), "{help}");
            assert_eq!(
                extension_flags(),
                vec!["--lang", "--available", "--trust", "--forget-pin"]
            );
        }
        assert!(
            !help.contains("--durability-mode"),
            "the page must not repeat the query-run options:\n{help}"
        );
    }
}
