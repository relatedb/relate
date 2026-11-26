#!/usr/bin/env bash
# Name the relate FFI library for this OS.
#
# Source (caller receives the exports; does not compile):
#   source scripts/ffi-lib.sh
# Sets RELATE_LIB to
# "${RELATE_LIB:-<repo root>/target/debug/<primary>}" (the repo root
# derived from this script's own location, like every sibling script)
# and prepends that library directory to DYLD_LIBRARY_PATH on Darwin,
# PATH on the MINGW/MSYS/CYGWIN shells, or LD_LIBRARY_PATH on Linux; any
# other OS is refused before anything is exported. A preset RELATE_LIB
# is left unchanged once it names a path with a directory component (a
# bare filename or a root-level path is refused), and the loader path
# follows its directory.
#
# Beside the library path, three derived values are exported so no
# consumer re-sniffs the uname case:
#   RELATE_LIB_DIR      the library directory, from RELATE_LIB (LABS-631),
#                       so consumers read it instead of re-spelling
#                       ${RELATE_LIB%/*}
#   RELATE_FFI_LINK     msvc|cc, the link-style discriminator
#   RELATE_FFI_LOADER   the loader variable's NAME for this OS, so a
#                       consumer prepends to it without its own uname case
#
# Execute (prints basenames and does not export RELATE_LIB):
#   bash scripts/ffi-lib.sh --name
#   bash scripts/ffi-lib.sh --names

_ffi_sourced=0
if (return 0 2>/dev/null); then
  _ffi_sourced=1
fi

if [[ "$_ffi_sourced" -eq 0 ]]; then
  set -euo pipefail
fi

# The scratch roster and its single cleanup: every name this file sets,
# and the two functions it owns. Defined above the first refusal, so a
# refusal can restore the caller's environment however early it fires.
_ffi_scratch="_ffi_sourced _ffi_os _ffi_primary _ffi_names _ffi_loader _ffi_link _ffi_lib _ffi_root _ffi_prev"
_ffi_cleanup() { # the scratch roster's single cleanup, both exits call it
  unset -f _ffi_cleanup _ffi_die
  unset $_ffi_scratch _ffi_scratch
}

# The one refusal path: the message on stderr, the cleanup, then the
# mode's own exit — `return` when sourced, `exit` when executed. Every
# refusal goes through it, so no failure site re-decides how it leaves
# the shell or whether the environment is restored.
_ffi_die() {
  # The mode is read before the cleanup unsets the roster it lives in.
  local sourced="$_ffi_sourced"
  printf 'ffi-lib: %s\n' "$*" >&2
  _ffi_cleanup
  if [[ "$sourced" -eq 1 ]]; then
    return 1
  fi
  exit 1
}

# One uname case names every library and the loader variable for this OS.
# An OS outside the table fails loudly instead of falling through to the
# Linux names — a silent default hands a non-Linux host the wrong library
# and loader variable, and the failure surfaces far from here. The OS
# name is read once, into `_ffi_os`: the table's scrutinee and any
# message about it are then the same string.
_ffi_os="$(uname -s)"
case "$_ffi_os" in
  Darwin)
    _ffi_names=(librelate_ffi.dylib librelate_ffi.so)
    _ffi_loader=DYLD_LIBRARY_PATH
    _ffi_link=cc
    ;;
  MINGW* | MSYS* | CYGWIN*)
    _ffi_names=(relate_ffi.dll)
    _ffi_loader=PATH
    _ffi_link=msvc
    ;;
  Linux)
    _ffi_names=(librelate_ffi.so)
    _ffi_loader=LD_LIBRARY_PATH
    _ffi_link=cc
    ;;
  *)
    _ffi_die "unsupported OS: $_ffi_os — add its library and loader names to the uname case" ||
      return 1
    ;;
esac
# The primary library is the first name every arm lists — derived here,
# once, so an arm cannot disagree with its own list.
_ffi_primary="${_ffi_names[0]}"

if [[ "$_ffi_sourced" -eq 0 ]]; then
  if [[ $# -ne 1 ]]; then
    _ffi_die "expected exactly one argument: --name or --names"
  fi
  case "$1" in
    --name) printf '%s\n' "$_ffi_primary" ;;
    --names) printf '%s\n' "${_ffi_names[@]}" ;;
    *)
      _ffi_die "unknown argument: $1"
      ;;
  esac
  exit 0
fi

# Resolve and validate before any export: RELATE_LIB_DIR is a
# cross-consumer contract, so a value whose directory component is
# unusable (absent or empty — a root-level path) fails loudly at this
# boundary, and the failure path leaves the caller's environment
# exactly as it found it.
_ffi_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
_ffi_lib="${RELATE_LIB:-$_ffi_root/target/debug/$_ffi_primary}"
case "$_ffi_lib" in
  ?*/*) ;;
  *)
    _ffi_die "RELATE_LIB must name a path with a directory component: $_ffi_lib" ||
      return 1
    ;;
esac
export RELATE_LIB="$_ffi_lib"
export RELATE_FFI_LINK="$_ffi_link"
export RELATE_LIB_DIR="${_ffi_lib%/*}"
export RELATE_FFI_LOADER="$_ffi_loader"
# The loader variable's name is data (the uname case above); the
# dynamic export honors it without a second branch over it.
_ffi_prev="${!_ffi_loader:-}"
export "$_ffi_loader=${RELATE_LIB_DIR}${_ffi_prev:+:$_ffi_prev}"

_ffi_cleanup
return 0
