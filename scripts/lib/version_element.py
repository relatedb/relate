#!/usr/bin/env python3
"""The one accessor machine behind release-bump.sh's XML version sweeps.

Both swept documents — the Maven pom and the SDK-style csproj — carry a
project-level version element the release must move in lockstep with the
tag, and both need the same reader/writer discipline: structural access
through the parse, a one-line exact-match rewrite so comments and every
other byte survive, and an atomic sidecar write so a failure mid-write
never truncates the document. That policy lives here once; each document
module carries only its spec ([`VersionDocument`]): which element, which
indent, how the root and the element are selected, and what a refusal
says. A change to the XML-text rule, the rewrite path, or the diagnostic
shape therefore lands once and is tested once.

Usage (from a spec module):
  version_element.main(SPEC, argv)

Both verbs are argparse subcommands, so the usage grammar has one spelling:
`--help` prints it, and an unknown verb or arity is an argparse usage error
(exit 2). `set` refuses a version that cannot land as XML text, so a value
that would rewrite the document is an argparse error rather than a
successful exit that leaves it malformed. A document that cannot be read,
decoded, parsed, or written is a one-line diagnostic on stderr and exit 1
— never a traceback.

`set` locates the element through the parse, then rewrites only that one
line by exact match (the spec's indent, the old value). The file is read
once and both the parse and the rewrite derive from those bytes; the
rewritten text lands through the shared atomic sidecar writer
(scripts/lib/atomic_write.py).
"""

from __future__ import annotations

import argparse
import dataclasses
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Callable

from atomic_write import atomic_write


class VersionElementError(Exception):
    """A refusal: the document does not carry what the verb needs."""


class Refusal(Exception):
    """A structural refusal raised by a spec's root check or selector.

    The spec knows the document's shape but not its path; `_load` is the
    one place a refusal is rendered, and it adds the path there.
    """


@dataclasses.dataclass(frozen=True)
class VersionDocument:
    """The spec one swept document contributes to the shared machine."""

    label: str
    """The CLI's prog and diagnostic prefix, e.g. `pom.py`."""

    description: str
    """The argparse description."""

    file_metavar: str
    """The file argument's metavar, e.g. `POM`."""

    get_help: str
    set_help: str

    check_root: Callable[[str], None]
    """The root policy as one behavior: returns when the root element is
    the document's expected root — matched however the document spells
    tags, by local name for a namespaced document and by exact tag
    otherwise — and raises [`Refusal`] when it is not, so a namespace
    refusal is declared where it belongs and never surfaces for an
    unrelated wrong root."""

    element: str
    """The version element's tag — the one spelling; the spec's selector
    builds its path from it and the rewrite anchors on it."""

    indent: int
    """The indent width the version element's line sits at — the rewrite's
    anchor, so an exact indent+value match cannot hit a deeper-nested
    element with the same tag."""

    select: Callable[[ET.Element, str], str]
    """The document's element selector, called with the parsed root and
    the element tag: returns the element's stripped text. A malformed
    shape or an absent element raises [`Refusal`] with its message —
    whether a blank entry is its own refusal or reads as absent is the
    document's own policy, stated in its selector."""


def local_name(tag: str) -> str:
    """The tag's local name — the part after a namespace's closing brace.

    Public: a spec's selector matches child elements by local name when the
    document parses with a default xmlns, so it is part of the machine's
    shared surface, not a private helper.
    """
    return tag.rsplit("}", 1)[-1]


def _version_text(value: str) -> str:
    """argparse type: a version that becomes the element's text, nothing else.

    Two rules, one guard and one message each. The CLI's own value rule: a
    version is non-empty and carries no whitespace. The XML-text rule: the
    value must be the element's text, checked by parsing the very element
    the writer emits, so a `<`, `&`, or a control character XML forbids is
    refused instead of written. What a release version is spelled like
    belongs to the sweep that passes it in.
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


def _load(doc: VersionDocument, path: Path) -> tuple[str, bytes]:
    """Read `path`, validate its shape, and return the version text and the
    raw bytes — every refusal a [`VersionElementError`]."""
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise VersionElementError(f"{path}: cannot read: {exc}") from exc
    try:
        root = ET.fromstring(raw)
    except ET.ParseError as exc:
        raise VersionElementError(f"{path}: not well-formed XML: {exc}") from exc
    try:
        doc.check_root(root.tag)
        old = doc.select(root, doc.element)
    except Refusal as exc:
        raise VersionElementError(f"{path}: {exc}") from exc
    return old, raw


def _rewrite(doc: VersionDocument, path: Path, text: str, old: str, new: str) -> None:
    """Rewrite the version element's line, or raise
    [`VersionElementError`] when it is not there."""
    pad = " " * doc.indent
    pattern = re.compile(
        rf"^{pad}<{doc.element}>{re.escape(old)}</{doc.element}>$", re.MULTILINE
    )
    # A callable replacement keeps a version's backslashes literal, where a
    # replacement template would read them as group references.
    rewritten, found = pattern.subn(
        lambda _: f"{pad}<{doc.element}>{new}</{doc.element}>", text, count=1
    )
    if not found:
        raise VersionElementError(
            f"{path}: no {doc.indent}-space <{doc.element}>{old}</{doc.element}> "
            f"line to rewrite"
        )
    atomic_write(path, rewritten)


def _get(args: argparse.Namespace) -> int:
    """Print the document's version text."""
    old, _ = _load(args.doc, args.path)
    print(old)
    return 0


def _set(args: argparse.Namespace) -> int:
    """Rewrite the document's version text to `args.version`."""
    old, raw = _load(args.doc, args.path)
    # The bytes are text only on this arm, so the decode lives here: `get`
    # reads `old` off the parse and never needs the whole file decoded.
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise VersionElementError(f"{args.path}: cannot read: {exc}") from exc
    try:
        _rewrite(args.doc, args.path, text, old, args.version)
    except OSError as exc:
        raise VersionElementError(f"{args.path}: cannot write: {exc}") from exc
    return 0


def _parser(doc: VersionDocument) -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog=doc.label, description=doc.description)
    # No verb dest: dispatch is per-subparser (set_defaults(run=...) below),
    # so nothing reads the parsed verb name.
    commands = parser.add_subparsers(required=True)

    get_cmd = commands.add_parser("get", help=doc.get_help)
    get_cmd.add_argument("path", type=Path, metavar=doc.file_metavar)
    # One handler per subparser: argparse owns the verb dispatch, so `main`
    # never re-tests the verb as a string.
    get_cmd.set_defaults(run=_get, doc=doc)

    set_cmd = commands.add_parser("set", help=doc.set_help)
    set_cmd.add_argument("path", type=Path, metavar=doc.file_metavar)
    set_cmd.add_argument("version", type=_version_text, metavar="VERSION")
    set_cmd.set_defaults(run=_set, doc=doc)

    return parser


def main(doc: VersionDocument, argv: list[str] | None = None) -> int:
    args = _parser(doc).parse_args(argv)
    try:
        return args.run(args)
    except VersionElementError as exc:
        # The CLI's one diagnostic: `<label>: <message>` on stderr, exit 1.
        print(f"{doc.label}: {exc}", file=sys.stderr)
        return 1
