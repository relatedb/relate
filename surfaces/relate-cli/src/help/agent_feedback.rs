//! The `agent-feedback` command help page: the agent's surface — one
//! structured report sent under the person's standing grant.

/// The `agent-feedback` synopsis, shared by its help page and its usage
/// errors.
pub(super) const AGENT_FEEDBACK_USAGE: &str =
    "Usage: relate agent-feedback [--category NAME] --expected TEXT --observed TEXT \
     [--command PATH] [--error-code CODE] [--model MODEL] [--request-id UUID]";

pub(super) fn agent_feedback_help() -> String {
    let max_text = relate_feedback::report::MAX_TEXT_BYTES;
    let max_model = relate_feedback::policy::MAX_MODEL_CHARS;
    format!(
        "{AGENT_FEEDBACK_USAGE}

agent-feedback sends one person-consented report about this CLI to the
maintainers. It never prompts and requires the person's live grant: without
one it refuses (not_enabled) and names 'relate feedback enable'. Only the
validated, redacted payload leaves the machine, and every attempt is
receipted locally.

agent-feedback flags:
  --category NAME     One of friction (the default), unclear_error,
                      unexpected_behavior, or missing_capability
  --expected TEXT     Required; what you expected, non-blank and at most
                      {max_text} bytes (a longer value is rejected, never truncated)
  --observed TEXT     Required; what actually happened, the same rules as
                      --expected
  --command PATH      The CLI command path; kept only when it names a
                      visible command, else dropped and named in
                      dropped_fields
  --error-code CODE   The CLI error code; kept only when it is one of the
                      exit table's nonzero code names, else dropped and
                      named in dropped_fields
  --model MODEL       The agent model label: lowercase, at most {max_model}
                      characters, a provider prefix or an o<digit> family
                      name; else dropped and named in dropped_fields
  --request-id UUID   A lowercase UUIDv4 request id; an invalid value is a
                      violation and a fresh id is used (the same id can be
                      resent after failed or unknown)
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::help::{command_help, HelpScope, HelpTopic};

    #[test]
    fn agent_feedback_help_lists_the_synopsis_and_every_flag() {
        let help = command_help(HelpTopic::AgentFeedback);
        crate::help::assert_single_usage(&help, AGENT_FEEDBACK_USAGE);
        for needle in [
            "--category",
            "--expected",
            "--observed",
            "--command",
            "--error-code",
            "--model",
            "--request-id",
        ] {
            assert!(help.contains(needle), "{needle:?} missing:\n{help}");
        }
        assert_eq!(
            HelpScope::Command(HelpTopic::AgentFeedback).usage(),
            AGENT_FEEDBACK_USAGE
        );
    }
}
