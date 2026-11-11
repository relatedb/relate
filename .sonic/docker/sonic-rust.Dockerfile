# Local CI image for the Rust lanes. Built by the Sonic before action; not published.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=mold" \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=mold"

RUN echo 'Acquire::Retries "5";' > /etc/apt/apt.conf.d/80retries \
    && sed -i -E 's#https?://(archive|security)\.ubuntu\.com/ubuntu#http://archive.ubuntu.com/ubuntu#g' /etc/apt/sources.list.d/ubuntu.sources
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        cmake \
        curl \
        gcc \
        git \
        make \
        mold \
        pkg-config \
        python3 \
        python3-yaml \
        xz-utils \
    && rm -rf /var/lib/apt/lists/* \
    && git config --system --add safe.directory /workspace

# Rust 1.88.0 matches rust-toolchain.toml. nextest is the CI test runner.
# The toolchain is installed outside /root so stages running as the host
# uid can execute it. /cargo-home and /cargo-target are the cache mounts;
# mode 1777 lets that uid write a volume Docker initialized as root.
# get.nexte.st redirects to an anonymous GitHub release. The tarball is
# fetched through scripts/github-download.sh (BuildKit secret when the
# host has GITHUB_TOKEN; anonymous only outside GitHub Actions).
ARG GITHUB_ACTIONS=
RUN --mount=type=bind,from=scripts,source=github-download.sh,target=/tmp/github-download.sh \
    --mount=type=secret,id=GITHUB_TOKEN,required=false \
    arch="$(uname -m)" \
    && case "$arch" in \
        x86_64) nexte_triple=x86_64-unknown-linux-gnu ;; \
        aarch64|arm64) nexte_triple=aarch64-unknown-linux-gnu ;; \
        *) echo "unsupported architecture: $arch" >&2; exit 1 ;; \
    esac \
    && if [ -s /run/secrets/GITHUB_TOKEN ]; then export GITHUB_TOKEN="$(cat /run/secrets/GITHUB_TOKEN)"; fi \
    && export GITHUB_ACTIONS="${GITHUB_ACTIONS}" \
    && curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain 1.88.0 --profile minimal \
            --component rustfmt,clippy \
    && sh /tmp/github-download.sh \
        --repo nextest-rs/nextest \
        --tag cargo-nextest-0.9.87 \
        --asset "cargo-nextest-0.9.87-${nexte_triple}.tar.gz" \
        -o /tmp/nextest.tar.gz \
    && sh /tmp/github-download.sh \
        --repo nextest-rs/nextest \
        --tag cargo-nextest-0.9.87 \
        --asset "cargo-nextest-0.9.87-${nexte_triple}.sha256" \
        -o /tmp/nextest.sha256 \
    && expected="$(awk '{print $1; exit}' /tmp/nextest.sha256)" \
    && actual="$(sha256sum /tmp/nextest.tar.gz | awk '{print $1}')" \
    && [ -n "$expected" ] && [ "$expected" = "$actual" ] \
    && mkdir -p /tmp/nextest-unpack \
    && tar -xzf /tmp/nextest.tar.gz -C /tmp/nextest-unpack \
    && install -m 0755 "$(find /tmp/nextest-unpack -type f -name cargo-nextest | head -n 1)" /usr/local/cargo/bin/cargo-nextest \
    && rm -rf /tmp/nextest.tar.gz /tmp/nextest.sha256 /tmp/nextest-unpack \
    && chmod -R a+rX /usr/local/rustup /usr/local/cargo \
    && mkdir -p /cargo-home /cargo-target \
    && chmod 1777 /cargo-home /cargo-target \
    && rustc --version \
    && cargo nextest --version

# The baseline stages drive the host daemon. The CLI and buildx live here
# so those stages do not also receive GITHUB_TOKEN. buildx is a GitHub
# release; the token is a BuildKit secret, the same way nextest is fetched.
RUN --mount=type=bind,from=scripts,source=install-docker-cli.sh,target=/tmp/install-docker-cli.sh \
    --mount=type=bind,from=scripts,source=github-download.sh,target=/tmp/github-download.sh \
    --mount=type=secret,id=GITHUB_TOKEN,required=false \
    if [ -s /run/secrets/GITHUB_TOKEN ]; then export GITHUB_TOKEN="$(cat /run/secrets/GITHUB_TOKEN)"; fi \
    && export GITHUB_ACTIONS="${GITHUB_ACTIONS}" \
    && bash /tmp/install-docker-cli.sh \
    && rm -rf /tmp/docker.tgz /tmp/docker /tmp/docker-buildx \
    && chmod 0755 /usr/local/bin/docker /usr/local/lib/docker/cli-plugins/docker-buildx \
    && docker --version \
    && docker buildx version

# ADR-0255: one-shot CI builds. Incremental debug artifacts are huge and
# unused across container runs; line tables keep backtraces useful.
ENV CARGO_INCREMENTAL=0 \
    CARGO_PROFILE_DEV_DEBUG=line-tables-only

WORKDIR /workspace
