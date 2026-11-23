#!/bin/sh
# Hermetic suite for the repository signing key and
# scripts/sign-repo-metadata.sh.
#
# The committed production pair — distribution/repo-signing-key.asc and
# its .fingerprint — is checked against the committed fixture signature:
# gpg --verify must accept the signed fixture metadata against the
# committed public key, and must reject tampered metadata. The signing
# script's own happy path cannot use the production key (its private half
# exists only as the REPO_GPG_KEY Actions secret, by design), so the suite
# copies the script into a sandbox repository whose committed key files
# hold a freshly generated fixture keypair and runs the full script
# against that — the same code path the publish workflow runs, with the
# production key only ever touched through its committed public half.
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
SCRIPT="$ROOT/scripts/sign-repo-metadata.sh"
PUBLIC_KEY="$ROOT/distribution/repo-signing-key.asc"
FINGERPRINT_FILE="$ROOT/distribution/repo-signing-key.fingerprint"
FIXTURE_RELEASE="$ROOT/scripts/tests/fixtures/repo-signing/Release"
FIXTURE_SIGNATURE="$ROOT/scripts/tests/fixtures/repo-signing/Release.gpg"

# The shared TAP harness (LABS-622): tap_check/tap_skip/tap_done,
# assert_same, tap_sandbox, the counters, output format, and sandbox
# policy spelled once.
. "$ROOT/scripts/tests/lib/tap.sh"

# The suite needs gpg; a host without it skips the whole suite under its
# own name instead of running on into misleading not-oks.
if ! command -v gpg >/dev/null 2>&1; then
	tap_skip "repository signing key suite" "gpg not available"
	tap_done
	exit 0
fi

# Short sandbox tag, short GNUPGHOME names: gpg-agent sockets must fit
# the kernel's 104-char sun_path limit, and the macOS TMPDIR default
# already spends most of it (keygen fails with "File name too long"
# otherwise).
tap_sandbox repo-sign-key

# --- The committed production key --------------------------------------

# The consumer-side flow: the fixture repository tree carries the
# committed signature, and the committed public key is the only trust
# anchor — exactly the keyring a user's apt/dnf would hold.
keyring="$sandbox/keyring"
mkdir -m 700 "$keyring"
gpg --homedir "$keyring" --batch --import "$PUBLIC_KEY" 2>/dev/null

tree="$sandbox/repo-tree"
mkdir -p "$tree/dists/stable"
cp "$FIXTURE_RELEASE" "$tree/dists/stable/Release"
cp "$FIXTURE_SIGNATURE" "$tree/dists/stable/Release.gpg"

gpg --homedir "$keyring" --batch --verify \
	"$tree/dists/stable/Release.gpg" "$tree/dists/stable/Release" >/dev/null 2>&1
tap_check "gpg --verify accepts the signed fixture metadata against the committed public key" $?

# The negative control: a metadata byte changed after signing must fail
# verification — the signature has to bind the exact bytes.
echo "Tampered: no" >> "$tree/dists/stable/Release"
gpg --homedir "$keyring" --batch --verify \
	"$tree/dists/stable/Release.gpg" "$tree/dists/stable/Release" >/dev/null 2>&1
if [ $? -ne 0 ]; then
	tap_ok "gpg --verify rejects tampered fixture metadata"
else
	tap_not_ok "gpg --verify rejects tampered fixture metadata"
fi

# The fingerprint recorded beside the key is the key's own, 40-hex.
key_fingerprint=$(gpg --show-keys --with-colons "$PUBLIC_KEY" 2>/dev/null \
	| grep '^fpr:' | head -n 1 | cut -d: -f10)
recorded_fingerprint=$(tr -d ' \t\r\n' < "$FINGERPRINT_FILE")
assert_same "the recorded fingerprint is the committed public key's fingerprint" \
	"$key_fingerprint" "$recorded_fingerprint"
echo "$recorded_fingerprint" | grep -Eq '^[0-9A-F]{40}$'
tap_check "the recorded fingerprint is 40 uppercase hex characters" $?

# --- The signing script ------------------------------------------------

