# ADR-0569: Integer division overflow is a checked error

Status: Proposed
Claim: 79dee92b635bbb2602736138fb7785db865b57f5

## Context

Cypher integer arithmetic is checked: an operation whose result cannot be
represented as a 64-bit integer is a runtime failure in live Neo4j
2026.10.1 (GQLSTATUS 22003, "numeric value out of range"). The engine
raises it for `+`, `-`, `*` and unary minus, but ADR-0175 recorded one
exception: integer division let `i64::MIN / -1` wrap to `i64::MIN`,
reasoning that Java's plain `/` and Rust's `wrapping_div` produce the same
two's-complement result.

Live Neo4j 2026.10.1 no longer behaves that way: the operation raises
22003 ("The numeric value -9223372036854775808 / -1 is outside the
required range"), and the openCypher corpus pins the failure. The wrap
also answers a value outside the representable range without saying so.
Integer modulo keeps the wrap: live still answers
`-9223372036854775808 % -1` as 0, which is exact.

## Decision

Integer division is checked like the other integer arithmetic operations:
the one overflowing operand pair (`i64::MIN / -1`) is a runtime error in
the type class the engine's other integer overflows already use. Modulo
keeps its wrapping result.

## Alternatives considered

- **Keep the wrap (status quo).** Rejected: live Neo4j 2026.10.1 raises
  22003 for the same expression and the corpus pins the error; a wrapped
  answer silently reports a value outside the representable range.
- **Add a new arithmetic error variant for the case.** Rejected: the
  engine's existing integer-overflow type error already carries every
  other arithmetic overflow site and is one of the classes the reference
  reports; a second variant would fork the class table for one operand
  pair.
- **Check modulo as well.** Rejected: live answers
  `-9223372036854775808 % -1` as 0 — the exact result — so there is no
  overflow to report.

## Consequences

- Integer division reports its overflow through the same type-class error
  the other integer arithmetic operations use.
- ADR-0175's division bullet is superseded; the rest of that ADR stands.
