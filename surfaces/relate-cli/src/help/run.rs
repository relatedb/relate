//! The `run` command help page (ADR-0429 §1, LABS-552): the shared
//! synopsis, the dynamic installed-actions section — the one
//! machine-state-dependent part of any command's page, which is why it
//! lives here and never on the top-level page — and the shared
//! store-flag block. The per-action pages are the host's own renderer
//! (`extensions::host::action_help`), reached through the verb's
//! pre-scan.

use crate::extensions::registry::ActionRegistry;
use crate::extensions::run::{installed_actions, load_registry_or_exit};
use relate_formatter::sanitize_error_text;

/// The `run` synopsis, shared by its help page and its usage errors:
/// the one closed grammar — `<name>` is an installed action's mount
/// name, never a reserved word, and installed names never enter the
/// top-level vocabulary (ADR-0429 §1).
pub(super) const RUN_USAGE: &str = "Usage: relate run <name> [flags]";

/// The `run` page over the real machine state: the registry resolves
/// through the shared XDG rule, and a load refusal fails the verb
/// closed through the run mount's usage-error path (exit 2, the
/// refusal, this synopsis and its hint) — the page never renders a
/// partial listing (ADR-0429 §6).
pub(super) fn run_help() -> String {
    run_help_with(&load_registry_or_exit())
}

/// [`run_help`] over a given registry — the fixture-test twin (the
/// `load`/`load_with` pattern): the installed-actions section reads
/// the registry's visible extension rows, in the walk's sorted order.
pub(super) fn run_help_with(registry: &ActionRegistry) -> String {
    let mut page = format!("{RUN_USAGE}\n\n");
    page.push_str("Run an installed command-kind extension: the child walks its\n");
    page.push_str("source and emits rows over the wire; the host alone owns the store, the\n");
    page.push_str("caps and the exit codes.\n\n");
    // The dynamic section (ADR-0429 §1): installed names live here and
    // nowhere else above the action's own page — the top-level page
    // stays byte-stable across installs. Every fragment is manifest
    // data, sanitized where it is placed.
    page.push_str("Installed actions:\n");
    let installed: Vec<_> = installed_actions(registry).collect();
    if installed.is_empty() {
        page.push_str("  none (relate extension install <name> adds one)\n");
    } else {
        for entry in installed {
            let name = sanitize_error_text(entry.mount_name());
            page.push_str(&format!(
                "  {name:<21} v{} — run 'relate run {name} --help' for its flags\n",
                sanitize_error_text(&entry.command().version)
            ));
        }
    }
    page.push('\n');
    for line in crate::help::DATABASE_FLAG_HELP {
        page.push_str(line);
        page.push('\n');
    }
    page.push('\n');
    page.push_str("An uninstalled name fails closed with the discovery error — nothing is\n");
    page.push_str("fetched; the flags an action accepts are its own\n");
    page.push_str("manifest's flag table.\n");
    page
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::testkit::{install_package, manifest_for};

    /// The fixture registry the page tests render over — an installed
    /// `greet` under a tempdir XDG (the testkit's own pattern).
    fn fixture_registry() -> (ActionRegistry, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        install_package(&tmp, "greet-pkg", &manifest_for("greet", "", ""));
        let xdg = tmp.path().to_string_lossy().into_owned();
        let pairs = [("XDG_CONFIG_HOME", xdg.as_str())];
        let registry = ActionRegistry::load_with(&crate::telemetry::env_of(&pairs)).unwrap();
        (registry, tmp)
    }

    #[test]
    fn the_page_opens_with_the_synopsis_and_lists_the_installed_actions() {
        let (registry, _tmp) = fixture_registry();
        let page = run_help_with(&registry);
        crate::help::assert_single_usage(&page, RUN_USAGE);
        assert!(
            page.contains("Installed actions:\n  greet"),
            "the installed listing is missing:\n{page}"
        );
        // The listed row points at the action's own page and carries
        // the manifest's version — manifest data, sanitized where
        // placed.
        assert!(
            page.contains("v0.1.0 — run 'relate run greet --help' for its flags"),
            "the listing's row:\n{page}"
        );
        // The shared store block (the one store-path fact every import
        // grammar documents) renders on the verb's page too.
        for line in crate::help::DATABASE_FLAG_HELP {
            assert!(page.contains(line), "{line:?} missing:\n{page}");
        }
        // The discovery contract is stated on the page.
        assert!(
            page.contains("nothing is\nfetched; the flags an action accepts"),
            "the discovery note:\n{page}"
        );
    }

    #[test]
    fn the_ship_with_zero_state_lists_none_and_names_the_install_path() {
        let registry = ActionRegistry::load_with(&crate::telemetry::env_of(&[])).unwrap();
        let page = run_help_with(&registry);
        assert!(
            page.contains(
                "Installed actions:\n  none (relate extension install <name> adds one)\n"
            ),
            "the zero-installed listing:\n{page}"
        );
    }

    /// A hidden extension appears in no listing (ADR-0429 §2) — the
    /// page's dynamic section counts only the visible rows, exactly as
    /// the missing-name error does.
    #[test]
    fn a_hidden_extension_is_not_listed() {
        let tmp = tempfile::tempdir().unwrap();
        install_package(
            &tmp,
            "shy-pkg",
            &manifest_for("shy", "", r#","hidden": true"#),
        );
        let xdg = tmp.path().to_string_lossy().into_owned();
        let pairs = [("XDG_CONFIG_HOME", xdg.as_str())];
        let registry = ActionRegistry::load_with(&crate::telemetry::env_of(&pairs)).unwrap();
        let page = run_help_with(&registry);
        assert!(
            !page.contains("shy"),
            "the hidden extension leaked into the listing:\n{page}"
        );
        assert!(
            page.contains("Installed actions:\n  none"),
            "the zero-installed listing:\n{page}"
        );
    }
}
