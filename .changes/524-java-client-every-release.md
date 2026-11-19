### Changed

- Every release now publishes the Java client to Maven Central
  automatically through the release pipeline, and the publish profile
  gains a dry run that rehearses it against `main` without uploading.
