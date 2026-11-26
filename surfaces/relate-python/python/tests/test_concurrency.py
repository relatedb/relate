"""Threading semantics: GIL release and connection thread affinity.

Two concerns (GH #352 / LABS-700):

- GIL release: ``execute`` / ``execute_arrow`` release the GIL around the
  Rust engine call, so other threads make progress while a query runs; the
  first tests assert many threads running queries concurrently all complete
  without deadlock -- exercising the ``py.detach`` path end to end.
- Connection affinity: a ``Connection`` is usable only on the thread that
  opened it (``threadsafety == 1``). Cross-thread use is a typed
  ``ProgrammingError`` (never a pyo3 ``PanicException``), and releasing the
  last reference on another thread is safe.
"""

import threading
import time

import relate


def test_concurrent_connections_complete():
    results = {}
    errors = []

    def worker(i):
        try:
            conn = relate.connect(":memory:")
            conn.executemany("CREATE (:N {v: $v})", [{"v": j} for j in range(100)])
            cur = conn.execute("MATCH (n:N) RETURN count(*) AS c")
            results[i] = cur.fetchone()["c"]
            conn.close()
        except Exception as e:  # pragma: no cover - surfaced via assert below
            errors.append((i, repr(e)))

    threads = [threading.Thread(target=worker, args=(i,)) for i in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=30)

    assert not any(t.is_alive() for t in threads), "a worker thread hung"
    assert not errors, f"workers raised: {errors}"
    assert results == {i: 100 for i in range(8)}


def test_background_query_does_not_block_main_thread():
    """While a background thread runs a batch of queries with the GIL released,
    the main thread continues making progress (a counter advances)."""
    conn = relate.connect(":memory:")
    stop = threading.Event()

    def background():
        c = relate.connect(":memory:")
        for _ in range(200):
            c.executemany("CREATE (:N {v: $v})", [{"v": j} for j in range(20)])
        c.close()
        stop.set()

    t = threading.Thread(target=background)
    t.start()

    ticks = 0
    while not stop.is_set() and ticks < 10_000_000:
        ticks += 1

    t.join(timeout=30)
    conn.close()
    # If the GIL were held for the whole background run, the main loop could not
    # have advanced before `stop` was set. A nonzero tick count is a (coarse)
    # signal the GIL was released during the queries.
    assert ticks > 0


def test_connection_used_on_other_thread_raises_programming_error():
    """GH #352 / LABS-700: using a Connection on a thread other than the one
    that opened it is a typed refusal (``ProgrammingError``), never a pyo3
    ``PanicException`` (which ``except Exception`` cannot catch)."""
    conn = relate.connect(":memory:")
    conn.execute("CREATE (:T {id: 1})")
    outcome = {}

    def worker():
        try:
            conn.execute("MATCH (t:T) RETURN t").fetchall()
        except relate.ProgrammingError as e:
            outcome["error"] = e
        except BaseException as e:  # pragma: no cover - the panic regression
            outcome["panic"] = e

    t = threading.Thread(target=worker)
    t.start()
    t.join(timeout=30)
    assert not t.is_alive(), "worker thread hung"
    assert "panic" not in outcome, (
        f"cross-thread use raised {outcome.get('panic')!r} instead of ProgrammingError"
    )
    assert isinstance(outcome.get("error"), relate.ProgrammingError), outcome
    # The refusal is not fatal: the owning thread keeps the connection usable.
    cur = conn.execute("MATCH (t:T) RETURN count(t) AS c")
    assert cur.fetchall()[0]["c"] == 1
    conn.close()


def test_connection_dropped_on_other_thread_is_safe():
    """GH #352 / LABS-700: releasing the last reference on a foreign thread
    neither panics nor emits an unraisable exception — the deferred cleanup
    (the native ``Drop`` checkpoint) runs from that thread."""
    import sys

    unraisable = []
    default_hook = sys.unraisablehook
    sys.unraisablehook = unraisable.append
    try:
        holder = [relate.connect(":memory:")]
        t = threading.Thread(target=holder.clear)
        t.start()
        t.join(timeout=30)
    finally:
        sys.unraisablehook = default_hook
    assert not t.is_alive(), "worker thread hung"
    assert unraisable == [], f"unraisable exceptions during foreign-thread drop: {unraisable}"


def test_concurrent_foreign_call_during_query_raises_programming_error():
    """A foreign-thread call landing *inside* the owner's GIL-released query
    window is refused by the affinity check (``ProgrammingError``), not by
    pyo3's borrow check (which would surface as an ``OperationalError``).

    The query path takes ``&self`` for exactly this reason: a shared borrow
    always lets the typed thread-affinity refusal run first.

    The interleaving is deterministic, not sleep-timed: the owner keeps
    querying until the caller has made its attempt, so the attempt lands
    while a query is in flight and the connection is certainly still open —
    the refusal asserted below is therefore always the thread-affine one.
    """
    holder = []
    ready = threading.Event()
    caller_done = threading.Event()
    outcome = {}

    def owner():
        conn = relate.connect(":memory:")
        conn.execute("CREATE (:T {id: 1})")
        holder.append(conn)
        ready.set()
        deadline = time.monotonic() + 30
        while not caller_done.is_set() and time.monotonic() < deadline:
            conn.execute("UNWIND range(1, 200000) AS i WITH i WHERE i % 2 = 0 RETURN count(i)")
        outcome["owner_ok"] = True
        conn.close()

    t = threading.Thread(target=owner)
    t.start()
    assert ready.wait(timeout=30), "owner thread never became ready"
    try:
        holder[0].execute("MATCH (t:T) RETURN t")
        outcome["succeeded"] = True
    except relate.ProgrammingError as e:
        outcome["refusal"] = e
    finally:
        caller_done.set()

    t.join(timeout=60)
    assert not t.is_alive(), "owner thread hung"
    assert outcome.get("owner_ok") is True, "the owner's own query failed"
    assert not outcome.get("succeeded"), "the concurrent foreign call was not refused"
    refusal = outcome.get("refusal")
    assert isinstance(refusal, relate.ProgrammingError), outcome
    assert "thread-affine" in str(refusal), refusal
