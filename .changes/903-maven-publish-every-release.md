### Notes

- Release tooling change: the Java client now publishes to Maven Central
  with every ordinary release (`publish-maven-central` in `release.yml`
  gates on `!offline` alone, ADR-0704) instead of behind the manual
  `registries` opt-in. The npm/PyPI/crates.io legs keep the opt-in;
  `publish-registries.yml` with `maven=true` remains the recovery arm.
