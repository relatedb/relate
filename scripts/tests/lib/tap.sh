# The one TAP-ish assertion harness for the scripts/tests shell tests
# (LABS-622): numbered `ok`/`not ok` lines, a SKIP form, command wrappers
# that state their own outcome, an equality assertion over extracted
# literals, a sandbox helper with signal-safe cleanup, and a plan+summary
# footer whose exit status is 0 only when nothing failed. POSIX sh
# bodies — source it:
#   . "$(dirname -- "$0")/lib/tap.sh"
#
# State (tap_num, tap_passed, tap_failed, sandbox, and the helpers'
# tap_-prefixed scratch) lives here, once — harnesses never re-declare
# counters or sandbox policy.

tap_num=0
tap_passed=0
tap_failed=0

tap_ok() { # $1 = description
	tap_num=$((tap_num + 1))
	printf 'ok %d - %s\n' "$tap_num" "$1"
	tap_passed=$((tap_passed + 1))
}

tap_not_ok() { # $1 = description
	tap_num=$((tap_num + 1))
	printf 'not ok %d - %s\n' "$tap_num" "$1"
	tap_failed=$((tap_failed + 1))
}

tap_skip() { # $1 = description, $2 = reason — an ok line that skips
	tap_ok "$1 # SKIP $2"
}

tap_check() { # $1 = description, $2 = status (0 = pass, like $?)
	if [ "$2" -eq 0 ]; then
		tap_ok "$1"
	else
		tap_not_ok "$1"
	fi
}

tap_run_success() { # $1 = description; rest = the command that must exit 0
	tap_desc="$1"
	shift
	if "$@" >/dev/null 2>&1; then
		tap_ok "$tap_desc"
	else
		tap_not_ok "$tap_desc"
	fi
}

tap_run_failure() { # $1 = description; rest = the command that must exit non-zero
	tap_desc="$1"
	shift
	if "$@" >/dev/null 2>&1; then
		tap_not_ok "$tap_desc"
	else
		tap_ok "$tap_desc"
	fi
}

assert_same() { # $1 = description, $2.. = values that must all be non-empty and equal
	tap_desc="$1"
	shift
	[ "$#" -ge 2 ] || {
		echo "assert_same: needs at least two values: $tap_desc" >&2
		exit 2
	}
	tap_status=0
	tap_first=''
	for tap_value in "$@"; do
		[ -n "$tap_value" ] || tap_status=1
		if [ -z "$tap_first" ]; then
			tap_first=$tap_value
		elif [ "$tap_value" != "$tap_first" ]; then
			tap_status=1
		fi
	done
	tap_check "$tap_desc" $tap_status
}

tap_sandbox() { # $1 = mktemp tag; sets $sandbox, cleans up on EXIT and signals
	sandbox=$(mktemp -d "${TMPDIR:-/tmp}/${1}.XXXXXX")
	trap 'rm -rf "$sandbox"' EXIT
	trap 'exit 1' HUP INT TERM
}

tap_done() { # plan + summary; exit status 0 only when nothing failed
	printf '%d..%d\n' 1 "$tap_num"
	printf '# pass %d, fail %d (of %d)\n' "$tap_passed" "$tap_failed" "$tap_num"
	[ "$tap_failed" -eq 0 ]
}
