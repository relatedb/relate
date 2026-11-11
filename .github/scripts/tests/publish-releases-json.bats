# Behavioral tests for publish-releases-json.sh
#
# Locks the ADR-0250 releases.json publisher contract: the document is a
# full snapshot of the live release list (schema_version 1, RFC 3339 UTC
# generated_at), `releases` holds the 10 newest STABLE core-namespace
# (published, non-draft, non-prerelease, vX.Y.Z — extension-channel
# releases never enter the document, LABS-806) releases newest first,
# `latest` is the newest
# stable release (null when none exists) and equals releases[0],
# `latest_prerelease` is the newest published non-draft prerelease (a
# reserved slot that never enters the window), `notes` is the release body
# verbatim — the prepended narrative notes included, "" and never null when
# the release has no body — and every asset name resolves through the
# ADR-0250 §3 download URL template
# (https://github.com/relatedb/relate/releases/download/{tag}/{asset}).
#
# The gh CLI is stubbed on PATH (no network): the stub serves a committed
# fixture release set as `gh release list` and `gh release view <tag>`, and
# logs its argv so the tests can assert which releases were hydrated.
# `generated_at` is pinned via PUBLISH_RELEASES_JSON_GENERATED_AT so the
# output is compared against the committed expected-output fixture
# (fixtures/publish-releases-json-expected.json) twice: canonically
# (jq -S, asserting the document independently of any jq version's
# pretty-printer) and byte-exactly (asserting the serialization itself —
# see the byte-exact test below).
#
# Local run:
#   bats .github/scripts/tests/publish-releases-json.bats
#
# Requirements:
#   bats-core >= 1.5.0  (brew install bats-core)
#   bash, jq

bats_require_minimum_version 1.5.0

load gh-stub

SCRIPT="$(cd "$(dirname "$BATS_TEST_FILENAME")/.." && pwd)/publish-releases-json.sh"
FIXTURES_DIR="$(cd "$(dirname "$BATS_TEST_FILENAME")" && pwd)/fixtures"

RELEASES_FIXTURE="${FIXTURES_DIR}/publish-releases-json-releases.json"
EXPECTED_FIXTURE="${FIXTURES_DIR}/publish-releases-json-expected.json"
EMPTY_FIXTURE="${FIXTURES_DIR}/publish-releases-json-releases-empty.json"
BAD_ASSET_FIXTURE="${FIXTURES_DIR}/publish-releases-json-releases-bad-asset.json"
EXTENSIONS_FIXTURE="${FIXTURES_DIR}/publish-releases-json-releases-extensions.json"

# ADR-0250 §3: assets are downloaded through the non-API route; this is the
# template every generated asset name must resolve through.
DOWNLOAD_URL_TEMPLATE='https://github.com/relatedb/relate/releases/download'

# Fixed generated_at, so the generated document is comparable to the
# committed fixture.
FIXED_GENERATED_AT='2026-10-15T12:00:00Z'

# ---------------------------------------------------------------------------
# setup / teardown
# ---------------------------------------------------------------------------

setup() {
  TEST_TMP="$(mktemp -d)"
  STUBS_DIR="${TEST_TMP}/stubs"
  mkdir -p "${STUBS_DIR}"
  GH_STUB_LOG="${TEST_TMP}/gh.log"
  OUT="${TEST_TMP}/releases.json"

  # gh stub — never touches the network. `release list` projects the
  # list-shaped JSON gh emits (note: fixture order, so the script must sort);
  # `release view <tag>` serves the one matching release object. The
  # harness around the arms (argv log, GH_STUB_FAIL injection,
  # unexpected-invocation fail) is the shared writer's; GH_STUB_FAIL=<list|
  # view> drives the fail-closed paths.
  write_gh_stub "${STUBS_DIR}/gh" <<'ARMS'
  "release list")
    jq -c '[.[] | {tagName, publishedAt, isDraft, isPrerelease}]' "${GH_STUB_RELEASES:?GH_STUB_RELEASES not set}"
    ;;
  "release view")
    jq -e --arg tag "${3:?tag required}" '.[] | select(.tagName == $tag)' "${GH_STUB_RELEASES:?GH_STUB_RELEASES not set}"
    ;;
ARMS

  export TEST_TMP STUBS_DIR GH_STUB_LOG OUT
}

