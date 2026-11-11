//! The `push` command help page: the shared synopsis plus its flags.

/// The `push` synopsis, shared by its help page and its usage errors.
pub(super) const PUSH_USAGE: &str = "\
Usage: relate push --to CREDS --database PATH \
[--query Q | --labels L1,L2 | --types T1,T2] [--batch-size N] \
[--nodes-update POLICY] [--rels-update POLICY] [--updated-at NAME[,NAME]] \
[--rel-mode MODE] [--merge-key LABEL:PROP[,PROP]] [--rel-key TYPE:PROP[,PROP]] \
[--unkeyed-labels ACTION] [--dry-run] [--json] [--allow-plaintext]";

pub(super) fn push_help() -> String {
    format!(
        "{PUSH_USAGE}

push copies a scoped subgraph of --database into a Neo4j/Aura remote over
HTTP Query API v2 — pull's mirror, over the same client.
  --to CREDS   Env file (NEO4J_URI, NEO4J_USERNAME, NEO4J_PASSWORD, optional
               NEO4J_DATABASE), or a Query API URI (username/password then
               come from the process environment)
  -d, --database PATH  Source store directory or .rl bundle (required; not :memory:)
  --query Q    Only what the statements RETURN (nodes/rels/paths; a `;`-list,
               file path, or `-` for stdin). One of --query/--labels/--types;
               none pushes the whole store
  --labels L1,L2  Only nodes with these labels, and the rels between them
  --types T1,T2   Only rels of these types, with both their endpoints
  --batch-size N   Rows per batched statement (default 5000)
  --nodes-update POLICY  add (default) | replace | skip, per label or global
               (Movie=replace,Person=skip). add: SET += keeps remote-only
               properties; replace: SET = overwrites; skip: create missing only
  --rels-update POLICY  Same policy vocabulary, per relationship type or global
  --updated-at NAME[,NAME]  Candidate timestamp properties; when both sides
               carry one, a strictly newer remote row is skipped (counted
               remote-newer), newer-or-equal local applies (equal is
               idempotent), incomparable pairs apply with a warning count
  --rel-mode MODE  keyed (default; merge on the type's resolved key — a
               --rel-key override, else its relationship uniqueness/key
               constraint — falling back to one rel of a type between two
               nodes when neither exists), structural (force the pair even
               when a key exists; parallel rels collapse), create
               (non-idempotent escape hatch)
  --merge-key LABEL:PROP[,PROP]  Merge key for one label, overriding the
               constraint-derived key (repeatable)
  --rel-key TYPE:PROP[,PROP]  Merge key for one relationship type, overriding
               its constraint-derived key or supplying one where no
               constraint exists (repeatable)
  --unkeyed-labels ACTION  skip (default; reported and counted) or create
               (one-shot seeding rows; non-idempotent)
  --dry-run    Print the plan (counts, merge keys, statement shapes, batch
               counts, policies) without touching the remote
  --json       Print the run report as one JSON object instead of the table
  --allow-plaintext  Permit Basic auth to a non-loopback http:// Query API
                     (insecure; prints a warning); default off

Merge keys come from the store's uniqueness and node/relationship key
constraints (composite keys included), overridable per label and per type;
a label with none is reported, never guessed. Nothing is ever deleted
remotely; a failed batch fails the push naming the statement and row range,
and the report lists exactly which batches landed.
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::help::{command_help, HelpScope, HelpTopic};

    #[test]
    fn push_help_lists_the_shared_synopsis_and_its_flags() {
        let help = command_help(HelpTopic::Push);
        crate::help::assert_single_usage(&help, PUSH_USAGE);
        for present in [
            "--to",
            "--query",
            "--labels",
            "--types",
            "--batch-size",
            "--nodes-update",
            "--rels-update",
            "--updated-at",
            "--rel-mode",
            "--merge-key",
            "--unkeyed-labels",
            "--dry-run",
            "--json",
            "--allow-plaintext",
        ] {
            assert!(help.contains(present), "{present:?} missing:\n{help}");
        }
        assert_eq!(
            HelpScope::Command(HelpTopic::Push).hint(),
            "Run 'relate push --help' for more information."
        );
    }
}
