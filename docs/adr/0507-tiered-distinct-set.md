# ADR-0507: One tiered distinct set under the DISTINCT sites

Status: Proposed

Claim: 08d7746898d6ab09032653b704cd4cc8b5b8d505

## Context

LABS-852's motivating shape, `MATCH (s:User {id: $id})-->()-->()-->()-->(n:User) RETURN DISTINCT n.id`
on a 1.8M-edge social-network store (`$id = 7`), spends 862 ms: 722 ms walking 663,730 path
rows and 102 ms in the DISTINCT dedup that leaves 44,674 rows (14.9x
collapse). The large micro-shape `MATCH (n:User) RETURN DISTINCT n.age`
reduces 100,000 rows to 94 in 71 ms. This ADR covers only the mechanical
part, the distinct set. Path-engine pruning is out of scope and left to an
engine follow-up: per-level frontier dedup, a DISTINCT-on-start semi-join,
normalising chains into quantified patterns, and a per-branch used-rel set.

Five sites dedup today, all keyed by `GroupKey(Norm)`
(`cypherlite-executor/src/group_key.rs:290`). It has a derived
`Hash + Eq` and is the one equivalence relation for DISTINCT, UNION,
GROUP BY and the property index:

1. RETURN/WITH DISTINCT projection, `exec/project.rs:670-692`:
   `HashSet<Vec<GroupKey>>`, charged through
   `workspace::insert_row_key_with_charge`. With `*`, the columns come from
   `visible_keys(&row)` per row, so row arity varies within one set.
2. UNION dedup: `exec/mod.rs` `fold_parts` (697), `union_merge` (787), and
   subqueries through `exec/call/mod.rs:416`. Uses
   `HashSet<Vec<GroupKey>>` and charges no workspace.
3. `count(DISTINCT …)` seats in `exec/agg_fold/acc.rs` `DistinctAcc`
   (187). `DistinctKind::Ints(HashSet<i64>)` is a fast path that promotes
   to `Keys(HashSet<GroupKey>)` by re-keying every held int. The workspace
   charge rolls back if the budget refuses an insert.
4. `workspace::dedup_first_seen` (`workspace.rs:729`). The one first-seen
   value-list dedup, used by `coll.distinct` and the batch aggregate's
   DISTINCT operand. Uses `HashSet<GroupKey>`.
5. `exec/match_cap.rs` `DistinctLimitCap`. Uses `HashSet<Vec<GroupKey>>`,
   bounded by the LIMIT and uncharged.

Per row, the hot loop (site 1) costs:

- a `Vec<Value>` clone of the projected columns;
- a `Vec<GroupKey>` heap allocation, even for one column;
- under workspace accounting, two SipHash-1-3 probes (`contains`, then
  `insert`), each over the derived hash's discriminant-plus-payload writes;
- a table entry of `size_of::<Vec<GroupKey>>()` plus the key's heap.

At 102 ms / 663,730 rows that is about 150 ns per row. Most of it is
allocation and hashing, not the table.

Every site hashes with std `RandomState`. `rustc-hash` (Fx) is already a
workspace dependency, used for the engine-id `CoreMap` tables. The
walker's used-sets already share one process-wide `RandomState`
(`pattern_walk/mod.rs:345`, LABS-822), which avoids a per-set
thread-local seed read.

What must stay the same: semantics identical to `GroupKey`.

- Null is reflexive: all nulls are one key.
- Every NaN is its own key, nested ones included, through the
  process-global nonce. Neo4j groups NaN with itself on some paths, but
  ours is pinned and stays.
- Numbers compare by mathematical value: `1 ≡ 1.0`, `+0.0 ≡ -0.0 ≡ 0`, and
  non-integral or out-of-range floats compare by IEEE bits.
- Strings compare byte-wise. Maps are sorted by key and compared
  recursively. Nodes and rels compare by element id. A zoned DateTime is
  its instant plus `ZoneId`.
