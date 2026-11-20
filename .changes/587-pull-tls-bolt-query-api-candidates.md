### Fixed

- `relate pull` (and `push`, the same `NEO4J_URI` resolution) now reaches
  a self-hosted server through a TLS Bolt `NEO4J_URI`: a
  `neo4j+s://host:7687` URI (also `neo4j+ssc`, `bolt+s`, `bolt+ssc`; the
  port may be omitted) resolves through ordered Query API origin
  candidates — `https://host:7473` then `https://host` (443), a
  plaintext Bolt URI `http://host:7474` then `http://host` (80) — the
  server's own port first, the generic web port as the proxied fallback,
  the first origin answering at the HTTP layer winning (ADR-0633). A
  failure that does not prove the request was undelivered (a timeout, a
  reset connection, a TLS or protocol error) is never retried on
  another origin, so a delivered request is never re-POSTed.
  Previously the TLS spellings built `https://host` (port 443 only,
  where a self-hosted server has no Query API) and a `+s` URI with an
  explicit non-default bolt port reused that port verbatim as the Query
  API port. Malformed ports (`neo4j://host:abc`, previously silently
  remapped to :7474) and scheme-less or unknown-scheme Aura-suffixed
  URIs now fail loudly with fixed text instead of being accepted
  silently; an `http://` URI to an Aura host is used as-is instead of
  being silently upgraded to `https`. Valid Aura and plain-Bolt URIs
  are unchanged.
