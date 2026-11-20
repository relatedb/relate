### Changed

- The Docker image's HTTP and Bolt startup log lines now match the Neo4j
  server's format — `Bolt enabled on 0.0.0.0:7687.` and `HTTP enabled on
  0.0.0.0:7474.` — so log-waiting tooling built for the Neo4j image
  (testcontainers and friends) treats the image as a drop-in replacement;
  the auth banner lines and the MCP startup notice are unchanged.
- A Ctrl-C during `serve`'s startup window is now handled cleanly — the
  shutdown watcher is armed before any port binds, so an early SIGINT
  shuts the server down with its final checkpoint instead of killing the
  process mid-startup.
