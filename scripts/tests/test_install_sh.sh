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

# The one extraction idiom for the two installers' single-quoted
# constants: the value between the quotes on the assignment line whose
# name matches $1, read from file $2. The name pattern is passed whole,
# so install.sh's `NAME='...'` and install.ps1's `$Name = '...'` share
# it. Empty when no line matches (assert_same fails on an empty operand,
# so absence cannot pass a guard vacuously).
quoted_literal() { # $1 = name pattern, $2 = file
	sed -n "s/^$1 *= *'\\(.*\\)'\$/\\1/p" "$2"
}

# The one extraction idiom for a shell script's bare constants — install.sh's
# and the release gate's alike: the unquoted value on the `NAME=value` line
# of file $2, empty when the constant is absent (an empty operand fails
# assert_same, so absence cannot pass a guard vacuously).
sh_bare_const() { # $1 = constant name, $2 = file
	sed -n "s/^$1=//p" "$2"
}

# The one extraction idiom for a Rust `const NAME: &str = "VALUE";`
# declaration (an optional `pub(crate) ` visibility prefix): the value
# with its escaped quotes unescaped, empty when the constant is absent.
# The declaration may wrap — the value then starts on the line after the
# `=`, as the Windows one-liner's does.
rust_str_const() { # $1 = constant name, $2 = file
	sed -n "/^\(pub(crate) \)\{0,1\}const $1: &str =/{
		/[;]\$/!n
		s/^.*&str = *//
		s/^ *\"\(.*\)\"[;]\$/\\1/p
	}" "$2" | sed 's/\\"/"/g'
}

# The one extraction idiom for install.sh's override seams: the env var
# name in a `VAR=${ENV:-$CONST}` assignment line, empty when the seam is
# absent.
installer_override_env() { # $1 = install.sh variable, $2 = its default constant
	sed -n "s/^$1=\${\([A-Z_]*\):-\$$2}\$/\1/p" "$INSTALLER"
}

# The one extraction idiom for the glibc promise's distro list: the
# parenthesized, comma-separated `Debian …` list that follows the floor.
# The spec states it on its own line, install.sh's advice mid-sentence, so
# the match is unanchored and both homes share this one filter; the comma
# is what tells the list apart from the spec's other parenthetical Debian
# mention. Reads a text stream on stdin and collapses repeats by value;
# empty when absent (an empty operand fails assert_same, so absence cannot
# pass a guard vacuously).
glibc_distro_list() {
	sed -n 's/.*(\(Debian[^)]*,[^)]*\)).*$/\1/p' | sort -u
}

# The pinned SIGNING_KEY in install.sh must match the canonical public key
# committed at distribution/release-signing-key.pub; a release that rotates one
# without the other breaks verification for every user.
pinned_key=$(quoted_literal SIGNING_KEY "$INSTALLER")
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
installer_ceiling=$(sh_bare_const LEGACY_MANIFEST_CEILING "$INSTALLER")
rust_ceiling=$(sed -n 's/^const LEGACY_MANIFEST_CEILING: (u64, u64, u64) = (\([0-9][0-9]*\), \([0-9][0-9]*\), \([0-9][0-9]*\));$/\1.\2.\3/p' \
	"$manifest_rs")
assert_same "drift guard: install.sh's LEGACY_MANIFEST_CEILING matches manifest.rs (ADR-0457)" \
	"$installer_ceiling" "$rust_ceiling"

# Prefix parity: the producer's printf, install.sh's assignment and the
# harness fixture writer's constant must all equal the literal manifest.rs
# carries, and install.sh's case pattern must match the header through that
# one variable rather than a re-spelled literal.
installer_prefix=$(quoted_literal SUMS_VERSION_PREFIX "$INSTALLER")
rust_prefix=$(rust_str_const SUMS_VERSION_PREFIX "$manifest_rs")
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
installer_releases_url=$(quoted_literal RELEASES_JSON_URL "$INSTALLER")
rust_releases_url=$(rust_str_const RELEASES_JSON_URL "$releases_rs")
assert_same "drift guard: install.sh's RELEASES_JSON_URL matches releases_json.rs (ADR-0571)" \
	"$installer_releases_url" "$rust_releases_url"

