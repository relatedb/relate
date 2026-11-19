---
type: adr
area: cli
claim: ee39c36ba0ca5b0e6d7355115310d92e9b6da65e
name: 0477-safe-mode
status: proposed
tags:
  - agents
  - security
title: Safe mode refuses agent-typical dangerous statement shapes
REFERENCES:
  - "[[adr/0136-read-only-open]]"
  - "[[adr/0452-workspace-limit-enables-admission]]"
  - "[[adr/0462-value-nesting-depth]]"
  - "[[adr/0463-expression-allocation-budget]]"
  - "[[adr/0466-traversal-depth-admission-limit]]"
---

# ADR-0477: Safe mode refuses agent-typical dangerous statement shapes

## Context

The Lovable-style failure mode is an agent that treats a graph database
like a scratch pad: unbounded scans (`MATCH (n) RETURN n`) and label-less
writes that make the graph unqueryable later (`CREATE (n)`). Neither is a
bug in the engine; each is a shape a human would rarely write and an agent
writes confidently.

CypherLite already has the individual hardening knobs: the read-only open
([ADR-0136](./0136-read-only-open.md)), the traversal-depth admission limit ([ADR-0466](./0466-traversal-depth-admission-limit.md)), the concurrent
logical-workspace pool ([ADR-0452](./0452-workspace-limit-enables-admission.md)), the value-nesting cap ([ADR-0462](./0462-value-nesting-depth.md)), and the
per-allocation expression budget ([ADR-0463](./0463-expression-allocation-budget.md)). What was missing is a single
switch that turns the *policy* on, so an operator can hand an agent a store
without auditing the agent's Cypher first.

Two facts shape the decision.

- **Opt-in safety modes get left off.** A mode that must be enabled per
  invocation is enabled only by operators who already understand the risk;
  an agent will not enable it for itself, and a wrapper that forgets it
  silently regresses to the unsafe default. We keep the mode opt-in (a
  database engine cannot run an agent's queries safely without the
  operator's consent, and a false positive on legitimate human Cypher is
  worse than the risk), and mitigate with documentation: the flag's help
  row and the CLI/rust/Bolt specs. Advertising the mode as an agent profile
  in the bundled skill is a follow-up (PR B).
- **A guardrail must be narrow.** The openCypher TCK contains label-less
  relationship-only creates (`CREATE ()-[r:名前]->()`), and ordinary
  exploratory queries on small stores legitimately omit `LIMIT`. Refusing
  those would make safe mode unusable rather than safe. Every rule below is
  therefore stated in its narrow form and carries an explicit exemption
  list; the TCK continues to run with safe mode off and stays green.

Precedent: [ADR-0136](./0136-read-only-open.md) (the read-only open), [ADR-0452](./0452-workspace-limit-enables-admission.md) (workspace admission is
off unless asked), [ADR-0462](./0462-value-nesting-depth.md) (a fixed runtime cap with no knob), [ADR-0463](./0463-expression-allocation-budget.md)
(the per-allocation expression budget), [ADR-0466](./0466-traversal-depth-admission-limit.md) (a typed admission
refusal that every surface maps without a new mapping).

## Decision

### 1. One opt-in flag, one open option

- CLI: `--safe` (valueless), negated by `--no-safe`. Environment:
  `CYPHERLITE_SAFE_MODE=1/0`. Ladder: flag, then environment, then off.
  A bad environment value exits 2 before the store opens.
- Embedded: `OpenOptions::safe_mode: bool`, default `false`.
- `cypherlite serve` (HTTP, MCP, and Bolt) accepts the same pair; a
  standalone `cypherlite-bolt` host opts in through the open option.
  Python, FFI, and WASM keep the default (`false`).
- `Database::is_safe_mode()` exposes the fixed fact. It is never
  auto-detected.

### 2. The two guardrails

The front-end runs one new validator, `validate_statement_shape`, in the
same per-statement battery as the traversal bound ([ADR-0466](./0466-traversal-depth-admission-limit.md)), so it sees
every nested body at every depth before anything lowers. The guardrails are
a `SafetyFeature` enum and a `(feature, check-fn)` table; `safe_mode: true`
resolves to the default feature set, so a later declarative
`--safe=<feature list>` (LABS-703) selects a subset rather than refactoring
the checks.

