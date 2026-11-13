#!/bin/sh
# Hermetic test harness for scripts/stamp-swift-package.sh (ADR-0433 §6).
#
# The stamp script resolves the repository from its own location, so the
# harness copies it and the real Package.swift into a sandbox repo and stamps
# there; the working tree is never touched. Needs `swift` (macOS; runs in
# `make test-swift`).
#
# Output is TAP-ish: one `ok`/`not ok` line per assertion, then a summary.
# Exit status is 0 only if every assertion passed.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)

# The shared TAP harness (LABS-622): tap_check/tap_done, the counters,
# output format, and sandbox policy spelled once.
. "$ROOT/scripts/tests/lib/tap.sh"

# The suite needs swift (checksum computation + stamping); a host
# without it skips the whole suite under its own name instead of
# running on into misleading not-oks.
if ! command -v swift >/dev/null 2>&1; then
	tap_skip "SwiftPM stamp suite" "swift not available"
	tap_done
	exit 0
fi

tap_sandbox relate-stamp-test

repo="$sandbox/repo"
mkdir -p "$repo/scripts"
cp "$ROOT/scripts/stamp-swift-package.sh" "$repo/scripts/"
# The stamp edits through the shared scripts/lib/sedi.sh; the sandbox repo
# resolves its own lib, so the copy rides along.
cp -R "$ROOT/scripts/lib" "$repo/scripts/lib"
cp "$ROOT/Package.swift" "$repo/Package.swift"
cp "$repo/Package.swift" "$sandbox/original.swift"
stamp="$repo/scripts/stamp-swift-package.sh"

printf 'not a real framework\n' >"$sandbox/payload.txt"
zip="$sandbox/RelateFFI.xcframework.zip"
(cd "$sandbox" && zip -q "$zip" payload.txt)
expected="$(swift package compute-checksum "$zip")"

# 1. A valid stamp writes both constants and changes nothing else.
out="$(bash "$stamp" v1.2.3 "$zip" 2>&1)"; rc=$?
tap_check "stamps a valid tag" "$rc"
[ "$out" = "$expected" ]; tap_check "prints the SwiftPM checksum" $?
grep -qx 'let releaseTag = "v1.2.3"' "$repo/Package.swift"; tap_check "writes releaseTag" $?
grep -qx "let releaseChecksum = \"$expected\"" "$repo/Package.swift"; tap_check "writes releaseChecksum" $?
changed="$(diff "$sandbox/original.swift" "$repo/Package.swift" | grep -c '^>')"
[ "$changed" = 2 ]; tap_check "changes exactly the two constant lines" $?

# 2. Re-stamping (a re-dispatched release) overwrites in place.
tap_run_success "re-stamps a pre-release tag" bash "$stamp" v1.2.4-rc.1 "$zip"
grep -qx 'let releaseTag = "v1.2.4-rc.1"' "$repo/Package.swift"; tap_check "re-stamp replaces releaseTag" $?

# 3. Refusals leave the manifest untouched.
cp "$repo/Package.swift" "$sandbox/before.swift"
bash "$stamp" 1.2.3 "$zip" >/dev/null 2>&1; rc=$?
[ "$rc" = 2 ]; tap_check "rejects a tag without the v prefix (exit 2)" $?
bash "$stamp" v1.2.3 "$sandbox/missing.zip" >/dev/null 2>&1; rc=$?
[ "$rc" = 2 ]; tap_check "rejects a missing zip (exit 2)" $?
bash "$stamp" v1.2.3 >/dev/null 2>&1; rc=$?
[ "$rc" = 2 ]; tap_check "rejects a missing argument (exit 2)" $?
cmp -s "$sandbox/before.swift" "$repo/Package.swift"; tap_check "refusals leave Package.swift unchanged" $?

# 4. A manifest whose constant is not declared exactly once is refused.
printf 'let releaseTag = ""\n' >>"$repo/Package.swift"
cp "$repo/Package.swift" "$sandbox/dup.swift"
bash "$stamp" v1.2.3 "$zip" >/dev/null 2>&1; rc=$?
[ "$rc" = 1 ]; tap_check "rejects a duplicated releaseTag (exit 1)" $?
cmp -s "$sandbox/dup.swift" "$repo/Package.swift"; tap_check "duplicate refusal leaves Package.swift unchanged" $?

tap_done
