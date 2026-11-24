#!/usr/bin/env bash
# Build the Debian/Ubuntu (APT) channel from a release's staged assets: the
# `relate` .deb and the signed APT repository tree that serves it, for one
# architecture per run (ADR-0659). The release workflow builds one tree per
# architecture, attaches each as its own tarball and relate-web serves them
# at https://relatedb.ai/apt.
#
# The package keeps the real binary in a package-owned directory and
# symlinks it onto PATH, so the provenance marker
# `.relate-package-managed` lands beside the real file where `relate
# upgrade` looks for it (ADR-0659, "Install provenance").
#
# usage: build-apt-repo.sh <binary> <arch> <version> <glibc-floor> <out-dir>
#   <binary>       the released linux CLI for <arch>
#                  (relate-linux-amd64, relate-linux-arm64)
#   <arch>         the Debian architecture the package declares (amd64,
#                  arm64) — the control's Architecture, the pool file name
#                  and the binary-<arch> index path all derive from it
#   <version>      the release version without the `v` (e.g. 1.2.0)
#   <glibc-floor>  the floor the ISA gate records (e.g. GLIBC_2.35) —
#                  declared as `libc6 (>= 2.35)` in Depends
#   <out-dir>      where the repository tree is written (created if absent;
#                  must be empty or absent so a stale index cannot ride)
# env:   REPO_GPG_KEY — the base64-encoded repository signing key
#        (scripts/sign-repo-metadata.sh signs dists/stable/Release with it)
#
# Fails closed: every argument and tool is checked before anything is
# written, and no publication step runs on a half-built tree.

set -euo pipefail

usage() {
	echo "usage: build-apt-repo.sh <binary> <arch> <version> <glibc-floor> <out-dir>" >&2
	exit 2
}

[ "$#" -eq 5 ] || usage
binary=$1
arch=$2
version=$3
glibc_floor=$4
out=$5

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$ROOT/scripts/lib/common.sh"

for tool in dpkg-deb gzip; do
	if ! command -v "$tool" >/dev/null 2>&1; then
		echo "build-apt-repo: $tool not found" >&2
		exit 1
	fi
done

if [ ! -f "$binary" ]; then
	echo "build-apt-repo: $binary: no such file" >&2
	exit 1
fi
# Refuse $1 unless it matches the extended regex $2; $3 names what it should be.
require_match() {
	if ! printf '%s' "$1" | grep -Eq "$2"; then
		echo "build-apt-repo: $3" >&2
		exit 1
	fi
}
require_match "$arch" '^[a-z0-9]+$' "architecture '$arch' is not a Debian architecture name"
require_match "$version" '^[0-9]+\.[0-9]+\.[0-9]+$' "version '$version' is not MAJOR.MINOR.PATCH"
require_match "$glibc_floor" '^GLIBC_[0-9]+\.[0-9]+$' "glibc floor '$glibc_floor' is not GLIBC_MAJOR.MINOR"
# A pre-existing tree would let yesterday's index ride today's Release.
if [ -d "$out" ] && [ -n "$(ls -A "$out")" ]; then
	echo "build-apt-repo: $out is not empty" >&2
	exit 1
fi

libc_floor=${glibc_floor#GLIBC_}

# The one upgrade command the package records in its provenance marker;
# `relate upgrade` prints it verbatim (ADR-0659, "Upgrade deferral").
upgrade_command='sudo apt-get update && sudo apt-get install --only-upgrade relate'

stage=$(mktemp -d "${TMPDIR:-/tmp}/relate-deb.XXXXXX")
trap 'rm -rf "$stage"' EXIT
trap 'exit 1' HUP INT TERM

pkgroot="$stage/relate"
mkdir -p "$pkgroot/DEBIAN" "$pkgroot/usr/lib/relate" "$pkgroot/usr/bin"
install -m 0755 "$binary" "$pkgroot/usr/lib/relate/relate"
ln -s ../lib/relate/relate "$pkgroot/usr/bin/relate"
printf '{"manager": "deb", "upgrade": "%s"}\n' "$upgrade_command" \
	>"$pkgroot/usr/lib/relate/.relate-package-managed"

cat >"$pkgroot/DEBIAN/control" <<CONTROL
Package: relate
Version: $version
Architecture: $arch
Maintainer: Relate maintainers <relate@relatedb.ai>
Section: database
Priority: optional
Depends: libc6 (>= $libc_floor)
Homepage: https://relatedb.ai/
Description: embedded openCypher graph database CLI
 Relate is an embedded, in-memory property graph engine with a command-line
 shell for interactive and scripted Cypher queries.
CONTROL

umask 022
deb="$out/pool/main/r/relate/relate_${version}_${arch}.deb"
mkdir -p "$(dirname "$deb")"
dpkg-deb --build --root-owner-group "$pkgroot" "$deb" >/dev/null

# The package index: the .deb's control fields plus the location and
# digest apt resolves the download from. Written by hand because it is
# exactly those fields — dpkg-scanpackages would add a dpkg-dev dependency
# to the release runner for nothing.
index_dir="$out/dists/stable/main/binary-$arch"
mkdir -p "$index_dir"
packages="$index_dir/Packages"
{
	dpkg-deb -f "$deb"
	printf 'Filename: pool/main/r/relate/%s\n' "$(basename "$deb")"
	printf 'Size: %s\n' "$(wc -c <"$deb" | tr -d ' ')"
	printf 'SHA256: %s\n' "$(sha256_of "$deb")"
	printf '\n'
} >"$packages"
gzip -9 -n -c "$packages" >"$packages.gz"

# The Release file lists its own indices' digests, and the repository
# signing key signs exactly these bytes; the key is published beside the
# tree so the documented install command can fetch it from the same root.
release="$out/dists/stable/Release"
{
	printf 'Origin: Relate\n'
	printf 'Label: Relate\n'
	printf 'Suite: stable\n'
	printf 'Codename: stable\n'
	printf 'Architectures: %s\n' "$arch"
	printf 'Components: main\n'
	printf 'Description: Relate APT repository\n'
	printf 'Date: %s\n' "$(LC_ALL=C date -u '+%a, %d %b %Y %H:%M:%S UTC')"
	printf 'SHA256:\n'
	for f in "$packages" "$packages.gz"; do
		printf ' %s %s %s\n' \
			"$(sha256_of "$f")" \
			"$(wc -c <"$f" | tr -d ' ')" \
			"main/binary-$arch/$(basename "$f")"
	done
} >"$release"

"$ROOT/scripts/sign-repo-metadata.sh" "$release" "$release.gpg"
cp "$ROOT/distribution/repo-signing-key.asc" "$out/repo-signing-key.asc"

echo "built $deb"
echo "wrote the signed APT repository tree at $out"
