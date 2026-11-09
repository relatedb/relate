use super::agg_fold::take_path_fold_bindings;
use super::test_support::{
    build, error_of, exec_autocommit, exec_cypher, exec_cypher_resolved, try_exec_cypher,
};
use super::*;
use crate::graph::Graph;
use relate_core::NodeId;

fn person_named(g: &Graph, id: i64, first: &str, last: &str) -> NodeId {
    g.create_node(
        vec!["Person".into()],
        BTreeMap::from([
            ("firstName".into(), Value::String(first.into())),
            ("lastName".into(), Value::String(last.into())),
            ("ID".into(), Value::Int(id)),
        ]),
    )
    .unwrap()
}

#[test]
fn path_fold_counts_distinct_comments_in_a_place() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let berlin = g
        .create_node(
            vec!["Place".into()],
            BTreeMap::from([("name".into(), Value::String("Berlin".into()))]),
        )
        .unwrap();
    let paris = g
        .create_node(
            vec!["Place".into()],
            BTreeMap::from([("name".into(), Value::String("Paris".into()))]),
        )
        .unwrap();
    let local = person_named(&g, 10, "Ada", "Local");
    let away = person_named(&g, 11, "Bea", "Away");
    g.create_rel(local, berlin, "personIsLocatedIn".into(), BTreeMap::new())
        .unwrap();
    g.create_rel(away, paris, "personIsLocatedIn".into(), BTreeMap::new())
        .unwrap();
    for (author, id, content) in [
        (local, 1, Some("one")),
        (local, 2, Some("two")),
        (local, 3, None),
        (away, 4, Some("away")),
    ] {
        let mut props = BTreeMap::from([("ID".into(), Value::Int(id))]);
        if let Some(content) = content {
            props.insert("content".into(), Value::String(content.into()));
        }
        let comment = g.create_node(vec!["Comment".into()], props).unwrap();
        g.create_rel(comment, author, "commentHasCreator".into(), BTreeMap::new())
            .unwrap();
    }
    g.commit_write();
    let _ = take_path_fold_bindings();
    let result = exec_cypher(
        &g,
        "MATCH (c:Comment)-[:commentHasCreator]->(p:Person)-[:personIsLocatedIn]->(l:Place) \
         WHERE c.content IS NOT NULL AND l.name = \"Berlin\" \
         RETURN COUNT(DISTINCT c.ID) AS num_comments",
    );
    assert_eq!(result.rows, vec![vec![Value::Int(2)]]);
    assert_eq!(take_path_fold_bindings(), 2);
}

#[test]
fn path_fold_counts_distinct_likers() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let rafael = person_named(&g, 1, "Rafael", "Alonso");
    let other = person_named(&g, 2, "Other", "Author");
    let ada = person_named(&g, 3, "Ada", "Liker");
    let bea = person_named(&g, 4, "Bea", "Liker");
    let cam = person_named(&g, 5, "Cam", "Liker");
    let comment = |id: i64, author: NodeId| {
        let comment = g
            .create_node(
                vec!["Comment".into()],
                BTreeMap::from([("ID".into(), Value::Int(id))]),
            )
            .unwrap();
        g.create_rel(comment, author, "commentHasCreator".into(), BTreeMap::new())
            .unwrap();
        comment
    };
    let first = comment(10, rafael);
    let second = comment(11, rafael);
    let foreign = comment(12, other);
    g.create_rel(ada, first, "likeComment".into(), BTreeMap::new())
        .unwrap();
    g.create_rel(ada, second, "likeComment".into(), BTreeMap::new())
        .unwrap();
    g.create_rel(bea, first, "likeComment".into(), BTreeMap::new())
        .unwrap();
    g.create_rel(cam, foreign, "likeComment".into(), BTreeMap::new())
        .unwrap();
    g.commit_write();
    let _ = take_path_fold_bindings();
    let result = exec_cypher(
        &g,
        "MATCH (p:Person)<-[:commentHasCreator]-(c:Comment)<-[:likeComment]-(p2:Person) \
         WHERE p.firstName = \"Rafael\" AND p.lastName = \"Alonso\" \
         RETURN COUNT(DISTINCT p2.ID) AS num_persons",
    );
    assert_eq!(result.rows, vec![vec![Value::Int(2)]]);
    assert!(take_path_fold_bindings() >= 3);
}

