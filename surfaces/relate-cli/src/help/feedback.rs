//! The `feedback` command help page: the person's surface — the freed text
//! that becomes a prefilled GitHub issue, and the grant's action words.

/// The `feedback` synopsis, shared by its help page, its usage errors and
/// the bare command's state output.
pub(super) const FEEDBACK_USAGE: &str =
    "Usage: relate feedback [TEXT | enable | disable] [--category NAME] [--expected TEXT] [--observed TEXT]";

pub(super) fn feedback_help() -> String {
    format!(
        "{FEEDBACK_USAGE}

feedback files a public GitHub issue about this CLI: TEXT becomes the issue
title and body in your browser, and you edit and submit the form there.
Nothing is sent from this command. Without an argument, feedback prints the
standing agent-feedback grant.

  TEXT       What went wrong. A multi-line TEXT's first line is the title and
             the rest the body; a one-line TEXT is the title, cut at 80
             characters back to a space, while the body keeps the whole text.
  enable     Let agents send feedback reports for 30 days. Needs a terminal:
             the line it prints names the destination before the grant is
             recorded.
  disable    Revoke the agent-feedback grant.

The action words are always actions: to file a report whose text is
\"enable\" or \"disable\", pass it with --observed.

feedback flags:
  --category NAME   Prefix the title with a category; default friction. One
                    of friction, unclear_error, unexpected_behavior, or
                    missing_capability
  --expected TEXT   Append what you expected to the issue body
  --observed TEXT   The explicit spelling of TEXT (giving both is a usage
                    error)
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::help::{command_help, HelpScope, HelpTopic};

    #[test]
    fn feedback_help_lists_the_synopsis_the_actions_and_the_issue_flags() {
        let help = command_help(HelpTopic::Feedback);
        crate::help::assert_single_usage(&help, FEEDBACK_USAGE);
        for needle in [
            "TEXT",
            "enable",
            "disable",
            "--category",
            "--expected",
            "--observed",
        ] {
            assert!(help.contains(needle), "{needle:?} missing:\n{help}");
        }
        assert_eq!(
            HelpScope::Command(HelpTopic::Feedback).usage(),
            FEEDBACK_USAGE
        );
    }
}
