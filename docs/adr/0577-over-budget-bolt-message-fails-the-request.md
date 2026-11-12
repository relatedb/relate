# 0577. An over-budget Bolt message fails its request instead of closing the connection

- **Status**: Proposed
Claim: 0b42995f4b1002d64c698e21154d08d2ce8a2b01
- **Date**: 2026-11-11
- **Deciders**: the maintainers

## Context

ADR-0082 bounds a decoded Bolt message to a fixed number of PackStream values
and says an over-budget message "ends only its Bolt connection", like any
other malformed message. That is right for a message that is not valid
PackStream: the stream may be out of sync and nothing can be answered.

It is wrong for a message that is valid and merely large. The framing layer
has already read it whole, so the connection is in sync. Closing it gives the
client no reason. Drivers read a dropped connection as a transient network
failure and retry the same request, which can never succeed. A client loading
rows in large batches sees repeated defunct-connection errors and retries
while the server logs a terse limit message nobody reads.

## Decision

A message that decodes cleanly up to the value budget and then exceeds it is a
rejected request, not a protocol breach:

- The session answers with a FAILURE carrying the request-invalid status code
  and an authored message that states what was measured (message bytes and a
  lower bound on decoded values), the per-message limit, how values are
  counted, and the remedy: send fewer rows per request.
- The session enters the failed state (ADR-0110). Messages pipelined behind it
  answer IGNORED until RESET, exactly as after any failed request, and the same
  connection then serves the next request.
- Before the session is open there is no state to fail, so each gate answers in
  its own policy: the pre-HELLO gate sends the unauthorized FAILURE and closes
  (ADR-0126), the rejected-HELLO state answers IGNORED (ADR-0313), and the 5.x
  AUTHENTICATION gate sends the limit FAILURE and closes (ADR-0484).
- A message queued ahead of a RESET in the same batch answers IGNORED, as every
  interruptible message does (ADR-0317).
- Every other decode error stays fatal (ADR-0315): the decoder reports the
  budget as its own typed error, distinct from malformed input, so the session
  never discriminates by message text.

The budget itself, the wire-size cap and the depth cap are unchanged. The
reported value count is a lower bound because the decoder stops at the limit
rather than scanning the rest of the message.

## Alternatives considered

- **Keep closing the connection.** Rejected: it triggers the driver's retry
  loop and hides the cause.
- **Decode the rest of the message to report an exact count.** Rejected: it
  spends memory and time on the very input the budget exists to refuse, for
  a number that does not change what the client must do.
- **Raise the budget only.** Rejected as the fix: any fixed count is arbitrary
  and the next larger batch fails the same way. Sizing the budget by decoded
  bytes is a separate decision.

## Consequences

- A client over the budget gets one non-retryable client error naming the limit
  and the remedy, and its connection survives.
- The Bolt specification's statement that the limit terminates the connection
  changes to this behavior.
- ADR-0082's "ends only its Bolt connection" no longer applies to the value
  budget; its counting rule and limit stand.