#[test]
fn path_fold_groups_interests_of_students() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let school = g
        .create_node(
            vec!["Organisation".into()],
            BTreeMap::from([("name".into(), Value::String("IISc".into()))]),
        )
        .unwrap();
    let other = g
        .create_node(
            vec!["Organisation".into()],
            BTreeMap::from([("name".into(), Value::String("Other".into()))]),
        )
        .unwrap();
    let rust = g
        .create_node(
            vec!["Tag".into()],
            BTreeMap::from([("name".into(), Value::String("Rust".into()))]),
        )
        .unwrap();
    let go = g
        .create_node(
            vec!["Tag".into()],
            BTreeMap::from([("name".into(), Value::String("Go".into()))]),
        )
        .unwrap();
    let mut next_id = 1;
    let mut student = |org: NodeId, tags: &[NodeId]| {
        let person = person_named(&g, next_id, "Stu", "Dent");
        next_id += 1;
        g.create_rel(person, org, "studyAt".into(), BTreeMap::new())
            .unwrap();
        for tag in tags {
            g.create_rel(person, *tag, "hasInterest".into(), BTreeMap::new())
                .unwrap();
        }
    };
    student(school, &[rust, go]);
    student(school, &[rust]);
    student(other, &[rust]);
    g.commit_write();
    let _ = take_path_fold_bindings();
    let result = exec_cypher(
        &g,
        "MATCH (p:Person)-[:studyAt]->(o:Organisation), (p)-[:hasInterest]->(t:Tag) \
         WHERE o.name = \"IISc\" \
         RETURN t.name AS name, COUNT(*) AS tag_count \
         ORDER BY tag_count DESC LIMIT 1",
    );
    assert_eq!(
        result.rows,
        vec![vec![Value::String("Rust".into()), Value::Int(2)]]
    );
    assert!(take_path_fold_bindings() >= 2);
}

#[test]
fn path_fold_requires_both_comma_patterns() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let school = g
        .create_node(
            vec!["Organisation".into()],
            BTreeMap::from([("name".into(), Value::String("Oxford".into()))]),
        )
        .unwrap();
    let tag = g
        .create_node(
            vec!["Tag".into()],
            BTreeMap::from([("name".into(), Value::String("Shakespeare".into()))]),
        )
        .unwrap();
    let enroll = |id: i64, links: &[(&str, NodeId)]| {
        let person = g
            .create_node(
                vec!["Person".into()],
                BTreeMap::from([("ID".into(), Value::Int(id))]),
            )
            .unwrap();
        for (kind, target) in links {
            g.create_rel(person, *target, (*kind).into(), BTreeMap::new())
                .unwrap();
        }
    };
    enroll(1, &[("studyAt", school), ("hasInterest", tag)]);
    enroll(2, &[("studyAt", school), ("hasInterest", tag)]);
    enroll(3, &[("studyAt", school)]);
    enroll(4, &[("hasInterest", tag)]);
    g.commit_write();
    let _ = take_path_fold_bindings();
    let result = exec_cypher(
        &g,
        "MATCH (p:Person)-[:studyAt]->(o:Organisation), (p)-[:hasInterest]->(t:Tag) \
         WHERE o.name = \"Oxford\" AND t.name = \"Shakespeare\" \
         RETURN COUNT(DISTINCT p.ID) AS num_p",
    );
    assert_eq!(result.rows, vec![vec![Value::Int(2)]]);
    assert_eq!(take_path_fold_bindings(), 2);
}

