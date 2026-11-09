#!/usr/bin/env python3
"""Official-driver compat assertions for the LABS-323 Bolt fixes and the
Bolt 5.4 pins (ADR-0484).

Imported and run by ``python_driver_smoke.py`` against the same plaintext
server the ci.yml smoke leg binds — the ``driver`` parameter on ``run``
hands the caller's driver to the shared blocks, and the blocks that
construct their own drivers (transaction_smoke, routing_smoke, the
``__main__`` entry) resolve their targets from ``_topology``
(``bolt_uri`` / ``routing_uri``). Every block names the
LABS-323 finding ids it pins — ``bolt-compat-findings.json`` ``F-NN``
(wire probes) and ``neo4rs-compat-findings.json`` ``D-NN`` (driver
probes) — or the ADR-0484 decision it pins. Findings only a raw socket
can observe (handshake bytes, HELLO metadata, chunk framing, protocol
breaches) stay in the ``.bolt`` scripts under
``tests/compat/scripts/``.
"""
from datetime import date, datetime, time, timedelta, timezone

import pytz
from _topology import bolt_uri, routing_uri
from neo4j import GraphDatabase
from neo4j.exceptions import ClientError, CypherSyntaxError
from neo4j.spatial import CartesianPoint, WGS84Point
from neo4j.time import Date, DateTime, Duration, Time

# The plaintext smoke server every driver construction below dials — the
# same `--bolt-identity` server ``python_driver_smoke.py`` connects to
# (the current python driver rejects the honest Relate/ agent, so
# every official-driver connection runs behind the shim). Every
# construction resolves its target from the topology's one source
# (``_topology.bolt_uri`` / ``_topology.routing_uri``) — no resolved
# address crosses this module's API, and no module state carries it.
# One constant for the throwaway credentials.
AUTH = ("neo4j", "unused")

# A `datetime.time` needs a fixed-offset tzinfo whose `utcoffset` accepts a
# time; the stdlib `timezone` raises TypeError there, so use pytz (a neo4j
# dependency). The documented example (docs/api/bolt/examples/
# temporal_params.py) carries its own copy of this helper: examples must
# stay self-contained so a user can copy them whole.
def assert_param_equals(driver, value, constructor, label):
    """Assert `$p = <constructor>` answers true for this Python value."""
    records, _, _ = driver.execute_query(
        f"RETURN $p = {constructor} AS ok", p=value
    )
    assert records[0]["ok"] is True, f"{label}: $p != {constructor}"


