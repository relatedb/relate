#!/usr/bin/env python3
"""The output-boundary guard of the release-notes pipeline (ADR-0618):
the agent's publishable artifacts — notes.md and slack-payload.json —
are the one place untrusted pull-request prose can reach a public
surface (the release body, the Slack channel) without a human reading
it. The deny-by-default sandbox (ADR-0125 amendment 3) bounds what the
agent can DO, and the workflow's FIREWORKS_API_KEY scan bounds one
secret — this script bounds the CONTENT, for an end-user audience.

Design: presence-refusal plus real parsers. Nothing here emulates a
markdown or URL grammar with string matching — every decode rule and
prefix match is a bypass surface, and three security rounds proved it
(entity spellings of any character, backslash escapes, angle
destinations, userinfo: ``relatedb.ai@evil.example`` IS evil.example,
where a prefix match sees the allowed host and stops). So:

  - Every literal URL candidate is judged by urllib.parse.urlsplit:
    scheme, userinfo, host and port are the parser's fields. Userinfo
    and ports are refused outright (our notes never carry them), the
    host must match the allowlist exactly, and a github.com URL must
    sit under the project's tree. Candidates are judged EVERYWHERE —
    inside code fences too, because a fenced command is a copy-paste
    target on the release page.
  - Outside code fences, material a renderer WOULD decode — an HTML
    entity sequence, a backslash escape of URL-shaping punctuation —
    is refused by presence alone: a legitimate note never carries one
    there (the prompt forbids raw HTML, links are plain markdown, code
    examples live in fences), and no decode is attempted. Inside a
    fence an entity or escape is literal inert text and is allowed.
    Letter escapes (\\t, \\n in an inline code span) stay allowed: they
    cannot shape a URL.
  - Raw HTML tags are refused outside code fences, any tag name;
    angle autolinks (<https://…>) are exempt — they are URLs and are
    judged by the URL rules.
  - Protocol-relative destinations are refused outright: our links are
    absolute.
  - The payload is decoded by json.loads — a real parser — and every
    string it holds is judged by the same rules. A payload that is not
    valid JSON is a hard failure.
  - Internal reference ids (tracker, proposal, decision record) are
    refused anywhere in either artifact.

Over-matching is always fail-closed; under-matching is what the parsers
foreclose.

Usage: release_notes_validate.py --notes NOTES_FILE [--payload PAYLOAD_FILE]

  --notes     required; must exist and be non-empty
  --payload   optional; missing or empty = skip (the workflow owns when
              the payload must exist, ADR-0465); present but not valid
              JSON is a hard failure

Exit 0 = boundary clean; exit 1 = findings on stderr, one ::error::
line each.
"""

from __future__ import annotations

import argparse
import json
import re
import shlex
import sys
from pathlib import Path
from urllib.parse import urlsplit

# The allowlist: the project's own GitHub tree and site, plus the
# RFC 2606 reserved placeholder hosts (nothing can be hosted there, so a
# placeholder URL in an example is never a live external link). host →
# the path prefix the URL must sit under ("/" = any path).
ALLOWED_URLS: dict[str, str] = {
    "github.com": "/relatedb",
    "relatedb.ai": "/",
    "www.relatedb.ai": "/",
    "example.com": "/",
    "example.org": "/",
    "example.net": "/",
    "localhost": "/",
}

# Literal URL candidates: every `https?:`-prefixed run (case-
# insensitive), in ANY separator spelling — the canonical `://`, a
# single slash, no slashes at all, backslashes — because a browser
# normalizes every one of those to `https://`, and a destination the
# extractor never hands the parser is a destination the parser never
# judges. The run ends at whitespace and the markdown terminators.
# Bare www forms (GitHub autolinks those too) complete the set. The
# extractor deliberately OVER-matches: every weird spelling is captured
# and judged, so a miss requires a shape that is not `https?:`-
# prefixed at all.
URL_CANDIDATE = re.compile(
    r"https?\\?:[^\s)>\"`]+"
    r"|www\\?\.[A-Za-z0-9][^\s)>\"`]*"
    # Clickable non-web schemes (a mailto or data destination is as
    # clickable as an https one) and any scheme with an authority -
    # the `//` marks a real URL where a Cypher label (`b:Person`) or a
    # map literal is not. Both are extracted so the parser can refuse
    # them; a bare `word:word` stays prose.
    r"|(mailto|data|javascript|ftp|ftps|file|ssh|sftp|telnet|gopher|ws"
    r"|wss|irc|news|blob|vbscript):[^\s)>\"`]+"
    r"|[a-zA-Z][a-zA-Z0-9+.-]*://[^\s)>\"`]+",
    re.IGNORECASE,
)

