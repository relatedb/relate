#!/usr/bin/env bash
# Refuse a .sonic/ci.yml that mounts the Docker socket outside the
# reviewed set, bind-mounts the socket path itself, or forwards
# GITHUB_TOKEN into a socket stage other than the image build.
#
# gosonic mounts the socket for whatever this file says, and a volumes
# entry can mount it without docker_socket. This script is the policy
# that lives in the repository. The composite action and the background
# cadence jobs run it before gosonic.
#
# Usage: check-sonic-socket-policy.sh [path-to-ci.yml]
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
file=${1:-"$root/.sonic/ci.yml"}

if [ ! -f "$file" ]; then
  echo "check-sonic-socket-policy: $file: no such file" >&2
  exit 2
fi

exec python3 - "$file" <<'PY'
import re
import sys

path = sys.argv[1]
text = open(path, encoding="utf-8").read().splitlines()

# The stages that may set docker_socket: true. Adding one is a change to
# this list, in the same pull request that needs the socket.
SOCKET_ALLOWLIST = {
    "build-sonic-rust",
    "build-sonic-polyglot",
    "loadtest-baselines",
    "query-latency",
    "memory-controls",
    "memory-controls-full",
    "memory-shape",
    "clean",
}
# The image build is the only socket stage that may see the token. It
# passes the token as a BuildKit secret. Every later socket stage finds
# the CLI already in the image.
TOKEN_WITH_SOCKET = {"build-sonic-rust"}
TRUE_VALUES = {"true", "yes", "on"}


def code_of(line):
    return re.sub(r"\s+#.*$", "", line).rstrip()


def read_list(lines, index):
    items = []
    while index < len(lines):
        raw = lines[index]
        stripped = raw.strip()
        if stripped == "" or stripped.startswith("#"):
            index += 1
            continue
        match = re.match(r"^\s+-\s+([A-Za-z_][A-Za-z0-9_]*)\s*$", code_of(raw))
        if not match:
            break
        items.append(match.group(1))
        index += 1
    return items, index


def flow_list(rest):
    inner = rest.strip()
    if not (inner.startswith("[") and inner.endswith("]")):
        return None
    body = inner[1:-1].strip()
    if body == "":
        return []
    return [part.strip().strip("'\"") for part in body.split(",")]


anchors = {}
index = 0
while index < len(text):
    code = code_of(text[index])
    match = re.match(r"^\s+(?:optional_)?secrets:\s*(.*?)\s*$", code)
    if match:
        rest = match.group(1).strip()
        named = re.match(r"&([A-Za-z0-9_-]+)$", rest)
        if named:
            items, index = read_list(text, index + 1)
            anchors[named.group(1)] = items
            continue
    index += 1

section = None
current = None
stages = []


def begin(name):
    global current
    current = {"name": name, "socket": False, "secrets": set(), "sock_lines": []}
    stages.append(current)


errors = []
for lineno, raw in enumerate(text, start=1):
    code = code_of(raw)
    if re.match(r"^[A-Za-z0-9_-]+:\s*", code):
        section = code.split(":", 1)[0]
        current = None
        continue
    if section not in ("actions", "stages"):
        continue
    stage_name = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", code)
    if stage_name:
        begin(stage_name.group(1))
        continue
    if current is None or code.strip() == "":
        continue
    if "docker.sock" in code:
        current["sock_lines"].append(lineno)
    socket = re.match(r"^\s+docker_socket:\s*(.*?)\s*$", code)
    if socket:
        value = socket.group(1).strip().strip("'\"").lower()
        if value in TRUE_VALUES:
            current["socket"] = True
        continue
    secrets = re.match(r"^\s+(?:optional_)?secrets:\s*(.*?)\s*$", code)
    if not secrets:
        continue
    rest = secrets.group(1).strip()
    if rest.startswith("*"):
        anchor = rest[1:]
        if anchor not in anchors:
            errors.append(
                f"{path}:{lineno}: secret alias *{anchor} is not defined"
            )
            continue
        current["secrets"].update(anchors[anchor])
        continue
    if rest.startswith("&"):
        anchor = rest[1:]
        if anchor not in anchors:
            errors.append(
                f"{path}:{lineno}: secret anchor &{anchor} has no list"
            )
            continue
        current["secrets"].update(anchors[anchor])
        continue
    if rest == "":
        items, _ = read_list(text, lineno)
        current["secrets"].update(items)
        continue
    flowed = flow_list(rest)
    if flowed is None:
        errors.append(
            f"{path}:{lineno}: could not read the secret list {rest!r}"
        )
        continue
    current["secrets"].update(flowed)

for stage in stages:
    name = stage["name"]
    for lineno in stage["sock_lines"]:
        errors.append(
            f"{path}:{lineno}: stage {name} mounts the Docker socket through "
            "volumes. Set docker_socket: true on an allowlisted stage instead."
        )
    if not stage["socket"]:
        continue
    if name not in SOCKET_ALLOWLIST:
        errors.append(
            f"stage {name} sets docker_socket: true and is not on the "
            "allowlist. Add it to scripts/check-sonic-socket-policy.sh in "
            "the same change that needs the socket."
        )
    if "GITHUB_TOKEN" in stage["secrets"] and name not in TOKEN_WITH_SOCKET:
        errors.append(
            f"stage {name} mounts the Docker socket and forwards "
            "GITHUB_TOKEN. Remove GITHUB_TOKEN from that stage. "
            "build-sonic-rust is the only socket stage that may receive it."
        )

if errors:
    for error in errors:
        print(f"check-sonic-socket-policy: {error}", file=sys.stderr)
    sys.exit(1)
PY
