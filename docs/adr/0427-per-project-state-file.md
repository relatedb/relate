---
type: adr
area: cli
claim: a2b64ece187fd4adf978028417edeed77f3a967d
name: 0427-per-project-state-file
status: proposed
tags:
  - agents
  - commands
title: Per-project state file (`.cypherlite/`)
REFERENCES:
  - "[[adr/0403-in-band-truncation-records]]"
  - "[[adr/0404-agent-facing-cli-surface]]"
  - "[[adr/0407-agent-mode-override-ladder]]"
---

# ADR-0427: Per-project state file (`.cypherlite/`)

## Context

Every one-shot `cypherlite query` today needs `--database PATH`
re-passed, and the flag's default is `:memory:` — a forgotten flag does
not error, it silently opens an **empty in-memory store**, so a
mistyped session reads as "no data" instead of "no flag". There is no
remembered context between invocations: an agent (or a human in a
project directory) answering five questions re-states the database
path five times, and a pipeline that drops the flag quietly queries a
void.

The external baseline (LABS-481 G-4, from the DuckDB Agent Mode
research): agent skills work best when every one-shot run re-inits
from an idempotent per-project state — DuckDB's skills ship an
append-only `state.sql` each run restores from. Our need is narrower:
we do not need to restore engine state, only to **remember which store
a project uses** — but the shape of the value (a small, per-project,
machine-written fact that makes one-shot runs continuous) is the same.

The blast radius of the current behavior is agent-shaped: agents run
one-shot invocations from a working directory ([ADR-0404](./0404-agent-facing-cli-surface.md)'s whole
premise), so they hit the forgot-the-flag case more often than humans
do — and `--database` is the one fact every run repeats.

## Decision

A per-project state file, `.cypherlite/state.json`, remembers the
project's store. The resolution order for a statement-executing run
(`query`, the path-first form, `pull`, `push`, `info`):

1. `--database PATH` — always wins, and a run that passes it
   **updates** the state to that path
2. the project state file — a run with no flag resolves the remembered
   path
3. `:memory:` — the current default, only outside a project (or with
   an explicit `--database :memory:`)

**The project root** is the nearest ancestor directory of the current
working directory that contains `.git` or `.cypherlite/`; with none
found, there is no project and no state (step 3 applies).

**The file** (`state.json`, v1) carries exactly:

```json
{
  "version": 1,
  "database": "/abs/path/to/project/graph.cl",
  "writtenBy": "cypherlite 0.13.0"
}
```

Nothing else. Parameters are per-run facts, not project facts; the
database path is the one thing every invocation shares.

**Who writes:** every successful statement-executing or `info` run
that opened a store via an explicit `--database` or a resolved state
path — the write is atomic (temp file + rename in the same directory)
and idempotent (rewriting the same content is a no-op; content that
already matches is not re-serialized, so the file's mtime does not
churn on read-only workloads). Failed runs never write.

**Who reads / misbehaving state:** a run with no `--database` in a
project reads the file. A missing file is not an error (step 3 rules
only if no explicit path was ever recorded — inside a project with no
state, a flagless run fails loudly with "no `--database` given and no
`.cypherlite/state.json` in this project" rather than silently opening
`:memory:`); a file whose `version` is unknown, or whose JSON does not
parse, or whose `database` points nowhere, is a usage error (exit 2)
naming the file and the remedy (delete it, or pass `--database` to
overwrite).

**What it enables** — the shape of an agent (or human) session in a
project:

```bash
$ cypherlite query --database ./graph.cl 'CREATE (:Person {name: "Alice"})'
# state.json now remembers ./graph.cl (resolved absolute at write)

$ cypherlite query 'MATCH (p:Person) RETURN count(p)'
1

$ cypherlite info            # same store, no flag, no repetition
$ cypherlite pull --from dump.json 'MATCH (n) RETURN n AS e'
# pull seeds and updates the same remembered store
```

An agent's one-shot runs become continuous: the second and every later
invocation in a project carries no database plumbing, the CLI's skill
can teach "the CLI remembers your project's database", and two
projects on the same machine cannot collide (the state is
project-scoped, not user-global).

**Not state** (explicitly out of scope for v1): parameters, output
mode, row caps — those are per-run ergonomics with their own ladders
([ADR-0407](./0407-agent-mode-override-ladder.md), [ADR-0403](./0403-in-band-truncation-records.md)); engine state, WAL or checkpoint location (the
store's own files own those); anything user-global (no `~/.config`
tier).

`.cypherlite/state.json` is machine-local: the project should ignore
it (a one-line `.gitignore` hint ships in the docs; absolute paths
make a committed file wrong on every other checkout).

## Alternatives considered

- **Status quo (`--database` every run, `:memory:` default).** Rejected
  for the reasons above: the forgot-the-flag failure is silent, and
  the flag is the one fact every invocation repeats.
- **A user-global config (XDG `~/.config/cypherlite/`).** Rejected:
  wrong scope. Multiple projects on one machine would share one
  remembered store, and an agent working across projects would ping-
  pong the global; per-project is the unit agents actually work in.
- **An environment variable (`CYPHERLITE_DATABASE`) persisted by the
  user's shell.** Rejected as the mechanism (kept as a possible later
  rung beside `--database`): env persistence is shell-specific,
  invisible to the project (another contributor has no idea why runs
  resolve differently), and already carries the workspace's knob
  convention (`CYPHERLITE_*` are runtime knobs, not remembered facts).
- **DuckDB's append-only restore script (`state.sql`).** Rejected as
  over-fit: their file restores engine state across one-shot runs; ours
  only remembers a path. An append-only script grows forever and
  invites replay semantics we do not need; one small JSON with a
  version field is the honest size of this fact.
- **`--database` defaulting to a conventional path (`./graph.cl`)
  instead of `:memory:`.** Rejected: a convention silently creates or
  opens a store by accident — the same silent-default failure the
  state file avoids, now with writes landing in an unintended place.

## Consequences

- A flagless statement run in a project with no state **fails loudly**
  instead of opening `:memory:` — a behavior change; existing
  pipelines that relied on the silent `:memory:` default must pass
  `--database :memory:` (or `--database` explicitly, as all docs
  examples already do).
- `--database` becomes self-documenting across a team: the first
  contributor's run records the project's store; the flag table in
  `docs/api/cli/spec.md`, the flag's help entry, and the skill
  bundle's CLI reference document the resolution order in the same
  change as the implementation.
- The `query`, path-first, `pull`, `push`, and `info` surfaces gain one
  shared resolution helper (the one policy, one place rule); the
  lifecycle verbs that never execute statements never write state.
- Corrupt-state handling is a usage error (exit 2), matching
 `CYPHERLITE_MAX_ROWS`'s contract ([ADR-0407](./0407-agent-mode-override-ladder.md)'s round), not a runtime
  error.
- Implementation is its own PR once this ADR is Accepted; the
  implementation carries the unit tests (resolution order, atomic
  write, corrupt-state exit 2) and e2e tests (flagless run resolves
  the remembered store) in the same change.
