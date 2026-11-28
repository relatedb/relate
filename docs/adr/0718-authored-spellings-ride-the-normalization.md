---
type: adr
area: engine
claim: 2ff0b406d7350fa38e15a603f4bb51b31de7c1c5
date: 2026-11-28
deciders: Relate maintainers
name: 0718-authored-spellings-ride-the-normalization
status: proposed
tags:
  - perf
  - planner
title: The authored column spelling rides the normalization output; the result epilogue never re-parses
REFERENCES:
  - "[[adr/0503-template-keyed-plan-cache]]"
  - "[[adr/0651-plan-cache-hit-verification-without-front-reparse]]"
  - "[[adr/0663-cache-hit-inline-route-for-shallow-bounded-plans]]"
---

# ADR-0718: The authored column spelling rides the normalization output

## Status

Proposed — retires the one remaining per-result parse on the cache-hit
path (ADR-0663's recorded follow-up, REL8-2009).

## Context

A plan compiled from the normalizer's rewritten text names its result
columns after the rewritten expressions, so an auto-extracted literal
appears in the user-visible column as its placeholder: `RETURN 1` answers
the column `$__AUTO_INT_0__`. ADR-0663 added a restore that re-parses the
statement's raw text through the front (`authored_column_names`) at
result time to recover the authored spelling — a full parse per
auto-parameterized result, plus the preview-options threading and the
parser's projection-shape model the restore had to carry to keep the
re-parse honest. The re-parse is also the stated reason the cache-hit
inline route's containment rule declines the entire auto-parameter
family: a hit must not run on a thread whose stack the re-parse's
recursion would land on (ADR-0663's third conjunct).

The normalizer already produces what the restore re-derives:
[`Normalized::spellings`] — the authored source text of each extracted
literal, in placeholder-index order. Placeholder names are
`$__AUTO_<KIND>_<n>__` with `n` the creation counter, and creation
follows token order, so `spellings[n]` is the authored text of the
placeholder the plan names. A plan column is the rewritten expression's
source text under the lowering's own `column_label` policy; substituting
each placeholder with its spelling reproduces the authored name through
data the admission already holds. No parse, no preview threading, no
projection-shape model — the same shape as ADR-0651's removal of the
admission re-parse: what a hit needs was computed once, before the plan
existed; carrying it beats recomputing it beside the result.

## Decision

1. **The spellings ride the normalization's existing channels.** Stage 2
   already normalizes: the hit payload and the insert seed carry
   `spellings` beside the values they already carry. Stage 1 (the raw
   fast path) does not normalize — its entry records the spellings once,
   at `note_raw`/insert time, beside the raw text and values it already
   records (the same text always extracts the same spellings, the same
   invariant the recorded values rest on). The admission hands them to
   the result epilogue as one field.

2. **The restore is a substitution.** A column that carries an
   auto-parameter placeholder has each placeholder replaced by its
   spelling — the one canonical substitution lives in the parser crate,
   beside the placeholder grammar it owns (`substitute_auto_parameters`).
   The trigger keeps ADR-0663's two conjuncts (a column carries the
   placeholder; the plan references an auto-parameter), and the
   substitution is total for every placeholder the plan can name — the
   spellings and the plan's placeholders come from the same
   normalization of the same text. `authored_renames` (the streamed
   path) applies the same substitution to the planned output names.

3. **The re-parse machinery is deleted.** `authored_column_names` and
   its parser-level twin `authored_column_label` leave the front (the
   lowering's own `column_label` stays — it names compiled plans); the
   preview threading into the naming path goes with them. The result
   epilogue's naming inputs become: the columns (or planned names) and
   the spellings — both already in hand.

4. **Accepted divergence, recorded here:** on a first execution the
   substituted name reproduces the source's spacing exactly — the splice
   preserves the literal span's trailing separator whitespace verbatim,
   so a multi-space original answers its own spacing (`RETURN 1  +  2`
   → `1  +  2`, `RETURN 1+2` → `1+2`). On a **template hit** the plan
   is shared across the template family and was compiled from the FIRST
   statement's rewritten text: the current statement contributes its
   literals (the spellings — always exact), the plan its between-token
   spacing. `RETURN 1+2` hitting the template of `RETURN 1 + 2` answers
   `1 + 2` where the re-parse answered `1+2` — a display-name nuance in
   result metadata, inherent to plan sharing (the same plan serves the
   family), and the one behavior change this ADR makes against the
   re-parse. The splice's glue rule (an added space where a literal was
   glued to an identifier-shaped token — degenerate adjacency the lexer
   would otherwise merge into one parameter name) is the only other
   divergence, in source text that barely parses. Aliased columns are
   untouched (an alias never carries a placeholder).

## Consequences

- The auto-parameterized result path runs no parse after the admission:
   the last per-result front invocation on the cache-hit path is gone,
   alongside ADR-0651's removal of the per-hit admission re-parse. The
   naming cost becomes a bounded string substitution over the result's
   own column names.
- The spellings are stored state: the plan-cache entry's raw record
   grows by the spellings (≤ the raw text's own bytes, the same bound
   the recorded values rest on), charged by the entry's logical-byte
   accounting like the values are.
- The cache-hit inline route's third containment conjunct
   (`plan_references_auto_parameter`) loses its stated rationale — the
   re-parse it guards against no longer exists. Removing it (widening
   the route to the auto-parameter family) is a separate change with its
   own tests and measurement: this ADR only removes the dependency, and
   the conjunct stays in force until that change lands.
- The parser's projection-shape model no longer has a naming consumer;
   it remains the parser's own model for what a statement's final
   `RETURN` projects (its `Wildcard`/`Omitted` distinction is the
   lowered shape's, not the restore's).
- The substitution is the one home for the authored spelling of a
   placeholder: any future consumer that needs the authored text of an
   extracted literal consults the spellings, never a re-parse.
