# 0069. CLI lifecycle commands as positionals, plus self-upgrade design

- **Status**: Accepted
- **Date**: 2026-09-20
- **Deciders**: CypherLite maintainers (Linear LABS-26)

## Context

CypherLite’s CLI ships as raw release binaries on GitHub Releases (ADR-0067
fixes the release and versioning policy: manual dispatch, tag-after-build, on
`neo4j-labs/cypherlite`), and ADR-0068 added a `curl | bash` installer built on
the release asset contract (`cypherlite-<os>-<arch>` binaries plus a
`SHA256SUMS` manifest, `Authorization: Bearer $GITHUB_TOKEN` while the
repository is internal, macOS/Linux-only scope). Linear issue **LABS-26**
("CLI upgrade command",
<https://linear.app/neo4j/issue/LABS-26/cli-upgrade-command>) asks for the
next step: an in-binary `upgrade` command so installed users can move to the
latest release without re-running the installer.

The behavioral reference is a self-updating CLI tool's `update` command
of the same author: check GitHub releases for a newer version,
download and verify the checksum, atomically replace the running binary, and
then refresh the installed skill bundle with prompt/`--yes`/hint semantics.
This decision adopts that shape for CypherLite, adapted to this repository’s
raw-binary asset contract.

The CLI grammar is the second half of the motivation. The `cypherlite` CLI’s
argument parser is hand-rolled and has no subcommands today: the first
positional argument is the database path, and everything else is a flag. The
agent-skill installer was squeezed into that grammar as `cypherlite --skill
install [--agent NAME]` — a lifecycle command expressed as a flag plus value,
which is inconsistent with the rest of the surface (the value-less `--help`,
`--version`, and the path-first invocation). Adding `upgrade` as a second
flag-shaped command (`--upgrade`) would cement two grammars for one category
of behavior. The agreed grammar rule is that lifecycle verbs are positional
commands.

The decision below is already agreed; this ADR records it before it is
implemented, per the repository’s decision-log rule.

## Decision

Lifecycle verbs become positional commands, and `upgrade` is implemented as
one of them with the fetch/verify/replace design below.

1. **Positional lifecycle commands.** `skill`, `upgrade`, and `update` are
   recognized **only as the first argument** to the `cypherlite` CLI:
   - `cypherlite skill install [--agent NAME]`
   - `cypherlite skill remove [--agent NAME]`
   - `cypherlite upgrade [--yes] [--check]`
   - `cypherlite update` — an alias of `upgrade`.

   Database paths literally named `skill`, `upgrade`, or `update` stay
   reachable via a `./` prefix (e.g. `cypherlite ./skill`) or after `--`
   (e.g. `cypherlite -- skill`).
2. **The `--skill`/`-skill` and `--agent`/`-agent` flags are removed
   outright.** This is a breaking change. Because these flags never shipped
   in a release (they postdate the tree but predate 0.4.0, the first stable
   release), there is no installed base to migrate and therefore no
   migration-hint special-casing: the removed spellings hit the generic
   unknown-flag error like any other unrecognized argument.
3. **Upgrade fetch.** The command issues
   `GET https://api.github.com/repos/neo4j-labs/cypherlite/releases/latest`,
   attaching `Authorization: Bearer $GITHUB_TOKEN` only when the environment
   variable is set (the repository is currently internal; this is parity with
   `docs/static/install.sh`, ADR-0068). The release’s `tag_name` (leading `v`
   stripped) is semver-compared against the binary’s own `CARGO_PKG_VERSION`;
   if the latest release is not newer, the command prints an up-to-date
   message and exits 0.
4. **Asset resolution.** Asset ids are parsed from the release JSON with
   `serde_json` (no shell scraping). The platform binary
   `cypherlite-<os>-<arch>` and `SHA256SUMS` are downloaded via the GitHub API
   asset endpoint with an `Accept: application/octet-stream` header.
5. **Verification and dependencies.** The sha256 of the downloaded binary is
   verified against its `SHA256SUMS` entry using a **new** `sha2` workspace
   dependency. HTTP uses the **existing** workspace `ureq = "3.4"` dependency
   (already declared, previously unused). No other new dependencies are added.
6. **Atomic self-replace.** The verified binary is staged at a temporary path
   in `current_exe()`’s directory (guaranteeing the same filesystem), chmod
   755, then renamed over the running binary. This is macOS/Linux only, the
   same platform scope as the ADR-0068 installer; on Windows the command
   errors with a not-supported message pointing at the manual `.exe` download
   (a running `.exe` cannot be replaced).
7. **Skill refresh after a successful replace.** Agents are detected first;
   if none are detected the step is skipped silently. On an interactive TTY
   the command prompts `Update installed cypherlite skill? [Y/n]` with yes as
   the default; `--yes` auto-accepts; a non-interactive run without `--yes`
   prints a `run: cypherlite skill install` hint instead. On yes, the **new**
   binary is spawned as `cypherlite skill install` with inherited stdio, so
   the new embedded skill bundle is what gets installed (the old process only
   holds the old bundle).
8. **`--check` flag.** A read-only current-vs-latest report: it changes
   nothing on disk and exits 0.

## Alternatives considered

- **`--upgrade` flag form instead of a positional command** — rejected: it
  doubles down on the awkward command-as-flag shape that made `--skill`
  inconsistent with the rest of the CLI; the agreed grammar rule is that
  lifecycle verbs are positional.
- **Keep `--skill` as a deprecated alias shim** — rejected: 0.4.0 is the
  first stable release and the flag is days old, so there is effectively no
  installed base to protect; a shim would preserve two grammars for the same
  category indefinitely.
- **Windows side-by-side download (`cypherlite.new.exe` plus manual swap
  instructions)** — rejected: an untested surface with no reference
  implementation; ADR-0068 already scoped the installer to macOS/Linux, and
  the README’s manual `.exe` download covers Windows.
- **Printing migration hints for the removed flags** — rejected: the
  `--skill`/`--agent` flags never shipped in a release (this is unreleased
  software), so there is no installed base scripted against them to migrate;
  the generic unknown-flag error is enough.

## Consequences

- The CLI gains a positional-command grammar rule: the first positional
  argument is intercepted only when it is exactly `skill`, `upgrade`, or
  `update`. The query engine and the rest of the CLI surface are unchanged,
  and paths named after lifecycle verbs remain reachable via `./` or `--`.
- The breaking removal of `--skill`/`--agent` requires the CHANGELOG breaking
  entry and documentation updates in the same change: `docs/api/cli/spec.md`,
  `docs/docs/reference/cli-flags.md`, and the embedded skill bundle’s
  `references/cli.md` must all describe the positional form.
- The release asset contract (`cypherlite-<os>-<arch>` plus `SHA256SUMS`) is
  now load-bearing for a **third** consumer — the CLI itself, in addition to
  the README’s manual download table and the ADR-0068 installer. Changing
  asset names or the `SHA256SUMS` format now requires a new ADR referencing
  this one, ADR-0067, and ADR-0068.
- `GITHUB_TOKEN` becomes a documented environment variable of the CLI (used
  by `upgrade` while the repository is internal), matching the installer’s
  auth model; it becomes unnecessary but harmless once the repository is
  public.
- The upgrade flow depends on the GitHub releases API shape
  (`releases/latest`, `tag_name`, asset ids, the octet-stream asset
  endpoint). A change in that API or a move away from GitHub Releases
  requires a new ADR referencing this one.
- `ureq` moves from declared-but-unused to used, and `sha2` enters the
  workspace dependency set; both are pinned at the workspace level per the
  existing dependency convention.
- Self-replacement renames a file over the running binary, which is only
  sound on Unix-like filesystems; the Windows error path keeps Windows users
  on the manual `.exe` download, an intentional scope limit recorded here so
  it is not mistaken for an oversight.

## Later references

ADR-0261 refines the §7 skill-refresh step: `upgrade` pre-flight classifies
each detected agent's `<skills dir>/cypherlite` as Missing, Owned, Legacy, or
Foreign (installed = Owned or Legacy, where Legacy is recognized by its
`SKILL.md` frontmatter declaring `name: cypherlite` **and** carrying a
machine-written `version:` line; a `name: cypherlite` bundle with no version
line is Foreign). Installed targets are auto-updated
without a prompt; Foreign directories are reported and left untouched; only an
all-Missing set prompts to install, with `run: cypherlite skill install` as the
non-interactive hint. The rest of this decision, including the
positional-command grammar, is unchanged.

ADR-0115 strengthens the §5 verification step: the release publishes an
OpenSSH SSHSIG signature `SHA256SUMS.sig` over `SHA256SUMS` (Ed25519,
namespace `cypherlite-release`), and `upgrade` verifies it against a public key
pinned in `upgrade_cmd.rs` using the RustCrypto `ssh-key` crate before
checksumming the downloaded binary. Verification fails closed when the
signature asset is missing or invalid.

ADR-0190 adds `<command> --help` handling and scoped usage errors without
changing the positional-command grammar: `-h`/`--help` after a compiled-in
command word prints that command's help to stdout and exits 0, and a usage
error prints the error line, the command's synopsis, and a
`Run 'cypherlite <command> --help' for more information.` hint instead of the
full help text.

ADR-0200 adds `query` as a first-positional statement-session verb with
`--database`. It is not an exit-after lifecycle command: after parse it runs
the same oneshot / stdin / shell session as the path-first form.

ADR-0231 lets the bare form (no command word, no statement) open the
interactive shell when stdin is a terminal; the positional-command grammar
and the non-TTY help print are unchanged.

ADR-0250 supersedes the §3 fetch step and the §4 asset resolution of this
decision: `upgrade`/`update` reads `releases.json` from
`https://neo4j-labs.github.io/cypherlite/releases.json` and downloads assets
through the non-API route
`https://github.com/neo4j-labs/cypherlite/releases/download/{tag}/{asset}`.
The GitHub REST API leaves the update path entirely (no fallback, not even
when `GITHUB_TOKEN` is set), and `GITHUB_TOKEN` is reduced to an optional
`Authorization: Bearer` header on the asset downloads only — it is never
sent on the metadata request. The version comparison, the ADR-0115 SSHSIG
verification, the atomic self-replace, and the §7 skill refresh are
unchanged.

ADR-0586 changes §6's Windows refusal: it now points at the PowerShell
installer, `install.ps1`, instead of the manual `.exe` download, and is
raised only for the replace. `upgrade --check` downloads and replaces
nothing, so it runs on Windows too and carries the refusal as its advice
line. Self-replace on Windows remains out of scope.
