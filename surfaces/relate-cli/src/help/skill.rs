//! The `skill` command help page: the shared synopsis plus its flags.

/// The `skill` synopsis, shared by its help page and its usage errors.
pub(super) const SKILL_USAGE: &str =
    "Usage: relate skill install|remove|list|check [--agent NAME] [--force]";

pub(super) fn skill_help() -> String {
    format!(
        "{SKILL_USAGE}

skill manages the embedded agent skill bundle.
  install   Copy the bundle into every target agent's skills dir
  remove    Remove the installed bundle (marker-owned dirs only)
  list      Print each target agent's install state and path
  check     Verify installed bundles match this binary's bundle (drift
            detection); exit 1 on any drift, 0 when clean
  --agent NAME  Target one agent (default: all detected)
  --force       Clobber <skills dir>/relate even when the command would
                otherwise refuse it (install: an unmarked, unrecognizable
                dir; remove: any unmarked dir); not valid with list|check
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::help::{command_help, HelpTopic};

    #[test]
    fn skill_help_lists_the_shared_synopsis_and_its_flags() {
        let help = command_help(HelpTopic::Skill);
        crate::help::assert_single_usage(&help, SKILL_USAGE);
        assert!(
            help.contains("--agent") && help.contains("--force"),
            "{help}"
        );
        assert!(help.contains("otherwise refuse it"), "{help}");
    }
}
