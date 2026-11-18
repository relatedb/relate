#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C.UTF-8

# ============================================================
# install-opencode.sh
#
# Single source of truth for the pinned opencode install used
# by .github/workflows/release-notes.yml and
# .github/workflows/opencode-review-reusable.yml (the ADR-0258
# tarball-plus-sha256 pattern). Bump OPENCODE_VERSION and
# OPENCODE_SHA256 here together — both workflows call this
# script and carry no pins of their own.
#
# OPENCODE_SHA256 pins the artifact content: a swapped or
# corrupted tarball at the download endpoint fails
# `sha256sum -c` and aborts the install before anything
# executes on the runner.
#
# The download path segment has NO leading `v`.
#
# Usage: install-opencode.sh
# Env var (optional): OPENCODE_INSTALL_DIR — extraction target
#                     (default /usr/local/bin). sudo is used
#                     only when the target is not writable.
# ============================================================

OPENCODE_VERSION=2.3.0
OPENCODE_SHA256=fc0f81522aceeabac47693d88389efa2bc517ce74c2c38fd6967819fcc6e4d3b

INSTALL_DIR="${OPENCODE_INSTALL_DIR:-/usr/local/bin}"
TARBALL=/tmp/opencode.tgz

curl -fsSL -o "${TARBALL}" "https://opencode.ai/files/bin/${OPENCODE_VERSION}/opencode-linux-x64.tar.gz"
echo "${OPENCODE_SHA256}  ${TARBALL}" | sha256sum -c -

mkdir -p "${INSTALL_DIR}"
if [ -w "${INSTALL_DIR}" ]; then
  tar -xzf "${TARBALL}" -C "${INSTALL_DIR}"
else
  sudo tar -xzf "${TARBALL}" -C "${INSTALL_DIR}"
fi

"${INSTALL_DIR}/opencode" --version
