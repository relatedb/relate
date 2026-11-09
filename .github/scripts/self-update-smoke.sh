#!/usr/bin/env bash
# Self-update smoke test: prove `relate upgrade` actually upgrades.
#
# Resolves the previous stable release, downloads its platform binary
# and the release's SHA256SUMS manifest into a private temp dir, and
# checksum-verifies the binary against the manifest entry naming the
# asset (sha256_of, the shared helper; the manifest is install.sh's
# "<64-hex>  <name>" format) before anything chmod +x's or executes
# it. Then it runs `upgrade` on that isolated copy (HOME isolated to
# the temp dir), and asserts the copy then reports the expected
# version. The installed relate on PATH must stay untouched, and
# no skill-install lines may print: the isolated HOME has no agent
# dirs, so the post-upgrade skill refresh must skip silently
# (ADR-0261).
#
# The inner upgrade reads the live website releases.json (ADR-0250) and
# downloads the new release's assets through the probe-then-fallback
# route (ADR-0358): while the repository is internal the direct route
# 404s and the authenticated GitHub API asset route serves the assets,
# so GITHUB_TOKEN is required. The website's releases.json must already
# list the expected version as latest: the release pipeline used to
# guarantee that by chaining this smoke after the Pages deploy
# (ADR-0330); the website now lives in the separate relate-web repository
# (ADR-0546) and deploys itself on the release pipeline's
# repository_dispatch poke, so the smoke checks that currency
# itself before downloading anything — the live-site currency gate
# below skips with a stated reason while the site lags the release
# list, instead of downloading into a guaranteed wrong-version assert.
#
# Prerequisites: gh (authenticated), curl, and jq on PATH, GITHUB_TOKEN
# set.
#
# Usage: GITHUB_TOKEN=$(gh auth token) bash .github/scripts/self-update-smoke.sh
#   (from anywhere; resolves the repo root from its own path)
#
# Opt-in environment (the release workflow's smoke gate wires these;
# unset overrides mean resolve everything live):
#   GITHUB_TOKEN      required while the repository is internal: the
#                     authenticated GitHub API asset route the inner
#                     upgrade falls back to (ADR-0358 §3), and what gh
#                     uses for the release list and the asset download
#   EXPECTED_VERSION  the X.Y.Z the upgraded copy must report (default:
#                     the checkout's workspace Cargo.toml version — the
#                     release being verified)
#   PREV_TAG          the release tag to upgrade from (default: the
#                     newest stable CORE tag — vX.Y.Z, the ADR-0250
#                     stable shape; extension-channel releases are not
#                     candidates — that is not the expected version,
#                     resolved from the live release list)
#   ASSET             the release asset name to download (default:
#                     relate-<os>-<arch> for this host)
#
# Exit status: 0 = smoke passed, or skipped with a stated reason (no
# previous stable release — the expected release is the first stable
# release ever; a pre-0.12.6 previous release, which cannot self-update
# while the repository is internal, ADR-0358; or a live site whose
# releases.json does not list the expected version as latest — Pages
# deploys are paused, and an upgrade against a site that does not know
# the release cannot be canaried); non-zero = the self-update path broke.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# The one repository slug the release channel already pins (the CLI's
# release_channel::REPO, install.sh's REPO).
REPO_SLUG="relatedb/relate"

log() { echo "[self-update-smoke] $*"; }
fail() {
  echo "[self-update-smoke] FAIL: $*" >&2
  exit 1
}

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    fail "$1 not found on PATH${2:+; $2}"
  fi
}

need gh
need jq
need curl

if [[ -z "${GITHUB_TOKEN:-}" ]]; then
  fail "GITHUB_TOKEN is not set — required while the repository is internal (the upgrade's API asset route, ADR-0358 §3); e.g. GITHUB_TOKEN=\$(gh auth token) bash $0"
fi

# --- Scratch space ----------------------------------------------------------
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
# The isolated HOME every spawn of the downloaded binary runs with.
ISOLATED_HOME="$WORK/home"
mkdir -p "$ISOLATED_HOME"

# --- Expected version -------------------------------------------------------
# The shared TOML reader (the same source release-bump.sh and
# build-wasm.sh use) reads the workspace version off Cargo.toml.
# shellcheck source=../../scripts/lib/toml.sh
source "$REPO/scripts/lib/toml.sh"
# The shared script helpers: sha256_of (common.sh's "do not paste
# copies" rule) checksums the PATH binary in the smoke section below.
# shellcheck source=../../scripts/lib/common.sh
source "$REPO/scripts/lib/common.sh"

