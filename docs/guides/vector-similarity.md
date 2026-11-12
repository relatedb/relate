# Vector Similarity Search

This guide covers practical patterns for using vector embeddings with Relate.

## Store embeddings on nodes

Store embedding vectors as list properties:

```cypher
CREATE (:Article {
  title: 'Graph Theory',
  embedding: [0.12, -0.34, 0.56, 0.78, -0.91]
});
```

## Find top-K similar items

Use `ORDER BY ... DESC LIMIT k` to find the most similar items:

```cypher
MATCH (a:Article)
WITH a, vector.similarity.cosine(a.embedding, $queryVector) AS score
WHERE score > 0.5
RETURN a.title, score
ORDER BY score DESC
LIMIT 10;
```

Pass the query vector as a parameter from the host language (Python embedding API shown here):

```python
result = db.execute(query, {
    "queryVector": [0.12, -0.34, 0.56, 0.78, -0.91],
})
```

## Index-backed search: the SEARCH clause

The examples above score every candidate (`vector.similarity.cosine` + `ORDER
BY`). With a vector index, the `SEARCH` clause does the top-K lookup (and the
candidate scan) inside the engine:

```cypher
CREATE VECTOR INDEX article_embed FOR (a:Article) ON a.embedding WITH [a.topic];

MATCH (a:Article)
SEARCH a IN (VECTOR INDEX article_embed FOR $queryVector LIMIT 10)
RETURN a.title, score AS similarity;
```

`search` filters candidates by the index's `WITH [...]`-declared properties
with simple comparisons joined by `AND` — applied before the ranking and the
`LIMIT`, so the filter cannot be starved by non-matching near neighbours:

```cypher
MATCH (a:Article)
SEARCH a IN (VECTOR INDEX article_embed FOR $queryVector
             WHERE a.topic = 'graph'
             LIMIT 10)
RETURN a.title, score AS similarity;
```

Filtering on a property the index does not declare in `WITH [...]` raises
`WrongPropertyForVectorSearchFilter`. `SKIP` is not allowed on vector searches
(full-text only). For keyword search over text properties, `CREATE FULLTEXT
INDEX` plus `SEARCH ... IN (FULLTEXT INDEX ...)` covers the term-matching case —
see the [Cypher syntax reference](../reference/cypher-syntax.md#search).

## Hybrid: vector + full-text

A hybrid query composes the two index kinds by chaining `MATCH ... SEARCH`
clauses through `WITH` — one `SEARCH` per `MATCH`, and the second search
intersects with the first: a candidate that differs from the already-bound
variable is dropped, not re-bound.

```cypher
MATCH (a:Article)
SEARCH a IN (VECTOR INDEX article_embed FOR $queryVector LIMIT 50)
WITH a
MATCH (a) SEARCH a IN (FULLTEXT INDEX article_text FOR 'graph database' LIMIT 50)
RETURN a.title, score AS text_score;
```

Semantics, matching Neo4j:

- semantic recall (vector) then exact term filtering (full-text), or the
  reverse order — each `MATCH ... SEARCH` narrows the rows the next sees
- the intersection can be empty; a full-text term matching none of the vector
  candidates yields no rows
- two `SEARCH` clauses in the *same* `MATCH` are a syntax error

For rank-fusion hybrid retrieval (combining both ranked lists with weighted
reciprocal rank fusion rather than intersecting them), see Neo4j's
[hybrid search guide](https://neo4j.com/developer/genai-ecosystem/hybrid-search/) --
the same WRRF pattern composes over the `SEARCH` clause's `SCORE AS` output.

## Hybrid: vector search + graph traversal

Find similar articles, then traverse to find their authors:

```cypher
MATCH (a:Article)
WITH a, vector.similarity.cosine(a.embedding, [0.1, 0.2, 0.3, 0.4, 0.5]) AS score
WHERE score > 0.7
MATCH (author:Person)-[:WROTE]->(a)
RETURN author.name, a.title, score
ORDER BY score DESC;
```

## Threshold-based filtering vs. top-K

**Threshold** — return everything above a similarity score:

```cypher
MATCH (d:Document)
WITH d, vector.similarity.cosine(d.embedding, $query) AS sim
WHERE sim > 0.8
RETURN d.title, sim;
```

**Top-K** — return the K most similar regardless of absolute score:

```cypher
MATCH (d:Document)
RETURN d.title, vector.similarity.cosine(d.embedding, $query) AS sim
ORDER BY sim DESC
LIMIT 5;
```

## Choosing a distance function

| Function | Use case | Notes |
|---|---|---|
| `vector.similarity.cosine(a, b)` | Text, normalized embeddings | Rebounded to `[0, 1]`; `1.0` = same direction. Ignores magnitude. |
| `vector.similarity.euclidean(a, b)` | Ranking by absolute distance | `1 / (1 + squaredEuclideanDistance)`, rebounded to `(0, 1]`. |
| `vector_distance(a, b, metric)` | Thresholds on a raw metric | `metric` is `EUCLIDEAN`, `EUCLIDEAN_SQUARED`, `MANHATTAN`, `COSINE`, `DOT` or `HAMMING`; lower = closer. |

`vector_distance(..., DOT)` returns the negated dot product, so a larger dot
product (more aligned) is a smaller distance. The similarity functions accept a
plain list of numbers; `vector_distance` takes a constructed `vector(...)` value.

## Vector index search with CALL procedure

Create a vector index and use `db.index.vector.queryNodes` for top-K nearest neighbor search:

```cypher
CREATE VECTOR INDEX article_embeddings
FOR (a:Article) ON (a.embedding)
OPTIONS {indexConfig: {`vector.dimensions`: 384, `vector.similarity_function`: 'cosine'}};
```

```cypher
CALL db.index.vector.queryNodes('article_embeddings', 5, $queryVector)
YIELD node, score
RETURN node.title, score;
```

This returns the 5 most similar nodes ordered by cosine similarity score (descending).

## Vector utility functions

`vector_dimension_count(v)` reports a vector's dimensionality and
`vector_norm(v, metric)` its distance from the origin (`EUCLIDEAN` or
`MANHATTAN`). Both, like `vector_distance`, take a constructed `vector(...)`
value:

```cypher
WITH vector([3, 4], 2, INT8) AS v
RETURN vector_dimension_count(v) AS dims, vector_norm(v, EUCLIDEAN) AS len;
// Returns 2 and 5.0
```

Use `vector()` to construct a first-class vector value from a list, its dimensionality, and a coordinate type (`FLOAT32`, `INT8`, `INT16`, or `INT32`):

```cypher
RETURN vector([0.1, 0.2, 0.3], 3, FLOAT32) AS v;
```

:::tip
A vector index is schema metadata, not an ANN structure: top-k search is an exact scan over the indexed vectors (bounded top-k heap), so results are exact with no recall trade-off, and cost grows linearly with the number of indexed vectors (ADR-0256). The Neo4j HNSW tuning keys are accepted for compatibility but do not change the scan. The index is configured via `OPTIONS` on `CREATE VECTOR INDEX` — set `` `vector.dimensions` `` and `` `vector.similarity_function` `` (`cosine` or `euclidean`) inside `indexConfig`.
:::
