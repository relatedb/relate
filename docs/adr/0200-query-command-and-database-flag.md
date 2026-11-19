---
type: adr
area: cli
date: 2026-10-10
deciders: CypherLite maintainers
name: 0200-query-command-and-database-flag
status: proposed
tags:
  - commands
title: "`query` command and `--database`"
REFERENCES:
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0156-directory-clone]]"
  - "[[adr/0231-bare-invocation-opens-shell-on-tty]]"
  - "[[adr/0286-fs-to-graph-explicit-db-exits]]"
---

# ADR-0200: `query` command and `--database`

## Context

LABS-247 and the Slack CLI review asked for an explicit verb so a misspelled
command cannot become a database path, and so the common case looks like
`neo4j-cli query [cypher]`. The store path is a flag on every command that
takes one database, so a path is never the first positional. The agreed
shape is:

```bash
cypherlite query "RETURN 2" --database /tmp/triha/my-db
```

`query --query` is awkward once `query` is the verb, so the statement is a
positional (or `-c` / `--command`). Slack named `-d` as the short form of
`--database`. That is the one new single-dash flag; AGENTS.md still forbids
adding others.

## Decision

1. **`query` is a first-positional command word in every bundle.** It is a
 statement-session verb, not an exit-after lifecycle command ([ADR-0069](./0069-cli-lifecycle-commands.md)).
   A directory named `query` is `--database query` or `--database ./query`.
2. **Grammar:** `cypherlite query [STATEMENT] [--database PATH] [session
   flags]`. `STATEMENT` is one Cypher statement or a `;`-separated list, the
   same slot as `-c` / `--command`. The two spellings cannot be combined.
   On the path-first form, no `QUERY` prints help and exits; it does not
   open the shell or read stdin. `query` with no `STATEMENT` keeps the
   session trichotomy: a terminal opens the shell and any other stdin is
   the script list.
3. **`-d` / `--database PATH`** is the store on `query`, on the path-first
   form, on `pull`, and on `info`. `-d` is the short form of `--database`.
   Omitted on a statement session, the store is `:memory:`. `pull` and `info`
   require it and reject `:memory:`. A value that starts with `-` is still a
   path (`-d -dashed_db`).
4. **The path-first form is** `cypherlite [OPTIONS] [--database PATH]
   [QUERY]`. The only positional is the statement. A second positional is a
 usage error. `clone SOURCE DEST` stays two positionals ([ADR-0156](./0156-directory-clone.md)).
 `lab fs-to-graph` keeps `--db` ([ADR-0286](./0286-fs-to-graph-explicit-db-exits.md)).
5. **`query --help`** prints the command synopsis and the same query-run
   options as top-level help, including `--database`, from one shared
   options block.

## Alternatives considered

- **No `query` verb (keep path-first only).** Rejected: a misspelled command
  becomes a database path, and the form is less aligned with `neo4j-cli
  query`.
- **Session flag `--query`, `--cypher`, or `--statement` on a `query`
  verb.** Rejected: `query --query` is awkward; `-c` / `--command` already
  names the one-shot slot; a second spelling would be a second grammar.
- **No short form.** Rejected: Slack asked for `-d`, matching `neo4j-cli`.
  It is the one new single-dash flag. Further short flags stay forbidden.
- **Require `--database`.** Rejected: `:memory:` remains the default for
  one-shot and agent runs, matching the path-first default.

## Consequences

- `query` is listed in `COMMAND_WORDS` and in top-level `Commands:` in every
  bundle (`full`, `agent`, `minimal`).
- Help, `docs/api/cli/spec.md`, the flag reference, the skill bundle, and
  the return-one example describe the same contract.
- [ADR-0069](./0069-cli-lifecycle-commands.md) still governs exit-after lifecycle verbs; this ADR adds the
  statement-session verb beside them.

## Later references

[ADR-0231](./0231-bare-invocation-opens-shell-on-tty.md) supersedes the second half of Decision 2 on an interactive
terminal: the bare path-first form with no `QUERY` opens the interactive
shell there (the same session `query` runs). With piped or redirected
stdin it still prints help and exits.