# The override env var is the other half of that seam (ADR-0571):
# releases_json.rs reads it through RELEASES_JSON_URL_ENV and install.sh
# references it on the metadata_url line — one spelling, held equal here
# so the two consumers cannot drift onto different seam names.
rust_releases_env=$(rust_str_const RELEASES_JSON_URL_ENV "$releases_rs")
installer_releases_env=$(installer_override_env metadata_url RELEASES_JSON_URL)
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
repo_slug=$(rust_str_const REPO "$channel_rs")
installer_download_base=$(quoted_literal DOWNLOAD_BASE "$INSTALLER")
rust_base_template=$(sed -n 's|.*format!("\([^"]*\)/{repo}/releases/download").*|\1|p' "$route_rs")
assert_same "drift guard: install.sh's DOWNLOAD_BASE matches download_route.rs's canonical base (ADR-0585)" \
	"$installer_download_base" "$rust_base_template/$repo_slug/releases/download"

# The override env var is the other half of that seam: download_route.rs
# reads it through DOWNLOAD_URL_ENV and install.sh references it on the
# download_base line — one spelling, held equal here so the two
# consumers cannot drift onto different seam names.
rust_download_env=$(rust_str_const DOWNLOAD_URL_ENV "$route_rs")
installer_download_env=$(installer_override_env download_base DOWNLOAD_BASE)
assert_same "drift guard: install.sh's override env var matches download_route.rs (ADR-0585)" \
	"$rust_download_env" "$installer_download_env"

# The PowerShell installer (ADR-0586) carries its own copy of every
# constant above; its behavior is exercised by
# scripts/tests/test_install_ps1.ps1 on the Windows lane, and its copies are
# held to install.sh's and the CLI's here, on every host, so a rotation or a
# URL move cannot leave the Windows installer behind.
PS_INSTALLER="$ROOT/distribution/install.ps1"
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
# SIGNER, which the real-roundtrip machinery signs under; its PowerShell
# copies are held to it by the Windows constants table below.
INSTALLER_SIGNER=$(quoted_literal SIGNER "$INSTALLER")

# The Chocolatey template (ADR-0659) carries the release URL and the Windows
# asset the installers use, and the shared package-managed marker. Its own
# behavior runs on Windows alone (the publish-registries job), but its copies
# are held to install.sh's and the shared vocabulary here, on every host, so
# a URL move or an asset rename cannot strand the channel.
CHOCO_TEMPLATE="$ROOT/distribution/chocolatey/tools/chocolateyinstall.ps1"
installer_asset_prefix=$(sh_bare_const ASSET_PREFIX "$INSTALLER")

# Every PowerShell constant the two Windows homes carry is one row: the
# constant's name, the homes that carry it (p = install.ps1,
# c = the Chocolatey template), the value it must equal — a variable or a
# command substitution, expanded once when the table is read — and the
# label, the only per-row text. The loop is this family's one guard shape:
# what was one shape typed out once per (constant, file) pair is now one
# model of "a constant with homes". The name pattern is built as a bracket
# expression (`[$]Name`), the escape-free way to match a literal dollar in
# a BRE. A row missing its property, homes or label, and a home letter that
# names no file, red the suite instead of being skipped (`expected` needs no
# check of its own: assert_same rejects an empty operand). What the table
# asserts is counted, not what it reads: the counter ticks once per home
# actually checked, so a row that reaches no home cannot inflate it, and a
# lost table cannot run vacuously.
windows_const_checks=0
while IFS='|' read -r prop homes expected label; do
	if [ -z "$prop" ] || [ -z "$homes" ] || [ -z "$label" ]; then
		tap_check "Windows constants table: a row needs a property, a home and a label (got '$prop|$homes|$label')" 1
		continue
	fi
	for home in $homes; do
		case $home in
		p) windows_home=$PS_INSTALLER; home_name=install.ps1 ;;
		c) windows_home=$CHOCO_TEMPLATE; home_name='the Chocolatey template' ;;
		*)
			tap_check "Windows constants table: unknown home '$home' for \$$prop reds the suite" 1
			continue
			;;
		esac
		windows_const_checks=$((windows_const_checks + 1))
		assert_same "drift guard: $label ($home_name)" \
			"$(quoted_literal "[$]$prop" "$windows_home")" "$expected"
	done
