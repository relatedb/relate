---
type: adr
area: surface
claim: f14e5ae971e01500393bf1d6179ce380b884ce6f
name: 0342-bolt-request-fields-fail-malformed
status: proposed
tags:
  - bolt
  - errors
title: Bolt request scalar fields fail malformed values with Request.Invalid
REFERENCES:
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0177-bolt-parameter-conversion]]"
  - "[[adr/0307-bolt-failure-carries-neo4j-status-codes]]"
  - "[[adr/0311-bolt-tx-open-results-by-qid]]"
  - "[[adr/0313-bolt-hello-requires-user-agent]]"
---

# ADR-0342: Bolt request scalar fields fail malformed values with Request.Invalid

## Context

The end-of-feature review of the Bolt compatibility work (security finding
F3) found three related gaps in how the session decodes the scalar fields
of a request:

- A RUN whose statement field is absent or not a string was executed as
  the empty statement (`fields.first().and_then(Pack::as_str).unwrap_or("")`),
  so the client received a misleading engine syntax error instead of a
  failure naming the malformed message — while the params field a few
 lines below fails loudly ([ADR-0177](./0177-bolt-parameter-conversion.md)).
- The PULL/DISCARD `n` and `qid` fields used `-1` as both the documented
  absent default and the fallback for a present-but-malformed value
  (`n: "2"`, `qid: 2.5`), so malformed wire input silently became
  "drain everything" or "the most recent result" instead of failing like
 the unknown-`qid` path beside it ([ADR-0311](./0311-bolt-tx-open-results-by-qid.md)).
- Four bespoke scalar extractors existed — `extra_db`, `extra_int`, the
  HELLO `get` closure and the ROUTE address lookup — each re-decoding the
  same map with its own empty/absent policy.

[ADR-0177](./0177-bolt-parameter-conversion.md) already fixes the malformed-parameter policy: a value the server
cannot convert fails the RUN with `Neo.ClientError.Request.Invalid`
([ADR-0307](./0307-bolt-failure-carries-neo4j-status-codes.md)) and enters the failed state ([ADR-0110](./0110-bolt-connection-query-governance.md)). [ADR-0313](./0313-bolt-hello-requires-user-agent.md) requires a
string `user_agent` on HELLO. The request-field decode should follow the
same policy through one accessor family.

## Decision

1. **One extra-accessor family** (`extra`, `extra_str`, `extra_int`) in
   `session.rs` replaces the four bespoke copies. `extra` is the one map
   lookup; `extra_str` extracts a non-empty string; `extra_int` returns
   `Result<Option<i64>, String>` where `None` means only "the key is
   absent" and a present value of another type is an error naming the
   found kind.
2. **Absent keeps the documented default; present-but-malformed fails.**
   A key that is absent (or an empty string, which every string field
   treats as absent) applies the documented default: `-1` for `n`/`qid`,
   the home database `neo4j` for `db`, the connection's local address for
   ROUTE's `address`. A present value of the wrong type fails the message
   with `Neo.ClientError.Request.Invalid` and the failed state, exactly
 like the [ADR-0177](./0177-bolt-parameter-conversion.md) parameter path: `unsupported extra value for
   <key>: <kind> (must be an Int)`.
3. **The RUN statement field is required.** An absent or non-string
   statement is the same malformed-request failure: `unsupported request
   value for statement: <kind> (RUN statement must be a string)`, or
   `missing RUN statement (RUN statement must be a string)` when the field
   is absent. Nothing executes and the session enters the failed state.
4. **An empty `user_agent` is rejected like an absent one** ([ADR-0313](./0313-bolt-hello-requires-user-agent.md)):
   `extra_str` treats `""` as absent, and HELLO requires the field, so an
   empty string gets the same `Illegal value for field "user_agent"` text.
5. **The `-1` sentinel is only a default, never a malformed-value
   fallback**, and the same policy covers `n` and `qid` (a malformed
   `qid` no longer silently addresses the most recent result).

## Alternatives considered

- **Keep `unwrap_or("")` and the `unwrap_or(-1)` sentinel.** Rejected: it
  hides a malformed request behind a legal default, diverges from the
  params path's explicit failure, and is the security F3 finding.
- **Fail only the statement and keep the integer sentinel.** Rejected: a
  malformed `qid` silently addresses another open result and a malformed
  `n` silently drains the result — the same class of bug, in the same
  message family.
- **Make `extra_str` keep empty strings and special-case HELLO.** Rejected:
  it re-splits the one accessor policy; `db` and `address` already treat
  `""` as absent, and an empty `user_agent` names nothing useful, so
  HELLO rejects it as absent.
- **Return `Option<i64>` and let each call site decide on malformed
  values.** Rejected: a present-but-malformed value is not an absent one;
  the typed error must travel out of the accessor so every call site
  cannot re-invent the policy.

## Consequences

- A malformed RUN/PULL/DISCARD/HELLO scalar field answers
  `Neo.ClientError.Request.Invalid`, sets the failed state (IGNORED until
  RESET) and never executes or consumes anything. RESET recovers.
- The `n`/`qid` defaults apply only when the key is absent; the unknown-`qid`
  path (`no open result with qid <qid>`) is unchanged.
- One accessor family consults `Pack::get`; the four bespoke copies and
  the empty-filter duplication are gone.
- `docs/api/bolt/spec.md` documents the statement requirement, the
  malformed `n`/`qid` failure and the empty `user_agent` rejection.
- Pinned by the Bolt unit tests
  (`a_run_without_a_string_statement_is_rejected`,
  `a_malformed_qid_is_a_request_invalid_failure`,
  `a_malformed_n_is_a_request_invalid_failure`, the HELLO rejection loop)
  and the corpus scripts
  `tests/compat/scripts/protocol/run-malformed-statement.bolt` and
  `tests/compat/scripts/protocol/malformed-pull-extra.bolt`.

## Later references

- `bolt-compat-tests/fix-bolt-polish` (round-2 review): the `-1` default is
  only the absent-key default, so a present `n` outside `-1` and the
  non-negative Ints (e.g. `-5`) fails with
  `Neo.ClientError.Request.Invalid` — message
  `unsupported extra value for n: <n> (must be -1 or a non-negative Int)` —
  instead of falling into `usize::try_from`'s error arm and draining the
  result. The decode now reads `-1` drains, `n >= 0` takes
  `min(n, rows left)`, anything else fails, and the accessor family gains
  `extra_int_or` so the absent-key default is applied in one place. Pinned
  by `an_out_of_range_n_is_a_request_invalid_failure` and the extended
  `malformed-pull-extra.bolt` script.
- `bolt-compat-tests/fix-consume-decode` (round-3 review): the PULL/DISCARD
  decode moves out of `Session::consume` into `fields.rs` as
  `ConsumeRequest::parse` — the one malformed-request exit, matching
  `RunRequest::parse` — and the cap conversion becomes
  `usize::try_from(n).unwrap_or(usize::MAX)`, so an `n` above `usize::MAX`
  on a 32-bit target caps at the rows left instead of panicking on a
  64-bit-only invariant. Pinned by the `ConsumeRequest` unit tests and
  `a_huge_n_drains_the_result`.
