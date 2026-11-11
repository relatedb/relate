# 0097. Exclusive ownership and private creation for persistent storage

- **Status**: Proposed
- **Date**: 2026-09-23
- **Deciders**: CypherLite maintainers

## Context

ADR-0072 routes the WAL and snapshots through a synchronous VFS.
ADR-0074 packs the same virtual tree into **one host file** for `.cl` paths.
Today independent `Database::open` calls can initialize or recover the same
persistent store concurrently. In particular, URI parsing initializes a
bundle, creates Native directories, and resolves the snapshot codec before
`open_store` replays the WAL. Locking only in `open_store` is too late
(LABS-73/83). New files and directories also inherit permissive Unix defaults.

Rust 1.88 does not stabilize `std::fs::File::try_lock`. The inspected `fs4`
0.8.4 `FileExt::try_lock_exclusive` offers nonblocking `flock` on Linux/macOS
and `LockFileEx` with immediate failure on Windows. The live public result
path is `cypherlite_core::Error`, re-exported by `cypherlite::Error`;
`cypherlite_error::StorageError::Locked` is a separate, currently unused path.
Rust 1.88 also does not stabilize Windows `MetadataExt::volume_serial_number`
or `MetadataExt::file_index`; reset and lock identity checks need another way
to compare actual filesystem objects, including directories.

## Decision

1. **Acquire before accessing database contents.** Every persistent
   `Database::open`/`open_with` (and storage URI parse used to open a store)
   takes nonblocking exclusive OS ownership during URI parsing, before
   `BundleVfs::open_or_create` can initialize or load a bundle, before codec
   resolution, and before Native manifest, WAL, or snapshot access.
   Only the minimal creation of missing directory components and the lock
   target itself may precede acquisition; no WAL or bundle bytes may be
   initialized, truncated, or recovered first. A failed acquisition must
   close its temporary handles without altering existing database contents.
   `:memory:` remains independent and takes no filesystem lock. Use `fs4`
   0.8.4's nonblocking `FileExt::try_lock_exclusive` at the Rust 1.88 MSRV;
   the dependency and its OS-specific implementation are part of this
   ownership decision.

2. **Native identity, including WAL overrides.** Create/open a stable
   regular `.cypherlite.lock` file inside the Native database root, then acquire its
   exclusive lock. The marker filename is unique to CypherLite: unrelated
   `.lock` files created by other tools (for example a `pytest-0/.lock` in a
   parent directory) are not database identities and must never be treated as
   an ancestor or nested-owner marker. Only `.cypherlite.lock` names a
   CypherLite identity. Never unlink or replace this file on ordinary close: a
   newly created inode could admit a second owner while the first still
   holds the old inode. The root may be created first, privately, solely to
   obtain `.cypherlite.lock`. With the root lock held, create/open and lock a second
   stable `.cypherlite.lock` in the WAL directory, whether that directory is the
   default `<root>/wal` or `OpenOptions.wal_dir`. Acquire root first, WAL
   second, before creating/replaying WAL segments or resolving the codec;
   release both on any later open failure. If root and WAL directory resolve
   to the same filesystem identity, reuse the root lease rather than
   self-contending; one physical `.cypherlite.lock` remains a root identity (ADR-0090),
   not both a root and WAL role. This normal-open exception does not relax
   reset's equal-root/WAL rejection in (4). Two distinct roots pointing at one
   WAL directory thus
    contend even if their root locks differ. A root/WAL overlap may also
    produce conservative contention; it must never allow concurrent WAL
    writers. Do not use path-string-only ownership checks. For both default
    and overridden Native WAL, root WAL file operations (open, list, and
    rotate) at the canonical directory returned by the WAL lease, using an
    empty VFS prefix. The database root VFS continues to serve snapshots.
    A supported `root/wal` symlink alias may resolve to an external WAL
    directory at open, but later retargeting that alias must not redirect WAL
    I/O to an unleased directory. This is an identity-bound path choice for
    cooperative ownership, not a guarantee against hostile same-user
    mutation of the leased directory itself (see (5)).

