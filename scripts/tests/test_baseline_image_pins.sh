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

# Read the *declared* pin, validated: a caller's own BASELINE_*_IMAGE override
# must not change what this guard certifies, a read that fails is its own
# failure naming the pin, and only a name:tag@sha256:<64 hex> reference ever
# reaches the one-home scan (whose predicate is derived from the pair).
read_pin() { # $1 = build|run; prints the validated reference or fails
	ref=$(env -u BASELINE_BUILD_IMAGE -u BASELINE_RUN_IMAGE \
		bash "$ROOT/scripts/baseline-env.sh" --print-image "$1") || return 1
	case "$ref" in
		""|*[[:space:]]*)
			printf 'baseline-image-pins: the %s pin is not one reference: %s\n' "$1" "$ref" >&2
			return 1
			;;
	esac
	if printf '%s\n' "$ref" | grep -Eq '^[^@:]+:[^@]+@sha256:[0-9a-f]{64}$'; then
		printf '%s\n' "$ref"
	else
		printf 'baseline-image-pins: the %s pin is not name:tag@sha256:<64 hex>: %s\n' \
			"$1" "$ref" >&2
		return 1
	fi
}

build_ref=$(read_pin build) || build_ref=""
run_ref=$(read_pin run) || run_ref=""
tap_check "the build pin reads and validates" "$([ -n "$build_ref" ] && echo 0 || echo 1)"
tap_check "the run pin reads and validates" "$([ -n "$run_ref" ] && echo 0 || echo 1)"

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
	repos=$(printf '%s\n%s\n' "$1" "$2" | sed 's/:.*//' | sort -u \
		| sed 's/[][\.*^$+?(){}|]/\\&/g' | paste -sd'|' -)
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