done <<TABLE
RelateSigningKey|p c|$pub_key|the pinned signing key matches distribution/release-signing-key.pub
RelateSumsVersionPrefix|p c|$rust_prefix|the sums header prefix matches manifest.rs (ADR-0457)
RelateLegacyManifestCeiling|p c|$rust_ceiling|the legacy ceiling matches manifest.rs (ADR-0457)
RelateReleasesJsonUrl|p|$rust_releases_url|the releases.json URL matches releases_json.rs (ADR-0571)
RelateDownloadBase|p|$installer_download_base|the download base matches install.sh's (ADR-0585)
RelateSigner|p c|$INSTALLER_SIGNER|the signer matches install.sh's SIGNER (ADR-0115)
RelateReleaseUrl|c|$installer_download_base|the release base matches install.sh's DOWNLOAD_BASE
RelateAsset|c|$installer_asset_prefix-windows-amd64.exe|the Windows asset is the shared relate-windows-amd64.exe
RelatePackageMarker|p c|$(sh_bare_const MARKER_NAME "$INSTALLER")|the package-managed marker matches install.sh's MARKER_NAME
RelateTagPattern|c|$(quoted_literal '[$]RelateTagPattern' "$PS_INSTALLER")|the vX.Y.Z tag grammar is install.ps1's one literal
TABLE
[ "$windows_const_checks" -gt 0 ]
tap_check "drift guard: the Windows constants table checked at least one home" $?

# The one extraction idiom for a PowerShell function: the lines from
# `function $1 {` through its column-0 closing brace, empty when the function
# is absent (assert_same fails on an empty operand, so a missing copy cannot
# pass vacuously).
ps_function_body() { # $1 = function name, $2 = file
	sed -n "/^function $1 {$/,/^}$/p" "$2"
}

# The one normalization of the two copies' naming differences, so a shared
# body compares equal: the refusal helper (install.ps1's Stop-RelateInstall,
# the template's Stop-RelateChocolateyInstall) and the script label each copy
# prefixes a warning with both fold to one token.
normalize_shared() {
	sed -e 's/Stop-RelateChocolateyInstall /Stop-Relate /g' \
		-e 's/Stop-RelateInstall /Stop-Relate /g' \
		-e 's/chocolateyinstall\.ps1: /SCRIPT: /g' \
		-e 's/install\.ps1: /SCRIPT: /g'
}

# The function names a PowerShell file declares: one sed idiom over both
# copies, so the shared set below is derived, never a hand-kept list.
ps_function_names() { # $1 = file
	sed -n 's/^function \([A-Za-z0-9-]*\) {.*/\1/p' "$1"
}

# A Chocolatey package is installed from the community repository and cannot
# source install.ps1, so its template carries its own copy of every helper it
# shares with the one-liner. The intersection of the two files' declared
# function names is the shared set: each is one body here, on every host, so
# a fix to either copy cannot strand the other, and a helper added to both
# copies joins the guard the moment it lands in both.
shared_count=0
for shared_function in $(ps_function_names "$PS_INSTALLER"); do
	ps_function_names "$CHOCO_TEMPLATE" | grep -qx "$shared_function" || continue
	shared_count=$((shared_count + 1))
	assert_same "drift guard: $shared_function is one body in install.ps1 and the Chocolatey template" \
		"$(ps_function_body "$shared_function" "$PS_INSTALLER" | normalize_shared)" \
		"$(ps_function_body "$shared_function" "$CHOCO_TEMPLATE" | normalize_shared)"
done
# Guard the guard: an empty intersection would make the loop vacuous.
[ "$shared_count" -gt 0 ]
tap_check "drift guard: install.ps1 and the Chocolatey template share at least one helper" $?

# The Windows one-liner is spelled in three places — install.sh's pointer
# for Git Bash/MSYS/Cygwin, the CLI's Windows upgrade refusal, and
# install.ps1's own header — and must be one string.
installer_windows_one_liner=$(quoted_literal WINDOWS_INSTALL_ONE_LINER "$INSTALLER")
rust_windows_one_liner=$(rust_str_const WINDOWS_INSTALL_ONE_LINER \
	"$ROOT/surfaces/relate-cli/src/upgrade_cmd/mod.rs")
assert_same "drift guard: the Windows install one-liner is one string in install.sh and upgrade_cmd (ADR-0586)" \
	"$installer_windows_one_liner" "$rust_windows_one_liner"
grep -qF "#   $installer_windows_one_liner" "$PS_INSTALLER"
tap_check "drift guard: install.ps1's header documents the same one-liner" $?

