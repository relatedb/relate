# Shared build caches: one build per SHA, shared everywhere

Implementation notes for ADR-0251 §5, the same style of record as
[`ci-lanes.md`](ci-lanes.md) is for §2–§4 and
[`cadences.md`](cadences.md) is for §6. The audit behind the ADR: compile
dominates the local and CI bill (the corpus-profile tests run in ~2.5 s but
their compile is ~24 s per crate-family; the engine suite spends ~89 s
cold-compiling against ~118 s executing), and the same binaries were being
rebuilt per lane — the CLI e2e harness built its own copy, the doc-example
and smoke lanes built release copies, and the Docker lane rebuilt the world
in-container.

The contract that replaces that: **one build per SHA, staged once, consumed
everywhere it is valid.** Three mechanisms, one per environment.

## Local: `target/.prebuilt/<sha>/` + `RELATE_BIN`

`scripts/prebuilt-bin.sh` (also `make prebuilt`,
`make prebuilt PREBUILT_PROFILE=release`) stages the `relate` binary for
the current commit into `target/.prebuilt/<sha>[-dirty]/<profile>/` and
prints its path. A clean tree at the same SHA reuses the staged binary
verbatim; a dirty tree reruns cargo (incremental no-op when nothing changed)
and re-stages when the output is newer.

Consumers resolve in order and always fall back to the normal cargo target:

| Consumer | Resolution order |
| --- | --- |
| CLI e2e harness (`surfaces/relate-cli/tests/support/mod.rs::binary_path`) | `RELATE_BIN` if it names an existing file → `CARGO_BIN_EXE_relate` |
| `scripts/check-doc-examples.sh` | `RELATE_BIN` → `target/.prebuilt/<sha>/<profile>/relate` (current commit only, clean or `-dirty`) → `target/release` → `target/debug` |
| `scripts/npm-smoke-test.sh` | `RELATE_LIB` if it names an existing file → `target/{debug,release}` FFI library search (pre-existing behavior) |
| `make docker` | `DOCKER_PREBUILT_DIR` set → copy staged binaries into the image; unset → in-container chef build (below) |

Typical loop — build once, run everything against that build:

```bash
export RELATE_BIN="$(scripts/prebuilt-bin.sh)"
cargo test -p relate-cli            # e2e harness spawns $RELATE_BIN
bash scripts/check-doc-examples.sh      # same binary, no release rebuild
```

