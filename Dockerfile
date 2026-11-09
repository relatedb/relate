# relate-bolt server image (ADR-0128), now the unified `relate
# serve` runtime (ADR-0249): one process serving Bolt (7687), the HTTP
# Query API v2 (7474), and MCP over streamable HTTP (7475) on one open
# store with one shared credential (ADR-0232).
#
# FROM scratch: the runtime is musl-static binaries only — no distro, no
# shell, no build toolchain. The seed-then-serve entrypoint logic is a tiny
# Rust binary (surfaces/relate-bolt/src/bin/entrypoint/), because
# scratch has no shell to run a script.
#
# Build:  make docker           # docker build -t relate-bolt:<version> .
# Run:    make docker-run       # -p 7687/7474/7475, data volume in ./target/docker-data
#         docker run relate-bolt --database /data/other.rl   # args pass to `relate serve`
#
# Config resolution, most to least canonical (see docker docs in
# docs/api/bolt/spec.md, ADR-0128 and ADR-0249):
# 1. Container args pass through to `relate serve` (CMD defaults:
#    --host 0.0.0.0 --database /data/graph --bolt 7687; the entrypoint
#    appends the unset ports — http 7474, mcp 7475 for a single run,
#    derived non-overlapping bases under INSTANCES=N — so a bare
#    `docker run -e INSTANCES=2` never collides with the CMD's own
#    pins). --host 0.0.0.0 is deliberate: a container must bind
#    0.0.0.0 to be reachable at all; serve is plaintext-only and warns
#    loudly on a non-loopback host (ADR-0248) — this is a dev/test image,
#    and auth (ADR-0232 §1) is still on by default. The ADR-0128 bolt
#    spellings still work as aliases: --uri ≡ --database,
#    --bind HOST:PORT ≡ --host HOST --bolt PORT, --allow-plaintext is
#    dropped with a note. serve itself has no TLS (LABS-381, ADR-0368 retired the
#    TLS-capable standalone server): front the container with a
#    TLS-terminating proxy for encrypted Bolt.
# 2. /config/seed.json (mounted; path override: RELATE_CONFIG):
#    {"force": false, "steps": [
#       {"query": "UNWIND range(1,100) AS i CREATE (:Person {id: i})"},
#       {"load": "value=https://…", "query": "UNWIND $value.items AS q MERGE …"},
#       {"pull": "/config/neo4j.env", "query": "MATCH (o)-[r]-() RETURN o, r"}]}
#    Steps run in order, once, when the database does not exist yet.
# 3. Env fallback: RELATE_QUERY, RELATE_LOAD + RELATE_LOAD_QUERY,
#    RELATE_PULL_FROM + RELATE_PULL_QUERY, RELATE_FORCE_SEED=1
#   INSTANCES=N           spawn N serve processes; every surface's port
#                         steps base+0..N-1 from its base. Unset --http/
#                         --mcp bases derive from the bolt base so the
#                         ranges never overlap: bolt B..B+N-1, http
#                         B+N..B+2N-1, mcp B+2N..B+3N-1 (N=2, defaults:
#                         bolt 7687-7688, http 7689-7690, mcp 7691-7692 —
#                         publish -p 7687-7692:7687-7692); per-instance
#                         URIs are suffixed (graph-1.rl…); PID 1 handles
#                         SIGINT/SIGTERM and kills every child (Ctrl-C /
#                         docker stop work); docker rm -f's SIGKILL is
#                         data-safe too (fsync'd WAL)
#   RELATE_AUTH           one credential for ALL THREE surfaces (ADR-0232
#                         §1): user:password, none, or unset = a generated
#                         token printed in the startup banner (docker logs
#                         — the one token there unlocks Bolt, HTTP, and MCP
#                         alike) and persisted next to the database
# The engine's native env vars (RELATE_LOGICAL_GRAPH_SIZE_LIMIT,
# RELATE_DURABILITY_MODE, RELATE_CHECKPOINT_INTERVAL_SECS,
# RELATE_LOAD_CSV_DIR) pass straight through.

# Where the runtime binaries come from (ADR-0251 §5): `build` compiles them
# in-container below (the from-scratch guarantee, CI default);
# `prebuilt` copies host-staged static binaries from target/docker-prebuilt/
# (`make docker DOCKER_PREBUILT_DIR=<dir>` stages them there and passes the
# build-arg). BuildKit builds only the stages in the selected chain, so the
# prebuilt path never pays for the Rust toolchain.
ARG BINARY_SOURCE=build

