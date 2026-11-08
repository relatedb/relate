# Behavioral tests for publish-extensions-json.sh
#
# Locks the ADR-0431 §1 channel-index publisher contract: the signed
# extensions.json served beside releases.json at the website root,
# written only by the release pipeline. The document is a full
# snapshot of the live release list — one row per package
# (sorted by name, `hidden: false`), one entry per published version
# (newest first, the contract the default install's newest- pick
# relies on), one asset row per platform tarball (os/arch in the
# channel scope, the tag it rides, the asset name, its SHA256, the
# protocol column from the tarball's own manifest, the MCP revision).
# Drafts and
# prereleases never enter the index; a published tag claiming the
# `relate-extensions-` prefix that does not parse is refused;
# every asset must follow the `<name>-<X.Y.Z>-<os>-<arch>.tar.gz`
# convention and name the release's package and version; every
# tarball must carry the manifest the tag claims. The output is
# signed (SSHSIG, namespace relate-release) — an unsigned index
# is not a publishable artifact.
#
# The gh CLI is stubbed on PATH (no network): the stub serves a
# committed fixture release set as `gh release list`, `gh release
# view <tag>` and `gh release download <tag> --pattern <asset>` (the
# committed fixture tarballs), and logs its argv so the tests can
# assert which releases were hydrated and downloaded. `ssh-keygen`
# is stubbed too: it writes a marker .sig and logs its argv, so the
# signing invocation (namespace, keyfile) is pinned without a real
# key. `generated_at` is pinned via
# PUBLISH_EXTENSIONS_JSON_GENERATED_AT so the output is compared
# against the committed expected-output fixture
# (fixtures/publish-extensions-json-expected.json); that comparison
# is canonicalized (jq -S) so it asserts the document, not a jq
# version's pretty-printer.
#
# Local run:
#   bats .github/scripts/tests/publish-extensions-json.bats
#
# Requirements:
#   bats-core >= 1.5.0  (brew install bats-core)
#   bash, jq, tar, shasum

bats_require_minimum_version 1.5.0

load gh-stub

SCRIPT="$(cd "$(dirname "$BATS_TEST_FILENAME")/.." && pwd)/publish-extensions-json.sh"
FIXTURES_DIR="$(cd "$(dirname "$BATS_TEST_FILENAME")" && pwd)/fixtures"

RELEASES_FIXTURE="${FIXTURES_DIR}/publish-extensions-json-releases.json"
EXPECTED_FIXTURE="${FIXTURES_DIR}/publish-extensions-json-expected.json"
EMPTY_FIXTURE="${FIXTURES_DIR}/publish-extensions-json-releases-empty.json"
BAD_ASSET_FIXTURE="${FIXTURES_DIR}/publish-extensions-json-releases-bad-asset.json"
BAD_TAG_FIXTURE="${FIXTURES_DIR}/publish-extensions-json-releases-bad-tag.json"
MISMATCH_FIXTURE="${FIXTURES_DIR}/publish-extensions-json-releases-mismatch.json"

# ADR-0431 §1 / ADR-0250 §3: the assets are downloaded through the
# non-API route; this is the template every generated asset name
# must resolve through.
DOWNLOAD_URL_TEMPLATE='https://github.com/relatedb/relate/releases/download'

# Fixed generated_at, so the generated document is comparable to the
# committed fixture.
FIXED_GENERATED_AT='2026-10-15T12:00:00Z'

# A stand-in private key: never read as a key (ssh-keygen is stubbed);
# its value is what the script must write into the signing keyfile.
FIXTURE_PRIVATE_KEY='test-release-signing-key-material'

# ---------------------------------------------------------------------------
# setup / teardown
# ---------------------------------------------------------------------------

