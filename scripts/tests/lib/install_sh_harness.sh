# Harness machinery for the install.sh test harness
# (scripts/tests/test_install_sh.sh): the fixture and stub factories, the
# per-case run harness, the real-ssh-keygen roundtrip machinery, and the
# argv-log helpers.
#
# Expects from the caller, before sourcing: `tap_sandbox` has set
# `$sandbox`, tap.sh (for the `tap_skip` the roundtrip skips use) and
# scripts/lib/common.sh (for `sha256_of`) are sourced. The caller also
# owns `$ROOT`, `$INSTALLER`, `$fixtures`, `$stubbin` and `$itmp` (its
# sandbox layout). The factories — `install_sh_fixtures`,
# `install_sh_stubs` and `build_real_roundtrip_fixtures` — expect the
# caller's section-0 guards to have set `$LEGACY_MANIFEST_TAG` and
# `$PUBLISHER_FIXTURE_TAG` by the time they are called (the guards run
# after this file is sourced and before the factories). Never executed
# directly.
#
# The two factories build the release-asset fixtures and the PATH stubs the
# installer is exercised against; their shared table is below.

# The shared table: the fixture tag/prefix spellings the fixture writers and
# assertions consult.
# The tag the default fixture release resolves to. The signed SHA256SUMS
# fixture names it in its ADR-0457 version header, and the releases.json
# documents name it in their latest slot.
TEST_TAG=v9.9.9

# A tag the default release must not match: the wrong-version fixture names
# it in the header, and the assertion expects the mismatch.
SUMS_WRONG_TAG=v9.9.8

# The ADR-0457 header prefix as the fixture writer spells it. Section 0 of
# the harness holds this and install.sh's copy to manifest.rs's constant.
SUMS_VERSION_PREFIX='# relate-release-version: '

# Write the releases.json document whose latest names tag $1 to path $2, in
# the publisher's jq serialization (one key per line, two-space indents) —
# the shape install.sh's parse reads. A null/empty tag emits the null-latest
# shape (ADR-0250 §2's nullable slot: no stable release published) over the
# same template: `"latest": null` and the empty releases window.
# latest_prerelease is non-null with a different tag on purpose: a parse
# that reads the wrong slot must fail the install loudly, never install the
# prerelease.
write_releases_fixture() { # $1 = tag, $2 = output path
	_tag="$1"
	_out="$2"
	# The latest slot and the releases window are the only tag-driven
	# parts of the one template below: a real tag fills both with the
	# release entry, the null/empty arm with the null keyword and the
	# empty array.
	case "$_tag" in
	'' | null)
		_latest=null
		_window='[]'
		;;
	*)
		_latest=$(printf '{
    "tag": "%s",
    "published_at": "2026-09-01T00:00:00Z",
    "notes": "",
    "prerelease": false,
    "assets": [
      "relate-darwin-arm64",
      "relate-linux-amd64",
      "SHA256SUMS",
      "SHA256SUMS.sig"
    ]
  }' "$_tag")
		_window=$(printf '[
    {
      "tag": "%s",
      "published_at": "2026-09-01T00:00:00Z",
      "notes": "",
      "prerelease": false,
      "assets": [
        "relate-darwin-arm64",
        "relate-linux-amd64",
        "SHA256SUMS",
        "SHA256SUMS.sig"
      ]
    }
  ]' "$_tag")
		;;
	esac
	cat <<EOF >"$_out"
{
  "schema_version": 1,
  "generated_at": "2026-09-01T00:00:00Z",
  "latest": $_latest,
  "latest_prerelease": {
    "tag": "v0.11.0-rc.1",
    "published_at": "2026-09-01T00:00:00Z",
    "notes": "",
    "prerelease": true,
    "assets": [
      "relate-darwin-arm64"
    ]
  },
  "releases": $_window
}
EOF
}

