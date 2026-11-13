# ADR-0219: Python parameter conversion — UUID, bytes, tuple, explicit Point, ordered maps

Status: Accepted

## Context

LABS-291 items 4–7 close gaps in the Python binding's parameter
conversion (`surfaces/cypherlite-python/src/convert.rs`):

1. `uuid.UUID`, `bytes`/`bytearray`, and `tuple` parameters are rejected
   with the generic "cannot convert a parameter of Python type …" error,
   even though each has an obvious Cypher value (the Neo4j Python driver
   accepts all three).
2. A plain dict carrying an integer `srid` plus `x`/`y` or
   `longitude`/`latitude` keys is *silently reinterpreted* as a point on
   the way in (`try_point_dict`). A user who passes
   `{"srid": 4326, "x": 1.0, "y": 2.0}` as an ordinary map parameter
   gets back a point, not the map they sent — a silent type change with
   no opt-in.
3. Returned map keys must reach the Python `dict` in the engine's map
   order; the surface must not reorder them.
4. Non-dict `params` and non-string keys fail with pyo3-internal wording
   ("'list' object cannot be converted to 'PyDict'") instead of a
   DB-API-shaped message.

The engine has no binary scalar value (`Value`/`CypherValue` carry no
byte-array variant), and openCypher defines none. Engine semantics are
out of scope for this change (owned by other LABS-291 items).

## Decision

Parameter conversion (`py_to_cypher_value_at`) now accepts:

- **`uuid.UUID`** → `STRING` of the canonical lowercase hyphenated form
  (`str(uuid)`), matching this surface's existing stance that UUIDs stay
  strings (`value_to_cypher` already renders engine UUIDs as
  strings).
- **`bytes` / `bytearray`** → `LIST<INTEGER>` of byte values 0–255.
  This is the only lossless representation the engine can store today;
  the ADR records the mapping explicitly so a future engine byte-array
  value can replace it in one place. The round trip is type-asymmetric
  by necessity: bytes sent come back as a list of ints.
- **`tuple`** → `LIST`, same as `list` (nested tuples included).

**Points become explicit-only.** The `srid`-sniffing dict
reinterpretation is removed: every Python `dict` parameter is a Cypher
map, full stop. A point parameter is constructed only from an explicit
`cypherlite.Point` (the shape returned points already have, so
round-tripping a returned point still works). Building a point from
coordinate parts stays in Cypher via `point($map)` / `point({...})`.
This is a behavior change: code that relied on the implicit
`{srid, x, y}` → point conversion must now pass
`cypherlite.Point(srid, x, y)` or build the point in the query.

**Map key order is preserved end-to-end on this surface.** The
converter walks the engine's `IndexMap` in iteration order and inserts
into the Python `dict` in that order (Python dicts are
insertion-ordered), and this is locked by tests. (The engine core's own
map storage order is engine semantics and out of scope here.)

**Parameter-shape errors speak the DB-API language.** `Database.execute`
/ `execute_arrow` take the `params` argument as an arbitrary object and
validate it themselves: a non-dict raises
`TypeError("query parameters must be a dict …")` and a non-string key
raises `TypeError("query parameter keys must be strings …")`. Both
surface through `Connection._raw_execute`'s existing
`TypeError → ProgrammingError` mapping, so callers see
`cypherlite.ProgrammingError` with an actionable message instead of
pyo3's internal conversion wording.

## Alternatives considered

- **Keep the `srid`-dict sniffing** — rejected: it silently changes the
  type of an ordinary map parameter, and the mis-fire
  (`{"srid": …, "x": …, "y": …}` as data) is undetectable and
  unrecoverable. Explicit construction is how every other typed value on
  this surface already works (temporals via `datetime`, points via the
  returned `Point` class).
- **Encode `bytes` as a hex or base64 `STRING`** — rejected: opaque to
  Cypher (no indexing, no length, no element access), and the encoding
  choice becomes a hidden contract. A list of byte values is at least a
  real Cypher value with faithful content.
- **Reject `bytes` until the engine grows a binary scalar** — rejected:
  PEP 249 mandates a usable `Binary` constructor and the data is
  representable losslessly today; the mapping is recorded here so it can
  be re-pointed at a future engine byte-array value without a new API.
- **`uuid.UUID` → two 64-bit integer halves (the engine's UUID storage
  form)** —
  rejected: the surface's documented stance is that UUIDs cross as their
  canonical string form; introducing a second representation only on the
  parameter path would split the dialect.
- **Raise pyo3's `PyTypeError` with the default extract wording** —
  rejected: that wording names pyo3 internals (`PyDict`) the user never
  wrote; the DB-API tree maps `TypeError` to `ProgrammingError`, so the
  message is what the user sees.

## Consequences

- `convert.rs` gains UUID/bytes/tuple arms and loses `try_point_dict`;
  `execute`/`execute_arrow` validate the `params` shape themselves.
- Dicts are always maps: a returned point round-trips through the
  `Point` class, but a dict built from a point's fields stays a map.
  This is a **breaking behavior change** for anyone relying on the
  implicit conversion; it is called out in the changelog and the Python
  docs (spec + description + library reference + both skills).
- Parameter errors from the native layer are stable, documented
  messages ending up as `cypherlite.ProgrammingError`.
- Returned map key order is a tested contract of the surface.

## Later references

- ADR-0327: the engine gains a statement-scoped byte array. This surface
  keeps sending `bytes`/`bytearray` as `LIST<INTEGER>`, which is also how
  `value_to_cypher` renders an engine byte array; only Bolt carries real
  bytes.
