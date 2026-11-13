# ADR-0217: One shared single-input aggregate table, exact-arity backstop

Status: Proposed

## Context

ADR-0225 gave the single-input aggregates a two-layer arity contract: front
validation (`check_aggregate_arity`) plus an `agg_eval` backstop. The
review pass on the LABS-276 branch found the contract encoded three times
and drifting:

- the front held a hand-maintained five-name table
  (`ONE_INPUT_AGGREGATES`) beside the canonical
  `cypherlite_ir::AGGREGATES` set, and the two disagreed about the class:
  `stdev`/`stdevp` are single-input aggregates in `AGGREGATES` but absent
  from the front table, so `RETURN stdev()` skipped validation and surfaced
  only as the executor's string-style error — the same malformed-arity
  class, two different error shapes.
- the backstop fired for every member of `AGGREGATES` with zero arguments
  and hardcoded "expected 1", so `percentileCont()` was rejected with a
  wrong expected count, preempting the existing two-argument call-site
  check ADR-0225 explicitly preserved.
- the backstop tested only `args.is_empty()` while the front tested
  `args.len() != 1`, so an IR construction reaching `agg_eval` without
  front validation could still evaluate `sum(x, y)` and silently drop the
  second argument.

## Decision

One table, one contract, both layers:

1. **The class lives once**, in `cypherlite-ir` next to `AGGREGATES`:
   `SINGLE_INPUT_AGGREGATES = ["sum", "avg", "min", "max", "collect",
   "stdev", "stdevp"]`. `stdev`/`stdevp` join the class ADR-0225's
   normative "every single-input aggregate" wording already described —
   the reference behavior is the same `42I13` error. `count` stays out
   (the parser's keyword path already guarantees its arity) and the
   percentiles stay out (two arguments, their own call-site check).
   `cypherlite-front::agg` re-exports it, as it does `AGGREGATES`.
2. **Front validation** consumes the shared table unchanged in reach:
   any member call with arity other than one is
   `ErrorKind::InvalidNumberOfArguments`, over RETURN/WITH items, GROUP BY
   keys and ORDER BY expressions, with one message clause
   (`name(input) requires exactly one argument, got N`).
3. **The executor backstop** draws every aggregate's arity contract from
   one home: `cypherlite-ir` holds a single
   `(name, AggregateArity)` table from which `AGGREGATES` and
   `SINGLE_INPUT_AGGREGATES` (the `SingleInput` class minus `count`)
   derive — an aggregate without a contract, or a class table that
   disagrees with the vocabulary, is unrepresentable — and
   `aggregate_arity(name)` returns the contract — `SingleInput` (exactly
   one argument, the `InvalidNumberOfArguments` shape) for `count` and
   the `SINGLE_INPUT_AGGREGATES` members, `PercentilePair` (exactly two,
   the historical percentile message) for the percentiles. The class owns
   the rejection message, so the executor's one guard builds the error
   from the contract. A call with
   any other arity errors before the row loop — the single-input class
   with the same message shape the front raises, the percentiles with
   their historical "percentile needs two arguments", so one arity class
   has one error. Two gaps found in review are closed by drawing from the
   full table rather than the single-input subset: a backtick-escaped
   `` `count`() `` bypasses the parser's count keyword path (and front
   validation) and previously panicked on `args[0]` over a non-empty
   group; and a percentile call with more than two arguments previously
   passed the `args.len() < 2` guard and silently dropped `args[2..]`.

## Alternatives considered

- **Keep the five-name front table and add stdev/stdevp to it.** Rejected:
  it leaves the class defined twice (front table plus the executor's
  inline condition) and the next aggregate still lands in only one of
  them — the drift this ADR removes.
- **Move the backstop's expected count per function into the executor.**
  Rejected: a per-function expected-count switch in the executor
  duplicates the table again. The review round's refinement lands the
  switch in `cypherlite-ir` instead — `aggregate_arity` next to
  `AGGREGATES` is the single home, and the executor keeps one guard.
- **Drop the executor backstop (front validation is guaranteed).**
  Rejected: ADR-0225 keeps the second layer exactly for IR constructions
  that bypass the front (the planner-ir diagnostic parser included), and
  the exact-arity form is strictly stronger than the zero-argument form
  it replaces.

## Consequences

- `RETURN stdev()` / `stdevp()` (and their alias spellings) is now
  the front's typed `InvalidNumberOfArguments` error instead of the
  executor's string-style one — same rejection, one shape.
- `RETURN sum(x, y)` is rejected at validation; an IR construction that
  bypasses the front gets the same rejection from `agg_eval` instead of
  silently aggregating only the first argument.
- `percentileCont()` / `percentileDisc()` with any arity other than two
  keep their historical "percentile needs two arguments" error, as
  ADR-0225 promised; over-arity calls no longer silently drop `args[2..]`.
- A backtick-escaped `` `count`() `` is now the executor's
  `InvalidNumberOfArguments` error instead of a panic (non-empty group)
  or a silent `0` (empty group).
- Adding a new single-input aggregate means one edit to
  `SINGLE_INPUT_AGGREGATES`; both layers follow.
- The openCypher TCK corpus is unaffected (well-formed calls only);
  `cypherlite-tck` stays 3892/3892.
