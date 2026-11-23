### Changed

- Sustained writes and bulk imports no longer stall behind a WAL-rotation
  checkpoint. A checkpoint now freezes its cut — WAL pin, snapshot pin,
  schema and dirty-id image — and dumps beside in-flight commits instead
  of excluding them (ADR-0643); a commit group waits for a checkpoint
  only when the closed-segment backlog exceeds its bound (ADR-0644).
- A Bolt client writing a CREATE-only stream commits once per batch
  instead of once per statement, so write throughput is no longer
  bounded by per-statement commit cost (ADR-0642).

### Added

- Every completed checkpoint reports its duration and its trigger on
  stderr.
