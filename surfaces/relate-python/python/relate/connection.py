"""DB-API 2.0 Connection for Relate."""

from typing import Any, Callable, Dict, List, Optional, Tuple

from relate._native import RelateNativeError, Database
from relate.cursor import Cursor
from relate.result_summary import ResultSummary
from relate.exceptions import (
    NATIVE_ERROR_ATTRS,
    DataError,
    Error,
    OperationalError,
    ProgrammingError,
    exception_for,
)

# All 11 mutation counters, always present on ``Cursor.stats``. The native
# module already emits all 11 (zeros included); this is a defensive pad.
_STAT_KEYS = (
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
)


# The native error-fact vocabulary lives in ``exceptions`` (one source:
# ``NATIVE_ERROR_ATTRS`` also derives the ``Error`` class defaults there).
# Here it is only consulted: the attributes the native module attaches to
# every ``RelateNativeError`` and the mapped DB-API exception carries
# over unchanged — the binding vocabulary (``kind``/``tck_class``) and the
# engine's ``ErrorInfo`` facts (ADR-0502 §2), the latter never re-derived
# here.


def _map_native_error(exc: RelateNativeError) -> Exception:
    """Map a native engine error onto the DB-API exception tree, using the
    ``.kind`` / ``.tck_class`` attributes the native module attaches, and
    carry every native attribute over to the mapped exception. The native
    module attaches all six to every error it raises; an instance raised
    by hand (a test double) carries none, and maps by the ``None`` pair
    to :class:`DatabaseError`, as before."""
    attrs = {name: getattr(exc, name, None) for name in NATIVE_ERROR_ATTRS}
    mapped = exception_for(attrs["kind"], attrs["tck_class"])(str(exc))
    for name, value in attrs.items():
        setattr(mapped, name, value)
    return mapped


def _map_call_error(exc: Exception) -> Exception:
    """The single mapping policy for a native call boundary: engine errors
    (``RelateNativeError``) map by their ``.kind`` / ``.tck_class``;
    pyo3 also surfaces native failures as plain ``RuntimeError``, which is
    operational."""
    if isinstance(exc, RelateNativeError):
        return _map_native_error(exc)
    return OperationalError(str(exc))


def _native(op: Callable[[], Any]) -> Any:
    """Run one native call; a native failure raises mapped onto the DB-API
    tree (see :func:`_map_call_error`). Returns the call's result."""
    try:
        return op()
    except (RelateNativeError, RuntimeError) as e:
        raise _map_call_error(e) from None


def _capture_native_error(op: Callable[[], Any]) -> Optional[Exception]:
    """Run one native call through the canonical :func:`_native` boundary,
    returning the mapped DB-API exception instead of raising it — for
    ``close()``, which runs a second step regardless. An error a nested
    boundary already mapped (the transaction probe inside
    ``_rollback_open_transaction``) passes ``_native`` through unchanged
    and is captured as-is — so a probe failure never skips the native
    close."""
    try:
        _native(op)
    except Error as e:
        return e
    return None


