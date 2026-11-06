#!/usr/bin/env bash
# The wasm toolchain setup for the Sonic npm-smoke stage (LABS-622):
# wasm-bindgen (it writes $CARGO_HOME/bin, which is not
# on the image PATH) and binaryen, both onto PATH.
#
# Source it — the PATH exports must reach the caller's shell (the
# sourcing stages declare `shell: bash`), and sourcing also puts that
# shell under set -euo pipefail:
#   source scripts/install-wasm-toolchain.sh
set -euo pipefail

: "${CARGO_HOME:?source install-wasm-toolchain.sh with CARGO_HOME set}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

export PATH="${CARGO_HOME}/bin:$PATH"
bash "${here}/install-wasm-bindgen.sh"
bindir="$(bash "${here}/install-binaryen.sh" /tmp/binaryen)"
export PATH="${bindir}:$PATH"
