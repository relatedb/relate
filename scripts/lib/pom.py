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

`set` locates the element through the parse, then rewrites only that one
line by exact match (2-space indent, the old value) so comments, attribute
style, and every other byte of the file survive untouched — ElementTree
serialization would drop the pom's comments.
"""

from __future__ import annotations

import re
import sys
import xml.etree.ElementTree as ET


def _local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def _project_version_element(root: ET.Element) -> ET.Element | None:
    if _local(root.tag) != "project":
        raise SystemExit(f"pom.py: root element is <{ _local(root.tag) }>, not <project>")
    for child in root:
        if _local(child.tag) == "version":
            return child
    return None


def main(argv):
    usage = "usage: pom.py get <pom> | pom.py set <pom> <version>"
    # Read the verb behind an arity guard so the one usage check below can
    # test it without indexing past the end of argv.
    verb = argv[1] if len(argv) > 1 else None
    if not (
        (verb == "get" and len(argv) == 3)
        or (verb == "set" and len(argv) == 4)
    ):
        raise SystemExit(usage)
    pom = argv[2]
    try:
        root = ET.parse(pom).getroot()
    except ET.ParseError as exc:
        print(f"pom.py: {pom} is not well-formed XML: {exc}", file=sys.stderr)
        return 1
    element = _project_version_element(root)
    if element is None or (element.text or "").strip() == "":
        print(f"pom.py: {pom} has no project <version> entry", file=sys.stderr)
        return 1
    old = element.text.strip()

    if verb == "get":
        print(old)
        return 0

    new = argv[3]
    # Rewrite the one line the parse identified: the project-level
    # <version> sits at 2-space indent, so an exact 2-space + old-value
    # match cannot hit a dependency's deeper-indented version line.
    pattern = re.compile(r"^(  )<version>" + re.escape(old) + r"</version>$")
    lines = open(pom).read().split("\n")
    for i, line in enumerate(lines):
        if pattern.match(line):
            lines[i] = f"  <version>{new}</version>"
            open(pom, "w").write("\n".join(lines))
            return 0
    print(f"pom.py: {pom}: no 2-space <version>{old}</version> line to rewrite", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