#[test]
fn path_fold_counts_comments_by_place() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let tag = g
        .create_node(
            vec!["Tag".into()],
            BTreeMap::from([("name".into(), Value::String("Copernicus".into()))]),
        )
        .unwrap();
    let other = g
        .create_node(
            vec!["Tag".into()],
            BTreeMap::from([("name".into(), Value::String("Other".into()))]),
        )
        .unwrap();
    let india = g
        .create_node(
            vec!["Place".into()],
            BTreeMap::from([("name".into(), Value::String("India".into()))]),
        )
        .unwrap();
    let spain = g
        .create_node(
            vec!["Place".into()],
            BTreeMap::from([("name".into(), Value::String("Spain".into()))]),
        )
        .unwrap();
    let comment = |id: i64, tag: NodeId, place: NodeId| {
        let comment = g
            .create_node(
                vec!["Comment".into()],
                BTreeMap::from([("ID".into(), Value::Int(id))]),
            )
            .unwrap();
        g.create_rel(comment, tag, "commentHasTag".into(), BTreeMap::new())
            .unwrap();
        g.create_rel(comment, place, "commentIsLocatedIn".into(), BTreeMap::new())
            .unwrap();
    };
    comment(1, tag, india);
    comment(2, tag, india);
    comment(3, tag, spain);
    comment(4, other, india);
    g.commit_write();
    let _ = take_path_fold_bindings();
    let result = exec_cypher(
        &g,
        "MATCH (c:Comment)-[:commentHasTag]->(t:Tag), (c)-[:commentIsLocatedIn]->(l:Place) \
         WHERE t.name CONTAINS \"Copernicus\" \
         RETURN l.name AS name, COUNT(c.ID) AS comment_count \
         ORDER BY comment_count DESC",
    );
    assert_eq!(
        result.rows,
        vec![
            vec![Value::String("India".into()), Value::Int(2)],
            vec![Value::String("Spain".into()), Value::Int(1)],
        ]
    );
    assert_eq!(take_path_fold_bindings(), 3);
}

#[test]
fn variable_length_and_relationship_sum_stay_materialized() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g
        .create_node(
            vec!["A".into()],
            BTreeMap::from([("id".into(), Value::Int(1))]),
        )
        .unwrap();
    let mid = g.create_node(vec!["M".into()], BTreeMap::new()).unwrap();
    let b = g
        .create_node(
            vec!["B".into()],
            BTreeMap::from([("id".into(), Value::Int(2)), ("n".into(), Value::Int(10))]),
        )
        .unwrap();
    let c = g
        .create_node(
            vec!["C".into()],
            BTreeMap::from([("id".into(), Value::Int(3))]),
        )
        .unwrap();
    g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    g.create_rel(a, mid, "R".into(), BTreeMap::new()).unwrap();
    g.create_rel(mid, b, "R".into(), BTreeMap::new()).unwrap();
    g.create_rel(
        a,
        b,
        "E".into(),
        BTreeMap::from([("w".into(), Value::Int(10))]),
    )
    .unwrap();
    g.create_rel(b, c, "E".into(), BTreeMap::new()).unwrap();
    g.commit_write();

    let _ = take_path_fold_bindings();
    let varied = exec_cypher(&g, "MATCH (a:A)-[:R*1..2]->(b:B) RETURN count(*) AS n");
    assert_eq!(varied.rows, vec![vec![Value::Int(2)]]);
    assert_eq!(take_path_fold_bindings(), 0);

    let summed = exec_cypher(
        &g,
        "MATCH (a:A)-[e:E]->(b:B)-[:E]->(c:C) RETURN sum(e.w) AS s",
    );
    assert_eq!(summed.rows, vec![vec![Value::Int(10)]]);
    assert_eq!(take_path_fold_bindings(), 0);
}

