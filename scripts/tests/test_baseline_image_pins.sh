#!/bin/sh
# The baseline images' digest pins have one home — scripts/baseline-env.sh
# (REL8-2081): every lane path reads it, no other file declares an image
# reference, and the pinned pair is the tagged-digest form so a stale pin is
# visible on read. The drift this guards is the real one: a second floating
# declaration (the Makefile's exported tags, the ceremony's own defaults)
# re-resolves the tag on every pull and splits the ADR-0071 class between
# lanes — which is how the memory lanes ended up on a different run image
# than the make-driven ones.
#
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)

. "$ROOT/scripts/tests/lib/tap.sh"

# Read the *declared* pins: a caller's own BASELINE_*_IMAGE override must not
# change what this guard certifies, and a read that fails is its own failure —
# never an empty value silently handed to the shape check.
read_pin() { # $1 = build|run
	env -u BASELINE_BUILD_IMAGE -u BASELINE_RUN_IMAGE \
		bash "$ROOT/scripts/baseline-env.sh" --print-image "$1"
}

build_ref=$(read_pin build) || {
	tap_not_ok "the pins home did not print the build pin"
	tap_done
	exit 1
}
run_ref=$(read_pin run) || {
	tap_not_ok "the pins home did not print the run pin"
	tap_done
	exit 1
}

check_ref() { # $1 = description, $2 = the reference
	if printf '%s\n' "$2" | grep -Eq '^[^@:]+:[^@]+@sha256:[0-9a-f]{64}$'; then
		tap_ok "$1 is name:tag@sha256:<64 hex>"
	else
		tap_not_ok "$1 is not name:tag@sha256:<64 hex>: $2"
	fi
}

check_ref "the build pin" "$build_ref"
check_ref "the run pin" "$run_ref"

# One home: no other tracked file may *spell* an image reference — a second
# literal is the drift source, whatever shape carries it (a bare shell
# default, the `${VAR:-…}` form, with or without export, a make assignment, a
# Dockerfile ARG default). Consumers that bind the name from the home (the
# ceremony's read, the drift check's) are the sanctioned use and do not match:
# the predicate requires an image repository right after the value's opening
# quote or `${VAR:-`. The search's own status is the third state: no match (1)
# passes, any other non-zero means the search could not run.
bindings=$(git -C "$ROOT" grep -n -E '^[[:space:]]*(export[[:space:]]+|ARG[[:space:]]+)?(BASELINE_BUILD_IMAGE|BASELINE_RUN_IMAGE)[[:space:]]*[:?]?=[[:space:]]*["'"'"']?(\$\{[A-Za-z_]+:-)?(rust|debian):' -- \
	':!scripts/baseline-env.sh' ':!scripts/tests' 2>/dev/null)
grep_status=$?
case "$grep_status" in
	1)
		tap_ok "no other file binds the image names"
		;;
	0)
		tap_not_ok "second binding(s) of the image names: $(printf '%s' "$bindings" | tr '\n' '; ')"
		;;
	*)
		tap_not_ok "the one-home search could not run (git grep status $grep_status)"
		;;
esac

tap_done
