"""Transaction metadata: the ``tx_metadata`` kwarg on ``execute`` (ADR-0610).

Run from the repo root after `pip install ./surfaces/relate-python`:

    python docs/api/python/examples/tx_metadata.py
"""

import os

import relate


def main():
    # The whole machinery is the `value-provenance` preview feature's
    # (ADR-0610/ADR-0371): `RELATE_PREVIEW` is read once, at open, so the
    # opt-in must be set before connecting. The previous value is restored
    # on the way out, so running the example leaves the process as it
    # found it.
    previous = os.environ.get("RELATE_PREVIEW")
    os.environ["RELATE_PREVIEW"] = "value-provenance"
    try:
        conn = relate.connect(":memory:")
        conn.autocommit = False

        # The kwarg seeds the transaction's metadata map before the
        # statement runs; values convert exactly like parameters.
        conn.execute(
            "CREATE (p:Person {name: $name})",
            {"name": "Ada"},
            tx_metadata={"by": "app", "run": 42},
        )

        # While the transaction is open, the live map is readable back.
        row = conn.execute(
            "CALL tx.getMetaData() YIELD metadata RETURN metadata"
        ).single()
        assert row["metadata"] == {"by": "app", "run": 42}

        # `CALL tx.setMetaData` is the statement channel onto the same
        # map: seeds merge in call order, later keys winning.
        conn.execute("CALL tx.setMetaData({by: 'pipeline'})")
        row = conn.execute(
            "CALL tx.getMetaData() YIELD metadata RETURN metadata"
        ).single()
        assert row["metadata"] == {"by": "pipeline", "run": 42}

        # The merged map serializes into the commit's record at the
        # transaction boundary and is cleared after (ADR-0610).
        bookmark = conn.commit()
        assert bookmark is not None  # 'relate:<n>' — the transaction wrote
        row = conn.execute(
            "CALL tx.getMetaData() YIELD metadata RETURN metadata"
        ).single()
        assert row["metadata"] == {}
        conn.close()

        # Without the opt-in the kwarg fails the authored preview error —
        # the embedded channel never silently drops the map. An empty
        # value enables nothing (ADR-0371's unset-equivalent).
        os.environ["RELATE_PREVIEW"] = ""
        plain = relate.connect(":memory:")
        try:
            plain.execute("CREATE (:Person {name: 'Bob'})", tx_metadata={"by": "app"})
        except relate.OperationalError as e:
            assert "preview feature 'value-provenance' is not enabled" in str(e)
            assert "set RELATE_PREVIEW=value-provenance" in str(e)
        else:
            raise AssertionError("expected the authored preview error")
        # The statement never ran: the connection stays usable.
        assert plain.execute("MATCH (n) RETURN count(n) AS n").single()["n"] == 0
        plain.close()
    finally:
        if previous is None:
            os.environ.pop("RELATE_PREVIEW", None)
        else:
            os.environ["RELATE_PREVIEW"] = previous

    print("tx_metadata example OK")


if __name__ == "__main__":
    main()
