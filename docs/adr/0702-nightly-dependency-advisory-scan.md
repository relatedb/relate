---
type: adr
area: infra
claim: 355db835303aaeac21c707db44018d82bbf7b7b6
date: 2026-11-26
deciders: the maintainers
name: 0702-nightly-dependency-advisory-scan
status: proposed
tags:
  - ci
  - clients
  - security
title: One nightly advisory scan covers every dependency ecosystem the repository ships
REFERENCES:
  - "[[adr/0661-spec-harness-regression-gate]]"
---

# 0702. One nightly advisory scan covers every dependency ecosystem the repository ships

## Context

Relate ships one Rust engine and thin clients in Go, Java, .NET,
JavaScript, Swift, Python and WASM that load it through the C ABI or a
Rust-built extension module. The only advisory scan was a weekly
`cargo deny check advisories` against the repository-root `Cargo.lock`.
It saw nothing else: not the fuzz crate's separate lock, the npm locks,
the NuGet lock files, the Maven manifests (the Java client and the Bolt
driver smoke), the Go modules, or the Python extras. Advisory databases
move independently of the repository, so a weekly cadence left up to a
week between a published advisory and a red run — and a red scheduled
run alerted no one: the first one went unnoticed.

## Decision

**One scheduled workflow runs nightly and on manual dispatch, and fails
on any advisory that is not explicitly ignored.**

1. **cargo-deny owns Rust.** It scans both Cargo locks — the workspace
   and the fuzz crate — against RustSec, triaged in `deny.toml` as before.
2. **OSV-Scanner owns every other ecosystem.** One recursive scan of the
   checkout reads the npm, NuGet and Go lock files and resolves Maven
   manifests transitively. Crates are excluded from it by a package
   override, so a Rust finding has exactly one triage home.
3. **Python extras are resolved at scan time.** The bindings' native code
   is Rust; their only third-party Python dependencies are the optional
   extras, declared as version floors. The scan resolves them the way a
   fresh install would today and hands the pinned result to OSV-Scanner.
   No lock file is committed for a library whose users resolve their own
   environment.
4. **Every ignore carries a reason and an expiry.** `deny.toml` keeps its
   expiry-comment convention; `osv-scanner.toml` uses the scanner's own
   expiry date, after which the ignore stops applying by itself. Expiry
   is a re-triage prompt, never a standing accept.
5. **A failed scheduled run files one issue.** The alarm follows the
   one-open-issue rule of the spec gate's watchdog
   ([ADR-0661](./0661-spec-harness-regression-gate.md)): the first failure
   opens the issue, later failures comment on it while it stays open. A
   manual dispatch only goes red.
6. **Scanner binaries install pinned** — a fixed version and a SHA-256 per
   platform asset computed from the downloaded bytes — like every other
   host-side tool installer.

## Alternatives considered

- **Per-ecosystem native tools** (`npm audit`, `dotnet list package
  --vulnerable`, `govulncheck`, `pip-audit`, Maven dependency-check).
  Five installs, five output formats and five ignore files over the
  advisory data OSV already aggregates. Rejected for maintenance cost.
- **OSV-Scanner for Rust as well.** One tool fewer, but it matches every
  lock entry, while cargo-deny evaluates the resolved crate graph for the
  workspace's targets and features — OSV flags crates the build never
  compiles — and Rust triage would leave `deny.toml`, which the license
  gate shares. Rejected.
- **Container image scanning.** The shipped server image is built from
  scratch and holds the relate binary and a CA bundle; the binary's
  dependency closure is the Cargo lock this workflow already scans, and
  the digest-pinned build-stage base images are kept current by
  Dependabot. Scanning the published image would add a registry
  credential path to a nightly job for no new coverage. Rejected.
- **Dependabot security alerts alone.** No ignore-with-expiry triage, no
  resolved Python extras, and alerts land in repository settings rather
  than the issue tracker. Kept as the update channel, not the gate.
- **Alerting only on advisories new since the last run.** Needs state
  carried between runs and hides long-standing unfixed advisories.
  Rejected; the ignore list is the baseline.

## Consequences

- The scan is red until every current finding is fixed or ignored;
  landing it means triaging the backlog it surfaces.
- Each run depends on external services — the RustSec database, OSV and
  deps.dev — so an outage reds a run without a new advisory. The issue
  links the run, so the cause is one click away.
- Maven results come from deps.dev's transitive resolution, not a
  committed lock; an upstream resolution change can surface or clear a
  finding with no repository change.
- The Python extras reflect the scanning interpreter's resolution; a
  user on an older interpreter can resolve versions the scan never sees.
- Dependabot also tracks the NuGet projects, the Dockerfiles' base
  images, the fuzz lock and the Bolt websocket example.