teardown() {
  rm -rf "${TEST_TMP}"
}

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Run the generator against a fixture release set with the gh stub on PATH.
# Args: FIXTURE [NAME=VALUE ...] — extra env assignments (applied last).
run_generator() {
  local fixture="$1"
  shift
  env PATH="${STUBS_DIR}:${PATH}" \
    GH_STUB_RELEASES="${fixture}" \
    GH_STUB_LOG="${GH_STUB_LOG}" \
    PUBLISH_RELEASES_JSON_GENERATED_AT="${FIXED_GENERATED_AT}" \
    "$@" \
    bash "${SCRIPT}" "${OUT}" 2>&1
}

# Canonical JSON (sorted keys, no pretty-printer dependence) of a file.
canonical() {
  jq -S . "$1"
}

# ---------------------------------------------------------------------------
# Tests: the generated document
# ---------------------------------------------------------------------------

@test "matches the committed expected-output fixture" {
  run run_generator "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  jq -e . "${OUT}" > /dev/null
  diff -u <(canonical "${EXPECTED_FIXTURE}") <(canonical "${OUT}")
}

# The publisher serialization pin, byte-exact against the producer: the
# generator above runs the real jq assembly (publish-releases-json.sh's
# `jq -n` document build) under this suite's own jq, and its output must
# be byte-identical to the committed fixture — not merely the same
# document. The fixture is what the install.sh harness serves end to end
# in its publisher_doc mode, and install.sh's latest_tag_of parses the
# publisher's serialization grammar (one key per line, two-space indents),
# so a reformatting jq flag or assembly change must fail here and force
# the fixture's regeneration rather than drift past the canonicalized
# comparison. (The chosen shape: the pin lives in this suite as a
# byte-exact cmp, rather than regenerating the install.sh harness's
# served document through the publisher; the harness's publisher_doc case
# is the other half, pinning that the parser reads this very fixture.)
@test "the serialization is byte-exact against the committed fixture" {
  run run_generator "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  cmp "${EXPECTED_FIXTURE}" "${OUT}"
}

@test "carries schema_version 1, the ADR top-level keys in order, and an RFC 3339 UTC generated_at" {
  run run_generator "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  run jq -r '.schema_version' "${OUT}"
  [ "$output" = "1" ]

  # Exactly the five top-level keys, in the ADR-0250 §2 table order.
  run jq -r 'keys_unsorted | join(",")' "${OUT}"
  [ "$output" = "schema_version,generated_at,latest,latest_prerelease,releases" ]

  # generated_at is RFC 3339 UTC (Z); pinned in this suite.
  run jq -r '.generated_at' "${OUT}"
  [ "$output" = "${FIXED_GENERATED_AT}" ]
}

@test "latest is the newest stable release and equals releases[0]" {
  run run_generator "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  run jq -r '.latest.tag' "${OUT}"
  [ "$output" = "v0.0.11" ]

  run jq -e '.latest == .releases[0]' "${OUT}"
  [ "$status" -eq 0 ]

  run jq -e '.latest.prerelease == false' "${OUT}"
  [ "$status" -eq 0 ]

  # Every release object carries exactly the ADR key set, in order.
  run jq -e '
    [.latest, .latest_prerelease, .releases[]]
    | all(.[]; (keys_unsorted | join(",")) == "tag,published_at,notes,prerelease,assets")
  ' "${OUT}"
  [ "$status" -eq 0 ]
}

@test "the window holds the 10 newest stable releases, newest first" {
  run run_generator "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  run jq -r '.releases | length' "${OUT}"
  [ "$output" -eq 10 ]

  run jq -r '[.releases[].tag] | join(",")' "${OUT}"
  [ "$output" = "v0.0.11,v0.0.10,v0.0.9,v0.0.8,v0.0.7,v0.0.6,v0.0.5,v0.0.4,v0.0.3,v0.0.2" ]

  # Newest first by publish time.
  run jq -e '[.releases[].published_at] | . == (sort | reverse)' "${OUT}"
  [ "$status" -eq 0 ]

  # The stable releases older than the window fell out of it.
  run jq -r '[.releases[].tag] | index("v0.0.1") // "absent"' "${OUT}"
  [ "$output" = "absent" ]
  run jq -r '[.releases[].tag] | index("v0.6.2") // "absent"' "${OUT}"
  [ "$output" = "absent" ]
}

@test "prereleases and drafts never enter the window" {
  run run_generator "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  run jq -e '[.releases[].prerelease] | all(.[]; . == false)' "${OUT}"
  [ "$status" -eq 0 ]

  # No prerelease tag and no draft release anywhere in the document.
  ! grep -qF '"v0.0.10-rc.1"' "${OUT}"
  ! grep -qF '"v0.12.0-draft"' "${OUT}"
  ! grep -qF "Draft body that must never be published." "${OUT}"
}

