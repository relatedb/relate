#!/bin/sh
# Sign one APT/RPM repository metadata file (an apt `Release` file, an rpm
# `repomd.xml`) with the repository signing key. The key's public half is
# committed as distribution/repo-signing-key.asc with its 40-hex
# fingerprint beside it; the private half never lives in the repository —
# CI reads it base64-encoded from the REPO_GPG_KEY Actions secret
# (ADR-0630) and this script imports it into a throwaway GNUPGHOME that is
# removed on exit, failure included.
#
# usage: sign-repo-metadata.sh <metadata-file> <signature-file>
# env:   REPO_GPG_KEY — the base64-encoded armored GPG secret key
#
# Fails closed at every step: an empty secret, a secret that does not
# decode as an OpenPGP key, a secret whose key is not the one the
# committed public key records, and a signature that does not verify
# against the committed public key all abort before anything is published.

set -u

usage() {
	echo "usage: sign-repo-metadata.sh <metadata-file> <signature-file>" >&2
	exit 2
}

[ "$#" -eq 2 ] || usage
metadata=$1
signature=$2

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
public_key="$ROOT/distribution/repo-signing-key.asc"
fingerprint_file="$ROOT/distribution/repo-signing-key.fingerprint"

if [ -z "${REPO_GPG_KEY:-}" ]; then
	echo "::error::REPO_GPG_KEY is empty — store the base64-encoded repository signing secret key as the REPO_GPG_KEY repository secret (RELEASING.md, \"Repository signing key\")" >&2
	exit 1
fi

for required in "$metadata" "$public_key" "$fingerprint_file"; do
	if [ ! -f "$required" ]; then
		echo "::error::missing file: $required" >&2
		exit 1
	fi
done

expected_fingerprint=$(tr -d ' \t\r\n' < "$fingerprint_file")

# Two throwaway homes: the secret signs from one, and the freshly made
# signature must verify against the committed public key in the other —
# the same keyring a user's apt/dnf holds, so a secret/public mismatch
# cannot sign anything that consumers would accept.
signing_home=$(mktemp -d "${TMPDIR:-/tmp}/relate-repo-sign.XXXXXX")
verify_home=$(mktemp -d "${TMPDIR:-/tmp}/relate-repo-verify.XXXXXX")
cleanup() {
	rm -rf "$signing_home" "$verify_home"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
chmod 700 "$signing_home" "$verify_home"

if ! printf '%s' "$REPO_GPG_KEY" | base64 --decode \
	| gpg --homedir "$signing_home" --batch --import 2>/dev/null; then
	echo "::error::REPO_GPG_KEY does not decode and import as an OpenPGP key" >&2
	exit 1
fi

imported_fingerprint=$(gpg --homedir "$signing_home" --batch \
	--with-colons --list-secret-keys 2>/dev/null \
	| grep '^fpr:' | head -n 1 | cut -d: -f10)
if [ -z "$imported_fingerprint" ]; then
	echo "::error::the imported secret key reports no fingerprint" >&2
	exit 1
fi
if [ "$imported_fingerprint" != "$expected_fingerprint" ]; then
	echo "::error::the REPO_GPG_KEY secret key ($imported_fingerprint) is not the key the committed public key records ($expected_fingerprint) — the secret and distribution/repo-signing-key.asc disagree" >&2
	exit 1
fi

if ! gpg --homedir "$signing_home" --batch --yes --pinentry-mode loopback \
	--passphrase '' --local-user "$expected_fingerprint" \
	--detach-sign --armor --output "$signature" "$metadata" 2>/dev/null; then
	echo "::error::signing $metadata failed" >&2
	exit 1
fi

if ! gpg --homedir "$verify_home" --batch --import "$public_key" 2>/dev/null; then
	echo "::error::the committed public key $public_key does not import" >&2
	exit 1
fi
if ! gpg --homedir "$verify_home" --batch --verify "$signature" "$metadata" >/dev/null 2>&1; then
	echo "::error::the new signature does not verify against the committed public key $public_key" >&2
	exit 1
fi

echo "signed $metadata -> $signature (key $expected_fingerprint)"