- The first-seen representation is the one emitted.

The LABS-852 literature survey fixes the tier shape. Use an inline array with a linear compare below roughly 16-32 u64
keys (small-object optimisation, Rust BTreeMap linear nodes). Use a
SwissTable/hashbrown table above that, presized when promoted. Use a dense
bitset only once `k ≥ N/2^7`; that is the Roaring memory break-even, where
a 16 B hash entry equals N/8 bitset bytes. Promote only upward, by moving
the keys already held. Radix partitioning (HyPer, DuckDB about 10k
groups/thread) pays only with parallelism or spill. Bitsets need the
graph's id high-water mark.

## Decision

### 1. One module, no path-engine coupling

The new module `cypherlite-executor/src/distinct_set.rs` (tests in
`distinct_set_tests.rs`, following the `group_key_tests.rs` pattern)
defines `pub(crate) struct DistinctSet`. It sits at crate level beside
`group_key.rs` and `workspace.rs`, not under `exec/`. It may import only:

- `crate::group_key`
- `crate::workspace`'s charge and delta helpers
- the hasher seam (§4)
- `cypherlite_core` (`Value`, `tokens`)
- `rustc_hash`

It must never import from `exec/gpm`, `exec/kernels`, `pattern_walk`, or
anything else under `exec/`. The `xtask` boundary rules cannot see this,
so a unit test greps the module source for `crate::exec` and
`crate::pattern_walk` and fails if either appears.

### 2. The one canonical derivation: `GroupKey::class`

`group_key.rs` gains one function. Equality of the derived form is then
equality of `Norm` by construction, and no site ever re-derives it.

```rust
pub(crate) enum KeyClass { Null, Word(WordTag, u64), General }
pub(crate) enum WordTag { Bool, Int, FloatBits, Node, Rel }
impl GroupKey { pub(crate) fn class(&self) -> KeyClass }
```

It is one exhaustive `match` over `Norm`. Every variant is spelled out, so
there is no `_ =>` arm and a new `Norm` variant breaks the build until it
is classified.

| `Norm` | `KeyClass` |
| --- | --- |
| `Null` | `Null` |
| `Bool(b)` | `Word(Bool, b as u64)` |
| `Num(Int(i))` | `Word(Int, i as u64)` |
| `Num(Bits(b))` | `Word(FloatBits, b)` |
| `Num(Nan(_))` | `General` (the nonce key stays in `GroupKey` equality) |
| `Node(id)` | `Word(Node, id as u64)` |
| `Rel(id)` | `Word(Rel, id as u64)` |
| `Str`, `Bytes`, `List`, `Map`, `Path`, `Temporal`, `Point`, `Uuid`, `Vector` | `General` |

Why this is exact:

- **Injective within a tag.** Each payload is a bijection of the variant's
  field: `i64 → u64` reinterpretation, IEEE bits as-is, and `bool → 0/1`.
  `Norm` has already applied every equivalence rule: `Int(1) ≡ Float(1.0)`
  both become `Num(Int(1))`, and `±0.0` becomes `Int(0)`. So
  `class(a) == class(b)` holds exactly when `a == b`.
- **Disjoint across tags.** Separate tags hold disjoint `Norm` variants,
  and every variant carries an implicit type tag. So `Int(1)`,
  `Node(1)`, `Bool(true)` and the float bit pattern 1 never meet.
- **Encodability depends only on the equivalence class.** Equal keys
  always land in the same class, so a `General` key never equals a word
  key.

Temporals are `General` in this PR. Only `Date` packs cleanly into a u64.
`Time` (i64 + i32), `LocalDateTime` and zoned DateTime (instant + zone)
exceed 64 bits or need range-checked packing, and nothing motivating
dedups on dates. A later row needs its own injectivity proof and test.
`Uuid` is 128 bits and stays `General`.

There is no inverse function (word → `GroupKey`). The tiers below never
convert a word back into a general key, so no second derivation exists to
drift.

