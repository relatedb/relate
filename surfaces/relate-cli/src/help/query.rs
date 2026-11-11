//! The `query` command help page (ADR-0200): the shared synopsis plus the
//! statement-session options block.

/// The `query` synopsis, shared by its help page and its usage errors.
pub(super) const QUERY_USAGE: &str = "Usage: relate query [STATEMENT] [--database PATH]";

pub(super) fn query_help() -> String {
    let mut s = String::new();
    s.push_str(QUERY_USAGE);
    s.push('\n');
    s.push('\n');
    s.push_str("query runs a statement session: a positional STATEMENT or\n");
    s.push_str("-c/--command (one statement or a `;`-separated list). With no\n");
    s.push_str("STATEMENT, a script is read from stdin, or the interactive shell opens\n");
    s.push_str("on a terminal. The path-first form with no QUERY opens the same shell on a\n");
    s.push_str("terminal; on piped or redirected stdin it prints help instead.\n");
    s.push_str("-d / --database sets\n");
    s.push_str("the store; omitted, it is :memory:. The same flag is the store on\n");
    s.push_str("the path-first form, on pull, and on info.\n");
    s.push('\n');
    s.push_str(&super::session_options_text("STATEMENT"));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::help::{command_help, HelpScope, HelpTopic};

    #[test]
    fn query_help_lists_the_shared_synopsis_database_and_session_options() {
        let help = command_help(HelpTopic::Query);
        crate::help::assert_single_usage(&help, QUERY_USAGE);
        for present in [
            "-d, --database PATH",
            "runs a statement session",
            "-c, --command Q",
            "`;`-separated list",
            "--json",
            "--namespace NAME",
        ] {
            assert!(help.contains(present), "{present:?} missing:\n{help}");
        }
        assert_eq!(HelpScope::Command(HelpTopic::Query).usage(), QUERY_USAGE);
        assert_eq!(
            HelpScope::Command(HelpTopic::Query).hint(),
            "Run 'relate query --help' for more information."
        );
    }
}
