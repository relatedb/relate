# Relate agent contract

Relate: embedded, in-memory property graph engine. Durable form: fsync'd
local WAL + local Parquet (or JSON) checkpoints. Query contract: openCypher
TCK. Remote access: Bolt. Clients wrap C ABI. Public Rust crate: `relate`
(`relate::Database`).

Rules apply to every change. History never weakens them.

## Non-negotiables

### 1. Decision log

Architecture, storage, protocol, dialect, public-API, or dependency decision
→ ADR in [`docs/adr/`](docs/adr/) **before** implementation. Not ADR-worthy:
stylistic or trivially reversible edits.

- Filename `NNNN-short-kebab-slug.md` (four-digit sequence, never reused);
  one decision per file; cite others as `ADR-NNNN`
- Template order: **Context**, **Decision**, **Alternatives considered**
  (each ends with rejection reason), **Consequences**
- **ADRs are public records.** No internal references — no ticket, issue,
  or CIP proposal IDs; no internal tracker/chat/wiki links; no internal
  host or tool names. No person names — roles, not people ("the
  maintainers"). No sensitive information of any kind: credentials,
  tokens, internal URLs, unreleased plans, customer or personal data.
  The `adr-numbers` CI check fails any line a change adds under
  `docs/adr/` carrying `LABS-###`, `CIP-###`, or a `linear.app` link
- Status starts `Proposed`; set `Accepted` only when agreed. Accepted ADRs
  immutable — later change = new ADR + **Later references** note on old one
- Mint number via `python3 scripts/adr_mint.py` (atomic claim; never list
  directory). Minted numbers never below 0300
- **Mint late, mint once.** Mint only when the ADR text is written and the
  file lands in the same change — never during planning, never to reserve;
  an abandoned claim burns the number permanently
- Every added ADR carries mint proof: `Claim: <sha>` header line after
  `Status:`, before `## Context`, naming mint's claim commit. CI check
  `adr-numbers` requires it (ADR-0301); unminted conflicting numbers fail

#### Writing guidance

An ADR records architecture and principles — the why and the shape of the
decision — not the implementation. Keep out:

- Code snippets, pseudo-code, and config dumps — name the surface or
  mechanism instead of quoting it
- File:line inventories of what changed — the diff is the record of that
- Step-by-step history of how the change was built or reviewed

Line budget, checkable with `wc -l docs/adr/NNNN-*.md`: target **≤ 80
lines**, hard ceiling **150 lines**. Over the ceiling, cut detail —
long alternative analyses, restated context, implementation asides —
never split one decision into two ADRs to fit the budget.

### 2. Client-facing documentation

Every public surface ships docs under `docs/api/<surface>/`: `spec.md`
(precise contract: types, errors, lifecycle), `description.md` (purpose,
product fit), `examples/` (runnable).

Surfaces: `rust`, `c`, `cli`, `python`, `javascript`, `java`, `go`, `swift`,
`bolt`, `http`, `mcp`, `wasm`, `extensions`.

- No public function, type, or protocol message ships without spec +
  description + ≥ 1 example. Examples are tests (Rust doctests /
  `examples/` crates, client integration tests, compiled C samples in CI)
- No new client language until the C ABI for that feature is documented.
  Clients wrap C ABI only — no parallel protocol
- Client API change → update
  `surfaces/relate-cli/src/skill/bundle/references/client-<surface>.md`
  same change; new surface → add reference (shared skeleton: install,
  minimal usage, gotchas, spec pointer) + one row in SKILL.md's
  selection-guidance table — BUNDLE_FILES drift-guard test in
  `skill_cmd.rs` enforces listing
- The Python wheel bundles a library skill
  (`surfaces/relate-python/python/relate/.agents/skills/relate/SKILL.md`,
  library-skills.io / Agent Skills format); Python API change → update it
  same change. Same rule for any other surface bundling a library skill
- Result metadata (resolved namespace, query type) stays aligned across
  surfaces: one surface gains a piece → every surface that can expose it
  gets it same change

### 3. CLI usage and flags

Every binary (`[[bin]]`, product or tooling) ships documented **usage** and
a **complete flag table** — same flags, positionals, defaults, env vars as
`--help`, same change as code. Include ≥ 1 copy-paste invocation + env vars
the binary or tests honor (e.g. `RELATE_FULL_DATA`).

| Binary kind | Where |
| --- | --- |
| Product (`relate` — the one product binary, ADR-0368; container `entrypoint`) | `docs/api/<surface>/spec.md` |
| Dataset / load-test | [`docs/datasets.md`](docs/datasets.md) |

Clap binaries: `--help` is generated from clap; `///` comments on args are
`--help` and must match docs. The `relate` CLI parses flags by hand — its
hand-written `--help` is the source of truth and must match docs.

**Commands vs flags, `relate` CLI** (ADR-0069, ADR-0200):

- Exit-after lifecycle verbs = **positional commands**, first argument
  only: `skill install|remove`, `upgrade` (alias `update`), `telemetry
  on|off|status`, `pull`, `push` (ADR-0401), `info`, `lab fs-to-graph`,
  `lab git-to-graph`, `serve` (ADR-0232, ADR-0248; runs until
  SIGINT/SIGTERM; `--mcp stdio` until host closes stdin, ADR-0368). The
  statement-session verb `query [STATEMENT] [--database PATH]` (every
  bundle) runs the engine instead of exiting. Shape: `<verb> [action]
  [--flags]`; full flag tables in `docs/api/cli/spec.md`
- Flags **configure the query-engine run** (output mode, params, storage
  knobs); never encode a lifecycle action. Removed `--skill`/`--agent`
  spellings get no migration hints — generic unknown-flag error
- New flags: double-dash kebab-case only; single-dash (`-json`, `-mode`, …)
  = legacy aliases, not pattern. `-d` = short `--database` (ADR-0200)
- Database path colliding with a command word: reach it via `./` prefix
  (`relate ./skill`) or after `--` (`relate -- skill`)
- New commands: singular nouns, `<resource> <action>`, ≤ 1 positional
  (extras → flags). Otherwise follow <https://clig.dev/>

Extension verbs (ADR-0429/0431); `run` and `extension` are reserved words
in every build (ADR-0104):

- `relate run <name> [flags]` dispatches an installed command-kind
  extension — the host parses argv from the manifest flag table, owns the
  store (`-d`/`--database` shared), drives the child's wire
- `relate extension init <name> [--lang sh|py|ts|rust] | install <name>
  [--github REPO@TAG|@SHA] [--url URL] [--from DIR] | list | remove |
  check <dir>` scaffolds and manages packages. Install tiers: signed
  first-party channel; `--github` commit-pinned releases with a per-user
  trust store; `--url`/`--from` trust acts. The scaffold generates the
  language's MCP client glue inline — no published client dependency.
  `check` runs a package directory through the mount's six stages without
  a store

Optional commands = Cargo features in `full` (default), `agent`, `minimal`
bundles (ADR-0104). New CLI capability not in every query run → own
feature, feature-gated tests, matching `--help` + bundle-table updates.

### 4. Test coverage

No behavior without tests.

| Layer | Where |
| --- | --- |
| Unit | next to code (`#[cfg(test)]` / crate `tests/`) |
| Integration | crate `tests/`, `relate/relate/tests/` |
| Cypher contract | openCypher TCK via the spec harness (ADR-0482, ADR-0520) |
| Client contract | each client's tests against C ABI |
| Persistence | WAL + snapshot roundtrip, Native + Memory VFS |

- Language contract lives in the spec harness (ADR-0482): it runs Neo4j's
  Cypher spec suites against the PR's Python wheel and reports coverage per
  ready PR — reporting-only, never a merge gate.
  [`docs/tck/compliance.md`](docs/tck/compliance.md) is the retired pin's
  history and a pointer
- The engine's TCK extension corpus and the CIP-derived eval suite live in
  the harness (ADR-0520); no in-repo allowlist, skip list, or pass claim
- Every public API example runs in CI

### Final gates (CI, `.github/workflows/ci.yml`)

- `cargo test -p relate-cli`
- `cargo test -p relate-cli --no-default-features --features agent`
- `cargo test -p relate-cli --no-default-features --features minimal`
- `cargo test -p relate-cli --no-default-features --features serve` (the
  bare-`serve` bundle lane, LABS-628)
- `cargo test -p relate-load`
- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets --exclude relate-python -- -D warnings`
- `cargo clippy -p relate-cli --all-targets --no-default-features --features agent -- -D warnings`
- `cargo clippy -p relate-cli --all-targets --no-default-features --features minimal -- -D warnings`
- `cargo clippy -p relate-cli --all-targets --no-default-features --features serve -- -D warnings`
  (serve-only items are dead code without a surface feature — a combination
  the other gates never lint; the serve nextest leg stays in the
  cli-bundles lane, LABS-638)

## Crate layout

One Cargo workspace, one `Cargo.lock`. Every member directly under exactly
one of four top-level groups (ADR-0103 + the `extensions/` group, ADR-0431 §11):

```
relate/                          the engine
  relate                         public Rust API (`relate::Database`)
  relate-model                   data model types (`CypherValue`)
  relate-parser                  Cypher lexer, parser, AST
  relate-error                   typed engine errors (`EngineError`)
  relate-core                    values, errors, IDs, config
  relate-catalog                 labels, rel types, property keys
  relate-ir                      Planner IR (not the query front-end)
  relate-front                   lowers parser AST → planner IR (ADR-0046)
  relate-planner                 bind + logical plan
  relate-executor                physical execution + in-memory graph
  relate-ontology                ontology document model, validation,
                                 serialization (ADR-0227); ontology→graph-type
                                 converter (ADR-0226; document→system_ontologies
                                 mapping not shipped, ADR-0229)
  relate-storage                 VFS + WAL + snapshot codecs + checkpoint
  relate-markdown                markdown vault grammar (ADR-0406): frontmatter
                                 parse/emit, wikilinks, sanitization
  relate-splitter                shared statement splitting / input completeness

surfaces/                        adapters of `relate::Database` to users, hosts, integrations
  relate-cli                     Cypher shell (binary `relate`)
  relate-bolt                    Bolt server
  relate-http                    HTTP Query API v2 server (ADR-0232)
  relate-mcp                     MCP (Model Context Protocol) server (ADR-0232)
  relate-auth                    shared generated-token basic auth for network surfaces (ADR-0232)
  relate-serve                   shared accept loop (cap, resilient accept, stacked threads) for network surfaces (ADR-0238)
  relate-python                  Python bindings (pyo3, DB-API)
  relate-ffi                     product C ABI (`relate_*`, JS/Java/Go)
  relate-wasm                    browser bindings (wasm-bindgen)
  relate-formatter               result output formats (table, CSV, JSON, JSONL, recordings, …)
  relate-load                    Neo4j/Aura Query API v2 → CREATE into Database (ADR-0080)
  relate-arrow                   CypherValue ↔ Arrow RecordBatch conversion
  relate-eggs                    hidden REPL dot-command seam + sprite kit for the CLI's `eggs` feature (ADR-0543)

testing/                         conformance, evaluation, datasets, tooling
  relate-datasets                synthetic graphs + loadtest binaries
  relate-test-support            shared test-only helpers (big-stack runner)
  relate-query-latency           graph-benchmark-ldbc query latency suite (ADR-0243)
  xtask                          boundary check (`cargo xtask check-boundaries`)

extensions/                      first-party extension crates — the child processes of
                                 the extension program (ADR-0431 §11), spawned by the host,
                                 never linked into a product binary
  relate-wire                    the extension mount's shared kit: the bounded reads, the
                                 caps, and the one MCP tool table both surfaces serve
                                 (ADR-0447) — the one extensions-group package a surface links
  relate-fs-to-graph             the fs-to-graph importer child, MCP stdio (ADR-0431 §11)
  relate-git-to-graph            the git-to-graph importer child, MCP stdio (ADR-0431 §11)
```

Crates sit at depth two (`<group>/<crate>/`); package names don't encode
group. Language clients (JS/Java/Go/Swift) under `clients/`; Swift manifest
= repository-root `Package.swift` (ADR-0433).

Dependency rules (ADR-0103), on resolved workspace graph by dep kind:

- **R1** — normal/build deps of `relate/*` → `relate/*` only
- **R2** — dev deps of `relate/*` → `relate/*` or
  `testing/relate-test-support`
- **R3** — normal/build deps of `surfaces/*` → `relate/*`, `surfaces/*`, or
  `extensions/relate-wire` (the one linkable extensions member; importer
  children are spawn-only); dev deps may also target
  `testing/relate-test-support` or any `extensions/*` member (test ties,
  never shipped)
- **R4** — `testing/*` → any workspace package
- **R5** — every member directly under `relate/`, `surfaces/`, `testing/`,
  or `extensions/`
- **R6** — new heavy external dep (large transitive tree, native/C++
  build, networking or async runtime, …) in any `relate/*` crate → own ADR
- **R7** — normal and build deps of `extensions/*` target `relate/*` (the
  engine's leaf crates only — a first-party child carries no `Database`,
  ADR-0430 §6) or `extensions/*`; their dev deps may also target
  `testing/relate-test-support`. A first-party child never links a surface:
  the host is not linkable into a child

R1–R5 and R7 enforced by `cargo xtask check-boundaries` (CI); R6 = review
rule. Engine never depends on surfaces, extensions, or testing; engine code
needing formatter, loader, binding, or harness moves to a surface or
`relate-test-support`.

## Code-shape rules (stellar prevention, LABS-311)

Recurring stellar findings (135 from the #263/#278/#288/#295 rounds) are
prevented here, not re-caught in review;
`.agents/skills/stellar-review/tools/recurrence.py` tracks the recurrence
rate.

- **No silent fallbacks.** No `unwrap_or(Value::Null)`, `filter_map`
  dropping malformed entries, fabricated defaults over invariant-bound
  values — return a typed error or `expect` with an invariant. No
  `panic!`/`unreachable!` beside a handled form of the same failure
- **No shape-sniffing, no stringly-typed discrimination.** Match the typed
  variant (`Error::Capacity`, an enum) — never `eq_ignore_ascii_case("count")`,
  string prefixes, `str | True` sentinels. Add a variant if one is missing
- **One canonical helper per policy.** Before writing a report writer,
  scalar extractor, temp-dir helper, read guard, or frontmatter scan: find
  the existing one and call it. A second copy needs a stated reason
- **Parallel state = bug.** No two tables, literal subsets, or second
  `match` over the same vocabulary — derive one from the other. Never
  split one exhaustive `match` into two glued by `_ =>`
- **One public path.** Never `pub mod x` + re-export its items; new items
  are `pub(crate)` until a second crate needs them
- **Layering.** Sibling-shared helpers go in the lowest shared module;
  lower-level modules never import up into harnesses
- **No 1k-line creep.** Split before a file crosses 1000 lines (`wc -l`
  every file you grow). Exempt: pinned vendored artifacts byte-identical
  to upstream (e.g.
  `relate/relate-ontology/schema/ontology-graph-spec.schema.json`,
  ADR-0227 §6), and declared frozen reference records — a file whose
  header marks it a verbatim iteration/provenance artifact, not built or
  executed (e.g. `extensions/relate-dungeon/reference/`, LABS-884)
- **Don't re-split a fact.** A centralized fact (alias table, constant) is
  consulted at every use site, never re-derived at one
- **Dedup refactors preserve semantics** (kill/reap on deadline, error
  text); doc comments stay attached to their item; `use` blocks ordered

### Prevention distillation loop (LABS-311)

The rules above are loop output, not a fixed list. The nightly recurrence
job (07:00-09:00 Europe/Berlin) histograms every open PR's carried ledger
by checklist id and posts to the distillation tracking issue (#347). A
pattern crossing the **promotion threshold — ≥ 5 findings across ≥ 3 PRs
with a generalizable remedy** → candidate: distill it into one named
imperative rule (like the nine above), get human approval, land it as a
one-line PR here. One-off findings stay checklist items.

### Agent-process anti-patterns

Process, not code shape:

- **Never work around a permission-classifier block — report it.** A tool
  call denied by a classifier or permission guard (auto mode, hook,
  worktree git-guard) ends that line of work: surface the block to the
  human for a decision — never reroute the same action through another
  mechanism (synthetic commits, wrapper tools, subagents) to get past the
  guard. The denial is a human-in-the-loop signal, not an obstacle
- **Re-squash via `scripts/re-squash.sh <old-commit> -F <message-file>
  <file>...` only.** Bare `git reset --soft origin/main` is BANNED: it
  keeps the stale *index* (fix rounds silently drop), and when main moved
  the fresh commit's *tree* predates new merges → silently reverts them.
  The script builds the commit in object space off `origin/main` and
  aborts on staged paths outside the intended list or on main-moved
  intended paths — revert-proof by construction. `git commit --amend`
  only for an unpushed single commit
- **After every re-squash, the gate is set equality, not presence:**
  `git diff origin/main HEAD --name-only` must list EXACTLY the intended
  files — nothing more (foreign file = revert of main's work: abort
  BEFORE pushing), nothing less (missing file = dropped fix round)
- **`git add -A` in worktrees traps `.claude/worktrees` gitlinks** —
  stage explicit paths
- **Never anchor file insertion on a `doc-comment + item` pair.**
  Inserting between a `///` block and its fn glues the old doc onto the
  new item and leaves the old fn undocumented. Insert after the anchor's
  full body, or above the doc block as a complete doc+item unit
- **Every Linear ticket: `relate` project + exactly one Area label + one
  Theme label.** Filing via the CLI: `linear issue create` needs BOTH
  `--team <uuid>` (from `linear team list`) and project; comments via
  `linear issue comment create <id> --body "..."`. Area: `engine`,
  `cypher`, `surface`, `cli`, `infra`, `ecosystem`; Theme: `perf`,
  `correctness`, `observability`, `code-health`, `enhancement`,
  `agent-dx`. Bug = its Area + `correctness`; speed = + `perf`. Missing
  label or project = invisible to night-shift queries and dashboards.
  `issue update --labels` REPLACES the label set (no add-label form)
- **review-tasks issues: always file with status `Agent Backlog`** — the
  work-picking queries and backlog sweeps read that state; anything else
  is invisible to pickers until manually re-stated. `In Progress`/`Done`
  = the worker's lifecycle to drive, never the filer's default
- **Clean up after yourself at merge time.** Every workspace a PR used —
  worktree (`git worktree remove` + branch delete, local + remote),
  scratch clones and experiment dirs, gbuild trees/nodes and the
  feature's gbuild store, build caches (`target/`, shared-target
  caches) — removed at merge, not residue for a later round. A
  workspace with uncommitted changes is never deleted by another
  session — it stays until its owner confirms. `git worktree prune` +
  `git fetch --prune` after, so admin state matches disk

## Toolchain

- MSRV: Rust 1.88. With a local toolchain newer than the CI pin, run
  clippy as `rustup run 1.88 cargo clippy …` — different versions fire
  different lints. Sanctioned `-A` allowances (`-A deprecated -A
  clippy::chunks_exact_to_as_chunks`) cover pre-existing lints in
  untouched files only, never findings in the push's own files
- Edition: 2021
- License: Apache-2.0 (ADR-0055)

## Branching and pull requests

- **Always work from a worktree on a fresh branch.** No change is made in
  the main checkout: every task — feature, fix, docs, config — starts with
  `git worktree add <dir> -b <branch> origin/main` (or the appropriate
  stacked base), works and commits there, and PRs from there. The main
  checkout stays on `main` and clean, for reading and coordination only;
  it never carries a working diff or a task branch

## Merge bar and review-loop bounds

- **Fix before merge: blocker / critical / high / medium** findings from
  any review bot. **Defer nits and lows** — batch them into a
  `review-tasks` Linear issue (Agent Backlog, with a `Files:` section) and
  merge without them; never block a security or feature PR on polish
- **Bound the loop.** A round that only re-reports stale entries or
  repeats an already-ticketed class ends the loop: land the PR — target
  hours, not days
- **Small PRs by default.** One concern per PR; slice follow-ups into
  their own tickets — `main` moves fast
- **When history becomes un-rebasable, squash onto fresh `main`:** `git
  diff <merge-base> <head>` + `git apply --3way` on current `main`, then
  hand-port the restructured files — one commit, one pass
- **Rebase only when it pays.** Rebase when what landed on `main` since
  the fork conflicts or would balloon the review diff, otherwise stay —
  a green head over a moved base merges cleanly (trial: `git merge-tree
  <base-of-head> origin/main <head>`). Skip docs-only /
  `AGENTS.md`-only advances
- **Safety capabilities are declarative.** Resource caps,
  statement-shape guardrails, read-only scope are selected through one
  registry (LABS-703; the `PreviewFeatures` seam,
  relate-core/src/preview.rs) so levels/profiles need no new machinery
- **Rebase, never merge.** Branches stay linear: `git rebase <base>`,
  never `git merge <base>` — merge commits break GitHub's stacked-PR
  tooling and create conflicts the stack UI cannot resolve
- **Local reviews run on the cheapest model tier that follows a checklist
  against a pre-digested brief** (stellar-review SKILL.md model-tier
  rule); reserve strong models for judgment-heavy work. Per-pass
  override: `STELLAR_MODEL_*` env vars (ADR-0302)
- **Run local gates in parallel — 8-16 cores.** fmt + clippy + per-crate
  suites as concurrent background shells; per-invocation `--target-dir`
  or sequential retry resolves cargo lock contention
- **Squash to one commit before opening a PR; re-squash fix rounds into
  it** — one squashed commit rebases once, multi-commit PRs re-conflict
  commit-by-commit. Fix on an unpushed single commit = `git commit
  --amend`; everything else = the re-squash recipe above. Stack large
  work streams from the first branch (`main → b1 → b2 → …`, each PR
  `--base` its predecessor); `gh stack` maintains the cascade
- **Use gbuild for larger features** — a feature decomposing into
  multiple nodes or spanning multiple PRs goes through the gbuild plugin,
  not one hand-rolled branch; small changes go on a single branch
  straight. Loop: `plan` → `run` → `review` → `pr`
  - `/gbuild:plan <feature>` — chart the plan as a dependency graph in
    the Relate store: nodes with typed `INPUT`/`OUTPUT` fields +
    **mandatory concrete acceptance** (checkable by a fresh context),
    edges passing the cut test (`DEPENDS_ON` exactly when a node reads
    another's output). Feature branch in its own worktree; plan, don't
    do — even `research` nodes are charted, not run. `--add
    "<requirement>"` reopens with a delta
  - `/gbuild:run <feature>` — execute wave by wave: ready nodes
    dispatched concurrently, file-touching nodes in own worktrees, each
    reviewed by `gbuild-reviewer` against its acceptance before merging.
    Main context only; the store is the only writer and the memory —
    re-invoke `run` to resume
  - `/gbuild:review <feature>` — end-of-feature audit (`gbuild-auditor`)
    of the **accumulated diff**: abstraction quality, file size,
    spaghetti growth, cross-node duplication. Issues → `plan --add
    "<fix>"` + `run`; clean → `pr`
  - `/gbuild:pr` — push, open the PR, watch CI, auto-reopen + re-run on
    failed checks
  - `/gbuild:status <feature>` — frontier, blocked, in-flight, verdicts,
    cluster shapes
- **Open every PR as draft; mark ready only on human direction.** Drafts
  run the fast gate only — clippy, lint, the draft smoke lane (8 light
  crates' unit suites on a warm shared cache), three opencode reviews
  (code/stellar, security, rust). The full matrix (OS sweeps, Bolt
  driver smokes, corpus, feature bundles, loadtest gates,
  clients, WASM/site) runs only when ready. A clean fast gate = the
  signal to ask a human to mark it ready
- **Catch findings locally first** — local gates are cheaper and faster
  than any CI round. Run the stellar round through its one entry point,
  `.github/scripts/stellar-prep.sh local [--quick] [PR_NUMBER]` (no PR →
  round-1 against unpushed commits), plus the touched crates' test
  suites **before every push**, not once per PR. **The full round
  (carried ledger + dependency expansion + sharded briefs) is
  the mandatory pre-push gate, not optional** — it reviews approximately
  what CI's round will judge; fix what the passes find so CI's stellar
  round confirms instead of discovering. `--quick` (delta-only scope,
  one shard per pass) is a mid-work pre-filter, never a substitute: a
  clean quick round does NOT predict a clean CI round; a clean full
  round is the best local predictor of CI's verdict (still not
  identical — CI pins pass models via `STELLAR_MODEL_*` secrets; full
  option table: stellar-review SKILL.md Step 0)
- **After pushing a draft, watch checks in ~10-minute loops**: read new
  review findings, fix in the same squashed commit, re-push, repeat
  until the fast gate is clean
- **Read review comments incrementally, newest first** — only the latest
  round is current. `python3 scripts/pr_review_digest.py <PR>
  [--since-id ID]` prints comments newest first with `superseded`
  markers; the `last-comment-id` footer feeds the next `--since-id`. A
  finding covered by a newer round's "still open" list is done
- **The stellar ledger re-reports resolved findings verbatim on later
  heads.** Before churning code on a re-reported finding, verify the
  premise against the tree (grep the symbol, count claimed call sites);
  stale → state the evidence on the PR, don't change code. rust-review
  and security-review re-reports are real. Deliberately deferred
  findings (reason stated on the PR) → `review-tasks` Linear issue
  (status `Agent Backlog`) with a `Files:` section naming the scope —
  the nightly worker fetches explicit-scope tickets first and skips
  tickets naming no workspace path (LABS-684)
- **Review unit = changed area, not whole file.** A touched file pulls
  only its *changed* functions into review scope; surrounding untouched
  code = context. `ledger.py scope` anchors units to the diff's changed
  line ranges and pass briefs carry the changed-unit list
- **`agent-backlog-autopilot` is the one sanctioned self-merging agent.**
  The skill at `.agents/skills/agent-backlog-autopilot/SKILL.md` carries
  standing, invocation-gated authorization to drive one LABS
  agent-backlog card end to end and squash-merge its own PR. Bounds:
  the ready flip stays human-gated; steering inputs bounded to CI
  results, the three bot reviews, and invoking the human; a diff
  touching anything CI executes or gates on, auth, dependency
  manifests, or agent-instruction surfaces is never self-merged. The
  skill file is the full contract — no other surface grants merge
  authority
- **Before any push**: build, run tests, `cargo fmt --all --check`, and
  the FULL workspace lint — `cargo clippy --workspace --all-targets
  --exclude relate-python -- -D warnings` — never a `-p <crate>` subset
  as gate (crate-scoped runs build deps without linting them). Warm,
  the workspace run is cheap and is the exact CI command

## Agent tooling notes

- **gh GraphQL variables**: each `-f name=value` maps to a variable
  literally named `name` — flag name must equal the declared variable
  (`mutation($t:ID!)` needs `-f t=<id>`; mismatch fails per-call in
  stderr only)
- **Cross-session messaging**: a `SendMessage` invoke loaded via
  ToolSearch fails to fire → dispatch through `tool_use_tool` (same tool,
  same arguments) or fall back to an external channel (PR comment via
  `gh`). Direct invokes of ToolSearch-deferred tools can silently drop
  in routed sessions (claude-code#103057)

## The cookbook

The cookbook's recipes, their tests, and the `/cookbook/` pages live in
the relate-web repository (ADR-0528), which runs every recipe against the
WASM bundle it ships. Nothing here builds or tests them. Keep the WASM
`renderResult` export and `relate_formatter::recording` stable: a change to
the recording format changes every recipe's recorded results there.

## How to investigate a slow query

Target = query-latency suite (ADR-0243, ADR-0244): 30 graph-benchmark-ldbc
queries at SF0.1, timed and answer-checked, with an in-process sampling
profiler. Loop: [`docs/testing/query-latency.md`](docs/testing/query-latency.md)
— `cargo run --release -p relate-query-latency -- run --qids N
--min-time 5` for p50, `make query-latency-profile QID=N` for the
profile, one hypothesis per change, and the full SF0.1 suite (+ SF1 when
the effect grows with size) before the PR.

## Cypher dialect

Preview Cypher features (accepted into CIP, not yet in the pinned dialect
floor) gate behind `RELATE_PREVIEW` (ADR-0371): comma-separated kebab-case
names, read once at `Database::open`; unset/empty disables all; unknown
name = loud fail-closed open error; a gated feature invoked while disabled
fails with the authored `preview feature '<name>' is not enabled: set
RELATE_PREVIEW=<name>` error. The registry (`relate-core/src/preview.rs`)
is the single feature-name table — a feature PR adds its row there and
gates at the parse/dispatch point. Only syntax-extending features gate
(additive function names don't); ADR-0497's name-admission gates are the
recorded exception, and its taxonomy records the four gate-location
exceptions. The `db.ontology.*` procedure family (feature name `ontology`)
is one of them: it gates at procedure dispatch, tracking a non-final
external spec (ADR-0378) — while off, every call fails with the authored
`semantic error: SemanticError: preview feature 'ontology' is not enabled:
set RELATE_PREVIEW=ontology` error and `SHOW PROCEDURES` omits the four
rows; on, the rows are listed and callable.

Name gates hide a name, not a syntax: while off, a call fails with the
registry's ordinary `UnknownFunction` rejection (42N48) and `SHOW
FUNCTIONS` lists no row — never the authored preview error, which would
advertise a name the catalogue must not list. A gated syntax or procedure
family fails closed with the authored preview error unless the opt-in is
set.

Query front-end = **Relate's parser** via `relate-front` (ADR-0046); the
historical planner-IR parser is diagnostic-only
(`RELATE_PARSER=planner-ir`). The pattern engines carry the same
diagnostic-switch shape: `RELATE_PATTERN_ENGINE=auto|nfa|walk`
(ADR-0519), read once at `Database::open` — unset/empty/`auto` routes as
today, `nfa` forces the frontier NFA rung (ADR-0478) onto every
frontier-compilable clause, `walk` stands the traversal kernels down to
the per-row walker; an unknown value fails the open naming the three.
Diagnostic-only, never Cypher syntax; it dies with the old engines.
The openCypher TCK = language **floor**
(ADR-0056), measured by the spec harness: its `tck` suite is the upstream
TCK synced from Neo4j's compatibility-spec-suite, reported on every ready
PR with the ADR-0482 bands and never gated. The in-repo pinned 2024.3
corpus, its allowlist and its 3892-of-3892 claim are retired (ADR-0520).
The CIP-derived eval suite = **stretch** (the harness's `cip-eval` suite;
reporting only, no ratchet).

Database administration (ADR-0116): every store has three fixed namespaces
— `neo4j`, `system`, `system_ontologies`; `USE` routes the statement,
unknown name fails (42N00). Supported commands = `SHOW DATABASE[S]`, `SHOW
DEFAULT/HOME DATABASE`, `ALTER DATABASE … SET DEFAULT LANGUAGE` only —
everything else (`CREATE`/`DROP`/`ALTER DATABASE` otherwise,
`START`/`STOP`, aliases, composite forms) fails 51N27. `system_ontologies`
is listed but not queryable (ADR-0229): every statement routed there —
reads included, `EXPLAIN`-wrapped included — fails with the authored 42N17
text (`This database is not queryable. Use the db.ontology.* procedures on
the user database 'neo4j' instead.`); only the ADR-0116 §2
database-administration commands run from there; the stored ontology
document is reachable only via `db.ontology.*` procedures, and no graph is
derived from it. `SHOW ROLES/SERVERS/USERS` stays out of the merge gate
unless a later ADR brings it back. Bolt = transport (ADR-0057), not Neo4j
feature parity.

## Baseline loadtests

Loadtests frozen under `baselines/` = merge gate (ADR-0046). All baseline
fraud, identity, checkpoint and memory-shape runs use the predefined
baseline environment (ADR-0070): `make fraud-loadtest`, `make
identity-loadtest`, `make fraud-checkpoint`, `make memory-shape-loadtest`,
or `make baseline-loadtests` — compiled in `rust:1.88-bookworm`, run in
`debian:bookworm-slim`, 2 vCPUs, 2 GB RAM (`memory-shape` same caps,
`MEMORY_SHAPE_MEMORY` default `2g`, ADR-0289). Every image-built lane
builds from the one `Dockerfile.baseline`, parameterised by its bin list,
cargo-cache id and entrypoint (ADR-0495). Env knobs: `LOGICAL_MB` (default
32) sizes each shape; `ATTRIBUTES` (default 1) integer properties per node;
`TEXT_INDEXES`, `POINT_INDEXES`, `RANGE_INDEXES` (each default 0) indexed
attributes per node. Never freeze or gate-compare host-native `cargo run`
reports; `BASELINE_ENV=host` = debug escape only — `make
baseline-loadtests` refuses it.

Committed envelopes are CI-owned (ADR-0071): dispatch
`.github/workflows/create-loadtest-baselines.yml` on `ubuntu-24.04`; it
opens a PR with the envelopes (no push to `main`). The dispatch `runner`
input = the BARE runner label (`linux-4-core-x86`), never the class string
— the workflow composes the class itself; a full class string requests a
nonexistent label and queues silently for hours. Local `make
baseline-loadtests` writes `target/` envelopes only unless
`BASELINE_FREEZE_SOURCE=ci`. Every report carries its `environment` class;
CI `loadtest-baselines` hard-fails on class mismatch (`INCOMPATIBLE`).
Local `make gate` / `make compare-loadtests` skip ops/s and p99 on mismatch
but still require `errors == 0` and logical bytes. `memory-shape-loadtest`
(ADR-0289) is not in the CI compare set — the CI job runs and reports
fraud + identity only; the local target + freeze workflow remain,
re-entry = future decision.

`make query-latency` runs the timed 30-query SF0.1 suite in the same
recipe with its own memory cap (`QUERY_LATENCY_MEMORY`, default `6g`,
ADR-0245). The per-PR `query-latency` CI job runs it on
`linux-4-core-x86` (compile-bound host; the timed recipe inside stays
2-vCPU-capped); `make baseline-loadtests` freezes
`baselines/query-latency-sf0.1.json` with it (worst-of-N per-query p50);
the CI job compares per PR with the ADR-0245 bands (p50 <= max(1.5x,
+0.5 ms), peak RSS <= 1.15x). `make gate` runs the compare once an
envelope exists — until the first CI freeze lands, the latency compare is
skipped (memory-shape precedent, ADR-0289).