#[test]
fn repeated_relationship_variable_stays_on_the_matcher() {
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["A".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["B".into()], BTreeMap::new()).unwrap();
    let c = g.create_node(vec!["C".into()], BTreeMap::new()).unwrap();
    g.create_rel(a, b, "T".into(), BTreeMap::new()).unwrap();
    g.create_rel(b, c, "T".into(), BTreeMap::new()).unwrap();
    g.commit_write();

    let _ = take_path_fold_bindings();
    let implicit = exec_cypher(&g, "MATCH ()-[r]-()-[r]-() RETURN count(*) AS count");
    assert_eq!(implicit.rows, vec![vec![Value::Int(0)]]);
    assert_eq!(take_path_fold_bindings(), 0);

    let repeatable = exec_cypher(
        &g,
        "MATCH REPEATABLE ELEMENTS ()-[r]-()-[r]-() RETURN count(*) AS count",
    );
    assert_eq!(repeatable.rows, vec![vec![Value::Int(4)]]);
    assert_eq!(take_path_fold_bindings(), 0);

    let directed = exec_cypher(
        &g,
        "MATCH (:A)-[e:T]->(:B)-[e]->() RETURN count(*) AS count",
    );
    assert_eq!(directed.rows, vec![vec![Value::Int(0)]]);
    assert_eq!(take_path_fold_bindings(), 0);
}

/// The `__` namespace is reserved against user aliases (OPLANE_REQ-00328162):
/// an aggregate aliased into the `__` namespace makes `fold_return` decline
/// (the uniform reservation every optimized path honors) and the ordinary
/// aggregation answers with the user's column name kept.
#[test]
fn path_fold_declines_a_user_alias_in_the_internal_namespace() {
    let _ = take_path_fold_bindings();
    let built = build(&[&["A"], &["B"]], &[(0, 1, "R")]);
    let r = exec_cypher(
        &built.graph,
        "MATCH (a:A)-[:R]->(b:B) RETURN count(b) AS __agg_base",
    );
    assert_eq!(r.columns, vec!["__agg_base".to_string()]);
    assert_eq!(r.rows, vec![vec![Value::Int(1)]]);
    assert_eq!(take_path_fold_bindings(), 0);
}

/// The zero-argument backtick-escaped `` `count`() `` is not a path-fold
/// operand: it bypasses the parser's keyword path and front validation,
/// so the fold declines it (no bindings counted) and the general path
/// states the contract's arity rejection — the same rejection the
/// one-hop fold's decline yields on the one-hop shape.
#[test]
fn path_fold_declines_the_zero_argument_backtick_count() {
    let built = build(&[&["A"], &["B"], &["C"]], &[(0, 1, "R"), (1, 2, "R")]);
    let _ = take_path_fold_bindings();
    let one_hop = error_of(try_exec_cypher(
        &built.graph,
        "MATCH (:A)-[:R]->(:B) RETURN `count`()",
    ));
    let two_hop = error_of(try_exec_cypher(
        &built.graph,
        "MATCH (:A)-[:R]->()-[:R]->(:C) RETURN `count`()",
    ));
    assert_eq!(one_hop, two_hop);
    assert!(two_hop.contains("InvalidNumberOfArguments"), "{two_hop}");
    assert_eq!(take_path_fold_bindings(), 0);
}

