### Changed

- The Java client now publishes to Maven Central by default from a
  Publish-to-registries workflow dispatch, over a repaired natives gate
  (the `release` environment still gates the credentials, and
  `autoPublish=false` still leaves the Central Portal release in hand;
  uncheck `maven` to publish only the other registries).
