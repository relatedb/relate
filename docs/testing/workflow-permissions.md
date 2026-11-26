# Workflow permissions: the post-merge repository-default flip

Runbook for the one half of the workflow-permissions hardening that lives
outside the repository: flipping the repository-wide default granted to the
`GITHUB_TOKEN` to read-only, after the change that pins every scope
in-file has landed. The in-file half is done — every workflow under
`.github/workflows/` now pins its token scopes explicitly, from the
workflow-level read-only default in
[`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) (whose
"Read-only token default" comment records that nothing in that workflow
writes repo contents) to the per-job write blocks of the release chain.
The one deliberate exception — the reusable review workflow, which runs on
its callers' grants — is covered in the pre-flip checklist below. The flip
makes the repository default match those in-file blocks, so the fallback
for any future workflow that forgets one is read-only, not read-write.

The flip is deliberately **not** performed by that change. No workflow can
change repository settings — the `GITHUB_TOKEN` has no admin scope on the
settings API — and automating it would require a stored admin credential,
the ambient-credential shape this hardening exists to remove. It is a
maintainer's post-merge step: performed once, by hand, after the branch
merges. This page is the procedure.

## The flip

Setting path (repository admin, web UI):

> Settings → Actions → General → Workflow permissions →
> **Read repository contents** (the restricted option: read access to the
> `contents` and `packages` scopes only) → Save

The equivalent API call — the dedicated endpoint for exactly this pair of
settings:

```console
$ gh api -X PUT repos/relatedb/relate/actions/permissions/workflow \
    -f default_workflow_permissions=read \
    -F can_approve_pull_request_reviews=false
```

Read the state back before and after with the GET form of the same
endpoint:

```console
$ gh api repos/relatedb/relate/actions/permissions/workflow
{"default_workflow_permissions":"read","can_approve_pull_request_reviews":false}
```

If the read already says `read`, there is nothing to flip.
`can_approve_pull_request_reviews` stays `false`: the workflows that write
to PRs post comments (inline review comments, summary comments), never
approvals.

The restricted default grants the `GITHUB_TOKEN` only `contents: read` and
`packages: read` (plus the always-present `metadata: read`). It is a
**fallback, not a cap**: a workflow's or job's own `permissions:` block
still adjusts the token, first at workflow level, then at job level —
which is why pinning every scope in-file first makes the flip
behavior-neutral for everything already in the fleet.

### The organization-default consideration

The same setting exists one level up, under the organization (organization
owners; web UI: organization Settings → Actions → General → Workflow
permissions, or `gh api -X PUT orgs/<org>/actions/permissions/workflow`
with the same two fields). Its semantics shape the decision:

- An organization that selects the read-only default **cascades it**: every
  member repository shows the same option selected with the permissive
  option disabled, and repositories created in the organization inherit
  the organization's setting.
- A repository-level `PUT …/actions/permissions/workflow` fails with
  **409 Conflict** when the owning organization's policy prevents the
  change — a 409 there means the organization default already governs this
  repository. Read the organization state
  (`gh api orgs/<org>/actions/permissions/workflow`) before flipping
  either level.

For this repository the flip is self-contained at either level: with every
scope pinned in-file, the default feeds nothing after the branch, so the
repository-level flip stands on its own. The organization-level default is
the stronger posture — it covers every other repository in the
organization and disables their permissive option — and can follow its own
timeline; checking the organization's current default first makes the
repository flip a decision rather than a duplicate.

## Pre-flip checklist

**Nothing floats on the default.** After the branch, exactly one workflow
file carries no `permissions:` block at all. Verify with
`grep -L 'permissions:' .github/workflows/*.yml` — it must list only
`opencode-review-reusable.yml`. Every other workflow pins a workflow-level
default, and the jobs that need more carry their own blocks: in
`.github/workflows/ci.yml` the five job-level blocks are `adr-numbers`
(`contents: read`), `query-latency` (`actions: read`, `contents: read`),
and the three PR-comment jobs — `loadtest-baselines`, `memory-controls`,
`memory-shape` — which add `pull-requests: write` for the comparison
comment.

**The review lane — the ambient-permissions exception.** The four
review-lane workflows are the one part of the fleet that deliberately
still runs on ambient — caller-granted — permissions:

| Workflow | Permissions |
| --- | --- |
| `opencode-review-reusable.yml` | None in the file, by design. Its header records that trigger, draft/same-repo if-guard, concurrency, and permissions all live in the callers, because a called workflow can never exceed the caller job's permissions — a block here would be dead text. The lane's token is exactly what the calling job grants. |
| `opencode-review-stellar.yml` | Single calling job pinned: `contents: read` (checkout and the scripted endpoint fetches), `pull-requests: write` (inline review comments on the diff), `issues: write` (the PR summary comment lives on the issues API). |
| `opencode-review-rust.yml` | Same single-job pin as the stellar caller. |
| `opencode-review-security.yml` | Same single-job pin as the stellar caller. |

Verify before flipping that each caller still carries that job-level
block. A caller that loses it puts the reusable's posting steps — the ones
that set `GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}` — on whatever the
repository default is, which is the one place the flip could reach the
review lane. The model step itself is tokenless (`GH_TOKEN: none`,
[ADR-0603](../adr/0603-tokenless-model-scripted-posting.md)) and cannot
feel the flip either way.

**The other model-running workflow.** `release-notes.yml` runs the model
inline (`opencode run` for note generation) rather than through the
reusable, and pins its own workflow-level `contents: write` — its
`gh release edit` step needs it. It declares its `workflow_call` secrets
explicitly (the least-privilege shape the reusable workflow set) and is
watched with the release chain below.

**Read back the current state** of both the repository and the
organization defaults (the GET endpoints above) before touching either.

## Post-flip watch items

The scheduled and dispatch-only workflows never ran the branch's changes on
its pull request — scheduled triggers fire on the default branch only, and
the release chain is manual dispatch — so their first post-merge runs are
also their first runs under the read-only default. (A `workflow_dispatch`
run against the feature branch can exercise them early; see
[cadences.md](cadences.md).) Watch the first live run of each:

| Group | Workflows (schedule, first live run post-merge) |
| --- | --- |
| Nightly | `proptest-nightly.yml` (03:00 UTC), `graph-examples-nightly.yml` (03:17 UTC), `background-cadences.yml` nightly legs (03:30 UTC), `jepsen.yml` (04:50 UTC), `dependency-advisories.yml` (05:00 UTC; its `report` job pins `issues: write`) |
| Cadence | `stellar-nightshift.yml` (hourly, 01:00–06:00 UTC), `background-cadences.yml` 6-hourly legs (:17) |
| Release chain | `release.yml` (next dispatch; chains `release-notes.yml`, and optionally `publish-registries.yml`, whose default path is a direct dispatch against the tag), `release-extensions.yml` (next package release; chains `publish-extensions-json.yml`), `publish-releases-json.yml` |

What a first-run failure means:

- **HTTP 403 "Resource not accessible by integration"** — a write scope
  that relied on the default: a missed in-file block. The fix is to pin
  that scope on the job in its workflow file (the convention the branch
  established), not to revert the flip — the census above says no existing
  job depends on the default, so a 403 is a bug to pin, not a posture to
  restore. The rollback exists (the same `PUT` with
  `default_workflow_permissions=write`, or the UI option) and is the last
  resort.
- **A failure without the 403 shape** — a pinned action failing to
  resolve, an installer digest mismatch, the deny-expiry gate: a hardening
  regression surfaced by the first live run, not a permissions problem.
  The cadence contract applies — a red cadence lane files a ticket, it
  never blocks a PR ([cadences.md](cadences.md)).

On the next release, watch the write-scoped jobs specifically: tag and
release creation (the `contents: write` jobs of `release.yml` and
`release-extensions.yml`), OIDC trusted publishing (the `id-token: write`
jobs of `publish-registries.yml`), the signed channel index
(`publish-extensions-json.yml`, `contents: write`), and the releases JSON
(`publish-releases-json.yml`, `contents: write`).
