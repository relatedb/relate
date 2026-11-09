#!/usr/bin/env bash
# Install the static Docker CLI into the current container (the gosonic
# stages that must drive the HOST daemon through its socket: the runner
# image has no docker binary). One canonical copy — the stages that need
# it (loadtest-baselines, query-latency, memory-controls) all call this
# script. The baseline image build sets DOCKER_BUILDKIT=1 and uses cache
# mounts, so the Buildx plugin is installed beside the CLI.
set -euo pipefail

# The pinned digests (27.5.1): an unsigned tarball from a CDN is
# verified, never trusted. Bump procedure: set the version in the URL,
# then for each arch
#   curl -fsSL -o docker.tgz https://download.docker.com/linux/static/stable/<arch>/docker-<ver>.tgz
#   shasum -a 256 docker.tgz
# and update the digest beside it below (Docker publishes no .sha256
# sidecar, so the digests are computed by hand at bump time).
case "$(uname -m)" in
  x86_64)
    docker_arch=x86_64
    pinned=4f798b3ee1e0140eab5bf30b0edc4e84f4cdb53255a429dc3bbae9524845d640
    buildx_pinned=32ed111e941e385c2fb8261eba06a4056915718fd606f8278834ac1931d261a2
    ;;
  aarch64|arm64)
    docker_arch=aarch64
    pinned=e6b53725a73763ab3f988c73f8772eaed429754c1a579db5ff11f21990fd1817
    buildx_pinned=138b587399b27bb61945a36d67866177b85dea1155101a2be63c7ab715f18a2e
    ;;
  *) echo "install-docker-cli: unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac
curl -fsSL -o /tmp/docker.tgz "https://download.docker.com/linux/static/stable/${docker_arch}/docker-27.5.1.tgz"
echo "${pinned}  /tmp/docker.tgz" | sha256sum --check --quiet
tar -xzf /tmp/docker.tgz -C /tmp
mv /tmp/docker/docker /usr/local/bin/docker

buildx_arch="$(uname -m)"
case "$buildx_arch" in
  x86_64) buildx_name=amd64 ;;
  *) buildx_name=arm64 ;;
esac
# The CLI tarball above is download.docker.com. buildx is a GitHub
# release; github-download.sh sends it through the API when a token is set.
here="$(cd "$(dirname "$0")" && pwd)"
sh "$here/github-download.sh" \
  --repo docker/buildx \
  --tag v0.19.3 \
  --asset "buildx-v0.19.3.linux-${buildx_name}" \
  -o /tmp/docker-buildx
echo "${buildx_pinned}  /tmp/docker-buildx" | sha256sum --check --quiet
mkdir -p /usr/local/lib/docker/cli-plugins
mv /tmp/docker-buildx /usr/local/lib/docker/cli-plugins/docker-buildx
chmod 0755 /usr/local/lib/docker/cli-plugins/docker-buildx
