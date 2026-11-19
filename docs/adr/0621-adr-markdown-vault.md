---
type: adr
area: ecosystem
claim: 1cb426ef874eb94ca74a5304aa8f8d1c9c9944f4
name: 0621-adr-markdown-vault
status: proposed
tags:
  - adr-process
  - agents
title: The ADR corpus is a markdown vault — normalized frontmatter, wikilink edges, and agent queries
REFERENCES:
  - "[[adr/0301-adr-claim-proof]]"
  - "[[adr/0406-markdown-vault-snapshot-codec]]"
  - "[[adr/0460-markdown-body-properties]]"
---

# 0621: The ADR corpus is a markdown vault — normalized frontmatter, wikilink edges, and agent queries

## Context

The decision log is 561 hand-written markdown files under `docs/adr/`,
authoritative but inert: their metadata — status, mint claim, date,
deciders — is prose in three generations of header forms (plain header
lines, MADR dash lists, `## Status` sections), their cross-references
are bare mentions, and their supersede facts are sentences a reader
must find and parse. Every consumer — the numbering check, agents,
maintainers — mines the same prose and re-derives its own partial view
of it.

The engine already ships the store this corpus wants: the markdown
vault snapshot codec ([ADR-0406](./0406-markdown-vault-snapshot-codec.md)) — a folder of markdown files with
YAML frontmatter and wikilink relationship keys is a live database —
with its ordered body-property grammar ([ADR-0460](./0460-markdown-body-properties.md)) behind it. ADR
numbers are claimed on an append-only ledger and proven by a claim sha
carried in the file ([ADR-0301](./0301-adr-claim-proof.md)).

## Decision

`docs/` is a vault store per [ADR-0406](./0406-markdown-vault-snapshot-codec.md): the tree opens in place
with the markdown snapshot format, the corpus loads as `:adr` nodes,
and what markdown cannot hold — the hidden engine sidecar, the
regenerated index and log views — is derived state, never content.
The corpus is rewritten once, in place, into the codec's canonical
form — the same bytes the codec itself emits, so opening and
checkpointing the vault never rewrites an ADR. Every file carries:

- frontmatter in the codec's canonical key order — `type: adr`, the
  sorted property keys, then the relationship keys — holding the
  metadata the body carried as prose, lifted as-is except that
  internal ticket pointers are dropped from the values that carried
  them: `status` mapped to proposed/accepted/superseded with any
  remainder as `status_note`, each key present only where the body
  carried it;
- `area`, exactly one of the six tracking areas (engine, cypher,
  surface, cli, infra, ecosystem), and `tags`, sorted kebab-case
  labels from one closed, near-duplicate-free vocabulary — the
  classification vault queries filter by;
- `REFERENCES` and `SUPERSEDES`, the file's outgoing edges as
  full-form wikilinks sorted by number: REFERENCES every other ADR
  the body or status mentions, SUPERSEDES the corpus's supersedes
  statements, lifted once from the prose and authoritative from here;
- the body: the title heading, then prose in which every ADR mention
  is a `[ADR-NNNN](./NNNN-slug.md)` markdown link — one vocabulary
  read two ways, the human-facing link and the machine-facing edge
  naming the same pair.

The mint proof moves with the metadata: the CI numbering check reads
the claim sha from the frontmatter `claim:` key — [ADR-0301](./0301-adr-claim-proof.md)'s proof
unchanged, its header line rehoused — so an added ADR without a
frontmatter claim fails as an unproven number.

This migration spends one exception to Accepted-ADR immutability:
every ADR, accepted ones included, is rewritten in place, once. The
rewrite is format-only in every file — no decision text changes —
save for the ticket scrub carried in the same spend, its scope
counted from the rewrite diff: in 34 files, body-prose sentences
naming internal tickets are reworded to drop the pointers; in 83
files, ticket pointers are stripped from the lifted metadata
values — deciders lines, title lines, and the reference lists that
feed the edge keys; 11 files carry both. Every rewritten line stays
within the public-record rule. The rule itself stands untouched
from here; a later change to an accepted ADR is still a new ADR
plus a Later-references note on the old one.

Agents read and query the vault through the relate CLI — Cypher over
the `:adr` nodes, by area, tag, status or edge — in preference to text
tools such as sed and grep; when the CLI is not on PATH, the text
tools are the fallback. The corpus stays plain markdown either way:
query power is added, never traded for readability.

## Alternatives considered

- Keep the corpus as prose and build text tooling over it. Rejected:
  parsing three header generations and mining prose for edges is
  shape-sniffing with more steps, and every consumer keeps its own
  drifting partial view.
- A generated index beside the corpus, built in CI. Rejected: derived
  state next to the authority drifts, and the engine already has the
  store that makes the files themselves the database.
- Move the corpus into a graph store with markdown as a generated
  view. Rejected: the files are the authority — git history, review
  and hand edits live there; inverting that puts the public record
  outside the repository.
- Keep the mint proof as a body header line. Rejected: the proof is
  metadata, the metadata's home is now the frontmatter, and two homes
  for one fact is the parallel state this form removes.

## Consequences

- The corpus is queryable where it lives: agents and maintainers ask
  for ADRs by area, tag, status or edge instead of mining prose; sed
  and grep remain the fallback when the CLI is not on PATH.
- New ADRs are hand-authored in the canonical form directly — this
  ADR is the first — carrying their minted number, frontmatter claim,
  linked mentions and edge keys from the start.
- The numbering check is form-aware — it reads the mint proof from
  the frontmatter `claim:` key — and the added-line public-record
  scan holds as before.
- The immutability exception is spent, not precedent: accepted ADRs
  are immutable again, format and content alike.
- A vault checkpoint rewrites nothing, so the review surface stays a
  plain markdown diff.
