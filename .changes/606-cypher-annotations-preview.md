### Added

- CIP-281 Cypher annotations behind `RELATE_PREVIEW=cypher-annotations`: a
  clause may be preceded by `@name(params)` annotations, which are parsed and
  checked, then discarded with no runtime effect. Parameters are literals
  only; two or more must all be named. Without the opt-in `@` stays a syntax
  error.
