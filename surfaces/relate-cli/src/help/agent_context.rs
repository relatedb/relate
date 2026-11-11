//! The `agent-context` command help page: the shared synopsis plus its
//! behavior contract.

/// The `agent-context` synopsis, shared by its help page and its usage
/// errors.
pub(super) const AGENT_CONTEXT_USAGE: &str = "Usage: relate agent-context [COMMAND]";

pub(super) fn agent_context_help() -> String {
    format!(
        "{AGENT_CONTEXT_USAGE}

agent-context prints the compact discovery readout for agent harnesses,
rendered from the same command table as --help, so the two
cannot drift. With no COMMAND it prints the command index, the query-run
flag names, and the exit-code table; with a COMMAND word it prints that
command's synopsis, flags and examples. Output is always compact text.
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::help::{command_help, HelpTopic};

    #[test]
    fn agent_context_help_lists_the_shared_synopsis() {
        let help = command_help(HelpTopic::AgentContext);
        crate::help::assert_single_usage(&help, AGENT_CONTEXT_USAGE);
        assert!(help.contains("COMMAND"), "{help}");
    }
}
