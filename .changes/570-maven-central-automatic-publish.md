### Changed

- The Java client's Maven Central deploy now rides the release's
  `registries` opt-in like npm, PyPI and crates.io, and a deployment
  publishes itself once Central validates it — no manual
  publish-registries dispatch afterwards, no Portal button. An
  unchecked dispatch (the default) publishes nothing anywhere, the
  Java client included, where the v1.2.0 cut attempted the jars from
  every release and failed importing an empty signing key. The Central
  credentials moved to repository secrets so the release's chained
  call can carry them, and the publishing plugin moves 0.7.0 → 0.12.0,
  whose deployment model reads the `warnings` field the Portal API
  newly returns — 0.7.0 crashed polling a validation the upload had
  already landed. A direct `publish-registries.yml` dispatch against
  the tag remains the recovery arm.
