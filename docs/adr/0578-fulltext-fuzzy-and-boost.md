# ADR-0578: Fulltext fuzzy terms and clause boost

Status: Proposed
Claim: 723bf0a96ab22e3bede3571861cb6f973ab137c8

## Context

The fulltext scanner (the `SEARCH` clause and the `db.index.fulltext.query*`
procedures share it) reads the Lucene-style query syntax but not the
fuzzy (`term~N`) and boost (`term^B`) suffixes. The suffix characters were
treated as separators, so `kolleger~1` became the two required terms
`kolleger` and `1`: a misspelling never matched, and `kolleger~1 AND lamp`
found nothing. Live Neo4j, whose fulltext indexes are Lucene, matches near
spellings and scales clause scores by the boost.

The engine has no inverted index: each query scans the index's entities
and builds per-field statistics (document counts, lengths, term document
frequencies) once per scan.
The behaviours below were calibrated with live probes against Neo4j
2026.09-enterprise; the few not probed are named in the decision.

## Decision

1. **Syntax.** `term~`, `term~N`, `term^B` and `field:term~N^B` are parsed
   into typed clause fields (an optional edit bound, a boost defaulting to
   1). A bare `~` is 2 edits; an integer above 2 clamps to 2; `~x` with
   `0 <= x < 1` is a minimum similarity, an edit bound of `floor((1 - x) *
   term length)` capped at 2 (`~0` is exact). A non-integer of 1 or more
   (`~1.5`) is a syntax error. Text after a `~` that is not a `^` boost is
   a separate word (`term~1x` is a bare `~` plus the term `1x`), and a
   detached `~1` or `^2` binds to the previous word, as Lucene's lexer
   reads them. A boost is an unsigned number: `^0` is legal (the clause
   still matches, scoring 0); `^`, `^x` and `^-1` are syntax errors. The
   suffix never leaks digits as terms.
2. **Expansion.** A fuzzy clause expands, per field, against that field's
   vocabulary (the document-frequency table already built per scan) by a
   bounded Damerau-Levenshtein distance (optimal string alignment: insert,
   delete, substitute, adjacent transposition each cost one; early exit
   past the bound). Lucene defaults apply: no pinned prefix, at most 50
   expansions, ordered closest, then most frequent, then lexicographic so
   results are deterministic. The expansions form an any-of group, so
   `AND`/`OR`/`NOT`/required semantics are unchanged. The expansion keeps
   only the best 50 while it scans, so its memory is bounded by the cap, and
   a query of more than 1024 clauses (Lucene's default clause limit) is a
   typed syntax error at parse, so the clause count cannot multiply the
   vocabulary scans.
3. **Scoring.** Each expanded term scores with the existing calibrated
   BM25, multiplied by a closeness weight of `1 - edits / min(query length,
   term length)`; exact terms weigh 1. A candidate whose weight would be
   zero is not a match. The clause's boost multiplies its score last;
   boost never changes which rows match.
4. **Reuse.** The distance routine is a small crate-private module so a
   future Cypher string-similarity function can share it.
5. **Calibration.** The suffix grammar, clamping, the closeness weight
   (0.875 at one edit and 0.75 at two on an eight-character term, scores
   exactly weight times BM25), exact boost scaling and the AND/NOT
   behaviour were confirmed on Neo4j 2026.09-enterprise. Not modelled:
   Lucene's document-frequency blending across expansions, and a fuzzy
   word that analyzes to several tokens applies the suffix to each token.

## Alternatives considered

- **Add a search-library dependency (an embedded Lucene port).** Rejected:
  a heavy dependency in the engine tree for one feature, a second
  analyzer/scoring stack beside the calibrated one, and an index the
  in-memory, scan-based design does not maintain.
- **Build a Levenshtein automaton intersected with a term dictionary.**
  Rejected: it pays off against a large sorted dictionary; the per-scan
  vocabulary is small and hash-ordered, so a bounded dynamic program with
  early exit is simpler and fast enough.
- **Maintain a persistent inverted index with a term dictionary.**
  Rejected: new durable state, write-path cost and recovery format for a
  query-time feature; independent of this decision and larger than it.
- **Treat suffix errors leniently (ignore the bad suffix).** Rejected: it
  silently changes the query, the failure mode this change removes.
- **Match fuzzy terms without a closeness weight.** Rejected: an exact
  spelling would tie with its neighbours, unlike Lucene.

## Consequences

- Fuzzy queries cost one vocabulary pass per fuzzy clause per field per
  scan, on top of the existing scan; no index maintenance.
- Queries containing `~` or `^` that previously tokenized loosely now
  either mean fuzzy/boost or fail with a syntax error.
- The procedures and `SEARCH` now return a typed error for a bad query
  suffix instead of an empty or surprising result.
- Exact, unboosted queries are unchanged.
- Phrase adjacency and `+`/`-` modifiers remain unsupported.
