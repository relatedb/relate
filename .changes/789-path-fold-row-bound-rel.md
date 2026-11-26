### Fixed

- `count(*)`, grouped counts and `count(DISTINCT …)` over a pattern whose
  relationship variable is bound by an earlier clause (`WITH r`, `UNWIND`, a
  `CALL` body, a failed `OPTIONAL MATCH`) now honor the pin instead of counting
  every relationship of the pattern's type.