# Execution-shaped lines: a pipe into a shell, a fetch verb, or an
# installer/container/vcs command — every spelling whose line can carry
# a network destination a reader will copy-paste. Such a line is a
# payload carrier, so every destination on it is audited — each bare
# host or IP token, path included, is judged by the same URL rules
# (github.com is prefix-scoped, userinfo and ports refused), and a
# pipe-to-shell line with no destination token at all is refused
# outright (the echo … | base64 -d | sh class: execute with no source
# is never ours). The signature set is deliberately non-exhaustive and
# over-strict: a line with no execution shape is never audited, so
# ordinary example lines — Cypher property accesses like b.id among
# them — cannot false-positive.
EXEC_LINE = re.compile(
    r"\|\s*(sudo\s+)?(xargs\s+)?(sh|bash|zsh|dash|ksh)\b"
    r"|\b(curl|wget|pip3?|npm|pnpm|yarn|npx|cargo|docker|podman|brew"
    r"|git\s+clone|go\s+install|relate\s+extension\s+install)\b",
    re.IGNORECASE,
)
PIPE_TO_SHELL = re.compile(
    r"\|\s*(sudo\s+)?(xargs\s+)?(sh|bash|zsh|dash|ksh)\b", re.IGNORECASE
)
# Runtime construction on an execution-shaped line: the payload is
# built at run time, so there is nothing to audit - refuse outright.
SUBSTITUTION = re.compile(r"\$\(|`|\beval\b")
# A fetch verb among the TOKENIZED words (git counts when cloning).
FETCH_VERB = re.compile(
    r"\b(curl|wget)\b|\bgit\s+clone\b", re.IGNORECASE
)
# Routing and TLS-verification flags: refused outright on
# execution-shaped lines, `--flag` and `--flag=value` alike.
ROUTING_FLAGS = {
    "--resolve", "--connect-to", "--proxy", "--socks", "--socks5-hostname",
    "-k", "--insecure", "-x",
}
# Source flags: their value (or, in space form, their next token) names
# where the command downloads from - judged in full, scheme or bare.
SOURCE_FLAGS = {
    "--url", "--from", "--index-url", "--extra-index-url", "--registry",
    "--channel",
}
# A bare destination token: a domain shape of two or more labels ending
# in a letter TLD, or an IPv4 literal, with an optional path — not part
# of a scheme-form URL (those the URL pass already judges) and not a
# path segment (a token right after a slash).
BARE_DESTINATION = re.compile(
    r"(?<![\w./@-])"
    r"(?:\d{1,3}(?:\.\d{1,3}){3}"
    r"|[a-zA-Z][a-zA-Z0-9-]*(?:\.[a-zA-Z][a-zA-Z0-9-]*)*\.[a-zA-Z]{2,})"
    r"(?:/[^\s)>\]\"'`]+)?"
)

# Encodable material outside code fences, refused by presence: an HTML
# entity sequence (any spelling — an entity can spell any character),
# or a backslash escape of URL-shaping punctuation (colon, slash, dot,
# at, backslash). Letter escapes cannot shape a URL and stay allowed.
ENCODED_MATERIAL = re.compile(r"&#?[0-9A-Za-z]+;|\\[.:/@\\]")

RAW_TAG = re.compile(r"<\s*/?\s*[A-Za-z][A-Za-z0-9-]*")
# Protocol-relative destinations resolve against whatever serves the
# page; our links are absolute. The destination may sit on the line
# AFTER the `](` — CommonMark allows the line ending as whitespace —
# so the pattern is searched over joined unfenced text, where \s
# crosses the break. MULTILINE lets the reference-definition arm match
# after any newline; its leading whitespace stays on one line.
PROTO_RELATIVE = re.compile(
    r"\]\(\s*<?//\w|^[ \t]*\[[^\]]+\]:[ \t]*\n?[ \t]*<?//",
    re.MULTILINE,
)

# Tracker, proposal and decision-record ids — LABS is the workspace's
# former name, REL8 its renamed one; both spellings are internal.
INTERNAL_REF = re.compile(r"\b(LABS|REL8|CIP|ADR)-[0-9]+\b")