/// Tombstone agreement (ADR-0485's `Whole` operand): a deleted endpoint is
/// unreachable in the fold since a deleted node has no live rels, so the
/// fold and the general path agree across a DETACH DELETE. The pinned
/// shape re-matches the deleted variable and counts nothing; over a graph
/// whose tombstones sit beside a live pair, the fold counts exactly the
/// live endpoint and the general path (materialized through a `WITH`)
/// answers the same.
#[test]
fn path_fold_agrees_with_the_general_path_across_a_detach_delete() {
    // The pinned shape on an all-doomed graph: the re-match of the
    // deleted variable binds nothing.
    let g = Graph::default();
    g.begin_write().unwrap();
    let a = g.create_node(vec!["N".into()], BTreeMap::new()).unwrap();
    let b = g.create_node(vec!["M".into()], BTreeMap::new()).unwrap();
    g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    let deleted = exec_autocommit(
        &g,
        "MATCH (m) DETACH DELETE m WITH m MATCH (n)-[:R]->(m) \
         RETURN count(m), count(DISTINCT m)",
    )
    .unwrap();
    assert_eq!(deleted.rows, vec![vec![Value::Int(0), Value::Int(0)]]);

    // One live pair survives beside the doomed pair's tombstones.
    let g = Graph::default();
    g.begin_write().unwrap();
    let n1 = g.create_node(vec!["N".into()], BTreeMap::new()).unwrap();
    let m1 = g.create_node(vec!["M".into()], BTreeMap::new()).unwrap();
    let n2 = g.create_node(vec!["Dead".into()], BTreeMap::new()).unwrap();
    let m2 = g.create_node(vec!["Dead".into()], BTreeMap::new()).unwrap();
    g.create_rel(n1, m1, "R".into(), BTreeMap::new()).unwrap();
    g.create_rel(n2, m2, "R".into(), BTreeMap::new()).unwrap();
    g.commit_write();
    let deleted = exec_autocommit(
        &g,
        "MATCH (m:Dead) DETACH DELETE m WITH m MATCH (n)-[:R]->(m) \
         RETURN count(m), count(DISTINCT m)",
    )
    .unwrap();
    assert_eq!(deleted.rows, vec![vec![Value::Int(0), Value::Int(0)]]);

    // The fold counts the one live endpoint — the tombstoned pair adds
    // nothing — and the general path agrees.
    let _ = take_path_fold_bindings();
    let folded = exec_cypher(&g, "MATCH (n)-[:R]->(m) RETURN count(m), count(DISTINCT m)");
    assert_eq!(folded.rows, vec![vec![Value::Int(1), Value::Int(1)]]);
    assert_eq!(take_path_fold_bindings(), 1);
    let general = exec_cypher(
        &g,
        "MATCH (n)-[:R]->(m) WITH n, m RETURN count(m), count(DISTINCT m)",
    );
    assert_eq!(general.rows, folded.rows);
}

/// A node variable repeated within one hop (`(x)-[:R]->(x)`) binds one
/// slot, so only a self-loop matches — the walker's mid-walk binding
/// semantics, which the fold's slot classification must agree with. The
/// graph holds a plain pair plus one real self-loop.
fn repeated_variable_graph() -> Graph {
    build(&[&["X"], &["X"], &["X"]], &[(0, 1, "R"), (2, 2, "R")]).graph
}

/// The repeated node variable through the streaming count seats: the fold
/// and the general path answer the same one self-loop.
#[test]
fn path_fold_counts_a_repeated_node_variable_as_a_self_loop() {
    let g = repeated_variable_graph();
    let _ = take_path_fold_bindings();
    let star = exec_cypher(&g, "MATCH (x:X)-[:R]->(x:X) RETURN count(*) AS n");
    assert_eq!(star.rows, vec![vec![Value::Int(1)]]);
    assert!(
        take_path_fold_bindings() > 0,
        "the star count takes the fold"
    );
    let whole = exec_cypher(&g, "MATCH (x:X)-[:R]->(x:X) RETURN count(x) AS n");
    assert_eq!(whole.rows, vec![vec![Value::Int(1)]]);
    assert!(
        take_path_fold_bindings() > 0,
        "the whole count takes the fold"
    );
    // `sum` declines the path fold and lands on the one-hop fold, which
    // never had the hole — its start bind rides the scratch row its end
    // check classifies against.
    let summed = exec_cypher(&g, "MATCH (x:X)-[:R]->(x:X) RETURN sum(x.id) AS s");
    assert_eq!(summed.rows, vec![vec![Value::Int(2)]]);
    // The general path (the walker) answers the same: a value projection
    // reads the bound row, not an aggregate.
    let value = exec_cypher(&g, "MATCH (x:X)-[:R]->(x:X) RETURN x.id AS id");
    assert_eq!(value.rows, vec![vec![Value::Int(2)]]);
    let collected = exec_cypher(&g, "MATCH (x:X)-[:R]->(x:X) RETURN collect(x.id) AS ids");
    assert_eq!(
        collected.rows,
        vec![vec![Value::List(vec![Value::Int(2)].into())]]
    );
}

