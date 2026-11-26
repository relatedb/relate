"""Per-statement timeout (ADR-0692).

``Connection.statement_timeout`` is a budget in seconds (``None``, the
default, is no bound). A statement still running past it is cancelled and
raises ``OperationalError``; the connection stays usable.
"""

import pytest

import relate

LONG = "UNWIND range(1, 2000000000) AS x RETURN count(x) AS n"


def test_default_is_no_bound(db):
    assert db.statement_timeout is None


def test_a_statement_past_its_budget_raises_operational_error():
    conn = relate.connect(":memory:")
    try:
        conn.statement_timeout = 0.2
        assert conn.statement_timeout == pytest.approx(0.2)
        with pytest.raises(relate.OperationalError) as exc_info:
            conn.cursor().execute(LONG)
        assert "query timed out after 200ms" in str(exc_info.value)
        # The connection stays usable, and None clears the bound.
        conn.statement_timeout = None
        assert conn.statement_timeout is None
        cursor = conn.cursor()
        cursor.execute("RETURN 1 AS n")
        assert cursor.fetchone()["n"] == 1
    finally:
        conn.close()


def test_zero_clears_the_bound(db):
    db.statement_timeout = 5
    db.statement_timeout = 0
    assert db.statement_timeout is None


def test_a_bad_value_is_rejected(db):
    with pytest.raises(TypeError):
        db.statement_timeout = "soon"
    with pytest.raises(ValueError):
        db.statement_timeout = -1
