---
type: adr
area: engine
date: 2026-09-23
deciders: CypherLite maintainers
name: 0090-native-reset-role-and-durability
status: proposed
tags:
  - storage
title: Role-mark Native identities and durably stage reset
REFERENCES:
  - "[[adr/0074-single-file-bundle-vfs]]"
  - "[[adr/0097-exclusive-storage-ownership]]"
  - "[[adr/0098-safe-atomic-replacement]]"
  - "[[adr/0120-ontology-document-persistence]]"
---

# 0090. Role-mark Native identities and durably stage reset

## Context

[ADR-0097](./0097-exclusive-storage-ownership.md) locks stable `.cypherlite.lock` files in the Native root and WAL directory and
stages an external WAL before the root on reset. Those files have identical,
empty contents. An unlocked, *closed* database root can therefore be mistaken
for an external WAL directory and recursively deleted. A two-move reset also
syncs its `.reset-incomplete` marker but not the directory entries for its
tombstones and renames. After a power failure, a root move can survive while
the earlier WAL move does not, allowing a fresh root to attach the old WAL.
[ADR-0098](./0098-safe-atomic-replacement.md)'s atomic-file replacement barrier does not persist these reset moves.

## Decision

Before any **destructive reset** of a named database URI, refuse an existing
final-component symlink or Windows reparse-point alias to a Native directory
or `.cl` bundle. Inspect the requested path itself without following the last
component (including dangling links), before canonicalizing it to a deletion
target, taking a lease, creating a role/marker/tombstone or unlinking anything;
repeat the identity check under the acquired ownership lease before staging.
Report `Error::Storage` and leave both the alias and its target unchanged,
even if the target is a valid closed store or an unrelated regular file. A
direct non-aliased reset still follows the role and WAL-first rules below.
This extra restriction is **reset-only**: ordinary open still resolves
supported aliases and locks their physical identity under [ADR-0097](./0097-exclusive-storage-ownership.md). A default
`<root>/wal` link found *inside* a directly named Native root still follows
the two-tree WAL rule below; it is not an alias for the requested database URI.

1. **Distinct, stable roles.** New Native root `.cypherlite.lock` files contain the exact
   bytes `CYPHERLITE-LOCK-v1 root\n`; new *distinct* WAL-directory `.cypherlite.lock`
   files contain `CYPHERLITE-LOCK-v1 wal\n`. These are *the existing lock
   inodes*, not additional lock files. The marker filename is unique to
   CypherLite (`.cypherlite.lock`); an unrelated `.lock` file created by
   another tool is never a database identity. Acquire the nonblocking lease on the
   unchanged inode before reading or initializing its contents. Write and
   sync a newly created role marker (including its containing directory's new
   entry on Unix) before using the directory for data. Reject unknown,
   malformed, wrong-role, or symlinked markers with `Error::Storage`, and
   reserve `Error::Locked` for real contention. A disjoint WAL path marked
   `root`, including a closed root, is never attachable as WAL or eligible for
   WAL reset. When root and WAL resolve to the **same physical directory**,
 [ADR-0097](./0097-exclusive-storage-ownership.md) reuses one lock inode and lease: it has the `root` role only, even
   when `wal_dir` was explicitly set to the root. Do not write a second WAL
   role into it or reject a valid root marker as wrong-role; a `wal`-role
 marker at that root is invalid. [ADR-0097](./0097-exclusive-storage-ownership.md) still rejects **reset** with
   root == WAL as unsafe overlap, *before* creating a lock/role/tombstone;
   sharing one lease on normal open does not permit deleting that directory.
   Perform role validation before creating/resetting either identity and under
 both leases (one for the same-directory case); retain [ADR-0097](./0097-exclusive-storage-ownership.md)'s alias,
   nested-owner and subtree checks.

