#!/usr/bin/env python3
"""Select a namespace over Bolt with the official Neo4j driver (ADR-0117).

Start the server first — behind the `--bolt-identity` compat shim,
because the current python driver validates the HELLO `server` agent and
rejects anything not starting `Neo4j/` (ADR-0293):
  RELATE_PREVIEW=ontology cargo run -p relate-cli -- serve --host 127.0.0.1 --bolt 7687 --database :memory: --bolt-identity Neo4j/5.26.0
The port comes from RELATE_BOLT_PORT_IDENTITY (scripts/bolt-smoke.sh
exports it beside the spawn; unset is a configuration error).
RELATE_PREVIEW=ontology is required: `system_ontologies` is invisible
without the opt-in (ADR-0593) — the smoke script exports it for its
spawned servers.
"""
from _topology import bolt_uri
from neo4j import GraphDatabase
from neo4j.exceptions import ClientError

def main():
    driver = GraphDatabase.driver(
        bolt_uri("RELATE_BOLT_PORT_IDENTITY"), auth=("neo4j", "unused"))
    # `database_` is sent as `db` in the auto-commit RUN extra. The server
    # runs under RELATE_PREVIEW=ontology (see the module docstring,
    # ADR-0593), so `system_ontologies` is visible; routed there the one
    # statement kind that still runs is the
    # administration set (ADR-0116 §2): SHOW DATABASES keeps its three rows.
    records, _, _ = driver.execute_query(
        "SHOW DATABASES", database_="system_ontologies"
    )
    assert len(records) == 3
    # A data query routed there fails ADR-0229's not-queryable gate (42N17),
    # reads included.
    try:
        driver.execute_query(
            "MATCH (n) RETURN count(n) AS c", database_="system_ontologies"
        )
        raise AssertionError("a data query on system_ontologies must fail")
    except ClientError as e:
        assert (
            "This database is not queryable. Use the db.ontology.* procedures "
            "on the user database 'neo4j' instead." in str(e)
        ), e
    # The selection does not carry over: this RUN is back in `neo4j`, and
    # the failed queries above left the driver usable.
    records, _, _ = driver.execute_query(
        "CREATE (p:Probe {name: 'Ada'}) RETURN p.name AS name"
    )
    assert records[0]["name"] == "Ada"
    # An unknown database fails with the CE text and Neo4j's code
    # (LABS-323 finding D-46).
    try:
        driver.execute_query("RETURN 1 AS n", database_="nosuch")
        raise AssertionError("database_='nosuch' must fail")
    except ClientError as e:
        assert "Graph not found: nosuch" in str(e), e
        assert e.code == "Neo.ClientError.Database.DatabaseNotFound", e
    driver.close()
    print("ok")

if __name__ == "__main__":
    main()
