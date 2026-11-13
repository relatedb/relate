#!/usr/bin/env bash
# Shared build entry for the Swift surface's binary target (ADR-0433): builds relate-ffi as a static
# library per Apple slice and assembles target/apple/RelateFFI.xcframework.
#
# Slices:
#   all    ios-arm64, ios-arm64_x86_64-simulator, macos-arm64_x86_64
#   macos  macos-arm64_x86_64 only (enough for `swift test` on a Mac)
#
# --zip also writes target/apple/RelateFFI.xcframework.zip, the release
# asset SwiftPM downloads (ditto keeps the framework's symlinks and puts the
# .xcframework directory at the archive root, as SwiftPM requires).
#
# Modes beyond the default build-and-assemble (ADR-0572, the release
# workflow's parallel shape):
#   --build-target TRIPLE  build that one slice's static library and stop
#                          (no assembly, no zip) — the release legs run one
#                          per triple and upload the library
#   --assemble-only        skip the builds; assemble (and zip with --zip)
#                          from the libraries already under target/, so a
#                          different job can stamp the assembled bytes
#
# The C header is never copied into the repo: it is staged from
# surfaces/relate-ffi/include/relate.h into the XCFramework with a
# module map for the `CRelate` clang module. Headers sit in a
# `CRelate/` subdirectory so the module map cannot collide with another
# XCFramework's in an app's shared include path.
#
# Env:
#   IPHONEOS_DEPLOYMENT_TARGET  (default 16.0) — must match Package.swift
#   MACOSX_DEPLOYMENT_TARGET    (default 13.0) — must match Package.swift
#   XCFRAMEWORK_CARGO_PROFILE   (default release) — a profile name
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SLICES="all"
ZIP=false
# Three mutually exclusive modes, one enum — a second mode flag is a
# single refusal, not a silent override:
#   build-and-assemble  the default: build every slice's library, then
#                       assemble the XCFramework
#   build-target        build one slice's library and stop — the release
#                       workflow's parallel legs
#   assemble-only       skip the builds; assemble from the libraries
#                       already under target/ — the release stamp job
MODE="build-and-assemble"
BUILD_TARGET=
mode_refuse() {
  echo "--build-target and --assemble-only are two modes; pick one" >&2; exit 2
}
need_triple() {
  echo "--build-target needs a triple as its value" >&2; exit 2
}
while [[ $# -gt 0 ]]; do
  case "$1" in
    --slices) SLICES="$2"; shift 2 ;;
    --slices=*) SLICES="${1#*=}"; shift ;;
    --zip) ZIP=true; shift ;;
    --build-target)
      [[ "$MODE" = build-and-assemble ]] || mode_refuse
      [[ -n "${2:-}" ]] || need_triple
      MODE="build-target"; BUILD_TARGET="$2"; shift 2
      ;;
    --build-target=*)
      [[ "$MODE" = build-and-assemble ]] || mode_refuse
      BUILD_TARGET="${1#*=}"
      [[ -n "$BUILD_TARGET" ]] || need_triple
      MODE="build-target"; shift
      ;;
    --assemble-only)
      [[ "$MODE" = build-and-assemble ]] || mode_refuse
      MODE="assemble-only"; shift
      ;;
    -h|--help)
      echo "usage: $0 [--slices all|macos] [--zip] [--build-target TRIPLE] [--assemble-only]" >&2
      echo "  default            build every slice's library, then assemble" >&2
      echo "  --build-target     build one slice's library and stop (no assembly)" >&2
      echo "  --assemble-only    assemble from the libraries already under target/" >&2
      exit 0
      ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [[ "$MODE" = build-target && "$SLICES" != all ]]; then
  echo "--build-target selects its own target; --slices is meaningless beside it" >&2; exit 2
fi
if [[ "$MODE" = build-target && "$ZIP" = true ]]; then
  echo "--build-target builds one library; there is no XCFramework to zip" >&2; exit 2
fi

export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-16.0}"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.0}"
PROFILE="${XCFRAMEWORK_CARGO_PROFILE:-release}"
OUT="$REPO/target/apple"
XCFW="$OUT/RelateFFI.xcframework"
LIB="librelate_ffi.a"

# cargo's `release` profile writes to target/<triple>/release; any other
# profile writes to target/<triple>/<profile>.
lib_for() { echo "$REPO/target/$1/$PROFILE/$LIB"; }

