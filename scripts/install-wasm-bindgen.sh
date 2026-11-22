#!/usr/bin/env bash
# Install wasm-bindgen-cli at the version the repository's Cargo.lock pins
# (the CLI must match the crate), unless that version is already on PATH.
#
# Prebuilt-first (ADR-0255): the upstream release ships static tarballs for
# the common hosts, so the default path is a seconds-long download instead
# of the ~2-minute `cargo install wasm-bindgen-cli --locked` compile the CI
# lanes used to pay per run. The tarball's published .sha256sum is verified
# before anything is installed. Hosts without a prebuilt (or a failed
# download/checksum) fall back to the from-source cargo install.
#
# Usage: bash scripts/install-wasm-bindgen.sh [REPO]
#   REPO  the checkout whose Cargo.lock decides (default: this script's)
set -euo pipefail
REPO="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
here="$(cd "$(dirname "$0")" && pwd)"
# The digest policy and the download-and-verify block are the repo's one
# canonical helpers (scripts/lib/common.sh). Sourced here, at the top
# level: a `.` inside `install_prebuilt` would fire that function's RETURN
# trap (bash runs it when a sourced script finishes too), deleting the
# temp dir before the download.
# shellcheck source=lib/common.sh
. "$here/lib/common.sh"
WBG=$(bash "$REPO/scripts/build-wasm.sh" --print-bindgen-version)
if [[ "$(wasm-bindgen --version 2>/dev/null | awk '{print $2}')" == "$WBG" ]]; then
  echo "wasm-bindgen $WBG"
  exit 0
fi

BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
mkdir -p "$BIN_DIR"

# The host's prebuilt target, or the refusal naming why there is none: the
# reason goes to stderr, so the caller's one message can stay a plain
# "compiling from source".
prebuilt_target() {
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64 | Linux-amd64) echo x86_64-unknown-linux-musl ;;
    Linux-aarch64 | Linux-arm64) echo aarch64-unknown-linux-musl ;;
    Darwin-arm64) echo aarch64-apple-darwin ;;
    Darwin-x86_64) echo x86_64-apple-darwin ;;
    *)
      echo "install-wasm-bindgen: no prebuilt for this host" >&2
      return 1
      ;;
  esac
}

install_prebuilt() {
  local target="$1" work asset
  work="$(mktemp -d)"
  trap 'rm -rf "$work"' RETURN
  asset="wasm-bindgen-${WBG}-${target}.tar.gz"
  # Release bytes go through the API when a token is set. The download
  # and the digest check are the repo's one canonical helper
  # (download_verified), which names its own cause on stderr; either
  # failure falls through to the source build below.
  download_verified "$here/github-download.sh" \
    rustwasm/wasm-bindgen "$WBG" "$asset" "$work/wbg.tar.gz" "${asset}.sha256sum" ||
    return 1
  tar --no-same-owner -xzf "$work/wbg.tar.gz" -C "$work" || {
    echo "install-wasm-bindgen: the prebuilt for $target did not unpack" >&2
    return 1
  }
  install -m755 "$work/wasm-bindgen-${WBG}-${target}/wasm-bindgen" "$BIN_DIR/wasm-bindgen"
  install -m755 "$work/wasm-bindgen-${WBG}-${target}/wasm-bindgen-test-runner" \
    "$BIN_DIR/wasm-bindgen-test-runner"
}

if target="$(prebuilt_target)" && install_prebuilt "$target"; then
  echo "wasm-bindgen $WBG (prebuilt $target)"
else
  echo "install-wasm-bindgen: compiling from source" >&2
  cargo install wasm-bindgen-cli --version "$WBG" --locked --force
  echo "wasm-bindgen $WBG"
fi