# Build the release-asset fixtures under $fixtures: the two platform
# binaries, their corrupted variant, SHA256SUMS (with the ADR-0457 header)
# and its header variants, the signature placeholder, and the releases.json
# metadata documents the stub's metadata arm serves. The caller sets
# LEGACY_MANIFEST_TAG (the guarded ceiling, `vX.Y.Z`) and
# PUBLISHER_FIXTURE_TAG (the committed publisher fixture's latest.tag,
# extracted by the section-0 guard) first.
install_sh_fixtures() {
	cat >"$fixtures/relate-darwin-arm64" <<'EOF'
#!/bin/sh
echo "relate fixture darwin-arm64"
EOF

	cat >"$fixtures/relate-linux-amd64" <<'EOF'
#!/bin/sh
echo "relate fixture linux-amd64"
EOF

	# Corrupted variant of the darwin binary for the checksum-fail case.
	{ cat "$fixtures/relate-darwin-arm64"; printf 'corrupt-bytes'; } \
		>"$fixtures/relate-darwin-arm64.corrupt"

	# SHA256SUMS in the format install.sh consumes: the ADR-0457 version
	# header first, then "<hex>  <name>" (two spaces).
	{
		printf '%s%s\n' "$SUMS_VERSION_PREFIX" "$TEST_TAG"
		printf '%s  %s\n' "$(sha256_of "$fixtures/relate-darwin-arm64")" relate-darwin-arm64
		printf '%s  %s\n' "$(sha256_of "$fixtures/relate-linux-amd64")" relate-linux-amd64
	} >"$fixtures/SHA256SUMS"

	# Headerless variant: a pre-ADR-0457 legacy manifest.
	sed '1d' "$fixtures/SHA256SUMS" >"$fixtures/SHA256SUMS.no-version"

	# Header naming another release: a replay/mismatch the installer must
	# reject.
	sed "1s|.*|${SUMS_VERSION_PREFIX}${SUMS_WRONG_TAG}|" \
		"$fixtures/SHA256SUMS" >"$fixtures/SHA256SUMS.wrong-version"

	# Present-but-empty header: a publish bug, not a legacy manifest — it
	# must fail hard, mirroring the Rust empty-header arm.
	sed "1s|.*|${SUMS_VERSION_PREFIX}|" \
		"$fixtures/SHA256SUMS" >"$fixtures/SHA256SUMS.empty-version"

	# No-space spelling (the prefix's trailing space removed): the Rust
	# parser's prefix literal requires it, so this is a headerless legacy
	# manifest.
	sed "1s|.*|${SUMS_VERSION_PREFIX% }${TEST_TAG}|" \
		"$fixtures/SHA256SUMS" >"$fixtures/SHA256SUMS.no-space-header"

	# Extra leading whitespace in the value: Rust trims both ends before
	# comparing, so the installer must too (the written form has exactly one
	# space, but the parsers agree on the tolerance).
	sed "1s|.*|${SUMS_VERSION_PREFIX} ${TEST_TAG}|" \
		"$fixtures/SHA256SUMS" >"$fixtures/SHA256SUMS.extra-space-header"

	# The signature is opaque to the installer (it only feeds the file to
	# ssh-keygen), so the fixture just needs to exist as a distinct asset.
	printf 'untrusted comment: relate test signature fixture\n' \
		>"$fixtures/SHA256SUMS.sig"

	# The empty body the failure modes serve: a 404/403 response whose
	# bytes are never used (install.sh dies on the status), as one shared
	# fixture rather than a per-arm sentinel.
	: >"$fixtures/empty"

	# The releases.json documents the stub's metadata arm serves: the
	# default latest (TEST_TAG) and the legacy-ceiling latest (the
	# legacy-tag case installs that release), plus the malformed, null
	# and bad-tag-shape variants for the resolution-failure cases. The
	# caller sets LEGACY_MANIFEST_TAG (the guarded ceiling) first.
	write_releases_fixture "$TEST_TAG" "$fixtures/releases.json"
	write_releases_fixture "$LEGACY_MANIFEST_TAG" "$fixtures/releases-legacy-tag.json"
	write_releases_fixture not-a-release-tag "$fixtures/releases-bad-tag.json"
	# A valid document whose latest is null (ADR-0250 §2's nullable slot:
	# no stable release published) — the prerelease slot stays non-null so
	# a parse that falls into it still fails loudly.
	write_releases_fixture null "$fixtures/releases-null-latest.json"
	# Not JSON at all: the bluntest malformed body.
	printf 'not json\n' >"$fixtures/releases-malformed.json"

	# The publisher's own committed output fixture — the document its
	# bats suite compares publish-releases-json.sh's output against —
	# served byte-for-byte (publisher_doc mode): the end-to-end pin that
	# the serialization the producer writes is the grammar install.sh's
	# latest_tag_of reads. Its latest.tag is PUBLISHER_FIXTURE_TAG
	# (derived by the caller's section-0 guard). The SHA256SUMS variant
	# follows: the manifest's ADR-0457 header must name the same tag or
	# the version binding rejects the release.
	cp "$ROOT/.github/scripts/tests/fixtures/publish-releases-json-expected.json" \
		"$fixtures/releases-publisher.json"
	sed "1s|.*|${SUMS_VERSION_PREFIX}${PUBLISHER_FIXTURE_TAG}|" \
		"$fixtures/SHA256SUMS" >"$fixtures/SHA256SUMS.publisher-tag"
}

