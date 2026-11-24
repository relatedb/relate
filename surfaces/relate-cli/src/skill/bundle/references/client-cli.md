# CLI feedback reports

## Install

Nothing extra: both feedback commands are built into the `relate` binary in
every bundle. In agent mode `agent-feedback` appears in `relate --help`
(`feedback` is the person's own command and is not listed there).

## The agent flow: draft, ask, send

A report is the person's decision, never yours. On friction in a session — a
command that failed, a wrong expectation, a missing capability — follow these
steps in this order:

1. **Draft.** Write a concise report: the observed problem, the failed
   attempts it caused, and the improvement you suggest.
2. **Ask.** Show the draft to the person and ask for explicit approval.
3. **Send — only after that approval** — with the full structural invocation:

```bash
relate agent-feedback \
  --category unclear_error \
  --expected "the info command to print the version" \
  --observed "it printed help instead" \
  --command info
```

An unapproved draft is dropped, never sent. Do not use the person's
one-liner (`relate feedback "…"`): that command is the person's own GitHub
issue form, exists only in their interactive run, and sends nothing.

`--category` defaults to `friction`; the other values are `unclear_error`,
`unexpected_behavior`, and `missing_capability`. `--expected` and
`--observed` are required. `agent-feedback` prints one JSON outcome line on
stdout: `{"outcome": ..., "reason": ..., "request_id": ..., ...}`. Exit
codes: **0** when the report was sent or skipped (telemetry opted out), **1**
when it was locally rejected, the send failed, or no grant is recorded, **2**
for a usage error. A locally rejected report records no payload in the local
receipt.

## The grant comes first

The send carries no permission of its own: `agent-feedback` requires the
person's live standing grant, and without one it refuses with `not_enabled`
(exit 1) naming `relate feedback enable`. The grant is the person's act at a
terminal — ask them to run:

```bash
relate feedback enable   # authorize agent reports for 30 days
```

The CLI also asks the person directly, one line, default **yes** (an empty
answer or `y`/`yes` grants; any other answer declines): once on their first
interactive feedback command, and on `skill install` and `relate upgrade`
runs — once per 30-day window, so a lapsed or declined grant is asked about
again near its end. An opted-out telemetry ladder suppresses the ask
entirely. Never try to grant it yourself: the agent-mode command does not
exist, and a piped `enable` is refused.

Agent-mode error output also points here: an agent run's diagnostics carry a
one-line hint to this flow, exactly once per run.

## Writing with transaction metadata

When you script a write into a store the person opened under the
`value-provenance` opt-in, attach `--tx-meta KEY=VALUE` pairs (repeatable,
later keys win) so the audit trail records who made the write:

```bash
RELATE_PREVIEW=value-provenance relate query \
  --database ./store \
  --tx-meta app=my-agent --tx-meta run=4711 \
  'CREATE (:Person {name: "ada"})'
```

The map rides the session's next committed transaction and is cleared
after it. The opt-in is the person's, not yours — it changes what the
store retains — so without it already set, drop the flag rather than
exporting the variable: a run that passes `--tx-meta` without the
opt-in fails with `preview feature 'value-provenance' is not enabled`
(exit 6) instead of silently dropping the metadata.

## Gotchas

- **Ask before every send.** The recorded grant is consent for the channel,
  not approval of *this* draft; an unapproved draft is never sent.
- **Free text is validated and redacted before any send.** Emails, URLs, host
  names, IP addresses, file paths, data filenames, secrets, Cypher snippets
  and pasted output are rejected whole (never rewritten); a port-less token
  in a known procedure namespace (`apoc.…`, `gds.…`, `db.ontology.…`) is an
  identifier, not a host name — a token with an explicit port is always
  assessed as a host; the one exception
  is a documentation link to this project's own docs or repository, by path
  (`relatedb.ai/<path>`, `github.com/relatedb/relate/<path>`), or to the
  Neo4j Cypher manual (`neo4j.com/docs/`). A bare host URL with no path
  (`relatedb.ai`, `github.com/relatedb/relate`) is refused. `--expected` and
  `--observed` are at most 255 bytes.
- **Optional fields are dropped, not refused.** `--command` survives only for
  a visible command path; `--error-code` only for a code name in the CLI's
  exit table (`fatal_error`, `usage_error`, `conflict`, `validation_error`,
  `timeout`); `--model` only for a lowercase provider-prefixed label (at most
  40 chars). The outcome's `dropped_fields` names what was removed.
- **Nothing is queued.** A report is one synchronous POST with a 2-second
  deadline and no retries; `--request-id` lets a retry keep the same id (and
  the `$insert_id` dedup key) after `failed` or `unknown`.
- **The recorded destination is the consent.** `RELATE_FEEDBACK_ENDPOINT`
  is resolved when the person grants (`relate feedback enable`) and recorded;
  a later environment change cannot divert it, and a different destination
  needs a new grant.
- **Local receipts** — `feedback-receipts.jsonl` in the config directory
  (`$XDG_CONFIG_HOME/relate/` or `~/.config/relate/`) records every rejection
  and send attempt.
- **Fused plan children** — in the `{"plan": …}` machine modes a seek-started
  `MATCH` nests display-only children marked by `FusedInto` (ADR-0622);
  skip them when counting executed operators. They carry no `Rows`/`TimeNs`/`DbHits`.
- **Memo counters** — a `PROFILE` table gains a `Memo` column (`hits/probes`,
  `*` when a memo gave up) only when the statement used an expression memo; the
  machine modes carry `MemoProbes`/`MemoHits`/`MemoInserts`/`MemoRefused`/
  `MemoBytes`/`MemoDisabledCount` (and the root's `MemoKinds`) on those operators
  only (ADR-0654). `MemoBytes` is not part of `MemoryBytes`.

Full contract: `docs/api/cli/spec.md` (flag tables, mode gating, exit codes,
environment, state files).
