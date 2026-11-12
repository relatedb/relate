"""Driver-style result rows (ADR-0213).

A ``Record`` is a ``dict`` — every mapping operation keeps working — plus the
Neo4j driver conveniences: attribute access, ``.data()``, and ``.keys()`` in
column order.
"""

from relate.row_values import map_row_value


def _as_data(value):
    """The Neo4j-driver form of one graph value (properties only)."""
    return value.data()


def _as_dict(value):
    """The lossless, JSON-ready form of one graph value."""
    return value.to_dict()


class Record(dict):
    """One result row: a ``dict`` with Neo4j-driver-style conveniences.

    ``r["name"]`` (DB-API habit), ``r.name`` (attribute access), and
    ``r.data()`` (a nested plain-data copy) all read the same values. Column
    order is the statement's RETURN order, not insertion order. Rows are
    read-only through attributes: ``r.name = x`` raises ``AttributeError``
    (``r["name"] = x`` still works — a ``Record`` is a ``dict``).
    """

    __slots__ = ("_columns",)

    def __init__(self, columns=None, **values):
        super().__init__(values)
        self._columns = list(columns) if columns is not None else list(values)

    def __getattr__(self, name):
        try:
            return self[name]
        except KeyError:
            raise AttributeError(name) from None

    def keys(self):
        """Column names in the statement's RETURN order (a copy)."""
        return list(self._columns)

    def data(self):
        """A plain ``dict`` copy of the row, converted like the Neo4j driver.

        A node or relationship becomes the ``dict`` of its properties, a path
        the list alternating node dicts and relationship types; lists and maps
        are converted recursively. Ids, labels and endpoints are dropped; use
        :meth:`to_dict` to keep them.
        """
        return map_row_value(self._ordered(), _as_data)

    def to_dict(self):
        """A JSON-ready ``dict`` copy of the row that keeps everything.

        A node becomes ``{"id", "element_id", "labels", "properties"}``, a
        relationship ``{"id", "element_id", "type", "start_node", "end_node",
        "start_node_element_id", "end_node_element_id", "properties"}``, a
        path ``{"nodes": [...], "relationships": [...]}``; lists and maps are
        converted recursively. Points, durations, temporals and vectors stay
        as they are.
        """
        return map_row_value(self._ordered(), _as_dict)

    def _ordered(self):
        """The row as a plain ``dict`` in column order (extra keys last)."""
        out = {c: self[c] for c in self._columns if c in self}
        out.update({k: v for k, v in self.items() if k not in out})
        return out

    def __reduce__(self):
        # Pickle round-trips through the column order plus the values.
        return (Record._wrap, (self._columns, dict(self)))

    @classmethod
    def _wrap(cls, columns, row):
        """Wrap one engine row (a plain dict) without changing its values."""
        rec = cls(columns)
        dict.update(rec, row)
        return rec
