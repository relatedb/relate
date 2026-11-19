---
type: adr
area: surface
name: 0222-javascript-tagged-template-queries
status: accepted
tags:
  - clients
title: JavaScript tagged-template queries
REFERENCES:
  - "[[adr/0216-javascript-client-ergonomics]]"
---

# ADR-0222: JavaScript tagged-template queries

## Context

LABS-291 (item 11). The Node client (`clients/javascript`,
[ADR-0216](./0216-javascript-client-ergonomics.md)) takes Cypher as a string plus a separate params object. The
idiomatic JavaScript convenience for this shape is a tagged template:

```js
conn.query`MATCH (p:Person {name: ${name}}) RETURN p`
```

Interpolating values into a statement string by concatenation is the
classic injection hole; a tag that rewrites each interpolation to a
parameter reference is injection-safe by construction — values never
enter the statement text. Tagged-template SQL/GraphQL clients (Slonik,
`sql` from Postgres.js, graphql-tag) have trained the audience to expect
the form. The decision is purely client-side: the engine, C ABI, and
every other surface are untouched.

## Decision

1. **`Connection.query` doubles as a template tag.** When the first
   argument is a template strings array (a call-site tag invocation),
   the tag builds the Cypher text with each interpolated value replaced
   by an auto-named `$p0, $p1, …` parameter and runs the existing
   `query(cypher, params)` path. The string form
   `query(cypher, params?)` stays the documented primary.
2. **Value dedup.** The same primitive value interpolated twice maps to
   one parameter (`SameValueZero` equality, `Map`-keyed); objects dedup
   by identity. Parameter names stay contiguous over the unique values.
3. **Unsupported values reject — in both forms, at any depth.**
   `undefined`, functions, symbols and non-finite numbers (`NaN`,
   `±Infinity`) throw a `TypeError` (the tag
   names the interpolation index, the string form the params key;
   nested leaves are named by their key path) — they have no
   JSON/Cypher representation, and silently stringifying them would
   drop the key (object) or null the slot (array) and corrupt the
   statement. The check walks nested objects and arrays; cyclic values
   terminate the walk and hit `JSON.stringify`'s own error. The
   plain-object rule applies at every depth: a nested
   Map/Set/Date/class instance has no enumerable own keys (or
   serializes via `toJSON`) — the same silent corruption. The string
   form also rejects a non-plain-object
   `params`. Both tightenings are loud failures at the client edge where
   the caller can see them. `null` is a value. A second argument on a tag
   call is a programmer error and throws.
4. **No fragment composition.** Interpolated values are always data;
   there is no nested-template or raw-fragment escape hatch — that would
   reopen the injection hole the tag exists to close.
5. **Docs and types land together**: the template-tag overload in
   `index.d.ts`, `docs/api/javascript/` spec + description + a tested
   example, the README, the CLI bundle's `client-javascript.md`
   reference, the npm-bundled library skill, and the CHANGELOG.

## Alternatives considered

- **String-concatenation ergonomics only (document `$params` better)** —
  rejected: the tag is the whole point; it removes the class of bug
  rather than documenting around it.
- **A separate `sql`-style export (`cypher\`…\`` producing a
  `{text, params}` pair)** — rejected: two query forms with a hand-off
  object instead of one method that accepts both call shapes; the
  method-tag form is one idea.
- **Fragment composition (nested tags splicing raw Cypher)** — rejected:
  useful for dynamic clauses, but it makes interpolated values sometimes
  code and sometimes data; a caller who needs dynamic text can build a
  string and use the primary string form.
- **Positional names `$1, $2` or names derived from the expression
  text** — rejected: `$1` is not the Cypher parameter spelling, and the
  tag receives values, not source text; `$p0…` is deterministic and
  collision-free against user params because the tag owns the whole
  statement.

## Consequences

- `conn.query` accepts two call shapes; `index.d.ts` carries the
  overload, and `test.js` covers interpolation, dedup, nested
  array/object values, and injection safety.
- No engine, FFI, or other-client work: the tag lowers to the existing
  params path before the C ABI boundary.
- Pre-1.0 npm package: additive, no breaking change.
