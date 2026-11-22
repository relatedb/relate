#!/usr/bin/env python3
"""Structural accessors for the Maven pom's project <version>.

The one reader/writer behind release-bump.sh's pom sweep (the sed-range
approach it replaces anchored on the file's layout — the optional
<name> element — rather than the version element itself, and could not
distinguish "wrong value" from "anchor missed").

The shared machine (scripts/lib/version_element.py) carries the access
discipline; this module carries only the pom's spec. The pom's own facts:
it parses with a default xmlns, so the root and the version element are
matched by local name — never assuming the prefix — and the project's
<version> is a direct child of <project> — dependency and plugin versions
live deeper and are never touched. A blank element is as absent as a
missing one.

Usage: `pom.py get <pom>` and `pom.py set <pom> <version>` — the verbs,
their arguments, and what each exits with are the CLI's own `--help`.
"""

from __future__ import annotations

import sys
import xml.etree.ElementTree as ET

from version_element import Refusal, VersionDocument, local_name, main


def _check_root(tag: str) -> None:
    """Refuse any root whose local name is not <project>."""
    if local_name(tag) != "project":
        raise Refusal(f"root element is <{local_name(tag)}>, not <project>")


def _select(root: ET.Element, element: str) -> str:
    """The project's own <version> text.

    The `{*}` path matches the element by local name in any namespace.
    A second project-level <version> is not a refusal — the first match in
    document order is the pom's own long-standing policy. A blank element
    is as absent as a missing one, so both raise the one absence refusal.
    """
    matches = root.findall(f"{{*}}{element}")
    value = (matches[0].text or "").strip() if matches else ""
    if not value:
        raise Refusal("no project <version> entry")
    return value


POM = VersionDocument(
    label="pom.py",
    description="Read or rewrite the Maven pom's project <version>.",
    file_metavar="POM",
    get_help="print /project/version; exit 1 when absent",
    set_help="rewrite /project/version to VERSION",
    check_root=_check_root,
    element="version",
    indent=2,
    select=_select,
)


if __name__ == "__main__":
    sys.exit(main(POM))
