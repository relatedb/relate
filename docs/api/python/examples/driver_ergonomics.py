"""Driver-style result ergonomics (ADR-0213): Record rows, single(), peek(),
data(), and to_df().

Run from the repo root after `pip install ./surfaces/relate-python`
(optionally `pip install relate-db[pandas]` for the DataFrame part):

    python docs/api/python/examples/driver_ergonomics.py
"""

import relate
from relate import Record, ResultNotSingleError


def main():
    db = relate.connect(":memory:")
    db.execute("UNWIND [{n: 'alice', s: 9}, {n: 'bob', s: 4}] AS r "
               "CREATE (:Person {name: r.n, score: r.s})")

    # Rows are Records: dict with attribute access and .data().
    cur = db.execute("MATCH (p:Person) RETURN p.name AS name, p.score AS score "
                     "ORDER BY name")
    row = cur.fetchone()
    assert isinstance(row, Record)
    assert row["name"] == row.name == "alice"
    assert row.keys() == ["name", "score"]
    assert row.data() == {"name": "alice", "score": 9}

    # data() converts nodes, relationships and paths to plain dicts/lists.
    node_row = db.execute("MATCH (p:Person {name: 'alice'}) RETURN p").fetchone()
    assert isinstance(node_row["p"], relate.Node)
    assert node_row.data() == {"p": {"name": "alice", "score": 9}}
    assert node_row["p"].data() == {"name": "alice", "score": 9}

    # to_dict() keeps ids, labels and properties structure, JSON-ready.
    full = node_row.to_dict()["p"]
    assert full["labels"] == ["Person"]
    assert full["properties"] == {"name": "alice", "score": 9}
    assert full["id"] == node_row["p"].id

    # peek() does not advance; data() consumes the rest.
    assert cur.peek()["name"] == "bob"
    assert [r.name for r in cur.data()] == ["bob"]
    assert cur.fetchall() == []

    # single() is exactly-one-or-error.
    assert db.execute("MATCH (p:Person) RETURN count(p) AS n").single().n == 2
    try:
        db.execute("MATCH (p:Person) RETURN p").single()
        raise AssertionError("expected ResultNotSingleError")
    except ResultNotSingleError:
        pass

    # to_df() (optional pandas extra) builds from the materialized rows.
    try:
        df = db.execute(
            "MATCH (p:Person) RETURN p.name AS name, p.score AS score "
            "ORDER BY name"
        ).to_df()
        assert df["name"].tolist() == ["alice", "bob"]
        print(df)
    except ImportError:
        print("pandas not installed; to_df() skipped "
              "(pip install 'relate-db[pandas]')")

    # consume() -> ResultSummary: counters, resolved namespace, query type,
    # and engine-reported elapsed_ms (ADR-0215 + ADR-0221) with its
    # parse_ms/plan_ms/execute_ms phase split (ADR-0491).
    summary = db.execute(
        "MATCH (p:Person) DELETE p"
    ).consume()
    assert summary.counters["nodes_deleted"] == 2
    # ADR-0332: a write with no returned columns is "w", not "rw".
    assert summary.query_type == "w"
    # ADR-0491: the split partitions the elapsed window.
    assert summary.parse_ms + summary.plan_ms + summary.execute_ms <= summary.elapsed_ms
    print(summary)

    db.close()
    print("driver ergonomics example OK")


if __name__ == "__main__":
    main()