2. **Preflight legacy and markerless directories.** A missing or empty
   `.cypherlite.lock` is not proof of either role. Before reset creates any missing lock
 file (including in an existing pre-[ADR-0097](./0097-exclusive-storage-ownership.md) data directory), marker or
   tombstone, inspect each existing physical root and WAL directory without
   following child symlinks/reparse points. Root evidence is a directly
   contained regular `manifest.json` or `graph.json`, or a directly contained
   physical `wal` directory with a typed `wal` marker and at least one
   correctly named segment whose first eight bytes are `MDBWAL01` (only for a
   distinct default WAL, never when the requested root and WAL are the same
   directory). An untyped child WAL segment alone cannot establish the
   parent's root identity: it could belong to a closed independent root
   opened with root == WAL. WAL evidence is a correctly named
   `NNNNNNNN.log` segment (eight decimal digits) with that magic directly in
   the proposed WAL directory. A pre-existing untyped direct `<root>/wal`
   (missing or empty `.cypherlite.lock`) cannot inherit WAL identity from the parent:
   even a root with its own manifest or typed `root` marker could contain an
   independently owned closed root at `wal` that used root == WAL. Never
   exempt its `.cypherlite.lock` in the nested-owner scan or upgrade it on open/reset.
   A typed direct `wal` marker remains recognizable after the outer root
 switches to a disjoint WAL; a markerless pre-[ADR-0097](./0097-exclusive-storage-ownership.md) root with a typed
   default WAL and valid segments remains recognizable too. Only a directory
   created by the current ordinary open (not merely observed absent before
   a raced-in directory appears) can receive a fresh `wal` role before data
   initialization; under its new lease, a failed nested scan leaves its
 stable marker rather than deleting it ([ADR-0097](./0097-exclusive-storage-ownership.md)).
   These bounded magic/name checks establish *role only*, not frame or
   snapshot validity; an unreadable object is an error and a present but
   corrupt `manifest.json`/`graph.json` is still root evidence against
   deletion. Require root evidence for a markerless existing root. A disjoint
   markerless/empty-marker WAL with segment magic but no root evidence is
   **still ambiguous**: a closed legacy database opened with root == WAL and
   not yet checkpointed has the same layout. Without a typed `wal` marker,
   refuse automatic attachment/role upgrade and destructive reset; report the
   path for explicit offline role migration after the operator verifies its
   ownership. Never infer ownership from the caller's `wal_dir` argument
   alone. If a root and WAL are the same physical directory, classify it only
   as a root: require root-specific evidence (`manifest.json`/`graph.json`)
   for an existing markerless/empty-marker directory; direct segment magic
   alone is ambiguous
   with a standalone WAL and must fail closed. Refuse contradictory evidence
   (including root evidence at a disjoint WAL) or ambiguous/nonempty
   unrecognizable directories without creating `.cypherlite.lock` or moving anything.
   A missing `.cypherlite.lock` in a *pre-existing empty directory* does not establish
   ownership for reset either. Preserve such data for explicit operator
 recovery. Continue [ADR-0097](./0097-exclusive-storage-ownership.md)'s path, nested-owner and both-tree preflights
    before identity creation.

 **Offline adoption of a direct default WAL.** A pre-[ADR-0097](./0097-exclusive-storage-ownership.md) Native root
    with a regular, directly contained `manifest.json` or `graph.json` and an
    untyped physical `<root>/wal` containing a correctly named WAL segment
    with `MDBWAL01` magic has a practical migration path. With **both database
    owners closed**, the operator must independently verify that this child is
    the root's own default WAL, not an independent root that used root == WAL;
    bytes and location cannot prove that fact. Expose the narrow
    `cypherlite_storage::adopt_native_default_wal(root: &Path)` offline operation
    for this explicit assertion. It accepts a typed root as well as an
    evidenced legacy root, but only an actual, directly contained `wal`
    directory (no symlink to an external WAL), with no root-specific evidence
    in the child. Require a real WAL segment with magic; an empty or unknown
    child is not eligible. Reject other nested `.cypherlite.lock` identities and `.cl`
    bundles in either tree, unknown/invalid roles, overlap/ancestor identities,
    reset-incomplete roots, and live lease contention. Inspect before creating
    either missing lock, acquire root then WAL leases on stable, no-follow
    inodes, verify identity, role, physical containment and both scans again
    under those leases, exempting **only the operator-selected child's own
    untyped `.cypherlite.lock`** for this offline operation. Sync root and WAL role bytes
    in-place under their respective leases and sync any newly created entries
    on Unix. Leave all WAL, manifest and snapshot bytes in place; no replay,
    reset, renames or automatic fallback. A failed sync is an error, and the
    caller must inspect the roles before retrying. After successful adoption,
    ordinary open/reset use the typed WAL marker without an exemption for
    other untyped children. This is an operator assertion, not automatic
    discovery or proof of ownership against a closed ambiguous independent
 database. Document it with a runnable offline example. [ADR-0097](./0097-exclusive-storage-ownership.md)'s
    exclusion of only typed direct WAL markers remains the rule for ordinary
    open/reset; the temporary untyped exemption exists only within this
    explicit offline adoption operation.

    Preflight reads an existing role marker only after taking a temporary
    nonblocking lease on its unchanged inode; release that lease before the
    final root-then-WAL lease acquisition. Inspect the root first, then the
    disjoint WAL, so a wrong-role or ambiguous WAL is rejected before creating
    a missing root marker. Under the final root lease, recheck root role
    evidence (including any typed child WAL) before locking the distinct WAL;
    repeat the WAL check under its final lease. No two leases are held in
    WAL-then-root order. A marker created after successful preflight remains
    a stable lock inode if a subsequent concurrent change fails validation.

    After that no-mutation preflight, create a missing `.cypherlite.lock` exclusively,
   without following a final-component link, in each evidenced existing root
   or typed direct contained WAL directory; open an empty legacy marker on
   its unchanged inode. Acquire root then distinct WAL leases nonblockingly
   and recheck filesystem identity,
   role evidence and both-tree scans under the leases before writing roles,
   reset markers or tombstones. If another opener created a marker, validate
   its role under the acquired lease rather than replacing it; a failed
   recheck leaves a new stable marker in place instead of unlinking it and
 splitting ownership ([ADR-0097](./0097-exclusive-storage-ownership.md)). Write/upgrade the verified role **in place
   while locked**, then sync its inode and, for a newly created entry, its
   parent directory on Unix *before* reset stages a tree. If creation or
   either sync fails, report the error and make no reset move; do not assume
   the role was durable. On Windows this step does not claim directory-entry
 power-loss durability ([ADR-0098](./0098-safe-atomic-replacement.md)). No forced upgrade on a read-only or
   failed ordinary open; legacy recognizable roots and typed direct WALs
   must still open after validation. A directory created by the current
 ordinary open can receive its role under [ADR-0097](./0097-exclusive-storage-ownership.md) before database
   initialization; reset of an absent directory remains a no-op, not a
   reason to create one.

