#!/usr/bin/env bash
# Compute the next release tag from existing `v*` git tags.
# Usage: release-next-version.sh patch|minor|major
# Prints exactly one line to stdout: the next tag (e.g. v0.4.1).
# With no prior tags, the seed is v0.1.0 regardless of bump (the first
# release cut from this repository; the cypherlite-era tags never came
# across, so the line starts here).
set -euo pipefail

usage() {
	echo "usage: $0 patch|minor|major" >&2
	exit 2
}

bump="${1:-}"
case "$bump" in
patch | minor | major) ;;
*) usage ;;
esac

latest="$(git tag --list 'v*' --sort=-v:refname | head -n1)"

if [[ -z "$latest" ]]; then
	next="v0.1.0"
else
	ver="${latest#v}"
	IFS='.' read -r major minor patch <<<"$ver"
	case "$bump" in
	major)
		major=$((major + 1))
		minor=0
		patch=0
		;;
	minor)
		minor=$((minor + 1))
		patch=0
		;;
	patch)
		patch=$((patch + 1))
		;;
	esac
	next="v${major}.${minor}.${patch}"
fi

if git rev-parse -q --verify "refs/tags/${next}" >/dev/null 2>&1; then
	echo "error: tag ${next} already exists" >&2
	exit 1
fi

echo "$next"