# A code fence opens only with at most three spaces of indentation — a
# four-space-indented ``` is an indented code block, whose marker is
# literal text. The marker run itself is captured: per CommonMark, a
# fence closes only at a line of the SAME fence character whose run is
# at least as long as the opening one — a three-backtick line inside a
# four-backtick fence is content, and toggling on it would desync the
# tracker from the renderer.
FENCE_MARKER = re.compile(
    r"^ {0,3}(?:(?P<list>[-*+]|\d{1,9}[.)])[ \t]{1,4})?(?P<run>`{3,}|~{3,})"
)
# The heading grammar: the `#` run is captured so its length — the
# level — is the match's own fact, never re-derived beside it.
HEADING = re.compile(r"^(#{2,6})\s+\S")


def heading_level(line: str) -> int | None:
    """The one home of the heading grammar and its level arithmetic:
    the level of a `#{2,6}` heading line, or None for a non-heading."""
    match = HEADING.match(line)
    return None if match is None else len(match.group(1))


# The refusal reasons, one home: every URL judgment names its reason,
# so a finding states what actually went wrong - a downgrade is not an
# off-allowlist host.
ALLOWED = "allowed"
OFF_HOST = "outside the project's hosts"
NOT_HTTPS = "over http, not https"
FOREIGN_SCHEME = "with a scheme that is not ours"
USERINFO = "with userinfo credentials"
WITH_PORT = "with a port"
MALFORMED = "that does not parse as a URL"

# Connect-string schemes the honest notes use (`relate pull --url`, the
# Bolt tools): judged by the same host table, with ports legitimate - a
# connection string carries one by nature. Everything else that is not
# https is refused outright.
CONNECT_SCHEMES = {"neo4j", "neo4j+s", "neo4j+scc", "bolt"}

SCHEME_PREFIX = re.compile(r"^([a-zA-Z][a-zA-Z0-9+.-]*):")


def url_verdict(candidate: str) -> str:
    """Judge one literal URL candidate with a real URL parser and name
    the reason when refused - ALLOWED, or the refusal phrase. Before
    parsing, the two decodings a reader's click applies, in order. The
    markdown layer first: a backslash before punctuation is the escape
    for that punctuation, so an escaped scheme separator decodes to the
    plain separator and an escaped @ decodes to the userinfo form. The
    URL layer then: in a special scheme a backslash that survives - one
    before a letter - is a slash, and any run of slashes after the
    scheme introduces the authority, so `https:evil` and `https:/evil`
    reach `https://evil.example`. `http` stays `http` and is refused (a
    downgrade, not a variation); a connect-string scheme is judged by
    the same host table with ports allowed; every other scheme is
    refused. Userinfo and ports are refused for https (our notes never
    carry them); the host must match the allowlist exactly, and the
    path must sit under the allowed prefix. Scheme-less www and
    bare-host tokens are judged as https."""
    text = candidate.strip().rstrip(".,;:!?")
    # Markdown layer: backslash-punctuation escapes decode to the
    # punctuation itself.
    text = re.sub(r"\\([!-/:-@\[-`{-~])", r"\1", text)
    # URL layer: a surviving backslash in a special scheme is a slash.
    text = text.replace("\\", "/")

    # A bare www host is judged as https, never parsed as a bogus
    # scheme (`www.relatedb.ai:81@evil` is a host, userinfo and port).
    if text.lower().startswith("www.") and "://" not in text:
        text = f"https://{text}"
    else:
        scheme = SCHEME_PREFIX.match(text)
        if scheme is not None:
            name = scheme.group(1).lower()
            if name == "https":
                text = "https://" + text[scheme.end():].lstrip("/")
            elif name == "http":
                text = "http://" + text[scheme.end():].lstrip("/")
            elif name in CONNECT_SCHEMES:
                try:
                    parsed = urlsplit(text)
                    hostname = parsed.hostname
                    parsed.port  # raises ValueError on a malformed port
                except ValueError:
                    return MALFORMED
                if parsed.username is not None or parsed.password is not None:
                    return USERINFO
                if hostname not in ALLOWED_URLS:
                    return OFF_HOST
                return ALLOWED
            else:
                return FOREIGN_SCHEME
        elif "://" not in text:
            text = f"https://{text}"
    try:
        parsed = urlsplit(text)
        hostname = parsed.hostname  # lowercased by the parser
        port = parsed.port  # raises ValueError on a malformed port
    except ValueError:
        return MALFORMED
    if parsed.username is not None or parsed.password is not None:
        return USERINFO  # everything before @ is credentials
    if parsed.scheme != "https":
        return NOT_HTTPS
    if port is not None:
        return WITH_PORT
    prefix = ALLOWED_URLS.get(hostname or "")
    if prefix is None:
        return OFF_HOST
    if prefix == "/":
        return ALLOWED
    # The path prefix compare is case-insensitive: github.com/RELATEDB
    # is the project's tree, redirect and all - the HOST is the trust
    # boundary, the prefix only scopes it.
    path = (parsed.path or "/").lower()
    if path == prefix or path.startswith(prefix + "/"):
        return ALLOWED
    return OFF_HOST


