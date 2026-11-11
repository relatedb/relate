#!/bin/sh
# relate installer:
#   curl -sSfL https://relatedb.ai/install.sh | bash
set -eu

REPO=relatedb/relate
ASSET_PREFIX=relate
BIN_NAME=relate
INSTALL_DIR="$HOME/.local/bin"

# Pinned release signing public key (ADR-0115). Verification never fetches the
# key from the release. The signer identity and the SSH signature namespace are
# both `relate-release`; changing either, or this key, requires a new ADR.
SIGNING_KEY='ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHgv/GHY5mtv/HopH/J/T7KrTT7eOVFEG2ueT4iRKwbK relate-release'

die() {
	echo "install.sh: $1" >&2
	exit 1
}

# The ADR-0457 signed-manifest version header. The prefix is byte-identical
# to `upgrade_cmd/manifest.rs`'s SUMS_VERSION_PREFIX and is matched literally
# (quoted expansion) in both spots below; the harness's section-0 guards hold
# the three copies together.
SUMS_VERSION_PREFIX='# relate-release-version: '

# The last release published before the signed SHA256SUMS version header
# existed (ADR-0457). A headerless manifest is accepted only at or below it.
LEGACY_MANIFEST_CEILING=0.14.0

# The release-metadata URL (ADR-0250 §1): the static releases.json on the
# project file host, the same document the CLI's upgrade reads.
# RELATE_RELEASES_JSON_URL overrides it at install time (the same dev/test
# seam); unset or empty means this canonical URL. The literal is pinned to
# releases_json.rs's RELEASES_JSON_URL constant by the harness's section-0
# guard.
RELEASES_JSON_URL='https://relatedb.ai/releases.json'

# True when the X.Y.Z version $1 is greater than $2. Unparseable input counts
# as greater, so the caller takes the fail-closed branch.
version_gt() {
	awk -v a="$1" -v b="$2" 'BEGIN {
		na = split(a, A, "."); nb = split(b, B, ".");
		if (na != 3 || nb != 3) exit 0;
		for (i = 1; i <= 3; i++) {
			if (A[i] !~ /^[0-9]+$/ || B[i] !~ /^[0-9]+$/) exit 0;
			if (A[i] + 0 > B[i] + 0) exit 0;
			if (A[i] + 0 < B[i] + 0) exit 1;
		}
		exit 1;
	}'
}

case "$(uname -s)" in
Darwin) os=darwin ;;
Linux) os=linux ;;
*)
	die "unsupported OS: $(uname -s). Supported: Darwin, Linux."
	;;
esac

case "$(uname -m)" in
x86_64 | amd64) arch=amd64 ;;
arm64 | aarch64) arch=arm64 ;;
*)
	die "unsupported architecture: $(uname -m). Supported: amd64, arm64."
	;;
esac

command -v curl >/dev/null 2>&1 ||
	die "curl is required. Install it and retry."

command -v ssh-keygen >/dev/null 2>&1 ||
	die "ssh-keygen (openssh-client) is required to verify release signatures; install openssh-client and retry."

tmp=$(mktemp -d)
tmp_out=""
trap 'rm -rf "$tmp"; [ -z "$tmp_out" ] || rm -f "$tmp_out"' EXIT

# curl|sh: no script path on disk, so all state lives in $tmp.
# api_get is the single place a GitHub API request is composed: the
# repos/$REPO URL base — one spelling, serving both the per-tag release
# fetch below and every asset download — and the Authorization header,
# whose bearer token is sent only when GITHUB_TOKEN is set. $1 is the
# API path under repos/$REPO; $2, if given, is one extra header (the
# asset-download Accept override).
api_get() {
	if [ -n "${GITHUB_TOKEN:-}" ]; then
		auth="Authorization: Bearer $GITHUB_TOKEN"
	else
		auth=""
	fi
	curl -fsSL ${auth:+-H "$auth"} ${2:+-H "$2"} \
		"https://api.github.com/repos/$REPO$1"
}

# Download a release asset by its API asset id, asking for the raw bytes with
# Accept: application/octet-stream. The browser download URL does not honor
# tokens for internal repos (it 404s), and the API endpoint also works
# unauthenticated on public repos, so this single endpoint serves both cases.
download_asset() {
	api_get "/releases/assets/$1" "Accept: application/octet-stream"
}