/// The pre-bound form of the repeated variable: a row-bound `x` accepts
/// only itself, so the plain pair binds nothing and the self-loop binds
/// once — the fold's row classification, unchanged.
#[test]
fn path_fold_repeated_node_variable_stays_self_loop_when_prebound() {
    let g = repeated_variable_graph();
    let _ = take_path_fold_bindings();
    let pair = exec_cypher(
        &g,
        "MATCH (x:X {id: 0}) MATCH (x)-[:R]->(x) RETURN count(*) AS n",
    );
    assert_eq!(pair.rows, vec![vec![Value::Int(0)]]);
    let _ = take_path_fold_bindings();
    let self_loop = exec_cypher(
        &g,
        "MATCH (x:X {id: 2}) MATCH (x)-[:R]->(x) RETURN count(*) AS n",
    );
    assert_eq!(self_loop.rows, vec![vec![Value::Int(1)]]);
    assert!(
        take_path_fold_bindings() > 0,
        "the prebound count takes the fold"
    );
}

/// The EXISTS form keeps answering through the existence probe: false for
/// the plain pair, true for the self-loop.
#[test]
fn exists_probe_repeated_node_variable_needs_a_self_loop() {
    let g = repeated_variable_graph();
    let pair = exec_cypher(
        &g,
        "MATCH (x:X {id: 0}) RETURN EXISTS { (x:X)-[:R]->(x:X) } AS e",
    );
    assert_eq!(pair.rows, vec![vec![Value::Bool(false)]]);
    let self_loop = exec_cypher(
        &g,
        "MATCH (x:X {id: 2}) RETURN EXISTS { (x:X)-[:R]->(x:X) } AS e",
    );
    assert_eq!(self_loop.rows, vec![vec![Value::Bool(true)]]);
}

/// The ADR-0485 two-hop graph: five (user, mid, item) bindings, one item
/// reachable through two mids, one item with no `score`.
fn two_hop_score_graph() -> Graph {
    let g = Graph::default();
    g.begin_write().unwrap();
    let user = |name: &str| {
        g.create_node(
            vec!["User".into()],
            BTreeMap::from([("name".into(), Value::String(name.into()))]),
        )
        .unwrap()
    };
    let u1 = user("u1");
    let u2 = user("u2");
    let x1 = g.create_node(vec!["Mid".into()], BTreeMap::new()).unwrap();
    let x2 = g.create_node(vec!["Mid".into()], BTreeMap::new()).unwrap();
    let item = |score: Option<i64>| {
        let props = match score {
            Some(score) => BTreeMap::from([("score".into(), Value::Int(score))]),
            None => BTreeMap::new(),
        };
        g.create_node(vec!["Item".into()], props).unwrap()
    };
    let m1 = item(Some(10));
    let m2 = item(Some(20));
    let m3 = item(Some(30));
    let m4 = item(None);
    let rel = |a: NodeId, b: NodeId| g.create_rel(a, b, "R".into(), BTreeMap::new()).unwrap();
    rel(u1, x1);
    rel(u2, x2);
    rel(x1, m1);
    rel(x1, m2);
    rel(x2, m2);
    rel(x2, m3);
    rel(x2, m4);
    g.commit_write();
    g
}

/// ADR-0485: the LABS-780 shape — `count(m)` over the bare endpoint of a
/// two-hop pattern — takes the path fold's whole-node slot source and
/// answers the group's binding count.
#[test]
fn path_fold_streams_count_over_a_bare_two_hop_endpoint() {
    let g = two_hop_score_graph();
    let _ = take_path_fold_bindings();
    let result = exec_cypher(&g, "MATCH (n)-[:R]->()-[:R]->(m) RETURN count(m)");
    assert_eq!(result.rows, vec![vec![Value::Int(5)]]);
    assert_eq!(take_path_fold_bindings(), 5);
}

