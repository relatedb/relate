---
type: adr
area: surface
claim: 8ba53240ef255b80b9fcf617a17af91f42d8d38e
name: 0390-serve-memory-limit-flag
status: accepted
tags:
  - memory
  - serve
title: "`cypherlite serve` carries the served store's `--memory-limit`"
REFERENCES:
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
  - "[[adr/0232-network-surfaces-charter]]"
  - "[[adr/0289-graph-shape-memory-pins]]"
  - "[[adr/0368-one-product-binary-serve-absorbs-bins]]"
---

# ADR-0390: `cypherlite serve` carries the served store's `--memory-limit`

## Context

[ADR-0368](./0368-one-product-binary-serve-absorbs-bins.md) retired the standalone `cypherlite-bolt` server — the one product
binary is `cypherlite`, whose `serve` command hosts every network surface
in-process on one open store. The memory-shape loadtest ([ADR-0289](./0289-graph-shape-memory-pins.md)) measures
the serving process's RSS against logical graph bytes per shape: its bolt
cases spawn the server child under `--memory-limit <bytes>` so the fill
stops when the served store reports the cap. The retired server carried
that flag; `serve` did not, so every bolt case failed with the loadtest's
own missing-binary error — and because `make baseline-loadtests` runs
memory-shape in every freeze round, the `create-loadtest-baselines`
workflow aborted before writing any envelope (which blocked the LABS-422
re-freeze on main). A short-term PR that dropped memory-shape from the
freeze set (#412) was closed unmerged; the agreed route is the storage
flag, not the drop.

The query session already has the same flag: `cypherlite [QUERY]
--memory-limit SIZE` (value grammar `bytes or mb/mib/gb/gib`, resolved by
`flags::memory_limit_from_env` as `--memory-limit` >
`CYPHERLITE_MEMORY_LIMIT` > unlimited) and applies it to its own
`OpenOptions::memory_limit`. Two processes — the query session and the
serving process — can therefore cap the same store differently, or rather:
until now only one of them could cap it at all.

## Decision

`cypherlite serve` accepts `--memory-limit SIZE` and applies it to the
served store's open (`OpenOptions::memory_limit`), with the query session's
exact value grammar and the one canonical resolution — `--memory-limit` >
`CYPHERLITE_MEMORY_LIMIT` > unlimited — shared through the existing
`flags::memory_limit_from_env` helper, so serve and query cannot drift on
the same store. The flag is a storage flag, not a surface flag: it is not
feature-gated (it configures the open, not a listener), a bad value (flag
or env) is a loud usage/startup error with the query path's message shape,
and the memory-shape loadtest's bolt cases now spawn
`cypherlite serve --bolt PORT --auth none --memory-limit <bytes>` on a
picked free port — with the `cypherlite` binary shipped beside the loadtest
binary in the [ADR-0289](./0289-graph-shape-memory-pins.md) runtime image.

## Alternatives considered

- **Drop memory-shape from the freeze set (PR #412).** Rejected: it shrinks
  the baseline coverage to work around a missing flag, and the loadtest's
  bolt cases would keep failing in every local `make memory-shape-loadtest`
  run — the coverage loss is permanent, the flag is one parse arm and one
  open assignment.
- **Give serve a parallel `--max-bytes`/serve-only spelling.** Rejected: a
  second name for the same policy is a parallel vocabulary (the second
  spelling would drift from the query flag's grammar and env resolution);
  the query session's flag is the established surface for this cap.
- **Pass the limit only via `CYPHERLITE_MEMORY_LIMIT` in the loadtest's
  child environment.** Rejected: the loadtest pins the flag spelling it
  passes (the env is the *default*, not the only channel), and the spawned
  child's env is deliberately scrubbed of the parent's limit variables —
  the cap must be stated, not inherited.
- **Also add `--checkpoint-interval-secs` now.** Deferred: the
  processing-memory loadtest's bolt cases need it, but that is a second
  decision with its own value grammar (an interval, not a size) — it lands
  as its own ADR when that loadtest is restored, not smuggled into this
  one.

## Consequences

- The memory-shape loadtest's bolt cases run again against the one product
 binary; `make memory-shape-loadtest` ([ADR-0070](./0070-docker-baseline-loadtest-environment.md) docker env) completes, so
  `make baseline-loadtests` and the freeze workflow can include
  memory-shape again. The re-freeze itself (the `runner=linux-4-core-x86`
  envelope dispatch, which also resolves LABS-422's digest drift) runs
  after this lands on main — the envelopes are frozen from main, not from
  this branch.
- The runtime image builds and copies two binaries (`memory-shape-loadtest`
  and `cypherlite`); the loadtest finds the server as a sibling of its own
  executable, so no path flag is needed inside the container.
- `serve --help`, the serve flag table in `docs/api/cli/spec.md`, and the
  suggestion list (`SERVE_FLAGS`) all gain the flag in the same change.
- A bad `CYPHERLITE_MEMORY_LIMIT` under a serve run now fails exactly as it
  does under a query run (exit 2 with the shared message) instead of being
  ignored — the flag's existence is what makes the env visible to serve.
- [ADR-0368](./0368-one-product-binary-serve-absorbs-bins.md) gains a Later references note: the follow-up this ADR is.

References, [ADR-0368](./0368-one-product-binary-serve-absorbs-bins.md), [ADR-0289](./0289-graph-shape-memory-pins.md), [ADR-0070](./0070-docker-baseline-loadtest-environment.md), [ADR-0232](./0232-network-surfaces-charter.md),
[ADR-0069](./0069-cli-lifecycle-commands.md).
