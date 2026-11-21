### Changed

- Warm plan-cache hits no longer re-run the front end per statement: repeated statements with fresh literals see a 9× smaller planning window (point read 6.5 µs → 0.7 µs p50 in-process). A value-dependent compile verdict still surfaces as the compile's own error — a cached plan that fails with a type error re-runs the statement once uncached (REL8-1120, ADR-0651).
