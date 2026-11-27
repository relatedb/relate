"""Relate - A lightweight, embeddable graph database with Cypher queries.

A DB-API 2.0 (PEP 249) interface over the native (Rust) Relate engine.

Usage::

    import relate

    conn = relate.connect(":memory:")
    conn.execute("CREATE (:Person {name: 'Alice', age: 30})")
    cur = conn.execute("MATCH (p:Person) RETURN p.name AS name, p.age AS age")
    for row in cur:
        print(row["name"], row["age"])
    conn.close()

Canonical DB-API cursor style, and transactions::

    conn.autocommit = False
    cur = conn.cursor()
    cur.execute("CREATE (:Person {name: $name})", {"name": "Bob"})
    conn.commit()          # or conn.rollback() to discard

Nodes and relationships come back as rich objects that expose ids, the
engine's ``elementId()`` strings and topology (:class:`Node`,
:class:`Relationship`, :class:`Path`, :class:`Point`); durations come back as
the lossless :class:`Duration` and vectors as :class:`Vector`, at every depth;
other temporal values come back as ``datetime`` objects when those carry the
value losslessly, and as :class:`Temporal` when they cannot (sub-microsecond
nanoseconds, or an IANA zone name). For Arrow interop (``execute_arrow``)
use the native :class:`Database` directly.

Rows are :class:`Record` dicts with the Neo4j driver's conveniences:
``row.name`` attribute access (read-only), ``row.data()``, column-ordered
``row.keys()``, and on the cursor ``single()``, ``peek()``, ``fetch(n)``,
``data()`` and ``to_df()`` (pandas via the optional ``[pandas]`` extra).
"""

from relate._native import (
    RelateNativeError,
    Database,
    Duration,
    Node,
    Path,
    Point,
    QueryResult,
    Relationship,
    Temporal,
    Vector,
)
from relate.connection import Connection
from relate.cursor import Cursor
from relate.record import Record
from relate.result_graph import ResultGraph
from relate.result_summary import ResultSummary
from relate.exceptions import (
    RelateError,
    DatabaseError,
    DataError,
    Error,
    IntegrityError,
    InterfaceError,
    InternalError,
    NotSupportedError,
    OperationalError,
    ProgrammingError,
    ResultNotSingleError,
    Warning,
)
from relate.types import (
    BINARY,
    DATETIME,
    NUMBER,
    ROWID,
    STRING,
    Binary,
    Date,
    DateFromTicks,
    Time,
    TimeFromTicks,
    Timestamp,
    TimestampFromTicks,
)

# --- DB-API 2.0 module globals (PEP 249) ---------------------------------

#: DB-API level implemented.
apilevel = "2.0"

#: Threads may share the module, but not connections (a ``Database`` is
#: single-threaded / GIL-bound). Level 1 per PEP 249.
threadsafety = 1

#: Parameter marker style. Relate/Cypher uses ``$name`` -- a named style
#: (spelled with ``$`` rather than ``:``). Pass parameters as a dict:
#: ``execute("... $name ...", {"name": value})``.
paramstyle = "named"


def _resolve_version() -> str:
    """Single-source the version from the installed package metadata, falling
    back to a literal for source/editable checkouts without metadata."""
    try:
        from importlib.metadata import PackageNotFoundError, version

        try:
            return version("relate")
        except PackageNotFoundError:
            return "1.4.1"
    except Exception:
        return "1.4.1"


__version__ = _resolve_version()

__all__ = [
    # Module API
    "connect",
    "Connection",
    "Cursor",
    "Database",
    "QueryResult",
    "Record",
    # Rich graph values
    "Node",
    "Relationship",
    "Path",
    "Point",
    "Temporal",
    "Duration",
    "Vector",
    # Exceptions (DB-API tree + alias)
    "RelateError",
    "Error",
    "Warning",
    "InterfaceError",
    "DatabaseError",
    "DataError",
    "OperationalError",
    "IntegrityError",
    "InternalError",
    "ProgrammingError",
    "NotSupportedError",
    "ResultNotSingleError",
    "RelateNativeError",
    # DB-API type objects & constructors
    "STRING",
    "BINARY",
    "NUMBER",
    "DATETIME",
    "ROWID",
    "Date",
    "Time",
    "Timestamp",
    "Binary",
    "DateFromTicks",
    "TimeFromTicks",
    "TimestampFromTicks",
    # DB-API globals
    "apilevel",
    "threadsafety",
    "paramstyle",
    "__version__",
]


def connect(
    database: str = ":memory:", namespace: str = "neo4j", read_only: bool = False
) -> Connection:
    """Open a connection to a Relate database.

    Args:
        database: Path to the database file, or ":memory:" for an in-memory
            database.
        namespace: Session namespace the connection's statements run in when
            they carry no ``USE`` clause: ``"neo4j"`` (default), ``"system"``
            or ``"system_ontologies"``, matched case-insensitively
            (ADR-0117).
        read_only: When true, load once and refuse writes and checkpoints
            (ADR-0136). A missing store is not created. Default false keeps
            the writable open.

    Returns:
        A :class:`Connection`.

    Raises:
        OperationalError: If the database cannot be opened, or ``namespace``
            names anything other than a fixed namespace
            (``Graph not found: <name>``, ADR-0146).
    """
    return Connection(database, namespace=namespace, read_only=read_only)
