#!/usr/bin/env bash
# Pull public Obsidian vaults into testdata/obsidian-vaults/ for the
# markdown snapshot codec's env-gated tests (ADR-0406 §7,
# RELATE_MARKDOWN_VAULTS). The vaults are untracked test data —
# never committed — and each pull is idempotent: an existing folder is
# skipped unless --force re-pulls it.
#
# Usage:
#   scripts/pull_obsidian_vaults.sh            # pull missing vaults
#   scripts/pull_obsidian_vaults.sh --force    # re-pull everything
#
# obsidian-help is trimmed to its en/ subtree after cloning (the repo
# carries ~25 locale copies plus binary assets; only the English docs
# are needed as a fixture).

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$root/testdata/obsidian-vaults"
force=0
case "${1:-}" in
  "") ;;
  --force) force=1 ;;
  *)
    echo "usage: scripts/pull_obsidian_vaults.sh [--force]; unknown option: $1" >&2
    exit 2
    ;;
esac

# name|url|subdir-to-keep (empty = keep everything)
vaults=(
  "awesome-obsidian-vault|https://github.com/obsidian-pkm-vault/awesome-obsidian-vault|"
  "kepano-obsidian|https://github.com/kepano/kepano-obsidian|"
  "example-vault|https://github.com/kevboh/example-vault|"
  "obsidian-sample-vault|https://github.com/Benature/obsidian-sample-vault|"
  "Obsidian-Books-Vault|https://github.com/doctorfree/Obsidian-Books-Vault|"
  "obsidian-hub|https://github.com/community-archive/obsidian-hub|"
  "obsidian-help|https://github.com/obsidianmd/obsidian-help|en"
)

mkdir -p "$dest"

# Any failure path removes its own staging directory: the trap holds the
# current iteration's staging dir (`tmp=""` until mktemp fills it, and
# `rm -rf ""` is a no-op).
tmp=""
trap 'rm -rf "$tmp"' EXIT

for entry in "${vaults[@]}"; do
  name="${entry%%|*}"
  rest="${entry#*|}"
  url="${rest%%|*}"
  keep="${rest#*|}"

  if [[ -d "$dest/$name" && $force -eq 0 ]]; then
    echo "skip $name (exists; use --force to re-pull)"
    continue
  fi

  echo "pull $name from $url"
  # A token makes the clone an authenticated GitHub request. Without one,
  # and outside GitHub Actions, the public clone stays anonymous.
  token="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
  case "$token" in
    GITHUB_TOKEN|GH_TOKEN) token="" ;;
  esac
  if [[ -z "$token" && "${GITHUB_ACTIONS:-}" == "true" ]]; then
    echo "pull_obsidian_vaults: GITHUB_TOKEN is required in GitHub Actions" >&2
    exit 1
  fi
  # Stage inside the destination's own tree: the final `mv` then stays
  # on one filesystem (an atomic rename), never a copy+delete that can
  # fail halfway across the /tmp boundary.
  tmp="$(mktemp -d "$dest/.pull.XXXXXX")"
  if [[ -n "$token" ]]; then
    git -c "http.extraheader=AUTHORIZATION: bearer ${token}" clone --depth 1 "$url" "$tmp/$name"
  else
    git clone --depth 1 "$url" "$tmp/$name"
  fi
  # Keep only the wanted subtree while preserving the vault root layout.
  if [[ -n "$keep" ]]; then
    (
      cd "$tmp/$name"
      find . -mindepth 1 -maxdepth 1 ! -name "$keep" ! -name .git -exec rm -rf {} +
      # Move the subtree's contents up so the vault root is the docs root.
      if compgen -G "$tmp/$name/$keep/*" >/dev/null; then
        shopt -s dotglob
        mv "$tmp/$name/$keep"/* "$tmp/$name/"
        rmdir "$tmp/$name/$keep"
      fi
    )
  fi
  rm -rf "$tmp/$name/.git"
  # The replace is renames only, all on the destination's own filesystem:
  # the old vault is parked beside the destination — never inside the
  # trap-cleaned staging dir — and survives until the new tree is
  # installed, so any failure between the two mv's leaves either the
  # old or the new vault complete at the destination — never a
  # half-deleted or half-copied tree, and never a destroyed old tree.
  old_parked=""
  if [[ -d "$dest/$name" ]]; then
    old_parked="$dest/.pull-old-$name.$$"
    mv "$dest/$name" "$old_parked"
  fi
  if ! mv "$tmp/$name" "$dest/$name"; then
    # Install failed: put the old vault back before the trap cleans the
    # staging tree.
    [[ -n "$old_parked" ]] && mv "$old_parked" "$dest/$name" || true
    exit 1
  fi
  [[ -n "$old_parked" ]] && rm -rf "$old_parked"
  rm -rf "$tmp"

  md=$(find "$dest/$name" -name '*.md' | wc -l | tr -d ' ')
  size=$(du -sh "$dest/$name" | cut -f1)
  echo "  -> $dest/$name ($md .md files, $size)"
done

echo
echo "Vaults ready under $dest — point RELATE_MARKDOWN_VAULTS at it,"
echo "e.g. RELATE_MARKDOWN_VAULTS=$dest/kepano-obsidian,$dest/obsidian-hub"
