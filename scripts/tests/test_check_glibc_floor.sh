#!/bin/sh
# Hermetic suite for the glibc floor gate (scripts/check-glibc-floor.sh):
# the pass path at or below the documented floor, a binary whose symbols
# require a newer glibc, and a binary that carries no GLIBC_ symbol
# versions at all. A fake readelf on PATH serves the canned
# `--version-info` report, so the suite needs no ELF binary and runs on
# any host.
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
GATE="$ROOT/scripts/check-glibc-floor.sh"

. "$ROOT/scripts/tests/lib/tap.sh"
. "$ROOT/scripts/tests/lib/gate_stubs.sh"

tap_sandbox check-glibc-floor

stubbin="$sandbox/bin"
tap_stub_readelf "$stubbin" --version-info version-info

run_gate() { # gate arguments
  PATH="$stubbin:$PATH" tap_run_capture "$sandbox/out" "$sandbox/err" -- bash "$GATE" "$@"
}

# --- pass: the highest symbol version is derived, not the last listed ---

below="$sandbox/below"
: >"$below"
cat >"$below.version-info" <<'REPORT'
Version symbols section '.gnu.version' entry 0 offset 0x0000000000002a44:
 Addr: 0000000000000000  Offset: 0x00002a44  Link: 5 (.dynstr)
  000:   0 (*local*)       2 (GLIBC_2.2.5)   3 (GLIBC_2.9)
  010:   4 (GLIBC_2.34)    2 (GLIBC_2.2.5)

Version needs section '.gnu.version_r' contains 1 entry:
 Addr: 0x0000000000000000  Offset: 0x00002a50  Link: 5 (.dynstr)
  000000: Version: 1  File: libc.so.6  Cnt: 3
  0x0010: Name: GLIBC_2.34  Flags: none  Version: 4
  0x0020: Name: GLIBC_2.9  Flags: none  Version: 3
  0x0030: Name: GLIBC_2.2.5  Flags: none  Version: 2
REPORT
run_gate "$below"
tap_check "the gate passes a binary at or below the documented floor" "$tap_status"
assert_same "the gate prints the highest GLIBC_ version (2.34, not 2.9 or the last listed 2.2.5)" \
	"$(cat "$sandbox/out")" GLIBC_2.34

# --- above the documented floor ----------------------------------------

above="$sandbox/above"
: >"$above"
cat >"$above.version-info" <<'REPORT'
Version symbols section '.gnu.version' entry 0 offset 0x0000000000003a44:
 Addr: 0000000000000000  Offset: 0x00003a44  Link: 5 (.dynstr)
  000:   0 (*local*)       2 (GLIBC_2.2.5)   3 (GLIBC_2.38)
REPORT
run_gate "$above"
tap_check_refusal "the gate fails a binary above the documented floor" "$tap_status"
assert_same "the refusal names the required version" \
	"$(cat "$sandbox/err")" \
	"check-glibc-floor: $above: requires GLIBC_2.38, above the documented floor GLIBC_2.35"

# --- no GLIBC_ symbol versions -----------------------------------------

nosyms="$sandbox/no-symbols"
: >"$nosyms"
cat >"$nosyms.version-info" <<'REPORT'
Version symbols section '.gnu.version' entry 0 offset 0x0000000000002a44:
 Addr: 0000000000000000  Offset: 0x00002a44  Link: 5 (.dynstr)
  000:   0 (*local*)       1 (*global*)

Version definition section '.gnu.version_d' contains 1 entry:
  000000: Rev: 1  Flags: BASE  Index: 1  Cnt: 1  Name: relate
REPORT
run_gate "$nosyms"
tap_check_refusal "the gate fails a binary without GLIBC_ symbol versions" "$tap_status"
assert_same "the refusal names the missing symbol versions" \
	"$(cat "$sandbox/err")" \
	"check-glibc-floor: $nosyms: no GLIBC_ symbol versions (not a glibc dynamic binary?)"

# --- usage -------------------------------------------------------------

run_gate
assert_same "the gate refuses to run without a binary (exit 2)" "$tap_status" 2
assert_same "the usage line names the argument" \
	"$(cat "$sandbox/err")" "usage: $GATE BINARY"

run_gate "$below" extra
assert_same "the gate refuses a second argument (exit 2)" "$tap_status" 2

tap_done
