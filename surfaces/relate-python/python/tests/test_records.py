"""Driver-style result ergonomics (ADR-0213): Record rows, data(), single(),
peek(), fetch(), keys(), to_df().

Every row a cursor yields is a ``Record`` — a ``dict`` with attribute access
and driver conveniences. The DB-API contract (rows are mappings) is unchanged.
"""

import pathlib
import pickle
import sys

import pytest

import relate
from relate import Record, ResultNotSingleError

class _Stub:
    """A test double forwarding everything to the wrapped object, with
    selected attributes overridden by the given values."""

    def __init__(self, inner, **overrides):
        self._inner = inner
        self._overrides = overrides

    def __getattr__(self, name):
        if name in self._overrides:
            return self._overrides[name]
        return getattr(self._inner, name)


@pytest.fixture
def three_rows(db):
    """A cursor holding three (name, score) rows."""
    cur = db.cursor()
    cur.execute(
        "UNWIND [{n: 'a', s: 1}, {n: 'b', s: 2}, {n: 'c', s: 3}] AS row "
        "RETURN row.n AS name, row.s AS score"
    )
    return cur


class TestRecord:
    def test_is_a_dict(self, three_rows):
        row = three_rows.fetchone()
        assert isinstance(row, dict)
        assert isinstance(row, Record)
        assert row == {"name": "a", "score": 1}
        assert dict(row) == {"name": "a", "score": 1}

    def test_attribute_access(self, three_rows):
        row = three_rows.fetchone()
        assert row.name == "a"
        assert row.score == 1
        with pytest.raises(AttributeError):
            row.missing

    def test_keys_follow_column_order(self, three_rows):
        row = three_rows.fetchone()
        assert row.keys() == ["name", "score"]

    def test_attribute_access_is_read_only(self, three_rows):
        row = three_rows.fetchone()
        with pytest.raises(AttributeError):
            row.name = "b"

    def test_keys_returns_a_copy(self, three_rows):
        row = three_rows.fetchone()
        keys = row.keys()
        keys.append("injected")
        assert row.keys() == ["name", "score"]

    def test_data_is_a_plain_dict_copy(self, three_rows):
        row = three_rows.fetchone()
        data = row.data()
        assert type(data) is dict
        assert data == {"name": "a", "score": 1}

    def test_pickle_round_trip(self, three_rows):
        row = three_rows.fetchone()
        back = pickle.loads(pickle.dumps(row))
        assert back == row
        assert back.keys() == ["name", "score"]


class TestFetchConveniences:
    def test_data_consumes_remaining(self, three_rows):
        rows = three_rows.data()
        assert [r["name"] for r in rows] == ["a", "b", "c"]
        assert three_rows.fetchall() == []

    def test_data_after_partial_fetch(self, three_rows):
        three_rows.fetchone()
        assert [r["name"] for r in three_rows.data()] == ["b", "c"]

    def test_peek_does_not_advance(self, three_rows):
        assert three_rows.peek()["name"] == "a"
        assert three_rows.peek()["name"] == "a"
        assert three_rows.fetchone()["name"] == "a"

    def test_peek_at_end_is_none(self, three_rows):
        three_rows.fetchall()
        assert three_rows.peek() is None

    def test_fetch_matches_fetchmany(self, three_rows):
        assert [r["name"] for r in three_rows.fetch(2)] == ["a", "b"]
        assert [r["name"] for r in three_rows.fetch(5)] == ["c"]
        assert three_rows.fetch(1) == []

    def test_keys_matches_columns(self, three_rows):
        assert three_rows.keys() == three_rows.columns == ["name", "score"]

    def test_iteration_yields_records(self, three_rows):
        assert [r.name for r in three_rows] == ["a", "b", "c"]


