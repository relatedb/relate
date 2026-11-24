#!/usr/bin/env bash
# Check the pinned baseline images against Docker Hub and file an issue when
# a tag has moved (REL8-2081).
#
# Why a cadence and not a gate: the pins in scripts/baseline-env.sh keep the
# ADR-0071 class stable, so a moved upstream tag is not a failure — the lanes
# still resolve the pinned bytes. What it is: a security signal. Debian
# republishes bookworm-slim every 1-3 weeks (8 repushes in the 16 weeks to
# 2026-11-24, all security-driven), and a pin left alone accumulates CVEs.
#
# One issue per image: the title names the image (`repo:tag`, which does not
# move), the body carries the pinned and current digests — so a republish
# comments on the existing issue instead of filing a new one, and the
# `in:title` search is stable across digests.
#
# Usage: scripts/baseline-image-drift.sh
#   No flags: the repo comes from the `gh` context (the checkout's remote, or
#   GITHUB_REPOSITORY in CI), so a malformed invocation cannot silently file
#   somewhere else. Files via the `gh` CLI (GH_TOKEN or an authenticated gh).
#   Exits 0 whether or not a tag moved — the issue is the signal, not the
#   status; an unusable pin is a refusal (exit 1) before any write.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
. "${ROOT}/scripts/lib/baseline-pin.sh"

# The pins home, with a seam so a test can hand this script a bad pair.
PINS_SH="${BASELINE_ENV_SH:-${ROOT}/scripts/baseline-env.sh}"

# Read and validate in one walk, and both before the loop that writes: a
# malformed run pin must not be discovered after the build pin has already
# filed an issue.
pins=()
for key in build run; do
	pins+=("$(read_pin "$PINS_SH" "$key")")
done

# current_digest <repo> <tag>: the digest Docker Hub serves for that tag,
# validated at this boundary — the answer is untrusted input, and a null or
# non-digest value must stop the run, not become `current` and reach an issue.
current_digest() {
	local served
	served=$(curl -fsSL "https://hub.docker.com/v2/repositories/library/${1}/tags/${2}" \
		| "${PYTHON:-python3}" -c '
import json, sys
digest = json.load(sys.stdin).get("digest")
if not isinstance(digest, str):
    sys.exit(f"baseline-image-drift: {digest!r} is not a digest")
print(digest)') || return 1
	digest_valid "$served" || {
		printf 'baseline-image-drift: %s is not sha256:<64 hex>\n' "$served" >&2
		return 1
	}
	printf '%s\n' "$served"
}

# upsert_issue <name> <pinned> <current>: one issue per image — the title
# names the image, so a republish comments instead of filing a duplicate.
upsert_issue() { # $1 = the pinned reference, $2 = the digest Docker Hub serves
	name="$(pin_part "$1" name)"
	pinned="$(pin_part "$1" digest)"
	current="$2"
	title="Baseline image drift: ${name}"
	body=$(
		printf '%s\n' \
			"The pinned baseline image \`${name}\` no longer matches Docker Hub." \
			"" \
			"- pinned:  \`${pinned}\`" \
			"- current: \`${current}\`" \
			"" \
			"Upstream republishes are security-driven, so this is a re-pin" \
			"candidate: edit \`scripts/baseline-env.sh\` (the one home — keep the" \
			"dated tag form), dispatch \`create-loadtest-baselines\`, and land both" \
			"so the envelopes and the pins describe one environment" \
			"(\`docs/testing/cadences.md\`)."
	)
	existing=$(gh issue list --state open --search "in:title \"${title}\"" \
		--json number --jq '.[0].number // empty')
	if [ -n "$existing" ]; then
		gh issue comment "$existing" --body "$body"
		echo "baseline-image-drift: ${name} moved to ${current}; updated issue #${existing}"
	else
		gh issue create --title "$title" --body "$body"
		echo "baseline-image-drift: ${name} moved to ${current}; filed ${title}"
	fi
}

# Resolve and fetch every pin before anything is written: a failure on the
# second image (a network error, or gh refusing the first write) must not
# leave the first image's issue already filed.
drifted=""
for ref in "${pins[@]}"; do
	pinned="$(pin_part "$ref" digest)"
	current="$(current_digest "$(pin_part "$ref" repo)" "$(pin_part "$ref" tag)")"
	if [ "$current" = "$pinned" ]; then
		echo "baseline-image-drift: $(pin_part "$ref" name) is current (${pinned})"
		continue
	fi
	drifted="${drifted}${ref} ${current}
"
done

# The write pass sees only validated references and fetched digests.
printf '%s' "$drifted" | while read -r ref current; do
	[ -n "$ref" ] || continue
	upsert_issue "$ref" "$current"
done
