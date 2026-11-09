#!/usr/bin/env bash
# Move a Swatinem/rust-cache tree into the Sonic cargo volumes and back.
#
# rust-cache restores the default host paths (./target and $CARGO_HOME).
# Sonic compiles in docker volumes, one subdirectory per stage
# (/cargo-target/<subdir>, /cargo-home). Seeding before `gosonic ci run`
# and copying back afterwards lets the pre-Sonic shared keys (dev-v2,
# release-v2, corpus-v2, and the plain per-job keys) warm the container
# without putting CARGO_TARGET_DIR in the job env — that variable is part
# of the rust-cache env hash and would split the key.
#
# Two modes (probed, see `bind_works`):
#
#   bind  the cargo volumes ARE the host trees — a bind-backed volume whose
#         device is the workspace target/ and $CARGO_HOME — so the stage
#         writes straight through and no copy happens. The seed is a rename
#         (the restored flat entries move under the stage subdir). This is
#         the CI path: the free tier cannot hold the build plus its copy.
#   copy  the original fallback for file-sharing that drops container-created
#         children through a bind (older Docker Desktop): the volumes stage
#         the trees and `docker cp` moves them across.
#
# Host files that are not in the cargo tree are left in place. Loadtest
# reports are written to the workspace target/ during the stage; wiping
# that directory would drop them before the upload step.
#
# SONIC_CACHE_SYNC_BIND picks the mode: auto (the default — probe with
# bind_works), 1 (force bind, skipping the probe — what the test harness
# uses), or 0 (never bind). Any other value fails loudly (LABS-631).
set -euo pipefail

mode="${1:?usage: sonic-cache-sync.sh seed|save <subdir>}"
subdir="${2:?usage: sonic-cache-sync.sh seed|save <subdir>}"

if [[ "$mode" != "seed" && "$mode" != "save" ]]; then
  echo "sonic-cache-sync: mode must be seed or save, got ${mode}" >&2
  exit 1
fi
if [[ ! "$subdir" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "sonic-cache-sync: refusing subdir ${subdir}" >&2
  exit 1
fi

# cacheVolumeName in gosonic: "gosonic-cache-" plus the volume source.
# Tests override the prefix so they do not write the developer's volumes.
volume_prefix="${SONIC_CACHE_VOLUME_PREFIX:-gosonic-cache}"
target_volume="${volume_prefix}-cargo-target"
home_volume="${volume_prefix}-cargo-home"
image="${SONIC_CACHE_SYNC_IMAGE:-ubuntu:24.04}"
cargo_home="${CARGO_HOME:-${HOME}/.cargo}"
workspace="${GITHUB_WORKSPACE:-$(pwd)}"
host_target="${workspace}/target"

# The (volume, volume-subdir, host-path) sync pairs — one table; the
# decode and the direction live in one helper, so seed and save differ
# only in the function they hand it (LABS-631).
sync_pairs=(
  "$target_volume|$subdir|$host_target"
  "$home_volume|registry|${cargo_home}/registry"
  "$home_volume|git|${cargo_home}/git"
)
for_pairs() { # $1 = copy_in | copy_out
  local fn="$1" _pair _vol _rel _host
  for _pair in "${sync_pairs[@]}"; do
    IFS='|' read -r _vol _rel _host <<<"$_pair"
    "$fn" "$_vol" "$_rel" "$_host"
  done
}

if ! docker image inspect "$image" >/dev/null 2>&1; then
  docker pull "$image"
fi
# A bind-backed volume makes the cache directory BE the host tree: the stage
# writes straight into the workspace target/ (the tree rust-cache saves) and
# no copy happens — the free tier cannot hold the build *plus* its copy (run
# 39658799548: cleaned to 23310 MB free, then ENOSPC during the save's
# docker cp). Docker Desktop's older file-sharing dropped container-created
# children through a bind, so probe once and fall back to the plain volume +
# docker cp on the machines where it still bites.
bind_works() {
  local probe="${volume_prefix}-bind-probe-$$" dir ok=0
  dir="$(mktemp -d)"
  if docker volume create --opt type=none --opt device="$dir" --opt o=bind "$probe" >/dev/null 2>&1; then
    docker run --rm -v "${probe}:/probe" "$image" \
      sh -c 'echo probe > /probe/written-by-container' >/dev/null 2>&1 || true
    [[ -f "$dir/written-by-container" ]] && ok=1
  fi
  docker volume rm -f "$probe" >/dev/null 2>&1 || true
  rm -rf "$dir"
  [[ "$ok" == 1 ]]
}

# A leftover volume from the other mode would silently mis-mount; drop it.
recreate_on_mode_mismatch() {
  local vol="$1" want="$2" opts
  opts="$(docker volume inspect "$vol" --format '{{index .Options "o"}}' 2>/dev/null || echo none)"
  if { [[ "$want" == bind ]] && [[ "$opts" != bind ]]; } \
    || { [[ "$want" != bind ]] && [[ "$opts" == bind ]]; }; then
    docker volume rm -f "$vol" >/dev/null 2>&1 || true
  fi
}

# One case validates the vocabulary and computes the mode: 1 forces
# bind (no probe), auto probes, 0 never binds, anything else fails
# before any probe runs.
bind_pref="${SONIC_CACHE_SYNC_BIND:-auto}"
bind_mode=0
case "$bind_pref" in
  1) bind_mode=1 ;;
  auto) bind_works && bind_mode=1 ;;
  0) ;;
  *)
    printf 'sonic-cache-sync: SONIC_CACHE_SYNC_BIND must be auto, 1, or 0 (got %s)\n' "$bind_pref" >&2
    exit 2
    ;;