expected="${EXPECTED_VERSION:-}"
if [[ -z "$expected" ]]; then
  expected="$(toml_section_version "$REPO/Cargo.toml" "[workspace.package]")"
fi
expected="${expected#v}"
if [[ ! "$expected" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  fail "expected version '$expected' (EXPECTED_VERSION or Cargo.toml) is not X.Y.Z"
fi
log "expected version: v$expected"

# --- Previous stable release --------------------------------------------------
prev_tag="${PREV_TAG:-}"
if [[ -z "$prev_tag" ]]; then
  # The same live-list read publish-releases-json.sh makes: gh release
  # list (newest 100 comfortably covers the head of the list), stable =
  # published non-draft non-prerelease, sorted by published_at newest
  # first. Only core-namespace tags (vX.Y.Z — the ADR-0250 §"Window rule"
  # stable shape) are candidates: the extension channel publishes its own
  # releases (relate-extensions-<name>-<X.Y.Z>, ADR-0431) between core
  # releases, and one of those must never be mistaken for a previous core
  # release. The previous stable release is the newest stable core tag
  # that is not the expected version — usually the second newest, because
  # the newest stable tag is the expected version itself.
  if ! gh release list --repo "$REPO_SLUG" --limit 100 \
    --json tagName,publishedAt,isDraft,isPrerelease >"$WORK/releases.json"; then
    fail "gh release list failed for $REPO_SLUG"
  fi
  prev_tag="$(jq -r --arg expected "$expected" '
    [ .[] | select((.isDraft | not) and (.isPrerelease | not))
          | select(.tagName | test("^v[0-9]+\\.[0-9]+\\.[0-9]+$")) ]
    | sort_by(.publishedAt) | reverse
    | map(select((.tagName | ltrimstr("v")) != $expected))
    | .[0].tagName // empty
  ' "$WORK/releases.json")"
  if [[ -z "$prev_tag" ]]; then
    # No stable core release other than the expected one: the expected
    # release is the first stable core release ever, so there is no
    # previous binary to upgrade from — a limitation of the release
    # history, not a broken self-update path. Skip (exit 0) before
    # downloading anything, the same way the floor case below skips.
    log "SKIP: v$expected is the first stable core release ever — the live release list has no stable core release (vX.Y.Z) other than v$expected to upgrade from; nothing downloaded, nothing upgraded"
    exit 0
  fi
fi
log "previous stable release: $prev_tag"

# --- The ADR-0358 floor --------------------------------------------------------
# v0.12.6 is the first release carrying the API-fallback download route;
# every earlier binary knows only the direct route, which 404s while the
# repository is internal, so an upgrade from one can never succeed. That
# is a limitation of the old binary, not a broken self-update path, so
# the smoke skips (exit 0) before downloading anything.
SELF_UPDATE_FLOOR="0.12.6"

# Is an X.Y.Z triple below the given floor? Numeric per-component
# comparison (10# guards a leading zero from being read as octal);
# anything that does not parse as a triple is a wiring error, failed
# loudly, never silently treated as above or below.
version_below() {
  local ver="$1" floor="$2" major minor patch floor_major floor_minor floor_patch
  IFS='.' read -r floor_major floor_minor floor_patch <<<"$floor"
  IFS='.' read -r major minor patch <<<"$ver"
  if [[ ! "$major" =~ ^[0-9]+$ || ! "$minor" =~ ^[0-9]+$ || ! "$patch" =~ ^[0-9]+$ ]]; then
    fail "previous release '$ver' does not parse as X.Y.Z"
  fi
  (( 10#$major > 10#$floor_major )) && return 1
  (( 10#$major < 10#$floor_major )) && return 0
  (( 10#$minor > 10#$floor_minor )) && return 1
  (( 10#$minor < 10#$floor_minor )) && return 0
  (( 10#$patch < 10#$floor_patch ))
}

if version_below "${prev_tag#v}" "$SELF_UPDATE_FLOOR"; then
  log "SKIP: $prev_tag predates v$SELF_UPDATE_FLOOR — pre-$SELF_UPDATE_FLOOR binaries cannot self-update while the repository is internal (ADR-0358); nothing downloaded, nothing upgraded"
  exit 0
fi

# --- The live-site currency gate ---------------------------------------------
# The inner upgrade reads the live website's releases.json (ADR-0250) and
# can only ever reach the version that document lists as latest. The
# release pipeline used to guarantee the site already listed the expected
# release by chaining this smoke after the Pages deploy (ADR-0330); the
# site is relate-web's now (ADR-0546) and can lag the release list — gh
# knows v<expected>, the site does
# not — and an upgrade run now would end in the wrong-version assert for
# a reason that is the pause, not the update path. Skip with a stated
# reason instead, the same contract as the first-stable-release and
# ADR-0358 floor skips: a limitation of the environment, not a broken
# self-update path. Only a well-formed latest tag different from the
# expected version skips — an unreachable or unreadable site is NOT a
# skip, because the inner upgrade must fail on that for real, so a
# failed or malformed read falls through and the smoke judges the
# upgrade on its own merits.
# releases_json.rs's RELEASES_JSON_URL, pinned here once — bash cannot
# read the Rust constant.
RELEASES_JSON_URL="https://relatedb.ai/releases.json"
site_latest="$(curl -fsSL "$RELEASES_JSON_URL" 2>/dev/null | jq -r '.latest.tag // empty' || true)"
if [[ "$site_latest" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ && "$site_latest" != "v$expected" ]]; then
  log "SKIP: the live site's releases.json lists $site_latest as latest, not v$expected — the site has not been redeployed for this release (the site is relate-web's, deployed on the release pipeline's poke, ADR-0546), so the upgrade path cannot be canaried against a site that does not know v$expected; nothing downloaded, nothing upgraded"
  exit 0
fi

# --- Platform asset -------------------------------------------------------------
# The same os/arch mapping install.sh and the release asset names use;
# ASSET overrides it for a host the mapping does not cover.
asset="${ASSET:-}"
if [[ -z "$asset" ]]; then
  case "$(uname -s)" in
    Darwin) os=darwin ;;
    Linux) os=linux ;;
    *) fail "unsupported OS: $(uname -s) (or set ASSET to the release asset to smoke)" ;;
  esac
  case "$(uname -m)" in
    x86_64 | amd64) arch=amd64 ;;
    arm64 | aarch64) arch=arm64 ;;
    *) fail "unsupported arch: $(uname -m) (or set ASSET to the release asset to smoke)" ;;
  esac
  asset="relate-$os-$arch"
fi
BIN="$WORK/$asset"

if ! gh release download "$prev_tag" --repo "$REPO_SLUG" \
  --pattern "$asset" --dir "$WORK"; then
  fail "release $prev_tag has no asset named $asset (or the download failed)"
fi
log "downloaded $asset from $prev_tag into $WORK"

# --- Integrity: the binary against the release's SHA256SUMS ------------------
# Every release publishes a SHA256SUMS asset over its CLI binaries
# (release.yml generates it, install.sh verifies it, the README points
# users at it). Download the previous release's manifest and
# checksum-verify the downloaded binary against the entry naming the
# asset BEFORE anything chmod +x's or executes it — an unverified
# previous-release binary must never run. Fail closed on every
# integrity break: a missing manifest, a malformed line, a missing
# entry, a checksum mismatch.
if ! gh release download "$prev_tag" --repo "$REPO_SLUG" \
  --pattern "SHA256SUMS" --dir "$WORK"; then
  fail "release $prev_tag has no asset named SHA256SUMS (or the download failed)"
fi
SUMS="$WORK/SHA256SUMS"

# Every line must be the shasum shape "<64-hex>  <name>" (two spaces,
# optional binary-mode asterisk — install.sh's format), except the
# optional ADR-0457 version header on line 1. One malformed line makes
# the whole manifest untrusted: fail closed on it rather than skip past
# it to the entry we want.
malformed_line="$(awk '
  NR == 1 && /^# relate-release-version: v[0-9]+\.[0-9]+\.[0-9]+$/ { next }
  /^[0-9a-fA-F]{64}[[:space:]]+\*?[^[:space:]]+$/ { next }
  { print; exit }
' "$SUMS")"
if [[ -n "$malformed_line" ]]; then
  fail "malformed SHA256SUMS line from $prev_tag: $malformed_line"
fi

# When the manifest carries the ADR-0457 version header, it must name the
# release whose assets were downloaded. (A headerless manifest predates
# the header; this smoke only verifies the previous release's artifact.)
sums_version="$(sed -n '1s/^# relate-release-version:[[:space:]]*//p' "$SUMS")"
if [[ -n "$sums_version" && "$sums_version" != "$prev_tag" ]]; then
  fail "SHA256SUMS from $prev_tag names version '$sums_version' — the manifest does not belong to this release"
fi

# The entry naming the asset — install.sh's lookup.
expected_sha="$(grep -E "^[0-9a-fA-F]{64}[[:space:]]+\*?${asset}\$" "$SUMS" | head -n 1 | awk '{print $1}' || true)"
if [[ -z "$expected_sha" ]]; then
  fail "SHA256SUMS from $prev_tag has no entry for $asset"
fi

actual_sha="$(sha256_of "$BIN")"
if [[ "$actual_sha" != "$expected_sha" ]]; then
  fail "checksum mismatch for $asset from $prev_tag: SHA256SUMS says $expected_sha, the downloaded binary is $actual_sha — executing nothing"
fi
log "verified $asset against SHA256SUMS from $prev_tag"

# Verified — only now may the binary become executable.
chmod +x "$BIN"

# --- Spawning the downloaded binary ---------------------------------------------
# Every spawn runs isolated:
#   - HOME and XDG_CONFIG_HOME point inside the scratch dir, so the
#     post-upgrade skill refresh finds no agent dirs and must skip
#     silently (ADR-0261 — no skill-install lines), and nothing the
#     binary writes (config, telemetry state) leaves the scratch dir.
#   - The CLI's dev/test route seams are unset: this smoke exists to
#     prove the live channel, and a caller-exported seam would silently
#     redirect it (npm-smoke-test.sh's `env -u RELATE_LIB`
#     precedent).
#   - DO_NOT_TRACK=1: a smoke run is not a user session (the release
#     workflow opts its lanes out the same way, ADR-0079).
# GITHUB_TOKEN passes through: the inner upgrade's API asset fallback
# needs it while the repository is internal (ADR-0358 §3).
run_bin() {
  env -u RELATE_RELEASES_JSON_URL -u RELATE_RELEASE_DOWNLOAD_URL \
    -u RELATE_GITHUB_API_URL \
    HOME="$ISOLATED_HOME" XDG_CONFIG_HOME="$ISOLATED_HOME/.config" \
    DO_NOT_TRACK=1 "$BIN" "$@"
}

# The downloaded copy must report the previous release's version before
# anything is upgraded — proves the right asset came down.
if ! before="$(run_bin --version)"; then
  fail "the downloaded $asset from $prev_tag failed to run (--version)"
fi
if ! grep -q "${prev_tag#v}" <<<"$before"; then
  fail "the downloaded $asset does not report $prev_tag: $before"
fi
log "downloaded copy reports: $(head -n 1 <<<"$before")"

# --- The smoke --------------------------------------------------------------------
# The relate installed on PATH, if any, must come out of this
# untouched; checksum it before and after with sha256_of (sourced from
# scripts/lib/common.sh above — the shared helper, not a pasted copy;
# distribution/install.sh alone keeps its own inline resolution,
# because a piped curl|sh installer cannot source repo files).
path_bin=""
path_sha=""
if command -v relate >/dev/null 2>&1; then
  path_bin="$(command -v relate)"
  path_sha="$(sha256_of "$path_bin")"
  log "relate on PATH: $path_bin — must stay untouched"
else
  log "no relate on PATH; the untouched-on-PATH check is vacuous"
fi

upgrade_log="$WORK/upgrade.log"
if ! run_bin upgrade >"$upgrade_log" 2>&1; then
  cat "$upgrade_log" >&2
  fail "self-upgrade of the isolated $prev_tag copy exited non-zero (transcript above)"
fi
cat "$upgrade_log"

if grep -qi skill "$upgrade_log"; then
  fail "skill lines in the upgrade output — the isolated HOME has no agent dirs, so the ADR-0261 refresh must skip silently: $(grep -i skill "$upgrade_log" | head -n 1)"
fi

if [[ -n "$path_bin" ]] && [[ "$(sha256_of "$path_bin")" != "$path_sha" ]]; then
  fail "the upgrade modified the relate installed on PATH ($path_bin)"
fi

if ! after="$(run_bin --version)"; then
  fail "the upgraded copy failed to run (--version)"
fi
if ! grep -q "$expected" <<<"$after"; then
  fail "the upgraded copy does not report v$expected: $(head -n 1 <<<"$after")"
fi


log "PASS: $prev_tag -> v$expected self-upgrade verified ($asset, isolated in $WORK)"