def values_smoke(driver):
    """Temporal, point and bytes values in both directions (values fixes)."""

    # Temporal results hydrate as the driver's native neo4j.time types
    # (F-51..F-56, F-58 / D-09..D-15): the server answers PackStream
    # temporal structures, never strings.
    records, _, _ = driver.execute_query(
        "RETURN date('2024-01-02') AS d, "
        "time('03:04:05.123456789+01:00') AS t, "
        "localtime('03:04:05.123456789') AS lt, "
        "datetime('2024-01-02T03:04:05.123456789+01:00') AS dt, "
        "datetime('2024-01-02T03:04:05[Europe/Stockholm]') AS z, "
        "localdatetime('2024-01-02T03:04:05.123456789') AS ldt, "
        "duration('P1Y2M3DT4H5M6.5S') AS dur"
    )
    row = records[0]
    assert row["d"] == Date(2024, 1, 2), row["d"]  # F-51 / D-09
    assert row["t"] == Time(3, 4, 5, 123456789, tzinfo=pytz.FixedOffset(60)), (
        row["t"]
    )  # F-52 / D-10
    assert row["lt"] == Time(3, 4, 5, 123456789), row["lt"]  # F-53 / D-11
    assert (
        row["dt"].iso_format() == "2024-01-02T03:04:05.123456789+01:00"
    ), row["dt"]  # F-54 / D-12
    assert isinstance(row["z"], DateTime), row["z"]  # F-55 / D-13
    assert row["z"].tzinfo.utcoffset(row["z"].to_native()) == timedelta(hours=1)
    assert str(row["z"].tzinfo) == "Europe/Stockholm", row["z"].tzinfo
    assert row["ldt"] == DateTime(2024, 1, 2, 3, 4, 5, 123456789), (
        row["ldt"]
    )  # F-56 / D-14
    assert row["dur"] == Duration(
        months=14, days=3, seconds=14706, nanoseconds=500000000
    ), row["dur"]  # F-58 / D-15

    # Point results hydrate as the driver's spatial types (F-59/F-60 /
    # D-16/D-17), float coordinates included.
    records, _, _ = driver.execute_query(
        "RETURN point({srid: 7203, x: 1.5, y: 2.5}) AS c2, "
        "point({srid: 9157, x: 1.0, y: 2.0, z: 3.0}) AS c3, "
        "point({longitude: 13, latitude: 55.6}) AS wgs"
    )
    row = records[0]
    assert row["c2"] == CartesianPoint((1.5, 2.5)), row["c2"]  # F-59 / D-16
    assert row["c3"] == CartesianPoint((1.0, 2.0, 3.0)), row["c3"]  # F-60 / D-17
    assert row["wgs"] == WGS84Point((13.0, 55.6)), row["wgs"]  # F-59 / D-16

    # Temporal, point and bytes parameters round-trip (D-26..D-32 /
    # D-33/D-34, F-64 / D-25). The Python driver proposes the Bolt 4.4 utc
    # patch in HELLO and only encodes datetimes as the UTC structures
    # 0x49/0x69 once the server echoes patch_bolt in HELLO SUCCESS
    # (F-10/F-57, ADR-0329) — and the server accepts the legacy structures
    # only without the patch — so the datetime round-trips below pin the
    # patch handshake as the driver sees it.
    offset = pytz.FixedOffset(60)
    assert_param_equals(
        driver, date(1970, 1, 1), "date('1970-01-01')", "D-26 date parameter"
    )
    assert_param_equals(
        driver,
        time(12, 30, tzinfo=offset),
        "time('12:30:00+01:00')",
        "D-27 time parameter",
    )
    assert_param_equals(
        driver, time(12, 30), "localtime('12:30:00')", "D-28 localtime parameter"
    )
    assert_param_equals(
        driver,
        datetime(1970, 1, 1, 2, 15, 0, 42, tzinfo=timezone(timedelta(hours=1))),
        "datetime('1970-01-01T02:15:00.000042+01:00')",
        "D-29 / F-10 / F-57 offset datetime parameter (utc patch)",
    )
    assert_param_equals(
        driver,
        pytz.timezone("Europe/Stockholm").localize(datetime(2021, 6, 1, 12, 0)),
        "datetime('2021-06-01T12:00:00+02:00[Europe/Stockholm]')",
        "D-30 / F-10 / F-57 zone-id datetime parameter (utc patch)",
    )
    assert_param_equals(
        driver,
        datetime(1970, 1, 2, 0, 0, 0, 42),
        "localdatetime('1970-01-02T00:00:00.000042')",
        "D-31 localdatetime parameter",
    )
    assert_param_equals(
        driver,
        Duration(months=1, days=2, seconds=3, nanoseconds=4),
        "duration('P1M2DT3.000000004S')",
        "D-32 duration parameter",
    )
    assert_param_equals(
        driver,
        CartesianPoint((1.0, 2.0)),
        "point({srid: 7203, x: 1.0, y: 2.0})",
        "D-33 2D point parameter",
    )
    assert_param_equals(
        driver,
        CartesianPoint((1.0, 2.0, 3.0)),
        "point({srid: 9157, x: 1.0, y: 2.0, z: 3.0})",
        "D-34 3D point parameter",
    )
    records, _, _ = driver.execute_query(
        "RETURN $b AS b", b=bytearray(b"\x00\x01\xff")
    )
    assert records[0]["b"] == b"\x00\x01\xff", records[0]["b"]  # F-64 / D-25
    print(
        "compat values smoke: temporal, point and bytes values hydrate and "
        "round-trip"
    )


