---
type: adr
area: surface
claim: ed05d0e11e359a0b1094074dc71ee7cccc0bd206
name: 0510-ffi-commit-status-convention
status: accepted
tags:
  - ffi
title: The C ABI v2 commit call reports through its status
REFERENCES:
  - "[[adr/0310-bolt-bookmarks]]"
  - "[[adr/0502-typed-c-abi-v2]]"
  - "[[adr/0506-ffi-numeric-entity-ids]]"
---

# ADR-0510: The C ABI v2 commit call reports through its status

## Context

[ADR-0502](./0502-typed-c-abi-v2.md) gave the typed C ABI v2 one error channel (§2): every fallible
status call returns `cypherlite_status` and fills a trailing
`char **error_json` out-param, while statement failures ride the result.
`cypherlite_conn_commit` is the one call [ADR-0502](./0502-typed-c-abi-v2.md) §7 left off that
convention: it returns `char *` — the transaction's bookmark — where
NULL means either "the commit failed" (the shape rides `*error_json`)
or "the transaction committed nothing" (no bookmark is fabricated for a
commit that crossed no WAL boundary). The filled out-param is the only
discriminator between the two NULLs, and a NULL `error_json` is a
documented opt-out — so a caller that opted out of the detail cannot
tell a failed commit from an empty one at all. The end-of-feature audit
of the typed-ABI-v2 stream flagged this as blocker #2. The ABI is
pre-public ([ADR-0502](./0502-typed-c-abi-v2.md) §8's additive-only freeze starts at the first
public release) and no external users exist, so the fix is a clean
break, not an additive symbol.

## Decision

`cypherlite_conn_commit` joins the status convention:

```c
cypherlite_status cypherlite_conn_commit(cypherlite_connection conn,
                                         char **bookmark_out,
                                         char **error_json);
```

- `CYPHERLITE_OK` with `*bookmark_out` non-NULL: committed — the
 bookmark is the engine's [ADR-0310](./0310-bolt-bookmarks.md) `cypherlite:<n>` string,
 caller-owned and freed with `cypherlite_free_string` ([ADR-0502](./0502-typed-c-abi-v2.md) §7,
  unchanged).
- `CYPHERLITE_OK` with `*bookmark_out` NULL: committed nothing — no
  bookmark is fabricated for a commit that crossed no WAL boundary.
- `CYPHERLITE_ERROR`: the commit failed — `*bookmark_out` is NULL and
 `*error_json` carries the [ADR-0502](./0502-typed-c-abi-v2.md) §2 shape; passing NULL for
  `error_json` still opts out of the detail, and the status alone now
  carries the failure/empty discrimination.

A NULL `bookmark_out` is a **defined refusal**: `CYPHERLITE_ERROR` with
the internal-stage "null out pointer" boundary shape, and the
transaction is NOT committed. Two reasons: every payload out-param of
this ABI (`database_open`'s handle out, `query_params`' result out)
refuses NULL exactly this way — only the `error_json` detail channel is
opt-out-able — and commit is the durability boundary: running it and
silently discarding the bookmark would be the silent-fallback class the
codebase rules out, on the one call whose payload is the durability
position.

`cypherlite_abi_version()` stays `2`: the signature change lands inside
[ADR-0502](./0502-typed-c-abi-v2.md) §8's pre-public window, the same window [ADR-0506](./0506-ffi-numeric-entity-ids.md) relied on
(theirs added symbols, this one moves a signature — allowed only
because no external users exist).

## Alternatives considered

- **Keep the `char *` return ([ADR-0502](./0502-typed-c-abi-v2.md) §7 as written).** Rejected: an
  opted-out caller cannot distinguish a failed commit from an empty one,
  and the convention break is the audit blocker; the pre-public window
  is the one chance to fix it without a version bump.
- **A NULL `bookmark_out` as a documented opt-out (commit runs, bookmark
  discarded).** Rejected: it diverges from every other payload
  out-param's refusal semantics and silently drops the durability
  position of a call the caller asked to run; no caller need exists to
  serve.
- **A bookmark accessor on the connection, read after a status-only
  commit.** Rejected: it parks state on the handle and adds a second
  read path for a value the call already holds — the out-param hands
  the bookmark over with the outcome, in one call.

## Consequences

- The header, the `docs/api/c` spec and examples, the FFI test suite and
  the C skill-bundle reference describe the new contract; the FFI suite
  covers the write commit, the empty commit, the no-transaction failure,
  the opted-out discrimination and the NULL `bookmark_out` refusal.
- The in-repo clients (JavaScript, Java, Go, Swift) break at this
  change; their migration is charted as the typed-ABI-v2 stream's own
  downstream nodes — expected, not an accident.
- `cypherlite_free_string`'s domain is unchanged: the bookmark stays
  caller-owned, only the delivery path moved.
- [ADR-0502](./0502-typed-c-abi-v2.md) §2's convention and §7's bookmark semantics stand; only the
  commit call's signature moved (Later references note added there).
