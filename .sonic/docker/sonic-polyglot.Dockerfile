# Rust image plus the language toolchains used by the polyglot CI lanes.
FROM localhost/relate-sonic-rust:1.88

ENV DEBIAN_FRONTEND=noninteractive \
    PATH=/usr/local/go/bin:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    PIP_BREAK_SYSTEM_PACKAGES=1 \
    UV_BREAK_SYSTEM_PACKAGES=1

# python3.12 is the interpreter. cargo test -p relate-python links
# libpython (PyO3 without extension-module), which needs the unversioned
# libpython3.12.so from python3.12-dev.
RUN apt-get update && apt-get install -y --no-install-recommends \
        maven \
        openjdk-21-jdk-headless \
        openssl \
        python-is-python3 \
        python3-pip \
        python3.12 \
        python3.12-dev \
    && rm -rf /var/lib/apt/lists/*

ARG NODE_VERSION=22.20.0
ARG GO_VERSION=1.22.12

RUN arch="$(uname -m)" \
    && case "$arch" in \
        x86_64) node_arch=x64; go_arch=amd64 ;; \
        aarch64|arm64) node_arch=arm64; go_arch=arm64 ;; \
        *) echo "unsupported architecture: $arch" >&2; exit 1 ;; \
    esac \
    && curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-${node_arch}.tar.xz" \
        | tar -xJ -C /usr/local --strip-components=1 \
    && curl -fsSL "https://go.dev/dl/go${GO_VERSION}.linux-${go_arch}.tar.gz" \
        | tar -xz -C /usr/local \
    && curl -LsSf https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=/usr/local/bin sh \
    && rustup target add wasm32-unknown-unknown \
    && node --version \
    && go version \
    && uv --version \
    && java -version \
    && mvn -version

WORKDIR /workspace
