#!/bin/sh
# The baseline images' digest pins have one home — scripts/baseline-env.sh
# (REL8-2081): every lane path reads it, no other file spells an image
# reference, and the pinned pair is the tagged-digest form so a stale pin is
# visible on read. The drift this guards is the real one: a second floating
# declaration (the Makefile's exported tags, the ceremony's own defaults)
# re-resolves the tag on every pull and splits the ADR-0071 class between
# lanes — which is how the memory lanes ended up on a different run image
# than the make-driven ones.
#
# The guard's three guarantees are three named steps: read the declared pair
# (validated), then the repo-wide one-home scan — scripts/baseline-env.sh is
# the one exemption, the home itself.
#
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)

. "$ROOT/scripts/tests/lib/tap.sh"
# The pin grammar's one home (scripts/lib/baseline-pin.sh), shared with the drift
# check and its test.
. "$ROOT/scripts/lib/baseline-pin.sh"

# Read the *declared* pin, validated: a caller's own BASELINE_*_IMAGE override
# must not change what this guard certifies, a read that fails is its own
# failure naming the pin, and only a name:tag@sha256:<64 hex> reference ever
# reaches the one-home scan (whose predicate is derived from the pair).
# The declared pin: the lib's read_pin (the one reader) inside an environment
# with the caller's own overrides unset, so this guard certifies what the home
# declares rather than whatever a local override resolved.
read_declared_pin() { # $1 = build|run
	env -u BASELINE_BUILD_IMAGE -u BASELINE_RUN_IMAGE \
		bash -c '. "$1/scripts/lib/baseline-pin.sh"; read_pin "$1/scripts/baseline-env.sh" "$2"' \
		_ "$ROOT" "$1"
}

build_ref=$(read_declared_pin build)
tap_check "the build pin reads and validates" $?
run_ref=$(read_declared_pin run)
tap_check "the run pin reads and validates" $?

# The one-home scan: no other tracked file may *spell* an image reference — a
# second literal is the drift source, whatever shape carries it (a bare shell
# default, the `${VAR:-…}` form, with or without export, a make assignment, a
# Dockerfile ARG default). Consumers that bind the name from the home (the
# ceremony's read) are the sanctioned use and do not match: the predicate
# requires an image repository right after the value's opening quote or
# `${VAR:-`. The accepted repositories are the ones the home pins — derived,
# not re-spelled, with ERE metacharacters escaped so a repository name is
# never pattern source — and the search's own status is a third state: no
# match (1) passes, any other non-zero means the search could not run.
check_one_home() { # $1/$2 = the validated build/run pins
	err_file="${TMPDIR:-/tmp}/baseline-image-pins-grep.err"
	repos=$(printf '%s\n%s\n' "$(pin_part "$1" repo)" "$(pin_part "$2" repo)" \
		| sort -u | sed 's/[][\.*^$+?(){}|]/\\&/g' | paste -sd'|' -)
	bindings=$(git -C "$ROOT" grep -n -E "^[[:space:]]*(export[[:space:]]+|ARG[[:space:]]+)?(BASELINE_BUILD_IMAGE|BASELINE_RUN_IMAGE)[[:space:]]*[:?]?=[[:space:]]*[\"']?(\$\{[A-Za-z_]+:-)?(${repos}):" -- \
		':!scripts/baseline-env.sh' 2>"$err_file")
	status=$?
	case "$status" in
		1)
			tap_ok "no other file binds the image names"
			;;
		0)
			tap_not_ok "second binding(s) of the image names: $(printf '%s' "$bindings" | tr '\n' '; ')"
			;;
		*)
			tap_not_ok "the one-home search could not run (git grep status $status): $(cat "$err_file" 2>/dev/null)"
			;;
	esac
}

if [ -n "$build_ref" ] && [ -n "$run_ref" ]; then
	check_one_home "$build_ref" "$run_ref"
else
	tap_skip "the one-home scan" "a pin did not read"
fi

tap_done
