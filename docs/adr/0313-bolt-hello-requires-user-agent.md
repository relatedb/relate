---
type: adr
area: surface
claim: 6c6b9302ed5eca2ecab781f84c2ae21fe4f3eaff
name: 0313-bolt-hello-requires-user-agent
status: proposed
tags:
  - bolt
  - conformance
title: Bolt HELLO requires user_agent
REFERENCES:
  - "[[adr/0126-bolt-basic-auth]]"
  - "[[adr/0139-bolt-failed-hello-delay]]"
  - "[[adr/0293-configurable-bolt-hello-identity]]"
  - "[[adr/0312-bolt-hello-recv-timeout-hint-and-noop-keepalive]]"
---

# ADR-0313: Bolt HELLO requires user_agent

## Context

Bolt 4.4 makes `user_agent` a required string in the HELLO extra map.
Latest Neo4j (LABS-323 findings F-09, F-68) rejects a HELLO without it:

- HELLO answers FAILURE `Neo.ClientError.Request.Invalid`, message
  `Illegal value for field "user_agent": Expected value to be non-null`,
  and no session opens.
- Every following message answers IGNORED (the probe's RESET).
- A second HELLO answers FAILURE `Message of type HelloMessage cannot be
  handled by a session in the AUTHENTICATION state.` and the server
  closes.

cypherlite-bolt accepts any HELLO map, so a client that omits the field
gets a working session here and a failure against Neo4j. Every official
driver sends `user_agent`. In-tree raw clients (test helpers, loadtest
clients) send empty HELLO maps today.

## Decision

1. A HELLO whose extra map has no string `user_agent` (absent, `null` or
 not a string) is rejected **before** the [ADR-0126](./0126-bolt-basic-auth.md) auth check: FAILURE
   `{code: "Neo.ClientError.Request.Invalid", message: "Illegal value for
   field \"user_agent\": Expected value to be non-null"}`. No session
 opens and the [ADR-0139](./0139-bolt-failed-hello-delay.md) failed-auth delay does not apply.
2. The connection stays open in a **rejected-HELLO state**. Every message
   except HELLO and GOODBYE answers IGNORED. A second HELLO answers
   FAILURE `Neo.ClientError.Request.Invalid` with `Message of type
   HelloMessage cannot be handled by a session in the AUTHENTICATION
   state.`, and the server closes. GOODBYE closes.
3. A HELLO with a string `user_agent` behaves as before: the [ADR-0126](./0126-bolt-basic-auth.md) auth
 check, then the [ADR-0293](./0293-configurable-bolt-hello-identity.md) metadata (plus the [ADR-0312](./0312-bolt-hello-recv-timeout-hint-and-noop-keepalive.md) hints).
4. The code constant is `REQUEST_INVALID_CODE` in `failures.rs`, the one
   line shared with the failure-code mapping work.
5. In-tree HELLO senders without `user_agent` (the bolt crate's test
   helpers, the shared test-support helpers, the CLI's Bolt e2e tests and
   the dataset loadtest client) gain one.

## Alternatives considered

- **Accept a missing `user_agent` with a warning.** Rejected: the goal is
  Neo4j's behaviour at Bolt 4.4, and a client that works here but fails on
  Neo4j hides its own bug.
- **Close the connection on the first rejected HELLO.** Rejected: Neo4j
  keeps it open and answers IGNORED, and drivers read the FAILURE before
  deciding to close.
- **Check auth first.** Rejected: Neo4j validates the message fields
  before authenticating, and a malformed HELLO is not a credential guess,
 so the [ADR-0139](./0139-bolt-failed-hello-delay.md) delay is not owed.

## Consequences

- Clients that omit `user_agent` now fail at HELLO with Neo4j's code and
  text. Official drivers are unaffected.
- The session gains a rejected-HELLO state beside the [ADR-0126](./0126-bolt-basic-auth.md) pre-HELLO
  gate.
- Pinned by `tests/compat/scripts/protocol/hello-requires-user-agent.bolt`
  and the `protocol_hello_tests.rs` unit tests.