# The STUB_MODE vocabulary is owned by the stub curl's dispatch (the
# comment above its resolution switch): releases_* select the metadata
# (releases.json) arm's document; sums_* select the manifest variant the
# direct sums arm serves; legacy_tag and publisher_doc select both arms
# together, serving one release end to end; sums_404, sums_403, sig_404
# and binary_404 fail one direct-route asset with the named status;
# corrupt and sig_invalid fail the binary body and the signature check.
# `ok` is the neutral value reset_case restores.

# Build the PATH stubs under $stubbin and sets `nosshbin` (the same stubs
# minus ssh-keygen, for the openssh-client-absent case). The stub curl never
# touches the network: it logs its argv (one arg per line, calls separated
# by an =CALL= marker) and serves fixtures keyed on the request URL, with
# failure modes selected via STUB_MODE. The direct-route requests arrive as
# `-o <file> -w '%{http_code}'`; serve() honors both: the body goes to the
# -o target (stdout when absent), and the status prints after it only when
# -w asked for one. Every URL outside the arms below — the metadata document
# and the three direct-route assets — hits the unexpected-URL arm and fails
# loudly, so any GitHub API request (the route the installer must never take
# again, ADR-0585) fails the install instead of silently succeeding.
install_sh_stubs() {
	cat >"$stubbin/curl" <<'EOF'
#!/bin/sh
# Stub curl: logs argv (one arg per line, calls separated by =CALL=) and
# serves fixtures based on the URL (last argument). Never uses the network.
{
	printf '=CALL=\n'
	for arg in "$@"; do printf '%s\n' "$arg"; done
} >>"$STUB_LOG"

url=""
out=""
want_code=""
prev=""
for arg in "$@"; do
	url="$arg"
	case "$prev" in
	-o) out="$arg" ;;
	-w)
		case "$arg" in
		*%{http_code}*) want_code=1 ;;
		esac
		;;
	esac
	prev="$arg"
done

# STUB_MODE is resolved once here, before the URL dispatch — the one
# site a mode's complete behavior lives at: the metadata document, each
# direct asset's body fixture and status. Every knob is a plain fixture
# path with a 200 default; a mode overrides exactly what it changes, the
# failure modes pointing their asset at the shared empty fixture beside a
# non-2xx status. The dispatch is total and fail-closed — an unknown
# mode fails every request loudly — so this case, not a side table, is
# the one place the mode vocabulary lives.
metadata_doc=releases.json
sums_body="$STUB_FIXTURES/SHA256SUMS"
sums_status=200
sig_body="$STUB_FIXTURES/SHA256SUMS.sig"
sig_status=200
darwin_body="$STUB_FIXTURES/relate-darwin-arm64"
darwin_status=200
case "$STUB_MODE" in
ok | sig_invalid) ;;
lookup_404) exit 22 ;;
releases_malformed) metadata_doc=releases-malformed.json ;;
releases_null_latest) metadata_doc=releases-null-latest.json ;;
releases_bad_tag) metadata_doc=releases-bad-tag.json ;;
legacy_tag)
	metadata_doc=releases-legacy-tag.json
	sums_body="$STUB_FIXTURES/SHA256SUMS.no-version"
	;;
