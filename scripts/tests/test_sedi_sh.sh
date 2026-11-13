#!/bin/sh
# TAP suite for the scripts/lib/sedi.sh contract: the in-place rewrite
# lands, the sidecar the helper owns never survives, every expression of
# one call applies, and a pattern that matches nothing is a no-op (never a
# failure). Both release scripts edit through this helper.
#
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)

. "$ROOT/scripts/tests/lib/tap.sh"

tap_sandbox relate-sedi-sh-test

# shellcheck source=../lib/sedi.sh
. "$ROOT/scripts/lib/sedi.sh"

# The one spelling of "no sidecar survives": the sidecar path is built the
# way the helper builds it — never a `*.bak` glob swept over the sandbox —
# and the assertion is that that path is absent.
assert_no_sidecar() { # $1 = description, $2 = the sedi target
	tap_run_failure "$1" test -e "$2.bak"
}

target="$sandbox/notes.txt"
printf 'version = "0.16.1"\nkeep = "unchanged"\n' >"$target"

sedi "$target" 's/version = "[^"]*"/version = "0.1.0"/'
grep -qx 'version = "0.1.0"' "$target"
tap_check "sedi: rewrites the match in place" $?
assert_no_sidecar "sedi: leaves no .bak sidecar behind" "$target"

# Several expressions in one call, as the Swift stamp passes them.
sedi "$target" \
	-e 's/version = "[^"]*"/version = "0.2.0"/' \
	-e 's/keep = "[^"]*"/keep = "edited"/'
grep -qx 'version = "0.2.0"' "$target" && grep -qx 'keep = "edited"' "$target"
tap_check "sedi: applies every expression of one call" $?

# A pattern that matches nothing is a no-op, not a failure: sed exits 0.
# The snapshot is a byte copy, never `$(cat)` — a command substitution
# strips trailing newlines, and "untouched" has to mean byte-identical.
cp "$target" "$sandbox/before.txt"
tap_run_success "sedi: a non-matching pattern succeeds" \
	sedi "$target" 's/nothing-here/x/'
cmp -s "$target" "$sandbox/before.txt"
tap_check "sedi: a non-matching pattern leaves the file byte-identical" $?

# A failed rewrite is the caller's failure to see: sed's own status, never
# the status of the cleanup that runs after it. A sidecar stands in place
# first — sed compiles its script before it opens the target, so a failure
# never creates one itself, and the cleanup assertion would otherwise hold
# vacuously.
: >"$target.bak"
cp "$target" "$sandbox/before-failure.txt"
tap_run_failure "sedi: a failing expression reports failure" \
	sedi "$target" 's/[unterminated/x/'
cmp -s "$target" "$sandbox/before-failure.txt"
tap_check "sedi: a failed rewrite leaves the file byte-identical" $?
assert_no_sidecar "sedi: the failure path removes the sidecar standing beside the target" "$target"

# sedi_remove_sidecars is what a caller restoring an interrupted run calls:
# a sidecar sedi did not leave is still removed through the same shape.
: >"$target.bak"
sedi_remove_sidecars "$target"
assert_no_sidecar "sedi_remove_sidecars: removes the .bak beside its file" "$target"

tap_done
