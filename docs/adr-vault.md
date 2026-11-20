# The ADR vault — authoring and querying the decision log

The decision log is a markdown vault
([ADR-0621](adr/0621-adr-markdown-vault.md)): every ADR under
[`docs/adr/`](adr/) carries its metadata as canonical YAML frontmatter and
loads as an `:adr` node, so agents and maintainers read and query it — by
area, tag, status or edge — instead of mining prose. This document is the
authoring and working reference for that corpus: the vault form, the CLI
workflow, minting and claim proof, the public-record rules, and the
writing guidance. The policy summary lives in AGENTS.md §1; the detail
lives here.

## The vault form

A new ADR is hand-authored in the codec's canonical form
([ADR-0621](adr/0621-adr-markdown-vault.md) on the codec of
[ADR-0406](adr/0406-markdown-vault-snapshot-codec.md)), so a vault
checkpoint never rewrites it:

```
---
type: adr
area: engine
claim: <40-hex sha of the mint claim commit>
date: YYYY-MM-DD
deciders: MemoryDB maintainers
name: NNNN-short-kebab-slug
status: proposed
status_note: in part by [ADR-NNNN](./NNNN-slug.md)
tags:
  - storage
  - wal
title: The decision in one line
REFERENCES:
  - "[[adr/NNNN-slug]]"
SUPERSEDES:
  - "[[adr/NNNN-slug]]"
---

# NNNN: The decision in one line

## Context
```

Keys sit in the codec's canonical order — `type: adr` first, then the
sorted property keys, then the sorted relationship keys — so `area`
follows `type:` and the `tags:` block precedes `title:`. Optional keys
are written only where the ADR carries the fact (`date`, `deciders`,
`status_note`); `REFERENCES`/`SUPERSEDES` are written only when non-empty,
one `  - "[[adr/NNNN-slug]]"` line per target under the key, sorted by
number, unique. `REFERENCES` holds every other ADR the file cites;
`SUPERSEDES` holds the ADRs it replaces. Every ADR mention — body text or
frontmatter value — is the same `[ADR-NNNN](./NNNN-slug.md)` link: one
vocabulary read two ways, the human-facing link and the machine-facing
edge. Metadata never repeats in the body: status, claim, date and
deciders live in the frontmatter alone.

### Status, superseding and immutability

Status lives in the frontmatter `status:` key: starts `proposed`, set
`accepted` only when agreed. A superseding ADR's `SUPERSEDES` entry
names the ADR it replaces; the replaced one's status becomes
`superseded`, any remainder in `status_note`. Accepted ADRs immutable —
later change = new ADR + **Later references** note on old one; the
one-time rewrite of the whole corpus
([ADR-0621](adr/0621-adr-markdown-vault.md)) — format-only in
every file save for the ticket scrub carried in the same spend:
body-prose rewords dropping internal ticket pointers in 34 files,
ticket pointers stripped from lifted metadata values (deciders,
titles, reference lists) in 83 files, 11 files carrying both —
spent the only exception to that rule — accepted ADRs are immutable
again, format and content alike.

### Filename and body shape

- Filename `NNNN-short-kebab-slug.md` (four-digit sequence, never reused);
  one decision per file; cite others as `[ADR-NNNN](./NNNN-slug.md)` links
- Template order: **Context**, **Decision**, **Alternatives considered**
  (each ends with rejection reason), **Consequences**

### Areas and tags

`area` is exactly one of the six tracking areas (engine, cypher,
surface, cli, infra, ecosystem). `tags` are sorted kebab-case topic
labels — free-form, but prefer the vocabulary already in use, and a new
tag must differ from every one below by more than case, hyphenation or
singular/plural: adr-process, agents, auth, bolt, catalog, checkpoint,
ci, clients, commands, conformance, cookbook, dialect, did-you-mean,
docs, errors, executor, extensions, ffi, functions, help, http, index,
latency-suite, licensing, load, loadtest, mcp, memory, namespaces,
observability, ontology, output, parquet, parser, pattern-matching, perf,
planner, procedures, product, python, release, repl, result-metadata,
review, search, security, semantics, serve, skill, storage, telemetry,
testing, transactions, values, wal, wasm, workspace, write-path.

## Working with ADRs — open, query, add, link