publisher_doc)
	metadata_doc=releases-publisher.json
	sums_body="$STUB_FIXTURES/SHA256SUMS.publisher-tag"
	;;
sums_no_version) sums_body="$STUB_FIXTURES/SHA256SUMS.no-version" ;;
sums_wrong_version) sums_body="$STUB_FIXTURES/SHA256SUMS.wrong-version" ;;
sums_empty_version) sums_body="$STUB_FIXTURES/SHA256SUMS.empty-version" ;;
sums_no_space_header) sums_body="$STUB_FIXTURES/SHA256SUMS.no-space-header" ;;
sums_extra_space_header) sums_body="$STUB_FIXTURES/SHA256SUMS.extra-space-header" ;;
sums_404)
	sums_body="$STUB_FIXTURES/empty"
	sums_status=404
	;;
sums_403)
	sums_body="$STUB_FIXTURES/empty"
	sums_status=403
	;;
sig_404)
	sig_body="$STUB_FIXTURES/empty"
	sig_status=404
	;;
corrupt) darwin_body="$STUB_FIXTURES/relate-darwin-arm64.corrupt" ;;
binary_404)
	darwin_body="$STUB_FIXTURES/empty"
	darwin_status=404
	;;
*)
	echo "stub curl: unexpected STUB_MODE: $STUB_MODE" >&2
	exit 22
	;;
esac

# serve $1 (a body fixture file) with status $2: the body to the -o
# target when one was given (stdout otherwise), the status after it only
# when -w asked for one. A missing fixture file fails loudly rather than
# serving an empty success.
serve() {
	if [ -f "$1" ]; then
		if [ -n "$out" ]; then cat "$1" >"$out"; else cat "$1"; fi
	else
		echo "stub curl: missing fixture: $1" >&2
		exit 22
	fi
	[ -z "$want_code" ] || printf '%s\n' "$2"
	exit 0
}

case "$url" in
*releases.json) serve "$STUB_FIXTURES/$metadata_doc" 200 ;;
*releases/download/*/SHA256SUMS) serve "$sums_body" "$sums_status" ;;
*releases/download/*/SHA256SUMS.sig) serve "$sig_body" "$sig_status" ;;
*releases/download/*/relate-darwin-arm64) serve "$darwin_body" "$darwin_status" ;;
*releases/download/*/relate-linux-amd64)
	serve "$STUB_FIXTURES/relate-linux-amd64" 200
	;;
*)
	echo "stub curl: unexpected URL: $url" >&2
	exit 22
	;;
esac
EOF

	cat >"$stubbin/uname" <<'EOF'
#!/bin/sh
# Stub uname: reports the OS/arch under test.
case "${1:-}" in
-s) printf '%s\n' "$STUB_UNAME_S" ;;
-m) printf '%s\n' "$STUB_UNAME_M" ;;
*) printf '%s\n' "$STUB_UNAME_S" ;;
esac
EOF

	chmod +x "$stubbin/curl" "$stubbin/uname"

	cat >"$stubbin/ssh-keygen" <<'EOF'
#!/bin/sh
# Stub ssh-keygen: logs argv in the same =CALL= format as stub curl, then
# succeeds or fails based on the mode env var. It never inspects the signature
# or stdin; the harness only exercises install.sh's control flow.
{
	printf '=CALL=\n'
	for arg in "$@"; do printf '%s\n' "$arg"; done
} >>"$STUB_KEYGEN_LOG"

case "$STUB_MODE" in
sig_invalid) exit 1 ;;
*) exit 0 ;;
esac
EOF

	chmod +x "$stubbin/ssh-keygen"

	# Same stubs minus ssh-keygen, for the "openssh-client absent" case. The
	# sandbox cannot rely on deleting it from $stubbin (other cases need it),
	# and /usr/bin cannot be left on PATH because the host may ship a real
	# ssh-keygen.
	nosshbin="$sandbox/stub-bin-no-ssh"
	mkdir -p "$nosshbin"
	cp "$stubbin/curl" "$stubbin/uname" "$nosshbin/"
	chmod +x "$nosshbin/curl" "$nosshbin/uname"
}