3. **Bundle identity.** For `.cl`, open the existing regular host file for
   read/write without truncation, or atomically create a missing one with
   `create_new`; acquire the exclusive lock on that **same file handle**
   before handing it to bundle initialization/load. Retain the locked handle
   (or a clone sharing its lock ownership) inside the bundle/lease. Do not
   reopen by path for initialization. `OpenOptions.wal_dir` remains rejected
   for `.cl` (ADR-0074). There is no bundle lock sidecar, OS WAL directory,
   or other additional host file.

4. **Aliases and lifetime.** Resolve relative paths and supported existing
   symlink aliases to their actual filesystem targets before opening the
   stable lock file or bundle file; verify targets are directories/regular
   files as appropriate. Existing hard links to a bundle, `file://`, and
   relative/absolute spellings must contend on file identity, not spelling.
   Dangling or cyclic symlinks and links to a nonmatching file type fail
   without creating through the link. Open the resolved lock target without
   following a symlink in its final component; refuse a pre-existing
   symlink or non-regular `.cypherlite.lock` rather than locking an unrelated file.
   Keep the lease shared by `StoreTarget` clones and VFS handles and through
   all `Database`/`Connection` references to `Shared`, including active
   checkpoints and the timer. Do not try to acquire another lock through a
   cloned file descriptor: independently opened handles, including those in
   the same process, must contend. Release only after the last live owner
   and final `Shared::drop` persistence complete. OS process exit/crash
   releases the locks; no stale-file deletion is needed. Any destructive
   reset/unlink of persistent storage must first coordinate with the same
   ownership identities so it cannot bypass a live owner or replace a locked
   bundle inode. For Native reset, while both identities remain locked, move
   the old root and any separate WAL override into exclusively created,
   unpredictable, same-parent private tombstone directories (`0700` on Unix)
   before releasing the leases or recursively deleting data. Moving a
   containing directory moves its nested WAL/root with it. For disjoint
   directories (including a default `wal` symlink aliasing an external WAL),
   write a durable reset-incomplete marker in the old root, then stage the
   **WAL first** while the old root is still locked, then stage the root.
   Openers check the marker under the root lock before creating or replaying
   any WAL. After the root is moved, new openers may create a fresh root and
   WAL; deletion touches only tombstones. If staging fails, restore the root
   first, then the WAL, with an OS-level no-replace rename for each rollback
   (including when the destination is a dangling symlink). Never restore an
   old WAL if its old root could not be restored: retain both tombstones and
   report their recovery paths. If the root is restored but WAL restoration
   fails, retain the marker and report the WAL tombstone; reopening that root
   fails closed until the old WAL is recovered. Remove the marker only after
   complete rollback. No recursive
   deletion starts until staging completes. Once deletion begins, a filesystem
   error can leave a partially removed tombstone and must be reported, not
   treated as an atomic rollback. Close locked handles before recursive
   deletion for Windows open-handle compatibility. Use Linux `renameat2` with
   `RENAME_NOREPLACE`, macOS `renamex_np` with `RENAME_EXCL`, and Windows
   `MoveFileW` without replacement; if unavailable, retain recoverable data
   and report an error rather than attempt a replace-allowed rollback.
   **Before any reset lock-file/directory creation, marker, tombstone, or
   rename**, resolve the root and WAL paths to filesystem identities,
   canonicalizing existing components and symlink aliases (including the
   nearest existing ancestor of a missing path). Refuse reset with a semantic
   `Storage` error, not `Locked`, if the WAL resolves to the root, contains
   the root, or is contained by the root except for exactly its direct
   `<root>/wal` directory. This exception applies to the default WAL and an
   explicit override resolving to that same direct directory, provided the
    path really resides there; a default `wal` symlink to a disjoint external
    directory is still a supported separate WAL. A WAL alias to an ancestor
    must never move or delete that ancestor, whose siblings can be independently
    owned databases. Reject ambiguous missing-path traversal rather than
    creating through it. Disjoint external overrides retain WAL-first staging.
    On macOS and Windows, a missing root or WAL component cannot be
    canonicalized: compare unresolved components case-insensitively when
    checking overlap, conservatively rejecting an ambiguous overlap even on a
    case-sensitive volume. If a remaining component contains non-ASCII and
    case folding cannot prove the names distinct under the host filesystem's
    Unicode rules, treat it as a possible overlap; this can reject otherwise
    disjoint non-ASCII paths on those platforms. The direct `<root>/wal`
    exception requires an exact path or a verified existing physical identity,
    never case-folding alone.
    **Reject nesting rather than locking an entire ancestor/subtree.** A
    persistent Native root or `.cl` bundle may not be opened under an existing
    persistent Native root: any regular ancestor `.cypherlite.lock` identifies such a root,
    regardless of whether its owner is currently live. Check ancestors before
    creating the new root or bundle and again after acquiring its own identity,
    before loading/initializing it. An ancestor `.cypherlite.lock` symlink or non-regular
    marker fails closed as invalid storage, not as an alias to follow. Resolve
    relative paths, existing symlink/junction aliases and missing-path prefixes
    to physical ancestors; a bundle hard link physically placed under a Native
    root is still nested. The Native root's *own* `.cypherlite.lock` is its identity, not
    a forbidden ancestor. Do not confuse the separately owned WAL-directory
    `.cypherlite.lock` with a Native root when an external WAL directory is used: a WAL
    directory is not a place for another database root or bundle. Existing
    default `<root>/wal` (including an external symlink alias) and a disjoint
    external WAL override remain supported; explicit root/WAL tree overlaps
    remain rejected as above.

    Before writing a reset marker, allocating a tombstone, renaming, or
    deleting anything, while holding the old root and WAL leases, inspect
    **both recursively deleted trees**: the physical Native root subtree and
    any disjoint external WAL subtree (including a default `wal` symlink
    alias). Before attaching an existing external WAL directory to a Native
    lease, inspect it before creating its lock and rescan under its lease.
    Refuse either operation on any nested `.cypherlite.lock` directory marker or
    regular `.cl` bundle file (case-insensitive extension). Inspect `.cypherlite.lock`
    and `.cl` candidates throughout both trees without opening every ordinary
    regular file for bundle magic: the supported bundle URI requires a `.cl`
    host path. A `.cl` symlink under the root to a file inside the root also
    fails closed even if its physical target has a different filename;
    an outbound symlink is safe to unlink without deleting its target. An
    external `.cl` symlink to a non-`.cl` physical file inside the tree cannot
    be discovered by scanning that tree; the physical file's non-`.cl` name
    does not establish an independently owned bundle there. Exclude only the
    root's own `.cypherlite.lock` and its directly contained `<root>/wal/.cypherlite.lock` when that
    directory has a typed WAL role (even if reset specifies a different
    override), or was created by the current ordinary open.
    An untyped pre-existing direct WAL marker remains a nested identity even
    with an evidenced parent (ADR-0090). In a disjoint WAL scan, exclude only
    that WAL directory's own
    `.cypherlite.lock`, verified by physical file identity; a child `.cypherlite.lock` is never a
    WAL marker. An untyped direct `wal/.cypherlite.lock`, including one under a root
    without its own `.cypherlite.lock`, is ambiguous and must instead be treated as a
    nested database identity;
    recognize `.cypherlite.lock` regardless of ASCII casing, and exempt an own marker
    only when its physical file identity matches the expected lock file (not
    merely because its spelling or parent case-folds to the expected name);
    do not traverse directory symlinks/junctions
    out of the root (the default WAL symlink alias is safe to unlink). Refuse
    with `Storage` on a nested marker, invalid marker, or unreadable subtree,
    leaving live independent owners, contents and names untouched. An
    existing legacy/fixture nested database is protected even if its owner is
    currently closed. Before creating a first root marker in an existing
    directory, also scan for nested identities; rescan under its new lease
    to cover concurrent creation. A child opener seeing a parent marker refuses before
    creating its own identity; rechecking after acquiring its identity closes
    the first-open race when the parent marker was created concurrently.
    A parent reset holding its root lease cannot pass its scan while a child
    successfully creates a new nested identity: either it sees that identity,
    or the child sees the parent's marker and fails before returning. Check
    the physical paths again across namespace changes so an opener never
    initializes a file through an obsolete moved-root identity. On Windows
    inspect reparse points without descending through them and use the same
    fail-closed policy; Unix modes are unaffected. On Windows obtain physical
    file/directory identity from `GetFileInformationByHandle` (volume serial
    plus full file index) on an owned OS handle. Open path identities with
    backup semantics for directories and open-reparse-point semantics for
    final-component aliases; compare a locked file's existing handle directly.
    A failed open or identity query must fail closed, never fall back to path
    spelling. Use a target-specific direct `windows-sys` dependency (Win32
    Foundation and Storage FileSystem features) for this Rust 1.88-compatible
    API. Unix continues to compare device and inode from metadata.

    A child created between the pre-lock WAL scan and WAL lock acquisition
    is refused by the post-lock scan. If that scan rejects a newly acquired
    marker, leave the stable marker in place: unlinking it while other
    openers can acquire or wait on that identity could split ownership.
    Pre-existing nested children are rejected before creating the marker.