def url_allowed(candidate: str) -> bool:
    """The boolean view of `url_verdict`."""
    return url_verdict(candidate) == ALLOWED


def _indent(line: str) -> int:
    """Leading spaces of a line."""
    return len(line) - len(line.lstrip(" "))


def line_fences(text: str) -> list[tuple[str, bool]]:
    """Every line of the document IN ORDER, each with its fence state:
    False outside a code fence, True inside. CommonMark fence rules,
    with the container realities the renderer honors: an opener may sit
    behind a list marker (`- ``` `) with a gap of one to four spaces -
    the fence lives in the list item, whose content indent is the run's
    column; a non-blank line indented LESS than that ends the list item
    and its fence, and the line itself is re-examined at top level (a
    wider gap is an indented code block, and the line is ordinary
    text); the opener's info string may not carry the fence character;
    an HTML comment block is raw HTML, opaque to fence tracking, so a
    marker inside it never opens a fence; and while a fence is open,
    only a line of the same fence character with a run at least as
    long, indented up to the opener's offset plus three, closes it.
    Marker lines are not content and carry no state of their own.

    One ordered view serves every channel: the URL and execution sweeps
    read every line (fenced lines are data - a fenced command is a
    copy-paste target), the markdown channels read the unfenced lines,
    and the empty-section rule reads the ordered document with fenced
    bodies as content - no second, reordered spelling of the document
    anywhere."""
    result: list[tuple[str, bool]] = []
    fence_char: str | None = None
    fence_len = 0
    fence_offset = 0
    fence_list_scoped = False
    comment_open = False
    for line in text.splitlines():
        if comment_open:
            # Raw HTML until the comment closes; a fence marker inside
            # is text.
            if "-->" in line:
                comment_open = False
            result.append((line, False))
            continue
        ends_by_dedent = (
            fence_char is not None
            and fence_list_scoped
            and line.strip() != ""
            and _indent(line) < fence_offset
        )
        if fence_char is not None and not ends_by_dedent:
            closer = re.match(
                r"^ {0,%d}%s{%d,}" % (fence_offset + 3, fence_char, fence_len),
                line,
            )
            if closer is not None:
                fence_char = None
            else:
                result.append((line, True))
            continue
        if ends_by_dedent:
            # The list item ends at this line: its fence with it. The
            # line is examined at top level below - it may open a new
            # fence there.
            fence_char = None
        # No fence is open, or the list item just ended at this line:
        # the line is examined at top level.
        if "<!--" in line and "-->" not in line.split("<!--", 1)[1]:
            # An HTML comment block opens: everything to the closer is
            # raw HTML, opaque to fence tracking.
            comment_open = True
            result.append((line, False))
            continue
        marker = FENCE_MARKER.match(line)
        valid_opener = (
            marker is not None
            and marker.group("run")[0] not in line[marker.end():]
        )
        if valid_opener:
            fence_char = marker.group("run")[0]
            fence_len = len(marker.group("run"))
            fence_offset = marker.start("run")
            fence_list_scoped = marker.group("list") is not None
        else:
            result.append((line, False))
    return result


