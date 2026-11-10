#!/usr/bin/env bash
# The Docker-baseline stage setup the Sonic loadtest-baselines and
# query-latency stages share (LABS-622): the host Docker CLI and the
# pinned baseline environment. The sonic-rust image already contains the
# CLI and buildx; install them only when this runs where they are absent.
# This script itself reads no CI env. The stages' later make targets
# consume the forwarded GITHUB_WORKSPACE / BASELINE_RUNNER /
# GITHUB_ACTIONS — the per-stage comments in .sonic/ci.yml are the single
# home of that forwarding contract.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ ! -x /usr/local/bin/docker ] || [ ! -x /usr/local/lib/docker/cli-plugins/docker-buildx ]; then
  bash "${here}/install-docker-cli.sh"
fi
bash "${here}/baseline-env.sh" pull
