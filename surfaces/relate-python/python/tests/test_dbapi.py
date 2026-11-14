"""PEP 249 (DB-API 2.0) conformance tests: globals, cursor(), description type
codes, rowcount, and the mapped exception tree."""

import pytest

import relate


# ============================================================
# Module globals
# ============================================================


def test_apilevel():
    assert relate.apilevel == "2.0"


def test_threadsafety():
    assert relate.threadsafety == 1


def test_paramstyle():
    assert relate.paramstyle == "named"


def test_type_objects_present():
    for name in ("STRING", "BINARY", "NUMBER", "DATETIME", "ROWID"):
        assert hasattr(relate, name)


def test_type_constructors_present():
    for name in (
        "Date",
        "Time",
        "Timestamp",
        "Binary",
        "DateFromTicks",
        "TimeFromTicks",
        "TimestampFromTicks",
    ):
        assert hasattr(relate, name)


# ============================================================
# Exception tree shape (PEP 249)
# ============================================================


def test_exception_tree_inheritance():
    assert issubclass(relate.Warning, Exception)
    assert issubclass(relate.Error, Exception)
    assert issubclass(relate.InterfaceError, relate.Error)
    assert issubclass(relate.DatabaseError, relate.Error)
    for sub in (
        relate.DataError,
        relate.OperationalError,
        relate.IntegrityError,
        relate.InternalError,
        relate.ProgrammingError,
        relate.NotSupportedError,
    ):
        assert issubclass(sub, relate.DatabaseError)


def test_relateerror_is_error_alias():
    assert relate.RelateError is relate.Error


# ============================================================
# Cursor via cursor() (canonical DB-API style)
# ============================================================


def test_cursor_returns_cursor(db):
    cur = db.cursor()
    assert isinstance(cur, relate.Cursor)


def test_cursor_is_fresh_each_call(db):
    assert db.cursor() is not db.cursor()


def test_cursor_execute_and_fetch(db):
    cur = db.cursor()
    ret = cur.execute("RETURN 42 AS x")
    assert ret is cur  # execute returns self for chaining
    assert cur.fetchone()["x"] == 42


def test_cursor_execute_with_params(db):
    cur = db.cursor()
    cur.execute("RETURN $n AS n", {"n": 7})
    assert cur.fetchone()["n"] == 7


def test_closed_connection_autocommit_assignment_raises(db):
    # Enabling autocommit commits first, and commit() refuses a closed
    # connection: the closed-connection edge of the autocommit setter.
    db.close()
    with pytest.raises(relate.ProgrammingError):
        db.autocommit = True


def test_unbound_cursor_execute_raises():
    cur = relate.Cursor()
    with pytest.raises(relate.ProgrammingError):
        cur.execute("RETURN 1")


def test_cursor_executemany(db):
    cur = db.cursor()
    cur.executemany("CREATE (:P {v: $v})", [{"v": 1}, {"v": 2}])
    cur.execute("MATCH (p:P) RETURN count(*) AS c")
    assert cur.fetchone()["c"] == 2


# ============================================================
# rowcount
# ============================================================


def test_rowcount_read(db):
    db.execute("CREATE (:N), (:N), (:N)")
    cur = db.execute("MATCH (n:N) RETURN n")
    assert cur.rowcount == 3


def test_rowcount_write_is_minus_one(db):
    cur = db.execute("CREATE (:N)")
    assert cur.rowcount == -1


# ============================================================
# description type codes
# ============================================================


def test_description_number_type_code(db):
    cur = db.execute("RETURN 1 AS x")
    assert cur.description[0][1] == relate.NUMBER


def test_description_string_type_code(db):
    cur = db.execute("RETURN 'hi' AS s")
    assert cur.description[0][1] == relate.STRING


def test_description_none_for_empty_result(db):
    # Columns present, but no rows -> the engine stamps ANY (ADR-0224),
    # which has no DB-API counterpart -> type_code is None.
    cur = db.execute("MATCH (n:DoesNotExist) RETURN n")
    assert cur.description == [("n", None, None, None, None, None, None)]


