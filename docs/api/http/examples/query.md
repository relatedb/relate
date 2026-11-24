# Querying over HTTP

Runnable form:
[`surfaces/relate-http/examples/query_v2.rs`](../../../surfaces/relate-http/examples/query_v2.rs)
(`cargo run -p relate-http --example query_v2`). The same exchange with
curl against a server listening on `127.0.0.1:7474` with auth `neo4j:s3cret`:

```sh
# A write: the response carries counters.
curl -u neo4j:s3cret -X POST http://127.0.0.1:7474/db/neo4j/query/v2 \
  -H 'content-type: application/json' \
  -d '{"statement": "CREATE (:City {name: $name})", "parameters": {"name": "Oslo"}}'
# {"data":{"fields":[],"values":[]},"counters":{"nodes_created":1,…},"bookmarks":[]}

# A parameterized read.
curl -u neo4j:s3cret -X POST http://127.0.0.1:7474/db/neo4j/query/v2 \
  -H 'content-type: application/json' \
  -d '{"statement": "MATCH (c:City {name: $name}) RETURN c.name AS name",
       "parameters": {"name": "Oslo"}, "includeCounters": true}'
# {"data":{"fields":["name"],"values":[["Oslo"]]},"counters":{…},"bookmarks":[]}

# A write carrying transaction metadata (ADR-0610): on a store started with
# RELATE_PREVIEW=value-provenance the map serializes into the commit's
# transaction record, where the provenance queries read it. Without the
# opt-in this request answers 400 with the authored preview error — the
# field is never silently dropped. The tx-route bodies take the same field
# (BEGIN / run / COMMIT alike, merged in call order).
curl -u neo4j:s3cret -X POST http://127.0.0.1:7474/db/neo4j/query/v2 \
  -H 'content-type: application/json' \
  -d '{"statement": "CREATE (:City {name: $name})",
       "parameters": {"name": "Bergen"}, "tx_metadata": {"by": "seed-script", "run": 42}}'
# {"data":{"fields":[],"values":[]},"counters":{"nodes_created":1,…},"bookmarks":[]}

# A Cypher error: HTTP 400 with the error envelope.
curl -u neo4j:s3cret -X POST http://127.0.0.1:7474/db/neo4j/query/v2 \
  -d '{"statement": "RETURN"}'
# {"errors":[{"code":"42N06","message":"syntax error: …"}]}

# No credential: HTTP 401 with a WWW-Authenticate challenge.
curl -X POST http://127.0.0.1:7474/db/neo4j/query/v2 -d '{"statement": "RETURN 1"}'
# {"errors":[{"code":"42NFF","message":"authentication required"}]}

# A large answer: --compressed advertises Accept-Encoding: gzip and decodes
# the reply, so the shell still sees the plain JSON envelope (ADR-0385).
# Answers of 65536 serialized bytes or more are also chunked; curl de-chunks
# them transparently too (ADR-0386).
curl --compressed -u neo4j:s3cret -X POST http://127.0.0.1:7474/db/neo4j/query/v2 \
  -H 'content-type: application/json' \
  -d '{"statement": "RETURN range(1, 500) AS r"}'
# {"data":{"fields":["r"],"values":[[[1,2,…,500]]]},"bookmarks":[]}
```

Hosted Neo4j Browser and Console use the unversioned v1.0 alias for typed
requests. The request type selects parameter decoding; `Accept` selects
the response encoding independently:

```sh
curl -u neo4j:s3cret -X POST http://127.0.0.1:7474/db/neo4j/query/v2 \
  -H 'Content-Type: application/vnd.neo4j.query' \
  -H 'Accept: application/vnd.neo4j.query' \
  -d '{"statement": "RETURN $n AS n",
       "parameters": {"n": {"$type": "Integer", "_value": "42"}}}'
```

The result's `data.values` is `[[{"$type":"Integer","_value":"42"}]]`.
