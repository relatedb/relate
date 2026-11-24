---
type: adr
area: engine
claim: 1f6a3a8b6f3fec7d93a01e48fb42f6b8e19824f4
date: 2026-11-24
deciders: Relate maintainers
name: 0672-fulltext-cjk-tokenization-and-strict-query-syntax
status: proposed
tags:
  - dialect
  - search
title: Fulltext: UAX #29 CJK tokenization, real cjk bigrams, and a strict query syntax
REFERENCES:
  - "[[adr/0578-fulltext-fuzzy-and-boost]]"
  - "[[adr/0583-fulltext-accent-folding-and-wildcards]]"
  - "[[adr/0636-fulltext-index-analyzer-both-sides]]"
---

# ADR-0672: Fulltext: UAX #29 CJK tokenization, real cjk bigrams, and a strict query syntax

## Context

Three defects in the fulltext surface — the shared scanner behind the
`SEARCH` clause and the `db.index.fulltext.query*` procedures — have one
shape: the surface accepts what it does not implement and answers a
plausible wrong result.

1. **CJK text is one token.** The tokenizer split on any run of
   non-alphanumeric characters. Han, Hiragana and Katakana are alphabetic,
   so `中文搜索引擎` was one token: no word inside it was findable, with
   the default analyzer or with `cjk`, which was admitted by the analyzer
   catalogue and mapped to the plain tokenizer while its description
   promised bigrams.
2. **The query parser accepts syntax it does not implement.** Grouping
   parentheses, field groups, ranges, regular expressions, the
   `+`/`-`/`!` clause modifiers and multi-term phrases were dropped and
   their words read as a flat OR: `(gamma OR one) AND beta` returned
   `one AND beta`, and `alpha AND !beta` required `beta` — the opposite of
   the request. Malformed operator sequences and the empty query answered
   instead of failing.
3. **The procedures rejected their documented `options` map.** Both
   fulltext query procedures describe `{skip, limit, analyzer}` but were
   registered with two arguments, so every Neo4j call that pages a
   fulltext search failed at compile time.

## Decision

1. **The standard tokenizer follows UAX #29 for the scripts written
   without spaces.** One token per Han ideograph and per Hiragana
   character, a whole Katakana run as one token — live's shape, so `中文`
   and `搜索` find `中文搜索引擎` under the default analyzer. Every
   analyzer that shared the old tokenizer shares this one.
2. **`cjk` emits overlapping bigrams** over each Han/Hiragana/Katakana
   run; a one-character run yields no term, as live's does. The analyzer
   table's `cjk` row selects its own behaviour instead of the standard
   one — the differentiated set is three, extending
   [ADR-0636](./0636-fulltext-index-analyzer-both-sides.md) §4.
3. **The query syntax rejects what it does not implement.** Grouping
   parentheses and field groups, ranges (`[a TO b]`, `{a TO b}`), regular
   expressions (`/re/`), the `+`/`-`/`!` clause modifiers, a quoted phrase
   of more than one term, an operator without a term on each side, and an
   empty query are authored syntax errors. The supported forms are the
   ones `docs/reference/cypher-syntax.md` documents; quotes still make one
   term literal (also after a `field:` prefix, `title:"alpha"`), so a quoted
   operator or modifier is text — the scanner carries that provenance to the
   parser instead of re-sniffing the word — and a literal term the analyzer
   reads as more than one term is a phrase, refused because no adjacency is
   matched. The parser gains no grouping, range, phrase or regex-adjacency
   semantics. This removes [ADR-0583](./0583-fulltext-accent-folding-and-wildcards.md)'s
   documented "phrases add no adjacency constraint, `+`/`-` are not read"
   shortcuts: a shortcut a caller cannot see is a wrong answer, so the
   unsupported form fails loudly instead.
4. **The procedures accept their `options` map.** `skip` and `limit` page
   the ranked hits exactly as live's do; the whole ranking is the default
   and `{}` is the whole ranking. `analyzer` names the query-side analyzer
   through the same validation and mechanism as `SEARCH ... WITH ANALYZER`
   ([ADR-0636](./0636-fulltext-index-analyzer-both-sides.md)'s override,
   which supersedes [ADR-0583](./0583-fulltext-accent-folding-and-wildcards.md)'s
   "the procedures ignore their analyzer option" note). Any other key — and
   a bad value — is rejected with an argument error, never accepted and
   ignored.

## Alternatives considered

- **Keep CJK runs whole and reject the `cjk` name.** Rejected: rejecting
  an admitted name breaks existing stores, and the reported text is simply
  not searchable — the tokenizer fix is small and shared.
- **Implement the Lucene constructs** (grouping, ranges, regex, phrases,
  modifiers). Rejected for this change: each is its own feature with its
  own scoring and paging semantics; the defect is the silent wrong answer,
  which rejection removes. `SEARCH` is the current-search replacement and
  carries the same parser, so the rejection reaches every entry point.
- **Accept `analyzer` and apply it in the procedures.** Rejected here: it
  would contradict ADR-0583's recorded divergence without a decision of
  its own; the same mechanism is already reachable through
  `WITH ANALYZER`, and the rejection names it.
- **Accept `analyzer` and silently ignore it.** Rejected: the
  silent-fallback shape the engine rejects everywhere else — a caller
  would get hits the requested analyzer would not produce.
- **Treat a one-character `cjk` run as a unigram.** Rejected: live's
  output has no unigram, so `图` matches nothing there; parity over
  convenience.

## Consequences

- CJK fulltext indexes are searchable by their words; such an index's
  terms, scores and document lengths change. Nothing is persisted — the
  corpus is tokenized per scan — so no store migration is needed.
- `cjk` finally behaves as its catalogue description says.
- A Neo4j query using grouping, ranges, regex, modifiers or phrases now
  fails with a syntax error instead of returning a set that may be a
  subset or a superset of the request. Applications that relied on the
  documented flat-OR reading of quotes or `+`/`-` must rewrite the query.
- `CALL db.index.fulltext.queryNodes('i', 'q', {limit: 10})` and its
  relationship twin run, as live; `{analyzer: …}` fails with a message
  naming `WITH ANALYZER`.
- `SHOW PROCEDURES` lists the third argument as `options = {} :: MAP` for
  both procedures.
