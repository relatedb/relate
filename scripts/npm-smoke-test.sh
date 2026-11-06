#!/usr/bin/env bash
# npm pack/install smoke tests: prove the packed npm artifacts actually
# install into a fresh project and answer a query.
#
# Three smokes:
#   embedded-fallback           @relate/relate tarball, native lib
#                               via RELATE_LIB (repo target/ build)
#   embedded-platform-package   same tarball plus a fabricated
#                               @relate/<platform>-<arch>
#                               tarball carrying the repo-built lib in lib/;
#                               no RELATE_LIB, so the optional-dependency
#                               loader in index.js must resolve it
#   wasm                        @relate/relate-wasm tarball,
#                               initialized in Node (no browser) from the
#                               packed relate_bg.wasm
#
# Prerequisites: node + npm on PATH, and a repo build of the FFI library
# (cargo build -p relate-ffi). The wasm bundle is built with
# scripts/build-wasm.sh --mode dev into a private directory (needs
# wasm-bindgen + wasm-opt, see scripts/install-wasm-bindgen.sh
# and scripts/install-binaryen.sh).
#
# Usage: bash scripts/npm-smoke-test.sh   (from anywhere; resolves the repo
# root from its own path)
#
# Opt-in environment (the release workflow's smoke gate uses these to
# exercise the actual packed release artifacts instead of re-packing from
# the repo; the default local/CI behavior is unchanged when they are unset):
#   RELATE_LIB           prebuilt native library to use instead of the
#                            repo target/ build (embedded-fallback smoke)
#   NPM_SMOKE_MAIN_TGZ       pre-packed @relate/relate tarball
#   NPM_SMOKE_PLATFORM_TGZ   pre-packed platform-package tarball for this
#                            host's platform (embedded-platform-package
#                            smoke; replaces the fabricated package)
#   NPM_SMOKE_WASM_TGZ       pre-packed @relate/relate-wasm tarball
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

log() { echo "[npm-smoke] $*"; }
fail() {
  echo "[npm-smoke] FAIL: $*" >&2
  exit 1
}

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    fail "$1 not found on PATH${2:+; $2}"
  fi
}

need node
need npm

# The wasm smoke runs the bundle in Node; the packages declare >=18.
NODE_MAJOR="$(node -e 'process.stdout.write(process.version.slice(1).split(".")[0])')"
if ((NODE_MAJOR < 18)); then
  fail "node >= 18 required (found $(node --version))"
fi

# --- Locate the native library ---------------------------------------------
# RELATE_LIB (same env var the client honors) points at a prebuilt
# library when it names an existing file. Otherwise search
# target/{debug,release} for each basename from scripts/ffi-lib.sh --names,
# debug before release.
NATIVE_LIB=""
if [[ -n "${RELATE_LIB:-}" ]]; then
  if [[ -f "$RELATE_LIB" ]]; then
    NATIVE_LIB="$RELATE_LIB"
  else
    fail "RELATE_LIB is set but not a file: $RELATE_LIB"
  fi
fi
if [[ -z "$NATIVE_LIB" ]]; then
  _ffi_names_text="$(bash "$REPO/scripts/ffi-lib.sh" --names)"
  _lib_names=()
  while IFS= read -r _ffi_name; do
    [[ -n "$_ffi_name" ]] && _lib_names+=("$_ffi_name")
  done <<< "$_ffi_names_text"
  for dir in debug release; do
    for name in "${_lib_names[@]}"; do
      if [[ -f "$REPO/target/$dir/$name" ]]; then
        NATIVE_LIB="$REPO/target/$dir/$name"
        break 2
      fi
    done
  done
  unset _ffi_names_text _lib_names _ffi_name
fi
if [[ -z "$NATIVE_LIB" ]]; then
  fail "no librelate_ffi in $REPO/target/{debug,release}; build it first: cargo build -p relate-ffi (or set RELATE_LIB)"
fi
log "native library: $NATIVE_LIB"

# --- Scratch space ----------------------------------------------------------
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
PACK_DIR="$WORK/pack"
mkdir -p "$PACK_DIR"

