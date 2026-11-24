### Fixed

- The fulltext query syntax rejects the Lucene constructs the engine does
  not implement — grouping parentheses and field groups, ranges, regular
  expressions, the `+`/`-`/`!` clause modifiers, a quoted phrase of more
  than one term, a dangling or doubled operator, and an empty query — with
  a syntax error, where they previously returned a request-shaped wrong set
  (`(gamma OR one) AND beta` answered `one AND beta`, `alpha AND !beta`
  required `beta`). The forms `docs/reference/cypher-syntax.md` lists are
  unchanged, and a quoted term keeps working after a field prefix
  (`title:"alpha"`), which previously dropped the field.
- Chinese and Japanese text in a fulltext index is searchable by its
  words: the tokenizer emits one term per Han ideograph and per Hiragana
  character and keeps a Katakana run whole, and the `cjk` analyzer emits
  overlapping bigrams over those runs instead of sharing the standard
  tokenizer (ADR-0672).
- `db.index.fulltext.queryNodes` and `db.index.fulltext.queryRelationships`
  accept their documented optional `options` map: `skip` and `limit` page
  the ranked hits, and `analyzer` names the query-side analyzer exactly as
  `SEARCH ... WITH ANALYZER` does. An unknown key is an argument error.
- The string-literal escapes `\r`, `\b` and `\f` decode to their control
  characters, in plain strings and in interpolated literals, so `'\r'` is
  one character and a stored `'a\r\nb'` holds a carriage return; an unknown
  escape still keeps its two-character form.
