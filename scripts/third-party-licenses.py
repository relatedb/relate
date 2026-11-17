#!/usr/bin/env python3
"""Third-party license tooling for the binary releases (LABS-1690).

Every Relate binary release is the Rust closure of four shipped crates —
the CLI, the C ABI (the native library under the JS, Java, Go and Swift
clients), the Python extension, and the WASM bundle — so those four
manifests define "what goes into a release". Dev-dependencies and testing/*
crates are out of scope.

Subcommands:
  notices [--write|--check]   build THIRDPARTY-NOTICES.md from the union of
                              the four cargo-about runs; --write updates the
                              committed file, --check reports drift (default:
                              print to stdout)
  policy  [--enforce]         run cargo-deny's license check per shipped
                              manifest against deny.toml; violations are
                              warnings unless --enforce
  all     [--enforce]         notices --check, then policy

The accepted-license vocabulary is spelled once, in deny.toml's
`[licenses] allow` — the list the policy step enforces. The notices step
hands cargo-about a config carrying the same list derived from it
(`about_config`), so no second copy exists to drift.

Exit status: 0 unless --enforce is set and a license violates the policy, or
the tooling itself failed (a missing tool or an unreadable run is never a
silent pass). Drift in the committed notices is always a warning.
Requires Python 3.11+ (the stdlib TOML reader) and cargo-about and cargo-deny
on PATH.
"""
import argparse
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass, field
from typing import NamedTuple

ROOT = pathlib.Path(__file__).resolve().parent.parent
NOTICES = ROOT / "THIRDPARTY-NOTICES.md"
ABOUT = ROOT / "about.toml"
DENY = ROOT / "deny.toml"
SHIPPED = [
    "surfaces/relate-cli",
    "surfaces/relate-ffi",
    "surfaces/relate-python",
    "surfaces/relate-wasm",
]


def warn(msg):
    prefix = "::warning::" if os.environ.get("GITHUB_ACTIONS") == "true" else "warning: "
    print(prefix + msg, file=sys.stderr)


def run(cmd, **kw):
    try:
        return subprocess.run(cmd, cwd=ROOT, text=True, capture_output=True, **kw)
    except FileNotFoundError:
        sys.exit(f"error: {cmd[0]} not found; install with `cargo install --locked {cmd[0]}`")


def about_json(manifest_dir, config, out):
    r = run(
        [
            "cargo", "about", "generate", "--config", str(config), "--locked",
            "--format", "json", "--manifest-path", f"{manifest_dir}/Cargo.toml",
            "-o", str(out),
        ]
    )
    if r.returncode != 0:
        sys.exit(f"error: cargo about failed for {manifest_dir}:\n{r.stderr}")
    return json.loads(out.read_text())


_PLACEHOLDER = re.compile(r"yyyy|\[name\]|<year>|<owner>|\{name\}", re.I)
# A copyright-holder line: the crate's own notice, kept under its license.
_COPYRIGHT_LINE = re.compile(r"Copyright\b|(?i:\(c\))\s+(?:\d|(?!You\b)[A-Z][a-z]+\b)|©")
TEXTS = ROOT / "scripts" / "license-texts"


def normalize_newlines(text):
    return text.replace("\r\n", "\n").replace("\r", "\n")


def is_copyright_line(line):
    return bool(_COPYRIGHT_LINE.match(line.strip()))


def _squash(text):
    return " ".join(text.split())


def copyright_notices(text):
    """The crate-specific copyright notices of a license text, each in its
    whitespace-squashed spelling — a notice's identity and the text shown
    for it are one value, so the result never depends on how its whitespace
    is spelled or on the order the lines arrive in (the template
    placeholders of the standard texts are not notices)."""
    found = set()
    for line in text.splitlines():
        # squashing is also the line-ending normalization: it eats every
        # whitespace run, a lone \r included
        line = _squash(line)
        if is_copyright_line(line) and not _PLACEHOLDER.search(line):
            found.add(line)
    return found


def canonical_text(texts):
    """The most common text of one license, compared whitespace-insensitively;
    ties go to the longest (most complete), then the lexicographically smallest."""
    groups = {}
    for t in texts:
        groups.setdefault(_squash(t), []).append(t)
    key = min(groups, key=lambda k: (-len(groups[k]), -len(k), k))
    return min(groups[key], key=lambda t: (-len(t), t))


