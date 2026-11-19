---
type: adr
area: cli
date: 2026-09-29
deciders: CypherLite maintainers (agreed in conversation)
name: 0270-cli-flag-parse-fetch-split
status: accepted
tags:
  - commands
title: "CLI flags parse in every build: split `--fetch-params` parsing from its fetch"
REFERENCES:
  - "[[adr/0104-cargo-feature-bundles]]"
  - "[[adr/0105-optional-parquet-codec]]"
  - "[[adr/0267-cli-fetch-params-flag]]"
---

# 0270. CLI flags parse in every build: split `--fetch-params` parsing from its fetch

- **Renumbered**: from 0137 (shared-number cleanup)

## Context

[ADR-0267](./0267-cli-fetch-params-flag.md) shipped `--fetch-params` with the whole `fetch_params` module — including
the `NAME=SOURCE` splitting and source classification — behind the `fetch-params`
cargo feature. That made flag *recognition* itself feature-gated: in the `minimal`
bundle `--fetch-params` hits the generic unknown-flag usage error.

[ADR-0104](./0104-cargo-feature-bundles.md)'s invariant is "every flag parses in every build" (only a *value* can
require a compiled-in capability — `--snapshot-format parquet` parses everywhere
and fails closed at open, per [ADR-0105](./0105-optional-parquet-codec.md)), and compiled-out *commands* still
recognize and fail with a typed not-included error. [ADR-0267](./0267-cli-fetch-params-flag.md)'s shape was the
first flag to break that, and the [ADR-0104](./0104-cargo-feature-bundles.md) exception paragraph written for it
was self-contradictory ("the flag still parses … without the feature it hits
the unknown-flag usage error").

The parse half of `fetch_params` (`parse_fetch_assignment`, `FetchSource`) is
pure string/`PathBuf` code with no `ureq` dependency — only the fetch half
(`fetch_source`/`fetch_url`/`read_file`/`read_capped`) needs the feature.

## Decision

Split parse from fetch:

- `parse_fetch_assignment`/`FetchSource` move ungated into `params.rs` (alongside
  the other argv decode helpers); the `--fetch-params` match arm in `main.rs` is
 ungated. The flag parses in every build, restoring [ADR-0104](./0104-cargo-feature-bundles.md)'s invariant.
- Only the fetch module stays behind the `fetch-params` feature. `params::resolve`
  fails closed without it: a parsed `--fetch-params` value is a runtime error
  (exit 1) with `--fetch-params is not compiled into this build (enable the
  "fetch-params" feature)` — the `--snapshot-format parquet` fails-closed
 precedent ([ADR-0105](./0105-optional-parquet-codec.md)).
- `--help` still lists `--fetch-params` only when compiled in (the commands and
  codecs precedent); the e2e bundle suite pins both the help parity and the
  fails-closed message.

## Alternatives considered

- **Keep recognition gated ([ADR-0267](./0267-cli-fetch-params-flag.md) as shipped)** — rejected: a flag that exists
  in the product erroring as "unknown flag" in a smaller bundle gives users a
  generic message instead of the typed not-compiled guidance, and it set a
  divergent convention for the next gated flag.
- **Fail closed at parse time (usage error, exit 2)** — rejected: a *value*
  parse succeeded; the missing capability is a runtime condition, and parquet
  already established fails-closed-after-parse for compiled-out capability.
- **Ungate the fetch too (drop the feature)** — rejected: the bundle contract
 ([ADR-0104](./0104-cargo-feature-bundles.md)) exists precisely so `minimal` can exclude the `ureq` HTTP surface;
  the fetch must stay physically absent there.

## Consequences

- "Every flag parses in every build" holds again for every current flag; the
 [ADR-0104](./0104-cargo-feature-bundles.md) exception paragraph reverts to stating the invariant holds (fails
  closed without the feature).
- `minimal`-bundle users get the typed message telling them which feature to
  enable instead of "unknown flag".
- Three of the previous `#[cfg]` sites around the flag disappear; the parse
  tests run ungated in every bundle.
- This amends [ADR-0267](./0267-cli-fetch-params-flag.md) (see its Later references note); [ADR-0267](./0267-cli-fetch-params-flag.md)'s caps, trust
  model, and `full`/`agent` bundle membership are unchanged.
