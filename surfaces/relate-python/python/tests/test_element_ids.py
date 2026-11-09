"""Element ids: the engine's ``elementId()`` spelling (ADR-0502 §6) —
``<sid>:rl-id:n<id>`` for nodes, ``r<id>`` for relationships, so node
0 never collides with relationship 0."""


def test_element_ids_match_cypher_element_id(db):
    db.execute("CREATE (:A)-[:T]->(:B)")
    row = db.execute(
        "MATCH (a:A)-[r:T]->(b:B) RETURN a, r, b, elementId(a) AS ea, "
        "elementId(r) AS er, elementId(b) AS eb"
    ).fetchone()
    a, r, b = row["a"], row["r"], row["b"]
    assert a.element_id == row["ea"] == f"4:rl-id:n{a.id}"
    assert r.element_id == row["er"] == f"5:rl-id:r{r.id}"
    assert b.element_id == row["eb"]
    assert r.start_node_element_id == a.element_id
    assert r.end_node_element_id == b.element_id


def test_node_zero_and_rel_zero_element_ids_differ(db):
    db.execute("CREATE (:A)-[:T]->(:A)")
    row = db.execute("MATCH (a)-[r]->() RETURN a, r").fetchone()
    assert row["a"].id == 0 and row["r"].id == 0
    assert row["a"].element_id != row["r"].element_id
