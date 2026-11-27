### Fixed

- A deprecated function called inside a SEARCH match filter (`id(m) IN …`) now reports one deprecation notification per call site, like every other call position, instead of one for the whole statement.
- A scalar right operand of `IN` — e.g. a number, string, boolean or temporal parameter — now answers as membership in the one-element list holding it (`x IN 7` is `x IN [7]`), instead of raising a type error. Containers that are not lists (a map, a vector) still raise the type error, the compile-time literal form (`1 IN 2`) still rejects, and vector/fulltext search filters keep their own list rule.
