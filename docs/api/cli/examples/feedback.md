# Example: file a feedback issue, and grant an agent's reports

Feedback has two surfaces (ADR-0588 §3): a person files a public GitHub
issue, and an agent sends a consented report in agent mode. Each mode has
exactly one of them; the other is an unknown command naming it.

## The person's channel

`relate feedback TEXT` prints a prefilled GitHub new-issue URL for this
repository and opens it in the default browser. Nothing is sent from the
command: you edit and submit the form on GitHub yourself.

```bash
relate feedback "the info command printed help instead of the version"
```

The first line of a multi-line text becomes the title and the rest the body.
A one-line text becomes the title, cut at 80 characters back to a space,
while the body keeps the whole text. `--category` (default `friction`)
prefixes the title, and `--expected` is appended to the body:

```bash
relate feedback --category unclear_error \
  --observed "it printed help instead" \
  --expected "the version"
```

The action words are always actions: to file an issue whose text is literally
`enable` or `disable`, spell it with `--observed`, as above.

A bare `relate feedback` prints the standing state, e.g.
`feedback disabled (not enabled)`, plus the synopsis.

## The agent channel's grant

The agent's reports need your grant, which lasts 30 days. Grant it at a
terminal (a piped run is refused: the grant is a person's act):

```bash
relate feedback enable
```

Expected stdout is `feedback enabled until <RFC3339>: agent reports go to
<destination> for 30 days`. The first bare `relate feedback` at a terminal
that finds no grant asks once — one line, default no — and never asks again; a
no is remembered as a decline. A run with no resolved destination skips the
ask rather than spending it. Revoke with:

```bash
relate feedback disable
```

## The agent's send

In agent mode the CLI exposes `relate agent-feedback` instead. It takes the
structured report flags, never prompts, and requires the live grant above:

```bash
relate agent-feedback \
  --category unclear_error \
  --expected "the info command to print the version" \
  --observed "it printed help instead" \
  --command info
```

Expected stdout is one compact JSON outcome document, e.g.
`{"outcome":"sent","reason":"sent","request_id":"…","report_id":"…",…}`. Exit
code 0 means the report was sent or skipped (telemetry opted out); 1 means it
was rejected locally, the send failed, or no grant is recorded (`not_enabled`,
naming `relate feedback enable`); 2 is a usage error. `--expected` and
`--observed` are free text, so they must not contain emails, URLs, host names,
IP addresses, file paths, data filenames, secrets, Cypher, or pasted output — a
report containing any of those is rejected whole and never sent. The one
exception: a documentation link to this project's own docs or repository, by
path (`relatedb.ai/<path>`, `github.com/relatedb/relate/<path>`), or to the
Neo4j Cypher manual (`neo4j.com/docs/`) is kept; a bare host URL with no path
(`relatedb.ai`, `github.com/relatedb/relate`) is refused. An agent drafts the
report, shows the person the draft, and sends only after their approval.

See the [CLI spec](../spec.md#feedback-reports) for the full flag tables,
mode gating, exit codes, and environment overrides.