# Fragile-but-accepted dependency-free parse of the release JSON from the API.
# Each asset object is serialized with its "url" (ending in
# /releases/assets/<id>) and its "name" on nearby lines, so derive the asset
# id from the most recent asset "url" and print it when the wanted name
# appears. The JSON "id" field is never read.
# A parse that yields nothing is a hard failure for the callers below, never a
# fallback to a guessed URL.
asset_id_for() {
	awk -v want="$1" '
		match($0, /\/releases\/assets\/[0-9]+/) {
			s = substr($0, RSTART, RLENGTH)
			sub(/.*\//, "", s)
			id = s
		}
		match($0, /"name"[[:space:]]*:[[:space:]]*"[^"]*"/) {
			s = substr($0, RSTART, RLENGTH)
			sub(/.*"name"[[:space:]]*:[[:space:]]*"/, "", s)
			sub(/"$/, "", s)
			if (s == want) { print id; exit }
		}
	'
}

# Raw binaries, no archive concatenation: relate publishes
# relate-<os>-<arch>.
asset="${ASSET_PREFIX}-${os}-${arch}"

# Dependency-free parse of the releases.json document (ADR-0250 §2),
# reduced to the one field the installer consumes: the latest release's
# tag. The publisher serializes the document with jq's default
# pretty-print (one key per line, two-space indents), so the top-level
# `latest` value is either null on the `"latest":` line or an object
# whose keys are the four-space-indented lines before its closing
# brace; a deeper-indented "tag" (the prerelease slot, the releases
# window) never matches. The parser owns its three outcomes, so the
# caller never matches a sentinel value: exit 0 prints the latest.tag
# on stdout, validated as vX.Y.Z; exit 2 = latest is null; exit 3 = no
# readable latest.tag; exit 4 = a latest.tag outside the vX.Y.Z shape,
# printed on stdout for the caller's message. No outcome ever falls
# back to a guessed tag.
latest_tag_of() {
	awk '
		BEGIN { status = 3 }
		!latest && /^  "latest"[[:space:]]*:[[:space:]]*null/ {
			status = 2
			exit
		}
		!latest && /^  "latest"[[:space:]]*:[[:space:]]*\{/ {
			latest = 1
			next
		}
		latest && /^  \}/ { exit }
		latest && match($0, /^    "tag"[[:space:]]*:[[:space:]]*"[^"]*"/) {
			s = substr($0, RSTART, RLENGTH)
			sub(/^    "tag"[[:space:]]*:[[:space:]]*"/, "", s)
			sub(/"$/, "", s)
			if (s != "") {
				if (s ~ /^v[0-9]+\.[0-9]+\.[0-9]+$/) {
					status = 0
				} else {
					status = 4
				}
				print s
				exit
			}
		}
		END { exit status }
	'
}

# The release tag comes from releases.json — the same document the
# CLI's upgrade resolves through (ADR-0250) — never from the GitHub
# API's latest-release endpoint: that endpoint returns the newest full
# release of any name, and the extension channel publishes its own
# releases into this repository (relate-extensions-<name>-<X.Y.Z>,
# ADR-0431), so after an extension release the endpoint names no
# product release at all. releases.json's latest is the newest stable
# core release by construction (ADR-0250 §2, §6), so it is the one
# source that cannot select an extension release. The request carries
# no Authorization header: the file is public and lives on a different
# origin than the GitHub API and the asset downloads (ADR-0250 §5).
metadata_url=${RELATE_RELEASES_JSON_URL:-$RELEASES_JSON_URL}
metadata_fail="could not read the release list from $metadata_url"
releases_doc=$(curl -fsSL "$metadata_url" || true)
[ -n "$releases_doc" ] ||
	die "$metadata_fail: the fetch failed"

# latest_tag_of owns the outcomes (its comment above): $tag is assigned
# only the validated vX.Y.Z spelling, and each failure status maps to
# its authored message here.
latest_status=0
latest_tag=$(printf '%s\n' "$releases_doc" | latest_tag_of) ||
	latest_status=$?
case "$latest_status" in
0) tag=$latest_tag ;;
2) die "$metadata_fail: latest is null: no stable release is published" ;;
4) die "$metadata_fail: latest.tag '$latest_tag' is not a vX.Y.Z release tag" ;;
*) die "$metadata_fail: the document is malformed: no latest.tag" ;;
esac

echo "Installing relate $tag ($os/$arch) to $INSTALL_DIR"

# The repository is internal, so an unauthenticated request 404s. Fetch
# the release JSON for the exact tag releases.json named — it carries
# the asset ids — and require its tag_name to equal that tag: the API
# must describe exactly the release asked for, never a neighboring
# one. GITHUB_TOKEN is the one credential that helps here.
release_json=$(api_get "/releases/tags/$tag" || true)
[ -n "$release_json" ] ||
	die "could not resolve release $tag for $REPO. If the repo is private, set GITHUB_TOKEN."
release_tag=$(printf '%s\n' "$release_json" |
	sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' |
	head -n 1)
[ -n "$release_tag" ] ||
	die "the $REPO API returned no tag_name for tag $tag; installing nothing"
[ "$release_tag" = "$tag" ] ||
	die "the $REPO API served release $release_tag for tag $tag; installing nothing"

