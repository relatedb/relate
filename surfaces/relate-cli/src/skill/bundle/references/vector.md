# Vector search

Relate provides first-class vector support: vector properties on nodes,
similarity/distance functions, vector indexes, and a top-k nearest-neighbor
procedure.

## Store embeddings

Store embedding vectors as list properties on nodes:

```cypher
CREATE (:Article {
  title: 'Graph Theory',
  embedding: [0.12, -0.34, 0.56, 0.78, -0.91]
});
```

## Similarity functions

`vector.similarity.cosine(a, b)` returns a **rebounded** similarity in `[0, 1]`
where 1.0 = same direction, 0.5 = orthogonal, 0.0 = opposite. This is the score
the vector index and `db.index.vector.queryNodes` are built on — not the raw
cosine of the angle.

```cypher
RETURN vector.similarity.cosine([1, 0], [0, 1]) AS sim;
-- 0.5
```

`vector.similarity.euclidean(a, b)` maps the squared Euclidean distance into a
similarity: `1 / (1 + squaredEuclideanDistance)`, rebounded to `(0, 1]`.

The similarity functions accept a `VECTOR` value or a plain list of numbers in
each position; the two arguments must have the same length.

## Distance functions

`vector_distance(v1, v2, metric)` returns the raw metric — lower is closer for
every metric (`DOT` is the negated dot product, so a more aligned pair is a
smaller distance). `metric` is a bare keyword: `EUCLIDEAN`,
`EUCLIDEAN_SQUARED`, `MANHATTAN`, `COSINE`, `DOT` or `HAMMING`. It takes
constructed `vector(...)` values:

```cypher
RETURN vector_distance(vector([0, 0], 2, INT8), vector([3, 4], 2, INT8), EUCLIDEAN) AS d;
-- 5.0
```

## Vector constructor

`vector(list, dimensions, coordinateType)` constructs a first-class
`VECTOR<type>(dim)` value. All three arguments are required. `coordinateType`
is a bare keyword: `INT8`/`INTEGER8`, `INT16`/`INTEGER16`, `INT32`/`INTEGER32`,
`FLOAT32`, or `INT`/`INTEGER`/`FLOAT`.

```cypher
RETURN vector([1.0, 2.0, 3.0], 3, FLOAT32) AS v;
```

## Vector utility functions

| Function | Description |
|----------|-------------|
| `vector_dimension_count(v)` | Return vector dimensionality |
| `vector_norm(v, metric)` | Distance from the origin under `EUCLIDEAN` or `MANHATTAN` |

Both take a constructed `vector(...)` value.

## Brute-force similarity search

Use `ORDER BY ... DESC LIMIT k` to find the most similar items without an index:

```cypher
MATCH (a:Article)
WITH a, vector.similarity.cosine(a.embedding, $queryVector) AS score
WHERE score > 0.5
RETURN a.title, score
ORDER BY score DESC
LIMIT 10;
```

## Create a vector index

```cypher
CREATE VECTOR INDEX article_embeddings
FOR (a:Article) ON (a.embedding)
OPTIONS {indexConfig: {`vector.dimensions`: 384, `vector.similarity_function`: 'cosine'}};
```

The `indexConfig` map accepts:
- `` `vector.dimensions` `` — the dimensionality of the indexed vectors (integer)
- `` `vector.similarity_function` `` — `'cosine'` or `'euclidean'` (string)

A vector index is schema metadata (name, label, property, dimensions,
similarity function); Relate builds no ANN structure. Top-k search is an exact
scan over the indexed vectors with a bounded top-k heap, so results are exact
(no recall trade-off, no `ef`-style tuning) and cost grows linearly with the
number of indexed vectors. The Neo4j HNSW tuning keys (`vector.hnsw.m`,
`vector.hnsw.ef_construction`) are accepted and reported for compatibility but
do not change the scan.

## Top-k nearest neighbor search

```cypher
CALL db.index.vector.queryNodes('article_embeddings', 5, $queryVector)
YIELD node, score
RETURN node.title, score;
```

`db.index.vector.queryNodes(indexName, k, queryVector)` returns the top `k`
nodes ordered by similarity score descending. `YIELD node, score` binds the
result columns.

## Hybrid: vector search + graph traversal

```cypher
MATCH (a:Article)
WITH a, vector.similarity.cosine(a.embedding, [0.1, 0.2, 0.3, 0.4, 0.5]) AS score
WHERE score > 0.7
MATCH (author:Person)-[:WROTE]->(a)
RETURN author.name, a.title, score
ORDER BY score DESC;
```