Row keys: a row of exactly one column is classified through its one
`GroupKey`. Every other arity (0, or 2 and up) is a row key
(`Vec<GroupKey>`, built with the existing `group_key::row_key`). A
one-column row and a two-column row are never equal: today's `Vec`
equality compares lengths first, and keeping them in separate tables
preserves that. So `insert_row(&[v])` is defined as `insert_value(v)`.
That is the single place where arity picks the path.

### 3. Tiers, thresholds, and their arithmetic

```
DistinctSet {
  null: bool,                          // the one reflexive null key, any tier
  words: Words,                        // KeyClass::Word keys
  spill: Option<Box<Spill>>,           // allocated on first need only
  charge: WorkspaceCharge,             // §5
}
Words = Inline { tag: Option<WordTag>, len: u8, slots: [u64; 16] }   // T1
      | Tables                                                        // T2 (in Spill)
Spill { words: [Option<WordTable>; WordTag::COUNT],   // T2, one table per tag
        singles: Option<HashSet<GroupKey, RandomState>>,
        rows: Option<HashSet<Vec<GroupKey>, RandomState>> }
WordTable = Id(HashSet<u64, FxBuildHasher>)       // tag Node | Rel
          | Value(HashSet<u64, RandomState>)       // tag Bool | Int | FloatBits
```

**T1: inline, tag-homogeneous, `2^4 = 16` u64 slots.**

- The slots take 16 × 8 B = 128 B, which is two 64 B cache lines. On x86
  that is the adjacent-line prefetch pair, so it costs about one fetch.
- A probe compares all 16 slots against the candidate and builds a
  16-bit match mask, then ANDs it with `(1 << len) - 1`. The loop has a
  fixed length and no early exit, so the compiler vectorises it: 4 × AVX2
  256-bit compares or 8 × NEON 128-bit compares, with no hash and no
  mispredicted branch. Unused slots are masked, not compared against a
  sentinel, because every u64 is a valid payload.
- The whole probe takes about 4-8 cycles, against about 15-20 cycles for
  one SipHash-1-3 u64 hash plus the table probe.
- Why not 2^5: it would double the inline footprint to 256 B in every
  `DistinctAcc`, which lives in every group of the agg-fold group tables.
  The cycle break-even (roughly 32-64) does not repay that.
- The tag is stored once per set, not per slot. That is what makes 8 B
  slots possible.
- T1 never allocates. A set that stays at 16 words or fewer never touches
  the heap or a hasher. This covers per-group `count(DISTINCT)` with small
  k, `DISTINCT … LIMIT n ≤ 16` caps, short `coll.distinct` lists, and the
  14-row `distinct_small_age_seek` shape.

**T2: per-tag u64 hashbrown tables (std `HashSet`).**

- On promotion each table is presized with `with_capacity(2^5 = 32)`,
  the next power of two above T1's capacity. hashbrown then picks 64
  buckets at load factor 7/8.
- The tables hold 8 B entries and hash one `u64` with no tag, so each
  insert is one write into the hasher.
- After the presize, hashbrown's own power-of-two doubling is the tier
  flow inside T2. No further explicit tier is added:
  - Entries stay L1-resident up to about 2^12 (32 KB / 8 B).
  - At `expansion_4`'s 44,674 keys the table is 2^16 buckets × 8 B =
    512 KB, which is L2-resident. A tag-plus-payload layout (16 B
    entries) would need 1 MB.
  - Nothing cheaper exists past L2 without parallelism or spill. Radix
    partitioning is out of scope, because CypherLite is single-threaded
    and in-memory.
- `General` single keys go to `singles` (`HashSet<GroupKey>`). Row keys of
  arity other than 1 go to `rows` (`HashSet<Vec<GroupKey>>`). Both are
  exactly today's representations, allocated lazily.

**T3: dense id bitset. Deferred, not in this PR.** Reasons:

- It needs the node/rel id high-water mark N from the graph. That would
  couple the set to `ExecGraph`, while today it needs only values.
