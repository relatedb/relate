# ADR-0435: The managed-transaction lifecycle contract

Status: Proposed
Claim: f2485a766ad5ffda0e42170cbed3d52b3ca2e34d

## Context

ADR-0237 mounted the Query API v2 managed-transaction routes on the
engine's real explicit transactions (ADR-0028). Every tx reply carries a
`transaction.expires` value computed as "now + 60 s"
(`surfaces/cypherlite-http/src/tx.rs`, `tx_block`). That value is
fabricated per reply: the registry (`TxRegistry`) records no
last-activity, enforces no deadline, and has no size bound. ADR-0237
recorded exactly this: an open tx lives until commit, rollback, or server
shutdown, and a client-side bookkeeping hint is all `expires` is.

The engine has 256 process-wide snapshot pin slots (`PIN_SLOTS`,
`cypherlite/cypherlite-executor/src/graph/mod.rs`). `Connection::begin`
pins one slot for the transaction's whole life; `commit`, `rollback`, and
`Connection` drop release it (`cypherlite/cypherlite/src/connection/txn.rs`).
The engine is single-writer: the commit loop is the sole write
serialization point, and dropping a `Connection` releases its pin
deterministically. A full pin table fails with the typed
`Error::Execution("snapshot pin: all 256 slots are held")` after a bounded
retry (ADR-0038, ADR-0111).