def check_text(label: str, text: str, *, fence_aware: bool) -> list[str]:
    """The full policy over one artifact's text. ``fence_aware`` marks
    the notes (markdown: fence rules apply); the payload's strings are
    plain text and are judged wholesale."""
    findings: list[str] = []
    if fence_aware:
        lines = line_fences(text)
    else:
        lines = [(line, False) for line in text.splitlines()]

    # Literal URLs are judged everywhere - fences included. The
    # verdict names the reason: a downgrade, a port or a userinfo form
    # is not an off-allowlist host. A closing delimiter may split the
    # true destination - `<https://relatedb.ai)@evil.example/x>` ends
    # the extracted run at the `)` while the rendered href carries the
    # userinfo - so when the text after a candidate, up to whitespace
    # or an angle bracket, contains an @, the whole run is judged and
    # the userinfo inside it refuses (over-match, fail-closed).
    for line, _ in lines:
        for match in URL_CANDIDATE.finditer(line):
            candidate = match.group(0)
            rest = line[match.end():]
            stop = len(rest)
            for delimiter in ("\t", " ", "<", ">"):
                found = rest.find(delimiter)
                if found != -1:
                    stop = min(stop, found)
            tail = rest[:stop]
            if "@" in tail:
                candidate += tail
            verdict = url_verdict(candidate)
            if verdict != ALLOWED:
                findings.append(
                    f"{label} carries a URL {verdict}: {candidate}"
                )

    # Execution-shaped lines (a shell pipe, a fetch verb, an
    # installer/container/vcs command, or the project's own remote
    # extension installer) are copy-paste payload carriers, so they are
    # judged with a real shell tokenizer: shlex splits the line the way
    # the shell reading it would - adjacent quoting concatenates, so
    # `127.0.0.'1':8765/` is the token `127.0.0.1:8765/`, which the URL
    # rules refuse as a port. Rules: a fetch verb (curl, wget, git
    # clone) must yield at least one allowlisted destination and no
    # refused one - pipe or no pipe (a fetch whose destination is
    # hidden in substitution, encoding or quotes has nothing judged,
    # and refuses); command substitution or eval on an
    # execution-shaped line refuses outright; argument sources of the
    # project's installer flags (`--github owner/repo[@tag]`, `--url`,
    # `--from`) are judged - the GitHub shorthand's owner must be the
    # project's own; a pipe into a shell needs an allowlisted
    # destination on the line; anything else (an installer without a
    # destination, `pip install relate-db`) passes until it names a
    # destination. Plain lines are never audited.
    for line, _ in lines:
        if not EXEC_LINE.search(line):
            continue
        refused = (
            f"{label} carries an execution-shaped line with "
            f"unaudited destinations: {line.strip()}"
        )
        if SUBSTITUTION.search(line):
            findings.append(refused)
            continue
        try:
            tokens = shlex.split(line)
        except ValueError:
            # Unbalanced quoting: not a line a reader could run as
            # written, and not one the guard can audit.
            findings.append(refused)
            continue
        source_refused = False
        # Routing and verification flags refuse outright, whatever form
        # they take: `--resolve=host:port:host` and `--connect-to` send
        # the request to a host of the attacker's choosing behind an
        # allowlisted literal URL, `--proxy` routes through one, and
        # `-k`/`--insecure` drops the certificate check that makes the
        # allowlist mean anything.
        for token in tokens:
            if token.split("=", 1)[0] in ROUTING_FLAGS:
                source_refused = True
        # Every argument source is judged, space form and `=` form
        # alike: `--github owner/repo[@tag]` is a GitHub shorthand -
        # the owner must be the project's own; `--url` and `--from`
        # carry a full source URL; any other `flag=value` whose value
        # is destination-shaped (`--index-url=host/path`) has that
        # value judged like any other destination. A source flag with
        # no argument refuses: the line is incomplete, not auditable.
        for index, token in enumerate(tokens):
            flag, _, value = token.partition("=")
            if flag == "--github":
                source = value or (
                    tokens[index + 1] if index + 1 < len(tokens) else ""
                )
                owner = source.split("/", 1)[0].split("@", 1)[0]
                if owner != "relatedb":
                    source_refused = True
            elif flag in SOURCE_FLAGS:
                source = value or (
                    tokens[index + 1] if index + 1 < len(tokens) else ""
                )
                if url_verdict(source) != ALLOWED:
                    source_refused = True
            elif value:
                # Any other flag value: scheme-form URLs are judged; a
                # bare host shape is not, because a plain filename
                # (`-o=install.sh`) is domain-shaped without being a
                # destination - source flags are where hosts ride.
                for candidate in URL_CANDIDATE.finditer(value):
                    if url_verdict(candidate.group(0)) != ALLOWED:
                        source_refused = True
        destinations = [
            token
            for token in tokens
            if URL_CANDIDATE.match(token) or BARE_DESTINATION.match(token)
        ]
        verdicts = [url_verdict(token) for token in destinations]
        has_allowed = ALLOWED in verdicts
        has_refused = any(v != ALLOWED for v in verdicts)
        fetch = FETCH_VERB.search(" ".join(tokens))
        if (
            source_refused
            or has_refused
            or (fetch and not has_allowed)
            or (PIPE_TO_SHELL.search(line) and not has_allowed)
        ):
            findings.append(refused)

    if fence_aware:
        # Encodable material outside code fences: refused by presence,
        # never decoded.
        for line, fenced in lines:
            if fenced:
                continue
            for match in ENCODED_MATERIAL.finditer(line):
                findings.append(
                    f"{label} carries entity-encoded or escaped material "
                    f"outside a code fence: {match.group(0)}"
                )
            # Raw HTML tags outside code fences, any tag name. The
            # exemption is the real angle-autolink form only: `<https`
            # must continue as `://`, else the token is a tag or an
            # email-style autolink (which renders as a clickable
            # mailto, off-allowlist) - never exempt, always refused.
            for match in RAW_TAG.finditer(line):
                token = match.group(0)
                is_autolink = (
                    re.fullmatch(r"<\s*https?", token, re.IGNORECASE)
                    and line[match.end():].startswith("://")
                )
                if not is_autolink:
                    findings.append(
                        f"{label} carries a raw HTML tag outside a code fence: "
                        f"{token}"
                    )

        # Protocol-relative destinations resolve against whatever serves
        # the page; our links are absolute. A document-scope sweep over
        # the joined unfenced text - a destination on the line after
        # the `](` crosses the per-line view, and a reference
        # definition may put its destination on the next line after the
        # colon - computed once, one finding per destination.
        outside_text = "\n".join(
            line for line, fenced in lines if not fenced
        )
        outside_lines = outside_text.splitlines()
        for match in PROTO_RELATIVE.finditer(outside_text):
            line_number = outside_text.count("\n", 0, match.start())
            findings.append(
                f"{label} carries a protocol-relative link destination: "
                f"{outside_lines[line_number].strip()}"
            )

        # Empty sections, as one walk with a pending heading: a heading
        # whose body carries no content before the next unfenced heading
        # of its own level or shallower renders as an empty section on
        # the page. Any non-blank line is content - a fenced example
        # body included, so a heading whose only body is a code block is
        # not empty. A deeper unfenced heading makes the pending one a
        # container (its body is subsections), not empty. A heading
        # inside a fence is example text: content, never a section.
        def empty_section(heading: str) -> None:
            findings.append(
                f"{label} carries an empty section: {heading.strip()}"
            )

        pending: str | None = None
        pending_level = 0
        for line, fenced in lines:
            level = None if fenced else heading_level(line)
            if pending is not None:
                if level is not None:
                    if level <= pending_level:
                        empty_section(pending)
                    # Deeper: the pending heading is a container.
                    pending, pending_level = line, level
                elif line.strip():
                    pending = None  # content, fenced bodies included
            elif level is not None:
                pending, pending_level = line, level
        if pending is not None:
            empty_section(pending)

    for match in INTERNAL_REF.finditer(text):
        findings.append(f"{label} carries an internal reference: {match.group(0)}")
    return findings


