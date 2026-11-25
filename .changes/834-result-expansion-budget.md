### Fixed

- A result value that is small in memory but exponential once expanded
  (`reduce(acc = [1], x IN range(1, 60) | [acc, acc])`) is refused with the
  expression-memory-limit error instead of hanging the renderer, and a
  `table` or `markdown` cell longer than 4096 characters is cut with a
  trailing `…` (ADR-0691, REL8-641).