/// The DISTINCT form dedups on the node id through `DistinctAcc`: m2 is
/// reached through both mids but counts once.
#[test]
fn path_fold_streams_count_distinct_over_a_bare_two_hop_endpoint() {
    let g = two_hop_score_graph();
    let _ = take_path_fold_bindings();
    let result = exec_cypher(&g, "MATCH (n)-[:R]->()-[:R]->(m) RETURN count(DISTINCT m)");
    assert_eq!(result.rows, vec![vec![Value::Int(4)]]);
    assert_eq!(take_path_fold_bindings(), 5);
}

/// Grouped on a key slot, the whole-node count still streams: one group
/// per user, each counting its own bindings (and distinct endpoints).
#[test]
fn path_fold_streams_a_grouped_bare_endpoint_count() {
    let g = two_hop_score_graph();
    let _ = take_path_fold_bindings();
    let result = exec_cypher(
        &g,
        "MATCH (n:User)-[:R]->()-[:R]->(m:Item) \
         RETURN n.name AS name, count(m) AS c, count(DISTINCT m) AS dc \
         ORDER BY name",
    );
    assert_eq!(
        result.rows,
        vec![
            vec![Value::String("u1".into()), Value::Int(2), Value::Int(2)],
            vec![Value::String("u2".into()), Value::Int(3), Value::Int(3)],
        ]
    );
    assert_eq!(take_path_fold_bindings(), 5);
}

/// The aggregates the fold does not stream (min/max/avg/sum) keep their
/// correct answers over the same two-hop shape — grouped and keyless,
/// plain and DISTINCT.
#[test]
fn two_hop_min_max_avg_sum_answer_over_the_shape() {
    let g = two_hop_score_graph();
    let keyless = exec_cypher(
        &g,
        "MATCH (n:User)-[:R]->()-[:R]->(m:Item) \
         RETURN min(m.score), max(m.score), avg(m.score), sum(m.score)",
    );
    assert_eq!(
        keyless.rows,
        vec![vec![
            Value::Int(10),
            Value::Int(30),
            Value::Float(20.0),
            Value::Int(80),
        ]]
    );
    let keyless_distinct = exec_cypher(
        &g,
        "MATCH (n:User)-[:R]->()-[:R]->(m:Item) \
         RETURN min(DISTINCT m.score), max(DISTINCT m.score), \
                avg(DISTINCT m.score), sum(DISTINCT m.score)",
    );
    assert_eq!(
        keyless_distinct.rows,
        vec![vec![
            Value::Int(10),
            Value::Int(30),
            Value::Float(20.0),
            Value::Int(60),
        ]]
    );
    let grouped = exec_cypher(
        &g,
        "MATCH (n:User)-[:R]->()-[:R]->(m:Item) \
         RETURN n.name AS name, min(m.score), max(m.score), avg(m.score), sum(m.score) \
         ORDER BY name",
    );
    assert_eq!(
        grouped.rows,
        vec![
            vec![
                Value::String("u1".into()),
                Value::Int(10),
                Value::Int(20),
                Value::Float(15.0),
                Value::Int(30),
            ],
            vec![
                Value::String("u2".into()),
                Value::Int(20),
                Value::Int(30),
                Value::Float(25.0),
                Value::Int(50),
            ],
        ]
    );
    let grouped_distinct = exec_cypher(
        &g,
        "MATCH (n:User)-[:R]->()-[:R]->(m:Item) \
         RETURN n.name AS name, sum(DISTINCT m.score), count(DISTINCT m.score) \
         ORDER BY name",
    );
    assert_eq!(
        grouped_distinct.rows,
        vec![
            vec![Value::String("u1".into()), Value::Int(30), Value::Int(2)],
            vec![Value::String("u2".into()), Value::Int(50), Value::Int(2)],
        ]
    );
}

