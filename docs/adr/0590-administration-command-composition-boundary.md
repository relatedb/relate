# ADR-0590: The administration-command composition boundary off the system database

Status: Proposed
Claim: 501378d2792f998ff71de31125ba9e04fd41dd53

## Context

Administration commands report on the `system` database. Live accepts
`SHOW DATABASES` from a user-database session bare, with its own `YIELD`,
and with a `RETURN` over the yielded columns; any other combination —
`WITH`, `MATCH`, `UNWIND`, another catalog command ahead of it, a `UNION`
arm — is the `51N28` `NotSystemDatabaseError`, naming the command it
refused. The same holds with an explicit `USE neo4j` prefix, where only the
*headed* spelling is answered `42N79` first (ADR-0116 §2).

ADR-0116 §2's later references recorded that boundary as deliberately not
implemented. Its rationale was an embedded-engine property: one user, no
privileges, a store-level catalog, so there is no wrong database a composed
administration command could run against. The engine therefore kept running
the composed forms, and the spec harness pinned the scenarios as expected
failures.

That rationale reaches the *safety* of running a composed command, not the
dialect: the observable behavior still disagrees with the server on shapes
the corpus exercises. The same record claimed live ran the mid-body
position; a 2026.10 verification contradicts it, as does the server's own
acceptance suite, whose mid-body `SHOW FUNCTIONS … SHOW DATABASES …`
scenario expects the rejection.

## Decision

Implement the boundary for statements not routed to the `system` database:
`SHOW DATABASES` composed with anything but its own `YIELD` and a trailing
`RETURN` fails with the server's `51N28` message, wherever it sits —
heading the statement, mid-body, or in any arm of its `UNION` chain.

The check is plan-based, in the namespace gate, because the decision
depends on the namespace the statement actually runs under: the same text
is the composition rejection on a user session and §4's clause-whitelist
rejection on a `system` session, and only the plan knows which. The
explicit-`USE` precedence falls out of the gate order — `42N79` for a
headed statement under a non-system `USE`, the whitelist for a `system`
target, this rule for everything else.

`SHOW DATABASES` is the whole subject, not the §2 command set: the other
such command, `ALTER DATABASE`, cannot be composed at all — the engine
rejects every composed form of it before this gate (the stand-alone rule,
or `51N27`) and the server's grammar does not parse those forms either.

Statements routed to `system` are unchanged: the command is where it
belongs, composition is allowed, and what limits the clauses there remains
the system-namespace whitelist (ADR-0116 §4). The `USE system` divergence
also stands: `USE system` plus an administration command stays accepted.

Every `SHOW DATABASE[S]` spelling is covered, not only the plural — the
singular named form and the `DEFAULT`/`HOME` forms are refused composed
too, and the message names the spelling refused, as the server does.
Bare, each of them still runs from any namespace.

## Alternatives considered

- **Keep the recorded divergence.** Rejected: the server rejects the
  composed forms, the corpus exercises them, and the embedded-engine
  rationale does not reach the routing shapes the divergence had grown to
  cover.
- **Enforce it in the front end, over the statement's own clause list.**
  Rejected: the front end does not know the session namespace, so it would
  reject a shape the `system` namespace admits — the same statement text is
  legal there and rejected elsewhere.
- **Extend it to the user- and role-management commands (`SHOW
  USERS`/`ROLES`/`SERVERS`, `TERMINATE TRANSACTIONS`).** Rejected here: the
  §2 vocabulary also decides the namespace exemption, so extending it moves
  those commands' behavior on `system_ontologies` too — its own decision.
- **Implement it on both sides — reject composed commands under `USE
  system` too.** Rejected: live accepts those chains, so rejecting them
  would add a divergence where the change removes one.

## Consequences

- Statements that previously ran now fail with `51N28`: the composed
  off-system forms of `SHOW DATABASES`.
- The spec harness's expected-fail pin for those scenarios is removed, so
  they count as coverage rather than as a known divergence.
- The dialect gains a rule that distinguishes a command by where it runs,
  not only by what it is: the same shape is legal on `system` and rejected
  elsewhere.
- The recorded claim that live runs the mid-body position is superseded;
  the mid-body and `UNION`-arm positions are rejected with the rest.
