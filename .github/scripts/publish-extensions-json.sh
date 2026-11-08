#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C.UTF-8

# ============================================================
# publish-extensions-json.sh
#
# Generates and signs the extension channel index served at
# https://relatedb.ai/extensions.json — the
# signed catalog `relate extension install` (default source)
# and `extension list --available` read (ADR-0431 §1: the index
# sits beside releases.json at the website root, written only by
# the release pipeline, never hand-edited; ADR-0250's rules carry
# over). Writes two files: the document and its SSHSIG signature
# `extensions.json.sig` (namespace relate-release, the one
# release signing key that also signs SHA256SUMS — ADR-0115;
# first-party is the release channel and its one signing root).
#
# The document is a FULL SNAPSHOT recomputed from the live GitHub
# release list on every run, never an append log (the ADR-0250 §6
# publisher contract): one row per extension package, one entry
# per published version, one asset row per platform tarball.
#
# The conventions the publisher itself created (the release
# workflow's release-extensions.yml cuts exactly these tags and
# assets; anything else on the channel is refused, never silently
# dropped):
#
#   - a package release is the tag
#     `relate-extensions-<name>-<X.Y.Z>` — published,
#     non-draft, non-prerelease (drafts and prereleases are
#     skipped before the convention checks, the releases.json
#     stable-only posture); a PUBLISHED tag claiming the
#     `relate-extensions-` prefix that does not parse as
#     `<name>-<strict X.Y.Z triple>` aborts the publish.
#   - every asset on a package release is the per-platform
#     tarball `<name>-<X.Y.Z>-<os>-<arch>.tar.gz` with
#     os/arch in the channel's platform scope (darwin/linux,
#     amd64/arm64 — ADR-0068); any other asset name aborts.
#     The one exemption is the channel's own output: the signed
#     index pair extensions.json + extensions.json.sig rides the
#     extension release as assets (ADR-0546) and is skipped, never
#     indexed.
#   - the tarball is the ADR-0431 §2 artifact; its extension.json
#     must name the same name/version the tag carries, and its
#     protocol string (the MCP revision, ADR-0447) becomes the index
#     row's `protocol` — all assets of one release must agree.
#
# Run this AFTER the extension release's assets are complete (the
# workflow chains it from create-release with `needs`).
#
# The release list and each release's assets are read with the
# `gh` CLI (`gh release list`, then `gh release view` and
# `gh release download` per package release — the tarballs are
# small; each is downloaded once for its SHA256 and its manifest).
# `gh` reads GH_TOKEN (or GITHUB_TOKEN) and GH_REPO from the
# environment; CI sets both.
#
# Usage: publish-extensions-json.sh OUTPUT_JSON OUTPUT_SIG
#
# Env:
#   RELEASE_SIGNING_PRIVATE_KEY
#     The release signing key (OpenSSH private key form). The
#     script writes it to a 0600 temp file, signs, and removes it —
#     the create-release SHA256SUMS signing shape. Required: an
#     unsigned index is not a publishable artifact.
#   PUBLISH_EXTENSIONS_JSON_GENERATED_AT
#     Test seam only: pins `generated_at` to a fixed RFC 3339 UTC
#     (Z) timestamp so the bats fixture comparison is deterministic.
#     Unset in CI, where the wall clock is used.
#
# Exit status: 0 = both outputs written; 1 = refused, with a
# `::error::` diagnostic on stderr and both outputs untouched
# (the document is built, validated, and signed in a temp dir
# first).
# ============================================================

# `gh release list` returns releases newest-created first; the
# newest 100 cover the whole channel comfortably, and the script
# assembles its own order regardless.
LIST_LIMIT=100