def summary_smoke(driver):
    """Summary metadata: query type, stats, t_first/t_last, the ADR-0491
    phase split, db, bookmarks."""

    # Auto-commit read (F-13 / D-55): the SUCCESS ending the result carries
    # type "r", t_last and db; RUN SUCCESS carries t_first (F-11). The
    # driver surfaces them as summary.query_type, result_consumed_after,
    # database and result_available_after.
    records, summary, _ = driver.execute_query("MATCH (n) RETURN count(n) AS c")
    assert summary.query_type == "r", summary.query_type
    assert summary.result_available_after is not None, summary  # F-11
    assert summary.result_consumed_after is not None, summary
    assert summary.database == "neo4j", summary.database

    # ADR-0491: the ending SUCCESS carries the statement's PARSE/PLAN/
    # EXECUTE split beside t_last, under the ADR-0491 contract keys —
    # parsing_time, planning_time, plan_execution_time as Float seconds.
    # The Python driver surfaces unknown SUCCESS keys raw through
    # summary.metadata, so the split round-trips through the official
    # driver without any driver change.
    metadata = summary.metadata
    for key in ("parsing_time", "planning_time", "plan_execution_time"):
        value = metadata[key]
        assert isinstance(value, float) and value >= 0.0, (key, value)

    # Auto-commit write (F-13/F-40 / D-52): type "w" plus the kebab-case
    # stats on the ending SUCCESS, surfaced as summary.counters.
    _, summary, _ = driver.execute_query(
        "CREATE (a:CompatSmokeA {v: 1})-[r:COMPAT_SMOKE]->(b:CompatSmokeB {v: 2})"
    )
    assert summary.query_type == "w", summary.query_type
    counters = summary.counters
    assert counters.nodes_created == 2, counters
    assert counters.relationships_created == 1, counters
    assert counters.properties_set == 2, counters
    assert counters.labels_added == 2, counters
    assert counters.contains_updates is True, counters

    # The w vs rw split runs on returned columns (D-53/D-54, ADR-0332): a
    # write with RETURN is "rw"; MATCH...SET and DETACH DELETE without
    # RETURN are "w".
    _, summary, _ = driver.execute_query(
        "CREATE (n:CompatSmokeC {v: 1}) RETURN n.v AS v"
    )
    assert summary.query_type == "rw", summary.query_type
    assert summary.counters.nodes_created == 1, summary.counters
    _, summary, _ = driver.execute_query("MATCH (n:CompatSmokeC) SET n.v = 2")
    assert summary.query_type == "w", summary.query_type  # D-53
    assert summary.counters.properties_set == 1, summary.counters
    _, summary, _ = driver.execute_query("MATCH (n:CompatSmokeC) DETACH DELETE n")
    assert summary.query_type == "w", summary.query_type  # D-54
    assert summary.counters.nodes_deleted == 1, summary.counters

    # Schema DDL is "s" with indexes-added stats (D-56).
    _, summary, _ = driver.execute_query(
        "CREATE INDEX compat_smoke_v FOR (n:CompatSmokeA) ON (n.v)"
    )
    assert summary.query_type == "s", summary.query_type
    assert summary.counters.indexes_added == 1, summary.counters

    # The DISCARD SUCCESS that ends a result carries the same keys as the
    # final PULL SUCCESS (F-19): consume() without streaming sends DISCARD
    # n=-1, and its bookmark reaches the session (F-14).
    with driver.session() as session:
        result = session.run("CREATE (n:CompatSmokeD {v: 1})")
        summary = result.consume()
        assert summary.query_type == "w", summary.query_type
        assert summary.counters.nodes_created == 1, summary.counters
        assert summary.result_consumed_after is not None, summary
        assert summary.database == "neo4j", summary.database
        assert session.last_bookmarks(), session.last_bookmarks()  # F-14

    # The auto-commit ending SUCCESS of a write carries the engine's commit
    # bookmark (F-14, ADR-0502 §7); a read commits nothing and carries none,
    # so a fresh session's bookmarks stay empty.
    with driver.session() as session:
        session.run("RETURN 1 AS x").consume()
        assert not session.last_bookmarks(), session.last_bookmarks()
    with driver.session() as session:
        session.run("CREATE (n:CompatSmokeE)").consume()
        assert session.last_bookmarks(), session.last_bookmarks()  # F-14

    # Inside an explicit transaction the ending PULL SUCCESS carries stats
    # (F-20); no PULL SUCCESS inside the transaction carries a bookmark —
    # COMMIT SUCCESS does (F-21/D-57).
    with driver.session() as session:
        with session.begin_transaction() as tx:
            summary = tx.run("CREATE (n:CompatSmokeF {v: 1})").consume()
            assert summary.query_type == "w", summary.query_type
            assert summary.counters.nodes_created == 1, summary.counters
            assert summary.counters.labels_added == 1, summary.counters
            assert summary.result_consumed_after is not None, summary
            tx.commit()
        assert session.last_bookmarks(), session.last_bookmarks()  # F-21 / D-57
    print(
        "compat summary smoke: query type, stats, t_first/t_last, the "
        "ADR-0491 phase split, db and bookmarks"
    )


