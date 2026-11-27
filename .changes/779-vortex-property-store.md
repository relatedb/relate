### Notes

- Experimental Vortex columnar property store behind the new `PropStore`
  seam (REL8-1982): the engine's default property path is unchanged; the
  store is an open-time option (`vortex` snapshot format) whose
  implementation lives in the workspace-excluded experiment crate
  `testing/relate-vortex-lab`, keeping the crates.io publish closure
  vortex-free. A report-only CI lane publishes the measurement kernels,
  the LDBC SF0.1 and pokec-shaped numbers, and the engine before/after
  A/B; the dependency decision and its crates.io-release exit condition
  are recorded in the publish-closure ADR.
