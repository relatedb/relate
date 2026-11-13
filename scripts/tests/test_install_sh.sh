#!/bin/sh
# Offline, hermetic test harness for distribution/install.sh.
#
# The installer is run with HOME, TMPDIR and PATH pointed at a sandbox whose
# stub-bin/ shadows `curl` (always) and `uname` (to fix the OS/arch under
# test). The stub curl never touches the network: it logs its argv (one arg
# per line, calls separated by an =CALL= marker) and serves fixtures keyed on
# the request URL, with failure modes selected via STUB_MODE. Every URL
# outside the metadata document and the three direct-route assets hits the
# stub's unexpected-URL arm, so any GitHub API request fails the install
# loudly. The harness writes only inside the sandbox and removes it on exit,
# failure included. The fixture/stub factories and the per-case run harness
# live in scripts/tests/lib/install_sh_harness.sh; this file holds the drift
# guards and the cases.
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

# The one extraction idiom for install.sh's single-quoted constants: the
# value of VAR='...' on its own assignment line, empty when the constant
# is absent (assert_same fails on an empty operand, so absence cannot
# pass a guard vacuously).
installer_literal() { # $1 = variable name
	sed -n "s/^$1='\\(.*\\)'\$/\\1/p" "$INSTALLER"
}

# The pinned SIGNING_KEY in install.sh must match the canonical public key
# committed at distribution/release-signing-key.pub; a release that rotates one
# without the other breaks verification for every user.
pinned_key=$(installer_literal SIGNING_KEY)
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
installer_prefix=$(installer_literal SUMS_VERSION_PREFIX)
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
installer_releases_url=$(installer_literal RELEASES_JSON_URL)
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

# The GitHub API is gone from the installer entirely (ADR-0585): the
# public repository serves every asset over the direct route, and the
# anonymous API's shared-IP 60/hour limit is the failure that broke the
# one-liner (ADR-0358's own finding). No spelling of an API request may
# return — not the base URL, not the per-tag, asset or latest endpoints.
! grep -qE 'api\.github\.com|/releases/(tags|assets|latest)' "$INSTALLER"
tap_check "drift guard: install.sh consults no GitHub API endpoint (ADR-0585)" $?

# The installer's direct-route download base is the base the CLI's
# upgrade reads (ADR-0585): one canonical URL, spelled once per side —
# install.sh's DOWNLOAD_BASE assignment, and download_route.rs's
# canonical_download_base_for composed over release_channel's REPO
# constant — held equal here so the two consumers cannot drift onto
# different download routes or different repositories. The slug comes
# from the CLI side, so a repository move there breaks this guard until
# the installer's base follows.
route_rs="$ROOT/surfaces/relate-cli/src/release_channel/download_route.rs"
channel_rs="$ROOT/surfaces/relate-cli/src/release_channel/mod.rs"
repo_slug=$(sed -n 's/^pub(crate) const REPO: &str = "\(.*\)";$/\1/p' "$channel_rs")
installer_download_base=$(installer_literal DOWNLOAD_BASE)
rust_base_template=$(sed -n 's|.*format!("\([^"]*\)/{repo}/releases/download").*|\1|p' "$route_rs")
assert_same "drift guard: install.sh's DOWNLOAD_BASE matches download_route.rs's canonical base (ADR-0585)" \
	"$installer_download_base" "$rust_base_template/$repo_slug/releases/download"

# The override env var is the other half of that seam: download_route.rs
# reads it through DOWNLOAD_URL_ENV and install.sh references it on the
# download_base line — one spelling, held equal here so the two
# consumers cannot drift onto different seam names.
rust_download_env=$(sed -n 's/^const DOWNLOAD_URL_ENV: &str = "\(.*\)";$/\1/p' "$route_rs")
installer_download_env=$(sed -n 's/^download_base=${\([A-Z_]*\):-\$DOWNLOAD_BASE}$/\1/p' \
	"$INSTALLER")
assert_same "drift guard: install.sh's override env var matches download_route.rs (ADR-0585)" \
	"$rust_download_env" "$installer_download_env"

