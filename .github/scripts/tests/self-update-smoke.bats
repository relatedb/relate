# Behavioral tests for self-update-smoke.sh
#
# Locks the self-update smoke contract: the previous stable release
# resolves live from the release list (published, non-draft,
# non-prerelease, not the expected version, newest first) and only from
# the core tag namespace vX.Y.Z — extension-channel releases are never
# candidates (LABS-806); a previous
# release below the v0.12.6 self-update floor (ADR-0358) skips with a
# stated reason and exit 0 — and so does the first-release-ever case, a
# release list with no stable release other than the expected one, and
# the live-site currency gate's case, a site whose releases.json does
# not list the expected version as latest (Pages deploys are paused, so
# the release pipeline can no longer chain this smoke after the deploy);
# an unreachable or unparseable site read is never a skip — the smoke
# falls through and the upgrade judges itself; a
# failed gh release list read and a non-X.Y.Z PREV_TAG fail closed; and
# the happy path proves the before version, the upgrade, the after
# version, and that the relate on PATH stays untouched.
#
# The downloaded previous binary is checksum-verified against the
# release's SHA256SUMS manifest before anything executes: the happy
# path serves a manifest whose entry matches the fixture binary and
# proceeds, while a missing manifest, no entry for the asset, a
# malformed line, a mismatched entry, and a tampered binary each fail
# closed with exit 1 and zero spawns of the binary.

# The gh CLI is stubbed on PATH (no network): the stub comes from the
# shared gh-stub writer (gh-stub.bash — the same harness
# publish-releases-json.bats and publish-extensions-json.bats use) and
# serves a committed fixture release set as `gh release list` and
# writes a fake binary on `gh release download` whose `--version`
# reports the downloaded tag and whose `upgrade` rewrites that version
# to SMOKE_NEW_VERSION — the version the smoke must then verify — and
# serves a SHA256SUMS manifest checksumming that binary (GH_STUB_SUMS
# corrupts it each fail-closed way). The stub logs its argv so the
# tests can assert which gh calls happened (a skip must download
# nothing), and the fake binary logs every spawn to SMOKE_BIN_LOG, so
# a fail-closed run can prove the binary never executed. sha256_of is
# sourced from scripts/lib/common.sh — the same shared helper the
# smoke script itself sources, not a pasted copy — and exported to the
# stub (export -f) so its manifest arm checksums with it too.
#
# The live-site currency gate's curl is stubbed the same way (no
# network): it serves SMOKE_SITE_FIXTURE — a releases.json document
# (ADR-0250 §2 shape) whose latest.tag the gate reads — on the one
# pinned URL it accepts, exits 1 on SMOKE_CURL_FAIL=1, and logs its
# argv to SMOKE_CURL_LOG. The default fixture lists SMOKE_NEW_VERSION
# as latest (the site is current), so every pre-gate behavior test
# proceeds exactly as it did before the gate existed; the stale and
# malformed fixtures drive the gate's skip and fall-through.
#
# Local run:
#   bats .github/scripts/tests/self-update-smoke.bats
#
# Requirements:
#   bats-core >= 1.5.0  (brew install bats-core)
#   bash, jq

bats_require_minimum_version 1.5.0

load gh-stub

SCRIPT="$(cd "$(dirname "$BATS_TEST_FILENAME")/.." && pwd)/self-update-smoke.sh"
FIXTURES_DIR="$(cd "$(dirname "$BATS_TEST_FILENAME")" && pwd)/fixtures"

# The shared script helpers (sha256_of for the PATH-binary checksum).
REPO_ROOT="$(cd "$(dirname "$BATS_TEST_FILENAME")/../../.." && pwd)"
source "${REPO_ROOT}/scripts/lib/common.sh"

