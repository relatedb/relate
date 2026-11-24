#!/bin/sh
# scripts/baseline-image-drift.sh (REL8-2081): the drift check must file an
# issue exactly when a pinned digest differs from the tag's current digest,
# key that issue on the image (not the digest that moved, so a republish
# comments instead of filing a duplicate), refuse a malformed pair before any
# write, and never treat a moved tag as a failure of its own. curl, gh and the
# pins home are stubbed — no network, no issue writes.
#
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
DRIFT="$ROOT/scripts/baseline-image-drift.sh"

. "$ROOT/scripts/tests/lib/tap.sh"
# The pin grammar's one home, so the test derives the parts the way the script
# does rather than re-splitting the reference.
. "$ROOT/scripts/lib/baseline-pin.sh"

tap_sandbox relate-baseline-image-drift-test

stubbin="$sandbox/stub-bin"
gh_log="$sandbox/gh.log"
hub_table="$sandbox/hub-digests"
pins_sh="$sandbox/pins.sh"
mkdir -p "$stubbin"

# curl answers the tag its URL ends in, in the Hub API's shape, from the
# `tag digest` table. gh records its argv and answers the list/comment/create
# contract (GH_EXISTING = the open issue number, if any). pins.sh stands in
# for scripts/baseline-env.sh so a case can hand the script a bad pair.
cat >"$stubbin/curl" <<'EOF'
#!/bin/sh
url=""
for arg in "$@"; do url="$arg"; done
tag="${url##*/}"
awk -v tag="$tag" '$1 == tag { printf "{\"digest\": \"%s\"}\n", $2 }' "$HUB_DIGESTS"
EOF
cat >"$stubbin/gh" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$GH_LOG"
case "$1 $2" in
	"issue list")
		if [ -n "${GH_EXISTING:-}" ]; then printf '%s\n' "$GH_EXISTING"; fi
		;;
esac
exit 0
EOF
chmod +x "$stubbin/curl" "$stubbin/gh"

export HUB_DIGESTS="$hub_table" GH_LOG="$gh_log" PATH="$stubbin:$PATH"

build_ref=$(read_pin "$ROOT/scripts/baseline-env.sh" build)
run_ref=$(read_pin "$ROOT/scripts/baseline-env.sh" run)
run_name=$(pin_part "$run_ref" name)
build_tag=$(pin_part "$build_ref" tag)
run_tag=$(pin_part "$run_ref" tag)
build_digest=$(pin_part "$build_ref" digest)
run_digest=$(pin_part "$run_ref" digest)
moved_digest="sha256:$(printf 'c%.0s' $(seq 64))"

write_pins() { # $1 = build ref, $2 = run ref
	cat >"$pins_sh" <<EOF
#!/bin/sh
case "\$2" in
	build) printf '%s\n' '$1' ;;
	run) printf '%s\n' '$2' ;;
esac
EOF
	chmod +x "$pins_sh"
}

# invoke <hub table> [<open issue number>]: the one way the script under test
# is run — the fixture written, the log truncated, the environment seamed —
# leaving the captured output in $out and the status in $rc.
invoke() {
	printf '%s\n' "$1" >"$hub_table"
	: >"$gh_log"
	out=$(GH_EXISTING="${2:-}" BASELINE_ENV_SH="$pins_sh" bash "$DRIFT" 2>&1)
	rc=$?
}

run_case() { # $1 = description, $2 = hub table, $3 = open issue (optional)
	invoke "$2" "${3:-}"
	tap_check "$1: exits 0" "$rc"
}

log_dump() { tr '\n' ';' <"$gh_log"; }

assert_no_write() { # $1 = description: no issue write happened at all
	if grep -Eq -e "issue (create|comment)" "$gh_log"; then
		tap_not_ok "$1 ($(log_dump))"
	else
		tap_ok "$1"
	fi
}

assert_gh() { # $1 = description, $2 = required regex, $3 = forbidden regex
	# `-e` so a pattern that starts with `--` (a gh flag) is a pattern, not an
	# option — the silent misparse would pass every forbidden check.
	if grep -Eq -e "$2" "$gh_log" && ! grep -Eq -e "$3" "$gh_log"; then
		tap_ok "$1"
	else
		tap_not_ok "$1 ($(tr '\n' ';' <"$gh_log"))"
	fi
}

write_pins "$build_ref" "$run_ref"

run_case "a current pair" "${build_tag} ${build_digest}
${run_tag} ${run_digest}"
assert_no_write "a current pair files nothing"
printf '%s\n' "$out" | grep -q "is current"
tap_check "a current pair reports the pin" $?

run_case "a moved run pin" "${build_tag} ${build_digest}
${run_tag} ${moved_digest}"
assert_gh "a moved run pin files one issue" "issue create" "issue comment"
# The title carries the image, never the digest that moved: scope the
# forbidden pattern to the title's own argument (the body legitimately
# quotes both digests).
assert_gh "the issue is keyed on the image, not the digest" \
	"issue create --title Baseline image drift: ${run_name} " \
	"--title Baseline image drift: [^ ]*@sha256:"

run_case "a repeat drift" "${build_tag} ${build_digest}
${run_tag} ${moved_digest}" 77
assert_gh "a repeat drift comments on the open issue" "issue comment 77" "issue create"

# A fetch failure on the second image files nothing, even though the first
# image has drifted: the resolve pass completes before any write (REL8-2081).
invoke "${build_tag} ${moved_digest}"
[ "$rc" -ne 0 ]
tap_check "a failed fetch on the run image exits non-zero" $?
assert_no_write "a failed fetch on the run image files nothing (the build drift waits)"

# A malformed pair refuses before any write: the run pin is a floating tag.
write_pins "$build_ref" "debian:bookworm-slim"
invoke "${build_tag} ${build_digest}"
[ "$rc" -ne 0 ]
tap_check "a malformed pin refuses (non-zero)" $?
assert_no_write "a malformed pin writes nothing"
# The refusal comes from the grammar's one reader (scripts/lib/baseline-pin.sh)
# and names the offending reference.
printf '%s\n' "$out" | grep -q "is not name:tag@sha256"
tap_check "the refusal names the rule it broke" $?
printf '%s\n' "$out" | grep -q "debian:bookworm-slim"
tap_check "the refusal names the offending reference" $?

tap_done
