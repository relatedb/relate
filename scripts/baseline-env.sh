#!/usr/bin/env bash
# Compile (unconstrained) and run (2 vCPU / 2 GB) in-tree loadtest binaries
# for the ADR-0070 baseline environment; `pull` fetches its two images first.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# `docker` here is the host daemon. Under gosonic the script's own view of
# the repo is /workspace, and binding that path makes the daemon create an
# empty host directory, so the nested cargo build has no sources. GITHUB_WORKSPACE
# is the host checkout path when this runs in GitHub Actions.
DOCKER_ROOT="${ROOT}"
case "${GITHUB_WORKSPACE:-}" in
  /*) DOCKER_ROOT="${GITHUB_WORKSPACE}" ;;
esac
usage="usage: baseline-env.sh pull | baseline-env.sh build|run <bin> [args...]"
MODE="${1:?$usage}"
shift
if [ "${MODE}" != pull ]; then
  BIN="${1:?$usage}"
  shift
fi

BASELINE_CPUS="${BASELINE_CPUS:-2}"
# Every suite, including memory-shape at its 32 MB default, stays in 2 GB.
BASELINE_MEMORY="${BASELINE_MEMORY:-2g}"
# CFS bandwidth on a 10 ms period instead of `--cpus=N` (which is the same
# quota on Docker's default 100 ms period). Same two CPUs of bandwidth, but a
# spent budget freezes the run for <= 10 ms instead of <= 100 ms (ADR-0070).
BASELINE_CPU_PERIOD_US="${BASELINE_CPU_PERIOD_US:-10000}"
BASELINE_CPU_QUOTA_US="${BASELINE_CPU_QUOTA_US:-$((BASELINE_CPU_PERIOD_US * BASELINE_CPUS))}"
# Digest-pinned (2026-11-05, LABS-1039): a mutable tag re-resolves on every
# pull, and Debian re-pushed bookworm-slim between a baseline freeze and a
# PR compare the same morning - the ADR-0071 environment walk hard-fails on
# the drifted run_image digest, red-ing the query-latency lane on every PR.
# Pin both images so freeze and compare resolve identical bytes forever; a
# deliberate re-pin is an env override plus a re-freeze.
BASELINE_BUILD_IMAGE="${BASELINE_BUILD_IMAGE:-rust:1.88-bookworm@sha256:af306cfa71d987911a781c37b59d7d67d934f49684058f96cf72079c3626bfe0}"
BASELINE_RUN_IMAGE="${BASELINE_RUN_IMAGE:-debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc}"
VOLUME_NAME="${RELATE_BASELINE_VOLUME:-relate-baseline-work}"
# Word-split like Make's CARGO_PROFILE (default `--release`).
# shellcheck disable=SC2206
CARGO_PROFILE_ARGS=( ${CARGO_PROFILE:---release} )

if ! command -v docker >/dev/null 2>&1; then
  echo "baseline env: docker is required (ADR-0070). Install Docker or set BASELINE_ENV=host for a non-gate debug run." >&2
  exit 1
fi

# Docker Hub times out now and then, so each pull is tried three times, 15
# then 30 seconds apart, before failing.
if [ "${MODE}" = pull ]; then
  for image in "${BASELINE_BUILD_IMAGE}" "${BASELINE_RUN_IMAGE}"; do
    for attempt in 1 2 3; do
      docker pull "${image}" && break
      if [ "${attempt}" = 3 ]; then exit 1; fi
      sleep $((attempt * 15))
    done
  done
  exit 0
fi

uid="$(id -u)"
gid="$(id -g)"

# `docker run` attaches to the container's streams and can drop the tail of a
# fast-exiting container. Start detached and replay the log driver instead, so
# the loadtest summary always reaches the terminal.
docker_stream() {
  local cid status
  cid="$(docker create "$@")"
  docker start "${cid}" >/dev/null
  docker logs -f "${cid}"
  status="$(docker wait "${cid}")"
  docker rm -f "${cid}" >/dev/null
  return "${status}"
}

can_bind_mount() {
  docker run --rm -v "$1:/mnt-test:ro" "${BASELINE_RUN_IMAGE}" true >/dev/null 2>&1
}

# Prefer a repo bind-mount (CI, native Linux Docker, Docker Desktop with the
# project in File Sharing). Otherwise stream sources into a named volume —
# Docker Desktop file-sharing does not include /workspaces on some agents.
MOUNT_ARGS=()
USER_ARGS=()
WORK_IS_VOLUME=0

if can_bind_mount "${DOCKER_ROOT}"; then
  MOUNT_ARGS=(-v "${DOCKER_ROOT}:/work")
  USER_ARGS=(--user "${uid}:${gid}")
else
  docker volume create "${VOLUME_NAME}" >/dev/null
  # Wipe the staged sources before extracting: tar only adds and overwrites,
  # so a file deleted or renamed upstream would linger and break the build
  # (e.g. a stale `redo.rs` beside a newer `redo/mod.rs`). `target` survives
  # as the cargo cache. `node_modules` is rewritten by cookbook-wasm and
  # npm-smoke while this snapshot is packed, and tar exits when a file
  # changes under it.
  tar -C "${ROOT}" --exclude target --exclude .git --exclude node_modules -cf - . \
    | docker run --rm -i \
      -v "${VOLUME_NAME}:/work" \
      -w /work \
      "${BASELINE_RUN_IMAGE}" \
      sh -c 'find /work -mindepth 1 -maxdepth 1 ! -name target -exec rm -rf {} + && tar -xf -'
  MOUNT_ARGS=(-v "${VOLUME_NAME}:/work")
  WORK_IS_VOLUME=1
fi

# The report path the wrapped binary was told to write (`--out <path>` or
# `--out=<path>`; the loadtest binaries and the query-latency suite share
# the flag), copied out of the volume after the run.
report_path() {
  local prev="" arg
  for arg in "$@"; do
    case "${arg}" in
      --out=*)
        printf '%s\n' "${arg#--out=}"
        return
        ;;
    esac
    if [ "${prev}" = "--out" ]; then
      printf '%s\n' "${arg}"
      return
    fi
    prev="${arg}"
  done
}

copy_report_out() {
  local rel="$1" cid
  if [ "${WORK_IS_VOLUME}" -eq 0 ] || [ -z "${rel}" ]; then
    return
  fi
  mkdir -p "${ROOT}/$(dirname "${rel}")"
  cid="$(docker create "${MOUNT_ARGS[@]}" -w /work "${BASELINE_RUN_IMAGE}" true)"
  docker cp "${cid}:/work/${rel}" "${ROOT}/${rel}"
  docker rm -f "${cid}" >/dev/null
}

case "$MODE" in
  build)
    # The in-container CARGO_HOME lives at the repo root, NOT under
    # target/: Swatinem/rust-cache's post-job cleanup walks target/ and
    # died with ENOENT opendir errors on the root-owned registry tree
    # (broken tests/target symlinks in registry sources), and the
    # registry would bloat the cache archive. (Comments live ABOVE the
    # call: a `#` line inside the \-continuation ends the command early.)
    docker_stream \
      "${USER_ARGS[@]}" \
      -e HOME=/tmp \
      -e CARGO_HOME=/work/.cargo-docker \
      "${MOUNT_ARGS[@]}" \
      -w /work \
      "${BASELINE_BUILD_IMAGE}" \
      cargo build "${CARGO_PROFILE_ARGS[@]}" -p "${PACKAGE:-relate-datasets}" --bin "${BIN}"
    ;;
  run)
    host_cpus="$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 0)"
    if [ "${host_cpus}" -gt 0 ] && [ "${host_cpus}" -lt "${BASELINE_CPUS}" ]; then
      echo "baseline env: host has ${host_cpus} CPUs, fewer than BASELINE_CPUS=${BASELINE_CPUS}; results are not comparable" >&2
      exit 1
    fi
    docker_stream \
      --cpu-period="${BASELINE_CPU_PERIOD_US}" \
      --cpu-quota="${BASELINE_CPU_QUOTA_US}" \
      --memory="${BASELINE_MEMORY}" \
      --memory-swap="${BASELINE_MEMORY}" \
      "${USER_ARGS[@]}" \
      --tmpfs /tmp:exec,rw,size=1g \
      "${MOUNT_ARGS[@]}" \
      -w /work \
      "${BASELINE_RUN_IMAGE}" \
      "./target/release/${BIN}" "$@"
    rel="$(report_path "$@")"
    copy_report_out "${rel}"
    if [ -n "${rel}" ] && [ -f "${ROOT}/${rel}" ]; then
      "${PYTHON:-python3}" "${ROOT}/scripts/baseline-fingerprint.py" stamp "${ROOT}/${rel}"
    fi
    ;;
  *)
    echo "$usage" >&2
    exit 1
    ;;
esac