5. **Private new paths, unchanged existing paths.** On Unix create every
   newly made Native database directory (root, `wal`, shard directories,
   and any new parent components made for the database) with mode `0700`;
   create new Native database files (including `.cypherlite.lock`, WAL, snapshot,
   and temporary files) and the `.cl` host file with mode `0600`. Apply the
   explicit creation mode even with a permissive umask; a restrictive umask
   may remove permissions, and such an open may then fail. Do not silently
   `chmod` existing paths or migrate their permissions. Existing usable
   directories and regular files retain their modes; existing symlink
   aliases may resolve to such targets, but an existing symlink at a lock
   file's final component is rejected. New files use exclusive creation
   rather than following a pre-existing final symlink; existing database
   files opened for modification must not silently follow a replacement
   symlink. Use Unix `O_NOFOLLOW` or Windows
   `FILE_FLAG_OPEN_REPARSE_POINT` with validation of the opened handle before
   truncating an existing file; pre-open symlink checks alone cannot enforce
   this across a replacement race. On Windows use the same nonblocking
   ownership and exclusive creation, but inherit host ACLs: Unix numeric modes
   are not promised and no Windows ACL hardening is implied. Locks are cooperative on supported
   local filesystems, not an authorization boundary or a guarantee against
   hostile same-user directory mutation or unenforced network locking.