# Resolve the asset ids (binary, manifest, signature) from the release JSON we
# already have, and die on an empty id rather than downloading the wrong thing.
asset_id=$(printf '%s\n' "$release_json" | asset_id_for "$asset")
sums_id=$(printf '%s\n' "$release_json" | asset_id_for SHA256SUMS)
sig_id=$(printf '%s\n' "$release_json" | asset_id_for SHA256SUMS.sig)
[ -n "$asset_id" ] ||
	die "release $tag from $REPO API has no asset named $asset"
[ -n "$sums_id" ] ||
	die "release $tag from $REPO API has no asset named SHA256SUMS"
[ -n "$sig_id" ] ||
	die "release $tag from $REPO API has no asset named SHA256SUMS.sig"
# Cheap first: the manifest and its signature are small; the binary is the
# large download and must not start until the manifest is authenticated.
download_asset "$sums_id" >"$tmp/SHA256SUMS" ||
	die "failed to download SHA256SUMS via API asset $sums_id"
download_asset "$sig_id" >"$tmp/SHA256SUMS.sig" ||
	die "failed to download SHA256SUMS.sig via API asset $sig_id"

# Authenticate the manifest before trusting any checksum it carries (ADR-0115).
# The pinned key is written in the identity form ssh-keygen's allowed_signers
# expects, and SHA256SUMS is fed on stdin. Fail closed: a missing, invalid or
# wrong-key signature installs nothing — and never downloads the binary.
printf 'relate-release %s\n' "$SIGNING_KEY" >"$tmp/allowed_signers"
ssh-keygen -Y verify -f "$tmp/allowed_signers" -I relate-release \
	-n relate-release -s "$tmp/SHA256SUMS.sig" <"$tmp/SHA256SUMS" ||
	die "SHA256SUMS signature verification failed for $tag; installing nothing"

# Bind the authenticated manifest to the release the API resolved (ADR-0457):
# the version header is inside the signed bytes, so a validly signed older
# manifest cannot be replayed for a newer tag. The prefix comes from the one
# constant above, whose literal the Rust side also pins: any other spelling —
# no space after the colon, a tab, a later line — is a headerless legacy
# manifest. A present-but-empty header is a publish bug and fails hard.
first_line=$(head -n 1 "$tmp/SHA256SUMS")
case "$first_line" in
"$SUMS_VERSION_PREFIX"*)
	sums_version=${first_line#"$SUMS_VERSION_PREFIX"}
	sums_version=$(printf '%s\n' "$sums_version" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')
	[ -n "$sums_version" ] ||
		die "SHA256SUMS from $tag carries an empty version header; installing nothing"
	[ "$sums_version" = "$tag" ] ||
		die "SHA256SUMS is for $sums_version, not the requested release $tag; installing nothing"
	;;
*)
	if version_gt "${tag#v}" "$LEGACY_MANIFEST_CEILING"; then
		die "SHA256SUMS from $tag has no signed version header; refusing to install a manifest whose version is not authenticated (legacy manifests are accepted only at or below v$LEGACY_MANIFEST_CEILING); installing nothing"
	else
		echo "install.sh: warning: SHA256SUMS from $tag has no signed version header (legacy manifest, accepted only at or below v$LEGACY_MANIFEST_CEILING)" >&2
	fi
	;;
esac

# The manifest is authenticated; only now is the binary download worth
# paying for.
download_asset "$asset_id" >"$tmp/$asset" ||
	die "failed to download $asset via API asset $asset_id"

# SHA256SUMS lines are "<64-hex>  <name>" (shasum -a 256, two spaces, no
# binary-mode asterisk).
expected=$(grep -E "^[0-9a-fA-F]{64}[[:space:]]+\*?${asset}\$" "$tmp/SHA256SUMS" |
	head -n 1 | awk '{print $1}')
[ -n "$expected" ] ||
	die "SHA256SUMS from $tag has no entry for $asset"

printf '%s  %s\n' "$expected" "$asset" | (
	cd "$tmp" &&
		if command -v sha256sum >/dev/null 2>&1; then
			sha256sum -c -
		else
			shasum -a 256 -c -
		fi
) || die "checksum verification failed for $asset; installing nothing"

# Atomic install: stage the verified binary at a temp name inside INSTALL_DIR
# (same filesystem) and rename it into place, so an interrupted install never
# leaves a partial binary at the final path. The EXIT trap removes the temp
# name on failure.
mkdir -p "$INSTALL_DIR"
tmp_out=$(mktemp "$INSTALL_DIR/.$BIN_NAME.XXXXXX")
cp "$tmp/$asset" "$tmp_out"
chmod +x "$tmp_out"
mv "$tmp_out" "$INSTALL_DIR/$BIN_NAME"
tmp_out=""

case ":$PATH:" in
*":$INSTALL_DIR:"*) ;;
*)
	echo "NOTE: $INSTALL_DIR is not in your PATH." >&2
	echo "Add it to your shell profile, e.g.:" >&2
	echo "  export PATH=\"$INSTALL_DIR:\$PATH\"" >&2
	;;
esac

echo "Installed relate $tag at $INSTALL_DIR/$BIN_NAME"