# The PowerShell installer (ADR-0586) carries its own copy of every
# constant above; its behavior is exercised by
# scripts/tests/test_install_ps1.ps1 on the Windows lane, and its copies are
# held to install.sh's and the CLI's here, on every host, so a rotation or a
# URL move cannot leave the Windows installer behind.
PS_INSTALLER="$ROOT/distribution/install.ps1"
ps_literal() { # $1 = variable name, without the leading $
	sed -n "s/^\\\$$1 = '\\(.*\\)'\$/\\1/p" "$PS_INSTALLER"
}
assert_same "drift guard: install.ps1's pinned signing key matches distribution/release-signing-key.pub" \
	"$(ps_literal RelateSigningKey)" "$pub_key"
assert_same "drift guard: install.ps1's sums header prefix matches manifest.rs (ADR-0457)" \
	"$(ps_literal RelateSumsVersionPrefix)" "$rust_prefix"
assert_same "drift guard: install.ps1's legacy ceiling matches manifest.rs (ADR-0457)" \
	"$(ps_literal RelateLegacyManifestCeiling)" "$rust_ceiling"
assert_same "drift guard: install.ps1's releases.json URL matches releases_json.rs (ADR-0571)" \
	"$(ps_literal RelateReleasesJsonUrl)" "$rust_releases_url"
assert_same "drift guard: install.ps1's download base matches install.sh's (ADR-0585)" \
	"$(ps_literal RelateDownloadBase)" "$installer_download_base"
grep -qF "Get-RelateHttpsSeam -Name '$rust_releases_env'" "$PS_INSTALLER" &&
	grep -qF "Get-RelateHttpsSeam -Name '$rust_download_env'" "$PS_INSTALLER"
tap_check "drift guard: install.ps1 reads the two override seams install.sh and the CLI read, through its https gate" $?
! grep -qE 'api\.github\.com|/releases/(tags|assets|latest)' "$PS_INSTALLER"
tap_check "drift guard: install.ps1 consults no GitHub API endpoint (ADR-0585)" $?
# Windows PowerShell 5.1 decodes a BOM-less script as ANSI (`irm | iex`
# and `-File` alike), so a UTF-8 multi-byte character turns into other
# characters, a smart quote among them, which PowerShell treats as a string
# delimiter. The installer stays pure ASCII.
! LC_ALL=C grep -q "$(printf '[\200-\377]')" "$PS_INSTALLER"
tap_check "drift guard: install.ps1 is pure ASCII (Windows PowerShell 5.1 reads it as ANSI)" $?

# The signer identity and namespace is one value per installer: install.sh's
# SIGNER, which the real-roundtrip machinery signs under, and install.ps1's
# $RelateSigner, held equal here.
INSTALLER_SIGNER=$(installer_literal SIGNER)
assert_same "drift guard: install.sh's SIGNER matches install.ps1's \$RelateSigner (ADR-0115)" \
	"$INSTALLER_SIGNER" "$(ps_literal RelateSigner)"

# The Windows one-liner is spelled in three places — install.sh's pointer
# for Git Bash/MSYS/Cygwin, the CLI's Windows upgrade refusal, and
# install.ps1's own header — and must be one string.
installer_windows_one_liner=$(installer_literal WINDOWS_INSTALL_ONE_LINER)
rust_windows_one_liner=$(sed -n '/^const WINDOWS_INSTALL_ONE_LINER: &str =$/{n;s/^ *"\(.*\)";$/\1/p;}' \
	"$ROOT/surfaces/relate-cli/src/upgrade_cmd/mod.rs" | sed 's/\\"/"/g')
assert_same "drift guard: the Windows install one-liner is one string in install.sh and upgrade_cmd (ADR-0586)" \
	"$installer_windows_one_liner" "$rust_windows_one_liner"
grep -qF "#   $installer_windows_one_liner" "$PS_INSTALLER"
tap_check "drift guard: install.ps1's header documents the same one-liner" $?

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
grep -qF "$installer_download_base/$TEST_TAG/relate-darwin-arm64" "$RUN_LOG"
tap_check "Darwin/arm64: downloads relate-darwin-arm64 over the direct route" $?
! grep -qF "$installer_download_base/$TEST_TAG/relate-linux-amd64" "$RUN_LOG"
tap_check "Darwin/arm64: does not download the linux-amd64 asset" $?
[ "$(api_call_count)" -eq 0 ]
tap_check "Darwin/arm64: no GitHub API request anywhere in the install" $?
cmp -s "$RUN_HOME/.local/bin/relate" "$fixtures/relate-darwin-arm64"
tap_check "Darwin/arm64: installed content is the darwin-arm64 fixture" $?

