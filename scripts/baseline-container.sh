#!/usr/bin/env bash
# The ADR-0070 container ceremony every image-built baseline lane shares:
# build the lane's binaries with Dockerfile.baseline, create the cgroup-capped
# container with the lane's bind mounts, follow the detached run's log, copy
# its report out and stamp the ADR-0071 environment class on it. The lane
# scripts (memory-shape-docker.sh, memory-controls-docker.sh)
# own only their names, their own flags and the
# binaries they measure.
#
# usage: baseline-container.sh --label <name> --bins "<bin>..." --cache-id <id>
#          --entrypoint <bin> --image <tag> --container <name> --out <path>
#          [--tmpfs <size>] [--path <flag>=<value>]... [--] <binary args...>
#
# --out is the host report path; the container writes /report/<its basename>.
# Every --path names one of the lane's path flags: a caller copy of that flag
# (either `--flag <value>` or `--flag=<value>`) is dropped from the binary
# args, the value is resolved against the repo root and bind-mounted, and the
# flag is re-appended with that absolute path. A bind mount keeps the path off
# the /tmp tmpfs, whose size counts against the cgroup memory limit.
#
# Env: BASELINE_CPUS (2), BASELINE_MEMORY (the container memory cap; no lane
# overrides the 2g default today — re-add a cap knob with the first lane
# that needs one),
# BASELINE_CPU_PERIOD_US (10000), BASELINE_CPU_QUOTA_US (period x cpus),
# BASELINE_BUILD_IMAGE / BASELINE_RUN_IMAGE (the Dockerfile's defaults),
# PYTHON (python3).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
usage="usage: baseline-container.sh --label <name> --bins \"<bin>...\" --cache-id <id> --entrypoint <bin> --image <tag> --container <name> --out <path> [--tmpfs <size>] [--path <flag>=<value>]... [--] <binary args...>"

label=""
bins=""
cache_id=""
entrypoint=""
image=""
container=""
host_out=""
memory="2g"
tmpfs="1g"
paths=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    --label) label="${2:?$usage}"; shift 2 ;;
    --bins) bins="${2:?$usage}"; shift 2 ;;
    --cache-id) cache_id="${2:?$usage}"; shift 2 ;;
    --entrypoint) entrypoint="${2:?$usage}"; shift 2 ;;
    --image) image="${2:?$usage}"; shift 2 ;;
    --container) container="${2:?$usage}"; shift 2 ;;
    --out) host_out="${2:?$usage}"; shift 2 ;;
    --tmpfs) tmpfs="${2:?$usage}"; shift 2 ;;
    --path) paths+=("${2:?$usage}"); shift 2 ;;
    --) shift; break ;;
    *)
      echo "baseline-container: unknown argument: $1" >&2
      echo "${usage}" >&2
      exit 1
      ;;
  esac
done
if [ -z "${label}" ] || [ -z "${bins}" ] || [ -z "${cache_id}" ] \
    || [ -z "${entrypoint}" ] || [ -z "${image}" ] \
    || [ -z "${container}" ] || [ -z "${host_out}" ]; then
  echo "${usage}" >&2
  exit 1
fi

