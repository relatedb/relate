#!/bin/sh
# Hermetic suite for the x86-64 baseline ISA gate
# (scripts/check-linux-isa.sh): the baseline-only pass, a higher ISA level
# in the ELF property note, a note section without the ISA line and an
# ELF file with no GNU property note section at all (the pinned
# rust:1.99-bookworm container's native build — the state the release
# lane's binary actually carries, accepted only under --pinned-toolchain),
# a note the classifier cannot parse, and the argument contract. A fake
# readelf on PATH serves the canned `-n` report, so the suite needs no
# ELF binary and runs on any host.
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
GATE="$ROOT/scripts/check-linux-isa.sh"

. "$ROOT/scripts/tests/lib/tap.sh"
. "$ROOT/scripts/tests/lib/gate_stubs.sh"

tap_sandbox check-linux-isa

stubbin="$sandbox/bin"
tap_stub_readelf "$stubbin" -n notes

run_gate() { # gate arguments
  PATH="$stubbin:$PATH" tap_run_capture "$sandbox/out" "$sandbox/err" -- bash "$GATE" "$@"
}

# One fixture writer: every property-note case shares this readelf -n
# preamble; only the Properties line differs.
notes_fixture() { # $1 = tag, $2 = the value after "Properties: "
	fixture=$sandbox/$1
	: >"$fixture"
	{
		cat <<'PREAMBLE'
ELF Header:
  Class:                             ELF64
Displaying notes found in: .note.gnu.property
  Owner                Data size 	Description
  GNU                  0x00000010	NT_GNU_PROPERTY_TYPE_0
PREAMBLE
		printf '      Properties: %s\n' "$2"
	} >"$fixture.notes"
}

# One case runner: the gate over a fixture, with the expected outcome and
# the exact expected line stated once — an empty line asserts the status
# only.
gate_case() { # $1 = description, $2 = pass|refusal, $3 = expected line (empty = status only); rest = gate arguments
	gc_desc=$1
	gc_want=$2
	gc_line=$3
	shift 3
	run_gate "$@"
	case "$gc_want" in
		pass) gc_assert=tap_check; gc_stream=out ;;
		refusal) gc_assert=tap_check_refusal; gc_stream=err ;;
		*)
			echo "gate_case: unknown outcome: $gc_want" >&2
			exit 2
			;;
	esac
	"$gc_assert" "$gc_desc" "$tap_status"
	if [ -n "$gc_line" ]; then
		assert_same "the $gc_want line is exact: $gc_desc" \
			"$(cat "$sandbox/$gc_stream")" "$gc_line"
	fi
}

# --- pass: the note lists the baseline ISA only ------------------------

notes_fixture baseline 'x86 ISA needed: x86-64-baseline'
gate_case "the gate passes a baseline-ISA binary" pass \
	"check-linux-isa: $sandbox/baseline: x86-64-baseline only" \
	"$sandbox/baseline"

# --- higher ISA level --------------------------------------------------

notes_fixture v3 'x86 ISA needed: x86-64-baseline, x86-64-v3'
gate_case "the gate fails a binary needing a higher ISA level" refusal \
	"check-linux-isa: $sandbox/v3: requires x86-64-v3; build at the x86-64 baseline ISA" \
	"$sandbox/v3"

# The refusal is unconditional: the vouch never lifts it.
gate_case "the gate fails a higher ISA level even under --pinned-toolchain" refusal "" \
	--pinned-toolchain "$sandbox/v3"

# --- no "x86 ISA needed" note ------------------------------------------

notes_fixture no-note 'x86 feature: IBT, SHSTK'

# Fail-closed by default: a note-less binary is also what a raised
# target-cpu build would look like, so only the lane's vouch accepts it.
gate_case "the gate refuses a note-less binary without the vouch" refusal \
	"check-linux-isa: $sandbox/no-note: no 'x86 ISA needed' GNU property note (the pinned rust:1.99-bookworm link emits none at baseline — pass --pinned-toolchain to accept that state)" \
	"$sandbox/no-note"

gate_case "the gate passes a note-less binary under --pinned-toolchain" pass \
	"check-linux-isa: $sandbox/no-note: no 'x86 ISA needed' note (nothing beyond x86-64-baseline; accepted under --pinned-toolchain)" \
	--pinned-toolchain "$sandbox/no-note"

