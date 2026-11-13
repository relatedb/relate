#!/usr/bin/env python3
"""Structural accessors for the Maven pom's project <version>.

The one reader/writer behind release-bump.sh's pom sweep (the sed-range
approach it replaces anchored on the file's layout — the optional
<name> element — rather than the version element itself, and could not
distinguish "wrong value" from "anchor missed").

The pom carries a default xmlns, so elements parse with a namespace; match
local names and never assume the prefix. Direct children of <project> only
— dependency and plugin versions live deeper and are never touched.

Usage:
  pom.py get <pom>          print /project/version; exit 1 when absent
  pom.py set <pom> <version>

Both verbs are argparse subcommands, so the usage grammar has one spelling:
`--help` prints it, and an unknown verb or arity is an argparse usage error
(exit 2). `set` refuses a version that cannot land as XML text, so a value
that would rewrite the document is an argparse error rather than a
successful exit that leaves the pom malformed. A pom that cannot be read,
decoded, parsed, or written is a one-line diagnostic on stderr and exit 1 —
never a traceback.

`set` locates the element through the parse, then rewrites only that one
line by exact match (2-space indent, the old value) so comments, attribute
style, and every other byte of the file survive untouched — ElementTree
serialization would drop the pom's comments. The file is read once and both
the parse and the rewrite derive from those bytes; the rewritten text lands
through a sibling temp and an atomic rename, so a failure mid-write never
truncates the pom.
"""

from __future__ import annotations

import argparse
import os
import re
import stat
import sys
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path


def _local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def _fail(message: str) -> int:
    """The module's one diagnostic: `pom.py: <message>` on stderr, exit 1."""
    print(f"pom.py: {message}", file=sys.stderr)
    return 1


def _version_text(value: str) -> str:
    """argparse type: a version that becomes the element's text, nothing else.

    Two rules, one guard and one message each. This CLI's own value rule: a
    version is non-empty and carries no whitespace. The XML-text rule: the
    value must be the element's text, checked by parsing the very element the
    writer emits, so a `<`, `&`, or a control character XML forbids is refused
    instead of written. What a release version is spelled like belongs to the
    sweep that passes it in (release-bump.sh's version-token rule).
    """
    if not value or any(ch.isspace() for ch in value):
        raise argparse.ArgumentTypeError(
            f"expected a non-empty version with no whitespace, got {value!r}"
        )
    try:
        element = ET.fromstring(f"<version>{value}</version>")
    except ET.ParseError as exc:
        raise argparse.ArgumentTypeError(
            f"expected a version that is XML text, got {value!r}: {exc}"
        ) from exc
    if element.text != value:
        raise argparse.ArgumentTypeError(
            f"expected a version that is XML text, got {value!r}"
        )
    return value


def _project_version(root: ET.Element) -> str | None:
    """The project's own <version> text, or None when it is absent or blank.

    One contract for the caller: a blank element is as absent as a missing
    one, so main tests a single value and never re-strips.
    """
    for child in root:
        if _local(child.tag) == "version":
            return (child.text or "").strip() or None
    return None


def _replace(pom: Path, text: str) -> None:
    """Write `text` over `pom` through a sibling temp and an atomic rename.

    The temp carries the `<target>.tmp.XXXXXX` sidecar shape release-bump.sh's
    rollback sweeps, and the rename preserves the target's mode (mkstemp
    creates 0600) — the same shape toml.sh's writer uses.
    """
    fd, tmp = tempfile.mkstemp(prefix=f"{pom.name}.tmp.", dir=pom.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            handle.write(text)
        os.chmod(tmp, stat.S_IMODE(pom.stat().st_mode))
        os.replace(tmp, pom)
    finally:
        # A no-op once the rename landed; otherwise the interrupted write
        # leaves nothing behind.
        Path(tmp).unlink(missing_ok=True)


def _rewrite(pom: Path, text: str, old: str, new: str) -> bool:
    """Rewrite the project-level <version> line; False when it is not there.

    The anchor is the 2-space indent the project's own <version> sits at, so
    an exact 2-space + old-value match cannot hit a dependency's
    deeper-indented version line.
    """
    pattern = re.compile(
        r"^  <version>" + re.escape(old) + r"</version>$", re.MULTILINE
    )
    # A callable replacement keeps a version's backslashes literal, where a
    # replacement template would read them as group references.
    rewritten, found = pattern.subn(
        lambda _: f"  <version>{new}</version>", text, count=1
    )
    if not found:
        return False
    _replace(pom, rewritten)
    return True


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="pom.py",
        description="Read or rewrite the Maven pom's project <version>.",
    )
    commands = parser.add_subparsers(dest="command", required=True)

    get_cmd = commands.add_parser(
        "get", help="print /project/version; exit 1 when absent"
    )
    get_cmd.add_argument("pom", type=Path, metavar="POM")

    set_cmd = commands.add_parser("set", help="rewrite /project/version to VERSION")
    set_cmd.add_argument("pom", type=Path, metavar="POM")
    set_cmd.add_argument("version", type=_version_text, metavar="VERSION")

    return parser


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)

    try:
        raw = args.pom.read_bytes()
    except OSError as exc:
        return _fail(f"{args.pom}: cannot read: {exc}")
    try:
        root = ET.fromstring(raw)
    except ET.ParseError as exc:
        return _fail(f"{args.pom}: not well-formed XML: {exc}")

    root_tag = _local(root.tag)
    if root_tag != "project":
        return _fail(f"{args.pom}: root element is <{root_tag}>, not <project>")
    old = _project_version(root)
    if old is None:
        return _fail(f"{args.pom}: no project <version> entry")

    if args.command == "get":
        print(old)
        return 0

    # The bytes are text only on the rewrite arm, so the decode lives here:
    # `get` reads `old` off the parse and never needs the whole file decoded.
    try:
        text = raw.decode("utf-8")
        found = _rewrite(args.pom, text, old, args.version)
    except UnicodeDecodeError as exc:
        return _fail(f"{args.pom}: cannot read: {exc}")
    except OSError as exc:
        return _fail(f"{args.pom}: cannot write: {exc}")
    if found:
        return 0
    return _fail(f"{args.pom}: no 2-space <version>{old}</version> line to rewrite")


if __name__ == "__main__":
    sys.exit(main())
