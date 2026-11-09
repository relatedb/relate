#!/usr/bin/env bash
# Run gosonic the way .sonic/ci.yml expects.
#
# Stages that set docker_socket build images or talk to the host daemon.
# gosonic refuses that mount unless the socket is explicitly allowed.
#
# secrets: copies a host variable by name and fails when the name is
# unset. An empty value means "not provided": local runs leave the token,
# Actions flag, workspace, runner label, and compare switch empty, and
# the stages treat that the way they used to treat a missing value.
set -euo pipefail

export GOSONIC_ALLOW_DOCKER_SOCKET=1
export GITHUB_TOKEN="${GITHUB_TOKEN:-}"
export GITHUB_ACTIONS="${GITHUB_ACTIONS:-}"
export GITHUB_WORKSPACE="${GITHUB_WORKSPACE:-}"
export BASELINE_RUNNER="${BASELINE_RUNNER:-}"
export CI_COMPARE="${CI_COMPARE:-}"

exec gosonic "$@"
