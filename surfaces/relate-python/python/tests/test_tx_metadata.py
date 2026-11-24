"""The ``tx_metadata`` kwarg on ``execute`` (ADR-0610).

The kwarg is the Python surface's embedded metadata channel: it seeds the
connection's per-transaction metadata map before the statement runs, and
the map is readable back through ``CALL tx.getMetaData()`` while the
transaction is open (the persisted record it serializes into is the
engine-side ``value_provenance.rs`` suite's coverage, read there through
the transaction-records observation surface).
"""

import inspect

import pytest

import relate

PREVIEW_ERROR = (
    "preview feature 'value-provenance' is not enabled: "
    "set RELATE_PREVIEW=value-provenance"
)


def gated_connect(monkeypatch):
    """Connect with the ``value-provenance`` opt-in scoped to the test.

    The opt-in is read once per open (ADR-0371), so it must be set before
    connecting; monkeypatch keeps it from leaking into other modules'
    stores (the ``test_namespace.py`` pattern).
    """
    monkeypatch.setenv("RELATE_PREVIEW", "value-provenance")
    return relate.connect(":memory:")


def live_metadata(conn):
    """The connection's live per-transaction metadata map (ADR-0610)."""
    cursor = conn.execute("CALL tx.getMetaData() YIELD metadata RETURN metadata")
    return cursor.single()["metadata"]


def test_connection_execute_tx_metadata_seeds_the_map(monkeypatch):
    conn = gated_connect(monkeypatch)
    conn.autocommit = False
    try:
        # The sqlite3-style convenience path: the kwarg seeds the open
        # transaction's map before the statement runs.
        conn.execute(
            "CREATE (p:Person {name: $name})",
            {"name": "Ada"},
            tx_metadata={"by": "app", "run": 42},
        )
        assert live_metadata(conn) == {"by": "app", "run": 42}
        # Later keys win (ADR-0610's merge order): a second seed through
        # the statement channel merges with the kwarg's.
        conn.execute("CALL tx.setMetaData({by: 'pipeline'})")
        assert live_metadata(conn) == {"by": "pipeline", "run": 42}
        conn.commit()
    finally:
        conn.close()


def test_cursor_execute_tx_metadata_seeds_the_map(monkeypatch):
    conn = gated_connect(monkeypatch)
    conn.autocommit = False
    try:
        cursor = conn.cursor()
        cursor.execute(
            "CREATE (p:Person {name: 'Ada'})", tx_metadata={"by": "app"}
        )
        assert live_metadata(conn) == {"by": "app"}
        # The map is per transaction, cleared after commit: a later
        # statement in a new transaction starts from an empty map.
        conn.commit()
        cursor.execute("CREATE (p:Person {name: 'Bob'})")
        assert live_metadata(conn) == {}
        conn.commit()
    finally:
        conn.close()


def test_native_execute_tx_metadata_seeds_the_map(monkeypatch):
    monkeypatch.setenv("RELATE_PREVIEW", "value-provenance")
    db = relate.Database(":memory:")
    try:
        db.begin_transaction()
        db.execute("CREATE (p:Person {name: 'Ada'})", tx_metadata={"by": "app"})
        result = db.execute("CALL tx.getMetaData() YIELD metadata RETURN metadata")
        assert result.data[0]["metadata"] == {"by": "app"}
        db.commit_transaction()
    finally:
        db.close()


def test_tx_metadata_is_keyword_only():
    for method in (relate.Connection.execute, relate.Cursor.execute):
        signature = inspect.signature(method)
        parameter = signature.parameters["tx_metadata"]
        assert parameter.kind is inspect.Parameter.KEYWORD_ONLY
        assert parameter.default is None


def test_tx_metadata_without_opt_in_fails_the_preview_error(monkeypatch):
    # The embedded channel is loud on a store that never opted in
    # (ADR-0610): the authored preview error — a `SemanticError`, so the
    # DB-API class is OperationalError — never a silent drop, and the
    # statement never runs, so the connection stays usable.
    monkeypatch.delenv("RELATE_PREVIEW", raising=False)
    conn = relate.connect(":memory:")
    try:
        with pytest.raises(relate.OperationalError) as exc_info:
            conn.execute("CREATE (:Person {name: 'Ada'})", tx_metadata={"by": "app"})
        assert PREVIEW_ERROR in str(exc_info.value)
        assert conn.execute("MATCH (n) RETURN count(n) AS n").single()["n"] == 0
    finally:
        conn.close()


def test_non_dict_tx_metadata_is_a_programming_error(db):
    with pytest.raises(relate.ProgrammingError) as exc_info:
        db.execute("CREATE (:Person {name: 'Ada'})", tx_metadata=42)
    assert "transaction metadata must be a dict" in str(exc_info.value)
