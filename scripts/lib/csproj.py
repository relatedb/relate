#!/usr/bin/env python3
"""Structural accessors for an SDK-style csproj's package <Version>.

The one reader/writer behind release-bump.sh's .NET sweep: the version the
NuGet package publishes must be the release tag, and the release's own
package-name check refuses a stale one — a sweep that silently missed the
file would be discovered at release time instead.

The shared machine (scripts/lib/version_element.py) carries the access
discipline; this module carries only the csproj's spec. The csproj's own
facts: it parses without a namespace (an SDK-style project has no xmlns),
so the root is matched by exact tag and a namespaced one is refused as a
namespace, not as a wrong element name; the element is a direct child of a
top-level <PropertyGroup>; and there must be exactly one — package
metadata can carry <Version> per property group, and a second entry would
make "the version" ambiguous, which is a refusal here rather than a
first-match guess. A blank entry is refused separately, so a malformed
project never reads as version "".

Usage: `csproj.py get <csproj>` and `csproj.py set <csproj> <version>` —
the verbs, their arguments, and what each exits with are the CLI's own
`--help`.
"""

from __future__ import annotations

import sys
import xml.etree.ElementTree as ET

from version_element import Refusal, VersionDocument, local_name, main


def _check_root(tag: str) -> None:
    """Refuse any root that is not exactly <Project> — a namespaced
    <Project> is a namespace refusal, never a wrong-element one, and a
    wrong element is named by local name, as the pom's root check does."""
    if tag == "Project":
        return
    if local_name(tag) == "Project":
        raise Refusal(
            "root element is a namespaced <Project> "
            "(a namespaced project is not supported)"
        )
    raise Refusal(f"root element is <{local_name(tag)}>, not <Project>")


def _select(root: ET.Element, element: str) -> str:
    """The package <Version> text.

    ElementTree's own path states the two element levels in one
    expression; the refusals carry their own messages because each is its
    own malformed shape.
    """
    entries = [(e.text or "").strip() for e in root.findall(f"./PropertyGroup/{element}")]
    if not entries:
        raise Refusal(f"no <{element}> entry in a PropertyGroup")
    if len(entries) > 1:
        raise Refusal(
            f"{len(entries)} <{element}> entries in PropertyGroups "
            f"({', '.join(entries)}); the package version is ambiguous"
        )
    if not entries[0]:
        raise Refusal(f"the <{element}> entry is blank")
    return entries[0]


CSPROJ = VersionDocument(
    label="csproj.py",
    description="Read or rewrite an SDK-style csproj's package <Version>.",
    file_metavar="CSPROJ",
    get_help="print the package <Version>; exit 1 when absent",
    set_help="rewrite the package <Version> to VERSION",
    check_root=_check_root,
    element="Version",
    indent=4,
    select=_select,
)


if __name__ == "__main__":
    sys.exit(main(CSPROJ))
