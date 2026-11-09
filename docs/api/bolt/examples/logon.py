#!/usr/bin/env python3
"""Connect at Bolt 5.4: LOGON, then an element-id-bearing result (ADR-0484).

Start the server first — behind the `--bolt-identity` compat shim,
because the current python driver validates the HELLO `server` agent
and rejects anything not starting `Neo4j/` (ADR-0293):
  cargo run -p relate-cli -- serve --host 127.0.0.1 --bolt 7687 --database :memory: --auth none --bolt-identity Neo4j/5.26.0
The port comes from RELATE_BOLT_PORT_IDENTITY (scripts/bolt-smoke.sh
exports it beside the spawn; unset is a configuration error).
"""
from _topology import bolt_uri
from neo4j import GraphDatabase

def main():
    # The driver's handshake proposes, in its own order: the manifest-v1
    # marker, the 5.0-5.8 range, 4.4 and 3.0. The server speaks 4.4 and
    # 5.4 only and answers the FIRST proposal covering a spoken version
    # (ADR-0484 decision 2) -- the 5.8 range covers 5.4, so every query
    # below runs the 5.4 message set.
    driver = GraphDatabase.driver(
        bolt_uri("RELATE_BOLT_PORT_IDENTITY"), auth=("neo4j", "unused"))
    assert driver.get_server_info().protocol_version == (5, 4)

    # At 5.x the HELLO does not authenticate: the driver sends user_agent
    # and bolt_agent, then LOGON with the token (basic here; under
    # --auth none every LOGON succeeds, and with a credential configured
    # the server checks it there, ADR-0484 decision 4). The query answers
    # through the LOGON-opened session.
    records, _, _ = driver.execute_query("RETURN 1 AS n")
    assert records[0]["n"] == 1

    # At 5.4 the graph structures carry element-id fields (ADR-0484
    # decision 6): Node appends element_id, Relationship appends
    # element_id, start_node_element_id and end_node_element_id. The
    # strings are the engine elementId() helper's own output
    # (4:relate-neo4j:n<N> / 5:relate-neo4j:r<N>) -- the same string the
    # elementId() function answers, never Neo4j's uuid form.
    records, _, _ = driver.execute_query(
        "CREATE (a:Probe {name: $ada})-[r:KNOWS]->(b:Probe {name: $bob}) "
        "RETURN a, r, elementId(a) AS eid, elementId(r) AS reid",
        ada="Ada",
        bob="Bob",
    )
    node, rel = records[0]["a"], records[0]["r"]
    assert node.element_id == records[0]["eid"], node.element_id
    assert rel.element_id == records[0]["reid"], rel.element_id
    assert node.element_id.startswith("4:relate-neo4j:n"), node.element_id
    assert rel.element_id.startswith("5:relate-neo4j:r"), rel.element_id
    driver.close()
    print("ok")

if __name__ == "__main__":
    main()