def standard_text(lic_id):
    """The SPDX list's standard text for a license id (committed under
    scripts/license-texts/, refreshed from github.com/spdx/license-list-data
    when the allow-list gains a license), or None when there is none.

    None is the one value for "no standard text": a missing committed file,
    an empty one, and a compound expression like
    `Apache-2.0 WITH LLVM-exception` (no single standard text exists for it)
    all answer the same.
    """
    path = TEXTS / f"{lic_id}.txt"
    if not path.is_file():
        return None
    return normalize_newlines(path.read_bytes().decode("utf-8")) or None


class CrateEntry(NamedTuple):
    """One crate using a license: its identity, its repository (None when
    the crate declares none), and the copyright notices its own license
    texts carry, sorted."""

    name: str
    version: str
    repo: str | None
    notices: tuple[str, ...]


class LicenseSection(NamedTuple):
    """One license's unit of the notices document: the id, the text shown
    once for it, and every crate that uses it."""

    lic_id: str
    text: str
    crates: tuple[CrateEntry, ...]


@dataclass
class _Crate:
    """One crate while its license entries are folded in: the repository it
    declares (None when it declares none) and the notices seen so far."""

    repo: str | None = None
    notices: set[str] = field(default_factory=set)


@dataclass
class _Grouping:
    """The mutable working state of one license id while its group entries
    are folded together: the texts its crates carry, and one record per
    crate."""

    texts: list[str] = field(default_factory=list)
    crates: dict[tuple[str, str], _Crate] = field(default_factory=dict)


def license_sections(groups):
    """The grouping step: {(license id, text): {(crate, version): repository
    or None}} -> the license sections, sorted by id.

    A license's text is its standard text when the committed SPDX list has
    one, else the most common of the texts its crates carry
    (`canonical_text`). A crate's notices are the copyright notices of every
    text it was seen under, keyed by their squashed form so one notice is one
    entry, and sorted.
    """
    by_id = {}  # id -> _Grouping
    for (lic_id, text), bucket in groups.items():
        grouping = by_id.setdefault(lic_id, _Grouping())
        grouping.texts.append(text)
        notices = copyright_notices(text)
        for crate, repo in bucket.items():
            folded = grouping.crates.setdefault(crate, _Crate())
            # the first repository a crate declares wins, and None is the
            # only "declares none" — a later one never clears an earlier
            if folded.repo is None:
                folded.repo = repo
            folded.notices |= notices
    return [
        LicenseSection(
            lic_id,
            standard_text(lic_id) or normalize_newlines(canonical_text(grouping.texts)),
            tuple(
                CrateEntry(name, version, folded.repo, tuple(sorted(folded.notices)))
                for (name, version), folded in sorted(grouping.crates.items())
            ),
        )
        for lic_id, grouping in sorted(by_id.items())
    ]


def format_sections(sections):
    """The pure formatter: the license sections as the THIRDPARTY-NOTICES.md
    body. Everything it prints is on the sections — nothing is derived from
    the raw groups here."""
    crates = {(c.name, c.version) for section in sections for c in section.crates}
    out = [
        "# Third-party notices",
        "",
        "Relate's binary releases (the `relate` CLI, the C ABI native library used",
        "by the JavaScript, Java, Go and Swift clients, the Python wheels, and the",
        "WASM bundle) include the third-party Rust crates listed below. Each is",
        "distributed under the license shown; the license text follows its list",
        "of crates, with the copyright notices each crate carries.",
        "",
        "Generated by `scripts/third-party-licenses.py notices --write`; do not",
        "edit by hand. See RELEASING.md.",
        "",
        f"{len(crates)} crates under {len(sections)} licenses.",
        "",
    ]
    for section in sections:
        out += [f"## {section.lic_id}", "", f"Used by {len(section.crates)} crates:", ""]
        for crate in section.crates:
            repo = f" ({crate.repo})" if crate.repo is not None else ""
            out.append(f"- {crate.name} {crate.version}{repo}")
            out += [f"  - {notice}" for notice in crate.notices]
        out += ["", "```text", section.text.strip("\n"), "```", ""]
    return "\n".join(out).rstrip("\n") + "\n"


