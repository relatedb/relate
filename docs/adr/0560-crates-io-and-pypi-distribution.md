# 0560. crates.io and PyPI distribution

Status: Proposed
Claim: 03840dd02e6982f4d54e2acd95fd19565dede688
Date: 2026-11-10
Deciders: the maintainers

## Context

The Rust facade (`relate::Database`) and the Python package (`relate-db`)
are consumable only as GitHub release assets; `cargo add relate` and
`pip install relate-db` do not work. npm distribution is ADR-0278; this
decision extends the same shape to the Rust and Python registries.

Name situation, checked at decision time:

- crates.io `relate` is already owned by the maintainers (the predecessor
  CLI project), so the facade keeps its existing package name.
- PyPI `relate` is taken by an unrelated project; the Python package has
  been named `relate-db` from the start (ADR-0054's one-C-ABI packaging).

Cargo requires every dependency of a published crate to exist on
crates.io, so the publishable set is not a choice: it is the facade's
transitive workspace closure — 17 crates (`relate` plus the 16 engine
crates it reaches). Everything else in the workspace (surfaces,
extensions, testing) is `publish = false`.

## Decision

Publish to both registries from a dedicated `publish-registries.yml`
workflow — called by `release.yml` once the GitHub release exists, and
dispatchable by hand with a tag and per-registry toggles so dry runs and
partial-failure recovery never require re-cutting a release. npm's jobs
(ADR-0278) move into the same workflow; artifacts always come from the
release's assets, so the chained run and a manual re-publish share one
code path. Everything is lockstep with the release tag and fail-closed
until its runbook setup is done:

- **crates.io** — the 17-crate closure publishes in topological
  dependency order. The first publish of each crate is a manual
  token publish (a crate must exist before trusted publishing can be
  configured on it); afterwards each crate gets a GitHub Actions trusted
  publisher for `publish-registries.yml` and the pipeline takes over with
  OIDC, no stored token. A short settle delay or retry rides between
  crates because the registry index lags a fresh publish.
- **PyPI** — the `relate-db` wheels and sdist the `python` job already
  builds publish via the official publish action with trusted publishing
  (`id-token: write`, no token). PyPI's pending-publisher mechanism
  configures trust before the project exists, so no manual first publish
  is needed; a TestPyPI dispatch input gives a dry-run target.

Workspace dependency entries for the workspace's own crates carry a
version pin (crates.io ignores the path half), swept by
`scripts/release-bump.sh` like every other version surface. The one
exception is the shared test-support crate: it stays path-only so the
dev-dependencies that reference it are stripped at package time instead
of pointing at a crate that never publishes.

## Alternatives considered

- **Rename the facade to `relate-db` on crates.io for symmetry with
  PyPI.** Rejected: the maintainers already own `relate`, and a rename
  abandons the name's existing ownership for no user benefit.
- **Publish only the facade crate.** Rejected: cargo resolves a
  published crate's workspace dependencies against the registry, so the
  whole closure must publish regardless.
- **Keep registries out of the pipeline; ship release assets only.**
  Rejected: `cargo add` / `pip install` are the idiomatic install paths
  for both ecosystems; release assets are a fallback, not a substitute.
- **Long-lived API tokens as CI secrets for the steady state.**
  Rejected: OIDC trusted publishing is available on both registries and
  matches ADR-0278's npm decision; a revocable secret is the bootstrap
  tool only.

## Consequences

- 17 crates and one PyPI project publish per release once the runbook
  setup lands; until then the jobs fail closed like ADR-0278's npm jobs.
- A new publishable crate means: description, `repository` inherited
  from the workspace, a version pin on its workspace dependency entry,
  and removal of its `publish = false` — the boundary check stays the
  facade's transitive closure.
- The first release after this decision needs the manual crates.io
  bootstrap (topological token publishes) before the trusted publishers
  can be configured; the runbook lives in `docs/PUBLISHING.md`.