# The slice groups, named once: the macOS pair lipos into the macos
# slice, the simulator pair into the iOS simulator slice, the device
# triple stands alone. ALL_TARGETS, the --slices selection, and every
# assembly input below read these tables — the triples are spelled
# nowhere else.
MACOS_TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
SIM_TARGETS=(aarch64-apple-ios-sim x86_64-apple-ios)
IOS_DEVICE_TARGET=aarch64-apple-ios
ALL_TARGETS=("${MACOS_TARGETS[@]}" "${SIM_TARGETS[@]}" "$IOS_DEVICE_TARGET")

case "$SLICES" in
  all) TARGETS=("${ALL_TARGETS[@]}") ;;
  macos) TARGETS=("${MACOS_TARGETS[@]}") ;;
  *) echo "unknown --slices value: $SLICES (want all|macos)" >&2; exit 2 ;;
esac
if [[ "$MODE" = build-target ]]; then
  valid=0
  for t in "${ALL_TARGETS[@]}"; do
    [[ "$BUILD_TARGET" = "$t" ]] && valid=1
  done
  if [[ "$valid" != 1 ]]; then
    echo "unknown --build-target: $BUILD_TARGET (want one of ${ALL_TARGETS[*]})" >&2; exit 2
  fi
  TARGETS=("$BUILD_TARGET")
fi

case "$MODE" in
  build-and-assemble | build-target)
    installed="$(rustup target list --installed)"
    for t in "${TARGETS[@]}"; do
      grep -qx "$t" <<<"$installed" || {
        echo "missing Rust target $t: rustup target add ${TARGETS[*]}" >&2
        exit 1
      }
    done

    for t in "${TARGETS[@]}"; do
      cargo build -p relate-ffi --profile "$PROFILE" --target "$t" \
        --manifest-path "$REPO/Cargo.toml"
    done
    ;;
  assemble-only) ;;
esac

# build-target is a build-only mode: the library is the deliverable (the
# release legs consume the printed path); no assembly follows.
if [[ "$MODE" = build-target ]]; then
  echo "$(lib_for "$BUILD_TARGET")"
  exit 0
fi

# assemble-only trusts the caller to have built the libraries (the
# release legs check out the exact commit); a missing or empty one is a
# loud stop, never a silent partial framework.
if [[ "$MODE" = assemble-only ]]; then
  for t in "${TARGETS[@]}"; do
    [[ -s "$(lib_for "$t")" ]] || {
      echo "assemble-only: missing or empty $(lib_for "$t")" >&2
      exit 1
    }
  done
fi

rm -rf "$OUT/headers" "$OUT/macos" "$OUT/ios-sim" "$XCFW"
mkdir -p "$OUT/headers/CRelate" "$OUT/macos" "$OUT/ios-sim"
cp "$REPO/surfaces/relate-ffi/include/relate.h" "$OUT/headers/CRelate/"
cat >"$OUT/headers/CRelate/module.modulemap" <<'EOF'
module CRelate {
    header "relate.h"
    link "iconv"
    link framework "CoreFoundation"
    export *
}
EOF

# Map each slice group through lib_for: lipo's input arity is then the
# table's length, never a subscript pair.
macos_libs=()
for t in "${MACOS_TARGETS[@]}"; do macos_libs+=("$(lib_for "$t")"); done
sim_libs=()
for t in "${SIM_TARGETS[@]}"; do sim_libs+=("$(lib_for "$t")"); done

lipo -create "${macos_libs[@]}" -output "$OUT/macos/$LIB"
args=(-library "$OUT/macos/$LIB" -headers "$OUT/headers")
if [[ "$SLICES" != macos ]]; then
  lipo -create "${sim_libs[@]}" -output "$OUT/ios-sim/$LIB"
  args+=(-library "$(lib_for "$IOS_DEVICE_TARGET")" -headers "$OUT/headers"
         -library "$OUT/ios-sim/$LIB" -headers "$OUT/headers")
fi

xcodebuild -create-xcframework "${args[@]}" -output "$XCFW"
echo "$XCFW"

if $ZIP; then
  rm -f "$XCFW.zip"
  ditto -c -k --keepParent "$XCFW" "$XCFW.zip"
  echo "$XCFW.zip"
fi