class TestSingle:
    def test_one_row(self, db):
        cur = db.execute("RETURN 42 AS answer")
        row = cur.single()
        assert row.answer == 42

    def test_zero_rows(self, db):
        cur = db.execute("MATCH (n:Nope) RETURN n")
        with pytest.raises(ResultNotSingleError):
            cur.single()

    def test_more_than_one(self, three_rows):
        with pytest.raises(ResultNotSingleError):
            three_rows.single()

    def test_consumes_on_success(self, db):
        cur = db.execute("RETURN 1 AS x")
        cur.single()
        assert cur.fetchall() == []


class TestToDf:
    def test_dataframe(self, three_rows):
        pd = pytest.importorskip("pandas")
        df = three_rows.to_df()
        assert isinstance(df, pd.DataFrame)
        assert list(df.columns) == ["name", "score"]
        assert df["name"].tolist() == ["a", "b", "c"]
        assert df["score"].tolist() == [1, 2, 3]

    def test_consumes(self, three_rows):
        pytest.importorskip("pandas")
        three_rows.to_df()
        assert three_rows.fetchall() == []

    def test_empty_result(self, db):
        pytest.importorskip("pandas")
        df = db.execute("MATCH (n:Nope) RETURN n").to_df()
        assert len(df) == 0

    def test_missing_pandas_errors_with_install_hint(self, three_rows, monkeypatch):
        import sys

        monkeypatch.setitem(sys.modules, "pandas", None)
        with pytest.raises(ImportError, match="pandas"):
            three_rows.to_df()


class TestResultGraph:
    @pytest.fixture
    def graph_cursor(self, db):
        db.execute("CREATE (a:Person {name: 'Alice'})-[:KNOWS]->(b:Person {name: 'Bob'})")
        db.execute("MATCH (a:Person {name: 'Alice'}) CREATE (c:Person {name: 'Carol'})-[:KNOWS]->(a)")
        cur = db.execute("MATCH (a:Person)-[r:KNOWS]->(b:Person) RETURN a, r, b")
        return cur

    def test_graph_deduplicates_entities(self, graph_cursor):
        g = graph_cursor.graph()
        assert len(g.nodes) == 3          # Alice, Bob, Carol — once each
        assert len(g.relationships) == 2
        assert g.nodes[0].id < g.nodes[1].id < g.nodes[2].id  # id-ordered
        assert len(g) == 5

    def test_graph_independent_of_fetch_position(self, graph_cursor):
        graph_cursor.fetchone()           # consume one row first
        g = graph_cursor.graph()
        assert len(g.nodes) == 3 and len(g.relationships) == 2

    def test_graph_from_nested_values_and_paths(self, db):
        db.execute("CREATE (a:P {name: 'a'}), (b:P {name: 'b'})")
        db.execute("MATCH (a:P {name: 'a'}), (b:P {name: 'b'}) CREATE (a)-[:REL]->(b)")
        cur = db.execute(
            "MATCH p = (a:P)-[r]->(b:P) RETURN [a, b] AS nodes, {rel: r} AS wrapped, p AS path"
        )
        g = cur.graph()
        assert len(g.nodes) == 2 and len(g.relationships) == 1

    def test_graph_empty_when_no_entities(self, three_rows):
        g = three_rows.graph()
        assert not g.nodes and not g.relationships
        assert len(g) == 0
        assert "ResultGraph" in repr(g)

    def test_node_relationship_driver_parity_members(self, graph_cursor):
        g = graph_cursor.graph()
        node = g.nodes[0]
        assert node.element_id == f"4:rl-id:n{node.id}"
        assert dict(node.items()) == dict(node.properties)
        rel = g.relationships[0]
        assert rel.element_id == f"5:rl-id:r{rel.id}"
        assert dict(rel.items()) == dict(rel.properties)
        # r.start / r.end are short aliases of the integer endpoint ids.
        assert (rel.start, rel.end) == (rel.start_node, rel.end_node)


