#!/bin/sh
# CLI-contract tests for scripts/premerge-check.sh: the argument surface
# only (every case exits before any merge or cargo run). `--pr` is the one
# way to name a pull request; a bare positional is always a rev.
#
# Assertions use the shared TAP harness (scripts/tests/lib/tap.sh).

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
SCRIPT="$ROOT/scripts/premerge-check.sh"

. "$ROOT/scripts/tests/lib/tap.sh"

tap_sandbox relate-premerge-cli-test

# A gh that always fails: a PR lookup must surface it, never fall back.
mkdir "$sandbox/bin"
printf '#!/bin/sh\necho "stub gh failure" >&2\nexit 1\n' >"$sandbox/bin/gh"
chmod +x "$sandbox/bin/gh"

# expect_exit <status> <stderr-substring> <description> [args...]
expect_exit() {
	want=$1
	needle=$2
	desc=$3
	shift 3
	out=$(PATH="$sandbox/bin:$PATH" "$SCRIPT" "$@" 2>&1 >/dev/null)
	status=$?
	case $out in
	*"$needle"*) found=0 ;;
	*) found=1 ;;
	esac
	if [ "$status" -eq "$want" ] && [ "$found" -eq 0 ]; then
		tap_ok "$desc"
	else
		tap_not_ok "$desc (status $status, want $want; want '$needle'; got: $out)"
	fi
}

expect_exit 2 "Usage" "no arguments prints usage"
expect_exit 2 "Usage" "an unknown flag prints usage" --bogus
expect_exit 2 "Usage" "--pr without a value prints usage" --pr
expect_exit 2 "Usage" "--pr with a non-number prints usage" --pr abc
expect_exit 2 "Usage" "--pr with an empty value prints usage" --pr ""
expect_exit 2 "Usage" "--pr= with an empty value prints usage" --pr=
expect_exit 2 "Usage" "--pr with two positionals prints usage" --pr 1 main extra
expect_exit 2 "Usage" "three positionals print usage" a b c
expect_exit 2 "gh could not read PR 7" "--pr reports a failing gh, never a rev fallback" --pr 7
expect_exit 2 "gh could not read PR 7" "--pr=N spelling" --pr=7 main
# A bare all-digit argument is a rev: no gh call, so it fails as a non-commit.
expect_exit 2 "999999999 is not a commit" "bare digits are a rev, not a PR" 999999999

# A scratch repository carrying a copy of the script, so the base and merge
# paths run against history this test controls. Both cases exit before cargo
# is invoked.
repo="$sandbox/repo"
mkdir -p "$repo/scripts/lib"
cp "$SCRIPT" "$repo/scripts/premerge-check.sh"
cp "$ROOT/scripts/lib/usage.sh" "$repo/scripts/lib/usage.sh"
git -C "$repo" init -q -b main
git -C "$repo" config user.email t@example.invalid
git -C "$repo" config user.name t
git -C "$repo" config commit.gpgsign false
echo base >"$repo/f"
git -C "$repo" add f scripts
git -C "$repo" commit -q -m base
git -C "$repo" checkout -q -b side
echo side >"$repo/f"
git -C "$repo" commit -q -am side
side=$(git -C "$repo" rev-parse HEAD)
git -C "$repo" checkout -q main
echo main >"$repo/f"
git -C "$repo" commit -q -am main
main_oid=$(git -C "$repo" rev-parse HEAD)

# run_repo <args...>: stderr and status of the script inside the scratch repo.
run_repo() {
	out=$("$repo/scripts/premerge-check.sh" "$@" 2>&1 >/dev/null)
	status=$?
}

# A full-length hex base that is no local commit is an environment error,
# never reinterpreted as a branch name to fetch.
run_repo "$side" 0123456789012345678901234567890123456789
case $out in
*"pinned base 0123456789012345678901234567890123456789 is not a local commit (fetch it first)"*) found=0 ;;
*) found=1 ;;
esac
if [ "$status" -eq 2 ] && [ "$found" -eq 0 ]; then
	tap_ok "an unresolvable full-hex base fails 2 as a pinned commit"
else
	tap_not_ok "unresolvable full-hex base (status $status; got: $out)"
fi

# A conflicting merge is the exit-1 verdict, with the prefixed header.
run_repo "$side" "$main_oid"
case $out in
*"premerge-check: the merge CONFLICTS — resolve before merging:"*) found=0 ;;
*) found=1 ;;
esac
if [ "$status" -eq 1 ] && [ "$found" -eq 0 ]; then
	tap_ok "a conflicting merge exits 1 with the conflict report"
else
	tap_not_ok "conflict arm (status $status; got: $out)"
fi

tap_done