setup() {
  TEST_TMP="$(mktemp -d)"
  STUBS_DIR="${TEST_TMP}/stubs"
  mkdir -p "${STUBS_DIR}"
  GH_STUB_LOG="${TEST_TMP}/gh.log"
  SSH_KEYGEN_LOG="${TEST_TMP}/ssh-keygen.log"
  OUT_JSON="${TEST_TMP}/extensions.json"
  OUT_SIG="${TEST_TMP}/extensions.json.sig"

  # gh stub — never touches the network. `release list` projects the
  # list-shaped JSON gh emits; `release view <tag>` serves the one
  # matching release object; `release download <tag> --pattern <asset>
  # --clobber --output <file>` copies the committed fixture tarball
  # named for the asset (`publish-extensions-json-<asset>`), refusing
  # to overwrite an existing output without `--clobber` exactly as gh
  # does — the per-asset loop's one-path reuse meets the real tool's
  # behavior, not a more permissive copy. The harness around the arms
  # (argv log, GH_STUB_FAIL injection, unexpected-invocation fail)
  # is the shared writer's; GH_STUB_FAIL=<list|view|download> drives the
  # fail-closed paths.
  write_gh_stub "${STUBS_DIR}/gh" <<'ARMS'
  "release list")
    jq -c '[.[] | {tagName, isDraft, isPrerelease}]' "${GH_STUB_RELEASES:?GH_STUB_RELEASES not set}"
    ;;
  "release view")
    jq -e --arg tag "${3:?tag required}" '.[] | select(.tagName == $tag)' "${GH_STUB_RELEASES:?GH_STUB_RELEASES not set}"
    ;;
  "release download")
    tag="${3:?tag required}"
    pattern=""
    output=""
    clobber=""
    shift 3
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --pattern) pattern="$2"; shift 2 ;;
        --output) output="$2"; shift 2 ;;
        --clobber) clobber=1; shift ;;
        *) shift ;;
      esac
    done
    [ -n "$pattern" ] && [ -n "$output" ] || { echo "gh stub: download needs --pattern and --output" >&2; exit 1; }
    fixture="${GH_STUB_TARBALLS:?GH_STUB_TARBALLS not set}/publish-extensions-json-${pattern}"
    [ -f "$fixture" ] || { echo "gh stub: no fixture tarball for asset '${pattern}'" >&2; exit 1; }
    if [ -e "$output" ] && [ -z "$clobber" ]; then
      echo "gh stub: '${output}' already exists (use --clobber to overwrite file or --skip-existing to skip file)" >&2
      exit 1
    fi
    cp "$fixture" "$output"
    ;;
ARMS

  # ssh-keygen stub — the signing half. Logs the argv, the key material
  # it was asked to sign with (the keyfile itself is removed with the
  # script's temp dir, so the log is where the test reads it from), and
  # writes the marker .sig beside the file it is asked to sign
  # (ssh-keygen's own contract).
  cat > "${STUBS_DIR}/ssh-keygen" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
keyfile=""
file="${*: -1}"
args=("$@")
i=0
while [ "$i" -lt "${#args[@]}" ]; do
  if [ "${args[$i]}" = "-f" ]; then
    keyfile="${args[$((i + 1))]}"
  fi
  i=$((i + 1))
done
printf 'argv:%s\n' "$*" >> "${SSH_KEYGEN_LOG:?SSH_KEYGEN_LOG not set}"
[ -n "${keyfile}" ] && printf 'key-material:%s\n' "$(cat "${keyfile}")" >> "${SSH_KEYGEN_LOG}"
printf 'fixture sshsig over %s\n' "$(basename "$file")" > "${file}.sig"
STUB
  chmod +x "${STUBS_DIR}/ssh-keygen"

  export TEST_TMP STUBS_DIR GH_STUB_LOG SSH_KEYGEN_LOG OUT_JSON OUT_SIG
}

teardown() {
  rm -rf "${TEST_TMP}"
}

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Run the publisher against a fixture release set with the gh and
# ssh-keygen stubs on PATH. Args: FIXTURE [NAME=VALUE ...] — extra env
# assignments (applied last).
run_publisher() {
  local fixture="$1"
  shift
  env PATH="${STUBS_DIR}:${PATH}" \
    GH_STUB_RELEASES="${fixture}" \
    GH_STUB_TARBALLS="${FIXTURES_DIR}" \
    GH_STUB_LOG="${GH_STUB_LOG}" \
    SSH_KEYGEN_LOG="${SSH_KEYGEN_LOG}" \
    RELEASE_SIGNING_PRIVATE_KEY="${FIXTURE_PRIVATE_KEY}" \
    PUBLISH_EXTENSIONS_JSON_GENERATED_AT="${FIXED_GENERATED_AT}" \
    "$@" \
    bash "${SCRIPT}" "${OUT_JSON}" "${OUT_SIG}" 2>&1
}

# Canonical JSON (sorted keys, no pretty-printer dependence) of a file.
canonical() {
  jq -S . "$1"
}

# ---------------------------------------------------------------------------
# Tests: the generated document
# ---------------------------------------------------------------------------

