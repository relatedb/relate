# 0114. WAL recovery rejects undecodable records and checks the store version first

- **Status**: Accepted
- **Date**: 2026-09-27
- **Deciders**: CypherLite maintainers (Linear LABS-102, LABS-63)

## Context

ADR-0086 §1 lets recovery trim a complete final frame on the active WAL
segment that "fail[s] its CRC or JSON decode (a relaxed append)", and ADR-0077
records the same tolerance. `scan_seg` implements both cases as `bad_eof_at`
(`cypherlite/cypherlite-storage/src/wal.rs:831-843`), and `Wal::open` then
replaces the segment with its intact prefix (`wal.rs:182-198`).

The checksum half is sound. An unsynced relaxed append can leave a frame whose
payload never reached stable storage, and its CRC fails. The decode half is
not. A frame whose CRC matches its payload was written completely. If that
payload does not decode, the writer produced something this binary does not
understand: in practice, a record from a newer CypherLite with a new `RedoOp`
variant or field. Trimming it silently discards a committed transaction.
ADR-0059 §3 already forbids that ("no silent skip or drop of an undecodable
record"), and it is exactly what a future WAL change must not trigger in older
readers.

`open_store` also runs WAL recovery before it validates the snapshot.
`load_checkpoint_lsn` is lenient, `Wal::open` may repair a tail, and only then
does `load` reject an unsupported `format_version` or `value_encoding`
(`cypherlite/cypherlite-storage/src/lib.rs:491-511`). Recovery can therefore
modify a store written by a newer binary before the reader notices that it
cannot read it.

LABS-102 (ADR-0116, ADR-0120) is the first change that needs newer stores to
fail closed. LABS-63 tracks store versioning in general; this ADR takes only
the two reader rules that every later format change depends on.

## Decision

1. **Only a failed checksum is a torn tail.** At the active segment's physical
   EOF, a complete final frame that fails its CRC stays a tolerated relaxed
   append (ADR-0086 §1, ADR-0077). A complete frame whose CRC matches but whose
   payload does not decode is corruption in every segment and position. Open
   fails with a storage error that names the segment and says the record may
   have been written by a newer CypherLite. Nothing is trimmed.
   `records_after` applies the same rule.
2. **Check the version before WAL recovery.** `open_store` reads and validates
   the snapshot's format marker before `Wal::open`: the Parquet manifest's
   `format_version`, and the JSON `graph.json` `value_encoding` and `read_only`
   guard. An unsupported value fails open with the existing typed error and
   leaves every WAL segment untouched. A store without a snapshot yet (a fresh
   directory, an empty bundle, `:memory:`) opens as today.
3. **Golden stores.** Commit small stores in the current format under
   `testdata/stores/`: a Parquet directory, a JSON directory and a `.cl`
   bundle, each with a checkpoint and a WAL tail. Tests open each one, assert
   its contents, checkpoint, and assert the result is still manifest v3 (or
   `value_encoding: 2`) with `MDBWAL01` segments. Later format ADRs add to this
   set instead of replacing it.

## Alternatives considered

- **Keep trimming undecodable tails.** Rejected: it silently drops a committed
  record written by a newer binary, contradicts ADR-0059 §3, and turns every
  future WAL change into a silent data-loss risk for tools that embed
  different CypherLite versions against the same store.
- **Skip the undecodable record and keep replaying.** Rejected: the recovered
  graph would miss a committed change while reporting success (ADR-0059 §3).
- **Bump the WAL magic for every future change instead.** Rejected: a magic
  bump protects binaries released before it, but readers from this ADR onward
  should fail closed on any undecodable record without one. ADR-0120 still
  bumps the magic, for the binaries already shipped.
- **Build the full LABS-63 migration framework first.** Rejected as a
  prerequisite: these two reader rules are small, testable on their own, and
  needed by ADR-0120 however the broader migrate-on-open work lands.

## Consequences

- Crashes in relaxed durability modes recover exactly as before: incomplete
  frames on any segment and a checksum-failed final frame on the active
  segment are trimmed.
- Opening a store written by a newer CypherLite fails before WAL repair, with
  an error that says so, instead of losing its newest record.
- A development-build record that no longer decodes already failed loudly
  when it was not the final frame (ADR-0059). Now the final frame fails the
  same way.
- The golden stores become the compatibility fixtures for ADR-0120 and
  LABS-63.
- ADR-0077 and ADR-0086 gain Later references notes.
