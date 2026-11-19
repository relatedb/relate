---
type: adr
area: cli
claim: 794feb4f7da4f0c1f3fe84c367327e02a1b876e8
name: 0480-one-access-flag-read-only-freshness
status: proposed
tags:
  - commands
title: One access flag — `--read-only[=snapshot|live]`
REFERENCES:
  - "[[adr/0136-read-only-open]]"
  - "[[adr/0137-wal-follow]]"
  - "[[adr/0249-docker-unified-serve-image]]"
  - "[[adr/0306-read-only-follow-servers]]"
SUPERSEDES:
  - "[[adr/0136-read-only-open]]"
  - "[[adr/0306-read-only-follow-servers]]"
---

# ADR-0480: One access flag — `--read-only[=snapshot|live]`

## Context

The CLI spells its open's access mode with two flags. `--read-only` ([ADR-0136](./0136-read-only-open.md))
loads the store once and refuses writes; `--follow` ([ADR-0137](./0137-wal-follow.md) §1 — "follow is
read-only plus a tail") does that and tails the WAL beside a writer. Two flags
for one enum variant needed two extra rules to keep them coherent:

- an implication — "follow implies read-only";
- a precedence — "with both, follow wins" ([ADR-0306](./0306-read-only-follow-servers.md) §1, the canonical fold in
  `parse_error.rs::fold_access`, shared by the query session's pre-verb
  grammar and `serve`'s post-verb one).

That is surface invented to express one value. LABS-702 decided (2026-11-01)
to fold the pair into one flag with a freshness value.

## Decision

1. **One flag: `--read-only[=snapshot|live]`**, in both access-flag
   grammars — the query session's pre-verb flags (`flags.rs`) and `serve`'s
   post-verb flags (`serve_args.rs`). Bare `--read-only` and
   `--read-only=snapshot` are today's read-only open (a frozen snapshot at
 open, [ADR-0136](./0136-read-only-open.md)); `--read-only=live` is today's `--follow` (tail the WAL,
 [ADR-0137](./0137-wal-follow.md)). "snapshot" is [ADR-0136](./0136-read-only-open.md)'s own word; "live" is the user-facing
   word for the freshness.
2. **`--follow` is removed.** It answers with the generic unknown-flag error
   — no alias, no migration hint (AGENTS.md §3's rule for removed
   spellings). No users were known at removal.
3. **The value is attached-form only.** `--read-only live` (space-separated)
   is a usage error naming the correct spelling — a space-separated value
   must never silently become the database path or the query. A value
   outside `{snapshot, live}` is a usage error listing the two.
4. **Once-only in both grammars** — a repeat is a usage error naming the
 flag, the same policy as every optional `serve` flag ([ADR-0306](./0306-read-only-follow-servers.md) §1). The
   implication and the precedence rules die with the second flag; the
   canonical fold is deleted.
5. **Position rules stay** ([ADR-0306](./0306-read-only-follow-servers.md) §6): a pre-verb access flag on a
   `serve` line is a usage error naming the post-verb spelling, and a
   pre-verb access flag on a lifecycle command that runs no query engine is
   a usage error — a silently-writable server or a silently-dropped fence
   is the hazard both rules exist to prevent.
6. **Internal names stay**: `OpenMode::ReadOnly` / `OpenMode::Follow`, and
   "WAL follow" in prose. "live" and "snapshot" are the user-facing words
   only; no engine or API identifier renames.
7. **The `live` refusal is unchanged in kind and named against the value**:
   `.cl` bundles and `:memory:` refuse `--read-only=live` with the engine's
   `Error::Semantic` (today's `--follow` refusal), the message naming the
   value spelling.
8. **Cross-surface spelling.** `--follow` was CLI-only — the C ABI's
   `cypherlite_database_open_read_only` and Python's `read_only=True` have
   no follow. If a surface ever gains the freshness axis, it spells it as a
   value on the same read-only axis (a mode argument), never a second flag
   with an implication rule. The per-query read-only scope (LABS-692) must
   NOT reuse the `--read-only` spelling — it stays in the engine and API as
   `begin(read_only=True)`-shaped, per that ticket's design note.
9. **The Docker entrypoint pass-through** ([ADR-0249](./0249-docker-unified-serve-image.md), restated by [ADR-0306](./0306-read-only-follow-servers.md)
   §7) keeps `--read-only` in its valueless set: the bare form must never
   consume the next token (`--read-only --auth x` stays two flags), and
   the entrypoint's `=`-arm already splits the attached
   `--read-only=live` into flag and value, re-rendered byte-identically.
   The spaced `--read-only live` therefore reaches the CLI as two tokens,
   which its grammar refuses loudly — the entrypoint never validates
   values. `--follow` stays a valueless pass-through spelling so it
   forwards verbatim and the CLI's unknown-flag error can name it
   exactly; it is no longer a flag the server accepts.

## Alternatives considered

- **Keep the two flags** — rejected: keeps the implication rule and the
  precedence rule, the class of subtle rule that generates doc paragraphs
  and bugs, and costs two `--help` rows plus a cross-reference for one enum
  variant.
- **`--follow=snapshot|live`** — rejected: names the flag after one
  variant's mechanic rather than the axis; `--follow=snapshot` is a
  contradiction in terms (following is the live behavior).
- **A separate `--live` flag** — rejected: another flag-plus-implication
  pair — the same disease, one more spelling to keep aligned.
- **Keep `--follow` as an alias of `--read-only=live`** — rejected: a
  hidden second spelling recreates the vocabulary drift this fold removes,
  and removed spellings get no migration hints (AGENTS.md §3).

## Consequences

- One `--help` line per grammar instead of two flags and a cross-reference;
  the canonical fold (`fold_access`), the `AccessFlag` pair vocabulary, and
  the serve/query repeat-policy split are deleted in favor of one once-only
  grammar over a freshness value.
- The conflict and vocabulary messages name one spelling.
- A future freshness value (bounded staleness, an explicit follow gap)
  lands as a new value on the same flag, not a new flag and a new
  implication rule.
- The CLI spec's flag tables, the serve synopsis, the bundled skill
  references that name the flags, and the e2e pins move to the new
  spelling; `--follow` pins become unknown-flag pins.
- Supersedes the flag *spelling* in [ADR-0136](./0136-read-only-open.md) (§ the read-only open) and
 [ADR-0306](./0306-read-only-follow-servers.md) (§1 the two serve flags, §7 the valueless pass-through) — both
  carry a Later-references note; nothing else in either decision changes.