Caveat: `cargo test -p relate-cli` still compiles the binary target as
part of the test build (cargo builds a package's bins when compiling its
integration tests, for `CARGO_BIN_EXE_*`). What `RELATE_BIN` removes is
the *second* build — the release rebuild in the doc-example lane, the
rebuilt binary in each smoke lane, and the re-staged binary across repeated
runs. The prebuilt probe never picks a stale SHA: only the current commit's
directory (clean or `-dirty`) matches. The same test build stages the
`extension-fixture` bin (`CARGO_BIN_EXE_extension-fixture`, the compiled
half of the extension e2e's language pair, ADR-0430) — a test fixture the
fixtures e2e suite spawns beside the prebuilt CLI, with no prebuilt
override of its own. The first-party importer children
(`extensions/relate-fs-to-graph`, `extensions/relate-git-to-graph`,
ADR-0431 §11) are other packages' bins, so their e2e
(`tests/e2e_fs_to_graph.rs`, `tests/e2e_git_to_graph.rs`) builds them
itself: one `cargo build -p <crate>` per test process, its executable
read from the build's JSON messages — a warm run is a cached no-op in
the same target dir, the one artifact the suite can name precisely.

## Docker lanes: prebuilt COPY instead of the chef build

The `relate-bolt` image (`Dockerfile`, ADR-0128/ADR-0249) has two binary
sources selected by `--build-arg BINARY_SOURCE=…`:

- `build` (default) — the in-container cargo-chef + sccache musl build. This
  stays the from-scratch guarantee and is what CI runs; the scheduled
  full-build verification of the image builds with this path.
- `prebuilt` — copies `relate` and `entrypoint` from
  `target/docker-prebuilt/` (re-included by `.dockerignore` for exactly this
  purpose; the image carries those two only since ADR-0368, and
  `scripts/prebuilt-bin.sh --bolt` still stages the entrypoint).
  BuildKit builds only the stages in the selected chain, so no
  Rust toolchain image is ever pulled; the CA bundle and `/data` skeleton
  come from a small `alpine` `certs` stage shared by both paths.

`make docker DOCKER_PREBUILT_DIR=<dir>` copies the two binaries from
`<dir>` into `target/docker-prebuilt/` and builds with the `prebuilt`
source. The binaries must be **static Linux executables** for the target
architecture — the runtime stage is `FROM scratch`, no loader, no libc.
Two ways to get them:

```bash
# On Linux with the musl target installed:
cargo build --release --target x86_64-unknown-linux-musl \
  -p relate-cli -p relate-bolt
make docker DOCKER_PREBUILT_DIR=target/x86_64-unknown-linux-musl/release

# Or reuse the binaries from a previously built image:
cid=$(docker create relate-bolt:latest)
mkdir -p target/.prebuilt/image
docker cp "$cid:/bin/relate" "$cid:/bin/entrypoint" \
  target/.prebuilt/image/
docker rm "$cid"
make docker DOCKER_PREBUILT_DIR=target/.prebuilt/image
```

(Cross-compiling musl from a Debian/Ubuntu host trips ring's cc-rs — see the
comment at the top of `Dockerfile`; the in-container build or the image
extraction above are the supported routes. ADR-0070 baseline loadtests are
untouched: they keep their pinned compile-and-run containers.)

## CI: rust-cache shared keys

The Ubuntu jobs use `Swatinem/rust-cache@v2` with per-profile `shared-key`s
so the engine+CLI compile is paid once per `Cargo.lock` hash per profile,
not once per job:

| Key | Jobs | Profile |
| --- | --- | --- |
| `dev` | `test` (3 shards), `feature-bundles`, `python`, `relate-bolt`, `relate-clients`, `npm-smoke`, `loadtest-smoke` | dev (`CARGO_PROFILE_DEV_DEBUG=line-tables-only` in every member so the restored cache is fingerprint-compatible — the trim `test` already needed for disk) |
| `release` | `stack-release`, `doc-examples`, `notebook` | release |
| `corpus` | `corpus` (merged TCK + CIP-eval, ADR-0252) | corpus (pre-existing, ADR-0212) |
| `os-filesystem`, `os-surface` | the Windows lanes | pre-existing per-lane keys |

Unshared on purpose: `lint` (clippy's check profile is a different artifact
set), the macOS/Windows OS lanes (platform-specific), `wasm` (wasm32 target)
and the docker-loadtest lanes (their build happens in-container). The
corpus family's within-run sharing is solved by merging the four jobs into
one `corpus` job (ADR-0252, [`corpus-build.md`](corpus-build.md)): the
cross-job rust-cache restore never paid off within a run because a fresh
checkout's source mtimes invalidate the restored target dir, so the three
gates now run as steps of the job that compiled them.

The fresh-checkout mtime invalidation is handled for the shared-key jobs by
**sccache** (`scripts/install-sccache.sh`, GHA backend, `RUSTC_WRAPPER=sccache`)
set up by the composite preamble before the cache step — see
[ADR-0331](../adr/0331-sccache-over-mtime-normalization.md) for the mechanism,
the correctness argument, and why it replaces the earlier (unsound)
mtime-normalization scheme. sccache keys each compilation unit on source
**content**, so a recompile on a fresh checkout is a cache hit regardless of
mtimes, and it can never serve an artifact built from different content.
Because sccache's GHA backend fails the whole build when the Actions cache
service is down, the preamble probes the backend with a server start first
and exports `RUSTC_WRAPPER` only on success — on an outage the lanes compile
uncached instead of going red (ADR-0331).

Trade-off to watch: a shared key means last-saver-wins per `Cargo.lock`
hash, so jobs with divergent feature sets partially recompile after a save
from a different member — still strictly better than the per-job cold
starts they replace. If the `dev` entry churns against the repo's 10 GB
cache pool, split the family rather than reverting to per-job keys.

### The `prebuild` handoff — removed (LABS-310 round 3)

The `dev` and `release` families briefly had warmer jobs (`prebuild`,
`prebuild-release`): a dedicated job compiled the workspace ahead of the
lanes so the shared key was warm before any lane started. **Removed.** The
warmer's `cargo test --workspace --no-run` produces feature-unified cargo
fingerprints — one build graph for the whole workspace — while every lane
compiles per-crate (`cargo nextest run -p X …`, `cargo build -p Y`),
whose fingerprints differ. Measured on the merged main: the warmer
recompiled 5m34s from scratch every run (its own restore never hit its
own artifacts' fingerprints either once `Cargo.lock` churned), and it sat
on the critical path as a pure gate — the lanes warmed themselves from
the shared key regardless (the Test lane restored with zero `Compiling`
lines). A gate that never warms and only gates is pure cost.

What replaced it: **the lanes self-warm in parallel.** Each shared-key
lane's rust-cache step carries an `id` and
`save-if: ${{ steps.<id>.outputs.cache-hit != 'true' }}` — a lane saves
the shared key ONLY when it restored cold (a miss). On a fresh
`Cargo.lock` generation the dev-key lanes race; the first to finish writes
the key, the rest no-op (a save on a hit was always a no-op; the `save-if`
makes the cold-key race single-writer instead of last-saver-wins with N
multi-GB uploads). The first cold run of a generation still pays one
compile per lane shape — the price the warmer was supposed to remove but,
per the fingerprint mismatch above, never did.

Lesson, recorded: **rust-cache warms only what the consumer's exact cargo
invocation would build.** A workspace-unified warmer and per-crate
consumers share a key but not a fingerprint graph. If a warmer is ever
reintroduced, it must run the lanes' exact command line (or the lanes must
switch to the warmer's unified invocation) — and its own warm-run cost
must be re-measured against the critical path it gates.

Fingerprint validity across the lanes rests on the same two facts as
before: content-addressed compiles (sccache keys each unit on source content,
so a fresh checkout's newer mtimes are irrelevant — ADR-0331)
and byte-identical env (workflow-level
`CARGO_INCREMENTAL`/`CARGO_PROFILE_DEV_DEBUG`, identical mold `RUSTFLAGS`
before the cache step).

Out of the shared-key families on purpose:

- `corpus` — it does its own merged corpus-profile compile under the
  `corpus` key (ADR-0252): a different profile, a different key, and a
  single-job shape that already pays its compile exactly once.
- `failfast`, `lint`, `link-check` — fast paths stay
  fast; `lint` compiles check artifacts, not the dev artifact set.
- The OS lanes and `wasm` — platform/target-specific caches.
- `loadtest-baselines` — Docker/cadence shapes.

### CI package managers: uv for Python, npm stays for Node

The `python` and `notebook` lanes install with `uv`
(`astral-sh/setup-uv@v6`, `enable-cache: true`, `uv pip install --system …`
into the setup-python interpreter, so maturin keeps linking against the
same Python the tests run). uv's resolver + wheel cache removes the
multi-minute pip installs; the lanes' test surface (pyo3/DB-API pytest
suite, maturin wheel build + install + import smoke, notebook execution)
is unchanged.

For Node the answer to "is yarn or whatever is fastest" is: pnpm (or bun)
is fastest, and we deliberately do NOT switch. Three reasons:

- `npm-smoke` must stay npm, full stop — its contract is that the
  published `@relate/relate*` tarballs install with the package
  manager users actually get (`npm pack` / `npm install`).
- The other Node usage (`clients/javascript` in `relate-clients`) only
  needs deps installed and would work under pnpm — but it ships a
  committed `package-lock.json`. pnpm needs a regenerated
  `pnpm-lock.yaml`, and the lockfile is a dev-workflow fact: local
  contributors run npm there today, and the JS client's published
  artifact is npm-shaped.
  Switching CI alone would fork the lock truth between CI and local dev.
- The absolute win is small: those installs are tens of seconds, not the
  compile-dominated minutes this document exists to kill.

Revisit only as a repo-wide lockfile migration decision (all Node dirs at
once, dev docs included), not as a CI-local tweak.

### rust-cache key construction (verified against the v2 source)

The cache key is

```
<prefix-key>[v0-rust]-<shared-key|…>-<os.type>-<os.arch>-<env-hash8>-<lock-hash8>
```

read from `src/config.ts` of the action:

- **`shared-key` replaces the job segment.** The job id
  (`add-job-id-key`, default `true` — which is why the run log's expanded
  `with:` block shows it even when unset) is appended ONLY in the
  `else` branch that runs when `shared-key` is empty. A job with
  `shared-key: dev` and `add-job-id-key: true` still keys on
  `v0-rust-dev-…`: the flag is a no-op there, and cross-job sharing is not
  defeated by it.
- **The env-hash segment hashes `rustc -vV` plus every environment
  variable starting with `CARGO`, `CC`, `CFLAGS`, `CXX`, `CMAKE` or
  `RUST`** (plus the `env-vars` input), sorted. This is the fragmentation
  that actually bit: the `test` job alone set `CARGO_INCREMENTAL=0` via a
  `GITHUB_ENV` step before rust-cache ran, so its env hash
  (`b2ab3321` in run 38706915770) differed from every other `dev` lane
  (`b74045d1`) — the lane meant to anchor the shared cache had a private
  one and compiled cold on the new runner class. ADR-0255 moves
  `CARGO_INCREMENTAL`/`CARGO_PROFILE_DEV_DEBUG` to workflow-level env and
  sets the mold `RUSTFLAGS` (also env-hashed) identically on every Linux
  lane BEFORE the rust-cache step, so all members of a shared-key family
  present byte-identical env.
- **The lock-hash segment** hashes the parsed workspace member manifests
  (path deps scrubbed) plus the parsed `Cargo.lock` package set, plus
  `.cargo/config.toml` / `rust-toolchain.toml` contents — content only, no
  mtimes, so the sccache content cache is unaffected by checkout timing.
- A lane that restores with `full match: false` prints the STALE entry's
  full key (`Restored from cache key "…-<old lock hash>"`); that trailing
  hash belongs to the previous generation, not the current run.

Also recorded here: the action's post-job cleanup walks the cached
`target/` tree, which is why the baseline docker builds must not keep
their in-container `CARGO_HOME` under `target/` (root-owned registry,
dangling `tests/target` symlinks → `ENOENT opendir` annotations, fixed by
moving it to `/.cargo-docker` in `scripts/baseline-env.sh`).

### Release workflow lanes

The release workflow's compile lanes run through the same mechanisms.
They are compile-dominated — measured on run 42703213258: the
pre-split `version` job that built the XCFramework 18.5 min (the head
gate every other release job waited on), CLI windows 15.9 min, wheels
windows/darwin-intel 14.6 min each — and before this every leg
cold-compiled the full dependency tree per release.

- The `xcframework` matrix legs (one Apple target each), the
  `swift-stamp` job (all five targets, for its rebase-rebuild retry
  path), and the `cli`/`ffi` jobs use the `setup-rust-ci` preamble
  with sccache on, **including the macOS and Windows legs**. This
  deviates from the CI OS lanes' `sccache: "false"`: those lanes are
  test-shaped and Linux-first, while the release poles are exactly the
  macOS/Windows legs, and a rust-cache restore alone cannot survive a
  fresh checkout's mtimes (ADR-0331's finding) — without sccache those
  legs stay cold every release. The `version` job itself compiles
  nothing since the ADR-0572 split (bump and push only, ubuntu).
- The wheel legs pass maturin-action's own `sccache: true` instead of
  the host preamble: the linux legs build inside manylinux containers a
  host-side `RUSTC_WRAPPER` never reaches, and the action wires the GHA
  backend through in both shapes.
- The wasm package job stays uncached, mirroring the CI `wasm` lane; at
  ~2 min it is not a pole.
- First release after the change pays the cache fill; the wins land from
  the second release onward (each release's `Cargo.lock` version bump
  rotates the rust-cache lock-hash — the stale-generation restore plus
  sccache is the expected warm shape, same as the shared-key families).

Pool note: each `xcframework` leg's plain-key rust-cache entry carries
one triple's dependency artifacts (small entries); the `swift-stamp`
entry carries five triples only when its retry path actually rebuilt.
If the 10 GB pool churns, split or trim the stamp entry first (same
guidance as the shared-key families above).
