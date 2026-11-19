---
type: adr
area: infra
claim: e5b737a4ed31936d17d70c1dedb94ef86d3eaa62
name: 0605-third-party-license-gate-for-binary-releases
status: proposed
tags:
  - licensing
  - release
title: Binary releases carry their third-party notices and pass a license allow-list
REFERENCES:
  - "[[adr/0055-apache-2-license]]"
---

# ADR-0605: Binary releases carry their third-party notices and pass a license allow-list

## Context

Every binary Relate ships — the CLI, the C ABI library under the
JavaScript, Java, Go and Swift clients, the Python wheels and the WASM
bundle — statically includes third-party Rust crates. Their licenses
require attribution to travel with the binaries, and the project is
Apache-2.0 ([ADR-0055](./0055-apache-2-license.md)), so a dependency under a license that conflicts with
that must not slip into a release unnoticed. Nothing checked either: the
closure was never inventoried, and no notices file existed.

## Decision

- **What ships is the closure of four crates**: the CLI, the C ABI, the
  Python extension and the WASM bundle, over normal and build dependencies.
  Dev-dependencies and the testing group never reach a binary and are out
  of scope. The other clients wrap the C ABI and bundle no further code.
- **A committed notices file.** The repository root carries a third-party
  notices file generated from that closure — license text grouped by
  distinct text, each crate listed under its license. It is regenerated on
  any dependency change, drift is reported at release time, and the release
  attaches it beside the binaries.
- **A license allow-list.** One policy file lists the permissive,
  Apache-2.0-compatible licenses the closure uses today; the notices
  generator's accepted list mirrors it. A new license is a deliberate edit
  to both, never an accident, and copyleft or non-commercial terms go to
  the maintainers first.
- **One tool per job.** A license-policy checker enforces the allow-list; an
  attribution generator builds the notices. One script drives both over the
  four manifests, since each tool reads one manifest at a time.
- **A reusable workflow, early in the release.** It runs in parallel with
  the version bump and can be dispatched alone as a dry run. It is
  report-only at first: violations and drift are warnings, a tool that
  cannot run is a failure. Enforcement is one switch — pass the flag and make
  the release's tag step depend on the job — taken once a few releases show
  the check never fails spuriously.

## Alternatives considered

- **A single workspace-wide run.** Simpler, but it pulls in the testing
  group's dependencies and over-reports what ships. Rejected: the notices
  would claim code no binary contains.
- **Hand-maintained notices.** No tooling dependency, but it drifts with
  every lockfile change and nothing proves completeness. Rejected.
- **Enforcing from day one.** Catches violations sooner, but an unproven
  gate that blocks a release on a tooling quirk costs more than a few
  report-only releases. Rejected for now.
- **One tool for both policy and notices.** Neither tool does both
  well: the policy checker does not emit license texts, and the generator
  does not model policy exceptions. Rejected.

## Consequences

- Releases ship a notices file and are checked against an explicit license
  allow-list; the closure is reviewable at any time.
- Two pinned tools are installed in the release workflow; their versions
  are bumped deliberately.
- Contributors who change a shipped crate's dependencies regenerate the
  notices file in the same change.
- Until enforcement is switched on, a violating license is a warning that a
  maintainer must act on; the gate does not hold a release back.
