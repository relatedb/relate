# ADR-0597: A closed import window for git-to-graph

Status: Proposed
Claim: de82f2bf74a9c7a5842d04c22d256923400bef80

## Context

ADR-0376 pinned the importer's history window to two flags, `--max-commits`
and `--since`, and both select ranges that end at HEAD. Two consequences
follow. An older era is unreachable: reading 2016–2020 with `--since` alone
means reading everything since 2016, and `--max-commits` counts down from
HEAD, so reaching 2018 means reading every commit after it. And `--since`
is a moving window: the same command reads more history every month, so a
rebuild that once fit under ADR-0377's 128 MiB `git log` cap can later
exceed it — a store's rebuild headroom shrinks by itself. A closed `[A, B]`
range is reproducible forever, however much history lands after B.

ADR-0377's design constraint "no new flag" was a constraint on resource
bounds — its rejected alternative was one cap flag per limit. A window
bound is not a cap: it is the same class of flag as `--since`, selecting
which commits are read rather than bounding what a read may cost.

The importer is now an extension package (ADR-0431 §11): its flags are
manifest rows the host parses by kind (ADR-0429 §5) and lowers through the
typed envelope (ADR-0430 §6), so a new flag rides existing machinery on
both sides of the wire.

## Decision

`--until DATE` joins the window flags with exactly `--since`'s mechanics:
a manifest string flag whose default is empty; empty means no bound; a
non-empty value forwards to git as `--until=DATE`, one argv element,
present only when given. Git filters by committer date, the same date
`--since` filters by, so a rebase or cherry-pick can place a commit
outside the window its author date implies. No value validation is added
beyond non-empty: git rejects a malformed date with its own stderr, as it
already does for `--since`.

`--since A --until B` is the closed range; `--until` alone reads an era
that need not end at HEAD. This supersedes ADR-0376's command shape and
pinned argv — each gains one optional element — and ADR-0377's
`LogTooLarge` refusal text, which now names `--until` among the flags that
read a smaller window, keeping that message's contract true: every message
names the flag that changes it. The package version moves to 0.14.0: the
channel tags one release per version, so changed bytes need a new tag.

## Alternatives considered

- **`--before` as a second spelling, git's own alias for `--until`.** One
  canonical name per concept; two spellings double the flag table, the
  help text and the envelope contract for no new capability. Rejected: the
  `--since` precedent is one name, and ADR-0383 records what a spelling
  supersession costs.
- **Author-date windows.** `authored_at` is the stored date, but git's
  window flags filter committer date; author-date filtering is a different
  reader with its own cap arithmetic. Rejected: the unreachable-era and
  moving-window problems are about which commits are read, and the
  committer date is the date the existing window already uses.
- **A revision range as the positional.** The manifest carries one
  positional (DIR); a range argument changes the command shape and still
  cannot express calendar eras. Rejected: dates are the vocabulary the
  existing window speaks.
- **Accumulate `CO_CHANGED` counts across disjoint slices.** The natural
  pairing — today counts are recomputed and `SET`, so disjoint slices
  overwrite each other — but that changes ADR-0376's pinned
  `MERGE`-plus-`SET` semantics and is a decision of its own. Rejected for
  this change: the closed window is useful alone; count accumulation gets
  its own ADR if it is wanted.

## Consequences

- ADR-0376 and ADR-0377 take Later-references notes pointing here. Their
  caps, numbers and every other refusal text stand; only the flag table,
  the pinned argv and the one refusal hint change.
- A host older than this package fails `--until` closed as an unknown
  flag (ADR-0429 §5); installing the bumped package is the upgrade path.
- An old era imports reproducibly, and a closed range cannot outgrow the
  log cap. A `--since`-only command still drifts — that moving window is
  unchanged; the closed range is the reproducible spelling.
- Docs and tests land with the flag: the CLI run example, the reference
  flag row and the skill-bundle catalog gain `--until`; argv pinning, a
  closed-range reader test, envelope lowering and an end-to-end
  middle-slice import through the real mount cover the wire.