@test "matches the committed expected-output fixture and signs beside it" {
  run run_publisher "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  jq -e . "${OUT_JSON}" > /dev/null
  diff -u <(canonical "${EXPECTED_FIXTURE}") <(canonical "${OUT_JSON}")

  # The signature is written, and it is the stub's output for exactly
  # the document bytes (signed in place: the copy the stub signed).
  [ -s "${OUT_SIG}" ]
  grep -qF "fixture sshsig over" "${OUT_SIG}"
}

@test "carries schema_version 1, the channel's top-level keys in order, and an RFC 3339 UTC generated_at" {
  run run_publisher "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  run jq -r '.schema_version' "${OUT_JSON}"
  [ "$output" = "1" ]

  # Exactly the three top-level keys, in the schema's order.
  run jq -r 'keys_unsorted | join(",")' "${OUT_JSON}"
  [ "$output" = "schema_version,generated_at,packages" ]

  run jq -r '.generated_at' "${OUT_JSON}"
  [ "$output" = "${FIXED_GENERATED_AT}" ]
}

@test "one row per package with hidden spelled false, sorted by name" {
  run run_publisher "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  run jq -r '[.packages[].name] | join(",")' "${OUT_JSON}"
  [ "$output" = "greet" ]

  # Every package object carries exactly the schema key set, in order,
  # with the hidden policy spelled (the CLI fixture's own shape).
  run jq -e 'all(.packages[]; (keys_unsorted | join(",")) == "name,hidden,versions")' "${OUT_JSON}"
  [ "$status" -eq 0 ]
  run jq -e 'all(.packages[]; .hidden == false)' "${OUT_JSON}"
  [ "$status" -eq 0 ]
}

@test "versions are newest first and every entry carries the manifest's protocol" {
  run run_publisher "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  # The newest listed first — the publisher contract the default
  # install's newest- pick relies on (the CLI takes versions[0]).
  run jq -r '.packages[0].versions | map(.version) | join(",")' "${OUT_JSON}"
  [ "$output" = "1.2.3,1.1.0" ]

  # The protocol column comes from the tarball's own manifest, not the
  # tag: every version carries it, every asset row agrees.
  run jq -e 'all(.packages[]; all(.versions[]; (.protocol == "2025-06-18") and (.assets | length >= 1)))' "${OUT_JSON}"
  [ "$status" -eq 0 ]

  # Every version object carries exactly the schema key set, in order.
  run jq -e 'all(.packages[]; all(.versions[]; (keys_unsorted | join(",")) == "version,protocol,assets"))' "${OUT_JSON}"
  [ "$status" -eq 0 ]
}

@test "asset rows carry os/arch in the channel scope, the tag, the asset name, and the tarball's SHA256" {
  run run_publisher "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  # The 1.2.3 darwin-arm64 row, its checksum pinned against the committed
  # fixture tarball's real digest (shasum is the same tool the script
  # runs — no re-tabled constant).
  expected_sha="$(shasum -a 256 "${FIXTURES_DIR}/publish-extensions-json-greet-1.2.3-darwin-arm64.tar.gz" | cut -d' ' -f1)"
  run jq -e \
    --arg sha "${expected_sha}" \
    '.packages[0].versions[0].assets[0]
      | .os == "darwin" and .arch == "arm64"
      and .tag == "relate-extensions-greet-1.2.3"
      and .asset == "greet-1.2.3-darwin-arm64.tar.gz"
      and .sha256 == $sha' "${OUT_JSON}"
  [ "$status" -eq 0 ]

  # Every asset object carries exactly the schema key set, in order.
  run jq -e 'all(.packages[]; all(.versions[]; all(.assets[]; (keys_unsorted | join(",")) == "os,arch,tag,asset,sha256")))' "${OUT_JSON}"
  [ "$status" -eq 0 ]

  # os/arch stay inside the channel's platform scope (ADR-0068).
  run jq -e 'all(.packages[]; all(.versions[]; all(.assets[]; (.os == "darwin" or .os == "linux") and (.arch == "amd64" or .arch == "arm64"))))' "${OUT_JSON}"
  [ "$status" -eq 0 ]
}