# --- build: musl-native Alpine, so the binaries are static by default ---------
# rust:1.88-alpine's host triple IS x86_64-unknown-linux-musl — no
# cross-compiler needed (the zig-cc / musl-gcc cross routes both fight
# ring's cc-rs: Debian's musl-gcc rejects cc-rs's -m64, and zig cc trips
# cc-rs's compiler-family detection). build-base provides the C toolchain
# ring's assembly glue needs.
# --- chef base: toolchain + the two pinned build tools -----------------------
FROM rust:1.88-alpine AS chef
RUN apk add --no-cache build-base ca-certificates curl python3
# sccache wraps rustc and cache-keys on content hash — cargo's own
# fingerprints are mtime-based and `COPY . .` refreshes every mtime, so
# without it every build recompiled the world even with a warm target
# cache (measured in CI: restored cache, full rebuild). Version + sha256
# pinned. The tarball is a GitHub release; github-download.sh uses the
# API when the BuildKit secret is present. python3 reads the asset list.
ARG GITHUB_ACTIONS=
ARG SCCACHE_VERSION=0.12.0
ARG SCCACHE_SHA256=b0e89ead6899224a4ba2b90e9073bf1ce036d95bab30f3dc33c1e1468bc4ad44
RUN --mount=type=bind,source=scripts/github-download.sh,target=/tmp/github-download.sh \
    --mount=type=secret,id=GITHUB_TOKEN,required=false \
    if [ -s /run/secrets/GITHUB_TOKEN ]; then export GITHUB_TOKEN="$(cat /run/secrets/GITHUB_TOKEN)"; fi \
    && export GITHUB_ACTIONS="${GITHUB_ACTIONS}" \
    && sh /tmp/github-download.sh \
        --repo mozilla/sccache \
        --tag "v${SCCACHE_VERSION}" \
        --asset "sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
        -o /tmp/sccache.tgz \
    && echo "${SCCACHE_SHA256}  /tmp/sccache.tgz" | sha256sum -c - \
    && tar -xzf /tmp/sccache.tgz --strip-components=1 -C /usr/local/bin "sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl/sccache" \
    && rm /tmp/sccache.tgz && sccache --version
RUN cargo install cargo-chef --locked --version 0.1.78
WORKDIR /src

# --- planner: the dependency recipe (manifest-shaped, source-independent) ---
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# --- build: cook deps (a real layer — cache-stable until manifests change),
#     then build the workspace on top ------------------------------------------------
FROM chef AS build
COPY --from=planner /src/recipe.json recipe.json
# Deps only, package-scoped so unrelated testing/* deps stay out.
# Type=local cache exports carry LAYERS but not cache mounts (measured in
# CI), so the deps must live in a layer, not a mount.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo chef cook --release --recipe-path recipe.json -p relate-cli -p relate-bolt
# The .dockerignore keeps this to Cargo.toml/lock, relate/*, surfaces/*,
# testing/* — everything cargo needs, no docs/fixtures/output.
COPY . .
# Cache mounts are build-speed only and local-dev-oriented (they persist in
# the local builder); nothing from a mount lands in the image — the runtime
# stage copies binaries out of /out.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/opt/sccache \
    RUSTC_WRAPPER=/usr/local/bin/sccache SCCACHE_DIR=/opt/sccache \
    cargo build --release -p relate-cli -p relate-bolt \
    && mkdir -p /out \
    && cp target/release/relate target/release/entrypoint /out/

# --- prebuilt: host-staged static binaries (ADR-0251 §5) -------------------
# `make docker DOCKER_PREBUILT_DIR=<dir>` copies relate and entrypoint
# into target/docker-prebuilt/ and builds with
# --build-arg BINARY_SOURCE=prebuilt; the chef/planner/build stages are not
# part of that chain, so no Rust toolchain is ever pulled. The binaries must
# be static Linux executables for the architecture being built (scratch has
# no loader and no libc).
FROM scratch AS prebuilt
COPY target/docker-prebuilt/ /out/

# --- certs: the CA bundle and the /data skeleton, decoupled from the chef
#     build so the prebuilt path doesn't pull a Rust image for two files ---
FROM alpine:3.21 AS certs
# The CA bundle is installed from the current repo index on every
# build: the eMudhra CA break (2026-10) lands as a ca-certificates
# package update, and `apk add --no-cache` pulls the current index --
# the only thing this stage's consumers copy out is the bundle itself,
# so no other package needs refreshing.
RUN apk add --no-cache ca-certificates \
    && mkdir -p /data-owner && touch /data-owner/.keep

# --- bins: select the binary source (build or prebuilt) --------------------
FROM ${BINARY_SOURCE} AS bins

# --- runtime: scratch, two static binaries + the CA bundle --------------
# `relate` is the one product binary (`relate serve`, ADR-0249) plus
# the seeding CLI — the standalone `relate-bolt` server is retired
# (LABS-381, ADR-0368); `entrypoint` is the PID-1 seed-then-serve supervisor.
FROM scratch
# /data must exist in the image owned by the runtime UID: an unmounted
# `docker run` gets an anonymous volume initialized from this directory,
# and a root-owned one would EACCES the USER 1000 process. (The .keep file
# defeats the empty-directory layer drop; a mounted volume overrides this.)
COPY --from=certs --chown=1000:1000 /data-owner /data
COPY --from=bins \
    /out/relate \
    /out/entrypoint \
    /bin/
# ureq/rustls load the system CA bundle; scratch has none, so carry it at
# the standard path.
COPY --from=certs /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
# Numeric UID/GID: scratch has no /etc/passwd. Mount the data volume
# writable by 1000 (docker run --user overrides as usual).
USER 1000:1000
VOLUME /data
# The three surface ports (ADR-0249): Bolt 7687, HTTP Query API v2 7474,
# MCP streamable-HTTP 7475 — one `relate serve` process, one
# credential, all on --host 0.0.0.0 (the container posture, loudly warned
# by serve itself per ADR-0248).
EXPOSE 7687 7474 7475
ENTRYPOINT ["/bin/entrypoint"]
# --http/--mcp stay OUT of the CMD: the entrypoint appends their defaults
# (7474/7475) for a single run and derives non-overlapping bases under
# INSTANCES=N — an explicit pin here would defeat that derivation.
CMD ["--host", "0.0.0.0", "--database", "/data/graph", "--bolt", "7687"]
