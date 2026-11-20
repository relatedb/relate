### Fixed

- An unaliased map projection in `WITH` (`WITH m{.*}`) now plans and runs,
  with the output column named after the base variable — the same implicit
  name `RETURN` already gave it — instead of demanding `AS`
  (`NoExpressionAlias`). The variable is shadowed by the projected map, the
  aliased form is unchanged, and `WITH m{.*}, m` still rejects as a
  duplicate column name.
- `CALL p() YIELD *` on a procedure body with no terminal RETURN (a FINISH
  body) now reports its own rejection — "a procedure body with no terminal
  RETURN cannot be yielded with `*`" — instead of the shared
  must-name-every-item message.