- Of the five sites, only a projection of a bare entity (`RETURN DISTINCT n`)
  keys `Node`/`Rel` words. `count(DISTINCT m)` offers `Value::Int(id)`
  (ADR-0485), and the motivating query keys a property `Int`, so T3 would
  not touch LABS-852's numbers.
- Its break-even, `k ≥ N/2^7` (782 on the measured N = 100,000, rounded up to
  the 2^10 boundary of k), pays where node-id dedup is the whole
  operation. That is the engine follow-up's per-level frontier dedup.
- The `Id`/`Value` split of `WordTable` is the seam T3 slots into.
  Promoting an `Id` table to a bitset moves only that table's held words
  and needs no rescan, but N must come from the caller.

### 4. Hasher policy

- **`Id` tables (`Node`, `Rel` tags) use Fx.** Use the same Fx hasher
  spelling as the `CoreMap` family in `graph/mod.rs`; hoist it if the
  alias is private, never re-spell it.
  - These ids are engine-allocated. No remote client surface can submit
    an entity with a chosen id: Bolt parameter decoding
    (`surfaces/cypherlite-bolt/src/param_structs.rs`, ADR-0177) rejects
    node and relationship structs, and HTTP/JSON cannot express them.
    The in-process Rust API is the one exception: an embedder can build a
    `Value::Node`/`Value::Relationship` parameter with any id and pass it
    to `query_with_params`. The embedder is trusted code in the same
    process (it can already exhaust memory directly), so Fx stays; an
    embedder that forwards untrusted input as entity parameters owns that
    validation.
  - Sequential ids under Fx's odd-constant multiply land in distinct low
    bits, which hashbrown uses for buckets.
  - If a surface ever accepts entity values from clients, this row must
    be revisited.
- **Everything else uses one seeded hasher: process-wide `RandomState`
  (SipHash-1-3).** This covers `Value` word tables (`Int`, `FloatBits`,
  `Bool`), `singles` and `rows`.
  - Clients control integer, float and string property values (Bolt,
    HTTP, MCP, parameters, loaded data). Fx is a keyless multiply, so an
    attacker can build integers that collide in hashbrown's bucket bits
    and force O(n²) probing (HashDoS).
  - A per-process secret seed defeats that without a new dependency.
- **The seeded hasher comes from one place.** LABS-822's
  `USED_SET_HASHER` / `shared_hasher()` moves out of `pattern_walk/mod.rs`
  into the lowest shared module (`cypherlite-executor/src/hash_seed.rs`,
  one `pub(crate) fn shared_random_state() -> RandomState`). `pattern_walk`
  and `distinct_set` both import it. Writing a second `OnceLock<RandomState>`
  would be a second copy of one policy.
  - Per-group `DistinctAcc` creation then never reads the thread-local
    seed cell, which LABS-822 measured as hot.
  - Sharing one per-process seed has the same HashDoS exposure as std's
    per-thread seed: the seed is still secret.
