# ADR-0583: Fulltext accent folding and wildcard terms

Status: Proposed
Claim: 336cb03fca1179883680c9cab73c1eb0ffc10382

## Context

This ADR extends ADR-0578, which gave the fulltext scanner (shared by the `SEARCH` clause and the
`db.index.fulltext.query*` procedures) fuzzy terms and boosts. Two gaps
remain against live Neo4j. A `standard-folding` index never folded
anything: every analyzer name except `english` mapped to the plain
standard tokenizer, so `eifrem` missed `Eifrém` and `andreas sateras`
missed `Andreas Satéras`. And `*` / `?` were separators, so a prefix such
as `Kollegg*` became the literal term `kollegg`.

The scanner stored tokens analyzed without reference to the index's own
analyzer; only the query side was differentiated. Behaviour below was
calibrated with live probes on Neo4j 2026.09-enterprise (2026.10.1 was not
available), across the default, `standard-folding`, `english`, `cjk`,
`french`, `german`, `simple`, `whitespace` and `keyword` analyzers.

## Decision

1. **Which analyzer folds.** Only `standard-folding` folds; the other
   probed names do not. (`german` matches `muller` through its own German
   normalisation, which is not modelled.) The fold is Lucene's ASCII
   folding: Latin diacritics drop, and ligatures and letter variants
   expand (`ß` to `ss`, `æ` to `ae`, `œ` to `oe`, `ø` to `o`, `ł` to `l`,
   `þ` to `th`), as do full-width forms. Greek and Cyrillic are untouched.
   It is a small hand-written table over the Unicode normalisation crate
   the executor already depends on, so no dependency is added. A
   standalone combining mark is dropped, a deliberate superset of Lucene.
2. **Where folding applies.** To the index's stored tokens (the index's
   configured analyzer is now read) and to analyzed query terms. Without
   `WITH ANALYZER` a query uses the index's folding but not its stemming,
   because stored tokens remain unstemmed (the ADR-0578 shortcut
   stands). `WITH ANALYZER` changes the query side only, as live does: a
   folded query term against an unfolded index still misses.
3. **Fuzzy terms are not folded.** Live lowercases them only and measures
   distance from the raw term to the folded stored term: `eifrém~0` misses
   on a folding index while `eifrém~1` reaches `eifrem`. The existing
   behaviour is therefore kept; the distance is not measured after folding.
4. **Wildcard syntax.** `*` is any run (including none), `?` exactly one
   character, a leading wildcard is allowed, `**` equals `*`, and a
   backslash makes the next character literal. A wildcard term is
   lowercased and never stemmed or stop-filtered. **Deliberate deviation
   from live 2026.09:** live lowercases wildcard terms only, so on a
   folding index `mull*` finds `Müller` and `Müll*`, `MÜLL*` and `EIFRÉ*`
   find nothing. Here, under an analyzer that folds, the literal parts of
   the pattern are folded like the stored tokens and plain terms, so all
   of those match; `*`, `?` and escapes are untouched, and `?` counts one
   character of the folded token (`stra?e` does not match `strasse`,
   `stra??e` does). Other analyzers are unchanged. Inside quotes the
   wildcard characters are plain text, as live reads `"tok1*"` as the term
   `tok1`. A `~N` suffix on a wildcard is ignored; `*^B` boosts. `*`,
   `field:*` and `*:*` match every entity with a token in the field.
5. **Expansion and scoring.** A wildcard expands against each field's
   vocabulary into the same any-of group the fuzzy path builds, so
   `AND`/`OR`/`NOT`, `field:` filters and boost compose unchanged. A field
   scores a constant 1.0 when it holds any matching term, however many
   terms or occurrences, summed over fields and clauses and times the
   boost. There is no expansion cap: live expands 3000 distinct terms with
   no clause-limit error, so a silent cap would drop real hits.
6. **Errors.** Suffix errors keep ADR-0578's typed syntax errors; a
   wildcard-only word may carry a boost; a stored analyzer name the engine
   does not know is reported as index-definition drift.

## Alternatives considered

- **Leave wildcard terms unfolded, as live does.** Rejected: a user who
  types `Müll*` on a folding index and finds nothing, while `müller`
  matches, is surprised; folding is the index's stated contract.
- **Fold fuzzy terms too.** Rejected for now: live does not, and the
  distance semantics against folded stored tokens need their own decision.
- **Add a search-library dependency for analysis.** Rejected: a heavy
  second analyzer stack for a table of Latin folds.
- **Full Unicode folding of every script.** Rejected: live leaves Greek and
  Cyrillic distinct, and the goal is parity.
- **Cap wildcard expansion like fuzzy (50 terms).** Rejected: live has no
  cap, so truncation would silently lose matches.
- **Score wildcards by per-term BM25.** Rejected: live scores a constant
  1.0 per matching field.
- **Apply the index analyzer's stemming and stop words to stored tokens.**
  Rejected for this change: independent, larger, and unneeded for folding.

## Consequences

- Folding indexes now match accent-insensitively; the three reported
  queries (`eifrem`, `andreas sateras`, a `Kollegg*` prefix) work.
- Queries containing `*` or `?` outside quotes now mean wildcard; before,
  they tokenized as separators.
- A wildcard costs one vocabulary pass per field per scan, and its
  expansion set is bounded by the vocabulary.
- Wildcard folding is the one place this change knowingly differs from
  live 2026.09 behaviour.
- Known divergences: wildcard terms under `english` are not stemmed or
  stop-filtered while stored tokens are unstemmed, so `runn*` matches here
  and not live; `german` normalisation is not modelled; the procedures
  still ignore their `analyzer` option.