/// ADR-0541: with the plan's tokens resolved, the fold's label checks and
/// property reads take ids from the plan — `Interner::get_id` calls do not
/// grow with the number of candidates.
#[test]
fn path_fold_lookups_do_not_grow_with_candidates() {
    const QUERY: &str = "MATCH (c:Comment)-[:commentHasCreator]->(p:Person) \
                         WHERE c.content IS NOT NULL AND p.ID = 1 \
                         RETURN COUNT(DISTINCT c.ID) AS n";
    let lookups_for = |comments: i64| {
        let g = Graph::default();
        g.begin_write().unwrap();
        let author = person_named(&g, 1, "Ada", "Local");
        for id in 0..comments {
            let comment = g
                .create_node(
                    vec!["Comment".into()],
                    BTreeMap::from([
                        ("ID".into(), Value::Int(id)),
                        ("content".into(), Value::String("x".into())),
                    ]),
                )
                .unwrap();
            g.create_rel(comment, author, "commentHasCreator".into(), BTreeMap::new())
                .unwrap();
        }
        g.commit_write();
        let _ = crate::store::take_get_id_calls();
        let result = exec_cypher_resolved(&g, QUERY);
        assert_eq!(result.rows, vec![vec![Value::Int(comments)]]);
        crate::store::take_get_id_calls()
    };
    let small = lookups_for(10);
    let large = lookups_for(1000);
    assert_eq!(small, large, "get_id calls grew with candidates");
}

/// ADR-0541: a property key the store never interned reads as absent from
/// the plan's token alone — the equality matches no row and the per-query
/// `get_id` calls (planning, the seek decision) do not grow with the
/// number of candidates, in every predicate shape that carries the key.
#[test]
fn absent_property_key_reads_do_not_grow_with_candidates() {
    for query in [
        "MATCH (p:Person) WHERE p.ghost = 1 RETURN p",
        "MATCH (p:Person) WHERE p.ghost IS NOT NULL RETURN p",
        "MATCH (p:Person {ghost: 1}) RETURN p",
        "MATCH (p:Person) WHERE p.ghost = 1 AND p.ID > 0 RETURN p.ID",
        "MATCH (p:Person) WHERE p.ghost = 1 RETURN count(p.ghost)",
    ] {
        let lookups_for = |people: i64| {
            let g = Graph::default();
            g.begin_write().unwrap();
            for id in 0..people {
                person_named(&g, id, "Ada", "Local");
            }
            g.commit_write();
            let _ = crate::store::take_get_id_calls();
            let result = exec_cypher_resolved(&g, query);
            assert!(
                result.rows.iter().all(|row| row[0] == Value::Int(0)) || result.rows.is_empty()
            );
            crate::store::take_get_id_calls()
        };
        assert_eq!(lookups_for(10), lookups_for(500), "{query}");
    }
}

/// ADR-0541: a relationship property equality and a relationship or node
/// pattern map on a key the store never interned match no row with the
/// same `get_id` count at 10 and 500 candidates — the pattern-map keys
/// and the relationship read carry the plan's token.
#[test]
fn absent_rel_and_pattern_map_keys_do_not_grow_with_candidates() {
    for query in [
        "MATCH (:Person)-[r:KNOWS]->(:Person) WHERE r.ghost = 1 RETURN r",
        "MATCH (:Person)-[r:KNOWS {ghost: 1}]->(:Person) RETURN r",
        "MATCH (a:Person)-[:KNOWS]->(b:Person {ghost: 1}) RETURN a",
    ] {
        let lookups_for = |people: i64| {
            let g = Graph::default();
            g.begin_write().unwrap();
            let first = person_named(&g, 0, "Ada", "Local");
            for id in 1..=people {
                let next = person_named(&g, id, "Ada", "Local");
                g.create_rel(
                    first,
                    next,
                    "KNOWS".into(),
                    BTreeMap::from([("since".into(), Value::Int(id))]),
                )
                .unwrap();
            }
            g.commit_write();
            let _ = crate::store::take_get_id_calls();
            let result = exec_cypher_resolved(&g, query);
            assert!(result.rows.is_empty(), "{query}");
            crate::store::take_get_id_calls()
        };
        assert_eq!(lookups_for(10), lookups_for(500), "{query}");
    }
}