# --- no .note.gnu.property section at all (the pinned container) -------

no_section="$sandbox/no-section"
: >"$no_section"
cat >"$no_section.notes" <<'REPORT'
ELF Header:
  Class:                             ELF64
Displaying notes found in: .note.ABI-tag
  Owner                Data size 	Description
  GNU                  0x00000010	NT_GNU_ABI_TAG (ABI version tag)
    OS: Linux, ABI: 3.2.0

Displaying notes found in: .note.gnu.build-id
  Owner                Data size 	Description
  GNU                  0x00000014	NT_GNU_BUILD_ID (unique build ID bitstring)
    Build ID: 81bbf74720a2875dcdfd756a0b3e0d293471ed57
REPORT

gate_case "the gate refuses a note-section-less binary without the vouch" refusal "" \
	"$no_section"
gate_case "the gate passes a note-section-less binary under --pinned-toolchain" pass \
	"check-linux-isa: $no_section: no 'x86 ISA needed' note (nothing beyond x86-64-baseline; accepted under --pinned-toolchain)" \
	--pinned-toolchain "$no_section"

# --- a note the classifier cannot parse --------------------------------

notes_fixture weird 'x86 ISA needed: x86-64-future'
gate_case "the gate refuses a note whose level it cannot classify" refusal \
	"check-linux-isa: $sandbox/weird:       Properties: x86 ISA needed: x86-64-future" \
	"$sandbox/weird"

# An unknown level beside baseline is equally unclassifiable: the whole
# list is judged, not probed for a substring.
notes_fixture mixed 'x86 ISA needed: x86-64-baseline, x86-64-future'
gate_case "the gate refuses baseline mixed with an unknown level" refusal \
	"check-linux-isa: $sandbox/mixed:       Properties: x86 ISA needed: x86-64-baseline, x86-64-future" \
	"$sandbox/mixed"

# Every level above baseline is named, comma-joined, in one refusal.
notes_fixture multi 'x86 ISA needed: x86-64-baseline, x86-64-v2, x86-64-v3'
gate_case "the gate names every higher ISA level in one refusal" refusal \
	"check-linux-isa: $sandbox/multi: requires x86-64-v2,x86-64-v3; build at the x86-64 baseline ISA" \
	"$sandbox/multi"

# --- readelf preflight --------------------------------------------------

# An empty PATH hides readelf; bash is invoked by absolute path so the
# gate itself still starts.
bash_bin=$(command -v bash)
mkdir -p "$sandbox/empty"
# A subshell: a PATH prefix on a function call can persist in sh.
(
	PATH="$sandbox/empty"
	tap_run_capture "$sandbox/out" "$sandbox/err" -- "$bash_bin" "$GATE" "$sandbox/baseline"
	exit "$tap_status"
)
tap_status=$?
assert_same "the gate exits 2 when readelf is not installed" "$tap_status" 2
assert_same "the refusal names the missing readelf" \
	"$(cat "$sandbox/err")" "check-linux-isa: readelf not found (install binutils)"

# The stub fails when the binary has no report beside it.
: >"$sandbox/not-elf"
run_gate "$sandbox/not-elf"
assert_same "the gate exits 1 when readelf fails on the file" "$tap_status" 1
assert_same "the refusal names the readelf failure" \
	"$(cat "$sandbox/err")" "check-linux-isa: $sandbox/not-elf: readelf failed (not an ELF file?)"

# --- usage --------------------------------------------------------------

run_gate
assert_same "the gate refuses to run without a binary (exit 2)" "$tap_status" 2
assert_same "the usage line names the flag and the argument" \
	"$(cat "$sandbox/err")" "usage: $GATE [--pinned-toolchain] BINARY"

run_gate "$sandbox/baseline" extra
assert_same "the gate refuses a second argument (exit 2)" "$tap_status" 2

run_gate --nope "$sandbox/baseline"
assert_same "the gate refuses an unknown flag (exit 2)" "$tap_status" 2
assert_same "the refusal names the unknown flag" \
	"$(cat "$sandbox/err")" "check-linux-isa: unknown flag: --nope"

tap_done