6. **Distinct public contention error.** Add
   `cypherlite_core::Error::Locked(String)` (display:
   `storage locked: {0}`), visible as `cypherlite::Error::Locked` from
   `Database::open` and other public storage-open paths. Use it only when
   nonblocking acquisition reports actual contention, whether in the same
   process or another process, for the root, WAL directory, or bundle.
   Permission failures, unsupported locking, corrupt files, bad path types,
   and other I/O errors remain `Error::Io`/`Error::Storage` as appropriate;
   do not map every lock syscall failure to contention. Document this
   variant, the open/close lifetime, and the new-path modes with a runnable
   exclusive-open example under `docs/api/rust/` when implemented. Do not
   claim `cypherlite_error::StorageError::Locked` is already returned.

## Alternatives considered

- **Lock only at `open_store`, or after constructing `BundleVfs`.** Rejected:
  bundle initialization and codec resolution can already touch persistent
  state, and native directory/WAL setup precedes recovery.
- **Use only an in-process path registry or block waiting for a lock.**
  Rejected: a registry does not protect against other processes or aliases;
  waiting makes a second opener hang instead of receiving a distinct error.
- **Use a `.cl.lock` sidecar or an OS lock on a Native directory.** Rejected:
  the sidecar breaks ADR-0074's one-host-file rule, and Windows directories
  do not provide the ordinary file locking handle required here.
- **Lock only the Native root, ignoring `wal_dir` aliases.** Rejected: two
  different roots can otherwise write the same overridden WAL directory.