abs_path() {
  case "$1" in
    /*) printf '%s\n' "$1" ;;
    *) printf '%s\n' "${ROOT}/$1" ;;
  esac
}

host_out="$(abs_path "${host_out}")"
report_in_container="/report/$(basename "${host_out}")"

# Every flag this script owns: --out always (the report is the ceremony's),
# plus the lane's declared path flags. Each is re-appended once below with the
# value this script resolved, so the container gets exactly one spelling.
owned=(--out)
mount_args=()
path_args=()
for path in ${paths[@]+"${paths[@]}"}; do
  case "${path}" in
    --*=*) flag="${path%%=*}"; value="${path#*=}" ;;
    *)
      echo "baseline-container: --path takes <flag>=<value>, got '${path}'" >&2
      exit 1
      ;;
  esac
  if [ -z "${value}" ]; then
    echo "baseline-container: --path ${flag} has no value" >&2
    exit 1
  fi
  owned+=("${flag}")
  value="$(abs_path "${value}")"
  mkdir -p "${value}"
  mount_args+=(-v "${value}:${value}")
  path_args+=("${flag}" "${value}")
done

# Drop every caller copy of an owned flag from the binary args: `--flag
# <value>` loses both tokens, `--flag=<value>` one. What remains is the lane's
# own arguments.
args=()
skip=0
for arg in "$@"; do
  if [ "${skip}" -eq 1 ]; then
    skip=0
    continue
  fi
  matched=0
  for flag in "${owned[@]}"; do
    if [ "${arg}" = "${flag}" ]; then
      skip=1
      matched=1
      break
    fi
    case "${arg}" in
      "${flag}"=*) matched=1; break ;;
    esac
  done
  if [ "${matched}" -eq 0 ]; then
    args+=("${arg}")
  fi
done

if ! command -v docker >/dev/null 2>&1; then
  echo "${label}: docker is required (ADR-0070). Set BASELINE_ENV=host for a non-gate debug run." >&2
  exit 1
fi

BASELINE_CPUS="${BASELINE_CPUS:-2}"
BASELINE_MEMORY="${BASELINE_MEMORY:-${memory}}"
BASELINE_CPU_PERIOD_US="${BASELINE_CPU_PERIOD_US:-10000}"
BASELINE_CPU_QUOTA_US="${BASELINE_CPU_QUOTA_US:-$((BASELINE_CPU_PERIOD_US * BASELINE_CPUS))}"
BASELINE_BUILD_IMAGE="${BASELINE_BUILD_IMAGE:-rust:1.88-bookworm}"
BASELINE_RUN_IMAGE="${BASELINE_RUN_IMAGE:-debian:bookworm-slim}"
export BASELINE_CPUS BASELINE_MEMORY BASELINE_CPU_PERIOD_US BASELINE_CPU_QUOTA_US
export BASELINE_BUILD_IMAGE BASELINE_RUN_IMAGE

# BuildKit resolves a missing base image into its build cache without leaving
# a tag behind, and the fingerprint stamp inspects both tags; ensure the pair
# first (the freeze workflow's own step, baseline-env.sh pull, with its
# retries).
if ! docker image inspect "${BASELINE_BUILD_IMAGE}" >/dev/null 2>&1 \
    || ! docker image inspect "${BASELINE_RUN_IMAGE}" >/dev/null 2>&1; then
  bash "${ROOT}/scripts/baseline-env.sh" pull
fi

echo "${label}  image ${image}  container ${BASELINE_MEMORY}"
# The image knobs ride the exported BASELINE_BUILD_IMAGE /
# BASELINE_RUN_IMAGE (ADR-0070's env contract; defaults in the Dockerfile);
# the lane knobs are the three parameters Dockerfile.baseline is built for.
DOCKER_BUILDKIT=1 docker build \
    --build-arg BASELINE_BUILD_IMAGE="$BASELINE_BUILD_IMAGE" \
    --build-arg BASELINE_RUN_IMAGE="$BASELINE_RUN_IMAGE" \
    --build-arg LANE_BINS="$bins" \
    --build-arg LANE_CACHE_ID="$cache_id" \
    --build-arg LANE_ENTRYPOINT="$entrypoint" \
  -f "${ROOT}/Dockerfile.baseline" \
  -t "${image}" \
  "${ROOT}"

host_cpus="$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 0)"
if [ "${host_cpus}" -gt 0 ] && [ "${host_cpus}" -lt "${BASELINE_CPUS}" ]; then
  echo "${label}: host has ${host_cpus} CPUs, fewer than BASELINE_CPUS=${BASELINE_CPUS}; results are not comparable" >&2
  exit 1
fi

# `docker run` attaches to the container's streams and drops the tail when
# the process exits. Start detached and follow the log driver instead.
# EXIT always removes the container, including a failed `docker start`.
# INT/TERM only kill so the log follow can finish and the report can be copied.
cleanup() {
  docker rm -f "${container}" >/dev/null 2>&1 || true
}
stop_run() {
  docker kill "${container}" >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap stop_run INT TERM

docker rm -f "${container}" >/dev/null 2>&1 || true
docker create \
  --name "${container}" \
  --cpu-period="${BASELINE_CPU_PERIOD_US}" \
  --cpu-quota="${BASELINE_CPU_QUOTA_US}" \
  --memory="${BASELINE_MEMORY}" \
  --memory-swap="${BASELINE_MEMORY}" \
  --tmpfs "/tmp:exec,rw,size=${tmpfs}" \
  ${mount_args[@]+"${mount_args[@]}"} \
  "${image}" \
  ${args[@]+"${args[@]}"} \
  ${path_args[@]+"${path_args[@]}"} \
  --out "${report_in_container}" >/dev/null
docker start "${container}" >/dev/null
set +e
docker logs -f "${container}"
status="$(docker wait "${container}")"
set -e

mkdir -p "$(dirname "${host_out}")"
if docker cp "${container}:${report_in_container}" "${host_out}" >/dev/null 2>&1; then
  "${PYTHON:-python3}" "${ROOT}/scripts/baseline-fingerprint.py" stamp "${host_out}"
fi
docker rm -f "${container}" >/dev/null 2>&1 || true
exit "${status}"