class TestToArrow:
    def test_arrow_table(self, three_rows):
        pytest.importorskip("pyarrow")
        table = three_rows.to_arrow()
        assert table.num_rows == 3
        assert table.column_names == ["name", "score"]
        assert table.column("name").to_pylist() == ["a", "b", "c"]

    def test_arrow_consumes_rows(self, three_rows):
        pytest.importorskip("pyarrow")
        three_rows.to_arrow()
        assert three_rows.fetchone() is None

    def test_arrow_empty_result_keeps_columns(self, db):
        pytest.importorskip("pyarrow")
        cur = db.execute("MATCH (p:Nothing) RETURN p.name AS name")
        table = cur.to_arrow()
        assert table.num_rows == 0
        assert table.column_names == ["name"]

    def test_missing_pyarrow_errors_with_install_hint(self, three_rows, monkeypatch):
        monkeypatch.setitem(sys.modules, "pyarrow", None)
        with pytest.raises(ImportError, match=r"relate-db\[arrow\]"):
            three_rows.to_arrow()



class TestConsume:
    def test_summary_counters_and_metadata(self, db):
        db.execute("CREATE (n:P {name: 'a'})")
        summary = db.execute("CREATE (n:P {name: 'b'})").consume()
        assert summary.counters["nodes_created"] == 1
        assert summary.query_type == "w"
        assert summary.namespace == "neo4j"
        assert summary.elapsed_ms >= 0.0
        # ADR-0491: the phase split is engine-stamped and partitions the
        # elapsed window — never more than the total.
        assert summary.parse_ms >= 0.0
        assert summary.plan_ms >= 0.0
        assert summary.execute_ms >= 0.0
        assert summary.parse_ms + summary.plan_ms + summary.execute_ms <= summary.elapsed_ms
        assert "ResultSummary" in repr(summary)

    def test_elapsed_ms_re_sources_the_engine_value(self, db, monkeypatch):
        # ADR-0221: the summary's elapsed_ms is the engine-stamped value
        # on the native QueryResult, not a client-edge perf_counter. Stamp
        # a known value at the native seam and require the exact value
        # back — a client-edge measurement would fail this. The same holds
        # for the ADR-0491 phase split.
        sentinel = 123.456
        native_execute = db._db.execute
        monkeypatch.setattr(
            db,
            "_db",
            _Stub(
                db._db,
                execute=lambda *a, **k: _Stub(
                    native_execute(*a, **k),
                    elapsed_ms=sentinel,
                    parse_ms=1.5,
                    plan_ms=2.5,
                    execute_ms=3.5,
                ),
            ),
        )
        summary = db.execute("RETURN 1 AS n").consume()
        assert summary.elapsed_ms == sentinel
        assert summary.parse_ms == 1.5
        assert summary.plan_ms == 2.5
        assert summary.execute_ms == 3.5

    def test_consume_discards_remaining_rows_and_is_idempotent(self, three_rows):
        summary = three_rows.consume()
        assert three_rows.fetchone() is None
        again = three_rows.consume()
        assert again.counters == summary.counters
        assert again.query_type == summary.query_type

    def test_read_summary(self, three_rows):
        assert three_rows.consume().query_type == "r"

    def test_consume_before_any_statement(self, db):
        with pytest.raises(relate.ProgrammingError):
            db.cursor().consume()

    def test_summary_constructor_is_keyword_only(self):
        # The constructor takes its fields by name only (LABS-944): four
        # adjacent millisecond floats and two optionals must never ride a
        # ten-parameter positional list.
        summary = relate.ResultSummary(
            counters={"nodes_created": 1},
            namespace="neo4j",
            query_type="w",
            elapsed_ms=3.0,
            parse_ms=1.0,
            plan_ms=1.0,
            execute_ms=1.0,
            notifications=[],
        )
        assert summary.counters["nodes_created"] == 1
        assert summary.plan is None and summary.bookmark is None
        with pytest.raises(TypeError):
            relate.ResultSummary(
                {"nodes_created": 1}, "neo4j", "w", 3.0, 1.0, 1.0, 1.0, []
            )