def test_description_none_for_write(db):
    cur = db.execute("CREATE (:N)")
    assert cur.description is None


def test_description_engine_sourced_types(db):
    # type_code comes from the engine's per-column stamp (ADR-0224), not
    # client-side inference: INTEGER -> NUMBER, STRING -> STRING, BOOLEAN
    # folds into NUMBER, DATE/TIME fold into DATETIME.
    cur = db.execute(
        "RETURN 1 AS i, 'hi' AS s, true AS b, date('2026-01-01') AS d"
    )
    codes = [d[1] for d in cur.description]
    assert codes == [relate.NUMBER, relate.STRING, relate.NUMBER, relate.DATETIME]


def test_description_node_type_code_is_none(db):
    # A node column stamps NODE (ADR-0224); graph entities have no DB-API
    # type-comparison code, so description reports None for them.
    db.execute("CREATE (:N)")
    cur = db.execute("MATCH (n:N) RETURN n")
    assert cur.description[0][1] is None


# ============================================================
# All 11 mutation stats
# ============================================================


def test_all_eleven_stats_present(db):
    cur = db.execute("CREATE (:T {a: 1})")
    assert set(cur.stats.keys()) == {
        "nodes_created",
        "nodes_deleted",
        "relationships_created",
        "relationships_deleted",
        "properties_set",
        "labels_added",
        "labels_removed",
        "indexes_added",
        "indexes_removed",
        "constraints_added",
        "constraints_removed",
    }


def test_stats_zeros_included(db):
    cur = db.execute("CREATE (:T)")
    # A pure create reports 0 deletes but the key is still present.
    assert cur.stats["nodes_deleted"] == 0
    assert cur.stats["nodes_created"] == 1


# ============================================================
# Ordered result rows
# ============================================================


def test_result_dict_preserves_column_order(db):
    cur = db.execute("RETURN 3 AS c, 1 AS a, 2 AS b")
    row = cur.fetchone()
    assert list(row.keys()) == ["c", "a", "b"]


# ============================================================
# Exception mapping (from native kind/tck_class metadata)
# ============================================================


def test_syntax_error_is_programming_error(db):
    with pytest.raises(relate.ProgrammingError):
        db.execute("INVALID SYNTAX HERE")


def test_missing_parameter_is_programming_error(db):
    """An omitted parameter fails the statement (ADR-0308); None is null."""
    with pytest.raises(relate.ProgrammingError) as excinfo:
        db.execute("RETURN $missing AS value")
    assert "ParameterMissing: Expected parameter(s): missing" in str(excinfo.value)
    assert db.execute("RETURN $p AS value", {"p": None}).fetchone()["value"] is None


def test_type_error_is_data_error(db):
    # Indexing a map with a non-string key is a runtime TypeError-class error.
    with pytest.raises(relate.DataError):
        db.execute("RETURN {a: 1}[0]")


def test_constraint_violation_is_integrity_error(db):
    db.execute("CREATE CONSTRAINT FOR (p:Person) REQUIRE p.email IS UNIQUE")
    db.execute("CREATE (:Person {email: 'a@b.com'})")
    with pytest.raises(relate.IntegrityError):
        db.execute("CREATE (:Person {email: 'a@b.com'})")


def test_unknown_param_type_is_programming_error(db):
    with pytest.raises(relate.ProgrammingError):
        db.execute("RETURN $x", {"x": object()})


def test_int_overflow_param_is_data_error(db):
    with pytest.raises(relate.DataError):
        db.execute("RETURN $x", {"x": 2**70})


def test_error_is_also_catchable_as_base(db):
    # Every mapped error is still a relate.Error (== RelateError).
    with pytest.raises(relate.Error):
        db.execute("INVALID SYNTAX HERE")


class TestConnectReadOnly:
    def test_read_only_connection_rejects_writes(self, tmp_path):
        db = relate.connect(str(tmp_path / "ro.rl"))
        db.execute("CREATE (n:Thing)")
        db.close()
        ro = relate.connect(str(tmp_path / "ro.rl"), read_only=True)
        with pytest.raises(relate.OperationalError):
            ro.execute("CREATE (n:Thing)")
        ro.close()
