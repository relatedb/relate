### Changed

- Cypher text may carry `\uXXXX` unicode escapes in the whitespace
  between tokens and in identifier text — `\u2028` between two tokens
  reads as whitespace, `\u0041` as the identifier `A` — matching the
  reference server's decoding. Parameters, numbers, comments and
  backtick names keep their rejections: an escape cannot spell a
  parameter name or any other token (ADR-0619).