- **No new dependencies.** A faster seeded hasher (foldhash, about
  2-4 cycles against SipHash's 15-20 for a u64) would be a direct
  dependency and needs its own ADR.

### 5. Workspace charge: one spelling, owned by the set

The set owns its `WorkspaceCharge`. Its `insert_value(&Value)` and
`insert_row(&[Value])` return `Result<bool>`, where `true` means
newly inserted and `false` means duplicate. Each insert:

1. Classifies and routes the key, with one hash probe: `insert`, never
   `contains` followed by `insert`.
2. On a new key, computes the delta for the representation that took the
   key and applies it through the existing
   `workspace::charge_distinct_delta`, which owns the
   `workspace_accounting()` gate: the delta closure runs only while a
   ledger is installed, so an admission-off run never walks a key for
   bytes.
3. The inline word slot charges before it writes, so a refusal leaves
   the tier untouched and `len == 0 ⟺ tag == None` needs no rollback.
   The table paths charge after the one probe: if the charge is refused,
   they remove the key just inserted and return the error. The set is
   then unchanged, though it may already be promoted; see §6. Words are
   `Copy`; a `singles` or `rows` key moves into its table on the one
   probe, so under accounting the insert keeps an undo copy of it to
   remove by (the copy `DistinctAcc`'s key path already made).

This merges today's two policies into one: the contains-first logic of
`insert_*_with_charge` and `DistinctAcc`'s rollback. Deltas come only from
the existing `workspace` helpers, one per representation:

| Representation | Delta (existing helper) |
| --- | --- |
| null flag | 0 |
| word (T1 or T2, per tag) | `int_insert_delta(len_of_that_tag)`, renamed `word_insert_delta`: stride 8 |
| `singles` | `group_key_insert_delta(len, &key)` |
| `rows` | `distinct_row_insert_delta(len, &key)` |

T1 and a T2 tag table charge the same stride-8 `buffer_bytes(len, 8)`
figure. Promotion therefore costs zero delta and never re-syncs the
charge, and the 17th word pays `buffer_growth(16, 17, 8)` as usual. The
oracle `DistinctSet::bytes()` sums the same per-representation figures,
using `int_set_bytes` (renamed to the word oracle, `word_set_bytes`),
`group_key_set_bytes` and `distinct_row_set_bytes`. It is the test oracle
and also the figure a clone retains, so it (and `distinct_row_set_bytes`)
is compiled outside tests.

`Clone` keeps `DistinctAcc`'s contract, with the decision at the
consumer: the set exposes `try_clone` (an empty set reserves nothing, a
filled one retains its `bytes()` through `WorkspaceCharge::retain_with`,
a refusal propagates) and `clone_contents` (the unreserved copy).
`DistinctAcc`'s `Clone` keeps today's fallback — a refused retain yields
the uncharged contents copy — as a stated consumer decision, not a
silent default inside the set.

### 6. Promotion: upward only, held keys moved, bounded

- **T1 → T2.** Promotion happens when a new distinct word arrives and
  either T1 already holds 16 words, or the word's tag differs from T1's
  tag. A duplicate offer at `len == 16` matches in T1 and never promotes.
  The steps:
  1. Allocate `Spill` if it does not exist yet.
  2. Create the held tag's table with `with_capacity(32)`.
  3. Move up to 16 held words into it.
  4. Insert the new word into its own tag's table, created with
     `with_capacity(32)` if absent.
- **No rescan and no demotion.** Promotion reads no input again, and the
  set never moves back to T1 once in T2.
- **Rollback does not undo promotion.** If the charge refuses the
  promoting insert, the new key is removed but the set stays in T2. That
  is harmless, because the T1 and T2 charge figures are the same.
- **`General` and row keys never trigger a move.** They go straight to
  their lazily created tables, while the word tier stays where it is. A
  set can therefore be T1 for its words while `singles` is live. This
  also replaces `DistinctKind`'s promote, which re-keyed every held int
  as a `GroupKey`.
- **Move work is bounded.** Explicit tier moves total at most 16 per set.
  hashbrown's doubling moves at most k amortised, so total move work
  stays at or below 2k. Sets only grow, apart from the one-key rollback,
  so the hysteresis Beamer needs for push/pull does not arise.
- **`len()`** = `null as usize` + words held (T1 len, or the sum of the
  tag tables) + `singles.len()` + `rows.len()`. `DistinctLimitCap::full`
  and `DistinctAcc::len` read it.

### 7. Equality and first-seen emission

Membership equals `HashSet<Vec<GroupKey>>` membership for rows and
`HashSet<GroupKey>` membership for values:

- **Null** is one flag, so it stays reflexive. `DistinctAcc` still drops
  nulls itself, before inserting, as aggregate null policy.
- **NaN** is `General`. Each `GroupKey::new` claims a fresh nonce, so
  every NaN is a new key, nested ones too.
- **All other rules** come through `Norm` (§2).

The set only decides membership and never emits. Every site keeps its
current pattern: push the row or value when `insert` returns `true`, or
`retain` on the result. So the T1 slot order, the per-tag table split and
hash iteration order cannot reorder output. First-seen emission order is
the input order filtered by the insert-result sequence, and §8's
differential test checks that sequence against the reference
element by element.

### 8. Site integration

All five sites adopt `DistinctSet`, and `DistinctKind` is absorbed:

1. **Projection** (`project.rs`). Replace the `HashSet` and the
   `distinct_charge` local with one `DistinctSet`, in an extracted
   `retain_distinct` stage. Every call goes through `insert_row`, whose
   one-column arm is the single arity-1 spelling; a non-`*` projection
   with one output column passes the column's value as a borrowed
   one-element slice (`std::slice::from_ref`), so no `Vec<Value>` is
   cloned and no `Vec<GroupKey>` is allocated. The set's charge drops
   with the set where `distinct_charge` drops today.
2. **UNION** (`exec/union.rs`: `union_mode`, `UnionFold`, `fold_parts`,
   subquery fold). The stage's union mode is derived once from the
   per-boundary flags (the planner forbids mixed joiners; a mixed vector
   fails loud). `UnionFold` owns the accumulated result and, in
   `Distinct` mode, the one `DistinctSet` every part's rows retain
   against before appending — no per-part flag arithmetic, no
   fresh-set rebuild at the first UNION joiner. `merge` is fallible:
   `Result<()>`, propagated by `fold_parts` and the subquery fold in
   `exec/call/mod.rs`, which already return `Result`. The UNION set
   is now charged; see Consequences.
3. **`DistinctAcc`** (`acc.rs`). It becomes `{ set: DistinctSet }`.
   `DistinctKind`, `Ints`, `promote`, `sync`, `charge_delta` and
   `exact_int` (if it has no other caller) are deleted. The integer fast
   path is the `Int` word tier: `exact_int` agreed with
   `Norm::Num(Int)` for `Int` and integral `Float`, so counts are
   unchanged. Clone and Default semantics are preserved (§5).
4. **`dedup_first_seen`** (`distinct_set.rs`, beside the set). It loops
   `insert_value` and drops the set, and with it the charge, before
   returning. That is the same release point as today.
5. **`DistinctLimitCap`** (`match_cap.rs`). `seen` becomes a
   `DistinctSet`. `offer` evaluates the items and calls `insert_row`
   (whose one-column arm keys the value directly, as at the projection
   site) and propagates the new `Result`.

Once every caller moves, delete `insert_group_key_with_charge` and
`insert_row_key_with_charge` together with their policy doc, so the
accounting has exactly one spelling.

File budgets:

- `group_key.rs` 760 → about 800 lines.
- `workspace.rs` 822 shrinks, as the two insert helpers leave.
- `acc.rs` shrinks.
- `distinct_set.rs` stays under 400 lines plus tests.

### 9. Test strategy, for the later nodes

- **Unit tests** in `distinct_set_tests.rs` and `group_key_tests.rs`:
  - `class` has one row per `Norm` variant.
  - Injectivity probes, each of which must stay apart:
    - `Int(1)` / `Float(1.0)` / `Node(1)` / `Rel(1)` / `Bool(true)`;
    - `FloatBits(1.5)` against `Int` of the same bits;
    - `-0.0` with `0`;
    - `i64::MIN`/`MAX` against out-of-range floats.
  - NaN stays apart: top-level, nested in a list, nested in a map.
  - Boundary pins at k = 15, 16 and 17 (2^4 − 1, 2^4, 2^4 + 1): the tier
    observed (a `#[cfg(test)]` tier probe), `len`, membership, and a
    duplicate at `len == 16` not promoting.
  - A tag switch at k = 3 promotes.
  - A general key does not move T1.
  - Null is reflexive in every tier.
  - Row arity 0, 1 and 2. A `*`-style mixed-arity sequence where `[x]`
    and `[x, y]` stay apart.
  - The summed deltas equal the `bytes()` oracle after every insert,
    across promotions.
  - A refused charge leaves `len`, membership and `bytes()` unchanged,
    including at the promoting 17th insert.
  - `Clone` charge semantics.
  - The module-source grep guard from §1.
- **Differential property test.** Random value sequences mix ints,
  integral and non-integral floats, ±0.0, NaN, null, bools, strings,
  lists, maps, nodes and rels, with k swept across 0..=2^7 and rows of
  arity 0-3. For each, run `DistinctSet` alongside the reference
  `HashSet<GroupKey>` / `HashSet<Vec<GroupKey>>`, inserting the same keys
  (the reference uses `contains` then `insert` on keys rebuilt the same
  way). Assert the per-insert `bool` sequences are identical, which pins
  first-seen order at every site, and that `len` agrees.
- **Existing site suites, which must pass:**
  - `exec/pushdown_tests.rs` (`distinct_keeps_first_of_each_value`,
    `distinct_limit_stops_neighbor_walk`), `limit_where_tests.rs`;
  - `aggregate_tests.rs`, `path_fold_tests.rs`, `direct_fold_tests.rs`;
  - `aggregate_charge_tests.rs`, where figures for int-only columns and
    for int-then-general mixes change by design (§5 and Consequences);
    each re-pinned figure carries a one-line reason;
  - `workspace/tests.rs`, `nested_eval_tests.rs`, `subquery_eval_tests.rs`.
- **Corpus:**
  - The 19 TCK DISTINCT feature files: Return4-6, With5, WithWhere1/4,
    Union1/2, Aggregation5/8, ReturnOrderBy1/2, WithOrderBy1/2,
    WithSkipLimit1, MatchWhere4, Merge9, Graph8 and
    CountingSubgraphMatches1.
  - Match4-6, Quantifier1-12, Merge5 and Create2.
  - cip-eval `coll-distinct.feature` and `union-distinct.feature`.
  - The full TCK, 3892/3892, must not regress.
- **Bench.** Run the qloop-852 harness shapes with digests identical to
  the baseline. The target is the distinct share of `expansion_4`
  (102 ms PROFILE ProduceResults time) improving at least 2x. Report a
  miss as measured; never tune the thresholds to hit it.

## Alternatives considered

- **Hasher swap only.** Either Fx at every site, or the shared
  `RandomState` with the structures unchanged. Rejected:
  - Fx on client-controlled values is a HashDoS vector (§4).
  - Swapping the hasher alone leaves the per-row `Vec<GroupKey>`
    allocation, the `Vec<Value>` clone and the double probe, which are
    most of the 150 ns per row.
- **foldhash (or ahash) as a direct dependency for the seeded tables.**
  Rejected for this PR. It is a dependency decision outside the
  mechanical scope, and the user directed no new dependencies. A
  follow-up ADR can weigh it against the measured remaining hash share.
- **Fill, then sort and dedup.** Rejected:
  - It is O(n log n) over n inputs, not k distinct, and the motivating
    shape has n = 14.9 × k.
  - It needs an index carry to restore first-seen order.
  - It blocks streaming sites (`DistinctAcc`, `DistinctLimitCap`).
- **A single hash tier with no inline tier.** Rejected:
  - Small sets are where per-set cost dominates: per-group
    `count(DISTINCT)`, LIMIT caps, short `coll.distinct`. A hash table
    costs a heap allocation plus a hash on every probe. T1 costs neither
    up to 2^4 keys.
- **T1 storing `GroupKey`s (heterogeneous) with an `Eq` scan.** Rejected:
  - `size_of::<GroupKey>()` × 16 spans many cache lines.
  - The enum compare branches per variant and does not vectorise.
- **T1 at 2^5 slots.** Rejected: it gives 256 B inline per set, doubling
  the per-group footprint in agg-fold tables. The cycle break-even does
  not justify that.
- **One tagged `(tag, u64)` table for all words.** Rejected:
  - 16 B entries double table bytes, to 1 MB at `expansion_4`'s 2^16
    buckets, spilling L2 on smaller cores.
  - Hashing 9 bytes instead of 8.
  - One hasher across id and value tags would break §4's split.
- **Packing the tag into the payload.** Rejected: it is not injective,
  because `Int` and `FloatBits` use the full 64 bits.
- **Words tier with an inverse (word → `GroupKey`) and promotion to a
  single general table on the first non-word key.** Rejected:
  - It needs a second, inverse derivation that has to be proven and kept
    in sync.
  - It moves keys again, which the per-class tables avoid entirely.
- **T3 dense id bitset now.** Deferred (§3): it couples the set to the
  graph's id high-water mark, and no motivating site keys pure ids.
- **Roaring bitmaps.** Rejected: new dependency (ADR) and no id-keyed
  motivating site.
- **Radix-partitioned hashing.** Rejected: it pays only with parallelism
  or spill, and the engine is single-threaded and in-memory.
- **Per-site specialisations**, such as keeping `DistinctKind::Ints` and
  adding a word path only to the projection. Rejected:
  - Parallel state and two accounting spellings break repo code-shape
    rules.
  - Five sites would drift on the next semantic fix.

## Consequences

- One structure and one accounting spelling serve every DISTINCT
  membership decision. `GroupKey` stays the only equivalence relation,
  and `KeyClass` is its only cheap projection. GROUP BY tables and
  property-index keys are untouched.
- **Charge figures change, by design, to the representation actually
  held:**
  - Single-column word-encodable DISTINCT (projection, `coll.distinct`,
    caps) charges a stride-8 figure, not the `GroupKey` figure.
  - `count(DISTINCT)` no longer re-charges held ints as `GroupKey`s when
    a non-integer arrives.
  - UNION and `DistinctLimitCap` sets are charged for the first time
    (decided by Michael, 2026-11-04). Under a tight token budget a very
    large UNION can now fail with a typed capacity error where it used
    to run uncharged. That is the intended, uniform accounting rule.
    The LIMIT-bounded cap's charge is trivially small.
- `union_merge` gains `Result` plumbing (`DistinctLimitCap::offer`
  already returns `Result<()>`; do not re-add plumbing there).
- `DistinctAcc` grows by the inline tier: about 128 B plus a few words of
  state against today's empty `HashSet`. In exchange, small-k groups make
  no heap allocation. Large group tables with `count(DISTINCT)` should be
  watched in the memory-shape lane.
- **What the win does and does not cover.** The win is confined to the
  dedup share. `expansion_4`'s 722 ms walk is untouched until the engine
  follow-up adds per-level node dedup and the DISTINCT-on-start
  semi-join. The large micro-shape's 71 ms is dominated by scan and
  property fetch, not the set, so its gain will be small.
- **Seams left for later work:** T3 (`WordTable::Id` → bitset, with N
  from the caller) and a faster seeded hasher (new ADR). The engine
  follow-up's frontier dedup can reuse `DistinctSet` keyed by `Node`
  words, Fx-hashed, without the set importing path-engine code.
- `pattern_walk/mod.rs` changes by one import: the shared seeded hasher
  moves to `hash_seed.rs`. Its behaviour (LABS-822) is unchanged.

## Later references

- ADR-0522 extracts §3's tier mechanics (the inline tier, the promoted
  table with §4's `Id`/`Value` hasher split, the §5 charge-and-rollback
  protocol) into the crate-level `TieredWordSet` utility, so raw-key
  consumers outside DISTINCT can hold the tiering; `DistinctSet`
  delegates its word storage to the utility with this ADR's semantics
  and tests unchanged. §1's boundary stands, widened one way: the
  pattern walker may import the utility, never `DistinctSet`.
