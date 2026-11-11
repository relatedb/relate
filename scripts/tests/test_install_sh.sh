#!/bin/sh
# Offline, hermetic test harness for distribution/install.sh.
#
# The installer is run with HOME, TMPDIR and PATH pointed at a sandbox whose
# stub-bin/ shadows `curl` (always) and `uname` (to fix the OS/arch under
# test). The stub curl never touches the network: it logs its argv (one arg
# per line, calls separated by an =CALL= marker) and serves fixtures keyed on
# the request URL, with failure modes selected via STUB_MODE. The harness
# writes only inside the sandbox and removes it on exit, failure included.
# The fixture/stub factories and the per-case run harness live in
# scripts/tests/lib/install_sh_harness.sh; this file holds the drift guards
# and the cases.
#
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
INSTALLER="$ROOT/distribution/install.sh"

# Shared script helpers (sha256_of, for the SHA256SUMS fixture below;
# common.sh's bodies are POSIX sh, so `.` works here).
. "$ROOT/scripts/lib/common.sh"
# The shared TAP harness (LABS-622): tap_check/tap_skip/tap_done,
# assert_same, the counters, output format, and sandbox policy spelled
# once.
. "$ROOT/scripts/tests/lib/tap.sh"

tap_sandbox relate-install-sh-test

stubbin="$sandbox/stub-bin"
fixtures="$sandbox/fixtures"
itmp="$sandbox/itmp"
mkdir -p "$stubbin" "$fixtures" "$itmp"

# The harness machinery (fixtures, stubs, run helpers) comes from the
# sourced lib; the case script below owns only the guards and assertions.
. "$ROOT/scripts/tests/lib/install_sh_harness.sh"

# --- 0. Drift guards ------------------------------------------------------------