# Reset the per-case knobs to their defaults. The case sets STUB_MODE,
# STUB_UNAME_S/_M, GITHUB_TOKEN (set in the environment: the installer
# must ignore it, ADR-0585), RELATE_RELEASES_JSON_URL,
# RELATE_RELEASE_DOWNLOAD_URL, RUN_PATH or RUN_INSTALLER after calling
# it.
reset_case() {
	STUB_MODE=ok
	STUB_UNAME_S=Darwin
	STUB_UNAME_M=arm64
	GITHUB_TOKEN=
	RELATE_RELEASES_JSON_URL=
	RELATE_RELEASE_DOWNLOAD_URL=
	STUB_FIXTURES_DIR=$fixtures
	RUN_PATH="$stubbin:/usr/bin:/bin"
	RUN_INSTALLER=$INSTALLER
}

# Run the installer fully sandboxed. Populates RUN_STATUS, RUN_HOME, RUN_LOG
# (stub curl argv log) and RUN_ERR (installer stderr). RUN_INSTALLER selects
# the script under test: the shipped install.sh everywhere except the
# real-roundtrip cases, which run a sed-patched copy (see section 3c).
install_run() { # $1 = case name
	case_name=$1
	RUN_HOME="$sandbox/home-$case_name"
	RUN_LOG="$sandbox/curl-$case_name.log"
	RUN_KEYGEN_LOG="$sandbox/keygen-$case_name.log"
	RUN_ERR="$sandbox/err-$case_name.log"
	mkdir -p "$RUN_HOME"
	: >"$RUN_LOG"
	: >"$RUN_KEYGEN_LOG"
	env -i \
		HOME="$RUN_HOME" \
		PATH="$RUN_PATH" \
		TMPDIR="$itmp" \
		STUB_LOG="$RUN_LOG" \
		STUB_KEYGEN_LOG="$RUN_KEYGEN_LOG" \
		STUB_FIXTURES="$STUB_FIXTURES_DIR" \
		STUB_MODE="$STUB_MODE" \
		STUB_UNAME_S="$STUB_UNAME_S" \
		STUB_UNAME_M="$STUB_UNAME_M" \
		${GITHUB_TOKEN:+GITHUB_TOKEN="$GITHUB_TOKEN"} \
		${RELATE_RELEASES_JSON_URL+RELATE_RELEASES_JSON_URL="$RELATE_RELEASES_JSON_URL"} \
		${RELATE_RELEASE_DOWNLOAD_URL+RELATE_RELEASE_DOWNLOAD_URL="$RELATE_RELEASE_DOWNLOAD_URL"} \
		/bin/sh "$RUN_INSTALLER" >"$sandbox/out-$case_name.log" 2>"$RUN_ERR"
	RUN_STATUS=$?
}

# Log helpers over the stub curl argv log. The URL regex is awk's: it
# matches any argument line, so anchor or escape it to the URL shape
# ('api\.github\.com', '/releases/download/', 'releases\.json').

# How many direct-route download requests the case made.
direct_call_count() {
	grep -c '/releases/download/' "$RUN_LOG"
}

# How many GitHub API requests the case made. The installer consults no
# GitHub API endpoint (ADR-0585), so this is zero in every case — the
# assertion shape that pins it.
api_call_count() {
	grep -c 'api\.github\.com' "$RUN_LOG"
}

# --- real ssh-keygen roundtrip machinery -------------------------------------
# These cases exercise the real verify path end to end: the stub curl still
# serves fixtures offline, but the stub ssh-keygen is removed from PATH so
# install.sh invokes the real ssh-keygen against a throwaway Ed25519 keypair
# generated inside the sandbox (it never leaves it). The shipped script pins
# SIGNING_KEY as a constant, so these cases run a sed-patched COPY of
# install.sh (built inside the sandbox, never touching the repo file) whose
# pinned-key line is replaced with the throwaway public key; every other
# case runs the shipped script byte-for-byte.
real_roundtrip=""
real_skip_reason=""