def transaction_smoke():
    """Two open results in one explicit transaction (F-23/F-24/D-41).

    Builds its own ``fetch_size=2`` driver so the interleaved reads below
    are real qid-addressed pages.
    """

    # A second RUN inside an explicit transaction opens its own result; the
    # earlier one stays open and addressable by qid, so each streams only
    # its own rows (F-23/F-24/D-41).
    paged = GraphDatabase.driver(bolt_uri("RELATE_BOLT_PORT_IDENTITY"),
                                auth=AUTH, fetch_size=2)
    try:
        with paged.session() as session:
            with session.begin_transaction() as tx:
                left = tx.run("UNWIND range(1, 5) AS a RETURN a")
                right = tx.run("UNWIND range(10, 50, 10) AS b RETURN b")
                left_iter, right_iter = iter(left), iter(right)
                assert next(left_iter)["a"] == 1
                assert next(right_iter)["b"] == 10
                assert next(left_iter)["a"] == 2
                assert next(right_iter)["b"] == 20
                assert [record["a"] for record in left] == [3, 4, 5]
                assert [record["b"] for record in right] == [30, 40, 50]
                tx.commit()
    finally:
        paged.close()
    print("compat transaction smoke: two open results stream their own rows")


def failure_smoke(driver):
    """Failure codes and their driver exception classes (failures fixes)."""

    # A syntax error raises the driver's syntax-error client exception
    # with Neo4j's code (F-41 / D-47).
    try:
        driver.execute_query("RETURN 1 +")
        raise AssertionError("a syntax error must fail")
    except CypherSyntaxError as error:
        assert error.code == "Neo.ClientError.Statement.SyntaxError", error

    # A statement that references an absent parameter fails before
    # execution with ParameterMissing (F-42/F-67/D-48) — no null record.
    try:
        driver.execute_query("RETURN $p AS p")
        raise AssertionError("a missing parameter must fail")
    except ClientError as error:
        assert error.code == "Neo.ClientError.Statement.ParameterMissing", error
        assert "Expected parameter(s): p" in str(error), error

    # An unknown database fails with Neo4j's code (D-46).
    try:
        with driver.session(database="nosuch") as session:
            session.run("RETURN 1 AS n").consume()
        raise AssertionError("an unknown database must fail")
    except ClientError as error:
        assert error.code == "Neo.ClientError.Database.DatabaseNotFound", error

    # A runtime error raised while executing an already-planned statement
    # surfaces while iterating, not at run (D-49, ADR-0319): run() returns
    # the planned columns and the first PULL fails with the arithmetic code.
    with driver.session() as session:
        result = session.run("UNWIND [1, 0] AS x RETURN 10 / x AS v")
        assert result.keys() == ["v"], result.keys()
        try:
            list(result)
            raise AssertionError("10 / 0 must fail while iterating")
        except ClientError as error:
            assert error.code == "Neo.ClientError.Statement.ArithmeticError", error

    # After a failed query the connection is reusable (D-50): the failed
    # state clears and the same driver answers the next query.
    records, _, _ = driver.execute_query("RETURN 42 AS n")
    assert records[0]["n"] == 42, records

    # A syntax error inside an explicit transaction fails that transaction;
    # rollback still answers and the transaction's writes are discarded
    # (D-51).
    with driver.session() as session:
        with session.begin_transaction() as tx:
            tx.run("CREATE (n:CompatSmokeRolled)")
            try:
                tx.run("RETURN 1 +").consume()
                raise AssertionError("a syntax error in a tx must fail")
            except CypherSyntaxError as error:
                assert error.code == "Neo.ClientError.Statement.SyntaxError", error
            tx.rollback()
        records, _, _ = driver.execute_query(
            "MATCH (n:CompatSmokeRolled) RETURN count(n) AS c"
        )
        assert records[0]["c"] == 0, records
    print(
        "compat failure smoke: Neo4j failure codes and driver exception "
        "classes"
    )


