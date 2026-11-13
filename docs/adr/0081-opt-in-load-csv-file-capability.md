# 0081. Opt-in, rooted local-file capability for LOAD CSV

- **Status**: Proposed
- **Date**: 2026-09-23
- **Deciders**: CypherLite maintainers

## Context

`LOAD CSV` currently accepts query-supplied local paths (including `file://`)
and reads them with `std::fs::read_to_string`. A query can read arbitrary
files, block on `/dev/zero` or a FIFO, amplify reads by repeating the same
source across input rows, and receive OS error text containing filesystem
paths (LABS-78; security-review handoff stream 2). This applies to queries
from every client, including Bolt. ADR-0257 bounds query *text* and ADR-0259
provisions its execution stack; neither limits the filesystem access or bytes
read by an executing query.

Using the database location as the CSV root would exclude `:memory:` and
`.cl` bundle databases and would implicitly grant access to files beside the
database. The file capability instead needs a separately configured root.
The existing `artists.csv` case is a canned in-memory fixture for the
evaluation suite, not a general network or file loader.

## Decision

**Local-file `LOAD CSV` is denied by default.** A database may opt in by
setting `OpenOptions.load_csv_dir: Option<PathBuf>` to a pre-existing CSV
directory. `OpenOptions.load_csv_max_bytes: Option<u64>` configures the
maximum number of source bytes for each file; setting a limit alone never
enables file access. At database open, resolve a relative configured directory
against the process working directory, canonicalize it, and require an
existing directory. Retain that canonical directory as the sole allowlist
root; it need not be, and is never inferred from, the database directory.
This works equally for `:memory:` and `.cl` databases.

`CYPHERLITE_LOAD_CSV_DIR` is an explicit environment opt-in when
`OpenOptions.load_csv_dir` is `None`. A supplied `OpenOptions.load_csv_dir`
takes precedence over the environment. Both `cypherlite` and
`cypherlite-bolt` expose `--load-csv-dir PATH`; their flag takes precedence
over `CYPHERLITE_LOAD_CSV_DIR`, and with neither set they deny local files.
An absent root remains disabled even if `load_csv_max_bytes` is set.

The default per-file read limit is **8 MiB (8,388,608 bytes)**, allowing
ordinary small CSV imports while bounding a single cached source to a
modest size. A caller may override it with
`OpenOptions.load_csv_max_bytes = Some(n)` only for
`1 <= n <= 67,108,864` (**64 MiB ceiling**); reject zero or larger
values at database open as invalid configuration, never interpret either as
unlimited. The ceiling accommodates larger local imports while keeping even
an explicitly raised per-file read bounded. The limit applies to actual
bytes consumed, regardless of pre-read file size: read at most `limit + 1`
bytes, and reject the source if the extra byte exists. Do not use an
unbounded `read_to_string` or trust metadata length as the enforcement.

For query-supplied sources, accept plain **relative** paths resolved under
the configured root and local `file:///` **absolute** paths only when the
target remains under that root. Reject bare absolute paths, lexical `..`
traversal, all other schemes, `file://` authorities (including `localhost`),
and malformed file URLs. Canonicalize the candidate target, verify it lies
within the canonical root by path components rather than string prefix,
and require it to be a regular file before opening it. An in-root symlink
to an out-of-root target is therefore rejected; devices, directories and
FIFOs are rejected as non-regular. There is no HTTP fetching.

On Unix, a local absolute URL has the form `file:///tmp/rows.csv`. On Windows,
only a forward-slash drive-rooted URL such as `file:///C:/csv/rows.csv` maps
to the absolute path `C:/csv/rows.csv` before canonicalization. A URL with an
authority, backslashes, a UNC/device/verbatim prefix, a drive-relative path,
percent encoding or lexical traversal is malformed; the canonical root still
confines the resulting path.

Every client-facing denial, invalid path, unavailable/non-regular file, or
I/O failure gets a generic typed `LOAD CSV file unavailable` error; a file
that exceeds the cap gets a generic typed `LOAD CSV file exceeds byte limit`
error. Invalid option values fail database open with a generic configuration
error. None of these errors includes the requested path, canonical path,
OS errno, or OS error text.

Cache successfully read source bytes (or parsed rows) by canonical target
**per statement**, shared across its input rows and nested execution paths.
Repeated references to the same file in one statement read source bytes at
most once; each input row still produces its own CSV result rows. A later
statement gets a new memo, so the first successful read is a stable snapshot
only for that statement. The exact
`https://data.neo4j.com/bands/artists.csv` URL remains a hardcoded,
in-memory fixture, even without a file root. It does not issue a
network request and does not enable other URLs or local filesystem access.

## Alternatives considered

- **Allow local files by default under the database directory.** Rejected:
  implicit access exposes files beside the database to any query, and
  `:memory:` and `.cl` databases have no suitable CSV directory.
- **Allow any path after opting in, relying on a byte cap alone.** Rejected:
  the cap limits volume but still allows reading sensitive files outside
  the intended import directory; special files can also block before the
  bound is reached.
- **Validate only metadata size and then read the entire file.** Rejected:
  size can change after inspection and special files may not report a useful
  length; only a bounded actual read enforces the limit.
- **Remove the canned URL or enable arbitrary HTTP to support it.**
  Rejected: removing it regresses the existing CIP evaluation case, while
  network fetching creates a new, unrelated capability. An exact canned
  fixture preserves the test without granting either file or network access.

## Consequences

- Applications that need local CSV imports must configure a pre-existing
  directory explicitly through the Rust option, CLI/Bolt flag, or environment
  variable; existing unrestricted local imports now receive generic errors.
  The options and opt-ins require public specs, descriptions, runnable
  examples, and CLI help/flag-table updates with their implementation.
- Canonicalization plus the regular-file check prevents static traversal,
  symlink escapes and special-file reads. It does **not** make inspection
  and opening atomic: a concurrent filesystem mutation (such as swapping a
  symlink between canonicalization/check and open) can invalidate the check.
  Atomic directory-relative opening would require a separate design.
- A statement holds at most one bounded copy of each distinct successfully
  referenced source in its memo; repeated input rows do not multiply file
  reads. The per-file cap is not a total query-memory or output-row governor.
- The exact artists fixture continues to work without a configured
  root; other HTTPS URLs stay unavailable.

## Later references

ADR-0108 closes the gap documented in the second Consequences bullet above,
which stated that canonicalization "does not make inspection and opening
atomic" and that "atomic directory-relative opening would require a separate
design." ADR-0108
records the canonical target's physical file identity at validation, opens it
with `O_NOFOLLOW` on Unix, and requires the opened handle's identity to equal
the recorded one before the bounded read, denying with the generic `LOAD CSV
file unavailable` on mismatch. The remaining ADR-0081 rules (root
confinement, regular-file requirement, byte cap, statement memoization and
generic errors) are unchanged.