# Generates a throwaway signing keypair at $1.
make_key() {
	"$real_keygen" -t ed25519 -N '' -C relate-release \
		-f "$1" </dev/null >/dev/null 2>&1
}

# Signs the SHA256SUMS in fixture tree $2 with key $1. The rm is
# load-bearing: ssh-keygen -Y sign refuses to overwrite an existing .sig
# without a tty, so the copied placeholder signature must go first.
sign_manifest() {
	rm -f "$2/SHA256SUMS.sig"
	"$real_keygen" -Y sign -f "$1" -n relate-release \
		"$2/SHA256SUMS" </dev/null >/dev/null 2>&1
}

# The real-roundtrip install protocol: run the patched installer copy
# against fixture tree $2 under the stub-curl case name $1. Call only
# under real_roundtrip=1 — patched_installer and keygen_dir exist only
# after build_real_roundtrip_fixtures.
real_install_run() { # $1 = case name, $2 = fixture tree
	reset_case
	STUB_FIXTURES_DIR=$2
	RUN_INSTALLER=$patched_installer
	RUN_PATH="$nosshbin:$keygen_dir:/usr/bin:/bin"
	install_run "$1"
}

# The skip arm every case shares: one tap_skip per description, under the
# failure that actually happened.
skip_real_case() {
	for d in "$@"; do
		tap_skip "$d" "$real_skip_reason"
	done
}

# The one availability probe and fixture build for the three
# real-roundtrip cases — the patched installer copy, the first key's
# signed tree (cases 1-2), and the second key's own tree (case 3) — as
# flat early-return guards, each failure reason beside its own command:
# an absent binary and a present-but-failing keygen are different skips.
# Sets real_roundtrip=1 only when the cases can consume ready fixtures
# untouched.
build_real_roundtrip_fixtures() {
	real_keygen=$(command -v ssh-keygen 2>/dev/null)
	real_fixtures="$sandbox/fixtures-real"
	[ -n "$real_keygen" ] || {
		real_skip_reason="ssh-keygen not available"
		return 1
	}
	make_key "$sandbox/realkey" || {
		real_skip_reason="ssh-keygen present but the first throwaway keypair failed"
		return 1
	}
	throwaway_pub=$(cat "$sandbox/realkey.pub")
	keygen_dir=$(dirname "$real_keygen")
	# Patched copy of the shipped installer: only the SIGNING_KEY='...'
	# line is substituted, pointing verification at the throwaway key.
	patched_installer="$sandbox/install-patched.sh"
	sed "s|^SIGNING_KEY='.*'\$|SIGNING_KEY='$throwaway_pub'|" \
		"$INSTALLER" >"$patched_installer" || {
		real_skip_reason="building the patched installer copy failed"
		return 1
	}
	# The first key's tree: the stub's fixture tree with a real
	# signature over SHA256SUMS made with the throwaway key.
	mkdir -p "$real_fixtures" &&
		cp "$fixtures"/* "$real_fixtures"/ || {
		real_skip_reason="building the first key's fixture tree failed"
		return 1
	}
	sign_manifest "$sandbox/realkey" "$real_fixtures" || {
		real_skip_reason="ssh-keygen present but signing the fixture manifest failed"
		return 1
	}
	# Case 3's own tree: a byte-identical manifest copy carrying the
	# SECOND key's signature — the cases never mutate a shared fixture.
	wrong_fixtures="$sandbox/fixtures-real-wrong"
	cp -r "$real_fixtures" "$wrong_fixtures" || {
		real_skip_reason="building the wrong-key fixture tree failed"
		return 1
	}
	make_key "$sandbox/realkey2" || {
		real_skip_reason="ssh-keygen present but the second throwaway keypair failed"
		return 1
	}
	sign_manifest "$sandbox/realkey2" "$wrong_fixtures" || {
		real_skip_reason="ssh-keygen present but the wrong-key signature failed"
		return 1
	}
	real_roundtrip=1
}
