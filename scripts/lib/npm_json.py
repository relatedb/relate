#!/usr/bin/env python3
"""Typed reads of the npm JSON files release-bump.sh verifies.

One JSON parse behind every npm read-back (manifest version, lockfile
top-level and root-entry versions, @relate platform pins), replacing
layout-sniffing sed/awk that an indent change would silently break.

Usage:
  npm_json.py version <file>          top-level "version"
  npm_json.py root-version <lockfile> packages[""]."version"
  npm_json.py pins <manifest> <version>
                                      count of the manifest's @relate/*
                                      optionalDependencies pinned to <version>
  npm_json.py lock-pins <lockfile> <version>
                                      the same count on packages[""]

A missing or non-string field is a nonzero exit with a message, never an
empty read.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Callable, NamedTuple, NoReturn

PIN_PREFIX = "@relate/"


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


def _version(obj: dict, where: str) -> str:
    return _field(obj, "version", str, where)


def _root_entry(data: dict, path: str) -> dict:
    packages = _field(data, "packages", dict, path)
    return _field(packages, "", dict, f"{path} packages")


def _pin_count(holder: dict, where: str, version: str) -> int:
    deps = _field(holder, "optionalDependencies", dict, where)
    count = 0
    for key, value in deps.items():
        if not key.startswith(PIN_PREFIX):
            continue
        if not isinstance(value, str):
            _die(f'{where}: optionalDependencies "{key}" is not a string')
        count += value == version
    return count


def _version_cmd(path: str) -> str:
    return _version(_load(path), path)


def _root_version_cmd(path: str) -> str:
    return _version(_root_entry(_load(path), path), f'{path} packages[""]')


def _pins_cmd(path: str, version: str) -> int:
    return _pin_count(_load(path), path, version)


def _lock_pins_cmd(path: str, version: str) -> int:
    return _pin_count(_root_entry(_load(path), path), f'{path} packages[""]', version)


class Command(NamedTuple):
    handler: Callable
    arity: int


COMMANDS = {
    "version": Command(_version_cmd, 1),
    "root-version": Command(_root_version_cmd, 1),
    "pins": Command(_pins_cmd, 2),
    "lock-pins": Command(_lock_pins_cmd, 2),
}


def main(argv: list[str]) -> int:
    command = COMMANDS.get(argv[1]) if len(argv) > 1 else None
    if command is None or len(argv) != 2 + command.arity:
        print(__doc__, file=sys.stderr)
        return 2
    print(command.handler(*argv[2:]))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
