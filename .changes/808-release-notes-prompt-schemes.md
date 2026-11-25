### Notes

- Release tooling fix: the release-notes prompt names the two output
  guard patterns the generation model kept quoting from changelog
  entries — connection-string schemes (`neo4j+s://host:port`) are
  refused as off-host URLs, and log lines embedding `host:port` as
  unaudited execution destinations, fenced or not; each rule carries
  the paraphrase the model should write instead.
