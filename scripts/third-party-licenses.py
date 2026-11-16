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

Exit status: 0 unless --enforce is set and a license violates the policy, or
the tooling itself failed (a missing tool or an unreadable run is never a
silent pass). Drift in the committed notices is always a warning.
Requires cargo-about and cargo-deny on PATH.
"""
import argparse
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
NOTICES = ROOT / "THIRDPARTY-NOTICES.md"
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


def about_json(manifest_dir, out):
    r = run(
        [
            "cargo", "about", "generate", "--config", "about.toml", "--locked",
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


def copyright_lines(text):
    """The crate-specific copyright lines of a license text (the template
    placeholders of the standard texts are not notices)."""
    found = []
    for line in normalize_newlines(text).split("\n"):
        line = line.strip()
        if is_copyright_line(line) and not _PLACEHOLDER.search(line) and line not in found:
            found.append(line)
    return found


def _squash(text):
    return " ".join(text.split())


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
    when the allow-list gains a license), or None."""
    path = TEXTS / f"{lic_id}.txt"
    return normalize_newlines(path.read_bytes().decode("utf-8")) if path.is_file() else None


def render(groups):
    """Markdown for {(license id, text): {(crate, version): repository}}:
    one section per license id with its text once and every crate that uses
    it, each with the copyright notices found in its own license file."""
    by_id = {}  # id -> {"std", "texts", "crates": {(name, version): (repo, [notices])}}
    for (lic_id, text), bucket in groups.items():
        if lic_id not in by_id:
            by_id[lic_id] = {"std": standard_text(lic_id), "texts": [], "crates": {}}
        entry = by_id[lic_id]
        entry["texts"].append(text)
        notices = copyright_lines(text)
        for key, repo in bucket.items():
            old_repo, old = entry["crates"].get(key, ("", []))
            entry["crates"][key] = (repo or old_repo, old + [n for n in notices if n not in old])
    crates = {k for e in by_id.values() for k in e["crates"]}
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
        f"{len(crates)} crates under {len(by_id)} licenses.",
        "",
    ]
    for lic_id in sorted(by_id):
        entry = by_id[lic_id]
        out += [f"## {lic_id}", "", f"Used by {len(entry['crates'])} crates:", ""]
        for (name, version), (repo, notices) in sorted(entry["crates"].items()):
            out.append(f"- {name} {version}" + (f" ({repo})" if repo else ""))
            out += [f"  - {n}" for n in sorted(notices)]
        text = entry["std"] or normalize_newlines(canonical_text(entry["texts"]))
        out += ["", "```text", text.strip("\n"), "```", ""]
    return "\n".join(out).rstrip("\n") + "\n"


def build_notices():
    groups = {}  # (license id, text) -> {(crate, version): repository}
    with tempfile.TemporaryDirectory() as tmp:
        for i, manifest_dir in enumerate(SHIPPED):
            data = about_json(manifest_dir, pathlib.Path(tmp) / f"{i}.json")
            for lic in data["licenses"]:
                bucket = groups.setdefault((lic["id"], lic["text"]), {})
                for used in lic["used_by"]:
                    c = used["crate"]
                    bucket[(c["name"], c["version"])] = c.get("repository") or ""
    return render(groups)



def license_list(path, key):
    """The quoted entries of `key = [ ... ]` in a flat TOML file."""
    m = re.search(rf"^{key}\s*=\s*\[(.*?)\]", (ROOT / path).read_text(), re.S | re.M)
    if not m:
        sys.exit(f"error: no `{key}` array in {path}")
    return sorted(re.findall(r'"([^"]+)"', m.group(1)))


def check_lists_agree():
    """about.toml's accepted list must equal deny.toml's allow list."""
    accepted, allow = license_list("about.toml", "accepted"), license_list("deny.toml", "allow")
    if accepted != allow:
        sys.exit(
            "error: about.toml `accepted` and deny.toml `allow` differ: "
            f"{sorted(set(accepted) ^ set(allow))}"
        )


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
    check_lists_agree()
    bad = 0
    for manifest_dir in SHIPPED:
        r = run(
            [
                # --exclude-dev scopes the dev-dependency exclusion to this
                # licenses invocation alone (shipped-license policy: dev
                # crates never ship): deny.toml deliberately carries no
                # [graph] exclude-dev, because a config-file exclusion
                # would also narrow the weekly advisories scan, which must
                # cover the full lock — CI toolchain dev-dependencies are
                # exactly the build-pipeline supply-chain surface it
                # exists to scan.
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
