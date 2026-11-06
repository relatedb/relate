#!/usr/bin/env bash
# Shared build entry for the relate-wasm distribution
# (ADR-0084, size pipeline in ADR-0100).
#
# Modes:
#   release  cargo --profile release-wasm  →  wasm-bindgen  →  wasm-opt -Oz
#   dev      cargo --release               →  wasm-bindgen  →  wasm-opt -O2
#
# Release is what CI and shipped bundles use (~4.3 MB / ~1.0 MB brotli).
# Dev is what iterative work wants: skips the release-wasm profile
# (fat LTO + codegen-units=1) so the cargo step is ~3× faster, and uses
# `-O2` instead of `-Oz` so wasm-opt is a few seconds instead of ~30 s.
#
# Output goes into `surfaces/relate-wasm/pkg/` (same layout wasm-pack
# used), so downstream consumers (examples/wasm-playground/, docs snippets)
# do not need to move.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# toml_section_version (shared TOML reader) stamps pkg/package.json with the
# [workspace.package] version from the repo-root Cargo.toml;
# toml_lock_package_version reads the pinned wasm-bindgen crate version out
# of Cargo.lock. Do not paste TOML-reading awk programs into this script.
# shellcheck source=lib/toml.sh
source "$REPO/scripts/lib/toml.sh"

# Print the wasm-bindgen crate version from Cargo.lock, or die with a real
# error. Callers (CI / deploy workflows) use this to pin the
# wasm-bindgen-cli install (scripts/install-wasm-bindgen.sh, prebuilt-first
# per ADR-0255) to the same version the crate resolved to. The failure
# guard lives here, once — call sites just use the value.
wbg_version_from_lock() {
  toml_lock_package_version "$REPO/Cargo.lock" wasm-bindgen || {
    echo "could not find wasm-bindgen version in Cargo.lock" >&2
    exit 1
  }
}

MODE="release"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode) MODE="$2"; shift 2 ;;
    --mode=*) MODE="${1#*=}"; shift ;;
    --print-bindgen-version)
      wbg_version_from_lock
      exit 0
      ;;
    -h|--help)
      echo "usage: $0 [--mode dev|release] | $0 --print-bindgen-version" >&2
      exit 0
      ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

case "$MODE" in
  release)
    PROFILE="release-wasm"
    PROFILE_DIR="release-wasm"
    WASM_OPT_LEVEL="-Oz"
    ;;
  dev)
    PROFILE="release"
    PROFILE_DIR="release"
    WASM_OPT_LEVEL="-O2"
    ;;
  *)
    echo "invalid --mode '$MODE' (expected: dev | release)" >&2
    exit 2
    ;;
esac

CRATE_DIR="$REPO/surfaces/relate-wasm"
# BUILD_WASM_OUT_DIR lets a caller keep its bundle off the shared pkg/
# directory. When two jobs run this script against the same checkout, one
# `rm -rf` of that directory removes the other's package.json.
OUT_DIR="${BUILD_WASM_OUT_DIR:-$CRATE_DIR/pkg}"
OUT_NAME="relate"

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "$1 not found on PATH" >&2
    echo "$2" >&2
    exit 1
  fi
}

WBG_VERSION="$(wbg_version_from_lock)"

need wasm-bindgen "Install: cargo install wasm-bindgen-cli --version $WBG_VERSION --locked"
need wasm-opt "Install (macOS): brew install binaryen ; (Linux) bash scripts/install-binaryen.sh DIR, then add DIR/bin to PATH"

INSTALLED_WBG="$(wasm-bindgen --version | awk '{print $2}')"
if [[ "$INSTALLED_WBG" != "$WBG_VERSION" ]]; then
  echo "wasm-bindgen CLI version mismatch: installed $INSTALLED_WBG, need $WBG_VERSION" >&2
  echo "  cargo install wasm-bindgen-cli --version $WBG_VERSION --locked --force" >&2
  exit 1
fi

# binaryen < 110 points the export of a module's second table at the first.
# Dev builds keep the target_features section, so wasm-bindgen adds an
# externref table as that second table, and the bundle then fails at init
# with "WebAssembly.Table.grow(): failed to grow table by 4". Ubuntu 24.04's
# apt package is binaryen 108. See scripts/install-binaryen.sh.
MIN_BINARYEN=110
BINARYEN_VERSION="$(wasm-opt --version | awk '{print $3}')"
if [[ "$BINARYEN_VERSION" =~ ^[0-9]+$ ]]; then
  if ((BINARYEN_VERSION < MIN_BINARYEN)); then
    echo "wasm-opt is binaryen $BINARYEN_VERSION; need $MIN_BINARYEN or newer" >&2
    echo "  (macOS) brew upgrade binaryen ; (Linux) bash scripts/install-binaryen.sh DIR" >&2
    exit 1
  fi