RELEASES_FIXTURE="${FIXTURES_DIR}/self-update-smoke-releases.json"
FIRST_RELEASE_FIXTURE="${FIXTURES_DIR}/self-update-smoke-releases-first.json"
PLUGIN_FIXTURE="${FIXTURES_DIR}/self-update-smoke-releases-plugin.json"
# The live-site currency gate's releases.json documents (ADR-0250 §2
# shape): latest.tag is what the gate reads — current lists
# SMOKE_NEW_VERSION (the site is current, the gate falls through),
# stale lists v0.13.1 (the site lags, the gate skips), malformed lists
# a non-triple (unreadable currency, never a skip), injected carries a
# newline-bearing tag (the CWE-117 shape — the regex must reject it
# before any log line).
SITE_CURRENT_FIXTURE="${FIXTURES_DIR}/self-update-smoke-site-current.json"
SITE_STALE_FIXTURE="${FIXTURES_DIR}/self-update-smoke-site-stale.json"
SITE_MALFORMED_FIXTURE="${FIXTURES_DIR}/self-update-smoke-site-malformed.json"
SITE_INJECTED_FIXTURE="${FIXTURES_DIR}/self-update-smoke-site-injected.json"

# The version the fake binary's `upgrade` installs. EXPECTED_VERSION is
# wired to it, so the smoke's after-version check must see exactly this.
SMOKE_NEW_VERSION="9.9.9"

# ---------------------------------------------------------------------------
# setup / teardown
# ---------------------------------------------------------------------------

setup() {
  TEST_TMP="$(mktemp -d)"
  STUBS_DIR="${TEST_TMP}/stubs"
  mkdir -p "${STUBS_DIR}"
  GH_STUB_LOG="${TEST_TMP}/gh.log"

  # gh stub — never touches the network. `release list` serves the
  # list-shaped JSON gh emits for --json tagName,publishedAt,isDraft,
  # isPrerelease; `release download` writes the fake binary plus its
  # version state into --dir, and on --pattern SHA256SUMS serves a
  # manifest checksumming that binary (GH_STUB_SUMS corrupts it each
  # fail-closed way). The harness around the arms (argv log,
  # GH_STUB_FAIL injection, unexpected-invocation fail) is the shared
  # writer's; GH_STUB_FAIL=list drives the fail-closed path.
  write_gh_stub "${STUBS_DIR}/gh" <<'ARMS'
  "release list")
    cat "${GH_STUB_RELEASES:?GH_STUB_RELEASES not set}"
    ;;
  "release download")
    tag="${3:?tag required}"
    pattern=""
    dir=""
    shift 3
    while [ $# -gt 0 ]; do
      case "$1" in
        --pattern) pattern="${2:?--pattern value required}"; shift 2 ;;
        --dir) dir="${2:?--dir value required}"; shift 2 ;;
        *) shift ;;
      esac
    done
    [ -n "${pattern}" ] || { echo "gh stub: --pattern missing" >&2; exit 1; }
    [ -n "${dir}" ] || { echo "gh stub: --dir missing" >&2; exit 1; }
    case "$pattern" in
      SHA256SUMS)
        # The manifest the smoke must verify its binary against. The
        # true entry is the line the binary arm recorded in .smoke-sum
        # (the smoke downloads the binary before the manifest, so it
        # is already on disk); GH_STUB_SUMS serves it intact (ok, the
        # default) or corrupts it one fail-closed way.
        [ "${GH_STUB_SUMS:-ok}" != "missing" ] || { echo "gh stub: simulated missing SHA256SUMS asset" >&2; exit 1; }
        line="$(cat "${dir}/.smoke-sum" 2>/dev/null || true)"
        [ -n "${line}" ] || { echo "gh stub: no .smoke-sum in ${dir} — the binary arm must run first" >&2; exit 1; }
        sum="${line%% *}"
        name="${line##* }"
        case "${GH_STUB_SUMS:-ok}" in
          ok)
            printf '# relate-release-version: %s\n%s\n' "${tag}" "${line}" > "${dir}/SHA256SUMS"
            ;;
          no-entry)
            # Well-formed lines, none naming the downloaded asset.
            printf '# relate-release-version: %s\n%s  relate-other-platform\n' \
              "${tag}" "${sum}" > "${dir}/SHA256SUMS"
            ;;
          malformed)
            # A hash that is not 64-hex: the line fails the shape check.
            printf 'deadbeef  %s\n' "${name}" > "${dir}/SHA256SUMS"
            ;;
          mismatch)
            # A different but well-formed entry for the asset: the
            # checksum of a different file, so it can never match.
            printf '# relate-release-version: %s\n%s  %s\n' \
              "${tag}" "$(sha256_of "${dir}/.smoke-version")" "${name}" > "${dir}/SHA256SUMS"
            ;;
          wrong-version)
            # A well-formed header naming another release: the manifest
            # does not belong to the tag whose assets were downloaded.
            printf '# relate-release-version: v0.0.1\n%s\n' "${line}" > "${dir}/SHA256SUMS"
            ;;
          tampered)
            # The manifest records the binary's true checksum, then
            # the binary itself is corrupted after the fact.
            printf '# relate-release-version: %s\n%s\n' "${tag}" "${line}" > "${dir}/SHA256SUMS"
            printf 'tampered' >> "${dir}/${name}"
            ;;
          *)
            echo "gh stub: unknown GH_STUB_SUMS=${GH_STUB_SUMS}" >&2
            exit 1
            ;;
        esac
        ;;
      *)
        # Fake binary: `--version` reports the state file, `upgrade`
        # rewrites it to SMOKE_NEW_VERSION (passed through the smoke's
        # env, the same way the real GITHUB_TOKEN reaches the inner
        # upgrade). Every spawn appends to SMOKE_BIN_LOG first, so a
        # fail-closed test can prove the binary never executed.
        cat > "${dir}/${pattern}" <<'FAKEBIN'
