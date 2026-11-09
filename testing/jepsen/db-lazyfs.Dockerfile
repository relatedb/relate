# The DB node for the lazyfs power-off lane (LABS-654). The stock product
# image is scratch — no shell, no FUSE — so this image wraps the SAME
# product binaries (copied out of the locally built relate-bolt image,
# the one `make docker` produces) in an ubuntu:24.04 userspace that can
# mount lazyfs (VLDB'24, dsrhaslab/lazyfs, C++17/fuse3) INSIDE the
# container:
#
#   /mnt/lazyfs   the mount serve sees — power-volatile: un-fsynced bytes
#                 live only in lazyfs's userspace memory
#   /data/root    lazyfs's backing root, on the compose /data volume —
#                 the "disk": only fsynced bytes ever reach it
#   /tmp/faults.fifo  lazyfs's fault FIFO (config/default.toml) — the
#                 clear-cache channel the nemesis writes over docker exec
#
# A faithful power-off of the whole "machine" = the clear-cache fault +
# docker kill (SIGKILL of the container kills serve AND lazyfs together);
# docker start re-runs lazyfs-entrypoint, which remounts lazyfs on the
# surviving backing root and execs the stock seed-then-serve supervisor.
# The binary under test is exactly what ships to users — only the disk
# under it is now honest about power loss.
#
# Build (run-poweroff.sh does this): from THIS directory,
#   docker build -f db-lazyfs.Dockerfile -t relate-jepsen-db-lazyfs:latest .
# Prerequisite: `make docker` at the repo root — the product stage below
# reads the local relate-bolt:latest; rebuild this image whenever the
# product image is rebuilt.

# --- lazyfs: clone at a PINNED commit and build (the recipe's ~90s build) ---
FROM ubuntu:24.04 AS lazyfs
# fuse3 + the C++17/CMake toolchain (make named explicitly — cmake's
# Unix Makefiles generator needs it and nothing else in the list pulls
# it); user_allow_other enabled exactly as the verified probe recipe
# does (lazyfs mounts with allow_other).
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        g++ make cmake libfuse3-dev libfuse3-3 fuse3 git ca-certificates \
    && sed -i 's/^#\s*user_allow_other/user_allow_other/' /etc/fuse.conf \
    && rm -rf /var/lib/apt/lists/*
# Pinned to the commit the local arm64 + upstream-CI probe verified
# (2026-11-01); fetch-by-SHA keeps the lane reproducible where a branch
# clone would float with upstream HEAD. The fetch is authenticated when
# the BuildKit secret is present (same rule as scripts/github-download.sh).
ARG LAZYFS_SHA=fa7d32ee9710d55a7e529ac3c375f6616563c78b
ARG GITHUB_ACTIONS=
RUN --mount=type=secret,id=GITHUB_TOKEN,required=false \
    git init /opt/lazyfs \
    && git -C /opt/lazyfs remote add origin https://github.com/dsrhaslab/lazyfs \
    && token="" \
    && if [ -s /run/secrets/GITHUB_TOKEN ]; then token="$(cat /run/secrets/GITHUB_TOKEN)"; fi \
    && if [ -z "$token" ] && [ "$GITHUB_ACTIONS" = "true" ]; then \
         echo "db-lazyfs: GITHUB_TOKEN is required in GitHub Actions so the lazyfs fetch is not anonymous" >&2; \
         exit 1; \
       fi \
    && if [ -n "$token" ]; then \
         git -C /opt/lazyfs -c "http.extraheader=AUTHORIZATION: bearer ${token}" fetch --depth 1 origin "${LAZYFS_SHA}"; \
       else \
         git -C /opt/lazyfs fetch --depth 1 origin "${LAZYFS_SHA}"; \
       fi \
    && git -C /opt/lazyfs checkout FETCH_HEAD \
    && cd /opt/lazyfs/libs/libpcache && ./build.sh \
    && cd /opt/lazyfs/lazyfs && ./build.sh

# --- product: the stock product image's binaries, unchanged ---
FROM relate-bolt:latest AS product

# --- runtime: ubuntu userspace (shell, fuse3, coreutils) + both payloads ---
FROM ubuntu:24.04
# fuse3 (fusermount3 + the mount helper), procps (pkill for the probe),
# coreutils' timeout for the nemesis's guarded FIFO write.
RUN apt-get update \
    && apt-get install -y --no-install-recommends fuse3 procps \
    && sed -i 's/^#\s*user_allow_other/user_allow_other/' /etc/fuse.conf \
    && rm -rf /var/lib/apt/lists/*
# The whole lazyfs tree: build/ binaries, scripts/mount-lazyfs.sh, and
# config/default.toml (whose fifo_path the entrypoint's FIFO default must
# match).
COPY --from=lazyfs /opt/lazyfs /opt/lazyfs
COPY --from=product /bin/relate /bin/entrypoint /bin/
COPY lazyfs/lazyfs-mount.sh lazyfs/lazyfs-entrypoint.sh lazyfs/lazyfs-probe.sh \
     /usr/local/bin/
ENTRYPOINT ["/usr/local/bin/lazyfs-entrypoint.sh"]
# Convenience default for a bare `docker run` for debugging; the compose
# override restates the full serve argv (compose `command` replaces this).
CMD ["--host", "0.0.0.0", "--database", "/mnt/lazyfs/graph", "--bolt", "7687"]
