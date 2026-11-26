"""DB-API 2.0 (PEP 249) exception hierarchy for Relate.

The standard tree::

    Exception
     +-- Warning
     +-- Error                    (also exported as ``RelateError``)
          +-- InterfaceError
          +-- DatabaseError
               +-- DataError
               +-- OperationalError
               +-- IntegrityError
               +-- InternalError
               +-- ProgrammingError
               +-- NotSupportedError

``RelateError`` is a backward-compatible alias for ``Error`` (the historical
Relate base class), so existing ``except RelateError`` code keeps
catching every database error.

Relate failures carry stable ``kind`` / ``tck_class`` metadata (surfaced by
the native module as ``.kind`` / ``.tck_class`` attributes). They map onto the
most specific exception below via :func:`exception_for` -- a constraint
violation becomes :class:`IntegrityError`, a type error becomes
:class:`DataError`, a syntax or parameter error becomes
:class:`ProgrammingError`, and so on.

Every mapped exception also carries the engine's own classification
(ADR-0502 §2), copied verbatim from the native error -- never derived from
``kind`` here: ``neo4j_code`` (e.g. ``Neo.ClientError.Statement.SyntaxError``),
``gql_status`` (e.g. ``42001``), ``stage`` (``"compile"``, ``"runtime"`` or
``"internal"``) and ``retryable`` (``True`` exactly when a retry of the
transaction can succeed, e.g. a write-write conflict). All four are ``None``
on an error the engine never classified (a binding-level misuse such as a
closed connection, or an error raised by the Python layer itself).
"""


class Warning(Exception):  # noqa: A001 - DB-API mandates this name
    """DB-API warning (not raised as an error)."""


# The engine's four ``ErrorInfo`` facts (ADR-0502 §2) — the sub-vocabulary
# that is ``None`` on an error the engine never classified.
ENGINE_ERROR_ATTRS = (
    "neo4j_code",
    "gql_status",
    "stage",
    "retryable",
)

# The native error-fact vocabulary: the attribute names the native module
# attaches to every ``RelateNativeError`` and the DB-API mapping in
# connection.py carries over unchanged — the binding vocabulary
# (``kind``/``tck_class``) plus the engine facts above. One source:
# ``Error``'s class defaults are derived from it below.
NATIVE_ERROR_ATTRS = ("kind", "tck_class") + ENGINE_ERROR_ATTRS


class Error(Exception):
    """Base class of every Relate database error (DB-API ``Error``).

    Attributes:
        kind: The finest-grained native error kind — always set on an
            error raised by the native module (the binding vocabulary
            every error carries, classified or not); ``None`` only on an
            error the Python layer raised itself.
        tck_class: The openCypher error class — set exactly when ``kind``
            is (the same native source attaches both).
        neo4j_code: The Neo4j status code — ``None`` unless the engine
            classified the error.
        gql_status: The GQLSTATUS code — ``None`` unless the engine
            classified the error.
        stage: ``"compile"``, ``"runtime"`` or ``"internal"`` — ``None``
            unless the engine classified the error.
        retryable: Whether retrying the transaction can succeed —
            ``None`` unless the engine classified the error.
    """


# The six error facts default to ``None`` on ``Error`` (and every
# subclass), so an error the Python layer raised itself still exposes
# them. Derived from ``NATIVE_ERROR_ATTRS`` — the one vocabulary source:
# connection.py's native-error mapping reads the same tuple, never a
# second list.
for _attr in NATIVE_ERROR_ATTRS:
    setattr(Error, _attr, None)
del _attr


# Backward-compatible alias for the historical Relate base class.
RelateError = Error


class InterfaceError(Error):
    """Error related to the database interface (native module communication)."""


class DatabaseError(Error):
    """Error related to the database itself."""


class DataError(DatabaseError):
    """Error caused by problems with the processed data (bad type/value)."""


class OperationalError(DatabaseError):
    """Error related to the database's operation (open, I/O, transaction)."""


class IntegrityError(DatabaseError):
    """Error when the relational/graph integrity of the database is affected.

    Raised for constraint violations (uniqueness, node key, existence, and
    deleting a still-connected node without ``DETACH``).
    """


class InternalError(DatabaseError):
    """Error internal to the database (an invariant the caller cannot fix)."""


class ProgrammingError(DatabaseError):
    """Programming error: syntax error, missing parameter, wrong number of
    arguments, operating on a closed connection, etc."""


class NotSupportedError(DatabaseError):
    """A method or database API not supported by Relate was used."""


class ResultNotSingleError(ProgrammingError):
    """``cursor.single()`` got zero or more than one row (ADR-0213 — the
    driver's ``ResultNotSingleError``)."""


# --- Mapping from native error metadata to a DB-API exception ------------

# Finest-grained: keyed on ``kind``. Overrides the coarser class map.
_KIND_TO_EXCEPTION = {
    # ConstraintVerificationFailed class -> integrity.
    "ConstraintViolation": IntegrityError,
    "DeleteConnectedNode": IntegrityError,
    # SemanticError class, but more specifically classifiable.
    "InternalError": InternalError,
    "DataException": DataError,
    "DataError": DataError,
    "MergeReadOwnWrites": ProgrammingError,
    # Binding-level: using a connection on a thread other than the one that
    # opened it (DB-API threadsafety = 1, GH #352 / LABS-700), and operating
    # on a closed one — the two misuse refusals.
    "WrongThread": ProgrammingError,
    "ConnectionClosed": ProgrammingError,
    # ProcedureError class -> calling an unknown/misused procedure is a
    # programming error.
    "ProcedureError": ProgrammingError,
    "ProcedureNotFound": ProgrammingError,
    # The statement deadline (ADR-0692): the query ran past its budget.
    "StatementTimeout": OperationalError,
    # EntityNotFound class -> a runtime/operational condition.
    "EntityNotFound": OperationalError,
    "DeletedEntityAccess": OperationalError,
    # Fixed-namespace errors (ADR-0116 / ADR-0117 / ADR-0229). Selecting an
    # unknown namespace, any statement routed to the not-queryable
    # `system_ontologies` namespace (the kind scan recovers its
    # `SemanticError` class token), or writing to a read-only database is
    # operational; an unsupported administration command is not supported;
    # changing the namespace with an open transaction is operational.
    "DatabaseNotFound": OperationalError,
    "WriteOnReadOnlyAccessDatabase": OperationalError,
    "UnsupportedAdministrationCommand": NotSupportedError,
    "TransactionError": OperationalError,
}

# Coarse fallback: keyed on ``tck_class`` (one of the openCypher error classes).
_CLASS_TO_EXCEPTION = {
    "SyntaxError": ProgrammingError,
    "TypeError": DataError,
    "ArgumentError": DataError,
    "EntityNotFound": OperationalError,
    "SemanticError": OperationalError,
    "ProcedureError": ProgrammingError,
    "ParameterMissing": ProgrammingError,
    "ConstraintVerificationFailed": IntegrityError,
}


def exception_for(kind, tck_class):
    """Return the DB-API exception class for a native database error.

    Args:
        kind: The finest-grained native error kind, or ``None``.
        tck_class: The openCypher error class, or ``None``.

    Prefers a kind-level mapping; falls back to the TCK error class; finally
    to :class:`DatabaseError`. Never collapses everything to
    :class:`OperationalError`.
    """
    if kind and kind in _KIND_TO_EXCEPTION:
        return _KIND_TO_EXCEPTION[kind]
    if tck_class and tck_class in _CLASS_TO_EXCEPTION:
        return _CLASS_TO_EXCEPTION[tck_class]
    return DatabaseError