1. **Label-less new nodes.** In `CREATE`/`MERGE`, a node pattern with a
   variable outside the **live scope at the write** and no label is
   refused. The scope is not an additive history of every name seen
   earlier: a `WITH` replaces it with exactly that projection's names
   (`WITH *` folds the current scope in), so `MATCH (n) WITH 1 AS x CREATE
   (n)` is a new label-less node; and an `OPTIONAL MATCH` binding may be
   null, so it does not prove the variable names an existing node and stays
   unbound for this purpose. Anonymous `()` endpoints and endpoints bound
   by an earlier non-optional clause (the relationship-only
   `CREATE ()-[r:X]->()` form) are exempt. Rationale: an unlabeled node is
   what makes a later `MATCH (n:Label)` silently return nothing; the fix is
   local and cheap.
2. **Unbounded reads.** A body whose final projecting clause (`RETURN`, or
   the last `WITH`) has no `LIMIT` and no reducing aggregate, after an
   earlier `MATCH` in the same body, is refused. The exemption is **per
   projected item**: a `collect()` item refuses even beside a reducing
   aggregate in the same projection (`RETURN count(n), collect(n)`), because
   collect() buffers and returns every row. Constant-only `RETURN 1`,
   `SHOW`/`CALL`-only statements, and write tails (`MATCH … SET`/`DELETE`,
   `WITH … SET`) are exempt.

### 3. Error taxonomy

`ErrorKind::SafeModeViolation` (SyntaxError class) is raised by the front
validator as an `EngineError`, then mapped by the existing lowering path to
`Error::syntax`. No surface needs a new mapping: CLI exit 6, HTTP 400 with
`42N06`, Bolt `Neo.ClientError.Statement.SyntaxError`. The message names
the shape, the remedy, and that this is a policy rather than a database
limit:

> This statement is not allowed in safe mode: `<shape>`. Safe mode refuses
> shapes an agent commonly gets wrong — `<remedy>`. This is a policy, not a
> database limit; reopen without `--safe` to allow it.

### 4. Dialect note

Safe mode does not change the Cypher dialect. The same statements parse and
plan identically; the refusal happens after parsing and before lowering,
and is a policy error, not a syntax-grammar divergence from Neo4j.

## Consequences

- Direct human use is unaffected: the mode is off everywhere by default and
  the guardrails are narrow.
- A `LIMIT`-less read of a genuinely small database is refused; the rule
  cannot see store size. The remedy is one `LIMIT`, and the message says so.
- A `CALL { … }` subquery body is validated on its own, so a correlated
  `MATCH … RETURN` inside it needs its own `LIMIT`. This is consistent with
  the engine's per-body lowering, but it is stricter than a human might
  expect.
- The mode is opt-in, so a wrapper that forgets it gets the old behavior.
  The mitigation is documentation, not enforcement; a stronger agent
  profile is deferred to the follow-up below.
- The TCK corpus is unaffected (it runs with safe mode off). The
  label-less relationship-only forms it contains are additionally exempt
  from the node guardrail, so a future safe-mode TCK lane would pass them.

## Deferred to the follow-up (PR B)

The original design bundled more enforcement; it is split out to keep this
change small:

- **Tightened defaults when the knob is unset:** traversal depth 100, the
 [ADR-0463](./0463-expression-allocation-budget.md) expression allocation cap at 32 MiB (once that knob lands),
  workspace admission on at a documented 64 MiB pool, and a lowered bulk-
  batch threshold. Every explicit flag or environment value would still
  win.
- **A third guardrail:** refuse a literal
  `CALL { … } IN TRANSACTIONS OF <n>` below 100 rows.
- **Runner framing:** under safe mode, frame the CLI's multi-statement
  input, MCP `batch_cypher`, and `run_script_wire` in one explicit
  transaction so a mid-script failure rolls earlier statements back.
- **Agent profile docs:** advertise `cypherlite serve --safe` and add a
  safe-mode section to the bundled agent-safety skill reference.
