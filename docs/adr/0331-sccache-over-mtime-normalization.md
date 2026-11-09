# ADR-0331: sccache (content-addressed) replaces mtime normalization for CI compile caching

Status: Accepted
Claim: 99dd31ca0c021ce92c53718346112dc52d66e8f7
Later references: the preamble installs the pinned release with scripts/install-sccache.sh (GitHub API asset download). The GHA backend and the probe are unchanged.

## Context

To make the shared `rust-cache` reusable across fresh checkouts, the CI
preamble stamped source mtimes (`scripts/normalize-mtimes.sh`): files
unchanged versus the merge-base got a fixed ancient epoch (so cargo treats
them as cache-fresh), files changed in the branch got the current time.

That scheme is **unsound**. Cargo's fingerprint check is mtime-based, so a
file stamped "ancient" is trusted even when its *content* differs from what
the restored cache was built from. The merge-base-relative "unchanged" is not
the same predicate as "identical to the cached build's input": when the cache
was saved by an older `main`, a branch's file that is unchanged against its
own base can still differ from the cached content — and the stale artifact is
served. This happened on #317: `ir.rs` (unchanged vs the branch's base, but
newer on `main`) was stamped ancient, the cached pre-export `ir` rlib was
reused, and `acc.rs` failed to compile. A merge gate that builds the previous
commit's binaries and reports green is a correctness failure, not a slow CI.

## Decision

Remove the mtime normalization entirely and use **sccache**
(`mozilla-actions/sccache-action` with the GitHub Actions backend,
`SCCACHE_GHA_ENABLED=true`, `RUSTC_WRAPPER=sccache`) as the compile cache in
the shared CI preamble. sccache keys each compilation unit on the **source
content + compiler flags**, so it can never return an artifact built from
different content — the failure mode above is structurally impossible.
`rust-cache` remains for the target directory (link/leaf artifacts), but the
correctness no longer depends on it: on a fresh checkout cargo may decide to
relink, and the recompiles are served by sccache.

The `setup-rust-ci` composite input that gated the normalization
(`normalize-mtimes`) is renamed `sccache` and now gates the sccache setup.
Because sccache's GHA backend treats a cache-service error as fatal, the
preamble first probes the backend with a server start (startup performs the
cache-check read) and exports `RUSTC_WRAPPER` only when it answers; on a
GitHub Actions cache outage the lane compiles uncached (correct, slower,
with a `::warning::` in the log) instead of failing every Rust job.

## Alternatives considered

- **Keep mtime normalization, fix the edge cases** — rejected: the flaw is
  fundamental (mtime cannot express content equality against an unknown
  cache-build commit); patching the merge-base computation only moves which
  branch content gets a stale artifact.
- **Drop all compile caching (plain cargo, fresh-checkout rebuild)** —
  rejected: correct but abandons the LABS-310 speed goal entirely.
- **Per-lane rust-cache keys (no sharing)** — rejected: the feature-plan
  mismatch (LABS-321, cargo-hakari) is the real cross-lane problem; per-lane
  keys multiply cache entries without addressing it.
- **`cargo-hakari` workspace-hack alone** — not rejected, but orthogonal:
  it unifies feature resolution so lanes share one build shape (LABS-321);
  sccache is what makes the *content* cache safe regardless of shape.

## Consequences

- CI compile caching is content-addressed and cannot serve stale artifacts;
  the merge-gate integrity risk is gone.
- sccache's GHA cache pool (10 GB, shared) is added alongside `rust-cache`;
  the corpus family's single-job merge (ADR-0252) still avoids the
  duplicated compile.
- `scripts/normalize-mtimes.sh` and `docs/testing/mtime-normalization.md` are
  deleted. ADR-0251 §5's "shared build caches" consequences are unaffected in
  intent (one compile per lockfile) — the mechanism changed from mtime-stamp
  reuse to content-addressed reuse; this ADR is the later reference for it.
- Cold-cache runs (new `Cargo.lock`) still pay one full compile; warm runs are
  served by sccache hits.
