---
type: adr
area: engine
claim: 091ee34868790f53fee839d8145889d5ed33f924
date: 2026-11-20
deciders: MemoryDB maintainers
name: 0636-fulltext-index-analyzer-both-sides
status: proposed
tags:
  - search
title: Fulltext: the index's analyzer analyzes stored tokens and the default query
REFERENCES:
  - "[[adr/0578-fulltext-fuzzy-and-boost]]"
  - "[[adr/0583-fulltext-accent-folding-and-wildcards]]"
---

# 0636: Fulltext: the index's analyzer analyzes stored tokens and the default query

## Context

The fulltext scanner — shared by the SEARCH clause and the
`db.index.fulltext.query*` procedures — applies a fulltext index's
configured `fulltext.analyzer` only as accent folding. Stored tokens pass
through the plain (or folding) tokenizer, never the configured analyzer's
stop-word filter or stemmer, and a query without `WITH ANALYZER` follows
the index's folding only:
[ADR-0583](./0583-fulltext-accent-folding-and-wildcards.md) kept
[ADR-0578](./0578-fulltext-fuzzy-and-boost.md)'s shortcut standing.

An `english`-configured index therefore misses the inflection crossings
live Neo4j answers with the same configuration: a query for `refunds`
finds no document holding `refund`, `charge` no `charged`, and a stop
word still matches every document carrying it. The option is admitted,
validated and echoed while its analysis never runs — the silent-fallback
shape the engine rejects everywhere else, and the reported defect: the
reporter ranked the index's documents in application code because the
index could not.

## Decision

1. **Stored tokens run the index's configured analyzer in full.** The
   `english` behaviour drops its stop words and Porter-stems; the
   `standard-folding` behaviour folds; the default and every other
   admitted name keep the plain tokenizer. The corpus statistics the BM25
   score reads are computed over these analyzed tokens.
2. **The no-override query follows the index's analyzer.** A query
   without `WITH ANALYZER` is analyzed exactly as the stored tokens were,
   so both sides agree; `WITH ANALYZER` still overrides the query side
   only, and cannot restore a token the index's analyzer dropped at store
   time.
3. **Wildcard and fuzzy terms are unchanged.** They are never stemmed or
   stop-filtered; they match the now-analyzed stored tokens, which is
   live's shape — a wildcard crosses inflections through the stems.
4. **The differentiated set stays two.** Only `english` and
   `standard-folding` carry behaviour of their own; the other admitted
   names share the plain tokenizer, so each unmodelled language stemmer
   remains a named divergence instead of a silent approximation.

## Alternatives considered

- **Keep the query-side-only shortcut.** Rejected: the reported defect is
  exactly that the echoed analyzer runs nothing — a stemmed query against
  unstemmed stored tokens matches only base forms, and a stop word still
  hits.
- **Reject the `english` option loudly instead of applying it.**
  Rejected: the name is live-admitted, so rejecting it breaks parity and
  existing stores, and the analysis is small and already in the engine.
- **Model every admitted analyzer's language stemmer.** Rejected: one
  stemmer per language for no reported need; `english` covers the
  reported corpus, and the divergence is named rather than approximated.

## Consequences

- An `english` index now matches across inflections and drops stop words
  on both sides, through the procedures and the SEARCH clause alike; the
  reported queries (`refunds`, `charge`, multi-term and prefix forms)
  answer as live does.
- Scores on such indexes change: the statistics are read over analyzed
  tokens. Default-analyzer indexes are unchanged.
- Wildcard and fuzzy clauses match stems, so a wildcard no longer needs
  to spell an inflection live's stemmer would remove.
