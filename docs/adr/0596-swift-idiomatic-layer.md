# ADR-0596: An idiomatic layer over the Swift surface

Status: Proposed
Claim: f2fd14e96aa08df0c82a900c4fd81723f93f934a

## Context

ADR-0433 reinstated Swift as a faithful wrapper of the tabular C ABI. Its
design record chose "synchronous core API; apps call it from their own
actor" and deferred `Codable` rows. Two consumers written since, the
in-repository iOS example and an external iOS app, each hand-built the same
layer on top: an actor owning the one connection, a row decoder, parameter
wrapping, timing logs, a first-launch store copy, and conversions for dates
and `Float` embeddings. Four public type names also collide with Apple's:
`Notification` (Foundation), `Namespace` (SwiftUI), `Category` (the
Objective-C runtime) and, by association, `Severity`.

## Decision

Add that layer to the package. The C ABI wrapper stays the only path to the
engine; nothing here adds a protocol or needs an ABI change.

1. **Managed asynchronous access.** `Database` gains asynchronous read and
   write entries. Each runs a *synchronous* body on a serial queue owned by a
   pooled connection (one writer, a bounded set of readers). This supersedes
   "synchronous core API" in ADR-0433's design record. The caller's task
   suspends while that queue blocks in the engine; a running statement
   still cannot be cancelled. The synchronous API remains.
2. **Reads and writes are transactions.** A read always rolls back: the ABI
   has no read-only connection and reports a query type only after the
   statement ran, so rolling back is what guarantees one snapshot and that
   nothing commits. A write commits when its body returns, with an optional
   bounded re-run on the engine's `retryable` verdict. A body receives a
   scope that can only run statements.
3. **Poison recovery by probe.** The error channel has no typed poison
   signal, and matching message text is forbidden. After a boundary-stage
   failure the managed layer probes the connection with a trivial statement
   and replaces it if that fails too. The failed work is never retried.
4. **Values in.** One conversion protocol turns Swift values into parameters
   and back out of results; `Encodable` values encode to a map. A statement
   value type is expressible by string interpolation, where an interpolated
   value becomes a bound parameter. Its entry point never shares a name with
   the `String` one: an overload would resolve an interpolated literal to
   `String` and inline the value into the statement text.
5. **Values out are records.** A result is a collection of `Record` views
   with typed, throwing accessors that name the column on a mismatch, and
   the existing record accessor returns them in place of dictionaries.
   "Record" is what the Neo4j drivers and this project's Python client call
   one row of a result, so a caller who knows either finds the same word
   here. Records decode to `Decodable` types strictly: an integer widens to
   a float and nothing else converts.
6. **Bridges, in the one module**: `Foundation.Date` (by proleptic-Gregorian
   arithmetic, since Foundation's calendar is Julian before 1582),
   `Swift.Duration`, `Float` vectors, latitude/longitude points, and Core ML
   and Core Location under a conditional import.
7. **Names.** `Notification`, `Namespace`, `Severity` and `Category` become
   `QueryNotification`, `DatabaseNamespace`, `NotificationSeverity` and
   `NotificationCategory`. The first two are removed outright, because an
   alias of either keeps the ambiguity; the last two keep deprecated aliases.
8. **Errors.** Calls whose only failure is the package's own error throw it
   typed. Predicates are read off the engine's code, each exercised against
   the live engine in the package's tests.
9. **Observation, provisioning, migration.** Per `Database` instance and
   in-process: a stream of commits with caller-supplied tags, and a trace
   callback that never carries parameter values. A helper seeds a store from
   a bundled copy, and a migrator runs idempotent schema statements plus
   versioned steps.

## Alternatives considered

- **An actor-based asynchronous API.** Rejected: a blocking engine call
  stalls the cooperative pool, custom executors are unavailable at the
  deployment floor, and an asynchronous body could hold a transaction across
  a suspension.
- **A separate pool type, or an add-on package.** Rejected: a second object
  for every app to pass around; and the renames, transaction scope and
  record collection change the package's own types.
- **Read-only from the query type; poison from the message.** Rejected: the
  type arrives after the statement committed, and the message is string
  discrimination over a typed channel.
- **Naming the result view `Row`.** Rejected: a word of this client's own
  where two established clients already say `Record`.

## Consequences

- Source-breaking in a minor release: the two removed names, the transaction
  body's parameter type, the record accessor's element type (a record still
  reads by column name and offers its dictionary), and two new cases on the
  package's error enum.
- The Swift surface gains conveniences the other clients lack; result
  metadata stays aligned. The commit stream sees only its own instance.
- Four ABI additions would simplify this layer, each its own decision: a
  typed poison signal, cancellation, a read-only connection, a bulk vector
  read.
