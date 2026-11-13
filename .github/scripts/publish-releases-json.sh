#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C.UTF-8

# ============================================================
# publish-releases-json.sh
#
# Generates the `releases.json` document served at
# https://relatedb.ai/releases.json — the static
# release-discovery file `relate upgrade` / `update` reads
# (ADR-0250 §2 schema, §3 download URL template, §6 publisher
# contract).
#
# The document is a FULL SNAPSHOT recomputed from the live GitHub
# release list on every run, never an append log. It is the CORE
# channel: only core-namespace releases (tags vX.Y.Z; the extension
# channel's relate-extensions-* releases have their own document,
# extensions.json — ADR-0431) are ever selected:
#
#   - `releases` holds the 10 most recent STABLE core releases
#     (published, non-draft, non-prerelease), newest first, including
#     the release that triggered this run; fewer when fewer exist.
#   - `latest` is the newest stable release — `releases[0]` when the
#     window is non-empty, `null` when no stable release exists.
#   - Prereleases never enter the window; the newest published
#     non-draft prerelease lands in `latest_prerelease` (a reserved
#     slot no consumer reads today).
#   - `notes` is the release body markdown VERBATIM (the empty string,
#     never null, when the release has no body).
#
# install.sh's latest_tag_of reads this document with a dependency-free
# awk parse over jq's default pretty-print (one key per line, two-space
# indents), so a change to this script's serialization must move with
# that parser or the installer's release selection silently strands.
#
# Run this AFTER .github/workflows/release-notes.yml has prepended
# the agent-written narrative notes to the release body: release.yml
# chains the publisher with `needs: [..., release-notes]` precisely so
# the body — and therefore `notes` — already carries them. Reading a
# body from before that prepend would publish notes without the
# narrative section.
#
# The release list is read with the `gh` CLI: `gh release list` for
# the ordering / draft / prerelease flags, then `gh release view` for
# the body and asset names of each selected release (at most 11 views:
# the 10-entry window plus the reserved prerelease slot). `gh` reads
# GH_TOKEN (or GITHUB_TOKEN) and GH_REPO from the environment; CI sets
# both.
#
# Usage: publish-releases-json.sh OUTPUT_FILE
#
# Env:
#   PUBLISH_RELEASES_JSON_GENERATED_AT
#     Test seam only: pins `generated_at` to a fixed RFC 3339 UTC (Z)
#     timestamp so the bats fixture comparison is deterministic.
#     Unset in CI, where the wall clock is used.
#
# Exit status: 0 = OUTPUT_FILE written; 1 = refused, with a
# `::error::` diagnostic on stderr and OUTPUT_FILE left untouched
# (the document is built and validated in a temp dir first).
# ============================================================

# The ADR-0250 §2 window: the 10 most recent stable releases.
WINDOW=10
# `gh release list` returns releases newest-created first; the newest
# 100 comfortably cover a 10-entry window even in a prerelease-heavy
# repository, and the script re-sorts by publish time regardless.
LIST_LIMIT=100

if [ "$#" -ne 1 ] || [ -z "${1:-}" ]; then
  echo "::error::Usage: publish-releases-json.sh OUTPUT_FILE"
  exit 1
fi
OUTPUT_FILE="$1"

for tool in gh jq; do
  if ! command -v "$tool" > /dev/null 2>&1; then
    echo "::error::${tool} not found on PATH; refusing to publish releases.json"
    exit 1
  fi
done

generated_at="${PUBLISH_RELEASES_JSON_GENERATED_AT:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}"
if ! printf '%s\n' "$generated_at" | grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$'; then
  echo "::error::generated_at '${generated_at}' is not an RFC 3339 UTC (Z) timestamp; refusing to publish releases.json"
  exit 1
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

# --- 1. Release list: ordering, draft and prerelease flags ----------
if ! gh release list --limit "${LIST_LIMIT}" \
  --json tagName,publishedAt,isDraft,isPrerelease > "${TMP_DIR}/list.json"; then
  echo "::error::gh release list failed; refusing to publish an incomplete releases.json"
  exit 1
fi

# Stable = published (non-draft), non-prerelease, in the CORE tag
# namespace (vX.Y.Z) — the ADR-0250 §"Invariants" stable shape, whose
# absence the CLI treats as a malformed file. The extension channel
# publishes its own releases (relate-extensions-<name>-<X.Y.Z>,
# ADR-0431) with their own discovery document (extensions.json); they
# never enter this one, however recently they were published. Newest
# first by published_at; jq's sort is stable, so equal timestamps keep
# gh's created_at ordering. Drafts carry no published_at, which is why
# the draft filter runs before the sort.
jq -r --argjson window "${WINDOW}" '
  [ .[] | select((.isDraft | not) and (.isPrerelease | not))
        | select(.tagName | test("^v[0-9]+\\.[0-9]+\\.[0-9]+$")) ]
  | sort_by(.publishedAt) | reverse | .[:$window] | .[].tagName