def notices_body(groups):
    """The notices document body for a cargo-about grouping: the grouping
    step then the formatter. The one composition of the two — the generator
    calls it, and so do the tests, so a step added here cannot leave them
    testing a stale pipeline."""
    return format_sections(license_sections(groups))


def build_notices():
    groups = {}  # (license id, text) -> {(crate, version): repository or None}
    with tempfile.TemporaryDirectory() as tmp:
        config = pathlib.Path(tmp) / "about.toml"
        config.write_text(about_config())
        for i, manifest_dir in enumerate(SHIPPED):
            data = about_json(manifest_dir, config, pathlib.Path(tmp) / f"{i}.json")
            for lic in data["licenses"]:
                bucket = groups.setdefault((lic["id"], lic["text"]), {})
                for used in lic["used_by"]:
                    c = used["crate"]
                    bucket[(c["name"], c["version"])] = c.get("repository")
    return notices_body(groups)


def deny_allow_list():
    """deny.toml's `[licenses] allow` list, read structurally through the
    stdlib TOML reader — no bespoke array parser to mis-truncate the list."""
    try:
        return tomllib.loads(DENY.read_text())["licenses"]["allow"]
    except (tomllib.TOMLDecodeError, KeyError) as exc:
        sys.exit(f"error: deny.toml carries no `[licenses] allow` list: {exc}")


def about_config():
    """cargo-about's config for one run: about.toml with the `accepted`
    license list derived from deny.toml's `[licenses] allow` appended.

    cargo-about and cargo-deny each read their policy from a file, so the
    vocabulary is spelled once — in deny.toml, the list the policy step
    enforces — and this is the one derivation of cargo-about's copy.
    about.toml carries only the settings cargo-about alone needs.

    The derived line goes ahead of about.toml's text, so it is a root key
    whatever tables the document carries after it, and the combined document
    is parsed here: a base document holding an `accepted` of its own fails
    to parse rather than silently deciding which list cargo-about sees.
    """
    accepted = ", ".join(f'"{lic}"' for lic in deny_allow_list())
    config = f"accepted = [{accepted}]\n" + ABOUT.read_text()
    try:
        tomllib.loads(config)
    except tomllib.TOMLDecodeError as exc:
        sys.exit(f"error: the generated cargo-about config does not parse: {exc}")
    return config


def notices_drift():
    """Warn when the committed notices differ from the generated ones."""
    current = NOTICES.read_bytes().decode() if NOTICES.exists() else ""
    if current == build_notices():
        print("THIRDPARTY-NOTICES.md is current")
        return False
    warn(
        "THIRDPARTY-NOTICES.md is out of date with the shipped dependency "
        "closure: run `python3 scripts/third-party-licenses.py notices --write` "
        "and commit it"
    )
    return True


def cmd_notices(args):
    if args.check:
        return 1 if notices_drift() else 0
    text = build_notices()
    if args.write:
        NOTICES.write_bytes(text.encode())
        print(f"wrote {NOTICES.relative_to(ROOT)}")
    else:
        print(text, end="")
    return 0


def cmd_policy(args):
    bad = 0
    for manifest_dir in SHIPPED:
        r = run(
            [
                # dev crates never ship (shipped-license policy); per-invocation
                # — a config-file exclusion would narrow the advisories scan —
                # see deny.toml's no-[graph] note for the full rationale.
                "cargo", "deny", "--manifest-path", f"{manifest_dir}/Cargo.toml",
                "--config", "deny.toml", "--exclude-dev", "check", "licenses",
            ]
        )
        if r.returncode == 0:
            print(f"licenses ok: {manifest_dir}")
            continue
        bad += 1
        print(r.stderr[-4000:], file=sys.stderr)
        warn(f"license policy violation in the {manifest_dir} closure (see deny.toml)")
    if bad and args.enforce:
        return 1
    return 0


def cmd_all(args):
    notices_drift()  # drift is always a warning, never a failure
    return cmd_policy(args)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    n = sub.add_parser("notices")
    g = n.add_mutually_exclusive_group()
    g.add_argument("--write", action="store_true")
    g.add_argument("--check", action="store_true")
    sub.add_parser("policy").add_argument("--enforce", action="store_true")
    sub.add_parser("all").add_argument("--enforce", action="store_true")
    args = ap.parse_args()
    handlers = {"notices": cmd_notices, "policy": cmd_policy, "all": cmd_all}
    return handlers[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