# The Linux glibc floor is one promise in three homes (REL8-1841):
# install.sh's preflight constant, the release gate's documented floor
# (scripts/check-glibc-floor.sh), and the CLI spec's Platform requirements.
# A bump that moves one home and not the others makes the installer refuse
# systems the release supports, or ship a promise the gate rejects.
installer_glibc_floor=$(sh_bare_const GLIBC_FLOOR "$INSTALLER")
gate_glibc_floor=$(sh_bare_const floor_version "$ROOT/scripts/check-glibc-floor.sh")
# The installer's floor stated as a value the preflight tests: one above it,
# for the cases and the table's above-floor row to share, so neither can
# drift. Derived by bumping the floor's major component alone — it stays
# above the floor whatever a bump makes it (a hand-picked ceiling like 9.9
# would silently flip both rows to refusals the day the floor passed it) and
# assumes nothing about the floor's component count.
glibc_above_floor=$(($(printf '%s' "$installer_glibc_floor" | cut -d. -f1) + 1)).0
# Every spelling of the floor in the spec, not the first of one grammar: the
# promise sentence ("glibc X.Y or newer") and the `GLIBC_X.Y` symbol-version
# token the cap and the gate sentences use are the same fact. Collapsing by
# value means identical restatements pass and a divergent one fails the
# guard, where a first-match pick would silently ignore it.
spec_glibc_floors=$({
	sed -n 's/^.*glibc \([0-9][0-9]*\.[0-9][0-9]*\) or newer.*$/\1/p' "$ROOT/docs/api/cli/spec.md"
	grep -o 'GLIBC_[0-9][0-9]*\.[0-9][0-9]*' "$ROOT/docs/api/cli/spec.md" | sed 's/^GLIBC_//'
} | sort -u)
# All three homes in one call: assert_same is variadic by contract, and it
# rejects an empty operand, so a home that lost its floor fails here rather
# than needing a precondition of its own.
assert_same "drift guard: the glibc floor is one value in install.sh, check-glibc-floor.sh and the CLI spec (REL8-1841)" \
	"$installer_glibc_floor" "$gate_glibc_floor" "$spec_glibc_floors"
# The floor's promise is a number AND the distro list beside it, and the
# installer's advice repeats both: the number is guarded above, the list
# here. Both homes go through the one distro-list extraction and are
# compared by equality, so a distro added to one home and not the other
# fails the guard in either direction — containment alone would pass an
# extra entry in the installer. The subject is the GLIBC_ADVICE constant
# both refusals emit, never the whole file, so a comment mention cannot
# pass it; and a reworded list, or a home that lost its list, fails the
# equality rather than passing vacuously.
spec_distro_floor=$(glibc_distro_list < "$ROOT/docs/api/cli/spec.md")
installer_glibc_advice=$(quoted_literal GLIBC_ADVICE "$INSTALLER")
installer_distro_floor=$(printf '%s\n' "$installer_glibc_advice" | glibc_distro_list)
assert_same "drift guard: the installer's glibc advice names the same distro floor as the spec" \
	"$spec_distro_floor" "$installer_distro_floor"

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
#
# Every refusal asserts the shape of the last stage it reached: the two uname
# switches are the installer's first step, so each of the three refusals below
# dies before any request and carries the preflight shape.

reset_case
install_run osarch-darwin
tap_check "Darwin/arm64: installer exits 0" "$RUN_STATUS"
grep -qF "$installer_download_base/$TEST_TAG/relate-darwin-arm64" "$RUN_LOG"
tap_check "Darwin/arm64: downloads relate-darwin-arm64 over the direct route" $?
! grep -qF "$installer_download_base/$TEST_TAG/relate-linux-amd64" "$RUN_LOG"
tap_check "Darwin/arm64: does not download the linux-amd64 asset" $?
assert_installed "Darwin/arm64: installed content is the darwin-arm64 fixture" \
	"$fixtures/relate-darwin-arm64"

reset_case_linux
# A plain supported glibc, deliberately not the boundary: the exact-floor
# arm is pinned once, in the preflight's verdict table below. The answer is
# $glibc_above_floor, derived in section 0 beside the floor it follows and
# shared with that table's above-floor row.
STUB_GLIBC_VERSION=$glibc_above_floor
install_run osarch-linux
tap_check "Linux/x86_64: installer exits 0" "$RUN_STATUS"
grep -qF "$installer_download_base/$TEST_TAG/relate-linux-amd64" "$RUN_LOG"
tap_check "Linux/x86_64: downloads relate-linux-amd64 over the direct route" $?
assert_installed "Linux/x86_64: installed content is the linux-amd64 fixture" \
	"$fixtures/relate-linux-amd64"

reset_case
STUB_UNAME_S=FreeBSD
install_run osarch-freebsd
assert_preflight_failure "unsupported OS (FreeBSD)" "unsupported OS"