Through the relate CLI whenever it is on PATH; plain text tools only as
the fallback:

```bash
# OPEN (once): bootstrap a throwaway copy of docs/ — the vault root is
# docs/ (ADRs load as :adr), never docs/adr; a plain folder never
# auto-detects (--snapshot-format markdown is required), read-only opens
# cannot create the store, and the real docs/ is never bootstrapped (a
# writable open checkpoints frontmatter onto the tracked docs files):
rm -rf /tmp/relate-adr-vault && cp -R docs /tmp/relate-adr-vault && \
  relate query --database /tmp/relate-adr-vault --snapshot-format markdown "RETURN 1"
# QUERY: read-only against the copy — frontmatter keys are properties,
# REFERENCES/SUPERSEDES are edges:
relate query --read-only --database /tmp/relate-adr-vault --mode jsonl "MATCH (a:adr) WHERE a.status = 'accepted' RETURN a.name LIMIT 5"
relate query --read-only --database /tmp/relate-adr-vault --mode jsonl "MATCH (a:adr)-[:SUPERSEDES]->(b:adr) RETURN a.name, b.name LIMIT 5"
```

- **ADD**: `python3 scripts/adr_mint.py` claims the number (it never
  writes the file); hand-write `docs/adr/NNNN-slug.md` in the vault form
  above — it loads as a node at the next open.
- **LINK**: `[ADR-NNNN](./NNNN-slug.md)` in the body, and the `NNNN-slug`
  key under `REFERENCES:` in the frontmatter (`SUPERSEDES:` when
  replacing) — the body link is for readers, the frontmatter key is the
  edge the database loads.

## Minting and claim proof

- Mint number via `python3 scripts/adr_mint.py` (atomic claim; never list
  directory). Minted numbers never below 0300
- **Mint late, mint once.** Mint only when the ADR text is written and the
  file lands in the same change — never during planning, never to reserve;
  an abandoned claim burns the number permanently
- Every added ADR carries mint proof: the frontmatter `claim:` key holds
  the 40-hex sha of mint's claim commit ([ADR-0301](adr/0301-adr-claim-proof.md)) — only the frontmatter
  key counts, a claim line in the body proves nothing. CI check
  `adr-numbers` requires it; run it locally with `make test-adr-scripts`
  and `python3 scripts/check_adr_numbers.py --base origin/main` (commit
  the ADR first: the claim leg reads committed diffs)

## Public records

**ADRs are public records.** No internal references — no ticket, issue,
or CIP proposal IDs; no internal tracker/chat/wiki links; no internal
host or tool names. No person names — roles, not people ("the
maintainers"). No sensitive information of any kind: credentials,
tokens, internal URLs, unreleased plans, customer or personal data.
The `adr-numbers` CI check fails any line a change adds under
`docs/adr/` carrying a Linear ticket id (`LABS-###`/`REL8-###`),
`CIP-###`, or a `linear.app` link —
and, repo-wide, any tracked line that ties a `CIP-###` id to
proposal-internal detail: quoted grammar, rule, section or deviation
labels, `NOTES.md` pointers, attribution phrases, approval facts,
quoted titles, or eval-corpus names. The repo-wide scan is not
diff-aware (existing lines fail too) and reads each line with its
successor, so a citation a line wrap splits mid-way fails as one
marker; a bare `CIP-###` id outside `docs/adr/`
does not fail, and the check's own fixtures
`scripts/tests/test_check_adr_numbers.py` and
`scripts/tests/test_adr_mint.py` are exempt.

## Writing guidance

An ADR records architecture and principles — the why and the shape of the
decision — not the implementation. Keep out:

- Code snippets, pseudo-code, and config dumps — name the surface or
  mechanism instead of quoting it
- File:line inventories of what changed — the diff is the record of that
- Step-by-step history of how the change was built or reviewed

Line budget for a newly written ADR, checkable with `wc -l
docs/adr/NNNN-*.md`: target **≤ 80 lines**, hard ceiling **150
lines**. Over the ceiling, cut detail — long alternative analyses,
restated context, implementation asides — never split one decision
into two ADRs to fit the budget. The budget is no corpus-wide
invariant: pre-existing files above the ceiling stand as the
one-time rewrite left them.