#!/usr/bin/env bash
set -euo pipefail
state="$(dirname "$0")/.smoke-version"
echo "spawned $*" >> "${SMOKE_BIN_LOG:?SMOKE_BIN_LOG not set}"
case "${1:-}" in
  --version)
    cat "$state"
    ;;
  upgrade)
    if [ "${2:-}" = "--check" ]; then
      # The upgraded copy's own check path.
      printf 'latest release:  %s\n' "${SMOKE_NEW_VERSION:?SMOKE_NEW_VERSION not set}"
      printf 'relate is up to date\n'
      exit 0
    fi
    printf 'relate %s\n' "${SMOKE_NEW_VERSION:?SMOKE_NEW_VERSION not set}" > "$state"
    printf 'upgrade: installed v%s\n' "${SMOKE_NEW_VERSION:?}"
    ;;
  *)
    echo "fake relate: unexpected invocation: $*" >&2
    exit 1
    ;;
esac
FAKEBIN
        printf 'relate %s\n' "${tag#v}" > "${dir}/.smoke-version"
        # The manifest arm serves this as the asset's true checksum line.
        printf '%s  %s\n' "$(sha256_of "${dir}/${pattern}")" "${pattern}" > "${dir}/.smoke-sum"
        ;;
    esac
    ;;
ARMS

  # curl stub — the live-site currency gate's read of the website's
  # releases.json (no network): serves SMOKE_SITE_FIXTURE on the one
  # pinned URL it accepts (any other URL fails loudly — the pin on
  # releases_json.rs's constant must not drift), exits 1 on
  # SMOKE_CURL_FAIL=1 (an unreachable site is never a skip), and logs
  # its argv to SMOKE_CURL_LOG so a test can assert the gate read the
  # site before skipping. The default fixture below lists
  # SMOKE_NEW_VERSION as latest, so every pre-gate behavior test
  # proceeds exactly as it did before the gate existed.
  SMOKE_CURL_LOG="${TEST_TMP}/curl.log"
  export SMOKE_CURL_LOG
  cat > "${STUBS_DIR}/curl" <<'CURLSTUB'
#!/usr/bin/env bash
set -euo pipefail
echo "curl $*" >> "${SMOKE_CURL_LOG:?SMOKE_CURL_LOG not set}"
# The one URL the smoke may read: the pinned website releases.json.
case " $* " in
  *" https://relatedb.ai/releases.json "*) ;;
  *) echo "curl stub: unexpected URL in: $*" >&2; exit 1 ;;
esac
[ "${SMOKE_CURL_FAIL:-}" != "1" ] || { echo "curl stub: simulated unreachable site" >&2; exit 1; }
cat "${SMOKE_SITE_FIXTURE:?SMOKE_SITE_FIXTURE not set}"
CURLSTUB
  chmod +x "${STUBS_DIR}/curl"
  export SMOKE_SITE_FIXTURE="${SITE_CURRENT_FIXTURE}"

  # A relate on PATH the smoke must leave untouched (its checksum
  # is taken before and after the run).
  printf '#!/usr/bin/env bash\necho "path relate (must stay untouched)"\n' \
    > "${STUBS_DIR}/relate"
  chmod +x "${STUBS_DIR}/relate"

  # Every spawn of the fake binary appends here: a fail-closed test
  # asserts the file stays empty — the binary never executed.
  # Truncated per test, like GH_STUB_LOG.
  SMOKE_BIN_LOG="${TEST_TMP}/bin.log"
  : > "${SMOKE_BIN_LOG}"
  export SMOKE_BIN_LOG

  # The stub's arms checksum with the same shared helper the smoke
  # script itself sources: exported through the environment to the
  # stub's bash, never an inline copy.
  export -f sha256_of

  export GH_STUB_RELEASES="${RELEASES_FIXTURE}"
  export GH_STUB_LOG
}