# Windows' POSIX-style shells get the PowerShell installer's one-liner
# (ADR-0586) instead of `unsupported OS`, and nothing is downloaded.
for windows_uname in MINGW64_NT-10.0-22631 MSYS_NT-10.0-19045 CYGWIN_NT-10.0-26100; do
	reset_case
	STUB_UNAME_S=$windows_uname
	install_run "osarch-$windows_uname"
	assert_preflight_failure "Windows shell ($windows_uname)" \
		"$installer_windows_one_liner"
	! grep -q "unsupported OS" "$RUN_ERR"
	tap_check "Windows shell ($windows_uname): not reported as an unsupported OS" $?
done

reset_case
STUB_UNAME_M=i386
install_run osarch-i386
assert_preflight_failure "unsupported arch (i386)" "unsupported architecture"

# Piped execution says so once, on stderr, and still installs; a run from a
# file stays quiet.
reset_case
RUN_VIA_PIPE=1
install_run piped
tap_check "piped run: installer exits 0" "$RUN_STATUS"
grep -qF "running from a pipe" "$RUN_ERR"
tap_check "piped run: says it ran before it could be read, and where to review it" $?
reset_case
install_run from-file
! grep -qF "running from a pipe" "$RUN_ERR"
tap_check "run from a file: no piped-execution note" $?

# --- 1'. Linux glibc preflight (REL8-1841) --------------------------------------
#
# The Linux binary is a glibc build with the documented floor, so a
# below-floor or non-glibc system is refused with the alternatives, before
# any request — instead of installing a binary the dynamic loader refuses
# at exec. The floor itself is one value across the installer, the release
# gate and the spec (guarded in section 0), so every needle names it
# through $installer_glibc_floor, never by its literal.
#
# One row per verdict arm of version_cmp and per probe-failure shape:
# getconf's answer, ldd's first line (either empty = that probe fails), the
# expected verdict (install or refuse — its own column, never inferred from
# the needle), and on a refusal row the stderr needle. Both refusals carry
# the alternatives, so the refusal arm greps the owned $installer_glibc_advice
# rather than re-splitting its text. The table is
# unquoted so $installer_glibc_floor expands in it. Two rows carry their
# weight beyond the arm they cover: `malformed-token` (a leading dot) and
# `bare-integer` are the shapes the first implementation's comparison read
# as "at least the floor" — install instead of refuse — so they pin the
# fix, not just the current behavior. A blank or malformed row is not
# skipped: it reaches the `*)` arm below and fails with the row named, so
# an authoring slip reds the suite instead of silently shrinking it. The
# row count is asserted after the table for the same reason from the other
# side: a lost table (a truncated heredoc, a botched terminator edit) runs
# zero iterations, and the TAP plan being dynamic would hide it.
glibc_rows=0
while IFS='|' read -r glibc_case getconf_answer ldd_line verdict needle; do
	# Row shape first, before the installer runs: the sibling Windows
	# constants table validates every column up front for the same reason —
	# a row missing its case name or verdict is an authoring slip, and it
	# should red the suite without spending an installer pass on it. A
	# verdict that is neither install nor refuse still falls to the `*)`
	# arm below.
	if [ -z "$glibc_case" ] || [ -z "$verdict" ]; then
		tap_check "glibc preflight table: a row needs a case name and a verdict (got '$glibc_case|$verdict')" 1
		continue
	fi
	glibc_rows=$((glibc_rows + 1))
	reset_case_linux
	STUB_GLIBC_VERSION=$getconf_answer
	STUB_LDD_LINE="$ldd_line"
	install_run "glibc-$glibc_case"
	case "$verdict" in
	install)
		tap_check "Linux glibc $glibc_case: installer exits 0" "$RUN_STATUS"
		assert_installed "Linux glibc $glibc_case: installs the linux-amd64 fixture" \
			"$fixtures/relate-linux-amd64"
		;;
	refuse)
		assert_preflight_failure "Linux glibc $glibc_case" "$needle"
		grep -qF "$installer_glibc_advice" "$RUN_ERR"
		tap_check "Linux glibc $glibc_case: the refusal names the alternatives" $?
		;;
	*)
		tap_check "Linux glibc $glibc_case: the table's verdict column is install or refuse (got '$verdict')" 1
		;;
	esac
done <<TABLE
below-floor|2.31||refuse|older than the $installer_glibc_floor relate-linux-amd64 requires
exact-floor|$installer_glibc_floor||install|
above-floor|$glibc_above_floor||install|
musl||musl libc (x86_64)|refuse|not glibc-based
ldd-below-floor||ldd (Ubuntu GLIBC 2.31-0ubuntu3.1) 2.31|refuse|older than the $installer_glibc_floor
ldd-at-floor||ldd (Ubuntu GLIBC $installer_glibc_floor-0ubuntu3.1) $installer_glibc_floor|install|
undetermined|||refuse|not glibc-based
malformed-token||ldd (Ubuntu GLIBC .35-0ubuntu3.1) .35|refuse|not glibc-based
bare-integer||ldd (Ubuntu GLIBC 2-0ubuntu3.1) 2|refuse|older than the $installer_glibc_floor
TABLE
[ "$glibc_rows" -gt 0 ]
tap_check "drift guard: the glibc preflight table has rows" $?