else
  echo "warning: could not read the binaryen version from 'wasm-opt --version'; need $MIN_BINARYEN or newer" >&2
fi

echo "[build-wasm] mode=$MODE  cargo build (--profile $PROFILE)"
cargo build \
  --manifest-path "$CRATE_DIR/Cargo.toml" \
  --lib \
  --profile "$PROFILE" \
  --target wasm32-unknown-unknown

# Honor CARGO_TARGET_DIR (the Pages deploy shares one target dir between
# its main and release-tag builds).
RAW="${CARGO_TARGET_DIR:-$REPO/target}/wasm32-unknown-unknown/$PROFILE_DIR/relate_wasm.wasm"
if [[ ! -f "$RAW" ]]; then
  echo "expected artifact not found: $RAW" >&2
  exit 1
fi

echo "[build-wasm] wasm-bindgen --target web"
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"
wasm-bindgen "$RAW" \
  --target web \
  --out-dir "$OUT_DIR" \
  --out-name "$OUT_NAME"

BG="$OUT_DIR/${OUT_NAME}_bg.wasm"
echo "[build-wasm] wasm-opt $WASM_OPT_LEVEL"
wasm-opt "$WASM_OPT_LEVEL" \
  --enable-bulk-memory \
  --enable-nontrapping-float-to-int \
  --enable-sign-ext \
  "$BG" -o "$BG.opt"
mv "$BG.opt" "$BG"

# The trap classifier every host shares (isWasmTrap) and the OPFS helper
# (ADR-0145) ship with the bundle. wasm-bindgen also copies opfs.js into
# snippets/ because the glue imports it.
cp "$CRATE_DIR/js/trap.js" "$CRATE_DIR/js/trap.d.ts" \
   "$CRATE_DIR/js/opfs.js" "$CRATE_DIR/js/opfs.d.ts" \
   "$OUT_DIR/"

# Stamp the npm package manifest from the template with the workspace
# version and ship the package README, so pkg/ is publishable as
# @relate/relate-wasm without further edits.
PKG_VERSION="$(toml_section_version "$REPO/Cargo.toml" "[workspace.package]")"
if [[ -z "$PKG_VERSION" ]]; then
  echo "could not find [workspace.package] version in $REPO/Cargo.toml" >&2
  exit 1
fi
sed "s/__VERSION__/$PKG_VERSION/g" \
  "$CRATE_DIR/npm/package.json.template" > "$OUT_DIR/package.json"
cp "$CRATE_DIR/npm/README.md" "$OUT_DIR/README.md"
echo "[build-wasm] stamped $OUT_DIR/package.json (version $PKG_VERSION)"

SIZE=$(wc -c <"$BG" | tr -d ' ')
# Keep the awk program single-quoted: macOS's /bin/bash 3.2 brace-expands
# "{printf …, …}" inside a quoted "$(…)", leaving awk with no action, so it
# waits on stdin.
MB=$(awk -v bytes="$SIZE" 'BEGIN { printf "%.2f", bytes / 1024 / 1024 }')
printf '[build-wasm] done: %s (%s MB, mode=%s)\n' "$BG" "$MB" "$MODE"

# Mirror the playground-facing files into the SPA's src tree so its
# Vite build (which does not know about pkg/) picks up a fresh bundle. The
# copy lives here instead of in the playground's npm scripts so all
# callers of this script (make build-wasm, npm) stay in
# sync.
PLAYGROUND_WASM_DIR="$REPO/examples/wasm-playground/src/wasm"
if [[ -z "${BUILD_WASM_OUT_DIR:-}" && -d "$REPO/examples/wasm-playground" ]]; then
  mkdir -p "$PLAYGROUND_WASM_DIR"
  cp "$OUT_DIR/${OUT_NAME}.js" \
     "$OUT_DIR/${OUT_NAME}.d.ts" \
     "$OUT_DIR/${OUT_NAME}_bg.wasm" \
     "$OUT_DIR/${OUT_NAME}_bg.wasm.d.ts" \
     "$OUT_DIR/trap.js" \
     "$OUT_DIR/trap.d.ts" \
     "$OUT_DIR/opfs.js" \
     "$OUT_DIR/opfs.d.ts" \
     "$PLAYGROUND_WASM_DIR/"
  if [[ -d "$OUT_DIR/snippets" ]]; then
    rm -rf "$PLAYGROUND_WASM_DIR/snippets"
    cp -R "$OUT_DIR/snippets" "$PLAYGROUND_WASM_DIR/snippets"
  fi
  echo "[build-wasm] copied to $PLAYGROUND_WASM_DIR/"
fi
