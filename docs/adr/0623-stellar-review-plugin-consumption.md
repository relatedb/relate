# ADR-0623: The stellar review pipeline is consumed from the stellar-review plugin

Status: Proposed

Claim: 8015b630b63b03de9b2f43fc0459786fcf6591b2

Date: 2026-11-19

Deciders: CypherLite maintainers (LABS-311)

## Context

ADR-0271's stellar review pipeline — the SKILL.md dispatcher, the four
pass agents, the pass mandates and doctrine, the Python tooling
(facts/units/similar/ledger/incremental/briefs/post), the
`stellar-prep.sh` entry point, and the CI prompt header — has lived
vendored in this repository since it was built here, duplicated across
`.agents/skills/stellar-review/`, `.claude/skills/` (a symlink view),
two harness agent directories (`.opencode/agents/`,
`.claude/agents/`, kept in sync by a test), `.github/scripts/`, and
`.github/prompts/`. Setting the same review up in any other repository
means hand-copying five locations plus CI wiring, and every consumer
must copy it again.

The pipeline is now an installable dual-harness plugin:
[`oskars.ai/plugins/stellar-review`](https://github.com/oskarhane/oskars.ai/tree/main/plugins/stellar-review)
— a Claude Code plugin (marketplace `stellar-review@oskars.ai`) and an
OpenCode plugin (npm `opencode-stellar-review`, published with npm
provenance from its own release workflow, gated on the tool test suite
this repository used to carry). The plugin generalizes three
repository-specific assumptions on the way out: `STELLAR_BASE_REF`
overrides the `origin/main` round-1 merge-base default, the LABS-311
recurring-pattern catalog became a default that repository code-shape
rules take precedence over, and per-pass model overrides are applied by
the skill's dispatcher from the `STELLAR_MODEL_*` environment instead of
generated agent config.

Two copies of one pipeline cannot both be canonical. This repository
keeps process machinery the plugin deliberately does not carry — the
night-shift family (`nightshift.py`, `linear_queue.py`, `passstats.py`,
`recurrence.py`; ADR-0292's planner and queue, Linear-integrated and
repository-specific) and the per-kind review lanes' own scripts
(ADR-0603), which import the stellar posting core.

## Decision

This repository consumes the pipeline from the plugin and deletes its
vendored copy. The plugin is canonical; drift is resolved by bumping
the pin, never by editing a copy.

- **Deletions**: `.agents/skills/stellar-review/` (skill, passes,
  references, core tools), the four `stellar-*` pass agents in
  `.opencode/agents/` and `.claude/agents/`, `.github/scripts/stellar-prep.sh`,
  and `.github/prompts/opencode-review-stellar.md`. The
  `.claude/skills` symlink view follows its target. The tool tests move
  with the plugin; `test_agents_sync.py` (two-harness agent copies) dies
  with the copies.
- **CI resolution, two installs of one pinned version**
  (`STELLAR_REVIEW_VERSION`, exact, bumped deliberately in lockstep
  across every workflow that spells it — the wiring test asserts they
  agree):
  - `npm install --prefix "$RUNNER_TEMP/stellar-review"` gives the
    shell steps a stable tools path, `STELLAR_ROOT`, for prep, posting
    and facts; and
  - `opencode plugin add opencode-stellar-review@<pin>` pre-seeds the
    CLI's plugin cache so the model step's `opencode run` loads the
    skill immediately instead of racing a background package install.
    Both lanes install: scripted runs the stellar pipeline; the
    per-kind lanes import the plugin's `facts.py`/`post.py` core.
- **The STELLAR_ROOT trust boundary**: published through `$GITHUB_ENV`
  as a fixed runner-authored value for the PRE-MODEL steps only — the
  post steps never trust that copy (the round-22 finding class: the
  model can append to the runner env file). Each post step pins
  `STELLAR_ROOT: ${{ runner.temp }}/stellar-review/node_modules/opencode-stellar-review`
  in its step env — the same fixed install path, from runner-provided
  values — and passes it through the `env -i` re-exec list.
- **Per-kind lanes (ADR-0603) unchanged in shape**:
  `opencode-review-prep.sh` and `opencode-review-post.py` stay
  repository scripts; their stellar dependency resolves through
  `STELLAR_ROOT` (prep: `T=`; poster: the imported
  `post`/`facts` core). `lib/review-endpoints.sh` stays — the per-kind
  prep and `review-restore.sh` source it; the plugin bundles its own
  copy, so the "one spelling" claim is now scoped per repository.
- **Per-pass model overrides (ADR-0302)**: the `pass_models.py`
  config-writer arm is gone with the vendored copy. The run step
  exports `STELLAR_MODEL_<PASS>` into the review run and the plugin's
  skill applies each non-empty value as that pass's sub-agent model
  (its SKILL.md Step 2). The environment contract, the
  secrets → vars → empty resolution order, and the run-level
  `OPENCODE_REVIEW_MODEL` are unchanged.
- **The stellar lane's CI prompt header ships with the plugin**
  (`templates/ci/opencode-review-stellar.md`); the reusable workflow
  cats it from `$STELLAR_ROOT`, one spelling, no repository copy to
  drift from.
- **The night-shift family is repository machinery, not the reviewer**:
  `nightshift.py`, `linear_queue.py`, `passstats.py` and `recurrence.py`
  move to `.github/scripts/stellar-nightshift/` with their tests; the
  tests (and the per-kind poster test) resolve the plugin's pipeline
  modules through `STELLAR_ROOT`, and skip with a stated reason when it
  is unset. `stellar-nightshift.yml` installs the pinned package in the
  two jobs that call `post.py fetch-ledger` (plan, recurrence).
- **Local rounds** require the plugin installed —
  `opencode plugin add opencode-stellar-review` /
  `claude plugin install stellar-review@oskars.ai` — the same
  user-level pattern gbuild already uses here; no project-level plugin
  config is added, so a PR cannot steer which plugins CI loads. The
  skill's Step 0 remains the one entry point; AGENTS.md's pointers say
  "the plugin's" now.
- **Drift guard**: `scripts/tests/test_stellar_plugin_wiring.py`
  (below) replaces the vendored tree's guards on this side.

## Consequences

- One canonical pipeline: fixes and doctrine changes land in the
  plugin and reach every consumer by a pin bump; this repository
  deletes ~4,200 vendored lines and the two-harness sync machinery.
- The npm registry becomes a lane dependency (pinned exact, published
  with provenance); a registry outage fails the review lanes loudly,
  the same way the pinned OpenCode tarball download already can.
- The pass agents are no longer registered repository agents under
  OpenCode (V2 plugins cannot register agents): the skill's runtime
  notes dispatch the built-in `general` subagent per brief file,
  pointed at the plugin's agent markdown, with the per-pass model from
  the tier guidance or `STELLAR_MODEL_*`. Isolation, one-brief-per
  dispatch and the never-post-a-partial-review rule are unchanged.
- Version bumps are a deliberate, reviewable change; the wiring test
  fails a bump that forgets a workflow.
- The docs-only no-op path, the fail-closed verdict chain, the timeout
  salvage path (LABS-635) and the restore-and-harden choreography are
  unchanged — only the tools' location moved.