# --- 2. The direct route and GITHUB_TOKEN --------------------------------------
#
# Every asset comes from the direct browser route and no GitHub API request
# is made (ADR-0585) — the anonymous API's shared-IP rate limit is the
# failure this shape removes. GITHUB_TOKEN is not read at all.

reset_case
install_run token-unset
tap_check "token unset: installer exits 0" "$RUN_STATUS"
! grep -q "Authorization" "$RUN_LOG"
tap_check "token unset: no Authorization header in any curl call" $?
[ "$(direct_call_count)" -eq 3 ]
tap_check "token unset: exactly three direct downloads (manifest + signature + binary)" $?

reset_case
GITHUB_TOKEN=test-token
install_run token-set
tap_check "token set: installer exits 0 — the token is not read" "$RUN_STATUS"
! grep -q "Authorization" "$RUN_LOG"
tap_check "token set: no Authorization header anywhere (GITHUB_TOKEN never rides a request)" $?
assert_installed "token set: installs the fixture binary" "$fixtures/relate-darwin-arm64"

# --- 2a. Direct-route failures --------------------------------------------------
#
# Every case asserts through the harness lib's failure-assertion helpers
# and keeps only what is specific to it beside the call.

reset_case
STUB_MODE=sums_404
install_run sums-404
assert_manifest_failure \
	"manifest 404" \
	"GET $installer_download_base/$TEST_TAG/SHA256SUMS: http status: 404"

reset_case
STUB_MODE=sums_403
install_run sums-403
assert_manifest_failure \
	"manifest 403 (rate-limit shape)" \
	"GET $installer_download_base/$TEST_TAG/SHA256SUMS: http status: 403"
! grep -qF "private" "$RUN_ERR"
tap_check "manifest 403 (rate-limit shape): no private-repository advice — that is not the failure" $?

reset_case
STUB_MODE=sig_404
install_run sig-404
assert_verify_failure \
	"signature 404" \
	"GET $installer_download_base/$TEST_TAG/SHA256SUMS.sig: http status: 404"

reset_case
STUB_MODE=binary_404
install_run binary-404
assert_download_failure \
	"binary 404" \
	"GET $installer_download_base/$TEST_TAG/relate-darwin-arm64: http status: 404"
[ "$(direct_call_count)" -eq 3 ]
tap_check "binary 404: manifest and signature were fetched and verified first" $?

# --- 2b. Download-base override (RELATE_RELEASE_DOWNLOAD_URL) --------------------

reset_case
RELATE_RELEASE_DOWNLOAD_URL=https://mirror.test/releases/download
install_run download-url-override
tap_check "download URL override: installer exits 0" "$RUN_STATUS"
grep -qF "https://mirror.test/releases/download/$TEST_TAG/SHA256SUMS" "$RUN_LOG"
tap_check "download URL override: every asset is fetched from the override base" $?
! grep -qF "$installer_download_base" "$RUN_LOG"
tap_check "download URL override: the canonical base is not fetched" $?

reset_case
RELATE_RELEASE_DOWNLOAD_URL=http://mirror.test/releases/download
install_run download-url-override-http
assert_preflight_failure \
	"download URL override, http scheme" \
	"RELATE_RELEASE_DOWNLOAD_URL must be an https:// URL"

reset_case
RELATE_RELEASE_DOWNLOAD_URL=http://127.0.0.1:9/releases/download
install_run download-url-override-loopback
assert_preflight_failure \
	"download URL override, loopback http (the CLI seam's dev carve-out)" \
	"RELATE_RELEASE_DOWNLOAD_URL must be an https:// URL"
reset_case
RELATE_RELEASE_DOWNLOAD_URL=
install_run download-url-override-empty
tap_check "download URL override, empty value: installer exits 0" "$RUN_STATUS"
grep -qF "$installer_download_base/$TEST_TAG/SHA256SUMS" "$RUN_LOG"
tap_check "download URL override, empty value: the canonical base is fetched (empty means default)" $?

# --- 3. Happy path ------------------------------------------------------------

