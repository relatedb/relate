# 0179. Bolt parameter conversion is crate-internal

- **Status**: Accepted
- **Date**: 2026-10-07
- **Deciders**: Oskar Hane (agreed in the end-of-feature review conversation), CypherLite maintainers (Linear LABS-189)

## Context

ADR-0177 item 1 pinned `Pack::to_mdb` as

```rust
pub fn to_mdb(&self) -> Result<cypherlite::Value, String>
```

and recorded it as a breaking change to the ADR-0281 library API. The
implementation (`surfaces/cypherlite-bolt/src/param_structs.rs`) shipped it
as a one-line delegate to the path-aware `to_mdb_at(&mut ParamPath::root())`,
and the RUN path never calls it: the parameter-conversion entry point
descends through `to_mdb_at` directly, one top-level parameter at a time,
so every failure carries the `$name` path ADR-0177 item 6 pins. Nothing in
production calls `to_mdb`; only the crate's own tests use it, as the
convenient whole-value root-path wrapper.

The end-of-feature stellar review flagged this as a boundaries defect: the
crate exports a public function with no production caller, and ADR-0177's
breaking-change record makes a client implementing embedded Relate track a
signature the server itself does not use. The PackStream surface ADR-0281
consumers link is `decode`, `encode`, `try_struct_msg`, `chunk_message`, and
the `Pack` type; the parameter conversion is an implementation detail of the
Bolt RUN handler.

## Decision

**`Pack::to_mdb` is crate-internal — test-only `pub(crate)` — and ADR-0177
item 1's `pub` signature and its breaking-change consequence are
superseded.**

The conversion contract itself is unchanged: `to_mdb` keeps the
`Result<cypherlite::Value, String>` shape and the root-path (`$`) failure
messages, and the RUN entry point (`params_to_map` in `param_structs.rs`)
continues to convert each top-level parameter through `to_mdb_at` with its
`$name` path. The public PackStream surface stays `decode`, `encode`,
`try_struct_msg`, `chunk_message`, and `Pack`; no public caller exists to
migrate.

## Alternatives considered

- **Keep `to_mdb` public as ADR-0177 pinned it.** Rejected: it is the
  reviewed defect — a public API with no production caller, and a recorded
  breaking change that only imposes migration work on consumers of a
  function the server never calls.
- **Delete `to_mdb` and rewrite its test callers onto `to_mdb_at`.**
  Rejected: every test call site would construct a `ParamPath` and pass a
  mutable reference to express "the whole value at the root", which is
  noise; the crate-internal wrapper keeps the tests reading as the
  root-path conversion they exercise, and the in-crate test modules keep
  compiling.
- **Move `to_mdb` behind the `cypherlite` facade (re-export) or add a new
  public conversion function.** Rejected: no consumer needs a whole-value
  conversion entry point; the Bolt RUN handler needs the path-aware form,
  and a second public path would repeat the same no-production-caller
  defect under a new name.
- **Leave the ADR-0177 record uncorrected and only change the
  visibility.** Rejected: ADR-0177 item 1 and its Consequences still claim a
  breaking public API change, so the decision log would contradict the
  code; per the decision-log rule, a later decision supersedes it and the
  old ADR gains a Later references note.

## Consequences

- `Pack::to_mdb` is test-only `pub(crate)` (`#[cfg(test)]`), so the crate's
  test modules that call it keep compiling and no public caller exists to
  migrate. ADR-0177 item 1 and its breaking-change consequence are
  superseded by this ADR; ADR-0177 gains a Later references note naming
  this ADR.
- The public PackStream surface is `decode`, `encode`, `try_struct_msg`,
  `chunk_message`, and `Pack` — no signature change ships, so the
  `CHANGELOG.md` `[Unreleased]` entry advertising a public `Pack::to_mdb`
  signature change is removed, and a client implementing embedded Relate
  has nothing to track.
- The parameter-conversion contract (ADR-0177 items 2-6) is untouched: the
  nine tags decode through the RUN entry point and `to_mdb_at`, failure
  messages and paths are unchanged, and the pinned tests stay green.
- No client-facing document change: the conversion is crate-internal, so
  `docs/api/bolt/spec.md`'s wire-surface and failure-message contract is
  unchanged.
