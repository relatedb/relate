"""The per-statement result summary (ADR-0215)."""

import copy
from typing import Any, Dict, List, Optional


class ResultSummary:
    """The metadata of one executed statement (ADR-0215).

    ``counters`` are the 11 mutation counters (same dict as
    :attr:`Cursor.stats`); ``namespace`` is the statement's resolved
    namespace and ``query_type`` its plan-time classification
    (ADR-0285); ``elapsed_ms`` is the engine-reported wall-clock
    execution time (ADR-0221): parse, plan and run, excluding
    client-side row handling. ``parse_ms``, ``plan_ms`` and
    ``execute_ms`` (ADR-0491) split that total into its PARSE / PLAN /
    EXECUTE windows — disjoint sub-windows whose sum never exceeds
    ``elapsed_ms``. ``notifications`` is the engine-native
    query-notification list (ADR-0363): empty on a plain run, populated
    by ``EXPLAIN``/``PROFILE``. ``plan`` is the statement's structured
    plan document (LABS-418, ADR-0381), or ``None`` on a plain run.
    ``bookmark`` is an auto-commit write's commit position (ADR-0502 §7),
    ``relate:<n>``, or ``None`` when the statement committed nothing.

    The constructor is keyword-only — every field above is passed by name
    (``counters=…``, ``bookmark=…``); :meth:`Cursor.consume
    <relate.cursor.Cursor.consume>` is the usual source of a summary.
    """

    def __init__(
        self,
        *,
        counters: Dict[str, int],
        namespace: str,
        query_type: str,
        elapsed_ms: float,
        parse_ms: float,
        plan_ms: float,
        execute_ms: float,
        notifications: List[Dict[str, Any]],
        plan: Optional[Dict[str, Any]] = None,
        bookmark: Optional[str] = None,
    ):
        self._counters = counters
        self._namespace = namespace
        self._query_type = query_type
        self._elapsed_ms = elapsed_ms
        self._parse_ms = parse_ms
        self._plan_ms = plan_ms
        self._execute_ms = execute_ms
        self._notifications = notifications
        self._plan = plan
        self._bookmark = bookmark

    @property
    def counters(self):
        """The mutation counters, as on ``Cursor.stats``."""
        return dict(self._counters)

    @property
    def namespace(self):
        """The statement's resolved namespace."""
        return self._namespace

    @property
    def query_type(self):
        """The plan-time classification: ``r`` / ``w`` / ``rw`` / ``s``."""
        return self._query_type

    @property
    def elapsed_ms(self):
        """Engine-reported wall-clock milliseconds for the statement
        (ADR-0221)."""
        return self._elapsed_ms

    @property
    def parse_ms(self):
        """Engine-reported PARSE-phase milliseconds (ADR-0491): the
        front-end parse and lowering window of :attr:`elapsed_ms`."""
        return self._parse_ms

    @property
    def plan_ms(self):
        """Engine-reported PLAN-phase milliseconds (ADR-0491): binding,
        logical planning, admission classification."""
        return self._plan_ms

    @property
    def execute_ms(self):
        """Engine-reported EXECUTE-phase milliseconds (ADR-0491): the
        dispatch — or the EXPLAIN plan render. The three phases are
        disjoint sub-windows whose sum never exceeds :attr:`elapsed_ms`."""
        return self._execute_ms

    @property
    def notifications(self):
        """The query notifications derived with the plan tree (ADR-0363).

        A list of dicts in the engine-native shape: ``code``,
        ``gql_status``, ``severity``, ``category``, ``title``,
        ``description``, ``remedy`` and, on operator-scoped kinds,
        ``operator_id``. Empty on a plain run; ``EXPLAIN``/``PROFILE``
        populate it from the plan they build.
        """
        return [dict(notification) for notification in self._notifications]

    @property
    def plan(self):
        """The statement's structured plan (LABS-370, ADR-0381).

        A ``relate.plan/v1`` document — ``schema``, ``profiled``,
        ``statement`` and ``root``, each node carrying ``operator``,
        ``details``, ``identifiers``, ``estimated_rows``,
        ``estimate_provenance`` (``measured``, ``sampled``, ``statistic`` or ``defaulted``;
        ``None`` exactly when ``estimated_rows`` is), ``actual``
        (PROFILE metrics, including the read-kind mix and the walk's
        ``start``), ``children`` and ``warnings`` (the engine's
        remedy-bearing notifications for that operator, ADR-0363). ``None``
        when the statement carried no plan (a plain run).
        """
        return None if self._plan is None else copy.deepcopy(self._plan)

    @property
    def bookmark(self):
        """The statement's bookmark (ADR-0502 §7): an auto-commit write's
        commit sequence spelled ``relate:<n>`` (ADR-0310), monotonic per
        store. ``None`` when the statement committed nothing -- a read, or
        any statement inside an explicit transaction, whose bookmark
        ``Connection.commit()`` returns."""
        return self._bookmark

    def __repr__(self):
        return (
            f"ResultSummary(query_type={self._query_type!r}, "
            f"namespace={self._namespace!r}, elapsed_ms={self._elapsed_ms:.3f})"
        )