def routing_smoke():
    """ROUTE answers the home database (F-36)."""

    # A neo4j:// (routing) URI makes the driver send ROUTE on connect and
    # adopt rt.db as its home database (F-36, ADR-0330): the resolved home
    # database is neo4j, so an unqualified session runs data queries there.
    routing = GraphDatabase.driver(routing_uri("RELATE_BOLT_PORT_IDENTITY"),
                                  auth=AUTH)
    try:
        records, summary, _ = routing.execute_query("RETURN 1 AS n")
        assert records[0]["n"] == 1, records
        assert summary.database == "neo4j", summary.database
    finally:
        routing.close()
    print("compat routing smoke: neo4j:// ROUTE resolves the home database")


def bolt_5_4_smoke(driver):
    """The official driver connected at Bolt 5.4 (ADR-0484).

    The driver's handshake — manifest marker, the 5.8 range, 4.4, 3.0, in
    its get_handshake() order — is answered 5.4 by first-cover-wins, so
    every construction below runs the 5.4 message set end to end: HELLO
    without auth fields, LOGON with the basic token, RUN and PULL
    (decisions 2-4).
    """

    # Basic-auth LOGON (decision 4): the negotiated version is 5.4 and the
    # driver's basic LOGON opens the session — the query answers through
    # the AUTHENTICATION-gated connection.
    server_info = driver.get_server_info()
    assert server_info.protocol_version == (5, 4), server_info.protocol_version
    records, _, _ = driver.execute_query("RETURN 1 AS n")
    assert records[0]["n"] == 1, records

    # Node element ids (decision 6): at 5.4 the Node structure carries the
    # element_id field, the driver hydrates it, and it is the same string
    # elementId() answers — the engine's 4:<segment>:n<id> form.
    driver.execute_query("MERGE (n:Bolt54Smoke {k: 1})")
    records, _, _ = driver.execute_query(
        "MATCH (n:Bolt54Smoke) RETURN n, elementId(n) AS eid"
    )
    node = records[0]["n"]
    assert node.element_id == records[0]["eid"], node.element_id
    assert node.element_id.startswith("4:relate-neo4j:n"), node.element_id

    # A telemetry-disabled connection (decision 5): the driver opts out and
    # the server's HELLO hint says telemetry.enabled false, so no TELEMETRY
    # is sent — the connection negotiates 5.4 and serves queries.
    opted_out = GraphDatabase.driver(bolt_uri("RELATE_BOLT_PORT_IDENTITY"),
                                    auth=AUTH, telemetry_disabled=True)
    try:
        opted_out.verify_connectivity()
        assert (
            opted_out.get_server_info().protocol_version == (5, 4)
        ), opted_out.get_server_info().protocol_version
        records, _, _ = opted_out.execute_query("RETURN 2 AS n")
        assert records[0]["n"] == 2, records
    finally:
        opted_out.close()
    print("compat bolt 5.4 smoke: LOGON flow, element ids, telemetry opt-out")


def run(driver):
    """Every compat block, in order."""
    values_smoke(driver)
    summary_smoke(driver)
    transaction_smoke()
    failure_smoke(driver)
    routing_smoke()
    bolt_5_4_smoke(driver)


if __name__ == "__main__":
    smoke_driver = GraphDatabase.driver(
        bolt_uri("RELATE_BOLT_PORT_IDENTITY"), auth=AUTH)
    try:
        run(smoke_driver)
    finally:
        smoke_driver.close()
    print("ok")
