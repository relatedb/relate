//! The `pull` command help page: the shared synopsis plus its flags.

/// The `pull` synopsis, shared by its help page and its usage errors.
pub(super) const PULL_USAGE: &str =
    "Usage: relate pull [--from FILE] --query Q [--param N=V] [--allow-plaintext] --database PATH";

pub(super) fn pull_help() -> String {
    format!(
        "{PULL_USAGE}

pull copies a Neo4j/Aura subgraph (HTTP Query API v2) into --database.
  --from FILE   Env file (NEO4J_URI, NEO4J_USERNAME, NEO4J_PASSWORD, NEO4J_DATABASE);
                omitted: use those process environment variables
  --query Q     Cypher statement, `;`-separated statement list, file path, or `-` (stdin);
               every statement must RETURN graph elements; all run in one process,
               one checkpoint at the end
  -d, --database PATH  Destination directory or .rl bundle (required; not :memory:)
  --param N=V   Bind a source query parameter (repeatable)
  --allow-plaintext  Permit Basic auth to a non-loopback http:// Query API
                     (insecure; prints a warning); default off
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::help::{command_help, HelpScope, HelpTopic};

    #[test]
    fn pull_help_lists_the_shared_synopsis_and_its_flags() {
        let help = command_help(HelpTopic::Pull);
        crate::help::assert_single_usage(&help, PULL_USAGE);
        for present in ["--from", "--query", "--param", "--allow-plaintext"] {
            assert!(help.contains(present), "{present:?} missing:\n{help}");
        }
        assert_eq!(
            HelpScope::Command(HelpTopic::Pull).hint(),
            "Run 'relate pull --help' for more information."
        );
    }
}