- **Lease the resolved WAL but continue accessing default WAL files through
  `root/wal`.** Rejected: a symlink at `root/wal` can be retargeted after
  acquisition, directing open, listing, or rotation to an unleased directory.
- **Remove Native lock files at close, or clone an open descriptor and relock
  it for each independent opener.** Rejected: unlink/recreate splits lock
  identity, while cloned descriptors can share one OS lock and hide duplicate
  opens in the same process.
- **Recursively delete the Native root while its `.cypherlite.lock` is held.** Rejected:
  removing the lock inode permits a new opener to create a different inode
  before the old WAL is gone; Windows may also refuse to delete an open file.
- **Move root before an external WAL and conditionally rename on rollback.**
  Rejected: a fresh root could attach the old WAL during a failed second move;
  `exists()` overlooks dangling symlinks and a check-then-rename can overwrite
  an independently created root. A reset-incomplete marker is needed when a
  moved WAL cannot be restored to a still-present old root.
- **Stage an ancestor/equal/other nested WAL tree under the root lease, or
  rely on a root/WAL lock conflict to detect overlap.** Rejected: the WAL
  directory lock does not cover an independently owned sibling database under
  an ancestor override, and an equal or nested tree can move more than the
  root's owned WAL. Conflict detection is not semantic path validation; only
   the direct, physically contained `<root>/wal` exception is safe to stage
   with the root.
- **Lock every ancestor or recursively lock all descendant database owners.**
  Rejected: cross-tree locking expands the persistent ownership protocol and
  cannot reliably discover a new descendant while reset is already scanning.
  Rejecting nested opens and refusing reset on pre-existing nested identities
  preserves independent owners without moving or deleting their trees.
- **Use Windows `MetadataExt` identity accessors or path strings for reset
  identity.** Rejected: the accessors are unstable at the Rust 1.88 MSRV,
  and spellings cannot prove a physical identity across aliases or renames.
- **Trust the root/WAL locks or scan only the Native root before recursive
  deletion.** Rejected: legacy or independently created nested databases have
  their own lock identities, including inside a disjoint WAL tree, so reset
  could destroy them even while they are live.
- **Automatically tighten modes on existing files or require Unix modes on
  Windows.** Rejected: changing existing user-managed permissions is a
  migration with compatibility implications; Windows uses ACLs, not Unix
  mode bits for this guarantee.
- **Return `Storage(String)` or the disconnected
  `cypherlite_error::StorageError::Locked` for contention.** Rejected: the
  former cannot be pattern-matched as contention and the latter is not the
  error returned by `cypherlite::Database::open`.

## Consequences

- Persistent databases are single-owner across cooperating processes and
  independent same-process opens. Root and WAL identity checks add two
  stable files for Native databases, including an overridden WAL directory;
  the `.cl` layout remains one host file under ADR-0074. Native WAL I/O uses
  its leased canonical directory even when the default WAL path is an alias;
  snapshots continue to use the root directory.
- Opening a locked database fails promptly with the new typed core error;
  failed opens and process crashes release acquired OS locks. A retained
  `Connection`, `StoreTarget`, or VFS handle keeps ownership until it can no
  longer mutate storage; checkpoint and final-drop persistence remain
  protected.
- Private Unix modes apply to newly created database paths, without silently
  changing existing installations. Tests must cover mode bits, aliases,
  explicit WAL-directory contention, failed-open cleanup, crash/drop
  release, and Windows-compatible open and lock behavior. Rust API spec,
  description, and a runnable example must accompany implementation.
- Reset refuses root/WAL overlap outside the direct contained WAL exception
  without changing either tree; canonical identity comparison includes aliases
  and missing paths. Nested Native roots and `.cl` bundles cannot open inside
  an existing Native root; reset refuses pre-existing nested identities without
  mutation, including a live independent owner. Ancestor checks add O(path
  depth) work to persistent opens; first opens of markerless existing Native
  directories and existing external WAL directories also scan O(entries).
  Reset scans both the root and disjoint WAL physical subtrees before any
  staging. A legacy nested tree must be moved out before its containing root
  or external WAL can be attached or reset.