The HTTP Query API is one request per connection (`Connection: close`,
`surfaces/cypherlite-http/src/http.rs`). The connection that began a
transaction is already gone before its next statement, so the TCP
connection cap (`--max-connections`, ADR-0333, default `0` = unlimited)
cannot bound open transactions: even `--max-connections 1` admits an
unbounded number of sequential begin requests, each leaving a registry
entry that holds a pin. An abandoned transaction is therefore permanent
state today: it outlives its connection, is unreachable except by the
client that holds the id (or its owner, ADR-0428), and holds a pin until
restart. Once the table is exhausted, every statement on every surface
fails, and the HTTP surface reports it as a 400 with the general code
(`run_error`'s catch-all) — a refusal that names neither the transaction
registry nor the client that caused it.

ADR-0237 deferred idle-timeout enforcement ("needs a reaper"). ADR-0428
made the registry embeddable and owner-scoped. ADR-0333 established the
knob discipline this ADR follows: flag → environment → default, `0` turns
a control off, a malformed environment value is a loud startup error, and
an unlimited control warns on a non-loopback bind.

## Decision

### 1. Idle expiry is enforced, lazily

- Each registry entry records `last_activity` on the monotonic clock
  (`Instant`). The operations that keep the transaction registered — begin
  and run, including the empty-body keep-alive probe — refresh it AFTER
  their statement completes; commit and rollback close the transaction
  (the entry leaves the registry), so there is no deadline left for them
  to refresh. Refreshing after completion is the point —
  an operation that refreshed only at entry would leave a transaction
  already past its deadline after any statement longer than the idle
  timeout, expiring a demonstrably active client. A failed run statement
  still refreshes it — the transaction survives for the client to close,
  so the client is alive.
- **The default idle timeout is 60 seconds.** The wire already advertises
  60 s; enforcing the advertised value makes it true instead of
  fabricated. An HTTP managed transaction has no connection between
  requests, so silence is the only liveness signal available, and a
  minute of it means the client is gone or stuck. Bolt's 300 s idle
  default governs a persistent connection whose silence between queries
  is normal, a different resource with a different meaning; an operator
  who wants that bar sets the flag. `0` disables expiry.
- Every registry operation reclaims expired entries in the same critical
  section it acts in, under the registry mutex: `admit` (begin) sweeps,
  and the `lease` lookup (run, commit, and rollback) takes its own target
  out of the map first and then sweeps the rest — one critical section,
  with the taken entry's expiry decided in hand (§3) rather than inferred
  from a presence probe around the sweep. `Lease::release` returns the
  entry for run; `Lease::take` is the one-way exit for commit and
  rollback. There is no background thread.
  The registry mutex serializes every registry operation, and no engine
  statement ever runs under it (the run route leases its entry out of the
  map, runs the statement unlocked, and returns the entry), so the sweep
  can neither race a lookup nor reclaim a transaction with a statement in
  flight — a leased entry is not in the map to be seen. The engine's
  single-writer commit loop means a swept `Connection` cannot be
  committing elsewhere. Expired
  entries are removed under the lock and their `Connection`s are dropped
  after the lock is released, so a slow drop never lengthens the critical
  section. Dropping the `Connection` rolls back the open transaction and
  releases its pin through the engine's ordinary deterministic drop path;
  a rollback error does not keep the entry alive, because drop is the
  documented backstop (the same policy as `roll_back`).
- An operation whose lease takes its target id and finds it expired gets
  the typed expired reply (§3), not the generic not-found. Once an
  UNRELATED operation's sweep has already reclaimed the entry, a later
  request on that id finds no entry and gets the generic not-found
  (`404`/`42000`)
  — the typed code is the first-observer's, which is sufficient for
  logs and telemetry; the busy-registry degradation to the generic code
  is accepted.
- `expires` becomes honest: the wall-clock rendering of
  `last_activity + idle_timeout` (RFC 3339 with milliseconds, UTC),
  recomputed on every reply that carries a transaction block. Enforcement
  uses the monotonic clock, so a wall-clock step can neither resurrect nor
  prematurely kill a transaction; the wall clock is only the client
  rendering of the same deadline. With expiry disabled, the `expires`
  member is omitted — the block is `{"id": …}` — because there is no
  deadline to report.

### 2. The registry has a cap

- **The default cap is 128 open managed transactions.** An open
  transaction holds one of the engine's 256 process-wide pin slots for its
  whole life. A cap of 128 bounds one HTTP registry to half the table and
  leaves at least 128 slots for auto-commit statements, Bolt and MCP
  sessions, and every other pin consumer. A default of 256 would let a
  full registry starve every other consumer and reproduce the failure this
  ADR removes; 64 refuses legitimate concurrent managed workloads the
  engine can serve.
- Unlike the ADR-0333 connection cap, whose default is `0` (unlimited),
  the registry default is non-zero. An abandoned transaction outlives its
  connection — which is already closed — and holds a pin until idle
  expiry, so a connection-cap default cannot protect it and an unbounded
  default would preserve the permanent-leak failure. `0` remains the
  explicit unlimited spelling, following the ADR-0333 convention.
- The cap is checked after the sweep and **before** `connect_namespace`
  and `Connection::begin`, so an over-cap begin never takes a pin. The
  cap counts open transactions, not statements: an entry leased out of
  the registry for a running statement still counts, because it still
  holds its pin. A begin's slot is likewise a reservation counted under
  the same registry lock from `admit` until its entry registers, and the
  RAII guard returns it on every failure or panic in between, so the
  admitted-but-unregistered window is bounded too. A burst of concurrent
  long statements or concurrent begins therefore cannot raise one
  registry's footprint above the cap (no `2 x max_open` overshoot); the
  count is given back when each statement returns or each begin
  registers, and a statement or begin that unwinds gives it back too.
- The cap is per registry. Product `serve` runs one HTTP server and one
  registry; an embedder running several HTTP servers must divide the
  process pin budget between them.

### 3. Reply contract

- **Over-cap begin:** `429 Too Many Requests` with the error envelope
  `{"errors":[{"code":"42N91","message":"transaction registry full: 128 open transactions (close one or raise --max-open-transactions)"}]}`.
  429 is the same status the surface already uses for retryable capacity
  (a retryable `ResourceLimit` maps to 429); the over-cap refusal is
  constructed in the tx route itself, before any engine call. `42N91` is a surface-authored
  code in the GQLSTATUS-style space, like `42NFF`: no engine error exists
  for a registry refusal. The message names the configured cap and the
  flag that raises it.
- **Expired transaction**, on any route naming its id: `404` with code
  `42N90` and a message naming the id and the idle timeout
  (`transaction tx-7 expired after 60s idle`). The HTTP status stays 404
  because the client's recovery is identical to a closed transaction
  (abandon the block and retry); the code is what distinguishes expiry
  for logs and telemetry. Owner scoping (ADR-0428) is preserved: a caller
  that does not own the transaction receives the unchanged generic
  404/`42000` not-found, so expiry never discloses that the id existed.
  Unknown and already-closed ids keep today's 404/`42000`.
- A successful tx reply keeps its current shape; only the `expires`
  member's value (or its absence when expiry is disabled) changes.

### 4. Flags and environment

- `--tx-idle-timeout-secs N` / `CYPHERLITE_TX_IDLE_TIMEOUT_SECS`, default
  `60`, `0` disables expiry. The `tx-` prefix keeps it distinct from
  Bolt's `--idle-timeout-secs`.
- `--max-open-transactions N` / `CYPHERLITE_MAX_OPEN_TRANSACTIONS`,
  default `128`, `0` is unlimited.
- Resolution goes through `cypherlite_serve::resolve_flag_or_env`
  (`surfaces/cypherlite-serve/src/lib.rs`), the shared flag → environment
  → default ladder stated once: the flag wins, a blank environment value
  counts as omitted (the default, not zero/unlimited), and a malformed
  flag or environment value is a loud startup error naming the flag or
  variable, never a silent default. The limits resolvers
  (`resolve_tx_limits`, `unguarded_tx_registry_warning`) live in
  `surfaces/cypherlite-http/src/tx_limits.rs` beside the reply renderer
  their validating constructor protects, and `serve` resolves them
  before auth so a malformed value cannot persist a generated token.
- The limits are `TxRegistry` state, set by a configured constructor;
  `TxRegistry::new()` keeps both defaults. Product `serve` constructs the
  registry from the resolved values and hands it to `HttpConfig` as
  today (ADR-0428). `HttpConfig` does not grow a second copy of the
  limits. The in-process model is typed: an idle timeout of `None`
  disables expiry and a cap of `None` is unlimited. The `0` spelling
  above is the **wire/flag** spelling; `TxLimits::new` converts it at
  that one boundary, so no consumer re-derives a zero sentinel.
- A startup warning fires only when the registry is wholly unguarded —
  `--max-open-transactions 0` and `--tx-idle-timeout-secs 0` together —
  on a non-loopback bind, naming both flags. Either protection alone is
  silent, following the ADR-0333 warning posture.
- The implementing change updates `docs/api/http/spec.md` (managed
  transactions and the error table), the hand-written `serve --help`
  text, and the flag table in the same change (AGENTS.md §3).

## Alternatives considered

- **A background sweeper thread.** Rejected: it adds a thread, a second
  lock/clock path, and time-dependent tests to an engine whose
  single-writer commit loop is the only serialization point. The sweep is
  O(open transactions) and only matters when a registry operation needs
  room, so running it under the registry mutex that every operation
  already takes is race-free and simpler; a sleeper also complicates
  shutdown joining and pin-release ordering for no reclaim a subsequent
  operation would not perform anyway.
- **Unbounded registry with pin-slot exhaustion as the only backstop
  (status quo).** Rejected: the backstop arrives only after all 256 slots
  are gone, so it starves auto-commit, Bolt, and MCP at the same moment it
  refuses the offending transaction; it surfaces as a 400 carrying the
  engine's `snapshot pin` execution error with the general code, which
  names neither the registry nor the client; and because an abandoned
  entry never releases, the condition is permanent until restart.
- **Connection-cap-only (no transaction cap).** Rejected: the HTTP surface
  closes the connection after one request, and the registry — not the
  connection — owns the transaction's lifetime. A connection cap bounds
  concurrent sockets, not the unbounded sequence of begin requests that
  leaves entries behind.
- **No idle expiry (status quo).** Rejected: `expires` is fabricated on
  every reply, so no client can distinguish a live transaction from an
  abandoned one; an abandoned transaction is unreachable except by its
  owner and holds a pin until restart; the advertised contract is false.
- **Follow the ADR-0333 default and ship the cap unlimited (`0`).**
  Rejected: the connection-cap precedent protects a resource that the
  connection itself owns and releases on close; a transaction outlives
  its connection, so the same default would preserve the leak. `0` stays
  available as the explicit opt-out.
- **A cap of 256 (the whole pin table).** Rejected: a full registry would
  consume every slot and make non-transaction work fail through no fault
  of its own; 128 reserves half the table.
- **Return `410 Gone` for an expired transaction.** Rejected: drivers
  already treat an unknown transaction id as 404 and recover by
  abandoning the block; a distinct status would fork client recovery
  paths for no information the typed `42N90` code does not already carry.
- **A per-transaction timer/reaper task.** Rejected for the same reasons
  as the background sweeper: a per-entry timer multiplies the concurrency
  surface and still needs the registry lock to remove the entry, which is
  exactly what the lazy sweep does without a timer.

## Consequences

- This is a behavior change: a transaction idle longer than 60 s is
  reclaimed with `404`/`42N90`, and a 129th concurrent begin receives
  `429`/`42N91`. The defaults were chosen so the Browser bootstrap and
  `executeRead`/`executeWrite` flows, whose transactions are short-lived,
  are unaffected. A workload that legitimately holds a transaction open
  across a slow external call between statements must raise
  `--tx-idle-timeout-secs` (or disable expiry).
- `expires` becomes truthful. With expiry disabled it is omitted; the
  default keeps it present, so a client that reads it (Browser's
  bookkeeping only, per ADR-0237) sees the same member.
- ADR-0237's "we do not expire open transactions" sentence and its
  deferred "idle-timeout enforcement matching `expires`" alternative are
  superseded. Per the immutable-ADR rule, the implementing change adds a
  Later references note to ADR-0237 (accepted ADRs are not edited by this
  decision).
- At the defaults, one HTTP registry can hold at most 128 of the 256 pin
  slots. Engine pin exhaustion from non-transaction traffic remains
  possible, but leaking HTTP transactions alone can no longer reach it:
  the registry refuses first.
- The lazy sweep runs only on transaction-route operations. A process
  whose transaction traffic stops retains its expired entries (bounded by
  the cap) until the next transaction-route operation or shutdown; this is
  the deliberate cost of not running a background thread.
- The cap is per registry. Embedders running several HTTP servers divide
  the 256-slot budget between them; a single embedder's default of 128 is
  a ceiling, not a process-wide reservation.
- The over-cap refusal happens before pin acquisition, so the refusal
  itself consumes no pin.
- The cap is an exact bound, not a point-in-time check: an admitted
  begin holds its slot from `admit` through `register`, counted under the
  registry mutex and released by the guard on every exit in between, so
  the cap is never exceeded — including under a burst of concurrent
  begins.
- `docs/api/http/spec.md`, the `serve --help` text, and the flag table
  change in the same change as the code (AGENTS.md §3).