3. **Durable WAL-first reset on Linux/macOS.** With both old identities
   locked, validate roles, physical paths and both recursively deleted trees
   before changing either. For a disjoint WAL, create and sync the
   `.reset-incomplete` marker and its root directory first. Reserve private,
   unpredictable, same-parent tombstone containers with exclusive creation;
   persist each container entry in its parent. Rename WAL into its tombstone,
   then sync **both** the old WAL parent and the tombstone container. Only
   after this WAL move is durable may the root be renamed into its tombstone;
   sync both its old parent and destination container before releasing leases
   or starting deletion. On an error or crash between phases, the old root
   still bears a durable fail-closed marker, or its missing name can only be
   recreated alongside a *durably absent* old WAL. Handle a default `wal`
   symlink to a disjoint external WAL as a two-tree reset. A verified direct
   contained `<root>/wal` moves with the root; sync its tombstone creation and
   root move before deletion too. A one-tree reset of only an existing
   external WAL follows the same tombstone and parent-directory barriers.

4. **Failure and rollback.** A failed sync is an error, not evidence that a
   rename did not publish. Keep old data in its tombstone and the
   reset-incomplete marker until a durable state can be proved. If restoring
 names, use [ADR-0097](./0097-exclusive-storage-ownership.md)'s no-replace root-first, WAL-second rollback; sync the
   source and destination directories for *each* restoration and remove/sync
   the marker only after both original names and their contents are durably
   restored. Never restore WAL to a new or unverified root. Do not recursively
   delete until both required moves and their barriers succeed; then close
   handles as needed for Windows and delete only the owned tombstones. Report
 stranded paths on failure. On Windows, [ADR-0098](./0098-safe-atomic-replacement.md) establishes no directory
   rename durability primitive: **fail closed before any marker, lease-created
   identity, tombstone, or rename for a disjoint external-WAL reset** until a
   tested equivalent namespace barrier is adopted. A contained/default-WAL
   reset may retain atomic-visibility semantics but must not claim power-loss
   durability unavailable on Windows.

## Alternatives considered

- **Canonicalize a final-component database-URI alias and reset its target.**
  Rejected: a caller naming a link could delete an unrelated or retargeted
  tree/file, even though alias resolution is useful for ordinary open.
- **Reject all aliases on ordinary open too.** Rejected: opening through a
  supported symlink/reparse alias shares the physical lease and does not
  authorize destruction; the reset-only restriction is sufficient.
