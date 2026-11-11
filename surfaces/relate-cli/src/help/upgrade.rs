//! The `upgrade` command help page (alias: `update`): the shared synopsis
//! plus its flags.

/// The `upgrade` synopsis, shared by its help page and its usage errors.
pub(super) const UPGRADE_USAGE: &str = "Usage: relate upgrade [--yes] [--check]";
/// The `update` alias line the `upgrade` page carries.
pub(super) const UPGRADE_ALIAS_USAGE: &str =
    "       relate update  [--yes] [--check]  (alias of upgrade)";

pub(super) fn upgrade_help() -> String {
    format!(
        "{UPGRADE_USAGE}
{UPGRADE_ALIAS_USAGE}

upgrade checks the latest release listed by the website's releases.json
and replaces the running binary (macOS and Linux only);
`update` is an alias.
  --yes    Auto-accept the post-upgrade skill refresh
  --check  Report current-vs-latest only; download and modify nothing
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::help::{command_help, HelpScope, HelpTopic};

    #[test]
    fn upgrade_help_names_its_update_alias() {
        let help = command_help(HelpTopic::Upgrade);
        crate::help::assert_single_usage(&help, UPGRADE_USAGE);
        assert!(help.contains(UPGRADE_ALIAS_USAGE), "{help}");
        assert!(help.contains("--yes") && help.contains("--check"), "{help}");
        // The description line is the source-of-truth prose for how upgrade
        // discovers releases (ADR-0250); pin it so wording drift fails here.
        assert!(
            help.contains(
                "upgrade checks the latest release listed by the website's releases.json"
            ),
            "{help}"
        );
        assert!(!help.contains("GitHub release"), "{help}");
        assert_eq!(
            HelpScope::Command(HelpTopic::Upgrade).hint(),
            "Run 'relate upgrade --help' for more information."
        );
    }
}