reset_case
STUB_UNAME_S=Linux
STUB_UNAME_M=x86_64
install_run osarch-linux
[ "$RUN_STATUS" -eq 0 ]
tap_check "Linux/x86_64: installer exits 0" $?
grep -qF "$installer_download_base/$TEST_TAG/relate-linux-amd64" "$RUN_LOG"
tap_check "Linux/x86_64: downloads relate-linux-amd64 over the direct route" $?
cmp -s "$RUN_HOME/.local/bin/relate" "$fixtures/relate-linux-amd64"
tap_check "Linux/x86_64: installed content is the linux-amd64 fixture" $?

reset_case
STUB_UNAME_S=FreeBSD
install_run osarch-freebsd
[ "$RUN_STATUS" -ne 0 ]
tap_check "unsupported OS (FreeBSD): installer dies non-zero" $?
grep -q "unsupported OS" "$RUN_ERR"
tap_check "unsupported OS (FreeBSD): error mentions unsupported OS" $?

# Windows' POSIX-style shells get the PowerShell installer's one-liner
# (ADR-0586) instead of `unsupported OS`, and nothing is downloaded.
for windows_uname in MINGW64_NT-10.0-22631 MSYS_NT-10.0-19045 CYGWIN_NT-10.0-26100; do
	reset_case
	STUB_UNAME_S=$windows_uname
	install_run "osarch-$windows_uname"
	[ "$RUN_STATUS" -ne 0 ]
	tap_check "Windows shell ($windows_uname): installer dies non-zero" $?
	grep -qF "$installer_windows_one_liner" "$RUN_ERR"
	tap_check "Windows shell ($windows_uname): error names the install.ps1 one-liner" $?
	! grep -q "unsupported OS" "$RUN_ERR"
	tap_check "Windows shell ($windows_uname): not reported as an unsupported OS" $?
	[ ! -s "$RUN_LOG" ]
	tap_check "Windows shell ($windows_uname): no request is made" $?
done

reset_case
STUB_UNAME_M=i386
install_run osarch-i386
[ "$RUN_STATUS" -ne 0 ]
tap_check "unsupported arch (i386): installer dies non-zero" $?
grep -q "unsupported architecture" "$RUN_ERR"
tap_check "unsupported arch (i386): error mentions unsupported architecture" $?

# Piped execution says so once, on stderr, and still installs; a run from a
# file stays quiet.
reset_case
RUN_VIA_PIPE=1
install_run piped
[ "$RUN_STATUS" -eq 0 ]
tap_check "piped run: installer exits 0" $?
grep -qF "running from a pipe" "$RUN_ERR"
tap_check "piped run: says it ran before it could be read, and where to review it" $?
reset_case
install_run from-file
! grep -qF "running from a pipe" "$RUN_ERR"
tap_check "run from a file: no piped-execution note" $?

# --- 2. The direct route and GITHUB_TOKEN --------------------------------------
#
# Every asset comes from the direct browser route and no GitHub API request
# is made (ADR-0585) — the anonymous API's shared-IP rate limit is the
# failure this shape removes. GITHUB_TOKEN is not read at all.

reset_case
install_run token-unset
[ "$RUN_STATUS" -eq 0 ]
tap_check "token unset: installer exits 0" $?
! grep -q "Authorization" "$RUN_LOG"
tap_check "token unset: no Authorization header in any curl call" $?
[ "$(direct_call_count)" -eq 3 ]
tap_check "token unset: exactly three direct downloads (manifest + signature + binary)" $?

