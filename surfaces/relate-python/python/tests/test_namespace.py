"""Session namespace selection (ADR-0117 §4).

``relate.connect(database=..., namespace=...)`` picks the namespace the
connection's statements run in when they carry no ``USE`` clause:
``"neo4j"`` (default), ``"system"`` or ``"system_ontologies"``, matched
case-insensitively. ``Connection.namespace`` reports it and
``Connection.set_namespace(name)`` changes it between transactions.
"""

import pytest

import relate


def test_connect_defaults_to_neo4j(db):
    assert db.namespace == "neo4j"


def test_connect_accepts_namespace_keyword():
    conn = relate.connect(":memory:", namespace="system")
    try:
        assert conn.namespace == "system"
    finally:
        conn.close()


def test_connect_namespace_is_case_insensitive():
    conn = relate.connect(":memory:", namespace="SYSTEM")
    try:
        assert conn.namespace == "system"
    finally:
        conn.close()


def test_connect_unknown_namespace_raises_operational_error():
    with pytest.raises(relate.OperationalError) as exc_info:
        relate.connect(":memory:", namespace="nosuch")
    assert exc_info.value.args[0].endswith("Graph not found: nosuch")


def test_set_namespace_switches_between_transactions(db):
    db.set_namespace("SYSTEM")
    assert db.namespace == "system"
    db.set_namespace("neo4j")
    assert db.namespace == "neo4j"


def test_set_namespace_unknown_name_raises(db):
    with pytest.raises(relate.OperationalError) as exc_info:
        db.set_namespace("nosuch")
    assert exc_info.value.args[0].endswith("Graph not found: nosuch")
    # A failed switch leaves the namespace unchanged.
    assert db.namespace == "neo4j"


def test_set_namespace_inside_transaction_is_refused(db):
    db.autocommit = False
    db.execute("CREATE (:Person {name: 'Alice'})")
    try:
        with pytest.raises(relate.OperationalError) as exc_info:
            db.set_namespace("system")
        assert (
            "Cannot change the namespace while a transaction is open."
            in exc_info.value.args[0]
        )
        assert db.namespace == "neo4j"
    finally:
        db.rollback()
    # After the transaction ends the switch succeeds again.
    db.set_namespace("system")
    assert db.namespace == "system"


def test_namespace_property_on_closed_connection_raises(db):
    db.close()
    with pytest.raises(relate.ProgrammingError):
        _ = db.namespace


# --- DB-API mapping of the fixed-namespace error kinds (ADR-0116/ADR-0117) --


def test_namespace_error_kinds_map_to_dbapi_classes():
    from relate.exceptions import (
        NotSupportedError,
        OperationalError,
        exception_for,
    )

    assert exception_for("DatabaseNotFound", "DatabaseNotFound") is OperationalError
    assert (
        exception_for(
            "WriteOnReadOnlyAccessDatabase", "WriteOnReadOnlyAccessDatabase"
        )
        is OperationalError
    )
    assert (
        exception_for(
            "UnsupportedAdministrationCommand", "UnsupportedAdministrationCommand"
        )
        is NotSupportedError
    )
    assert exception_for("TransactionError", "TransactionError") is OperationalError


def test_statement_in_system_ontologies_is_operational_error(monkeypatch):
    # ADR-0229 under the `ontology` opt-in (ADR-0593): `system_ontologies`
    # is visible but not queryable — every non-administration statement
    # routed there fails with the 42N17 SemanticError text, reads
    # included. The kind maps to OperationalError
    # (ADR-0116/ADR-0117), and administration commands still run from
    # there (ADR-0116 §2). The opt-in is scoped to this test through
    # monkeypatch — a module-level setdefault would leak into every other
    # module's stores, since pytest imports them all up front and the
    # engine re-reads the variable at each open.
    monkeypatch.setenv("RELATE_PREVIEW", "ontology")
    db = relate.connect(":memory:", namespace="system_ontologies")
    try:
        for statement in ["CREATE (:N)", "MATCH (n) RETURN count(n) AS n"]:
            with pytest.raises(relate.OperationalError) as exc_info:
                db.execute(statement)
            assert exc_info.value.args[0].endswith(
                "This database is not queryable. Use the db.ontology.* "
                "procedures on the user database 'neo4j' instead."
            )
        db.execute("SHOW DATABASES")
    finally:
        db.close()


def test_unsupported_administration_command_is_not_supported_error(db):
    with pytest.raises(relate.NotSupportedError):
        db.execute("CREATE DATABASE foo")