# The conventions above, as the patterns the checks share (bash
# regex; see the `[[ =~ ]]` sites). A package name is the CLI's
# name vocabulary; a version is a strict X.Y.Z triple — a
# prerelease suffix never parses (prereleases are skipped by flag
# before the convention checks, never by string shape).
TAG_PREFIX='relate-extensions-'
NAME_RX='[a-z0-9][a-z0-9-]*'
TRIPLE_RX='[0-9]+\.[0-9]+\.[0-9]+'
TAG_RX="^${TAG_PREFIX}(${NAME_RX})-(${TRIPLE_RX})\$"
ASSET_RX="^(${NAME_RX})-(${TRIPLE_RX})-(darwin|linux)-(amd64|arm64)\.tar\.gz\$"

if [ "$#" -ne 2 ] || [ -z "${1:-}" ] || [ -z "${2:-}" ]; then
  echo "::error::Usage: publish-extensions-json.sh OUTPUT_JSON OUTPUT_SIG"
  exit 1
fi
OUTPUT_JSON="$1"
OUTPUT_SIG="$2"

for tool in gh jq ssh-keygen tar shasum; do
  if ! command -v "$tool" > /dev/null 2>&1; then
    echo "::error::${tool} not found on PATH; refusing to publish extensions.json"
    exit 1
  fi
done

generated_at="${PUBLISH_EXTENSIONS_JSON_GENERATED_AT:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}"
if ! printf '%s\n' "$generated_at" | grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$'; then
  echo "::error::generated_at '${generated_at}' is not an RFC 3339 UTC (Z) timestamp; refusing to publish extensions.json"
  exit 1
fi

if [ -z "${RELEASE_SIGNING_PRIVATE_KEY:-}" ]; then
  echo "::error::RELEASE_SIGNING_PRIVATE_KEY is not set; the channel index is signed or not published"
  exit 1
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

# --- 1. Release list: the channel's package releases ---------------
if ! gh release list --limit "${LIST_LIMIT}" \
  --json tagName,isDraft,isPrerelease > "${TMP_DIR}/list.json"; then
  echo "::error::gh release list failed; refusing to publish an incomplete extensions.json"
  exit 1
fi

# Published (non-draft), non-prerelease, and the channel's package
# tag. Drafts and prereleases skip before the convention checks —
# the releases.json stable-only posture carried over.
jq -r '
  [ .[] | select((.isDraft | not) and (.isPrerelease | not)) ]
  | .[].tagName
' "${TMP_DIR}/list.json" > "${TMP_DIR}/published-tags.txt"