reset_case
GITHUB_TOKEN=test-token
install_run token-set
[ "$RUN_STATUS" -eq 0 ]
tap_check "token set: installer exits 0 — the token is not read" $?
! grep -q "Authorization" "$RUN_LOG"
tap_check "token set: no Authorization header anywhere (GITHUB_TOKEN never rides a request)" $?
[ "$(api_call_count)" -eq 0 ]
tap_check "token set: no GitHub API request — the token selects nothing" $?
cmp -s "$RUN_HOME/.local/bin/relate" "$fixtures/relate-darwin-arm64"
tap_check "token set: installs the fixture binary" $?

# --- 2a. Direct-route failures --------------------------------------------------
#
# The one assertion shape every installer-failure case shares: the
# installer dies non-zero, its stderr carries the case's needle, and
# nothing lands at the install path. Each case keeps only what is
# specific to it beside the call — the sibling of
# assert_resolution_failure below, serving section 2a's route failures
# and section 3b/3b''s verification and version-binding failures alike.
assert_download_failure() { # $1 = case label, $2 = stderr needle
	[ "$RUN_STATUS" -ne 0 ]
	tap_check "$1: installer dies non-zero" $?
	grep -qF "$2" "$RUN_ERR"
	tap_check "$1: stderr carries the expected message" $?
	[ ! -e "$RUN_HOME/.local/bin/relate" ]
	tap_check "$1: nothing installed" $?
}

reset_case
STUB_MODE=sums_404
install_run sums-404
assert_download_failure \
	"manifest 404" \
	"GET $installer_download_base/$TEST_TAG/SHA256SUMS: http status: 404"
[ "$(direct_call_count)" -eq 1 ]
tap_check "manifest 404: only the manifest request was made" $?
[ "$(api_call_count)" -eq 0 ]
tap_check "manifest 404: no GitHub API request — there is no fallback" $?

reset_case
STUB_MODE=sums_403
install_run sums-403
assert_download_failure \
	"manifest 403 (rate-limit shape)" \
	"GET $installer_download_base/$TEST_TAG/SHA256SUMS: http status: 403"
! grep -qF "private" "$RUN_ERR"
tap_check "manifest 403 (rate-limit shape): no private-repository advice — that is not the failure" $?
[ "$(api_call_count)" -eq 0 ]
tap_check "manifest 403 (rate-limit shape): no GitHub API request" $?

reset_case
STUB_MODE=sig_404
install_run sig-404
assert_download_failure \
	"signature 404" \
	"GET $installer_download_base/$TEST_TAG/SHA256SUMS.sig: http status: 404"
[ "$(direct_call_count)" -eq 2 ]
tap_check "signature 404: manifest and signature were the only requests" $?
! grep -qF "$installer_download_base/$TEST_TAG/relate-darwin-arm64" "$RUN_LOG"
tap_check "signature 404: the binary is not downloaded" $?

reset_case
STUB_MODE=binary_404
install_run binary-404
assert_download_failure \
	"binary 404" \
	"GET $installer_download_base/$TEST_TAG/relate-darwin-arm64: http status: 404"
[ "$(direct_call_count)" -eq 3 ]
tap_check "binary 404: manifest and signature were fetched and verified first" $?
[ -z "$(ls -A "$RUN_HOME/.local/bin" 2>/dev/null)" ]
tap_check "binary 404: no temp names left in the bin dir" $?

# --- 2b. Download-base override (RELATE_RELEASE_DOWNLOAD_URL) --------------------

reset_case
RELATE_RELEASE_DOWNLOAD_URL=https://mirror.test/releases/download
install_run download-url-override
[ "$RUN_STATUS" -eq 0 ]
tap_check "download URL override: installer exits 0" $?
grep -qF "https://mirror.test/releases/download/$TEST_TAG/SHA256SUMS" "$RUN_LOG"
tap_check "download URL override: every asset is fetched from the override base" $?
! grep -qF "$installer_download_base" "$RUN_LOG"
tap_check "download URL override: the canonical base is not fetched" $?

reset_case
RELATE_RELEASE_DOWNLOAD_URL=http://mirror.test/releases/download
install_run download-url-override-http
assert_download_failure \
	"download URL override, http scheme" \
	"RELATE_RELEASE_DOWNLOAD_URL must be an https:// URL"