esac

# The volume roster — the target tree and the cargo home — is spelled
# once; the helper picks bind-backed or plain creation from the mode
# (LABS-631).
setup_volume() { # $1 = volume, $2 = the host tree it mirrors in bind mode
  if [[ "$bind_mode" == 1 ]]; then
    # The device must exist before the bind-backed volume references it.
    mkdir -p "$2"
    recreate_on_mode_mismatch "$1" bind
    docker volume create --opt type=none --opt device="$2" --opt o=bind "$1" >/dev/null
  else
    # Copy mode: the volume carries the cache from the seed invocation
    # to the save one (the stage writes into it in between), so never
    # recreate it.
    recreate_on_mode_mismatch "$1" volume
    docker volume create "$1" >/dev/null
  fi
}
setup_volume "$target_volume" "$host_target"
setup_volume "$home_volume" "$cargo_home"

# Host paths go through `docker cp`, not a bind mount. A bind of a fresh
# directory drops newly created children on Docker Desktop (the same failure
# that keeps Sonic target dirs on a volume instead of the workspace mount).
volume_has() {
  docker run --rm --network none -v "$1":/vol "$image" \
    bash -c "[[ -d \"/vol/$2\" ]]"
}

copy_in() {
  local volume="$1"
  local rel="$2"
  local host_path="$3"
  local cid found
  [[ -d "$host_path" ]] || return 0
  found="$(find "$host_path" -mindepth 1 -print -quit)"
  [[ -n "$found" ]] || return 0
  cid="$(docker create -v "${volume}:/vol" "$image")"
  docker cp "$host_path/." "$cid:/vol/$rel"
  docker rm "$cid" >/dev/null
  # The copy lands as root. Compile stages run as the host uid.
  docker run --rm --network none \
    -e HOST_UID="$(id -u)" \
    -e HOST_GID="$(id -g)" \
    -v "${volume}:/vol" \
    "$image" \
    bash -c 'chown -R "$HOST_UID:$HOST_GID" "/vol/'"$rel"'"'
}

# docker cp chmods every destination path. A stage that writes the workspace
# as root leaves a directory the runner cannot chmod, and the copy fails
# with "operation not permitted". Hand the tree back first, then copy.
chown_tree() {
  local host_path="$1"
  docker run --rm --network none \
    -e HOST_UID="$(id -u)" \
    -e HOST_GID="$(id -g)" \
    -v "${host_path}:/host" \
    "$image" \
    bash -c 'chown -R "$HOST_UID:$HOST_GID" /host'
}

copy_out() {
  local volume="$1"
  local rel="$2"
  local host_path="$3"
  local cid
  if ! volume_has "$volume" "$rel"; then
    return 0
  fi
  mkdir -p "$host_path"
  chown_tree "$host_path"
  cid="$(docker create -v "${volume}:/vol" "$image")"
  docker cp "$cid:/vol/$rel/." "$host_path/"
  docker rm "$cid" >/dev/null
  # docker cp keeps the container uid. rust-cache saves as the runner.
  chown_tree "$host_path"
}

if [[ "$mode" == "seed" ]]; then
  if [[ "$bind_mode" == 1 ]]; then
    # The volume IS the workspace target/ and $CARGO_HOME: move the restored
    # flat entries under the stage subdir the container's CARGO_TARGET_DIR
    # points at (a rename, no second copy), and let the build write through.
    # A restored cache can carry both <subdir>/<name> and a flat <name>: a
    # stage that writes the workspace target/ flat during the run (the
    # baseline-env docker build's target/release) lands in the archive beside
    # the seeded tree, and a plain mv dies ENOTEMPTY — failing every later
    # seed that restores the same key (main runs 39706389122, 39714076796,
    # 39716090621). Both copies ride one archive and the flat one is the
    # stage's latest write, so it wins.
    mkdir -p "${host_target}/${subdir}"
    for entry in "${host_target}"/*; do
      [[ -e "$entry" ]] || continue
      base="$(basename "$entry")"
      [[ "$base" == "$subdir" ]] && continue
      rm -rf "${host_target}/${subdir}/${base}"
      mv "$entry" "${host_target}/${subdir}/"
    done
    echo "sonic-cache-sync: bind mode; restored cache moved under ${subdir}/ (no copy)"
  else
    for_pairs copy_in
  fi
else
  if [[ "$bind_mode" == 1 ]]; then
    # Nothing to copy: the stage wrote straight into the host tree. Hand
    # root-owned artifacts back to the runner for later steps and rust-cache.
    chown_tree "$host_target"
    chown_tree "$cargo_home"
    echo "sonic-cache-sync: bind mode; the build wrote through to ${host_target} (no copy)"
  else
    for_pairs copy_out
  fi
fi