# A fixture keypair stands in for the production key inside a sandbox
# repository the script resolves as its own ROOT — the production files
# are never overwritten and the user's keyring is never touched.
fixture_home="$sandbox/gnupg"
mkdir -m 700 "$fixture_home"
gpg --homedir "$fixture_home" --batch --passphrase '' --pinentry-mode loopback \
	--quick-generate-key 'relate-repo-signing-test-fixture' rsa2048 sign never \
	>/dev/null 2>&1
tap_check "the fixture keypair generates" $?
fixture_fingerprint=$(gpg --homedir "$fixture_home" --batch \
	--with-colons --list-secret-keys 2>/dev/null \
	| grep '^fpr:' | head -n 1 | cut -d: -f10)

sandbox_repo="$sandbox/script-repo"
mkdir -p "$sandbox_repo/scripts" "$sandbox_repo/distribution"
cp "$SCRIPT" "$sandbox_repo/scripts/"
gpg --homedir "$fixture_home" --armor --export "$fixture_fingerprint" \
	> "$sandbox_repo/distribution/repo-signing-key.asc"
printf '%s\n' "$fixture_fingerprint" \
	> "$sandbox_repo/distribution/repo-signing-key.fingerprint"
fixture_secret=$(gpg --homedir "$fixture_home" --armor \
	--export-secret-keys "$fixture_fingerprint" 2>/dev/null | base64 | tr -d '\n')

# Happy path: the script signs with the secret, matches it against the
# sandbox-committed fingerprint, and its signature verifies against the
# sandbox-committed public key.
cp "$FIXTURE_RELEASE" "$sandbox/unsigned-Release"
REPO_GPG_KEY="$fixture_secret" "$sandbox_repo/scripts/sign-repo-metadata.sh" \
	"$sandbox/unsigned-Release" "$sandbox/Release.gpg" > "$sandbox/sign.log" 2>&1
tap_check "sign-repo-metadata.sh signs and self-verifies with a matching key" $?
head -n 1 "$sandbox/Release.gpg" | grep -q '^-----BEGIN PGP SIGNATURE-----$'
tap_check "the signature is an armored detached signature" $?

verify_home="$sandbox/verify-home"
mkdir -m 700 "$verify_home"
gpg --homedir "$verify_home" --batch \
	--import "$sandbox_repo/distribution/repo-signing-key.asc" 2>/dev/null
gpg --homedir "$verify_home" --batch --verify \
	"$sandbox/Release.gpg" "$sandbox/unsigned-Release" >/dev/null 2>&1
tap_check "the script's signature verifies against the committed public key" $?

# The wrong-secret trust boundary: a secret key that is not the committed
# key must abort before signing anything.
cp "$FIXTURE_RELEASE" "$sandbox/unsigned-2-Release"
REPO_GPG_KEY="$fixture_secret" "$SCRIPT" \
	"$sandbox/unsigned-2-Release" "$sandbox/wrong.gpg" > "$sandbox/wrong.log" 2>&1
if [ $? -ne 0 ] && grep -q 'disagree' "$sandbox/wrong.log"; then
	tap_ok "a secret key that is not the committed key aborts loudly"
else
	tap_not_ok "a secret key that is not the committed key aborts loudly"
fi
[ ! -e "$sandbox/wrong.gpg" ]
tap_check "the aborted run wrote no signature" $?

# The empty-secret trust boundary (the v1.2.0 maven failure class,
# ADR-0630): a missing secret must fail naming it, not import nothing.
REPO_GPG_KEY= "$SCRIPT" "$sandbox/unsigned-Release" "$sandbox/empty.gpg" \
	> "$sandbox/empty.log" 2>&1
if [ $? -ne 0 ] && grep -q 'REPO_GPG_KEY' "$sandbox/empty.log"; then
	tap_ok "an empty REPO_GPG_KEY fails naming the secret"
else
	tap_not_ok "an empty REPO_GPG_KEY fails naming the secret"
fi

# Usage contract.
"$SCRIPT" > "$sandbox/usage.log" 2>&1
if [ $? -ne 0 ] && grep -q 'usage:' "$sandbox/usage.log"; then
	tap_ok "missing arguments print usage and exit non-zero"
else
	tap_not_ok "missing arguments print usage and exit non-zero"
fi

tap_done