[ "$(direct_call_count)" -eq 0 ] && [ "$(api_call_count)" -eq 0 ]
tap_check "download URL override, http scheme: no request of any kind is attempted" $?
! grep -qF "$installer_releases_url" "$RUN_LOG"
tap_check "download URL override, http scheme: not even the metadata fetch runs — the gate precedes every request" $?

reset_case
RELATE_RELEASE_DOWNLOAD_URL=http://127.0.0.1:9/releases/download
install_run download-url-override-loopback
assert_download_failure \
	"download URL override, loopback http (the CLI seam's dev carve-out)" \
	"RELATE_RELEASE_DOWNLOAD_URL must be an https:// URL"
[ "$(direct_call_count)" -eq 0 ] && [ "$(api_call_count)" -eq 0 ]
tap_check "download URL override, loopback http: the installer's https-only narrowing is deliberate — refused before any request" $?
reset_case
RELATE_RELEASE_DOWNLOAD_URL=
install_run download-url-override-empty
[ "$RUN_STATUS" -eq 0 ]
tap_check "download URL override, empty value: installer exits 0" $?
grep -qF "$installer_download_base/$TEST_TAG/SHA256SUMS" "$RUN_LOG"
tap_check "download URL override, empty value: the canonical base is fetched (empty means default)" $?

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
[ "$(api_call_count)" -eq 0 ]
tap_check "happy path: zero GitHub API requests — a shared-IP rate limit cannot break the install" $?
grep -q '^-Y$' "$RUN_KEYGEN_LOG" && grep -q '^verify$' "$RUN_KEYGEN_LOG" &&
	grep -qxF "$INSTALLER_SIGNER" "$RUN_KEYGEN_LOG"
tap_check "happy path: ssh-keygen -Y verify ran under the relate-release identity" $?
sig_line=$(grep -n "releases/download/$TEST_TAG/SHA256SUMS.sig\$" "$RUN_LOG" | head -n 1 | cut -d: -f1)
bin_line=$(grep -n "releases/download/$TEST_TAG/relate-darwin-arm64\$" "$RUN_LOG" | head -n 1 | cut -d: -f1)
[ -n "$sig_line" ] && [ -n "$bin_line" ] && [ "$sig_line" -lt "$bin_line" ]
tap_check "happy path: SHA256SUMS.sig is downloaded before the binary asset" $?

# --- 3a. Release resolution (releases.json, ADR-0571) ---------------------------

# The committed publisher fixture — the document publish-releases-json.sh's
# own suite compares its output against — served byte-for-byte as the
# metadata document (publisher_doc mode, with the matching manifest
# variant): the end-to-end pin that the serialization the producer writes
# is the grammar latest_tag_of reads. When the two drift apart, this is
# the case that fails. The direct route must be asked for exactly the
# tag the document names.
reset_case
STUB_MODE=publisher_doc
install_run publisher-fixture
[ "$RUN_STATUS" -eq 0 ]
tap_check "publisher fixture as releases.json: installer exits 0" $?
grep -qF "$installer_download_base/$PUBLISHER_FIXTURE_TAG/SHA256SUMS" "$RUN_LOG"
tap_check "publisher fixture as releases.json: the direct route is asked for the fixture's latest tag" $?
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

# The metadata seam is TLS-only like the download base (ADR-0586): a
# plaintext override dies before any request, and the canonical fetch is
# protocol-pinned across redirects.
reset_case
RELATE_RELEASES_JSON_URL=http://example.test/releases.json
install_run releases-url-override-http
assert_download_failure "metadata URL override, plain http" \
	"RELATE_RELEASES_JSON_URL must be an https:// URL"
[ ! -s "$RUN_LOG" ]
tap_check "metadata URL override, plain http: no request is made" $?

reset_case
install_run releases-fetch-pinned
curl_call_args 'releases\.json$' | grep -qF -- "--proto-redir"
tap_check "metadata fetch: protocol-pinned to https across redirects" $?

# The install directory comes from HOME, so an empty or relative HOME dies
# before any request instead of installing relative to the working directory.
for bad_home in relhome ''; do
	reset_case
	RUN_HOME_ENV=$bad_home
	install_run "home-${bad_home:-empty}"
	assert_download_failure "HOME '$bad_home'" "HOME must be an absolute directory"
	[ ! -s "$RUN_LOG" ]
	tap_check "HOME '$bad_home': no request is made" $?