@test "drafts, prereleases, and non-extension releases never enter the index" {
  run run_publisher "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  # No draft, no prerelease, no CLI release anywhere in the document.
  ! grep -qF 'egg' "${OUT_JSON}"
  ! grep -qF '1.3.0-rc.1' "${OUT_JSON}"
  ! grep -qF 'v0.12.5' "${OUT_JSON}"

  # Only the two published package releases were hydrated: one list
  # call, one view per release, and only the published package tags.
  [ "$(grep -c '^release list ' "${GH_STUB_LOG}")" -eq 1 ]
  [ "$(grep -c '^release view ' "${GH_STUB_LOG}")" -eq 2 ]
  ! grep -qF 'view v0.12.5 ' "${GH_STUB_LOG}"
  ! grep -qF 'view relate-extensions-egg-0.0.1 ' "${GH_STUB_LOG}"
  ! grep -qF 'view relate-extensions-greet-1.3.0-rc.1 ' "${GH_STUB_LOG}"
}

# ---------------------------------------------------------------------------
# Tests: the download URL template and the per-tarball fetches
# ---------------------------------------------------------------------------

@test "every asset name resolves through the ADR download URL template and every tarball is downloaded once" {
  run run_publisher "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  # One line per asset in the document: tag<TAB>asset<TAB>URL.
  run jq -r --arg tmpl "${DOWNLOAD_URL_TEMPLATE}" '
    .packages[] | .name as $name
    | .versions[] | .version as $version
    | .assets[]
    | "\(.tag)\t\(.asset)\t\($tmpl)/\(.tag)/\(.asset)"
  ' "${OUT_JSON}"
  echo "$output"
  [ "$status" -eq 0 ]
  [ -n "$output" ]
  [ "$(printf '%s\n' "$output" | wc -l | tr -d ' ')" -eq 4 ]

  while IFS=$'\t' read -r tag asset url; do
    [ -n "${tag}" ] || continue
    # The asset name is the URL's last path segment verbatim, so it
    # must not need escaping: no spaces, no slashes, no empty segments.
    if ! [[ "${url}" =~ ^https://github\.com/relatedb/relate/releases/download/[^/[:space:]]+/[^/[:space:]]+$ ]]; then
      echo "asset name does not resolve through the download URL template: ${tag} / ${asset} -> ${url}"
      return 1
    fi
  done <<< "$output"

  # Every tarball downloaded exactly once, for its checksum and its
  # manifest.
  [ "$(grep -c '^release download ' "${GH_STUB_LOG}")" -eq 4 ]
  run grep '^release download ' "${GH_STUB_LOG}"
  echo "$output"
  [ "$(printf '%s\n' "$output" | grep -cF -- '--pattern greet-1.2.3-')" -eq 2 ]
  [ "$(printf '%s\n' "$output" | grep -cF -- '--pattern greet-1.1.0-')" -eq 2 ]
  # Every download clobbers the loop's one shared output path — gh
  # refuses to overwrite it otherwise (the stub models the refusal, so
  # a --clobber-less publisher fails the happy path above; this pins
  # the flag in the argv the loop actually sent).
  [ "$(printf '%s\n' "$output" | grep -cF -- '--clobber')" -eq 4 ]
}

# ---------------------------------------------------------------------------
# Tests: the signing half
# ---------------------------------------------------------------------------

@test "the index is signed under the release namespace with the key from the environment" {
  run run_publisher "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  # One signing invocation: -Y sign -f <keyfile> -n relate-release
  # <the document copy>.
  [ "$(grep -c 'argv:-Y sign ' "${SSH_KEYGEN_LOG}")" -eq 1 ]
  grep -qF -- '-n relate-release' "${SSH_KEYGEN_LOG}"
  # The keyfile carried exactly the environment's private key (the
  # create-release SHA256SUMS signing shape: a 0600 temp file, removed
  # with the temp dir — the log is where the stub surfaced it).
  grep -qF "key-material:${FIXTURE_PRIVATE_KEY}" "${SSH_KEYGEN_LOG}"
}

@test "a missing signing key refuses and writes no output" {
  run run_publisher "${RELEASES_FIXTURE}" RELEASE_SIGNING_PRIVATE_KEY=""
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "::error::" <<< "$output"
  grep -qF "RELEASE_SIGNING_PRIVATE_KEY is not set" <<< "$output"
  [ ! -e "${OUT_JSON}" ]
  [ ! -e "${OUT_SIG}" ]
}

# ---------------------------------------------------------------------------
# Tests: edge cases and fail-closed behaviour
# ---------------------------------------------------------------------------

@test "a release list with no package releases yields an empty packages array" {
  run run_publisher "${EMPTY_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]

  run jq -c '{schema_version, generated_at, packages}' "${OUT_JSON}"
  [ "$output" = '{"schema_version":1,"generated_at":"'"${FIXED_GENERATED_AT}"'","packages":[]}' ]
  [ -s "${OUT_SIG}" ]
}

@test "re-running is idempotent apart from generated_at" {
  run run_publisher "${RELEASES_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]
  cp "${OUT_JSON}" "${TEST_TMP}/first.json"

  run run_publisher "${RELEASES_FIXTURE}" PUBLISH_EXTENSIONS_JSON_GENERATED_AT="2026-10-16T00:00:00Z"
  echo "$output"
  [ "$status" -eq 0 ]
  cp "${OUT_JSON}" "${TEST_TMP}/second.json"

  # generated_at moved...
  run jq -r '.generated_at' "${TEST_TMP}/second.json"
  [ "$output" = "2026-10-16T00:00:00Z" ]

  # ...and nothing else did.
  diff -u <(jq -S 'del(.generated_at)' "${TEST_TMP}/first.json") \
    <(jq -S 'del(.generated_at)' "${TEST_TMP}/second.json")
}

@test "gh release list failure fails closed and writes no output" {
  run run_publisher "${RELEASES_FIXTURE}" GH_STUB_FAIL=list
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "::error::" <<< "$output"
  [ ! -e "${OUT_JSON}" ]
  [ ! -e "${OUT_SIG}" ]
}

@test "gh release view failure fails closed and writes no output" {
  run run_publisher "${RELEASES_FIXTURE}" GH_STUB_FAIL=view
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "::error::" <<< "$output"
  [ ! -e "${OUT_JSON}" ]
  [ ! -e "${OUT_SIG}" ]
}

@test "gh release download failure fails closed and writes no output" {
  run run_publisher "${RELEASES_FIXTURE}" GH_STUB_FAIL=download
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "::error::" <<< "$output"
  [ ! -e "${OUT_JSON}" ]
  [ ! -e "${OUT_SIG}" ]
}

@test "an asset name that breaks the channel convention is refused" {
  run run_publisher "${BAD_ASSET_FIXTURE}"
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "::error::" <<< "$output"
  grep -qF "does not follow the channel convention" <<< "$output"
  [ ! -e "${OUT_JSON}" ]
  [ ! -e "${OUT_SIG}" ]
}

@test "the channel's own index files on a release are skipped, never indexed" {
  # ADR-0546: extensions.json + extensions.json.sig ride the extension
  # release as assets (relate-web pulls them from there); the publisher
  # must skip its own output instead of aborting on the convention.
  run run_publisher "${FIXTURES_DIR}/publish-extensions-json-releases-with-index.json"
  echo "$output"
  [ "$status" -eq 0 ]
  # The index matches the happy-path fixture exactly: the index assets
  # left no trace.
  run jq -e 'all(.packages[]; all(.versions[]; all(.assets[]; .asset | endswith(".tar.gz"))))' "${OUT_JSON}"
  [ "$status" -eq 0 ]
  run bash -c "cmp -s <(jq -S 'del(.generated_at)' '${OUT_JSON}') <(jq -S 'del(.generated_at)' '${EXPECTED_FIXTURE}')"
  [ "$status" -eq 0 ]
}

@test "a published tag claiming the channel prefix without the convention is refused" {
  run run_publisher "${BAD_TAG_FIXTURE}"
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "::error::" <<< "$output"
  grep -qF "does not follow the package-release convention" <<< "$output"
  [ ! -e "${OUT_JSON}" ]
  [ ! -e "${OUT_SIG}" ]
}

@test "a tarball whose manifest names another package is refused" {
  run run_publisher "${MISMATCH_FIXTURE}"
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "::error::" <<< "$output"
  grep -qF "carries a manifest naming" <<< "$output"
  [ ! -e "${OUT_JSON}" ]
  [ ! -e "${OUT_SIG}" ]
}

@test "a malformed pinned generated_at is refused" {
  run run_publisher "${RELEASES_FIXTURE}" PUBLISH_EXTENSIONS_JSON_GENERATED_AT="not-a-timestamp"
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "::error::" <<< "$output"
  [ ! -e "${OUT_JSON}" ]
  [ ! -e "${OUT_SIG}" ]
}

@test "missing output arguments are refused" {
  run env PATH="${STUBS_DIR}:${PATH}" bash "${SCRIPT}"
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "::error::" <<< "$output"
}