reset_case
install_run happy
tap_check "happy path: installer exits 0" "$RUN_STATUS"
[ -f "$RUN_HOME/.local/bin/relate" ]
tap_check "happy path: \$HOME/.local/bin/relate exists" $?
[ -x "$RUN_HOME/.local/bin/relate" ]
tap_check "happy path: installed file is executable" $?
assert_installed "happy path: installed content matches the fixture" "$fixtures/relate-darwin-arm64"
[ "$(ls -A "$RUN_HOME/.local/bin")" = "relate" ]
tap_check "happy path: bin dir holds exactly the installed binary (no temp leftovers)" $?
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
tap_check "publisher fixture as releases.json: installer exits 0" "$RUN_STATUS"
grep -qF "$installer_download_base/$PUBLISHER_FIXTURE_TAG/SHA256SUMS" "$RUN_LOG"
tap_check "publisher fixture as releases.json: the direct route is asked for the fixture's latest tag" $?
assert_installed "publisher fixture as releases.json: installs that release's product binary" \
	"$fixtures/relate-darwin-arm64"

reset_case
RELATE_RELEASES_JSON_URL=https://example.test/releases.json
install_run releases-url-override
tap_check "metadata URL override: installer exits 0" "$RUN_STATUS"
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
assert_preflight_failure "metadata URL override, plain http" \
	"RELATE_RELEASES_JSON_URL must be an https:// URL"

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
	assert_preflight_failure "HOME '$bad_home'" "HOME must be an absolute directory"
done

reset_case
RELATE_RELEASES_JSON_URL=
install_run releases-url-override-empty
tap_check "metadata URL override, empty value: installer exits 0" "$RUN_STATUS"
grep -qF "$installer_releases_url" "$RUN_LOG"
tap_check "metadata URL override, empty value: the canonical URL is fetched (empty means default)" $?

# --- 3b. Signature verification ------------------------------------------------

reset_case
STUB_MODE=sig_invalid
install_run sig-invalid
assert_verify_failure \
	"verify failure" \
	"signature verification failed"

# --- 3b'. Signed version binding (ADR-0457) ------------------------------------

reset_case
STUB_MODE=sums_no_version
install_run sums-no-version
assert_verify_failure \
	"no version header above the legacy ceiling" \
	"has no signed version header"

reset_case
STUB_MODE=sums_wrong_version
install_run sums-wrong-version
assert_verify_failure \
	"mismatched version header" \
	"SHA256SUMS is for $SUMS_WRONG_TAG, not the requested release $TEST_TAG"

reset_case
STUB_MODE=sums_empty_version
install_run sums-empty-version
assert_verify_failure \
	"empty version header" \
	"carries an empty version header"

reset_case
STUB_MODE=sums_no_space_header
install_run sums-no-space-header
assert_verify_failure \
	"no-space header spelling" \
	"has no signed version header"

reset_case
STUB_MODE=sums_extra_space_header
install_run sums-extra-space-header
tap_check "extra-space header value: installer exits 0 (both parsers trim)" "$RUN_STATUS"
assert_installed "extra-space header value: installs the fixture binary" "$fixtures/relate-darwin-arm64"

reset_case
STUB_MODE=legacy_tag
install_run legacy-tag
tap_check "headerless manifest at/below the legacy ceiling: installer exits 0" "$RUN_STATUS"
grep -q "legacy manifest" "$RUN_ERR"
tap_check "headerless manifest at/below the legacy ceiling: warns about the legacy manifest" $?
assert_installed "headerless manifest at/below the legacy ceiling: installs the fixture binary" \
	"$fixtures/relate-darwin-arm64"

reset_case
RUN_PATH="$nosshbin"
install_run no-ssh-keygen
assert_preflight_failure "ssh-keygen absent" "openssh-client"

# --- 3c. Real ssh-keygen roundtrip ---------------------------------------------
# The keypair, signing and patched-installer machinery lives in the harness
# (build_real_roundtrip_fixtures); these cases only assert over its result.
# The seven checks' descriptions are spelled once, in the list below, and
# the one probe branch drives both arms off them: the run arm asserts under
# $1..$7 in order, the skip arm skips the same list.

set -- \
	"real ssh-keygen: patched copy differs only in the SIGNING_KEY line" \
	"real ssh-keygen: installer exits 0 with a real signature" \
	"real ssh-keygen: installed content matches the fixture" \
	"real ssh-keygen: stub ssh-keygen was not invoked" \
	"real ssh-keygen, wrong signer key: installer dies non-zero" \
	"real ssh-keygen, wrong signer key: error mentions signature verification" \
	"real ssh-keygen, wrong signer key: nothing installed"

