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

tap_done
