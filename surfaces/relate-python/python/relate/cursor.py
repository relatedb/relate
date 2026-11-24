"""DB-API 2.0 Cursor for Relate."""

from typing import TYPE_CHECKING, Any, Dict, Iterator, List, Optional

from relate.exceptions import ProgrammingError, ResultNotSingleError
from relate.record import Record
from relate.result_graph import ResultGraph, collect_graph_entities
from relate.result_summary import ResultSummary
from relate.types import engine_type_code

if TYPE_CHECKING:
    from relate.connection import Connection


class Cursor:
    """A database cursor (PEP 249).

    A cursor can either be bound to a :class:`~relate.connection.Connection`
    (created via ``connection.cursor()``, then driven with ``cursor.execute()``)
    or pre-filled with a result set (constructed directly with ``columns`` /
    ``rows``). Both support ``fetchone`` / ``fetchmany``
    / ``fetchall`` and iteration, which advance a shared fetch position.
    """

    def __init__(
        self,
        connection: Optional["Connection"] = None,
        columns: Optional[List[str]] = None,
        rows: Optional[List[Dict[str, Any]]] = None,
        stats: Optional[Dict[str, int]] = None,
    ):
        self._connection = connection
        self._stats = dict(stats) if stats else {}  # pre-filled path only
        # _apply_result is the single write site for _columns/_column_types/
        # _rows/_summary/_position; None summary = no statement ran.
        self._apply_result(
            list(columns) if columns else [],
            [],
            list(rows) if rows else [],
            None,
        )
        #: DB-API: default number of rows ``fetchmany`` returns.
        self.arraysize = 1

    # --- Execution (bound cursors) ---------------------------------------

    def _apply_result(self, columns, column_types, rows, summary):
        """Install one execution's result state — the single write site for
        ``_columns`` / ``_column_types`` / ``_rows`` / ``_summary`` / the
        fetch position. A short ``column_types`` (the pre-filled path passes
        none) is padded with ``None`` here so ``_columns`` and
        ``_column_types`` are always the same length."""
        self._columns = columns
        self._column_types = list(column_types) + [None] * (
            len(columns) - len(column_types)
        )
        self._rows = rows
        self._summary = summary
        self._position = 0

    def execute(
        self,
        query: str,
        params: Optional[Dict[str, Any]] = None,
        *,
        tx_metadata: Optional[Dict[str, Any]] = None,
    ) -> "Cursor":
        """Execute a Cypher query, replacing this cursor's result set.

        ``tx_metadata`` (keyword-only, ADR-0610) seeds the connection's
        per-transaction metadata map before the statement — see
        :meth:`relate.connection.Connection.execute` for the channel's
        contract (merge order, the commit boundary, the preview gate).

        Returns ``self`` so calls can be chained (``cur.execute(q).fetchall()``).
        """
        if self._connection is None:
            raise ProgrammingError(
                "cursor is not bound to a connection; use connection.cursor()"
            )
        columns, column_types, rows, summary = self._connection._raw_execute(
            query, params, tx_metadata
        )
        self._apply_result(columns, column_types, rows, summary)
        return self

    def executemany(
        self, query: str, seq_of_params: List[Dict[str, Any]]
    ) -> "Cursor":
        """Execute ``query`` once per parameter set. The cursor reflects the
        final execution (or an empty result set if the sequence was empty)."""
        ran = False
        for params in seq_of_params:
            self.execute(query, params)
            ran = True
        if not ran:
            self._apply_result([], [], [], None)
        return self

    # --- Metadata --------------------------------------------------------

    @property
    def columns(self) -> List[str]:
        """Column names from the RETURN clause."""
        return list(self._columns)

    @property
    def description(self) -> Optional[List[tuple]]:
        """DB-API 2.0 description: a 7-tuple per column ``(name, type_code,
        display_size, internal_size, precision, scale, null_ok)``.

        ``None`` when the statement produced no result columns. ``type_code``
        comes from the engine-reported column type (ADR-0224), stamped from
        the first row and mapped onto the PEP 249 type-comparison objects:
        ``STRING`` / ``BINARY`` report themselves, ``INTEGER`` / ``FLOAT`` /
        ``BOOLEAN`` report ``relate.NUMBER``, ``DATETIME`` / ``DATE`` /
        ``TIME`` report ``relate.DATETIME``; every other canonical name
        (graph, spatial, list/map, duration types and ``ANY``, the empty or
        Null stamp) has no DB-API counterpart and reports ``None``.
        """
        if not self._columns:
            return None
        # _column_types is padded to _columns' length at the write site
        # (_apply_result); a pre-filled cursor's None pads report no
        # DB-API type code.
        return [
            (col, engine_type_code(engine_type), None, None, None, None, None)
            for col, engine_type in zip(self._columns, self._column_types)
        ]

    @property
    def rowcount(self) -> int:
        """Number of rows returned by a read query, or ``-1`` for a write query
        (whose effect is reported via :attr:`stats`) per the DB-API."""
        if self._columns:
            return len(self._rows)
        return -1

    @property
    def stats(self) -> Dict[str, int]:
        """Mutation statistics from the last executed query (all 11 counters)."""
        if self._summary is not None:
            return self._summary.counters
        return dict(self._stats)  # pre-filled cursor (no statement ran)

    # --- Fetching --------------------------------------------------------

    def _wrap(self, row: Dict[str, Any]) -> Record:
        """One engine row as a :class:`~relate.Record` (dict subclass)."""
        return Record._wrap(self._columns, row)

    def fetchone(self) -> Optional[Record]:
        """Fetch the next row, or ``None`` if exhausted."""
        if self._position >= len(self._rows):
            return None
        row = self._rows[self._position]
        self._position += 1
        return self._wrap(row)

    def fetchmany(self, size: Optional[int] = None) -> List[Record]:
        """Fetch up to ``size`` rows (default :attr:`arraysize`)."""
        if size is None:
            size = self.arraysize
        end = min(self._position + size, len(self._rows))
        rows = self._rows[self._position : end]
        self._position = end
        return [self._wrap(r) for r in rows]

    def fetchall(self) -> List[Record]:
        """Fetch all remaining rows."""
        rows = self._rows[self._position :]
        self._position = len(self._rows)
        return [self._wrap(r) for r in rows]

    # --- Driver-style conveniences (ADR-0213) ---------------------------

    def data(self) -> List[Record]:
        """All remaining rows (consumes them, like the driver's
        ``Result.data()``)."""
        return self.fetchall()

    def fetch(self, n: int) -> List[Record]:
        """The driver's spelling of :meth:`fetchmany`."""
        return self.fetchmany(n)

    def peek(self) -> Optional[Record]:
        """The next row without advancing the fetch position, or ``None``."""
        if self._position >= len(self._rows):
            return None
        return self._wrap(self._rows[self._position])

    def single(self) -> Record:
        """The one remaining row; raises :class:`~relate.ResultNotSingleError`
        on zero or more than one, like the driver's ``Result.single()``."""
        if self._position + 1 == len(self._rows):
            row = self._rows[self._position]
            self._position += 1
            return self._wrap(row)
        raise ResultNotSingleError(
            f"expected exactly one row, got {len(self._rows) - self._position}"
        )

    def keys(self) -> List[str]:
        """Column names — the driver's naming of :attr:`columns`."""
        return self.columns

    def consume(self) -> ResultSummary:
        """Discard any remaining rows and return the statement's
        :class:`~relate.ResultSummary` (ADR-0215) — the driver's
        ``Result.consume()`` semantics.

        The summary carries the mutation ``counters`` (as on
        :attr:`stats`), the resolved ``namespace`` and ``query_type``
        (ADR-0285), the engine-reported ``elapsed_ms`` (ADR-0221) with
        its ``parse_ms``/``plan_ms``/``execute_ms`` phase split
        (ADR-0491), and
        the native ``notifications`` list (ADR-0363; empty on a plain
        run, populated by ``EXPLAIN``/``PROFILE``).
        Idempotent: a second
        call returns the same summary. Raises ``ProgrammingError`` when
        no statement has run on this cursor.
        """
        if self._summary is None:
            raise ProgrammingError("no statement has been executed on this cursor")
        self._position = len(self._rows)
        return self._summary

    def graph(self) -> "ResultGraph":
        """The unique graph entities of this result as a
        :class:`~relate.ResultGraph` (ADR-0214).

        Walks **all** rows of the result — rows are materialized at execute
        time, so the fetch position is irrelevant (the driver's
        ``Result.graph()`` builds from consumed records; there is nothing to
        consume here). Every ``Node``, ``Relationship`` and ``Path`` a row
        carries, directly or nested in a list or map, is collected,
        de-duplicated by id. A result with no graph entities returns an
        empty ``ResultGraph``.
        """
        nodes, relationships = collect_graph_entities(self._rows)
        return ResultGraph(nodes, relationships)

    def to_df(self, **kwargs):
        """The remaining rows as a ``pandas.DataFrame`` (consumes them).

        pandas is an optional extra — ``pip install relate-db[pandas]``.
        Keyword arguments pass through to ``pandas.DataFrame``.
        """
        try:
            import pandas as pd
        except ImportError:
            raise ImportError(
                "to_df() needs pandas: pip install 'relate-db[pandas]'"
            ) from None
        rows = self.fetchall()
        return pd.DataFrame([r.data() for r in rows], columns=self._columns, **kwargs)

    def to_arrow(self, **kwargs):
        """The remaining rows as a ``pyarrow.Table`` (consumes them).

        pyarrow is an optional extra — ``pip install relate-db[arrow]``.
        Keyword arguments pass through to ``pyarrow.Table.from_pylist``.
        From the table, ``.to_pandas()`` and ``pl.from_arrow()`` both work,
        so this is the dependency-light bulk-conversion path.
        """
        try:
            import pyarrow as pa
        except ImportError:
            raise ImportError(
                "to_arrow() needs pyarrow: pip install 'relate-db[arrow]'"
            ) from None
        rows = self.fetchall()
        data = [r.data() for r in rows]
        if data:
            return pa.Table.from_pylist(data, **kwargs)
        # No rows: keep the column shape with a null-typed column each, so
        # .to_pandas() / pl.from_arrow() still see the right column names.
        schema = pa.schema([(name, pa.null()) for name in self._columns])
        return pa.Table.from_arrays(
            [pa.array([], type=pa.null()) for _ in self._columns], schema=schema
        )

    # --- Iteration (DB-API: advances the fetch position) -----------------

    def __iter__(self) -> Iterator[Dict[str, Any]]:
        return self

    def __next__(self) -> Dict[str, Any]:
        row = self.fetchone()
        if row is None:
            raise StopIteration
        return row

    def __len__(self) -> int:
        """Total number of rows in the result (not the number remaining)."""
        return len(self._rows)

    def close(self) -> None:
        """Close the cursor. Results are fully materialized, so this is a
        no-op; the cursor stays usable afterwards."""