if build_real_roundtrip_fixtures; then
	# Case 1: the patched copy differs from the shipped installer in
	# exactly the SIGNING_KEY line.
	[ "$(diff "$INSTALLER" "$patched_installer" | grep -c '^[<>]')" -eq 2 ]
	tap_check "$1" $?
	# Case 2: a real signature verifies and installs.
	real_install_run real-sig-ok "$real_fixtures"
	tap_check "$2" "$RUN_STATUS"
	assert_installed "$3" "$fixtures/relate-darwin-arm64"
	[ ! -s "$RUN_KEYGEN_LOG" ]
	tap_check "$4" $?
	# Case 3: the second key's own fixture tree fails closed —
	# verification against the first key must reject its
	# differently-signed manifest.
	real_install_run real-sig-wrong-key "$wrong_fixtures"
	[ "$RUN_STATUS" -ne 0 ]
	tap_check "$5" $?
	grep -q "signature verification failed" "$RUN_ERR"
	tap_check "$6" $?
	[ ! -e "$RUN_HOME/.local/bin/relate" ]
	tap_check "$7" $?
else
	skip_real_case "$@"
fi

# --- 4. Checksum failure --------------------------------------------------------

reset_case
STUB_MODE=corrupt
install_run checksum-fail
assert_download_failure "checksum fail" "checksum verification failed"

# --- 5. Resolution failure ------------------------------------------------------

# Every case asserts through assert_resolution_failure, beside its stage
# siblings in the harness lib, and keeps only what is specific to it
# beside the call.

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

# --- 5a. A package-managed install is never overwritten -------------------------

# The marker `relate upgrade` reads: a package writes it beside the real
# binary and exposes that binary on PATH, so the install path is a symlink
# into the package's own directory. The refusal body is the CLI's own
# sentence, so the two consumers cannot advise different commands.

reset_case
pm_keg="$sandbox/pm-keg"
pm_home="$sandbox/home-package-managed"
mkdir -p "$pm_keg" "$pm_home/.local/bin"
printf 'package-owned binary\n' >"$pm_keg/relate"
printf '{"manager": "homebrew", "upgrade": "brew upgrade relate"}\n' \
	>"$pm_keg/.relate-package-managed"
ln -s "$pm_keg/relate" "$pm_home/.local/bin/relate"
install_run package-managed
assert_install_failure "package-managed (symlinked install path)" \
	"relate was installed by homebrew; upgrade it with: brew upgrade relate"
[ -L "$pm_home/.local/bin/relate" ]
tap_check "package-managed: the package's symlink is untouched" $?
! grep -q 'releases.json' "$RUN_LOG"
tap_check "package-managed: refused before any request" $?

reset_case
pm_keg="$sandbox/pm-keg-malformed"
pm_home="$sandbox/home-package-managed-malformed"
mkdir -p "$pm_keg" "$pm_home/.local/bin"
printf 'package-owned binary\n' >"$pm_keg/relate"
printf '{"manager": "homebrew"}\n' >"$pm_keg/.relate-package-managed"
ln -s "$pm_keg/relate" "$pm_home/.local/bin/relate"
install_run package-managed-malformed
assert_install_failure "package-managed (malformed marker)" \
	"relate is package-managed; marker"
grep -qF "unreadable/invalid" "$RUN_ERR"
tap_check "package-managed (malformed marker): the refusal names the marker's fault" $?

reset_case
pm_home="$sandbox/home-unmanaged-existing"
mkdir -p "$pm_home/.local/bin"
printf 'old binary\n' >"$pm_home/.local/bin/relate"
install_run unmanaged-existing
tap_check "unmanaged existing install: installer exits 0" "$RUN_STATUS"
assert_installed "unmanaged existing install: installed content matches the fixture" \
	"$fixtures/relate-darwin-arm64"

# --- 6. Cleanliness -------------------------------------------------------------

# The installer consults no GitHub API endpoint (ADR-0585): the section-0
# static guard pins its spelling and the stub's unexpected-URL arm fails any
# API request loudly, so one assertion over every case's curl log covers the
# whole run instead of a per-case repeat.
! grep -q 'api\.github\.com' "$sandbox"/curl-*.log
tap_check "no case made a GitHub API request (ADR-0585)" $?

[ -z "$(ls -A "$itmp")" ]
tap_check "cleanliness: installer temp dirs removed (TMPDIR empty after all runs)" $?

# --- summary --------------------------------------------------------------------

tap_done
