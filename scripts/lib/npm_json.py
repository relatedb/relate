#!/usr/bin/env python3
"""Typed reads and the one pack-time write of the npm JSON files.

One JSON parse behind every npm read-back (manifest version, lockfile
top-level and root-entry versions, the @relate platform links), and the
one pack-time rewrite that pins those links to the release version
(ADR-0616), replacing layout-sniffing sed/awk that an indent change
would silently break.

The repository shape — every @relate-scoped optionalDependency entry
resolves the platform package it names as a file:platforms/<dir> link
to that package's in-repo directory (the ADR-0562 correspondence) — is
defined once here: _platform_links validates the map, the read verbs
count it, and `pin` rewrites exactly those entries. A versioned @relate
pin in a repository manifest is refused loudly: it is the shape the
release-broken npm ci came from.

Usage:
  npm_json.py version <file>          top-level "version"
  npm_json.py root-version <lockfile> packages[""]."version"
  npm_json.py file-links <manifest>
                                      count of the manifest's validated
                                      @relate/* file: platform links
  npm_json.py lock-file-links <lockfile>
                                      the same count on packages[""]
  npm_json.py lock-platform-versions <lockfile> <version>
                                      count of packages["platforms/<dir>"]
                                      entries whose version matches — the
                                      witness for the versions the release
                                      sweep writes into the linked entries
  npm_json.py pin <manifest>
                                      the pack-time write (ADR-0616):
                                      rewrite every @relate platform
                                      link's value to the manifest's own
                                      version; prints the pinned map

A missing or non-string field is a nonzero exit with a message, never an
empty read.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Callable, NamedTuple, NoReturn

PIN_PREFIX = "@relate/"
PIN_PACKAGE_PREFIX = "@relate/relate-"
FILE_LINK_PREFIX = "file:platforms/"


def _die(msg: str) -> NoReturn:
    raise SystemExit(f"npm_json.py: {msg}")


def _field(obj: dict, key: str, kind: type, where: str):
    """The one typed accessor: <key> of <obj> as <kind>, or a nonzero exit."""
    value = obj.get(key)
    if not isinstance(value, kind):
        _die(f'{where}: no {kind.__name__} "{key}"')
    return value


def _load(path: str) -> dict:
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        _die(f"{path}: top level is not an object")
    return data


def _save(path: str, data: dict) -> None:
    Path(path).write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")


def _version(obj: dict, where: str) -> str:
    return _field(obj, "version", str, where)


def _root_entry(data: dict, path: str) -> dict:
    packages = _field(data, "packages", dict, path)
    return _field(packages, "", dict, f'{path} packages')


def _platform_links(holder: dict, where: str) -> dict:
    """The repository shape, validated: every @relate-scoped
    optionalDependencies entry names a platform package and links it to
    that package's own directory (the ADR-0562 correspondence). Returns
    the validated name → link map; anything else is a nonzero exit."""
    deps = _field(holder, "optionalDependencies", dict, where)
    links = {}
    for key, value in deps.items():
        if not key.startswith(PIN_PREFIX):
            continue
        if not isinstance(value, str):
            _die(f'{where}: optionalDependencies "{key}" is not a string')
        if not key.startswith(PIN_PACKAGE_PREFIX):
            _die(f'{where}: optionalDependencies "{key}" is @relate-scoped but '
                 f"not a platform package name ({PIN_PACKAGE_PREFIX}*)")
        if not value.startswith(FILE_LINK_PREFIX):
            _die(f'{where}: optionalDependencies "{key}" is not a {FILE_LINK_PREFIX} link '
                 f'(got "{value}") — the repository shape must stay file:-linked; '
                 "registry pins are injected at pack time only")
        directory = value[len(FILE_LINK_PREFIX):]
        if key[len(PIN_PACKAGE_PREFIX):] != directory:
            _die(f'{where}: optionalDependencies "{key}" links "{value}" — the '
                 "platform link must resolve the package it names (its own "
                 "platforms/<dir> directory, ADR-0562)")
        links[key] = value
    return links


def _version_cmd(path: str) -> str:
    return _version(_load(path), path)


def _root_version_cmd(path: str) -> str:
    return _version(_root_entry(_load(path), path), f'{path} packages[""]')


def _file_links_cmd(path: str) -> int:
    return len(_platform_links(_load(path), path))


def _lock_file_links_cmd(path: str) -> int:
    return len(_platform_links(_root_entry(_load(path), path), f'{path} packages[""]'))


def _lock_platform_versions_cmd(path: str, version: str) -> int:
    packages = _field(_load(path), "packages", dict, path)
    count = 0
    for key, entry in packages.items():
        if not key.startswith("platforms/"):
            continue
        if not isinstance(entry, dict):
            _die(f'{path} packages["{key}"] is not an object')
        if _field(entry, "version", str, f'{path} packages["{key}"]') == version:
            count += 1
    return count


def _pin_cmd(path: str) -> None:
    """The pack-time write (ADR-0616): rewrite every @relate platform
    link's value to the manifest's own version, derived — never passed
    in — so the published pins are correct by construction."""
    data = _load(path)
    version = _version(data, path)
    links = _platform_links(data, path)
    if not links:
        _die(f"{path}: no @relate platform links to pin")
    deps = _field(data, "optionalDependencies", dict, path)
    for key in links:
        deps[key] = version
    _save(path, data)
    print(json.dumps({key: deps[key] for key in links}))


class Command(NamedTuple):
    handler: Callable
    arity: int


COMMANDS = {
    "version": Command(_version_cmd, 1),
    "root-version": Command(_root_version_cmd, 1),
    "file-links": Command(_file_links_cmd, 1),
    "lock-file-links": Command(_lock_file_links_cmd, 1),
    "lock-platform-versions": Command(_lock_platform_versions_cmd, 2),
    "pin": Command(_pin_cmd, 1),
}


def main(argv: list[str]) -> int:
    command = COMMANDS.get(argv[1]) if len(argv) > 1 else None
    if command is None or len(argv) != 2 + command.arity:
        print(__doc__, file=sys.stderr)
        return 2
    result = command.handler(*argv[2:])
    if result is not None:
        print(result)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