# pack <dir> <dest>: npm pack <dir> into <dest>, print the tarball path.
pack() {
  local out
  out="$(npm pack "$1" --pack-destination "$2" | tail -n 1)"
  if [[ -z "$out" || ! -f "$2/$out" ]]; then
    fail "npm pack of $1 produced no tarball in $2"
  fi
  echo "$2/$out"
}

# tgz_for <env-value> <env-name> <produce-cmd> [args...]: resolve a smoke
# tarball. A set <env-value> must be an existing file and is used as-is;
# otherwise run <produce-cmd>, which must print the tarball path. Keeping the
# produce step a command keeps it lazy: a pre-packed tarball skips the
# repo pack (and, for wasm, the bundle build) entirely.
tgz_for() {
  local env_value="$1" env_name="$2"
  shift 2
  if [[ -n "$env_value" ]]; then
    [[ -f "$env_value" ]] || fail "$env_name is not a file: $env_value"
    echo "$env_value"
  else
    "$@"
  fi
}

# Fabricate a @relate/<platform>-<arch> package around the
# repo-built native library and pack it (smoke 2 producer).
pack_platform_pkg() {
  local fab="$WORK/platform-pkg"
  mkdir -p "$fab/lib"
  cp "$PLATFORM_DIR/package.json" "$fab/package.json"
  cp "$NATIVE_LIB" "$fab/lib/$(basename "$NATIVE_LIB")"
  pack "$fab" "$PACK_DIR"
}

# Build the wasm bundle into this run's scratch dir, then pack it (smoke 3
# producer). A private directory, so another job writing the shared
# surfaces/relate-wasm/pkg at the same time cannot delete it mid-build.
# Runs inside tgz_for's $(...), so only the tarball path may reach stdout —
# progress goes to stderr.
pack_wasm_pkg() {
  local wasm_pkg="$WORK/wasm-pkg"
  log "smoke 3/3: building wasm bundle (scripts/build-wasm.sh --mode dev)" >&2
  BUILD_WASM_OUT_DIR="$wasm_pkg" bash "$REPO/scripts/build-wasm.sh" --mode dev >&2 \
    || fail "wasm bundle build failed"
  pack "$wasm_pkg" "$PACK_DIR"
}

# The Node one-query assertion shared by both embedded smokes: open an
# in-memory database through the installed @relate/relate package
# and check RETURN 1 AS n round-trips. Result shape per ADR-0216:
# column-keyed rows plus the stats counters and result metadata.
write_embedded_smoke() {
  cat >"$1/smoke.cjs" <<'JS'
const { Database } = require("@relate/relate");
const db = Database.open(":memory:");
const conn = db.connect();
const result = conn.query("RETURN 1 AS n");
if (
  result.columns.length !== 1 ||
  result.columns[0] !== "n" ||
  result.rows.length !== 1 ||
  result.rows[0].n !== 1 ||
  !result.stats ||
  result.stats.nodes_created !== 0 ||
  result.queryType !== "r" ||
  result.namespace !== "neo4j"
) {
  throw new Error("unexpected result: " + JSON.stringify(result));
}
console.log("query ok: RETURN 1 AS n -> 1");
JS
}

# No RESULT_* summary state: fail() exits before any summary could print, so
# a FAIL default is a dead state — each smoke logs PASS/SKIP directly as it
# completes.

# --- Smoke 1: embedded-fallback ---------------------------------------------
log "smoke 1/3: embedded-fallback (tarball + RELATE_LIB)"
MAIN_TGZ="$(tgz_for "${NPM_SMOKE_MAIN_TGZ:-}" NPM_SMOKE_MAIN_TGZ pack "$REPO/clients/javascript" "$PACK_DIR")"
log "packed $MAIN_TGZ"
PROJ1="$WORK/proj-fallback"
mkdir -p "$PROJ1"
(cd "$PROJ1" && npm init -y >/dev/null && npm install --no-audit --no-fund "$MAIN_TGZ")
write_embedded_smoke "$PROJ1"
(cd "$PROJ1" && RELATE_LIB="$NATIVE_LIB" node smoke.cjs) ||
  fail "embedded-fallback: query through the packed tarball failed"