- **Use one empty `.cypherlite.lock` role for both Native directories.** Rejected:
  locking protects live owners but cannot distinguish a closed database root
  from a disjoint WAL slated for recursive deletion.
- **Replace legacy lock files with role-named files or a new sidecar.** Rejected:
  changing the locked inode splits ownership with old processes, and another
  marker increases the alias/race surface without proving the original role.
- **Infer that every empty disjoint legacy `.cypherlite.lock` is a WAL.** Rejected: an
  independently owned but closed, empty database would be silently destroyed.
- **Trust WAL magic alone to tag or reset a disjoint legacy WAL.** Rejected:
  the same segment-only layout is possible at an uncheckpointed closed root
  that used root == WAL; magic proves file format, not directory ownership.
- **Exempt a direct untyped `wal/.cypherlite.lock` by its location, parent manifest or
  segment magic.** Rejected: a root using a disjoint external WAL could
  contain a closed independently owned root at `<root>/wal`, even if the
  outer root is evidenced and the child lock is empty.
- **Tell operators to hand-write `.cypherlite.lock` bytes or copy the WAL into a fresh
  directory.** Rejected: hand edits bypass inode leasing, no-follow checks and
  directory sync; copying risks dropping frames and silently changing storage
  identity. The narrow offline adoption operation preserves the original bytes
  and stable lock inodes while requiring an explicit ownership assertion.
- **Create role markers first, then inspect a markerless legacy reset path.**
  Rejected: a guessed `wal` role or a first root marker can change a closed
  database before discovering conflicting evidence; reset must preflight both
  trees before creating either identity.
- **Give a same-directory root/WAL two roles or infer root from segment magic
 alone.** Rejected: [ADR-0097](./0097-exclusive-storage-ownership.md) uses one lock identity for that directory, and
  bare segments also identify a standalone WAL directory.
- **Rename WAL then root without syncing their parent entries.** Rejected:
  file sync and rename visibility do not guarantee WAL-first crash ordering.
- **Attempt external-WAL reset on Windows using a no-op directory sync.**
 Rejected: it claims a barrier [ADR-0098](./0098-safe-atomic-replacement.md) explicitly cannot provide and can
  reconnect an old WAL to a new root after power loss.

## Consequences

- A reset request through a final-component Native root or `.cl` alias fails
  before any target mutation; the alias and target bytes remain unchanged.
  Ordinary open through a supported alias keeps the existing lock/role
  behavior; direct non-aliased reset and the vetted internal default-WAL
  alias follow the remaining ownership and durability checks.
- Existing identifiable Native stores retain their `.cypherlite.lock` inodes (or obtain
  one stable inode after a markerless-directory preflight) and can acquire
  roles without a wholesale layout migration. Ambiguous untyped existing WAL
  directories (even directly contained by an evidenced root), disjoint
  segment-only untyped WALs, markerless existing empty directories, and
  same-directory segment-only stores require explicit resolution instead of
  destructive reset. A same-directory root/WAL has one
  root marker on normal open and is
  rejected by reset's overlap check. `.cl` ownership remains on its single
 host file ([ADR-0074](./0074-single-file-bundle-vfs.md)).
- Pre-ADR default-WAL stores with no `.cypherlite.lock` files can be explicitly adopted
  offline after verifying the child belongs to the parent. A wrongly asserted
  closed independent segment-only root cannot be distinguished by format; the
  operator bears that ownership decision. No ordinary open/reset makes it.
- Unix reset may incur several extra fsyncs and leave reportable tombstones
  and a blocking marker after I/O failure. A failed reset must not make an old
  WAL available to a fresh root; fault/crash tests cover each barrier, nested
  closed owners, legacy roles, and alias cases. Native VFS tests inject a
  disjoint empty WAL with and without an existing `.cypherlite.lock` between the last
  inspection and final-component creation; both remain unclaimed, with the
  original lock inode/bytes retained when present. An existing role marker
  held by another owner makes preflight return `Locked` before reading its
  bytes or creating a missing root marker. Windows external-WAL reset is
  rejected before mutation pending a separately decided and tested barrier.

## Later references

[ADR-0120](./0120-ontology-document-persistence.md) adds the WAL segment magic `MDBWAL02` for segments that may contain
ontology ops. The role checks in this ADR that look for a segment starting
with `MDBWAL01` accept either magic.
