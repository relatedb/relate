### Changed

- The per-commit WAL record cap — a hard 64 MiB constant since ADR-0086 —
  is now 256 MiB by default and configurable at open:
  `OpenOptions::wal_payload_limit` for embedders, the
  `RELATE_WAL_PAYLOAD_LIMIT` environment variable (same `SIZE` grammar as
  every memory knob) for the CLI and served surfaces. A zero, negative,
  unparsable, or over-4-GiB value fails the open loudly naming the
  source; unset keeps the 256 MiB default; the refusal message names the
  configured value and keeps the smaller-transaction
  (`CALL { … } IN TRANSACTIONS`) remedy. One big commit in one statement
  no longer requires the rewrite — for example
  `RELATE_WAL_PAYLOAD_LIMIT=512mb relate --database ./mydata`
  (ADR-0650).