teardown() {
  rm -rf "${TEST_TMP}"
}

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Run the smoke with the gh stub on PATH and the wiring pinned:
# EXPECTED_VERSION (and the fake upgrade's target) is SMOKE_NEW_VERSION,
# GITHUB_TOKEN a stub value. Extra NAME=VALUE args (PREV_TAG=...,
# GH_STUB_SUMS=..., GH_STUB_FAIL=...) override.
run_smoke() {
  env PATH="${STUBS_DIR}:${PATH}" \
    SMOKE_NEW_VERSION="${SMOKE_NEW_VERSION}" \
    EXPECTED_VERSION="${SMOKE_NEW_VERSION}" \
    GITHUB_TOKEN="stub-token" \
    "$@" \
    bash "${SCRIPT}"
}

# One shared body for the fail-closed SHA256SUMS integrity paths: the
# named GH_STUB_SUMS corruption of the v0.13.0 manifest must abort the
# smoke with the expected FAIL message — after both downloads happened
# (the failure came from the verification, not from never
# downloading), and with zero spawns of the binary.
assert_integrity_fail() {
  local mode="$1" message="$2"
  run run_smoke PREV_TAG="v0.13.0" GH_STUB_SUMS="${mode}"
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "${message}" <<<"$output"
  grep -qE '^release download v0\.13\.0 ' "${GH_STUB_LOG}"
  grep -q -- '--pattern SHA256SUMS' "${GH_STUB_LOG}"
  [ ! -s "${SMOKE_BIN_LOG}" ]
}

# ---------------------------------------------------------------------------
# Tests: the ADR-0358 floor
# ---------------------------------------------------------------------------

@test "PREV_TAG below the v0.12.6 floor skips with exit 0 and downloads nothing" {
  run run_smoke PREV_TAG="v0.12.5"
  echo "$output"
  [ "$status" -eq 0 ]
  grep -qF "SKIP: v0.12.5 predates v0.12.6" <<<"$output"
  # A pinned PREV_TAG resolves nothing live: gh is never called.
  [ ! -s "${GH_STUB_LOG}" ]
}

@test "PREV_TAG at the floor (v0.12.6) proceeds to the download and upgrade" {
  run run_smoke PREV_TAG="v0.12.6"
  echo "$output"
  [ "$status" -eq 0 ]
  ! grep -qF "SKIP" <<<"$output"
  grep -qE '^release download v0\.12\.6 ' "${GH_STUB_LOG}"
  grep -qF "PASS: v0.12.6 -> v${SMOKE_NEW_VERSION} self-upgrade verified" <<<"$output"
}

@test "PREV_TAG above the floor (v0.13.0) proceeds to the download and upgrade" {
  run run_smoke PREV_TAG="v0.13.0"
  echo "$output"
  [ "$status" -eq 0 ]
  ! grep -qF "SKIP" <<<"$output"
  grep -qE '^release download v0\.13\.0 ' "${GH_STUB_LOG}"
  grep -qF "PASS: v0.13.0 -> v${SMOKE_NEW_VERSION} self-upgrade verified" <<<"$output"
}

@test "a PREV_TAG that does not parse as X.Y.Z fails loudly" {
  run run_smoke PREV_TAG="v0.12.6-rc.1"
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "does not parse as X.Y.Z" <<<"$output"
  [ ! -s "${GH_STUB_LOG}" ]
}

# ---------------------------------------------------------------------------
# Tests: the previous-release resolution
# ---------------------------------------------------------------------------

@test "no previous stable release (the first release ever) skips with exit 0 and downloads nothing" {
  export GH_STUB_RELEASES="${FIRST_RELEASE_FIXTURE}"
  run run_smoke
  echo "$output"
  [ "$status" -eq 0 ]
  grep -qF "SKIP: v${SMOKE_NEW_VERSION} is the first stable core release ever" <<<"$output"
  grep -q '^release list ' "${GH_STUB_LOG}"
  ! grep -q '^release download ' "${GH_STUB_LOG}"
}