log "embedded-fallback: PASS"

# --- Smoke 2: embedded-platform-package --------------------------------------
# Map this host onto a clients/javascript/platforms/<platform>-<arch> dir,
# matching the PLATFORMS table in clients/javascript/index.js: one keyed
# table over "<uname -s>-<uname -m>". An empty PLATFORM_DIR means this host
# has no platform package and the smoke skips.
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) PLATFORM_DIR="$REPO/clients/javascript/platforms/darwin-arm64" ;;
  Darwin-x86_64) PLATFORM_DIR="$REPO/clients/javascript/platforms/darwin-x64" ;;
  Linux-x86_64 | Linux-amd64) PLATFORM_DIR="$REPO/clients/javascript/platforms/linux-x64" ;;
  Linux-aarch64 | Linux-arm64) PLATFORM_DIR="$REPO/clients/javascript/platforms/linux-arm64" ;;
  MINGW*-x86_64 | MSYS*-x86_64 | CYGWIN*-x86_64) PLATFORM_DIR="$REPO/clients/javascript/platforms/win32-x64" ;;
  *) PLATFORM_DIR="" ;;
esac

if [[ -n "$PLATFORM_DIR" && -d "$PLATFORM_DIR" ]]; then
  log "smoke 2/3: embedded-platform-package ($(basename "$PLATFORM_DIR"), no RELATE_LIB)"
  PLAT_TGZ="$(tgz_for "${NPM_SMOKE_PLATFORM_TGZ:-}" NPM_SMOKE_PLATFORM_TGZ pack_platform_pkg)"
  log "packed $PLAT_TGZ"
  PROJ2="$WORK/proj-platform"
  mkdir -p "$PROJ2"
  (cd "$PROJ2" && npm init -y >/dev/null && npm install --no-audit --no-fund "$MAIN_TGZ" "$PLAT_TGZ")
  write_embedded_smoke "$PROJ2"
  # env -u: a caller-exported RELATE_LIB must not mask the
  # optional-dependency resolution this smoke exists to prove.
  (cd "$PROJ2" && env -u RELATE_LIB node smoke.cjs) ||
    fail "embedded-platform-package: query via the platform optional-dependency failed"
  log "embedded-platform-package: PASS"
else
  log "smoke 2/3: SKIP (no platform package for $(uname -s)-$(uname -m))"
fi

# --- Smoke 3: wasm ------------------------------------------------------------
WASM_TGZ="$(tgz_for "${NPM_SMOKE_WASM_TGZ:-}" NPM_SMOKE_WASM_TGZ pack_wasm_pkg)"
log "smoke 3/3: wasm (tarball initialized in Node, no browser)"
log "packed $WASM_TGZ"
PROJ3="$WORK/proj-wasm"
mkdir -p "$PROJ3"
(cd "$PROJ3" && npm init -y >/dev/null && npm install --no-audit --no-fund "$WASM_TGZ")
cat >"$PROJ3/smoke.mjs" <<'JS'
import init, { RelateDb } from "@relate/relate-wasm";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
// Node's fetch has no file: scheme, so hand init() the packed wasm bytes
// instead of letting it resolve the default URL. The single-object form is
// required: a positional argument trips wasm-bindgen's "deprecated
// parameters for the initialization function" warning.
const wasmPath = require.resolve("@relate/relate-wasm/relate_bg.wasm");
await init({ module_or_path: readFileSync(wasmPath) });
const db = new RelateDb();
const result = JSON.parse(db.execute("RETURN 1 AS n", null));
if (result.rows[0].n !== 1) {
  throw new Error("unexpected result: " + JSON.stringify(result));
}
console.log("query ok: RETURN 1 AS n -> 1");
JS
(cd "$PROJ3" && node smoke.mjs) || fail "wasm: query through the packed tarball failed"
log "wasm: PASS"
