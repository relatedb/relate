#!/bin/sh
# Hermetic suite for the Debian/Ubuntu channel builder
# (scripts/build-apt-repo.sh): the .deb's control declares the glibc floor
# and packages the binary where the provenance marker belongs, the
# Packages/Release indices carry the right digests, and the Release is
# signed with the repository key the tree publishes (ADR-0659).
#
# The signing key's private half is a CI secret, so the suite copies the
# script into a sandbox repository whose committed key files hold a
# freshly generated fixture keypair — the same code path the release runs,
# never the production key.
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)

. "$ROOT/scripts/tests/lib/tap.sh"
. "$ROOT/scripts/lib/common.sh"

# dpkg-deb builds the package and gpg signs the Release; a host without
# either skips the whole suite under its own name.
missing=''
command -v dpkg-deb >/dev/null 2>&1 || missing='dpkg-deb'
command -v gpg >/dev/null 2>&1 || missing="${missing:+$missing, }gpg"
if [ -n "$missing" ]; then
	tap_skip "APT repository builder suite" "$missing not available"
	tap_done
	exit 0
fi

tap_sandbox build-apt-repo

repo="$sandbox/repo"
mkdir -p "$repo/scripts/lib" "$repo/distribution"
cp "$ROOT/scripts/build-apt-repo.sh" "$repo/scripts/"
cp "$ROOT/scripts/sign-repo-metadata.sh" "$repo/scripts/"
cp "$ROOT/scripts/lib/common.sh" "$repo/scripts/lib/"

# The fixture keypair stands in for the production repository key.
fixture_home="$sandbox/gnupg"
mkdir -m 700 "$fixture_home"
gpg --homedir "$fixture_home" --batch --passphrase '' --pinentry-mode loopback \
	--quick-generate-key 'relate-apt-repo-test-fixture' rsa2048 sign never \
	>/dev/null 2>&1
tap_check "the fixture keypair generates" $?
fixture_fingerprint=$(gpg --homedir "$fixture_home" --batch \
	--with-colons --list-secret-keys 2>/dev/null \
	| grep '^fpr:' | head -n 1 | cut -d: -f10)
gpg --homedir "$fixture_home" --armor --export "$fixture_fingerprint" \
	>"$repo/distribution/repo-signing-key.asc"
printf '%s\n' "$fixture_fingerprint" \
	>"$repo/distribution/repo-signing-key.fingerprint"
fixture_secret=$(gpg --homedir "$fixture_home" --armor \
	--export-secret-keys "$fixture_fingerprint" 2>/dev/null | base64 | tr -d '\n')

# Stand-ins for the release binaries: the builder packages bytes, it never
# runs them, so a script that prints the release version is enough.
for asset in relate-linux-amd64 relate-linux-arm64; do
	printf '#!/bin/sh\necho "Relate v9.9.9"\n' >"$sandbox/$asset"
	chmod 755 "$sandbox/$asset"
done

out="$sandbox/tree"
REPO_GPG_KEY="$fixture_secret" "$repo/scripts/build-apt-repo.sh" \
	"$sandbox/relate-linux-amd64" amd64 9.9.9 GLIBC_2.35 "$out" >"$sandbox/build.log" 2>&1
tap_check "build-apt-repo.sh builds the package and repository tree" $?

deb="$out/pool/main/r/relate/relate_9.9.9_amd64.deb"
tap_run_success "the .deb is written to the pool path the index names" test -f "$deb"

# --- the package's own contract ----------------------------------------

control_package=$(dpkg-deb -f "$deb" Package)
assert_same "Package is relate" "$control_package" relate
assert_same "Version is the release version" \
	"$(dpkg-deb -f "$deb" Version)" 9.9.9
assert_same "Architecture is amd64" \
	"$(dpkg-deb -f "$deb" Architecture)" amd64
assert_same "Depends declares the ISA gate's glibc floor" \
	"$(dpkg-deb -f "$deb" Depends)" 'libc6 (>= 2.35)'

dpkg-deb -x "$deb" "$sandbox/x"
tap_run_success "the real binary lands in the package-owned directory" \
	test -x "$sandbox/x/usr/lib/relate/relate"
assert_same "the real binary is the staged release binary" \
	"$(cat "$sandbox/x/usr/lib/relate/relate")" \
	"$(cat "$sandbox/relate-linux-amd64")"
assert_same "PATH exposes the real binary by symlink" \
	"$(readlink "$sandbox/x/usr/bin/relate")" ../lib/relate/relate
assert_same "the provenance marker sits beside the real binary" \
	"$(cat "$sandbox/x/usr/lib/relate/.relate-package-managed")" \
	'{"manager": "deb", "upgrade": "sudo apt-get update && sudo apt-get install --only-upgrade relate"}'

# --- the repository index and its signature ----------------------------

packages="$out/dists/stable/main/binary-amd64/Packages"
grep -q '^Package: relate$' "$packages" &&
	grep -q '^Version: 9.9.9$' "$packages" &&
	grep -q '^Architecture: amd64$' "$packages" &&
	grep -q '^Filename: pool/main/r/relate/relate_9.9.9_amd64.deb$' "$packages"
