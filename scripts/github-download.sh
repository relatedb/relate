#!/bin/sh
# Download one GitHub release asset, or one raw.githubusercontent.com file.
#
# A token (GH_TOKEN, else GITHUB_TOKEN) uses the API. Release bytes come
# from the asset URL with Accept: application/octet-stream. The public
# releases/download URL stays anonymous even when an Authorization header
# is sent, so this script never uses it while a token is present.
# raw.githubusercontent.com is the same: the contents API with
# Accept: application/vnd.github.raw is the authenticated GET.
#
# GitHub Actions refuses the anonymous fallback. A token whose value is
# the literal name GITHUB_TOKEN or GH_TOKEN counts as unset — Sonic
# forwards the name when the host variable is absent.
#
# Usage:
#   github-download.sh --repo OWNER/NAME --tag TAG --asset NAME -o DEST
#   github-download.sh --raw https://raw.githubusercontent.com/OWNER/NAME/REF/PATH -o DEST
set -eu

usage() {
  echo "usage: github-download.sh (--repo OWNER/NAME --tag TAG --asset NAME | --raw URL) -o DEST" >&2
}

repo=""
tag=""
asset=""
raw=""
dest=""
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) repo="${2:?}"; shift 2 ;;
    --tag) tag="${2:?}"; shift 2 ;;
    --asset) asset="${2:?}"; shift 2 ;;
    --raw) raw="${2:?}"; shift 2 ;;
    -o|--output) dest="${2:?}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "github-download: unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

if [ -z "$dest" ]; then
  echo "github-download: -o DEST is required" >&2
  usage
  exit 2
fi
if [ -n "$raw" ] && [ -n "$repo$tag$asset" ]; then
  echo "github-download: pass either --raw or --repo/--tag/--asset" >&2
  exit 2
fi
if [ -z "$raw" ] && { [ -z "$repo" ] || [ -z "$tag" ] || [ -z "$asset" ]; }; then
  echo "github-download: --repo, --tag, and --asset are required" >&2
  usage
  exit 2
fi

token="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
case "$token" in
  GITHUB_TOKEN|GH_TOKEN) token="" ;;
esac

dest_dir=$(dirname "$dest")
mkdir -p "$dest_dir"

if [ -z "$token" ]; then
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    echo "github-download: GITHUB_TOKEN is required in GitHub Actions so the download is not anonymous" >&2
    exit 1
  fi
  if [ -n "$raw" ]; then
    case "$raw" in
      https://raw.githubusercontent.com/*) ;;
      *) echo "github-download: --raw must be a https://raw.githubusercontent.com URL" >&2; exit 2 ;;
    esac
    curl -fsSL -o "$dest" "$raw"
  else
    curl -fsSL -o "$dest" "https://github.com/${repo}/releases/download/${tag}/${asset}"
  fi
  exit 0
fi

if [ -n "$raw" ]; then
  case "$raw" in
    https://raw.githubusercontent.com/*) ;;
    *) echo "github-download: --raw must be a https://raw.githubusercontent.com URL" >&2; exit 2 ;;
  esac
  rest=${raw#https://raw.githubusercontent.com/}
  owner=${rest%%/*}
  rest=${rest#*/}
  name=${rest%%/*}
  rest=${rest#*/}
  ref=${rest%%/*}
  path=${rest#*/}
  if [ -z "$owner" ] || [ -z "$name" ] || [ -z "$ref" ] || [ -z "$path" ] || [ "$path" = "$rest" ]; then
    echo "github-download: --raw must be https://raw.githubusercontent.com/OWNER/NAME/REF/PATH" >&2
    exit 2
  fi
  curl -fsSL \
    -H "Authorization: Bearer ${token}" \
    -H "Accept: application/vnd.github.raw" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    -o "$dest" \
    "https://api.github.com/repos/${owner}/${name}/contents/${path}?ref=${ref}"
  exit 0
fi

owner=${repo%%/*}
name=${repo#*/}
if [ -z "$owner" ] || [ -z "$name" ] || [ "$owner" = "$name" ]; then
  echo "github-download: --repo must be OWNER/NAME" >&2
  exit 2
fi

if command -v python3 >/dev/null 2>&1; then
  py=python3
elif command -v python >/dev/null 2>&1; then
  py=python
else
  echo "github-download: python3 is required to read the GitHub release asset list" >&2
  exit 1
fi

api="https://api.github.com/repos/${owner}/${name}/releases/tags/${tag}"
asset_url=$(curl -fsSL \
  -H "Authorization: Bearer ${token}" \
  -H "Accept: application/vnd.github+json" \
  -H "X-GitHub-Api-Version: 2022-11-28" \
  "$api" | "$py" -c '
import json, sys
want = sys.argv[1]
try:
    data = json.load(sys.stdin)
except json.JSONDecodeError:
    raise SystemExit(1)
for item in data.get("assets") or []:
    if item.get("name") == want and item.get("url"):
        print(item["url"])
        raise SystemExit(0)
raise SystemExit(1)
' "$asset") || {
  echo "github-download: asset ${asset} not found on ${owner}/${name} tag ${tag}" >&2
  exit 1
}

curl -fsSL \
  -H "Authorization: Bearer ${token}" \
  -H "Accept: application/octet-stream" \
  -o "$dest" \
  "$asset_url"