@test "a failed gh release list fails closed with exit 1" {
  run run_smoke GH_STUB_FAIL=list
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "gh release list failed" <<<"$output"
  ! grep -q '^release download ' "${GH_STUB_LOG}"
}

@test "extension-channel releases are never previous-release candidates (LABS-806)" {
  # The plugin fixture carries two relate-extensions-* releases
  # published NEWER than every core release — exactly the v0.15.0
  # incident. The resolver must skip them (core namespace only,
  # ADR-0250 stable shape) and still resolve v0.13.1, and the smoke
  # must pass end-to-end without ever downloading a plugin release.
  export GH_STUB_RELEASES="${PLUGIN_FIXTURE}"
  run run_smoke
  echo "$output"
  [ "$status" -eq 0 ]

  grep -qF "previous stable release: v0.13.1" <<<"$output"
  grep -qF "PASS: v0.13.1 -> v${SMOKE_NEW_VERSION} self-upgrade verified" <<<"$output"
  ! grep -q '^release download relate-extensions' "${GH_STUB_LOG}"
}

@test "missing GITHUB_TOKEN fails closed" {
  run env -u GITHUB_TOKEN PATH="${STUBS_DIR}:${PATH}" \
    EXPECTED_VERSION="${SMOKE_NEW_VERSION}" bash "${SCRIPT}"
  echo "$output"
  [ "$status" -eq 1 ]
  grep -qF "GITHUB_TOKEN is not set" <<<"$output"
  [ ! -s "${GH_STUB_LOG}" ]
}

# ---------------------------------------------------------------------------
# Tests: the live-site currency gate (Pages deploys paused)
# ---------------------------------------------------------------------------

@test "a live site that does not list the expected version skips with exit 0 and downloads nothing" {
  # The stale fixture serves v0.13.1 as latest — the site never got the
  # new release's deploy — so the gate must skip before any download,
  # the same stated-reason contract as the floor and first-release
  # skips.
  run run_smoke PREV_TAG="v0.13.0" SMOKE_SITE_FIXTURE="${SITE_STALE_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]
  grep -qF "SKIP: the live site's releases.json lists v0.13.1 as latest, not v${SMOKE_NEW_VERSION}" <<<"$output"
  # The gate read the site before skipping — one curl, the pinned URL.
  grep -qF "curl -fsSL https://relatedb.ai/releases.json" "${SMOKE_CURL_LOG}"
  # ...and the skip downloaded nothing and executed nothing.
  ! grep -q '^release download ' "${GH_STUB_LOG}"
  [ ! -s "${SMOKE_BIN_LOG}" ]
}

@test "a live site that lists the expected version proceeds to the download and upgrade" {
  # The default current fixture lists the expected version as latest,
  # so the gate falls through and the smoke runs its full path.
  run run_smoke PREV_TAG="v0.13.0"
  echo "$output"
  [ "$status" -eq 0 ]
  ! grep -qF "SKIP" <<<"$output"
  grep -qE '^release download v0\.13\.0 ' "${GH_STUB_LOG}"
  grep -qF "PASS: v0.13.0 -> v${SMOKE_NEW_VERSION} self-upgrade verified" <<<"$output"
}

@test "an unreachable site is never a skip: the smoke falls through and runs the upgrade" {
  # SMOKE_CURL_FAIL=1 makes the stub exit 1 — the site is down. The
  # real inner upgrade would fail on that for real; the fake binary
  # here does not read the site, so the lock is the fall-through
  # itself: no SKIP line, the full happy path, the attempt logged.
  run run_smoke PREV_TAG="v0.13.0" SMOKE_CURL_FAIL=1
  echo "$output"
  [ "$status" -eq 0 ]
  ! grep -qF "SKIP" <<<"$output"
  grep -qF "curl -fsSL https://relatedb.ai/releases.json" "${SMOKE_CURL_LOG}"
  grep -qF "PASS: v0.13.0 -> v${SMOKE_NEW_VERSION} self-upgrade verified" <<<"$output"
}