done

reset_case
RELATE_RELEASES_JSON_URL=
install_run releases-url-override-empty
[ "$RUN_STATUS" -eq 0 ]
tap_check "metadata URL override, empty value: installer exits 0" $?
grep -qF "$installer_releases_url" "$RUN_LOG"
tap_check "metadata URL override, empty value: the canonical URL is fetched (empty means default)" $?

# --- 3b. Signature verification ------------------------------------------------

reset_case
STUB_MODE=sig_invalid
install_run sig-invalid
assert_download_failure \
	"verify failure" \
	"signature verification failed"
[ "$(direct_call_count)" -eq 2 ]
tap_check "verify failure: only SHA256SUMS and SHA256SUMS.sig were downloaded" $?
! grep -qF "$installer_download_base/$TEST_TAG/relate-darwin-arm64" "$RUN_LOG"
tap_check "verify failure: the binary asset is not downloaded before verification" $?

# --- 3b'. Signed version binding (ADR-0457) ------------------------------------

reset_case
STUB_MODE=sums_no_version
install_run sums-no-version
assert_download_failure \
	"no version header above the legacy ceiling" \
	"has no signed version header"
[ "$(direct_call_count)" -eq 2 ]
tap_check "no version header above the legacy ceiling: only SHA256SUMS and SHA256SUMS.sig were downloaded" $?
! grep -qF "$installer_download_base/$TEST_TAG/relate-darwin-arm64" "$RUN_LOG"
tap_check "no version header above the legacy ceiling: the binary is not downloaded" $?

reset_case
STUB_MODE=sums_wrong_version
install_run sums-wrong-version
assert_download_failure \
	"mismatched version header" \
	"SHA256SUMS is for $SUMS_WRONG_TAG, not the requested release $TEST_TAG"
[ "$(direct_call_count)" -eq 2 ]
tap_check "mismatched version header: only SHA256SUMS and SHA256SUMS.sig were downloaded" $?

reset_case
STUB_MODE=sums_empty_version
install_run sums-empty-version
assert_download_failure \
	"empty version header" \
	"carries an empty version header"
[ "$(direct_call_count)" -eq 2 ]
tap_check "empty version header: only SHA256SUMS and SHA256SUMS.sig were downloaded" $?

reset_case
STUB_MODE=sums_no_space_header
install_run sums-no-space-header
assert_download_failure \
	"no-space header spelling" \
	"has no signed version header"
[ "$(direct_call_count)" -eq 2 ]
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
[ "$(direct_call_count)" -eq 0 ]
tap_check "ssh-keygen absent: no download is attempted" $?

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

# --- 4. Checksum failure --------------------------------------------------------

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

# --- 5. Resolution failure ------------------------------------------------------

# The one assertion shape every resolution-failure case shares: the
# installer dies non-zero, its stderr carries the case's needle (the
# authored failure message, matched as a fixed string), and no download
# is attempted — resolution dies before any request beyond the metadata
# fetch. Each case keeps only what is specific to it beside this call.
assert_resolution_failure() { # $1 = case label, $2 = stderr needle
	[ "$RUN_STATUS" -ne 0 ]
	tap_check "$1: installer dies non-zero" $?
	grep -qF "$2" "$RUN_ERR"
	tap_check "$1: stderr carries the authored failure message" $?
	[ "$(direct_call_count)" -eq 0 ] && [ "$(api_call_count)" -eq 0 ]
	tap_check "$1: no download is attempted" $?
}

reset_case
STUB_MODE=metadata_unreachable
install_run metadata-unreachable
assert_resolution_failure \
	"metadata unreachable" \
	"could not read the release list from $installer_releases_url"
! grep -q "GITHUB_TOKEN" "$RUN_ERR"
tap_check "metadata unreachable: no GITHUB_TOKEN hint (no token can help fetch the website file)" $?
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

# --- 6. Cleanliness -------------------------------------------------------------

[ -z "$(ls -A "$itmp")" ]
tap_check "cleanliness: installer temp dirs removed (TMPDIR empty after all runs)" $?

# --- summary --------------------------------------------------------------------

tap_done
