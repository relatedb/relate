#!/usr/bin/env bash
# memory-controls in the ADR-0070 container limits (the shared ceremony is
# scripts/baseline-container.sh, the build recipe Dockerfile.baseline). The
# lane's own settings are the store path, which the ceremony bind-mounts off
# the /tmp tmpfs whose size counts against the cgroup memory limit, and that
# tmpfs size.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
path=()
if [[ -n "${MEMORY_CONTROLS_URI:-}" ]]; then
  path=(--path "--uri=${MEMORY_CONTROLS_URI}")
fi

exec bash "${ROOT}/scripts/baseline-container.sh" \
  --label memory-controls \
  --bins "relate memory-controls" \
  --cache-id memory-controls \
  --entrypoint memory-controls \
  --image "${MEMORY_CONTROLS_IMAGE:-relate-memory-controls:local}" \
  --container "${MEMORY_CONTROLS_CONTAINER:-relate-memory-controls}" \
  --out "${HOST_OUT:-target/memory-controls.json}" \
  --tmpfs "${MEMORY_CONTROLS_TMPFS:-1g}" \
  ${path[@]+"${path[@]}"} \
  -- "$@"
