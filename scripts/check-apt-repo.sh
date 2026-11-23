#!/usr/bin/env bash
# Install from a built APT repository in a clean ubuntu:24.04 container and
# prove the channel contract (ADR-0659): apt verifies the repository
# metadata against the committed public key (no
# --allow-unauthenticated), `apt-get install relate` installs the release
# binary, its `--version` prints the release version, and the package
# declares the glibc floor and carries the provenance marker.
#
# usage: check-apt-repo.sh <repo-dir> <glibc-floor>
#   <repo-dir>     the tree scripts/build-apt-repo.sh wrote
#   <glibc-floor>  the floor expected in Depends (e.g. GLIBC_2.35)
#
# Needs a Docker daemon; the container is pinned to linux/amd64 (the
# package's architecture) and the repository is mounted read-only, so no
# network and no host package state are involved.

set -euo pipefail

usage() {
	echo "usage: check-apt-repo.sh <repo-dir> <glibc-floor>" >&2
	exit 2
}

[ "$#" -eq 2 ] || usage
repo=$1
glibc_floor=$2

if [ ! -f "$repo/dists/stable/Release.gpg" ]; then
	echo "check-apt-repo: $repo: no signed repository (dists/stable/Release.gpg)" >&2
	exit 1
fi
if ! printf '%s' "$glibc_floor" | grep -Eq '^GLIBC_[0-9]+\.[0-9]+$'; then
	echo "check-apt-repo: glibc floor '$glibc_floor' is not GLIBC_MAJOR.MINOR" >&2
	exit 1
fi
libc_floor=${glibc_floor#GLIBC_}
if ! command -v docker >/dev/null 2>&1; then
	echo "check-apt-repo: docker not found" >&2
	exit 1
fi

repo=$(CDPATH= cd -- "$repo" && pwd)

# The container script: a clean ubuntu:24.04 with the default apt sources
# removed, so only the built repository is in play. The keyless attempt
# first is the negative control — an unsigned repository must be refused;
# the keyed attempt is the real install path the documented command uses.
container_script=$(cat <<'CONTAINER'
set -u
fail() { echo "FAIL: $*" >&2; exit 1; }

rm -f /etc/apt/sources.list
rm -rf /etc/apt/sources.list.d/*
mkdir -p /etc/apt/keyrings

# Negative control: the repository without its key must not verify.
echo "deb [trusted=no] file:///repo stable main" > /etc/apt/sources.list.d/relate.list
if apt-get update >/tmp/unsigned.log 2>&1; then
	fail "apt-get update accepted the repository with no key"
fi
grep -Eq 'NO_PUBKEY|is not signed' /tmp/unsigned.log \
	|| fail "the keyless apt-get update failed for some other reason: $(tail -n 3 /tmp/unsigned.log)"

# The documented install: the published key is the only trust anchor.
cp /repo/repo-signing-key.asc /etc/apt/keyrings/relate.asc
echo "deb [signed-by=/etc/apt/keyrings/relate.asc] file:///repo stable main" \
	> /etc/apt/sources.list.d/relate.list
apt-get update >/tmp/signed.log 2>&1 || fail "apt-get update failed against the signed repository: $(tail -n 3 /tmp/signed.log)"
grep -Eq 'NO_PUBKEY|is not signed|Release file.*is not valid' /tmp/signed.log \
	&& fail "the signed repository raised a verification error: $(grep -E 'NO_PUBKEY|is not signed|is not valid' /tmp/signed.log)"

apt-get install -y relate >/tmp/install.log 2>&1 || fail "apt-get install relate failed: $(tail -n 3 /tmp/install.log)"

# The release version is the package's own version; the binary must agree.
installed=$(dpkg-query -W -f='${Version}' relate)
reported=$(relate --version | sed -n '1s/^Relate v//p')
[ "$reported" = "$installed" ] \
	|| fail "relate --version reports '$reported', the package is '$installed'"

# The glibc floor and the provenance marker beside the real binary.
depends=$(dpkg-query -W -f='${Depends}' relate)
printf '%s' "$depends" | grep -qF "libc6 (>= FLOOR)" \
	|| fail "Depends is '$depends', not libc6 (>= FLOOR)"
marker=/usr/lib/relate/.relate-package-managed
[ -f "$marker" ] || fail "no provenance marker at $marker"
grep -q '"manager": "deb"' "$marker" || fail "marker does not name the deb manager: $(cat "$marker")"
grep -q '"upgrade": "sudo apt-get update && sudo apt-get install --only-upgrade relate"' "$marker" \
	|| fail "marker does not carry the upgrade command: $(cat "$marker")"
[ -L /usr/bin/relate ] || fail "/usr/bin/relate is not the package's symlink"

echo "PASS: relate $installed installed from the signed repository, --version agrees"
echo "PASS: Depends: $depends"
echo "PASS: marker $marker: $(cat "$marker")"
CONTAINER
)
container_script=${container_script//FLOOR/$libc_floor}

docker run --rm --platform linux/amd64 -v "$repo:/repo:ro" ubuntu:24.04 \
	sh -c "$container_script"