tap_check "Packages carries the control fields and the pool location" $?
assert_same "Packages records the .deb's sha256" \
	"$(sed -n 's/^SHA256: //p' "$packages")" "$(sha256_of "$deb")"

gzip -dc "$packages.gz" >"$sandbox/decompressed" &&
	cmp -s "$packages" "$sandbox/decompressed"
tap_check "Packages.gz is the same index compressed" $?

release="$out/dists/stable/Release"
for index in "$packages" "$packages.gz"; do
	relative="main/binary-amd64/$(basename "$index")"
	assert_same "Release records $relative's sha256" \
		"$(awk -v f="$relative" '$3 == f { print $1 }' "$release")" \
		"$(sha256_of "$index")"
	assert_same "Release records $relative's size" \
		"$(awk -v f="$relative" '$3 == f { print $2 }' "$release")" \
		"$(wc -c <"$index" | tr -d ' ')"
done
grep -q '^Architectures: amd64$' "$release" && grep -q '^Components: main$' "$release" &&
	grep -q '^Date: ' "$release"
tap_check "Release names the architecture, component and date apt needs" $?

keyring="$sandbox/keyring"
mkdir -m 700 "$keyring"
gpg --homedir "$keyring" --batch --import "$out/repo-signing-key.asc" 2>/dev/null
gpg --homedir "$keyring" --batch --verify "$release.gpg" "$release" >/dev/null 2>&1
tap_check "Release.gpg verifies against the public key the tree publishes" $?
assert_same "the tree publishes the same key distribution/ commits" \
	"$(cat "$out/repo-signing-key.asc")" \
	"$(cat "$repo/distribution/repo-signing-key.asc")"

# --- the arm64 tree derives every arch-bearing path from the argument ---

out_arm64="$sandbox/tree-arm64"
REPO_GPG_KEY="$fixture_secret" "$repo/scripts/build-apt-repo.sh" \
	"$sandbox/relate-linux-arm64" arm64 9.9.9 GLIBC_2.35 "$out_arm64" >"$sandbox/build-arm64.log" 2>&1
tap_check "build-apt-repo.sh builds the arm64 package and repository tree" $?

deb_arm64="$out_arm64/pool/main/r/relate/relate_9.9.9_arm64.deb"
tap_run_success "the arm64 .deb is written to the pool path the index names" test -f "$deb_arm64"
assert_same "the arm64 package declares the architecture it was given" \
	"$(dpkg-deb -f "$deb_arm64" Architecture)" arm64
packages_arm64="$out_arm64/dists/stable/main/binary-arm64/Packages"
grep -q '^Architecture: arm64$' "$packages_arm64" &&
	grep -q '^Filename: pool/main/r/relate/relate_9.9.9_arm64.deb$' "$packages_arm64"
tap_check "the arm64 index sits under binary-arm64 and names its own pool file" $?
grep -q '^Architectures: arm64$' "$out_arm64/dists/stable/Release"
tap_check "the arm64 Release names the architecture it was given" $?

# --- the refusals ------------------------------------------------------

refuses() { # $1 = description, rest = the command that must fail with its text
	tap_desc=$1
	shift
	if "$@" >"$sandbox/refusal.log" 2>&1; then
		tap_not_ok "$tap_desc"
	else
		tap_ok "$tap_desc"
	fi
}

refuses "a missing binary is refused" \
	env REPO_GPG_KEY="$fixture_secret" "$repo/scripts/build-apt-repo.sh" \
	"$sandbox/absent" amd64 9.9.9 GLIBC_2.35 "$sandbox/t1"
refuses "an architecture that is not a Debian name is refused" \
	env REPO_GPG_KEY="$fixture_secret" "$repo/scripts/build-apt-repo.sh" \
	"$sandbox/relate-linux-amd64" amd64/ 9.9.9 GLIBC_2.35 "$sandbox/t0"
refuses "a non-MAJOR.MINOR.PATCH version is refused" \
	env REPO_GPG_KEY="$fixture_secret" "$repo/scripts/build-apt-repo.sh" \
	"$sandbox/relate-linux-amd64" amd64 9.9 GLIBC_2.35 "$sandbox/t2"
refuses "a glibc floor that is not GLIBC_MAJOR.MINOR is refused" \
	env REPO_GPG_KEY="$fixture_secret" "$repo/scripts/build-apt-repo.sh" \
	"$sandbox/relate-linux-amd64" amd64 9.9.9 2.35 "$sandbox/t3"
refuses "a non-empty output directory is refused" \
	env REPO_GPG_KEY="$fixture_secret" "$repo/scripts/build-apt-repo.sh" \
	"$sandbox/relate-linux-amd64" amd64 9.9.9 GLIBC_2.35 "$out"
refuses "an empty REPO_GPG_KEY is refused" \
	env REPO_GPG_KEY="" "$repo/scripts/build-apt-repo.sh" \
	"$sandbox/relate-linux-amd64" amd64 9.9.9 GLIBC_2.35 "$sandbox/t4"
grep -q 'REPO_GPG_KEY' "$sandbox/refusal.log"
tap_check "the empty-secret refusal names the secret" $?
[ ! -e "$sandbox/t4/dists/stable/Release.gpg" ]
tap_check "a refused run publishes no signature" $?

tap_done
