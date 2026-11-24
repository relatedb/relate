#!/bin/sh
# The scripts/lib/common.sh helper suite: sha256_of's bare-hex contract.
#
# sha256_of feeds every digest-pinned install (cargo-auditable,
# cargo-deny) and the install.sh test fixtures; its contract is bare
# hex, whatever the file's name looks like. GNU sha256sum and perl
# shasum both mark an output line whose file name carries a backslash
# or newline with a leading '\' before the digest, and the release
# workflow's Windows legs pass $RUNNER_TEMP paths (C:\Users\...) in —
# the v1.1.0 windows FFI job failed its cargo-auditable digest pin on
# exactly that marker (want 717ba2…, got \717ba2…). The helper hashes
# through stdin so no name spelling can leak the marker into a digest;
# these cases pin that.
#
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)

# common.sh's bodies are POSIX sh, so `.` works here.
. "$ROOT/scripts/lib/common.sh"
# The shared TAP harness (LABS-622): tap_check/tap_done, assert_same,
# the counters, output format, and sandbox policy spelled once.
. "$ROOT/scripts/tests/lib/tap.sh"

tap_sandbox common-sh-test

# --- 0. The bare-hex shape ------------------------------------------------------

# bare_hex: 0 when $1 is exactly 64 lower-hex characters — no leading
# '\', no trailing field, no spaces. assert_same alone cannot rule the
# marker out: two marked digests compare equal too.
bare_hex() { # $1 = value
	case $1 in
	'' | *[!0-9a-f]*)
		return 1
		;;
	esac
	[ ${#1} -eq 64 ]
}

# --- 1. Name spellings ----------------------------------------------------------

printf 'relate' > "$sandbox/plain.zip"

# The Windows-runner shape: $RUNNER_TEMP arrives as C:\Users\... and the
# digest-pinned installs pass "$DIR/$ARCHIVE" straight in. Backslash is
# an ordinary character in a POSIX file name, so the sandbox reproduces
# the exact mixed-separator path the runner hashed.
win_parent='C:\Users\runneradmin\AppData\Local\Temp\cargo-auditable'
mkdir -p "$sandbox/$win_parent"
win_file="$sandbox/$win_parent/cargo-auditable-x86_64-pc-windows-msvc.zip"
printf 'relate' > "$win_file"

# A newline in the name is the tools' other marker trigger. The newline
# is spelled literally inside the quoted name: a command-substitution
# construction cannot carry it ($(printf '\n') is empty — substitution
# strips trailing newlines), and a literal in the source cannot be
# stripped by anything.
nl_file="$sandbox/line
break.zip"
printf 'relate' > "$nl_file"

plain_digest=$(sha256_of "$sandbox/plain.zip")
bare_hex "$plain_digest"
tap_check "plain name -> bare hex (64 lower-hex)" $?

win_digest=$(sha256_of "$win_file")
bare_hex "$win_digest"
tap_check "backslash-carrying name (the Windows \$RUNNER_TEMP shape) -> bare hex" $?

nl_digest=$(sha256_of "$nl_file")
bare_hex "$nl_digest"
tap_check "newline-carrying name -> bare hex" $?

assert_same \
	"the same content hashes identically under every name spelling" \
	"$plain_digest" "$win_digest" "$nl_digest"

# --- 2. The caller contract -----------------------------------------------------

# A missing file yields no output and a failed status, never a
# fabricated digest and never silent success: the digest-pinned
# installs guard on -z, and the helper owns its failure under any
# shell — no pipefail inheritance. The status is read plainly off the
# call (assert_failed_empty), so the assertion pins the helper's own
# exit, not a sentinel.
assert_failed_empty \
	"missing file -> no output, failed status (the helper owns its failure)" \
	sha256_of "$sandbox/absent.zip"

# --- 3. The python fallback -----------------------------------------------------

# A host that ships neither coreutils: the helper falls back to python,
# which reads the path from argv, so no name spelling can confuse it
# either. sha256_of_under narrows PATH to $2 and invokes $1 — the shell,
# named by its absolute path so the narrowed PATH cannot hide it — over
# $3.
sh_bin=$(command -v sh)
sha256_of_under() { # $1 = shell, $2 = PATH, $3 = file
	PATH="$2" "$1" -c '. "$1"; sha256_of "$2"' \
		sh "$ROOT/scripts/lib/common.sh" "$3"
}
py_only="$sandbox/py-only"
mkdir -p "$py_only"
py_src=
for cand in python3 python; do
	if command -v "$cand" >/dev/null 2>&1; then
		py_src=$(command -v "$cand")
		ln -s "$py_src" "$py_only/$cand"
		break
	fi
done
if [ -n "$py_src" ]; then
	fallback_digest=$(sha256_of_under "$sh_bin" "$py_only" "$win_file")
	assert_same \
		"the python fallback hashes the same bytes as the coreutils path" \
		"$plain_digest" "$fallback_digest"
else
	tap_skip \
		"the python fallback hashes the same bytes as the coreutils path" \
		"no python on PATH"
fi

# --- 4. No digest tool at all ---------------------------------------------------

# With no digest tool at all the helper still fails loudly: no output,
# failed status — never a fabricated digest.
no_tools="$sandbox/no-tools"
mkdir -p "$no_tools"
assert_failed_empty \
	"no digest tool -> no output, failed status" \
	sha256_of_under "$sh_bin" "$no_tools" "$sandbox/plain.zip"

# --- 5. host_key ----------------------------------------------------------------

# host_key maps `uname -s -m` to OS-ARCH, the installers' one host
# vocabulary; a stub uname on PATH stands in for each host.
host_key_for() { # $1 = uname -s, $2 = uname -m
	stub="$sandbox/uname-$1-$2"
	mkdir -p "$stub"
	cat >"$stub/uname" <<STUB
#!/bin/sh
case "\$1" in
-s) echo '$1' ;;
-m) echo '$2' ;;
esac
STUB
	chmod +x "$stub/uname"
	PATH="$stub:$PATH" host_key
}

assert_same "host_key: Linux x86_64" "linux-x86_64" "$(host_key_for Linux x86_64)"
assert_same "host_key: Linux amd64 spells x86_64" "linux-x86_64" "$(host_key_for Linux amd64)"
assert_same "host_key: Linux arm64 spells aarch64" "linux-aarch64" "$(host_key_for Linux arm64)"
assert_same "host_key: Linux armv7l" "linux-armv7" "$(host_key_for Linux armv7l)"
assert_same "host_key: Darwin arm64" "darwin-aarch64" "$(host_key_for Darwin arm64)"
assert_same "host_key: MINGW64 x86_64" "windows-x86_64" "$(host_key_for MINGW64_NT-10.0 x86_64)"
assert_failed_empty "host_key: an unknown OS prints nothing and fails" host_key_for FreeBSD x86_64
assert_failed_empty "host_key: an unknown arch prints nothing and fails" host_key_for Linux riscv64

tap_done
