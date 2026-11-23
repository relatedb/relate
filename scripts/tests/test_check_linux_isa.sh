#!/bin/sh
# Hermetic suite for the x86-64 baseline ISA gate
# (scripts/check-linux-isa.sh): the baseline-only pass, a higher ISA level
# in the ELF property note, and an ELF file with no "x86 ISA needed" note.
# A fake readelf on PATH serves the canned `-n` report, so the suite needs
# no ELF binary and runs on any host.
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
GATE="$ROOT/scripts/check-linux-isa.sh"

. "$ROOT/scripts/tests/lib/tap.sh"

tap_sandbox check-linux-isa

stubbin="$sandbox/bin"
mkdir -p "$stubbin"
cat >"$stubbin/readelf" <<'STUB'
#!/bin/sh
# Fake readelf: the fixture beside the named binary holds its report.
for last; do :; done
case "$1" in
  -n) cat "$last.notes" ;;
  *) exit 1 ;;
esac
STUB
chmod +x "$stubbin/readelf"

run_gate() { # $1 = description; rest = gate arguments
  tap_desc=$1
  shift
  PATH="$stubbin:$PATH" bash "$GATE" "$@" >"$sandbox/out" 2>"$sandbox/err"
  tap_status=$?
}

# --- pass: the note lists the baseline ISA only ------------------------

baseline="$sandbox/baseline"
: >"$baseline"
cat >"$baseline.notes" <<'REPORT'
ELF Header:
  Class:                             ELF64
Displaying notes found in: .note.gnu.property
  Owner                Data size 	Description
  GNU                  0x00000010	NT_GNU_PROPERTY_TYPE_0
      Properties: x86 ISA needed: x86-64-baseline
REPORT
run_gate "a baseline-ISA binary passes" "$baseline"
tap_check "the gate passes a baseline-ISA binary" "$tap_status"
assert_same "the pass line names the baseline ISA" \
	"$(cat "$sandbox/out")" "check-linux-isa: $baseline: x86-64-baseline only"

# --- higher ISA level --------------------------------------------------

v3="$sandbox/v3"
: >"$v3"
cat >"$v3.notes" <<'REPORT'
ELF Header:
  Class:                             ELF64
Displaying notes found in: .note.gnu.property
  Owner                Data size 	Description
  GNU                  0x00000010	NT_GNU_PROPERTY_TYPE_0
      Properties: x86 ISA needed: x86-64-baseline, x86-64-v3
REPORT
run_gate "a binary needing x86-64-v3" "$v3"
if [ "$tap_status" -ne 0 ]; then
	tap_ok "the gate fails a binary needing a higher ISA level"
else
	tap_not_ok "the gate fails a binary needing a higher ISA level"
fi
assert_same "the refusal names the higher level" \
	"$(cat "$sandbox/err")" \
	"check-linux-isa: $v3: requires x86-64-v3; build at the x86-64 baseline ISA"

# --- no "x86 ISA needed" note ------------------------------------------

no_note="$sandbox/no-note"
: >"$no_note"
cat >"$no_note.notes" <<'REPORT'
ELF Header:
  Class:                             ELF64
Displaying notes found in: .note.gnu.property
  Owner                Data size 	Description
  GNU                  0x00000010	NT_GNU_PROPERTY_TYPE_0
      Properties: x86 feature: IBT, SHSTK
REPORT
run_gate "a binary without the x86 ISA note" "$no_note"
if [ "$tap_status" -ne 0 ]; then
	tap_ok "the gate fails a binary without the x86 ISA note"
else
	tap_not_ok "the gate fails a binary without the x86 ISA note"
fi
assert_same "the refusal names the missing note" \
	"$(cat "$sandbox/err")" \
	"check-linux-isa: $no_note: no 'x86 ISA needed' GNU property note"

# --- usage -------------------------------------------------------------

run_gate "no binary"
assert_same "the gate refuses to run without a binary (exit 2)" "$tap_status" 2
assert_same "the usage line names the argument" \
	"$(cat "$sandbox/err")" "usage: $GATE BINARY"

run_gate "a second argument" "$baseline" extra
assert_same "the gate refuses a second argument (exit 2)" "$tap_status" 2

tap_done