def iter_strings(node: object) -> list[str]:
    """Every string value in a decoded JSON document, recursively."""
    if isinstance(node, str):
        return [node]
    if isinstance(node, list):
        return [s for item in node for s in iter_strings(item)]
    if isinstance(node, dict):
        return [s for value in node.values() for s in iter_strings(value)]
    return []


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="The release-notes output-boundary guard (ADR-0618)"
    )
    parser.add_argument("--notes", required=True, type=Path)
    parser.add_argument("--payload", type=Path, default=None)
    args = parser.parse_args(argv)

    notes = args.notes
    if not notes.is_file():
        print(f"::error::release-notes-validate: '{notes}' does not exist", file=sys.stderr)
        return 1
    notes_text = notes.read_text()
    if not notes_text.strip():
        print(f"::error::release-notes-validate: '{notes}' is empty", file=sys.stderr)
        return 1

    findings = check_text("notes", notes_text, fence_aware=True)

    if args.payload is not None and args.payload.is_file() and args.payload.stat().st_size > 0:
        try:
            payload = json.loads(args.payload.read_text())
        except json.JSONDecodeError as exc:
            print(
                f"::error::release-notes-validate: Slack payload is not valid JSON: {exc}",
                file=sys.stderr,
            )
            return 1
        for string in iter_strings(payload):
            findings.extend(check_text("Slack payload", string, fence_aware=False))

    for finding in findings:
        print(f"::error::release-notes-validate: {finding}", file=sys.stderr)
    if findings:
        print(
            f"::error::release-notes-validate: {len(findings)} boundary "
            "violation(s) — refusing to publish",
            file=sys.stderr,
        )
        return 1
    print(
        "release-notes-validate: output boundary clean "
        "(URLs parsed and allowlisted, no encodable material outside "
        "fences, no raw HTML, no internal references)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