@test "a latest tag that does not parse as X.Y.Z is never a skip (unreadable currency falls through)" {
  run run_smoke PREV_TAG="v0.13.0" SMOKE_SITE_FIXTURE="${SITE_MALFORMED_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]
  ! grep -qF "SKIP" <<<"$output"
  grep -qF "PASS: v0.13.0 -> v${SMOKE_NEW_VERSION} self-upgrade verified" <<<"$output"
}

@test "a tag carrying injected log content is never logged (the gate's regex is the log boundary)" {
  # The CWE-117 shape: a newline-bearing tag would render as its own
  # log entry if it ever reached the SKIP line. The gate's regex
  # rejects it first — nothing about the fetched value is printed and
  # the smoke falls through to judge the upgrade itself.
  run run_smoke PREV_TAG="v0.13.0" SMOKE_SITE_FIXTURE="${SITE_INJECTED_FIXTURE}"
  echo "$output"
  [ "$status" -eq 0 ]
  ! grep -qF "SKIP" <<<"$output"
  ! grep -qF "INJECTED_LOG_ENTRY" <<<"$output"
  grep -qF "PASS: v0.13.0 -> v${SMOKE_NEW_VERSION} self-upgrade verified" <<<"$output"
}

@test "the smoke's pinned releases.json URL matches the CLI's constant" {
  # bash cannot read releases_json.rs's RELEASES_JSON_URL, so the smoke
  # pins it — this lock keeps the pin and the constant from drifting
  # apart.
  grep -qF 'const RELEASES_JSON_URL: &str = "https://relatedb.ai/releases.json";' \
    "${REPO_ROOT}/surfaces/relate-cli/src/upgrade_cmd/releases_json.rs"
  grep -qF 'RELEASES_JSON_URL="https://relatedb.ai/releases.json"' "${SCRIPT}"
}

# ---------------------------------------------------------------------------
# Tests: the happy path
# ---------------------------------------------------------------------------

@test "happy path: live-resolved previous release upgrades to the expected version, PATH binary untouched" {
  local before_sha
  before_sha="$(sha256_of "${STUBS_DIR}/relate")"

  run run_smoke
  echo "$output"
  [ "$status" -eq 0 ]

  # The resolver picked the newest stable tag that is not the expected
  # version — v0.13.1 in the fixture, not the newer prerelease or draft.
  grep -qF "previous stable release: v0.13.1" <<<"$output"
  grep -qE '^release download v0\.13\.1 ' "${GH_STUB_LOG}"

  # The manifest came down with the binary, and its entry — the
  # fixture binary's own checksum — verified before anything ran.
  grep -q -- '--pattern SHA256SUMS' "${GH_STUB_LOG}"
  grep -qE 'verified relate-[a-z0-9-]+ against SHA256SUMS from v0\.13\.1' <<<"$output"

  # The before version (the downloaded copy reports the previous
  # release) and the after version (the PASS line carries the upgrade).
  grep -qF "downloaded copy reports: relate 0.13.1" <<<"$output"
  grep -qF "PASS: v0.13.1 -> v${SMOKE_NEW_VERSION} self-upgrade verified" <<<"$output"
  [ -s "${SMOKE_BIN_LOG}" ]


  # The relate on PATH stays untouched.
  [ "$(sha256_of "${STUBS_DIR}/relate")" = "${before_sha}" ]
}

# ---------------------------------------------------------------------------
# Tests: the SHA256SUMS verification of the downloaded binary
# ---------------------------------------------------------------------------

@test "a tampered binary fails the checksum verification without executing" {
  assert_integrity_fail "tampered" "checksum mismatch for relate-"
}

@test "a mismatched manifest entry fails the checksum verification without executing" {
  assert_integrity_fail "mismatch" "checksum mismatch for relate-"
}

@test "a missing SHA256SUMS asset fails closed without executing" {
  assert_integrity_fail "missing" "has no asset named SHA256SUMS"
}

@test "a manifest with no entry for the asset fails closed without executing" {
  assert_integrity_fail "no-entry" "SHA256SUMS from v0.13.0 has no entry for"
}

@test "a malformed manifest line fails closed without executing" {
  assert_integrity_fail "malformed" "malformed SHA256SUMS line from v0.13.0"
}

@test "a manifest naming another release fails closed without executing" {
  assert_integrity_fail "wrong-version" "names version 'v0.0.1'"
}