' "${TMP_DIR}/list.json" > "${TMP_DIR}/stable-tags.txt"

# The newest published non-draft prerelease, for the reserved slot —
# also core-namespace only (a v-prefixed tag; prerelease core tags may
# carry a -rc suffix, so the shape check is the looser prefix).
jq -r '
  [ .[] | select((.isDraft | not) and .isPrerelease)
        | select(.tagName | test("^v[0-9]")) ]
  | sort_by(.publishedAt) | reverse | (.[0].tagName // empty)
' "${TMP_DIR}/list.json" > "${TMP_DIR}/prerelease-tag.txt"

# --- 2. Hydrate each selected release: body + asset names -----------
# One release object per line (JSONL), schema key order per ADR-0250
# §2. `gh release view` returns the exact tag, publish time, body
# markdown, prerelease flag and asset objects; only the asset NAME is
# kept — download URLs are template-derived by consumers (ADR-0250
# §3), never carried in the file.
hydrate() { # $1 = tag, $2 = output file (one JSON release object)
  local tag="$1" out="$2" raw
  raw="$(mktemp "${TMP_DIR}/view.XXXXXX")"
  if ! gh release view "$tag" --json tagName,publishedAt,body,isPrerelease,assets > "$raw"; then
    echo "::error::gh release view '${tag}' failed; refusing to publish an incomplete releases.json"
    exit 1
  fi
  jq -c '{
    tag: .tagName,
    published_at: .publishedAt,
    notes: (.body // ""),
    prerelease: .isPrerelease,
    assets: [.assets[].name]
  }' "$raw" > "$out"
}

: > "${TMP_DIR}/stable.jsonl"
while IFS= read -r tag; do
  [ -n "$tag" ] || continue
  hydrate "$tag" "${TMP_DIR}/stable.obj.json"
  cat "${TMP_DIR}/stable.obj.json" >> "${TMP_DIR}/stable.jsonl"
done < "${TMP_DIR}/stable-tags.txt"

: > "${TMP_DIR}/prerelease.jsonl"
prerelease_tag="$(cat "${TMP_DIR}/prerelease-tag.txt")"
if [ -n "$prerelease_tag" ]; then
  hydrate "$prerelease_tag" "${TMP_DIR}/prerelease.obj.json"
  cat "${TMP_DIR}/prerelease.obj.json" >> "${TMP_DIR}/prerelease.jsonl"
fi

# --- 3. Assemble the document (ADR-0250 §2 key order) ---------------
jq -n \
  --arg generated_at "${generated_at}" \
  --slurpfile releases "${TMP_DIR}/stable.jsonl" \
  --slurpfile prerelease "${TMP_DIR}/prerelease.jsonl" \
  '{
    schema_version: 1,
    generated_at: $generated_at,
    latest: ($releases[0] // null),
    latest_prerelease: ($prerelease[0] // null),
    releases: $releases
  }' > "${TMP_DIR}/releases.json"

# --- 4. Validate before it can ship ---------------------------------
# Every tag and asset name must resolve through the ADR-0250 §3
# template (https://github.com/relatedb/relate/releases/download/
# {tag}/{asset}): the name is the last path segment verbatim, so a name
# carrying a slash or whitespace could never form that URL. Refuse to
# publish rather than ship an unresolvable asset list.
if ! jq -e '
  ([.latest, .latest_prerelease, .releases[]] | map(select(. != null)))
  | all(.[];
      (.tag | test("^[^/[:space:]]+$"))
      and (.assets | all(.[]; test("^[^/[:space:]]+$")))
    )
' "${TMP_DIR}/releases.json" > /dev/null; then
  echo "::error::a tag or asset name in releases.json cannot form a valid download URL (ADR-0250 §3); refusing to publish"
  exit 1
fi

# --- 5. Write the output --------------------------------------------
mkdir -p "$(dirname "${OUTPUT_FILE}")"
cp "${TMP_DIR}/releases.json" "${OUTPUT_FILE}"
echo "[releases.json] wrote ${OUTPUT_FILE} ($(jq -r '.releases | length' "${OUTPUT_FILE}") stable releases, generated_at=${generated_at})"
