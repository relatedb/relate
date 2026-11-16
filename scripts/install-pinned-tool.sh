#!/bin/sh
# Install one pinned tool from a GitHub release into /usr/local/bin.
#
# The Dockerfile chef stage bind-mounts this helper beside
# github-download.sh and calls it once per pinned build tool (sccache,
# cargo-auditable), so the download-verify-unpack flow — the
# supply-chain guarantee — has one home instead of one copy per tool.
# Each tool's RUN supplies the pin (repo/tag/asset/sha256) and the
# unpack shape (tar-mode, strip, binary).
#
# The asset is fetched through github-download.sh (the API when a
# token is present, never the anonymous releases/download GET while
# one is), the pinned digest is verified with `sha256sum -c` before
# anything is unpacked — the chef stage is Alpine, so sha256sum is the
# platform tool; the sha256sum-or-shasum policy for host-side
# installers lives in scripts/lib/common.sh, which a container stage
# cannot source — and only the named binary is extracted: the
# tarball's top directory also carries README and license files that
# must not land in /usr/local/bin.
#
# Usage (github-download.sh must sit beside this script — the bind
# mount does; a repo run finds it in scripts/):
#   install-pinned-tool.sh --repo OWNER/NAME --tag TAG --asset NAME \
#     --sha256 HEX --tar-mode gz|xz --strip N --binary NAME
#
# A BuildKit GITHUB_TOKEN secret at /run/secrets/GITHUB_TOKEN is
# exported when present; GITHUB_ACTIONS passes through the
# environment, so an Actions build without the secret fails loudly
# (through github-download.sh) instead of downloading anonymously.
set -eu

usage() {
  echo "usage: install-pinned-tool.sh --repo OWNER/NAME --tag TAG --asset NAME --sha256 HEX --tar-mode gz|xz --strip N --binary NAME" >&2
}

repo=""
tag=""
asset=""
sha256=""
mode=""
strip=""
binary=""
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) repo="${2:?}"; shift 2 ;;
    --tag) tag="${2:?}"; shift 2 ;;
    --asset) asset="${2:?}"; shift 2 ;;
    --sha256) sha256="${2:?}"; shift 2 ;;
    --tar-mode) mode="${2:?}"; shift 2 ;;
    --strip) strip="${2:?}"; shift 2 ;;
    --binary) binary="${2:?}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "install-pinned-tool: unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

for value in "$repo" "$tag" "$asset" "$sha256" "$mode" "$strip" "$binary"; do
  if [ -z "$value" ]; then
    echo "install-pinned-tool: --repo, --tag, --asset, --sha256, --tar-mode, --strip, and --binary are all required" >&2
    usage
    exit 2
  fi
done

case "$mode" in
  gz) tar_flag=z ;;
  xz) tar_flag=J ;;
  *) echo "install-pinned-tool: --tar-mode must be gz or xz (got $mode)" >&2; exit 2 ;;
esac

# The tarball's top directory is named after the asset; the binary
# sits inside it, so the member path is derived, not passed twice.
case "$asset" in
  *.tar.gz) stem="${asset%.tar.gz}" ;;
  *.tar.xz) stem="${asset%.tar.xz}" ;;
  *) echo "install-pinned-tool: --asset must be a .tar.gz or .tar.xz release tarball (got $asset)" >&2; exit 2 ;;
esac

if [ -s /run/secrets/GITHUB_TOKEN ]; then
  GITHUB_TOKEN="$(cat /run/secrets/GITHUB_TOKEN)"
  export GITHUB_TOKEN
fi

here="$(dirname "$0")"
archive="$(mktemp)"
trap 'rm -f "$archive"' EXIT

sh "$here/github-download.sh" \
  --repo "$repo" \
  --tag "$tag" \
  --asset "$asset" \
  -o "$archive"

echo "${sha256}  ${archive}" | sha256sum -c -

tar "-x${tar_flag}f" "$archive" --strip-components="$strip" -C /usr/local/bin "${stem}/${binary}"
