# ADR-0542: Engine-only, callable, unlisted built-in functions

Status: Proposed

Claim: 07e17f5a9c9169de7172dc5790d999b6e353209c

## Context

ADR-0497 aligned the built-in function catalogue with the Cypher-25 corpus
and settled four release-status rules: a name is default-surface (implemented,
advertised, callable), preview-gated, removed, or added from the corpus. All
four assume that every callable name is either part of the corpus or on its
way out of the registry. The only callable-but-unlisted names so far are the
lowering's internal targets, which users never write.

The engine now wants a small presentation helper that renders a number as a
text bar, useful in the shell and in cookbook output. It exists in no
reference server and no approved CIP. Listing it would add a row to `SHOW
FUNCTIONS` that the corpus does not have, and the spec harness compares that
catalogue row for row. Gating it behind a preview flag would hide it from the
users it is for, and the flag mechanism is reserved for features that track a
future corpus. None of the four rules fits.

## Decision

Introduce a fifth admission class, **engine-only, callable, unlisted**: a
built-in function that the engine owns, that is not part of the Cypher-25
corpus, and that works in every build and dialect without an opt-in, but that
is absent from the `SHOW FUNCTIONS` catalogue. It is a recorded exception to
ADR-0497's four rules, not a change to them.

- **Mechanism.** The name joins the executor's builtin registry and the
  front's unadvertised-name list, the same list the lowering-internal names
  use. It is not in the advertised list and not in the preview-gated table.
  The planner binds it; the catalogue never shows it.
- **Admission test.** A name qualifies only if it is not in the corpus, it
  has no preview-flag lifecycle (no CIP to track), and its behaviour is fully
  specified by the engine's own documentation. A name the corpus or an
  approved CIP carries follows ADR-0497 instead.
- **Disclosure.** Because the catalogue is silent, the functions reference
  carries a clearly labelled section for unlisted functions. That section is
  the only place such a name is documented.
- **Contract.** An engine-only function is held to the same standard as a
  catalogue function: typed errors, no silent defaults, bounded output, and
  its own row-level argument contract. Its arity is not "verified against a
  reference server", so the registry's lockstep test pins its names directly.
- **First member.** The text-bar function: it takes a value, a range and a
  width and returns a string of block characters at one-eighth cell
  resolution. Any null argument yields null; non-numeric arguments, non-finite
  numbers, an empty or inverted range, and a width outside a fixed cap are
  typed errors.
- **Exit.** If a reference server or an approved CIP later adds a function by
  the same name, ADR-0497's rules apply from then on and this class no longer
  covers it; the semantics must then be reconciled by a new ADR.

## Alternatives considered

- **Advertise it in the catalogue.** Rejected: it adds a row the corpus lacks
  and regresses the harness's catalogue comparison.
- **Gate it behind a preview flag.** Rejected: the flag registry exists for
  names tracking a future corpus, and an opt-in hides a helper that has no
  lifecycle to graduate through.
- **Keep it out of the engine and format in clients.** Rejected: query
  results would need client-side post-processing in every surface, and the
  value is a string the engine can produce deterministically.
- **Ship it as an extension.** Rejected: extensions are spawned child
  processes with their own wire contract, which is far heavier than a pure
  scalar function.

## Consequences

- The set of callable names is no longer a subset of the advertised names
  plus lowering internals; the unadvertised list now has a second, documented
  reason to exist, and review must check new entries against the admission
  test above.
- Discoverability relies on the functions reference, not on `SHOW FUNCTIONS`.
- Clients and tools that enumerate the catalogue will not see these names;
  that is intended.
- The registry's lockstep test carries one explicit engine-only arity whose
  names are listed in the test rather than derived from a reference spec.
- ADR-0497 gains a Later references note pointing here.
