# ADR-0330: The release run deploys the website after its gh-pages writes

Status: Accepted
Claim: ffc4b83048d2719965934ac977eb3cfff880949d
Date: 2026-10-16
Deciders: Oskar Hane (agreed in conversation), CypherLite maintainers (Linear LABS-325)

Later references: ADR-0546 supersedes this ADR in full — the website moved
to the relate-web repository; there is no gh-pages branch and no
Pages-deploy chain in this repository anymore.

## Context

The `gh-pages` branch is the source of the website, not what the website
serves. `.github/workflows/deploy-pages.yml` builds the live site from three
sources (the `gh-pages` tree, the playground built from `main`, and the
cookbook built from the latest release tag, ADR-0130) and deploys it with
`actions/deploy-pages`. A commit to `gh-pages` goes live only when that
workflow runs next.

A release writes `gh-pages` from three jobs in `.github/workflows/release.yml`:
`docs` (install.sh, ADR-0068), `update-website` (the agent-edited page,
ADR-0129) and `publish-releases-json` (releases.json, ADR-0250).
`deploy-pages.yml` triggers on three events, and none of them fires during a
release:

- **Pushes to `main`.** The release's only push to `main` is the
  version-bump commit, made with the default `GITHUB_TOKEN`. GitHub starts
  no workflows for `GITHUB_TOKEN`-triggered events (ADR-0265).
- **`workflow_run` of "Update website".** ADR-0129 turned
  `update-website.yml` into a reusable workflow called inside the "Release"
  run. A called workflow creates no run of its own, so this trigger fires
  only when "Update website" is dispatched by hand.
- **Manual `workflow_dispatch`.**

A release's website changes therefore went live only with the next human
merge to `main`. That covers the install.sh sync, the page update, the
cookbook at the new tag, and now `releases.json`. The gap was hidden because
`main` usually moves within hours. ADR-0250 §1 assumed `deploy-pages.yml`
needed no change. That assumption broke the update path at v0.12.4:
`releases.json` was committed to `gh-pages`, but
`https://neo4j-labs.github.io/cypherlite/releases.json` returned 404, so
`cypherlite upgrade` / `update` failed until Pages was dispatched by hand.

The same release exposed a separate bug, fixed in the same change without an
ADR. The three writers raced each other's `git push` to `gh-pages`. They now
push through `.github/scripts/push-gh-pages.sh`, which rebases onto a
concurrent writer's commit and retries.

## Decision

Chain the Pages deploy explicitly from the release run, using the
`workflow_call` pattern of ADR-0265 and ADR-0129:

1. `deploy-pages.yml` gains a `workflow_call` trigger. Its existing triggers
   stay: pushes to `main`, `workflow_run` of a manually dispatched "Update
   website", and `workflow_dispatch`.
2. `release.yml` gains a `deploy-pages` job that calls it with
   `needs: [create-release, docs, update-website, publish-releases-json]`,
   that is, after every job that writes `gh-pages`. The caller job grants
   `contents: read`, `pages: write` and `id-token: write`, the permissions
   `actions/deploy-pages` needs.
3. The job runs unless the release run was cancelled, and only if
   `create-release` succeeded:
   `if: ${{ !cancelled() && needs.create-release.result == 'success' }}`.
   The deploy copies whatever `gh-pages` holds. When one writer fails (for
   example, the website agent), the files the other writers published still
   go live. The failed writer still fails the release run.
4. The `pages` concurrency group moves from workflow level to job level in
   `deploy-pages.yml`. It then applies to the job the same way whether the
   workflow is called or run on its own, as with
   `publish-releases-json.yml`.

## Alternatives considered

- **Dispatch with `gh workflow run deploy-pages.yml` at the end of the
  release.** `workflow_dispatch` is exempt from the `GITHUB_TOKEN`
  suppression, so this would work. Rejected for the reasons ADR-0129 gave:
  the dispatch is fire-and-forget, the deploy is not part of the release
  run's graph or status, and the token needs `actions: write`.
- **Deploy from each `gh-pages` writer.** Rejected: one release would run
  three full site builds (wasm bundle, playground, cookbook). Two of them
  would deploy a partial `gh-pages`, and they would queue in the single
  `pages` slot, where GitHub keeps only one pending run and cancels the
  older one.
- **Serve Pages directly from the `gh-pages` branch.** Rejected: the site
  combines the playground and the cookbook (ADR-0130), which are built from
  `main` and from the latest tag. A branch-sourced Pages site cannot
  assemble them.
- **Keep relying on the next push to `main`.** Rejected: `releases.json` is
  the CLI's update path (ADR-0250), and it must be live when the release is.
  Waiting for an unrelated merge leaves `upgrade` broken for an unbounded
  time.

## Consequences

- When a release run finishes, its `releases.json`, install.sh, page update
  and cookbook (built at the new tag) are live with no manual step.
- The release run takes longer by one Pages build and deploy (wasm bundle,
  playground, cookbook), which runs after the website agent.
- The deploy builds the playground from the commit the release was
  dispatched on, the same rule as any dispatched deploy. The version-bump
  commit changes only version strings, which the playground does not show.
- Manual `update-website.yml` dispatches still deploy through
  `workflow_run`, and pushes to `main` still deploy directly.
- A **Later references** note is added to ADR-0129 and ADR-0250 per the
  decision-log rule.