: > "${TMP_DIR}/package-releases.jsonl"
while IFS= read -r tag; do
  [ -n "$tag" ] || continue
  if [[ ! "$tag" =~ ^${TAG_PREFIX} ]]; then
    continue # not a package release (e.g. a CLI release tag)
  fi
  if [[ ! "$tag" =~ ${TAG_RX} ]]; then
    echo "::error::tag '${tag}' claims the ${TAG_PREFIX} prefix but does not follow the package-release convention ${TAG_PREFIX}<name>-<X.Y.Z>; refusing to publish extensions.json"
    exit 1
  fi
  pkg_name="${BASH_REMATCH[1]}"
  pkg_version="${BASH_REMATCH[2]}"

  # --- 2. One package release: its platform tarballs --------------
  if ! gh release view "$tag" --json tagName,assets > "${TMP_DIR}/view.json"; then
    echo "::error::gh release view '${tag}' failed; refusing to publish an incomplete extensions.json"
    exit 1
  fi
  jq -r '.assets[].name' "${TMP_DIR}/view.json" > "${TMP_DIR}/assets.txt"
  if [ ! -s "${TMP_DIR}/assets.txt" ]; then
    echo "::error::release '${tag}' carries no platform tarballs; refusing to publish extensions.json"
    exit 1
  fi

  : > "${TMP_DIR}/asset-rows.jsonl"
  manifest_protocol=""
  while IFS= read -r asset; do
    [ -n "$asset" ] || continue
    # The channel's own index files ride the extension release as assets
    # (ADR-0546): they are this publisher's output, not a platform
    # tarball, so they are skipped — never indexed, never an abort.
    if [ "$asset" = "extensions.json" ] || [ "$asset" = "extensions.json.sig" ]; then
      continue
    fi
    if [[ ! "$asset" =~ ${ASSET_RX} ]]; then
      echo "::error::asset '${asset}' on release '${tag}' does not follow the channel convention <name>-<X.Y.Z>-<os>-<arch>.tar.gz; refusing to publish extensions.json"
      exit 1
    fi
    a_name="${BASH_REMATCH[1]}"
    a_version="${BASH_REMATCH[2]}"
    a_os="${BASH_REMATCH[3]}"
    a_arch="${BASH_REMATCH[4]}"
    if [ "$a_name" != "$pkg_name" ] || [ "$a_version" != "$pkg_version" ]; then
      echo "::error::asset '${asset}' on release '${tag}' does not name the release's package and version; refusing to publish extensions.json"
      exit 1
    fi

    # The tarball is small: download it once for its SHA256 (the
    # signed row's checksum) and its manifest (the protocol column).
    # `--clobber`: every asset of the loop lands on this one path, and
    # gh refuses to overwrite an existing output file without it — the
    # channel's first publish died on exactly that refusal at the
    # second asset.
    if ! gh release download "$tag" --pattern "$asset" \
      --clobber --output "${TMP_DIR}/asset.tar.gz"; then
      echo "::error::gh release download '${asset}' from '${tag}' failed; refusing to publish an incomplete extensions.json"
      exit 1
    fi
    sha256="$(shasum -a 256 "${TMP_DIR}/asset.tar.gz" | cut -d' ' -f1)"
    if ! manifest="$(tar -xzOf "${TMP_DIR}/asset.tar.gz" extension.json)"; then
      echo "::error::asset '${asset}' on release '${tag}' does not carry a readable extension.json; refusing to publish extensions.json"
      exit 1
    fi
    # One read of the three fields the index needs from the manifest,
    # fail-closed on any shape but the package manifest's (a name, a
    # version and a protocol string): the convention names carry no
    # spaces, so the single line round-trips.
    if ! manifest_fields="$(jq -r 'if ((.name | type) == "string") and ((.version | type) == "string") and ((.protocol | type) == "string") then "\(.name) \(.version) \(.protocol)" else empty end' <<<"$manifest")" \
      || [ -z "${manifest_fields}" ]; then
      echo "::error::asset '${asset}' on release '${tag}' carries a manifest that is not the package manifest shape (name, version, protocol); refusing to publish extensions.json"
      exit 1
    fi
    read -r m_name m_version m_protocol <<<"${manifest_fields}"
    if [ "$m_name" != "$pkg_name" ] || [ "$m_version" != "$pkg_version" ]; then
      echo "::error::asset '${asset}' on release '${tag}' carries a manifest naming '${m_name}' ${m_version}, not the release's '${pkg_name}' ${pkg_version}; refusing to publish extensions.json"
      exit 1
    fi
    if [ -z "$m_protocol" ]; then
      echo "::error::asset '${asset}' on release '${tag}' carries a manifest whose protocol is empty; refusing to publish extensions.json"
      exit 1
    fi
    if [ -n "$manifest_protocol" ] && [ "$m_protocol" != "$manifest_protocol" ]; then
      echo "::error::the assets of release '${tag}' disagree on the manifest protocol (${manifest_protocol} vs ${m_protocol}); refusing to publish extensions.json"
      exit 1
    fi
    manifest_protocol="$m_protocol"

    jq -c -n \
      --arg os "$a_os" \
      --arg arch "$a_arch" \
      --arg tag "$tag" \
      --arg asset "$asset" \
      --arg sha256 "$sha256" \
      '{os: $os, arch: $arch, tag: $tag, asset: $asset, sha256: $sha256}' \
      >> "${TMP_DIR}/asset-rows.jsonl"
  done < "${TMP_DIR}/assets.txt"

  jq -c -n \
    --arg name "$pkg_name" \
    --arg version "$pkg_version" \
    --arg protocol "$manifest_protocol" \
    --slurpfile assets "${TMP_DIR}/asset-rows.jsonl" \
    '{name: $name, version: $version, protocol: $protocol, assets: $assets}' \
    >> "${TMP_DIR}/package-releases.jsonl"