@test "extension-channel releases never enter the document (LABS-806)" {
  # The extensions fixture carries a stable plugin release published
  # NEWER than every core release and a plugin prerelease newer than the
  # newest core prerelease — exactly the v0.15.0 incident shape. This is
  # the core channel (ADR-0250 stable tags are vX.Y.Z; the CLI treats
  # any other latest.tag as a malformed file), so no
  # relate-extensions-* tag may appear anywhere in the document, and
  # neither the window nor the reserved prerelease slot may shift.
  run run_generator "${EXTENSIONS_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  # No extension-namespace tag anywhere in the document.
  ! grep -qF 'relate-extensions' "${OUT}"

  run jq -r '.latest.tag' "${OUT}"
  [ "$output" = "v0.0.11" ]

  run jq -r '[.releases[].tag] | join(",")' "${OUT}"
  [ "$output" = "v0.0.11,v0.0.10,v0.0.9,v0.0.8,v0.0.7,v0.0.6,v0.0.5,v0.0.4,v0.0.3,v0.0.2" ]

  # The plugin prerelease is newer than every core prerelease; the
  # reserved slot still names the newest CORE prerelease.
  run jq -r '.latest_prerelease.tag' "${OUT}"
  [ "$output" = "v0.0.12-rc.1" ]

  # Nothing from the extension namespace was ever hydrated.
  ! grep -q '^release view relate-extensions' "${GH_STUB_LOG}"
}

@test "latest_prerelease is the newest published prerelease, newer than latest" {
  run run_generator "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  run jq -r '.latest_prerelease.tag' "${OUT}"
  [ "$output" = "v0.0.12-rc.1" ]

  run jq -e '.latest_prerelease.prerelease == true' "${OUT}"
  [ "$status" -eq 0 ]

  # The reserved slot is independent of the stable window: the newest
  # prerelease is newer than the newest stable release.
  run jq -e '.latest_prerelease.published_at > .latest.published_at' "${OUT}"
  [ "$status" -eq 0 ]
}

@test "notes are the release body verbatim, narrative notes included" {
  run run_generator "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  # The newest stable release's body is the release-notes-prepend.sh output
  # shape (markers, heading, narrative, hand-written body). releases.json
  # must carry it verbatim — that is what makes running the publisher after
  # release-notes load-bearing (ADR-0250 §1/§6).
  run jq -r '.latest.notes' "${OUT}"
  grep -qF '<!-- release-notes:start -->' <<< "$output"
  grep -qF '## Release notes' <<< "$output"
  grep -qF 'stop hitting the 60/hour anonymous limit' <<< "$output"
  grep -qF '<!-- release-notes:end -->' <<< "$output"
  grep -qF 'Full changelog: https://github.com/relatedb/relate/compare/v0.0.10...v0.0.11' <<< "$output"
}

