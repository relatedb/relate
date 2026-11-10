# ADR-0559: Run-time argument rejections are their own error variant

Status: Proposed
Claim: 0d13d0b324cc7c45f5c96f59945642fc0ca94678

## Context

A family of function-argument rejections is discovered while a query
produces rows, not while it is bound: `range()`'s zero step, the
`substring()` / `left()` / `right()` range checks, list-slice bounds,
`point()`'s in-map values, `bar()`'s width and the percentile aggregates'
argument. Each already carries a typed kind (`NumberOutOfRange`,
`InvalidArgumentValue`) in its detail text, but the variant carrying it
was the legacy type-class one, so the classifier reported the type-error
class and the legacy `Neo.ClientError.Statement.TypeError` code.

Live Neo4j reports these statements under the `ArgumentError` class at
run time, with `Neo.ClientError.Statement.ArgumentError` — the same class
as the DDL `OPTIONS` / `indexConfig` rejections but a different code and
stage. The DDL variant is that compile-stage row: its classifier maps to
`Neo.ClientError.General.InvalidArguments`. Reusing it for run-time
failures would report the wrong code and stage to every surface, and
reusing the type-class variant keeps the wrong class. The mapping is
protocol-visible, so the decision is recorded rather than left implicit
in the construction sites.

## Decision

A run-time argument rejection is a distinct typed variant carrying the
kind and the detail as data, not as a prefix inside a message string.

- The rendered text keeps the pinned `semantic error: <Kind>: <detail>`
  shape, so the detail token and its wording do not move; only the class
  prefix changes, from the type-class rendering to the argument one.
- One constructor names the class and takes the typed kind; a consumer
  reads the kind and the detail through the typed-detail accessor the
  other typed variants already answer, so nothing parses rendered text.
- The classifier maps the variant to
  `Neo.ClientError.Statement.ArgumentError`, a general GQLSTATUS, the
  runtime stage, and non-retryable.
- The sites above migrate. A non-numeric percentile is a wrong-type
  rejection and keeps the invalid-argument-type variant, not this one.
- The plain-detail run-time sites that already build the DDL argument
  variant — the temporal constructors, point-component access, regex
  compilation and the fulltext analyzer option — are not migrated here:
  they report the DDL class today, and moving them changes the code they
  send.

## Alternatives considered

- **Keep the stringly type-class rendering.** Rejected: it classifies as
  a type error at the compile stage, and a consumer must parse the
  message to recover the class.
- **Reuse the DDL argument variant.** Rejected: it is the compile-stage
  row for `Neo.ClientError.General.InvalidArguments`; run-time failures
  would report the wrong code and stage.
- **One argument variant with the stage as a field.** Rejected: the two
  rows differ in code, stage and constructor shape, and a stage flag at
  every DDL match site is a second place for the mapping to drift.
- **Carry the kind as a prefix inside the detail string.** Rejected: it
  is stringly-typed discrimination; the kind is a typed value and
  consumers are entitled to it without scanning the rendered message.
- **Migrate every existing argument-rejection site.** Rejected: the
  plain-detail temporal, point-component and regex sites report a
  different, already-pinned code; reclassifying them is a separate
  decision with its own probes.

## Consequences

- The public Rust error enum gains a variant; every exhaustive match is
  compiler-checked.
- Bolt, the bindings and the CLI report the argument class at the runtime
  stage for the migrated sites; their rendered text keeps the
  `semantic error: <Kind>: <detail>` shape.
- The `CALL … IN TRANSACTIONS … REPORT STATUS` message hands over the
  bare `<Kind>: <detail>` text, as it does for the other typed-detail
  variants.
- ADR-0168 and ADR-0175 pin the old type-class rows for these sites; both
  gain Later references notes pointing here.
