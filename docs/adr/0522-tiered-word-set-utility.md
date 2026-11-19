---
type: adr
area: engine
claim: ba82fe59dfef039f9787e5236e1b65a185980ffa
name: 0522-tiered-word-set-utility
status: proposed
tags:
  - executor
  - perf
title: A tiered word-set utility beside the distinct set
REFERENCES:
  - "[[adr/0507-tiered-distinct-set]]"
---

# ADR-0522: A tiered word-set utility beside the distinct set

## Context

[ADR-0507](./0507-tiered-distinct-set.md)'s `DistinctSet` bundles two concerns in one module:

1. the **DISTINCT-site API** — value and row keys through
   `GroupKey::class`, the null flag, the general single-key and row-key
   tables, the workspace-charge contract callers rely on; and
2. the **tiered storage mechanics for one key class's raw `u64` words** —
   the inline `[u64; 16]` tier with its branch-free 16-lane probe, the
   promotion into a presized hash table, the charge-before-write
   rollback, and the stride-8 accounting both tiers share.

The second concern is useful well beyond DISTINCT. The label-union
planner work needs to merge candidate sets over raw node ids with the
same shape — the zero-allocation inline tier for the common
handful-of-keys case, one table past the boundary — and future raw-key
consumers (frontier dedup, per-branch used-rel sets) want the same.
But [ADR-0507](./0507-tiered-distinct-set.md) §1 rightly keeps `DistinctSet` itself out of the pattern
walker: the set carries DISTINCT semantics, and its boundary rule
(`distinct_set` imports only `group_key`, `workspace`'s charge helpers,
the hasher seam and the core crate — never `exec/` or `pattern_walk`)
exists so the dedup structure never becomes a path-engine dependency.

So the tiering is needed in places the distinct set must not reach.

## Decision

### 1. One crate-level utility: `tiered_set.rs`

`relate-executor/src/tiered_set.rs` defines `TieredWordSet` — the
tiered storage of exactly one key class's words, and nothing else:

- **T1** — up to `2^4 = 16` words inline in `[u64; 16]`, the
  fixed-length mask-compare probe, no heap and no hash;
- **T2** — one hash table per set, presized to `2^5 = 32`, the hasher a
  construction-time policy (`WordHash::Id`, keyless Fx, for
  engine-allocated ids; `WordHash::Value`, the process-wide seeded
 state, for client-derivable words — [ADR-0507](./0507-tiered-distinct-set.md) §4's split, moved down
  one level with the table it governs);
- the API is `insert(word) -> Result<bool>`, `len`, `bytes`,
  `promoted`, plus `promote_inline` (the caller-forced promotion) and
  the `clone_contents`/`try_clone` pair the distinct set's clone
  contract consumes.

The set knows raw `u64` words only — no `Value`, no `GroupKey`, no
`WordTag`, no DISTINCT semantics. Which words belong to which
instance, and what a switch between instances means, is the caller's
model. It owns its workspace charge: each insert is one probe charging
the delta of the representation that took the word — before the write
in the inline tier (a refused charge leaves the tier `Empty` or
untouched, [ADR-0507](./0507-tiered-distinct-set.md) §5's rollback property, preserved exactly), the
word removed again in the table tier.

The utility carries [ADR-0507](./0507-tiered-distinct-set.md) §1's import discipline unchanged: only
`workspace`'s charge and bytes helpers, the hasher seam and the core
crate — never the exec tree, never the pattern walker. The boundary
unit test moved with the mechanics: the module's own test greps its
source for those paths and fails if either appears.

### 2. `DistinctSet` delegates storage, keeps the model

`distinct_set.rs` keeps its public API, its `WordTag` model, the
general single-key and row-key tables, and the exhaustive tag→field
compile guard. Its word storage becomes `TieredWordSet`:

- one **active** tier held inline in the set — the current tag's words,
  the one inline phase;
- on a tag switch, the active tier is promoted and **parked** in its
  tag's spill slot (moved wholesale), and the incoming tag's parked
  tier — or a fresh one — takes the active seat. At most one inline
 tier exists at any moment, exactly [ADR-0507](./0507-tiered-distinct-set.md) §6's tag-homogeneous
  inline phase.

The active tier sits in the set rather than one tier per tag because
five inline tiers would grow `DistinctSet` roughly fivefold (about
780 B), tripling the per-group aggregate footprint [ADR-0507](./0507-tiered-distinct-set.md) §3
explicitly bounded when it rejected the 2^5 inline tier, and tipping
the large-variant lint in the aggregate, cap and union enums that hold
a set. One active seat plus parked spill tiers keeps the footprint at
one tier plus a word of tag state.

`distinct_set_tests.rs` passes unchanged — the behavior-identical
proof: membership, `len`, the `bytes` oracle, the charged == bytes
invariant after every offer, the refusal rollback in every tier state,
the clone charge contract, and the promotion boundaries all pin the
delegation.

### 3. The boundary, widened one way

`pattern_walk` (and any other raw-key consumer) may import
`crate::tiered_set` — the utility carries no DISTINCT semantics, so
the reason [ADR-0507](./0507-tiered-distinct-set.md) §1 barred the import is gone. `DistinctSet`
remains barred: the path engine still never touches the distinct set,
its key classes, or its general tables.

## Alternatives considered

- **Duplicate the tiering inside the pattern walker.** Rejected: a
  second copy of one storage policy — the boundary constant, the
  promotion move, the rollback and the stride-8 accounting would drift
  apart on the next fix to either site.
- **Relax [ADR-0507](./0507-tiered-distinct-set.md) §1 and let `pattern_walk` import `DistinctSet`.**
  Rejected: it couples DISTINCT semantics (`GroupKey` classification,
  the null flag, the general tables) into the path engine, and drags
  the walker into the distinct set's boundary obligations — the exact
  coupling §1 exists to prevent.
- **No utility; the label-union work uses a plain `HashSet`.**
  Rejected: it loses the zero-allocation inline tier for the common
  small case, which is the point of the candidate-merge shape, and
  still leaves the tiering stranded inside the distinct set.
- **Five `TieredWordSet`s inlined in `DistinctSet`, one per tag, each
  with its own inline tier.** Rejected: it grows `DistinctSet` about
 fivefold, multiplying the per-group aggregate footprint [ADR-0507](./0507-tiered-distinct-set.md) §3
  bounded and failing the large-variant lint in the aggregate, cap and
  union enums; the active-seat-plus-parked-spill shape delivers the
  same per-tag storage at one tier's footprint.

## Consequences

- The tier mechanics now have one spelling, reusable by raw-key
  consumers; `DistinctSet` is a policy layer — the tag model, the key
  classes, the general tables — over the utility.
- `DistinctSet` grows by the active tier's hasher and charge fields and
  one `Option<WordTag>` of tag state (about 8 B); the per-group
  aggregate footprint is unchanged in kind, and the memory-shape lane
  should confirm it stays so.
- A tag switch now moves one `TieredWordSet` wholesale into its spill
  slot instead of re-inserting the held words into a fresh table — the
  observable figures are identical (both tiers charge the same
  stride-8 figure, promotion costs nothing), which the unchanged
  distinct-set tests pin.
- The utility's `promoted` probe is consulted only by the distinct
  set's test tier gate today; the raw-key consumers arrive with their
  own call sites, carried until then under the repo's established
  test-only exemption idiom.
- The seam [ADR-0507](./0507-tiered-distinct-set.md) §3 left for a dense id bitset (the `Id` table)
  now lives one level down, in the utility's `WordTable` — a future
  bitset tier is a change to the utility, shared by every consumer.
