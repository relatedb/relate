#!/bin/sh
# Offline, hermetic check for the advance's scoped bump (ADR-0580): the
# advance job in release.yml sweeps the version surfaces through
# scripts/release-bump.sh --skip-changelog, which must leave CHANGELOG.md
# and the .changes/ fragment pool untouched — an advance is not a
# release cut, and rotating then would mint a release section for a
# version that never shipped; fragments are version-independent and ride
# into the next cut (ADR-0627). Against the empty-[Unreleased] changelog
# main carries right after a cut, the scoped bump leaves CHANGELOG.md
# byte-identical while the version surfaces still advance. The cut
# direction itself — what the bump does without the flag — is pinned in
# scripts/tests/test_release_bump.sh, the one place the release-cut
# paths live. The scoped flag is also the one argument the bump script
# accepts: any other is refused — usage, exit 1, nothing touched.
#
# Reuses the release-bump harness (stubs, fixture factory, run helper):
# run_bump takes the fixture and NEXT_VERSION and forwards any further
# arguments to release-bump.sh, so the scoped and refusal cases share
# the cut cases' one invocation.
#
# Run: sh scripts/tests/test_release_advance.sh
#
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)

. "$ROOT/scripts/tests/lib/tap.sh"

tap_sandbox relate-release-advance-test

# The stubs, the fixture factory, and the run helper (no cases here).
. "$ROOT/scripts/tests/lib/release_bump_harness.sh"

# --- Case 1: the advance leaves an empty-[Unreleased] changelog alone -----

after_cut_changelog "$sandbox/changelog-after-cut.md"

fx1="$sandbox/after-cut"
build_fixture "$fx1" "$sandbox/changelog-after-cut.md"

run_bump "$fx1" 0.1.1 --skip-changelog
tap_check "advance: scoped bump exits 0" $?

cmp -s "$sandbox/changelog-after-cut.md" "$fx1/CHANGELOG.md"
tap_check "advance: empty-[Unreleased] CHANGELOG.md byte-identical — no heading inserted, [Unreleased] ref unchanged" $?

grep -qF 'version = "0.1.1"' "$fx1/Cargo.toml"
tap_check "advance: the version surfaces still advanced" $?

# --- Case 1b: the advance leaves the fragment pool alone (ADR-0627) -----------
#
# Fragments are version-independent — they ride into the next cut, so
# the scoped bump collects nothing: the pool is intact and CHANGELOG.md
# still byte-identical.

fx1b="$sandbox/fragments"
build_fixture "$fx1b" "$sandbox/changelog-after-cut.md"
seed_fragments "$fx1b"

run_bump "$fx1b" 0.1.2 --skip-changelog
tap_check "advance with fragments: scoped bump exits 0" $?

cmp -s "$sandbox/changelog-after-cut.md" "$fx1b/CHANGELOG.md"
tap_check "advance with fragments: CHANGELOG.md byte-identical" $?

pool="$(ls -A "$fx1b/.changes" | sort | tr '\n' ' ')"
[ "$pool" = "2-flag.md 3-notes.md README.md " ]
tap_check "advance with fragments: the pool rides to the next cut intact" $?

# --- Case 2: refusal (an argument other than --skip-changelog) ----------------
#
# The scoped flag is the one spelling the bump script's argument loop
# accepts; any other argument is refused before the sweep starts — usage
# on stderr, exit 1, and nothing touched (the refusal precedes the traps
# and the sweep, so there is nothing to restore).

fx2="$sandbox/refusal"
build_fixture "$fx2" "$sandbox/changelog-after-cut.md"

run_bump "$fx2" 0.1.1 --rotate-changelog
[ $? -eq 1 ]
tap_check "refusal: an argument other than --skip-changelog exits 1" $?

grep -qF 'usage: NEXT_VERSION=<X.Y.Z>' "$fx2/stderr.log"
tap_check "refusal: usage printed to stderr" $?

cmp -s "$sandbox/changelog-after-cut.md" "$fx2/CHANGELOG.md"
tap_check "refusal: nothing touched before the refusal" $?

tap_done