class Connection:
    """A connection to a Relate database (PEP 249).

    Wraps the native (Rust) Relate engine running in-process. File-backed
    databases persist their data on ``commit()`` / ``close()`` (or ``flush()``).

    Two usage styles are supported:

    * sqlite3-style convenience -- ``connection.execute(query)`` runs the query
      and returns a result-bearing :class:`Cursor`.
    * canonical DB-API -- ``cur = connection.cursor(); cur.execute(query)``.

    Transactions
    ------------
    Connections default to **autocommit** (each statement is durable on its
    own). Set ``connection.autocommit = False`` to group statements into a
    transaction: the first statement opens it, and ``commit()`` / ``rollback()``
    end it (decision 3, backed by the engine's ``begin`` / ``commit`` /
    ``rollback``). Closing a connection with an open transaction rolls it back.
    """

    def __init__(
        self, database: str = ":memory:", namespace: str = "neo4j", read_only: bool = False
    ):
        self._database = database
        self._closed = False
        self._autocommit = True
        # Engine errors carry .kind/.tck_class (e.g. an unknown namespace
        # is DatabaseNotFound -> OperationalError). Whether a transaction is
        # open is the engine's own fact (``transaction_active()``) — the
        # binding keeps no mirror of it.
        self._db = _native(lambda: Database(database, namespace, read_only=read_only))

    # --- DB-API required attributes / methods ----------------------------

    @property
    def autocommit(self) -> bool:
        """Whether each statement commits on its own (default ``True``)."""
        return self._autocommit

    @autocommit.setter
    def autocommit(self, value: bool) -> None:
        value = bool(value)
        if value:
            # Switching to autocommit durably ends any open transaction —
            # commit() itself owns the "is one open" decision.
            self.commit()
        self._autocommit = value

    def cursor(self) -> Cursor:
        """Return a new DB-API cursor bound to this connection."""
        self._check_closed()
        return Cursor(connection=self)

    # --- Session namespace (ADR-0117) ------------------------------------

    @property
    def namespace(self) -> str:
        """The session namespace this connection's statements run in when
        they carry no ``USE`` clause: ``"neo4j"``, ``"system"`` or
        ``"system_ontologies"`` (the latter visible only under
        ``RELATE_PREVIEW=ontology``, ADR-0593)."""
        self._check_closed()
        return self._db.namespace()

    def set_namespace(self, name: str) -> None:
        """Change the session namespace. Names match case-insensitively.

        Raises:
            OperationalError: If ``name`` is not a fixed namespace
                (``Graph not found: <name>``, ADR-0146), or a
                transaction is open (``Cannot change the namespace while a
                transaction is open.``) -- namespaces change only between
                transactions.
        """
        self._check_closed()
        _native(lambda: self._db.set_namespace(name))

    def commit(self) -> Optional[str]:
        """Commit the open transaction (a no-op in autocommit mode).

        Returns the transaction's bookmark (ADR-0502 §7): the engine's commit
        sequence spelled ``relate:<n>``, monotonic per store -- or
        ``None`` when nothing was committed (autocommit mode, no open
        transaction, or a transaction that wrote nothing). An auto-commit
        write's bookmark is on its result instead
        (``cursor.consume().bookmark``).
        """
        self._check_closed()
        if not self._transaction_open():
            return None
        # A refused commit ("rollback required") leaves the engine's
        # transaction open, so rollback() still reaches it; one that
        # consumed the transaction before failing leaves none open — the
        # engine owns that fact, so there is nothing to resync here.
        return _native(self._db.commit_transaction)

    def rollback(self) -> None:
        """Roll back the open transaction, discarding its changes (a no-op in
        autocommit mode)."""
        self._check_closed()
        _native(self._rollback_open_transaction)

    # --- Query execution -------------------------------------------------

    def _raw_execute(
        self, query: str, params: Optional[Dict[str, Any]] = None
    ) -> Tuple[List[str], List[str], List[Dict[str, Any]], ResultSummary]:
        """Run a query, opening a transaction first if needed, and return
        ``(columns, column_types, rows, summary)`` — the column types are the
        engine-reported per-column names (ADR-0224) and feed
        ``Cursor.description``; the summary feeds
        ``Cursor.consume()`` (ADR-0215; its ``elapsed_ms`` is the
        engine-reported value, ADR-0221, and ``parse_ms``/``plan_ms``/
        ``execute_ms`` its phase split, ADR-0491). Native errors are mapped
        to the DB-API exception tree. Internal; used by :class:`Cursor`."""
        self._check_closed()
        # Whether a transaction is open is the engine's own fact (read
        # through :meth:`_transaction_open`, the one accessor) — no
        # Python-side mirror to keep in step — and the begin itself goes
        # through the same error-mapping boundary as every other native
        # call.
        if not self._autocommit and not self._transaction_open():
            _native(lambda: self._db.begin_transaction())
        try:
            result = _native(lambda: self._db.execute(query, params))
        except OverflowError as e:
            # A parameter integer out of 64-bit range.
            raise DataError(str(e)) from None
        except TypeError as e:
            # An unconvertible parameter type.
            raise ProgrammingError(str(e)) from None
        # ADR-0221: the engine stamps every result with the statement's
        # elapsed (parse, plan and run); ADR-0491 adds the PARSE / PLAN /
        # EXECUTE split of that total. The native module is co-packaged
        # in the same wheel, so the stamps are always present.
        elapsed_ms = result.elapsed_ms
        parse_ms = result.parse_ms
        plan_ms = result.plan_ms
        execute_ms = result.execute_ms

        stats = dict.fromkeys(_STAT_KEYS, 0)
        stats.update(result.stats)
        summary = ResultSummary(
            counters=stats,
            namespace=result.namespace,
            query_type=result.query_type,
            elapsed_ms=elapsed_ms,
            parse_ms=parse_ms,
            plan_ms=plan_ms,
            execute_ms=execute_ms,
            notifications=result.notifications,
            plan=result.plan,
            bookmark=result.bookmark,
        )
        return (
            list(result.columns),
            list(result.column_types),
            list(result.data),
            summary,
        )

    def execute(
        self, query: str, params: Optional[Dict[str, Any]] = None
    ) -> Cursor:
        """Execute a Cypher query and return a result-bearing cursor
        (sqlite3-style convenience)."""
        cursor = self.cursor()
        cursor.execute(query, params)
        return cursor

    def executemany(
        self, query: str, params_list: List[Dict[str, Any]]
    ) -> Cursor:
        """Execute ``query`` once for each parameter set; return the cursor
        reflecting the final execution."""
        cursor = self.cursor()
        cursor.executemany(query, params_list)
        return cursor

    # --- Lifecycle -------------------------------------------------------

    def flush(self) -> None:
        """Flush pending changes to disk (no-op for in-memory databases)."""
        self._check_closed()
        _native(self._db.flush)

    def _transaction_open(self) -> bool:
        """Whether the engine has a transaction open on this connection —
        its own fact, read through the same error-mapping boundary as
        every other native call (the binding keeps no mirror of it)."""
        return _native(self._db.transaction_active)

    def _rollback_open_transaction(self) -> None:
        """Roll back the engine's open transaction, if it has one. This
        helper is the single owner of that protocol — :meth:`rollback` and
        :meth:`close` both delegate, and each maps the helper's failures
        (the probe's already-mapped error passes through, the rollback's
        native error maps) onto the one DB-API vocabulary at its own
        boundary; whether a transaction is open is always the engine's
        own answer, via :meth:`_transaction_open`."""
        if self._transaction_open():
            self._db.rollback_transaction()

    def close(self) -> None:
        """Close the connection. Any open transaction is rolled back first
        (per the DB-API), then file-backed databases are flushed to disk.
        The native close is never skipped — even when the rollback failed —
        and the Python side only reports done after it ran."""
        if not self._closed:
            # Both steps run unconditionally — `or` would short-circuit the
            # native close whenever the rollback failed. First error wins;
            # the state settles in the finally before it re-raises.
            error = _capture_native_error(self._rollback_open_transaction)
            try:
                close_error = _capture_native_error(self._db.close)
            finally:
                self._closed = True
            if error is None:
                error = close_error
            if error is not None:
                raise error from None

    @property
    def closed(self) -> bool:
        """Whether this connection is closed."""
        return self._closed

    def __enter__(self) -> "Connection":
        return self

    def __exit__(self, exc_type, exc_val, exc_tb) -> None:
        self.close()

    def __del__(self) -> None:
        try:
            self.close()
        except Exception:
            pass

    def _check_closed(self) -> None:
        if self._closed:
            raise ProgrammingError("Cannot operate on a closed connection")