done < "${TMP_DIR}/published-tags.txt"

# --- 3. Assemble the document (schema key order) --------------------
# Packages sorted by name; versions newest first (the publisher
# contract the default install relies on: versions[0] is the
# newest); assets in the platform table's order. `hidden` is
# spelled explicitly (the schema's default, one policy for every
# generated row).
jq -n \
  --arg generated_at "${generated_at}" \
  --slurpfile releases "${TMP_DIR}/package-releases.jsonl" \
  '{
    schema_version: 1,
    generated_at: $generated_at,
    packages: (
      [$releases[]]
      | sort_by(.name)
      | group_by(.name)
      | map({
          name: .[0].name,
          hidden: false,
          versions: (
            sort_by(.version | split(".") | map(tonumber))
            | reverse
            | map({
                version: .version,
                protocol: .protocol,
                assets: (.assets | sort_by(.os, .arch))
              })
          )
        })
    )
  }' > "${TMP_DIR}/extensions.json"

# --- 4. Validate before it can ship --------------------------------
# Every value the CLI's decode would refuse is refused here first
# (the two sides of the contract are pinned together by the
# skeleton check and the CLI's fixture test), and every tag and
# asset name must resolve through the release download URL
# template — the releases.json §4 validation carried over.
if ! jq -e '
  all(.packages[];
      (.name | test("^[a-z0-9][a-z0-9-]*$"))
      and (.versions | length >= 1)
      and all(.versions[];
          (.protocol | type) == "string"
          and (.assets | length >= 1)
          and all(.assets[];
              (.os | test("^(darwin|linux)$"))
              and (.arch | test("^(amd64|arm64)$"))
              and (.tag | test("^[^/[:space:]]+$"))
              and (.asset | test("^[^/[:space:]]+$"))
              and (.sha256 | test("^[a-f0-9]{64}$")))))
' "${TMP_DIR}/extensions.json" > /dev/null; then
  echo "::error::the assembled extensions.json fails the channel schema; refusing to publish"
  exit 1
fi

# --- 5. Sign (the one release signing key, ADR-0115) ---------------
umask 077
keyfile="$(mktemp "${TMP_DIR}/signing-key.XXXXXX")"
signing_copy="$(mktemp "${TMP_DIR}/extensions-json.XXXXXX")"
printf '%s\n' "$RELEASE_SIGNING_PRIVATE_KEY" > "$keyfile"
chmod 600 "$keyfile"
cp "${TMP_DIR}/extensions.json" "$signing_copy"
# ssh-keygen writes the signature beside the file it signs; the
# copy keeps the output path the script's own two arguments own.
if ! ssh-keygen -Y sign -f "$keyfile" -n relate-release "$signing_copy"; then
  echo "::error::signing extensions.json failed; refusing to publish an unsigned index"
  exit 1
fi
test -s "${signing_copy}.sig"

# --- 6. Write the outputs -------------------------------------------
mkdir -p "$(dirname "${OUTPUT_JSON}")" "$(dirname "${OUTPUT_SIG}")"
cp "${TMP_DIR}/extensions.json" "${OUTPUT_JSON}"
cp "${signing_copy}.sig" "${OUTPUT_SIG}"
echo "[extensions.json] wrote ${OUTPUT_JSON} and ${OUTPUT_SIG} ($(jq -r '.packages | length' "${OUTPUT_JSON}") packages, $(wc -l < "${TMP_DIR}/package-releases.jsonl" | tr -d ' ') published versions, generated_at=${generated_at})"