@test "every notes field matches its release body (null body -> empty string)" {
  run run_generator "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  run jq -e --slurpfile src "${RELEASES_FIXTURE}" '
    [.latest, .latest_prerelease, .releases[]]
    | map(select(. != null))
    | all(.[];
        . as $out
        | ($src[0] | map(select(.tagName == $out.tag)) | .[0]) as $in
        | $out.notes == ($in.body // "")
      )
  ' "${OUT}"
  [ "$status" -eq 0 ]

  # The fixture has both flavours of "no body": an empty string and null.
  # Both must land as "" — never null.
  run jq -r '.releases[] | select(.tag == "v0.0.8") | .notes' "${OUT}"
  [ "$output" = "" ]
  run jq -r '.releases[] | select(.tag == "v0.0.7") | .notes' "${OUT}"
  [ "$output" = "" ]
}

# ---------------------------------------------------------------------------
# Tests: the ADR-0250 §3 download URL template
# ---------------------------------------------------------------------------

@test "every asset name resolves through the ADR download URL template" {
  run run_generator "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  # One line per asset in the document: tag<TAB>asset<TAB>template-derived URL.
  run jq -r --arg tmpl "${DOWNLOAD_URL_TEMPLATE}" '
    [.latest, .latest_prerelease, .releases[]]
    | map(select(. != null))
    | .[] as $r
    | $r.assets[]
    | "\($r.tag)\t\(.)\t\($tmpl)/\($r.tag)/\(.)"
  ' "${OUT}"
  echo "$output"
  [ "$status" -eq 0 ]
  [ -n "$output" ]

  while IFS=$'\t' read -r tag asset url; do
    [ -n "${tag}" ] || continue
    # The asset name is the URL's last path segment verbatim, so it must
    # not need escaping: no spaces, no slashes, no empty segments.
    if ! [[ "${url}" =~ ^https://github\.com/relatedb/relate/releases/download/[^/[:space:]]+/[^/[:space:]]+$ ]]; then
      echo "asset name does not resolve through the download URL template: ${tag} / ${asset} -> ${url}"
      return 1
    fi
  done <<< "$output"

  # ADR-0250 §3's worked example: the v0.0.11 darwin-arm64 binary.
  run jq -r --arg tmpl "${DOWNLOAD_URL_TEMPLATE}" '.latest | "\($tmpl)/\(.tag)/relate-darwin-arm64"' "${OUT}"
  [ "$output" = "https://github.com/relatedb/relate/releases/download/v0.0.11/relate-darwin-arm64" ]

  # The names the CLI requires in latest.assets (ADR-0250 §2/§3): the
  # platform binary, the manifest and its signature.
  for name in relate-darwin-arm64 relate-linux-amd64 SHA256SUMS SHA256SUMS.sig; do
    if ! jq -e --arg n "${name}" '.latest.assets | index($n) != null' "${OUT}" > /dev/null; then
      echo "latest.assets is missing ${name}"
      return 1
    fi
  done
}

# ---------------------------------------------------------------------------
# Tests: edge cases and fail-closed behaviour
# ---------------------------------------------------------------------------

@test "an empty release list yields null latest, null latest_prerelease and an empty window" {
  run run_generator "${EMPTY_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  run jq -c . "${OUT}"
  [ "$output" = '{"schema_version":1,"generated_at":"2026-10-15T12:00:00Z","latest":null,"latest_prerelease":null,"releases":[]}' ]
}

@test "re-running is idempotent apart from generated_at" {
  run run_generator "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]
  cp "${OUT}" "${TEST_TMP}/first.json"

  run run_generator "${RELEASES_FIXTURE}" PUBLISH_RELEASES_JSON_GENERATED_AT="2026-10-16T00:00:00Z"
  echo "$output"
  [ "$status" -eq 0 ]
  cp "${OUT}" "${TEST_TMP}/second.json"

  # generated_at moved...
  run jq -r '.generated_at' "${TEST_TMP}/second.json"
  [ "$output" = "2026-10-16T00:00:00Z" ]

  # ...and nothing else did.
  diff -u <(jq -S 'del(.generated_at)' "${TEST_TMP}/first.json") \
    <(jq -S 'del(.generated_at)' "${TEST_TMP}/second.json")
}

@test "only the window entries and the reserved prerelease slot are hydrated" {
  run run_generator "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  # One release list call, then exactly 10 + 1 release view calls.
  [ "$(grep -c '^release list ' "${GH_STUB_LOG}")" -eq 1 ]
  [ "$(grep -c '^release view ' "${GH_STUB_LOG}")" -eq 11 ]

  # Neither the draft nor a release that fell out of the window is fetched.
  ! grep -qE 'view v0\.12\.0-draft --json' "${GH_STUB_LOG}"
  ! grep -qE 'view v0\.7\.0 --json' "${GH_STUB_LOG}"
  ! grep -qE 'view v0\.6\.2 --json' "${GH_STUB_LOG}"
}

@test "gh release list failure fails closed and writes no output" {
  run run_generator "${RELEASES_FIXTURE}" GH_STUB_FAIL=list
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "::error::" <<< "$output"
  [ ! -e "${OUT}" ]
}

@test "gh release view failure fails closed and writes no output" {
  run run_generator "${RELEASES_FIXTURE}" GH_STUB_FAIL=view
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "::error::" <<< "$output"
  [ ! -e "${OUT}" ]
}

@test "an asset name that cannot form a download URL is refused" {
  run run_generator "${BAD_ASSET_FIXTURE}"
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "::error::" <<< "$output"
  grep -qF "cannot form a valid download URL" <<< "$output"
  [ ! -e "${OUT}" ]
}

@test "a malformed pinned generated_at is refused" {
  run run_generator "${RELEASES_FIXTURE}" PUBLISH_RELEASES_JSON_GENERATED_AT="not-a-timestamp"
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "::error::" <<< "$output"
  [ ! -e "${OUT}" ]
}

@test "a missing OUTPUT_FILE argument is refused" {
  run env PATH="${STUBS_DIR}:${PATH}" bash "${SCRIPT}"
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "::error::" <<< "$output"
}