# The pinned SIGNING_KEY in install.sh must match the canonical public key
# committed at distribution/release-signing-key.pub; a release that rotates one
# without the other breaks verification for every user.
pinned_key=$(sed -n "s/^SIGNING_KEY='\\(.*\\)'\$/\\1/p" "$INSTALLER")
pub_key=$(sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' \
	"$ROOT/distribution/release-signing-key.pub")
assert_same "drift guard: install.sh's pinned SIGNING_KEY matches distribution/release-signing-key.pub" \
	"$pinned_key" "$pub_key"

# The ADR-0457 wire contract exists in four spellings: the release producer's
# printf, manifest.rs's constants, install.sh's copy, and this harness's
# fixture writer. The prefix is the grammar itself (one space after the colon
# is load-bearing), so all four must carry the identical literal; the ceiling
# must agree too, or the two parsers accept different legacy windows.
manifest_rs="$ROOT/surfaces/relate-cli/src/upgrade_cmd/manifest.rs"
installer_ceiling=$(sed -n 's/^LEGACY_MANIFEST_CEILING=//p' "$INSTALLER")
rust_ceiling=$(sed -n 's/^const LEGACY_MANIFEST_CEILING: (u64, u64, u64) = (\([0-9][0-9]*\), \([0-9][0-9]*\), \([0-9][0-9]*\));$/\1.\2.\3/p' \
	"$manifest_rs")
assert_same "drift guard: install.sh's LEGACY_MANIFEST_CEILING matches manifest.rs (ADR-0457)" \
	"$installer_ceiling" "$rust_ceiling"

# Prefix parity: the producer's printf, install.sh's assignment and the
# harness fixture writer's constant must all equal the literal manifest.rs
# carries, and install.sh's case pattern must match the header through that
# one variable rather than a re-spelled literal.
installer_prefix=$(sed -n "s/^SUMS_VERSION_PREFIX='\\(.*\\)'\$/\\1/p" "$INSTALLER")
rust_prefix=$(sed -n 's/^const SUMS_VERSION_PREFIX: &str = "\(.*\)";$/\1/p' \
	"$manifest_rs")
release_prefix=$(sed -n "s/^[[:space:]]*printf '\(.*\)%s\\\\n'.*> SHA256SUMS\$/\1/p" \
	"$ROOT/.github/workflows/release.yml")
assert_same "drift guard: the ADR-0457 sums header prefix is one literal in install.sh, manifest.rs, release.yml and the harness fixture" \
	"$installer_prefix" "$rust_prefix" "$release_prefix" "$SUMS_VERSION_PREFIX"
grep -qF 'case "$first_line" in' "$INSTALLER" &&
	grep -qF '"$SUMS_VERSION_PREFIX"*)' "$INSTALLER"
tap_check "drift guard: install.sh matches the header through the shared prefix" $?

# The installer resolves its release tag from the same releases.json the
# CLI's upgrade reads (ADR-0571): one canonical URL, spelled once per
# side — install.sh's assignment and releases_json.rs's RELEASES_JSON_URL
# constant — held equal here so the two consumers cannot drift onto
# different metadata sources.
releases_rs="$ROOT/surfaces/relate-cli/src/upgrade_cmd/releases_json.rs"
installer_releases_url=$(sed -n "s/^RELEASES_JSON_URL='\\(.*\\)'\$/\\1/p" "$INSTALLER")
rust_releases_url=$(sed -n "s/^const RELEASES_JSON_URL: &str = \"\\(.*\\)\";\$/\\1/p" \
	"$releases_rs")
assert_same "drift guard: install.sh's RELEASES_JSON_URL matches releases_json.rs (ADR-0571)" \
	"$installer_releases_url" "$rust_releases_url"

# The override env var is the other half of that seam (ADR-0571):
# releases_json.rs reads it through RELEASES_JSON_URL_ENV and install.sh
# references it on the metadata_url line — one spelling, held equal here
# so the two consumers cannot drift onto different seam names.
rust_releases_env=$(sed -n 's/^const RELEASES_JSON_URL_ENV: &str = "\(.*\)";$/\1/p' \
	"$releases_rs")
installer_releases_env=$(sed -n 's/^metadata_url=${\([A-Z_]*\):-\$RELEASES_JSON_URL}$/\1/p' \
	"$INSTALLER")
assert_same "drift guard: install.sh's override env var matches releases_json.rs (ADR-0571)" \
	"$rust_releases_env" "$installer_releases_env"

# The GitHub API's latest-release endpoint is gone from the installer
# entirely (ADR-0571): after an extension release it names no product
# release, so resolution may not consult it in any spelling.
! grep -q 'releases/latest' "$INSTALLER"
tap_check "drift guard: install.sh requests no GitHub latest-release URL" $?

# The legacy-tag fixture (built below) derives from the ceiling the guard
# just extracted, so a constant change cannot leave the fixture on a stale
# hardcoded tag.
LEGACY_MANIFEST_TAG="v$rust_ceiling"

# The publisher-fixture documents (built below) name the committed
# publisher fixture's latest.tag — derived here from that fixture rather
# than hand-spelled, so regenerating it with a newer release moves the
# tag with it. The sed range is latest_tag_of's own grammar: the
# top-level latest object's four-space-indented tag line, closed at the
# object's two-space brace before the prerelease slot's tag. A null
# latest matches nothing, failing the non-empty guard below.
publisher_fixture="$ROOT/.github/scripts/tests/fixtures/publish-releases-json-expected.json"
PUBLISHER_FIXTURE_TAG=$(sed -n '/^  "latest": [{]$/,/^  [}]/ s/^    "tag": "\(.*\)",$/\1/p' \
	"$publisher_fixture")
[ -n "$PUBLISHER_FIXTURE_TAG" ]
tap_check "drift guard: the publisher fixture carries a latest.tag the installer fixtures follow" $?

# Build the fixtures and PATH stubs only now that their legacy-tag input is
# pinned to the guarded ceiling.
install_sh_fixtures
install_sh_stubs

# --- 1. OS/arch mapping -------------------------------------------------------

reset_case
install_run osarch-darwin
[ "$RUN_STATUS" -eq 0 ]
tap_check "Darwin/arm64: installer exits 0" $?
grep -q "releases/assets/$ASSET_ID_DARWIN\$" "$RUN_LOG"
tap_check "Darwin/arm64: downloads the relate-darwin-arm64 asset id" $?
! grep -q "releases/assets/$ASSET_ID_LINUX\$" "$RUN_LOG"
tap_check "Darwin/arm64: does not download the linux-amd64 asset id" $?
cmp -s "$RUN_HOME/.local/bin/relate" "$fixtures/relate-darwin-arm64"
tap_check "Darwin/arm64: installed content is the darwin-arm64 fixture" $?

reset_case
STUB_UNAME_S=Linux
STUB_UNAME_M=x86_64
install_run osarch-linux
[ "$RUN_STATUS" -eq 0 ]
tap_check "Linux/x86_64: installer exits 0" $?
grep -q "releases/assets/$ASSET_ID_LINUX\$" "$RUN_LOG"
tap_check "Linux/x86_64: downloads the relate-linux-amd64 asset id" $?
cmp -s "$RUN_HOME/.local/bin/relate" "$fixtures/relate-linux-amd64"
tap_check "Linux/x86_64: installed content is the linux-amd64 fixture" $?

reset_case
STUB_UNAME_S=FreeBSD
install_run osarch-freebsd
[ "$RUN_STATUS" -ne 0 ]
tap_check "unsupported OS (FreeBSD): installer dies non-zero" $?
grep -q "unsupported OS" "$RUN_ERR"
tap_check "unsupported OS (FreeBSD): error mentions unsupported OS" $?

reset_case
STUB_UNAME_M=i386
install_run osarch-i386
[ "$RUN_STATUS" -ne 0 ]
tap_check "unsupported arch (i386): installer dies non-zero" $?
grep -q "unsupported architecture" "$RUN_ERR"
tap_check "unsupported arch (i386): error mentions unsupported architecture" $?

# --- 2. GITHUB_TOKEN handling -------------------------------------------------

reset_case
GITHUB_TOKEN=test-token
install_run token-set
[ "$RUN_STATUS" -eq 0 ]
tap_check "token set: installer exits 0" $?
calls_to_url_have 'api\.github\.com' "Authorization: Bearer test-token"
tap_check "token set: every api.github.com call carries Authorization: Bearer test-token" $?
calls_to_url_lack 'releases\.json' "Authorization: Bearer test-token"
tap_check "token set: the releases.json request carries no Authorization header" $?
calls_to_url_have '/releases/assets/' "Accept: application/octet-stream"
tap_check "token set: asset downloads carry Accept: application/octet-stream" $?
[ "$(asset_call_count)" -eq 3 ]
tap_check "token set: exactly three asset downloads (binary + SHA256SUMS + SHA256SUMS.sig)" $?

reset_case
install_run token-unset
[ "$RUN_STATUS" -eq 0 ]
tap_check "token unset: installer exits 0" $?
! grep -q "Authorization" "$RUN_LOG"
tap_check "token unset: no Authorization header in any curl call" $?

# --- 3. Happy path ------------------------------------------------------------

reset_case
install_run happy
[ "$RUN_STATUS" -eq 0 ]
tap_check "happy path: installer exits 0" $?
[ -f "$RUN_HOME/.local/bin/relate" ]
tap_check "happy path: \$HOME/.local/bin/relate exists" $?
[ -x "$RUN_HOME/.local/bin/relate" ]
tap_check "happy path: installed file is executable" $?
cmp -s "$RUN_HOME/.local/bin/relate" "$fixtures/relate-darwin-arm64"
tap_check "happy path: installed content matches the fixture" $?
[ "$(ls -A "$RUN_HOME/.local/bin")" = "relate" ]
tap_check "happy path: bin dir holds exactly the installed binary (no temp leftovers)" $?
grep -q '^-Y$' "$RUN_KEYGEN_LOG" && grep -q '^verify$' "$RUN_KEYGEN_LOG" &&
	grep -q '^relate-release$' "$RUN_KEYGEN_LOG"
tap_check "happy path: ssh-keygen -Y verify ran under the relate-release identity" $?
sig_line=$(grep -n "releases/assets/$ASSET_ID_SIG\$" "$RUN_LOG" | head -n 1 | cut -d: -f1)
bin_line=$(grep -n "releases/assets/$ASSET_ID_DARWIN\$" "$RUN_LOG" | head -n 1 | cut -d: -f1)
[ -n "$sig_line" ] && [ -n "$bin_line" ] && [ "$sig_line" -lt "$bin_line" ]
tap_check "happy path: SHA256SUMS.sig is downloaded before the binary asset" $?

# --- 3a. Release resolution (releases.json, ADR-0571) -------------------------
# The stub's latest-endpoint arm serves the relate-extensions-dungeon-0.1.0
# release in every mode — the repository's newest full release after an
# extension ship — so every case already runs with the trap armed. These
# cases pin the resolution contract explicitly.

reset_case
install_run dungeon-latest
[ "$RUN_STATUS" -eq 0 ]
tap_check "extension release at the latest endpoint: installer exits 0" $?
cmp -s "$RUN_HOME/.local/bin/relate" "$fixtures/relate-darwin-arm64"
tap_check "extension release at the latest endpoint: installed content is the product binary fixture" $?
! grep -q 'releases/latest' "$RUN_LOG"
tap_check "extension release at the latest endpoint: the stub log carries no latest-endpoint request" $?
grep -qF "releases/tags/$TEST_TAG" "$RUN_LOG"
tap_check "extension release at the latest endpoint: the per-tag endpoint is asked for the product tag" $?

# The committed publisher fixture — the document publish-releases-json.sh's
# own suite compares its output against — served byte-for-byte as the
# metadata document (publisher_doc mode, with the matching per-tag release
# fixture and manifest variant): the end-to-end pin that the serialization
# the producer writes is the grammar latest_tag_of reads. When the two
# drift apart, this is the case that fails.
reset_case
STUB_MODE=publisher_doc
install_run publisher-fixture
[ "$RUN_STATUS" -eq 0 ]
tap_check "publisher fixture as releases.json: installer exits 0" $?
grep -qF "releases/tags/$PUBLISHER_FIXTURE_TAG" "$RUN_LOG"
tap_check "publisher fixture as releases.json: resolves the fixture's latest tag" $?
cmp -s "$RUN_HOME/.local/bin/relate" "$fixtures/relate-darwin-arm64"
tap_check "publisher fixture as releases.json: installs that release's product binary" $?

reset_case
RELATE_RELEASES_JSON_URL=https://example.test/releases.json
install_run releases-url-override
[ "$RUN_STATUS" -eq 0 ]
tap_check "metadata URL override: installer exits 0" $?
grep -qF "https://example.test/releases.json" "$RUN_LOG"
tap_check "metadata URL override: the override URL is fetched" $?
! grep -qF "$installer_releases_url" "$RUN_LOG"
tap_check "metadata URL override: the canonical URL is not fetched" $?

reset_case
RELATE_RELEASES_JSON_URL=
install_run releases-url-override-empty
[ "$RUN_STATUS" -eq 0 ]
tap_check "metadata URL override, empty value: installer exits 0" $?
grep -qF "$installer_releases_url" "$RUN_LOG"
tap_check "metadata URL override, empty value: the canonical URL is fetched (empty means default)" $?

# --- 3b. Signature verification ------------------------------------------------

reset_case
STUB_MODE=missing_sig
install_run sig-missing
[ "$RUN_STATUS" -ne 0 ]
tap_check "missing sig: installer dies non-zero" $?
grep -q "has no asset named SHA256SUMS.sig" "$RUN_ERR"
tap_check "missing sig: error names the missing SHA256SUMS.sig asset" $?
[ "$(asset_call_count)" -eq 0 ]
tap_check "missing sig: no /releases/assets/ download is attempted" $?
[ ! -e "$RUN_HOME/.local/bin/relate" ]
tap_check "missing sig: nothing installed" $?

reset_case
STUB_MODE=sig_invalid
install_run sig-invalid
[ "$RUN_STATUS" -ne 0 ]
tap_check "verify failure: installer dies non-zero" $?
grep -q "signature verification failed" "$RUN_ERR"
tap_check "verify failure: error mentions signature verification" $?
[ "$(asset_call_count)" -eq 2 ]
tap_check "verify failure: only SHA256SUMS and SHA256SUMS.sig were downloaded" $?
! grep -q "releases/assets/$ASSET_ID_DARWIN\$" "$RUN_LOG"
tap_check "verify failure: the binary asset is not downloaded before verification" $?
[ ! -e "$RUN_HOME/.local/bin/relate" ]
tap_check "verify failure: nothing at the final install path" $?

# --- 3b'. Signed version binding (ADR-0457) -----------------------------------

reset_case
STUB_MODE=sums_no_version
install_run sums-no-version
[ "$RUN_STATUS" -ne 0 ]
tap_check "no version header above the legacy ceiling: installer dies non-zero" $?
grep -q "has no signed version header" "$RUN_ERR"
tap_check "no version header above the legacy ceiling: error names the missing header" $?
[ "$(asset_call_count)" -eq 2 ]
tap_check "no version header above the legacy ceiling: only SHA256SUMS and SHA256SUMS.sig were downloaded" $?
! grep -q "releases/assets/$ASSET_ID_DARWIN\$" "$RUN_LOG"
tap_check "no version header above the legacy ceiling: the binary is not downloaded" $?
[ ! -e "$RUN_HOME/.local/bin/relate" ]
tap_check "no version header above the legacy ceiling: nothing installed" $?

reset_case
STUB_MODE=sums_wrong_version
install_run sums-wrong-version
[ "$RUN_STATUS" -ne 0 ]
tap_check "mismatched version header: installer dies non-zero" $?
grep -q "SHA256SUMS is for $SUMS_WRONG_TAG, not the requested release $TEST_TAG" "$RUN_ERR"
tap_check "mismatched version header: error names both versions" $?
[ "$(asset_call_count)" -eq 2 ]
tap_check "mismatched version header: only SHA256SUMS and SHA256SUMS.sig were downloaded" $?
[ ! -e "$RUN_HOME/.local/bin/relate" ]
tap_check "mismatched version header: nothing installed" $?

reset_case
STUB_MODE=sums_empty_version
install_run sums-empty-version
[ "$RUN_STATUS" -ne 0 ]
tap_check "empty version header: installer dies non-zero" $?
grep -q "carries an empty version header" "$RUN_ERR"
tap_check "empty version header: error names the empty header (not the legacy path)" $?
[ "$(asset_call_count)" -eq 2 ]
tap_check "empty version header: only SHA256SUMS and SHA256SUMS.sig were downloaded" $?
[ ! -e "$RUN_HOME/.local/bin/relate" ]
tap_check "empty version header: nothing installed" $?

reset_case
STUB_MODE=sums_no_space_header
install_run sums-no-space-header
[ "$RUN_STATUS" -ne 0 ]
tap_check "no-space header spelling: installer dies non-zero" $?
grep -q "has no signed version header" "$RUN_ERR"
tap_check "no-space header spelling: treated as headerless, like the Rust prefix" $?
[ "$(asset_call_count)" -eq 2 ]
tap_check "no-space header spelling: only SHA256SUMS and SHA256SUMS.sig were downloaded" $?

reset_case
STUB_MODE=sums_extra_space_header
install_run sums-extra-space-header
[ "$RUN_STATUS" -eq 0 ]
tap_check "extra-space header value: installer exits 0 (both parsers trim)" $?
cmp -s "$RUN_HOME/.local/bin/relate" "$fixtures/relate-darwin-arm64"
tap_check "extra-space header value: installs the fixture binary" $?

reset_case
STUB_MODE=legacy_tag
install_run legacy-tag
[ "$RUN_STATUS" -eq 0 ]
tap_check "headerless manifest at/below the legacy ceiling: installer exits 0" $?
grep -q "legacy manifest" "$RUN_ERR"
tap_check "headerless manifest at/below the legacy ceiling: warns about the legacy manifest" $?
cmp -s "$RUN_HOME/.local/bin/relate" "$fixtures/relate-darwin-arm64"
tap_check "headerless manifest at/below the legacy ceiling: installs the fixture binary" $?

reset_case
RUN_PATH="$nosshbin"
install_run no-ssh-keygen
[ "$RUN_STATUS" -ne 0 ]
tap_check "ssh-keygen absent: installer dies non-zero" $?
grep -q "openssh-client" "$RUN_ERR"
tap_check "ssh-keygen absent: error points at openssh-client" $?
[ "$(asset_call_count)" -eq 0 ]
tap_check "ssh-keygen absent: no asset download is attempted" $?

# --- 3c. Real ssh-keygen roundtrip ---------------------------------------------
# The keypair, signing and patched-installer machinery lives in the harness
# (build_real_roundtrip_fixtures); these cases only assert over its result.

build_real_roundtrip_fixtures

# Case 1: the patched copy differs from the shipped installer in exactly
# the SIGNING_KEY line. Each case spells its descriptions once (the d_
# variables) and skips under them when the probe failed.
d_patch="real ssh-keygen: patched copy differs only in the SIGNING_KEY line"
if [ -n "$real_roundtrip" ]; then
	[ "$(diff "$INSTALLER" "$patched_installer" | grep -c '^[<>]')" -eq 2 ]
	tap_check "$d_patch" $?
else
	skip_real_case "$d_patch"
fi

# Case 2: a real signature verifies and installs.
d_ok_status="real ssh-keygen: installer exits 0 with a real signature"
d_ok_content="real ssh-keygen: installed content matches the fixture"
d_ok_stub="real ssh-keygen: stub ssh-keygen was not invoked"
if [ -n "$real_roundtrip" ]; then
	real_install_run real-sig-ok "$real_fixtures"
	[ "$RUN_STATUS" -eq 0 ]
	tap_check "$d_ok_status" $?
	cmp -s "$RUN_HOME/.local/bin/relate" "$fixtures/relate-darwin-arm64"
	tap_check "$d_ok_content" $?
	[ ! -s "$RUN_KEYGEN_LOG" ]
	tap_check "$d_ok_stub" $?
else
	skip_real_case "$d_ok_status" "$d_ok_content" "$d_ok_stub"
fi

# Case 3: the second key's own fixture tree fails closed — verification
# against the first key must reject its differently-signed manifest.
d_wrong_status="real ssh-keygen, wrong signer key: installer dies non-zero"
d_wrong_err="real ssh-keygen, wrong signer key: error mentions signature verification"
d_wrong_installed="real ssh-keygen, wrong signer key: nothing installed"
if [ -n "$real_roundtrip" ]; then
	real_install_run real-sig-wrong-key "$wrong_fixtures"
	[ "$RUN_STATUS" -ne 0 ]
	tap_check "$d_wrong_status" $?
	grep -q "signature verification failed" "$RUN_ERR"
	tap_check "$d_wrong_err" $?
	[ ! -e "$RUN_HOME/.local/bin/relate" ]
	tap_check "$d_wrong_installed" $?
else
	skip_real_case "$d_wrong_status" "$d_wrong_err" "$d_wrong_installed"
fi

# --- 4. Checksum failure ------------------------------------------------------

reset_case
STUB_MODE=corrupt
install_run checksum-fail
[ "$RUN_STATUS" -ne 0 ]
tap_check "checksum fail: installer dies non-zero" $?
grep -q "checksum verification failed" "$RUN_ERR"
tap_check "checksum fail: error mentions checksum verification" $?
[ ! -e "$RUN_HOME/.local/bin/relate" ]
tap_check "checksum fail: nothing at the final install path" $?
[ -z "$(ls -A "$RUN_HOME/.local/bin" 2>/dev/null)" ]
tap_check "checksum fail: no temp names left in the bin dir" $?

# --- 5. Missing asset -----------------------------------------------------------

reset_case
STUB_MODE=missing_asset
install_run missing-asset
[ "$RUN_STATUS" -ne 0 ]
tap_check "missing asset: installer dies non-zero" $?
grep -q "has no asset named relate-darwin-arm64" "$RUN_ERR"
tap_check "missing asset: error names the missing asset" $?
[ "$(asset_call_count)" -eq 0 ]
tap_check "missing asset: no /releases/assets/ download is attempted" $?

# --- 6. Resolution failure ------------------------------------------------------

# The one assertion shape every resolution-failure case shares: the
# installer dies non-zero, its stderr carries the case's needle (the
# authored failure message, matched as a fixed string), and no asset
# download is attempted — resolution dies before any /releases/assets/
# request. Each case keeps only what is specific to it beside this call.
assert_resolution_failure() { # $1 = case label, $2 = stderr needle
	[ "$RUN_STATUS" -ne 0 ]
	tap_check "$1: installer dies non-zero" $?
	grep -qF "$2" "$RUN_ERR"
	tap_check "$1: stderr carries the authored failure message" $?
	[ "$(asset_call_count)" -eq 0 ]
	tap_check "$1: no asset download is attempted" $?
}

reset_case
STUB_MODE=lookup_404
install_run metadata-unreachable
assert_resolution_failure \
	"metadata unreachable" \
	"could not read the release list from $installer_releases_url"
! grep -q "GITHUB_TOKEN" "$RUN_ERR"
tap_check "metadata unreachable: no GITHUB_TOKEN hint (the token cannot help fetch the website file)" $?
[ "$(grep -c 'could not read the release list from' "$INSTALLER")" -eq 1 ]
tap_check "metadata unreachable: the message text appears exactly once in install.sh" $?

reset_case
STUB_MODE=releases_malformed
install_run releases-malformed
assert_resolution_failure \
	"malformed releases.json" \
	"could not read the release list from $installer_releases_url: the document is malformed"

reset_case
STUB_MODE=releases_null_latest
install_run releases-null-latest
assert_resolution_failure \
	"null latest" \
	"latest is null: no stable release is published"

reset_case
STUB_MODE=releases_bad_tag
install_run releases-bad-tag
assert_resolution_failure \
	"latest.tag outside the vX.Y.Z shape" \
	"is not a vX.Y.Z release tag"

reset_case
STUB_MODE=tag_404
install_run tag-lookup-fail
assert_resolution_failure \
	"per-tag lookup failure" \
	"could not resolve release $TEST_TAG"
grep -q "GITHUB_TOKEN" "$RUN_ERR"
tap_check "per-tag lookup failure: the failure carries the GITHUB_TOKEN hint" $?

reset_case
STUB_MODE=tag_mismatch
install_run tag-mismatch
assert_resolution_failure \
	"tag_name mismatch" \
	"served release $RELEASE_MISMATCH_TAG for tag $TEST_TAG"

# --- 7. Cleanliness -------------------------------------------------------------

[ -z "$(ls -A "$itmp")" ]
tap_check "cleanliness: installer temp dirs removed (TMPDIR empty after all runs)" $?

# --- summary --------------------------------------------------------------------

tap_done
