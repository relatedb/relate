use super::*;
use crate::push::collect::{LocalNode, LocalRel};
use crate::push::policy::PolicySpec;
use relate::Value;

fn keys() -> MergeKeys {
    let mut nodes = BTreeMap::new();
    nodes.insert("Person".to_string(), vec!["name".to_string()]);
    nodes.insert(
        "Event".to_string(),
        vec!["tenant".to_string(), "code".to_string()],
    );
    let mut rels = BTreeMap::new();
    rels.insert("SPOKE_AT".to_string(), vec!["slot".to_string()]);
    MergeKeys { nodes, rels }
}

fn person(id: i64, name: &str) -> LocalNode {
    LocalNode {
        id,
        labels: vec!["Person".into()],
        properties: BTreeMap::from([
            ("name".to_string(), Value::String(name.into())),
            ("age".to_string(), Value::Int(40)),
        ]),
    }
}

fn spoke_at(start: i64, end: i64) -> LocalRel {
    LocalRel {
        rel_type: "SPOKE_AT".into(),
        start,
        end,
        properties: BTreeMap::from([
            ("slot".to_string(), Value::Int(3)),
            ("note".to_string(), Value::String("keynote".into())),
        ]),
    }
}

fn event(id: i64, code: &str) -> LocalNode {
    LocalNode {
        id,
        labels: vec!["Event".into()],
        properties: BTreeMap::from([
            ("tenant".to_string(), Value::String("acme".into())),
            ("code".to_string(), Value::String(code.into())),
        ]),
    }
}

fn opts() -> PlanOpts {
    PlanOpts {
        nodes_update: PolicySpec::default_spec(),
        rels_update: PolicySpec::default_spec(),
        rel_mode: RelMode::Keyed,
        unkeyed_labels: UnkeyedAction::Skip,
    }
}

fn graph(nodes: Vec<LocalNode>, rels: Vec<LocalRel>) -> ScopeGraph {
    ScopeGraph {
        nodes,
        rels,
        dangling_rels: 0,
    }
}

/// A node group's identity, destructured: `(labels, keyed label, policy)`.
fn node_label_of(entity: &PlannedEntity) -> (&[String], &str, UpdatePolicy) {
    match &entity.kind {
        EntityKind::NodeLabel {
            labels,
            keyed_label,
            policy,
            ..
        } => (labels, keyed_label, *policy),
        other => panic!("a keyed node group, got {other:?}"),
    }
}

/// A written rel group, destructured: `(type, start label, start key,
/// end label, end key, policy)`.
#[allow(clippy::type_complexity)]
fn rel_of(entity: &PlannedEntity) -> (&str, &str, &[String], &str, &[String], UpdatePolicy) {
    match &entity.kind {
        EntityKind::Rel {
            rel_type,
            start_label,
            start_key,
            end_label,
            end_key,
            policy,
            ..
        } => (
            rel_type,
            start_label,
            start_key,
            end_label,
            end_key,
            *policy,
        ),
        other => panic!("a written rel group, got {other:?}"),
    }
}

/// The statement of an entity the plan writes.
fn statement_of(entity: &PlannedEntity) -> &str {
    entity.kind.statement().expect("a writing entity")
}

/// The plan groups nodes by their full sorted label set (the merge
/// identity is the set's keyed label — see
/// `secondary_labels_form_their_own_groups_and_survive` for the
/// multi-label case) and rels by `(type, start label, end label)`,
/// keys come from the endpoint nodes' own properties, and node groups
/// precede rel groups.
#[test]
fn plan_groups_nodes_then_rels_with_resolved_keys() {
    let g = graph(
        vec![person(1, "Ada"), person(2, "Bob"), event(3, "gql")],
        vec![spoke_at(1, 3), spoke_at(2, 3)],
    );
    let plan = build_plan(&g, &keys(), &opts()).unwrap();
    assert_eq!(plan.entities.len(), 3, "{plan:?}");
    // Keyed node groups in sorted label order: Event, Person, then rels.
    let events = &plan.entities[0];
    assert_eq!(
        node_label_of(events),
        (&["Event".to_string()][..], "Event", UpdatePolicy::Add)
    );
    assert_eq!(events.row_count(), 1);
    assert_eq!(events.merge_key, ["tenant".to_string(), "code".to_string()]);
    let people = &plan.entities[1];
    assert_eq!(
        node_label_of(people),
        (&["Person".to_string()][..], "Person", UpdatePolicy::Add)
    );
    assert_eq!(people.row_count(), 2);
    assert_eq!(people.merge_key, ["name".to_string()]);
    let rel = &plan.entities[2];
    assert_eq!(
        rel_of(rel),
        (
            "SPOKE_AT",
            "Person",
            &["name".to_string()][..],
            "Event",
            &["tenant".to_string(), "code".to_string()][..],
            UpdatePolicy::Add
        )
    );
    assert_eq!(rel.row_count(), 2);
    // The first rel row carries locally resolved endpoint keys, and —
    // under the keyed default with the type's key resolved — the rel
    // key properties too.
    let first_row = rel.rows().first().expect("a rel row");
    let map = |field| first_row.identity_map(field).expect("a keyed rel row");
    assert_eq!(
        map(IdentityField::Start)["name"],
        Value::String("Ada".into())
    );
    assert_eq!(map(IdentityField::End)["code"], Value::String("gql".into()));
    assert_eq!(map(IdentityField::Key)["slot"], Value::Int(3));
}

/// Secondary labels survive the push (LABS-907): the group is the
/// node's full label set, the statement writes every label, and two
/// nodes sharing the keyed label but carrying different secondaries
/// are separate groups. Rel endpoints still resolve through the
/// keyed label alone — the write created the node carrying it.
#[test]
fn secondary_labels_form_their_own_groups_and_survive() {
    let mut session_chunk = person(1, "Ada");
    session_chunk.labels = vec!["Chunk".into(), "SessionChunk".into()];
    let mut plain_chunk = person(2, "Bob");
    plain_chunk.labels = vec!["Chunk".into()];
    // Person keyed; Chunk is not — the keyed label is Person for both.
    session_chunk.labels.push("Person".into());
    plain_chunk.labels.push("Person".into());
    let g = graph(vec![session_chunk, plain_chunk], vec![spoke_at(1, 2)]);
    let plan = build_plan(&g, &keys(), &opts()).unwrap();
    // Two node groups (one per label set) plus the rel group.
    assert_eq!(plan.entities.len(), 3, "{plan:?}");
    let first = &plan.entities[0];
    assert_eq!(
        node_label_of(first),
        (
            &["Chunk".to_string(), "Person".to_string()][..],
            "Person",
            UpdatePolicy::Add
        )
    );
    // The merge is on the KEYED label; the secondary is SET onto the
    // node it found — an update of the labels, never a fork.
    assert!(
        statement_of(first).contains("MERGE (n:`Person` {`name`: row.key.`name`}) SET n:`Chunk`,"),
        "merge on the keyed label, set the secondary: {}",
        first.kind.statement().unwrap_or_default()
    );
    let second = &plan.entities[1];
    assert_eq!(
        node_label_of(second),
        (
            &[
                "Chunk".to_string(),
                "Person".to_string(),
                "SessionChunk".to_string()
            ][..],
            "Person",
            UpdatePolicy::Add
        )
    );
    assert!(
        statement_of(second)
            .contains("MERGE (n:`Person` {`name`: row.key.`name`}) SET n:`Chunk`:`SessionChunk`,"),
        "merge on the keyed label, set both secondaries: {}",
        second.kind.statement().unwrap_or_default()
    );
    // The rel resolves its endpoints through the keyed label.
    let rel = &plan.entities[2];
    assert_eq!(
        rel_of(rel),
        (
            "SPOKE_AT",
            "Person",
            &["name".to_string()][..],
            "Person",
            &["name".to_string()][..],
            UpdatePolicy::Add
        )
    );
}

/// A node with several labels groups under the first of its sorted
/// labels that carries a key; an unkeyed label is skipped and counted
/// by default.
#[test]
fn unkeyed_labels_are_skipped_and_counted_by_default() {
    let mut unkeyed = person(9, "no-key");
    unkeyed.labels = vec!["Untyped".into()];
    let g = graph(vec![person(1, "Ada"), unkeyed], vec![]);
    let plan = build_plan(&g, &keys(), &opts()).unwrap();
    assert_eq!(plan.entities.len(), 2, "{plan:?}");
    let skipped = &plan.entities[1];
    match &skipped.kind {
        EntityKind::NodeSkipped { labels } => assert_eq!(labels, &["Untyped".to_string()]),
        other => panic!("a skipped node bucket, got {other:?}"),
    }
    assert_eq!(skipped.skipped_unkeyed, 1);
    assert_eq!(skipped.row_count(), 0);
}

/// `--unkeyed-labels create` switches unkeyed labels to CREATE rows
/// (non-idempotent) and a labelless node lands in the reserved
/// labelless group.
#[test]
fn unkeyed_labels_create_switches_to_create_rows() {
    let mut unkeyed = person(9, "no-key");
    unkeyed.labels = vec!["Untyped".into()];
    let mut bare = person(10, "bare");
    bare.labels = vec![];
    let mut o = opts();
    o.unkeyed_labels = UnkeyedAction::Create;
    let g = graph(vec![unkeyed, bare], vec![]);
    let plan = build_plan(&g, &keys(), &o).unwrap();
    let create: Vec<_> = plan
        .entities
        .iter()
        .filter(|e| matches!(e.kind, EntityKind::NodeCreate { .. }))
        .collect();
    assert_eq!(create.len(), 2, "{plan:?}");
    match &create[0].kind {
        EntityKind::NodeCreate {
            labels,
            policy,
            rows,
            ..
        } => {
            assert!(labels.is_empty(), "the labelless group");
            assert_eq!(*policy, UpdatePolicy::Add);
            assert_eq!(rows.len(), 1);
        }
        other => panic!("a CREATE node group, got {other:?}"),
    }
    assert_eq!(
        create[0].kind.statement(),
        Some("UNWIND $rows AS row CREATE (n) SET n += row.props")
    );
    assert_eq!(
        create[1].kind.statement(),
        Some("UNWIND $rows AS row CREATE (n:`Untyped`) SET n += row.props")
    );
}

/// A rel whose endpoint label is unkeyed is skipped and counted on the
/// rel's group — never written, never silently dropped.
#[test]
fn rels_with_unkeyed_endpoints_are_counted_not_written() {
    let mut unkeyed = person(9, "no-key");
    unkeyed.labels = vec!["Untyped".into()];
    let g = graph(
        vec![person(1, "Ada"), unkeyed.clone(), event(3, "gql")],
        vec![spoke_at(1, 9), spoke_at(9, 3), spoke_at(1, 3)],
    );
    let plan = build_plan(&g, &keys(), &opts()).unwrap();
    // Three groups, discriminated by kind where it is decided:
    // (Person, Untyped) and (Untyped, Event) are RelSkipped buckets;
    // (Person, Event) is the one written Rel.
    let written: Vec<_> = plan
        .entities
        .iter()
        .filter(|e| matches!(e.kind, EntityKind::Rel { .. }))
        .collect();
    let counted: Vec<_> = plan
        .entities
        .iter()
        .filter(|e| matches!(e.kind, EntityKind::RelSkipped { .. }))
        .collect();
    assert_eq!(written.len(), 1, "{plan:?}");
    assert_eq!(written[0].row_count(), 1);
    assert_eq!(counted.len(), 2, "{plan:?}");
    let skipped: usize = counted.iter().map(|e| e.skipped_unkeyed).sum();
    assert_eq!(skipped, 2);
}

/// The keyed default merges on the resolved relationship key
/// (constraint- or `--rel-key`-derived) and falls back to structural
/// for a type with no key; the structural force ignores a key.
#[test]
fn keyed_mode_uses_the_relationship_key_and_falls_back() {
    let g = graph(
        vec![person(1, "Ada"), event(3, "gql")],
        vec![spoke_at(1, 3)],
    );
    let plan = build_plan(&g, &keys(), &opts()).unwrap();
    let rel = &plan.entities[2];
    assert_eq!(rel.merge_key, ["slot".to_string()]);
    assert!(statement_of(rel).contains("MERGE (a)-[r:`SPOKE_AT` {`slot`: row.key.`slot`}]->(b)"));
    let first_row = rel.rows().first().expect("a rel row");
    let key = first_row
        .identity_map(IdentityField::Key)
        .expect("a keyed rel row");
    assert_eq!(key["slot"], Value::Int(3));
    // A type with no resolved key falls back to structural (no error).
    let mut rel = spoke_at(1, 3);
    rel.rel_type = "WROTE_ABT".into();
    let g2 = graph(vec![person(1, "Ada"), event(3, "gql")], vec![rel]);
    let plan2 = build_plan(&g2, &keys(), &opts()).unwrap();
    let rel2 = &plan2.entities[2];
    assert_eq!(rel2.merge_key, Vec::<String>::new());
    assert!(
        rel2.rows()
            .first()
            .is_some_and(|row| row.identity_map(IdentityField::Key).is_none()),
        "the structural fallback writes pair rows"
    );
    assert!(statement_of(rel2).contains("MERGE (a)-[r:`WROTE_ABT`]->(b)"));
    // The structural force ignores a resolved key.
    let mut o = opts();
    o.rel_mode = RelMode::Structural;
    let plan3 = build_plan(&g, &keys(), &o).unwrap();
    let rel3 = &plan3.entities[2];
    assert!(statement_of(rel3).contains("MERGE (a)-[r:`SPOKE_AT`]->(b)"));
}

/// A node whose merge-key property is missing fails closed, naming the
/// label and property.
#[test]
fn missing_key_property_fails_closed() {
    let mut broken = person(5, "nobody");
    broken.properties.remove("name");
    let g = graph(vec![broken], vec![]);
    let err = build_plan(&g, &keys(), &opts()).unwrap_err();
    assert!(err.to_string().contains("merge-key property"), "{err}");
    assert!(err.to_string().contains("name"), "{err}");
    assert!(err.to_string().contains("Person"), "{err}");
}

/// A dangling rel (endpoint not in the scope) is attributed to the
/// reserved dangling bucket, not silently dropped.
#[test]
fn dangling_rels_land_in_the_reserved_bucket() {
    // finish_graph keeps dangling rels out of the row set (they cannot
    // resolve endpoints), so the plan sees only the counter.
    let mut g = graph(vec![person(1, "Ada")], vec![]);
    g.dangling_rels = 1;
    let plan = build_plan(&g, &keys(), &opts()).unwrap();
    let dangling: Vec<_> = plan
        .entities
        .iter()
        .filter(|e| matches!(&e.kind, EntityKind::Dangling))
        .collect();
    assert_eq!(dangling.len(), 1, "{plan:?}");
    assert_eq!(dangling[0].skipped_dangling, 1);
    assert_eq!(
        dangling[0].kind.statement(),
        None,
        "a counted bucket carries no statement"
    );
}

/// Keyedness is part of the rel group's identity: a keyed label
/// literally named `A:B` and an unkeyed {A, B} label set are
/// DIFFERENT endpoints (a joined spelling would collide them, and a
/// colliding group's keyedness would come from whichever rel arrived
/// first — the later rel could then panic on valid data).
#[test]
fn keyedness_is_part_of_the_rel_group_identity() {
    // A keyed node whose whole label is the odd spelling `A:B`.
    let mut odd = person(7, "Zoe");
    odd.labels = vec!["A:B".into()];
    // Person is keyed; a rel with a keyed :A:B start endpoint, and
    // one with an unkeyed {A, B} start endpoint.
    let mut unkeyed_start = person(8, "Mel");
    unkeyed_start.labels = vec!["A".into(), "B".into()];
    let mut rel_odd = spoke_at(7, 1);
    rel_odd.rel_type = "LINKED".into();
    let mut rel_unkeyed = spoke_at(8, 1);
    rel_unkeyed.rel_type = "LINKED".into();
    let g = graph(
        vec![person(1, "Ada"), odd, unkeyed_start],
        vec![rel_odd, rel_unkeyed],
    );
    // The odd spelling is a keyed label (its own merge key); the
    // {A, B} set carries none. `unkeyed_labels: Skip` counts the
    // unkeyed endpoint's rel, never written — and neither group
    // touches the other.
    let mut local_keys = keys();
    local_keys
        .nodes
        .insert("A:B".to_string(), vec!["name".to_string()]);
    let plan = build_plan(&g, &local_keys, &opts()).unwrap();
    // The kinds say which is which — the written Rel and the counted
    // RelSkipped, never a rows-shape proxy and never the emission
    // order.
    let written: Vec<_> = plan
        .entities
        .iter()
        .filter(|e| matches!(&e.kind, EntityKind::Rel { .. }))
        .collect();
    let counted: Vec<_> = plan
        .entities
        .iter()
        .filter(|e| matches!(&e.kind, EntityKind::RelSkipped { .. }))
        .collect();
    assert_eq!(
        written.len(),
        1,
        "one written group, the keyed-endpoint rel: {plan:?}"
    );
    assert_eq!(written[0].row_count(), 1);
    assert_eq!(
        counted.len(),
        1,
        "one counted group, the unkeyed-endpoint rel: {plan:?}"
    );
    assert_eq!(
        counted[0].skipped_unkeyed, 1,
        "the unkeyed-endpoint rel is counted, not written"
    );
}

/// Per-entity policies resolve onto the groups they name.
#[test]
fn per_entity_policies_reach_their_groups() {
    let g = graph(vec![person(1, "Ada"), event(3, "gql")], vec![]);
    let mut o = opts();
    o.nodes_update = PolicySpec::parse("Event=replace").unwrap();
    let plan = build_plan(&g, &keys(), &o).unwrap();
    assert_eq!(plan.entities[0].kind.policy(), Some(UpdatePolicy::Replace)); // Event
    assert_eq!(plan.entities[1].kind.policy(), Some(UpdatePolicy::Add)); // Person
    assert!(statement_of(&plan.entities[0]).contains("SET n = row.props"));
}

/// A counted bucket writes nothing, so it carries no policy — never a
/// fabricated one (LABS-932). The three counted kinds (unkeyed-skip node,
/// unkeyed-endpoint rel, dangling) all report `None`, while every writing
/// kind reports `Some`.
#[test]
fn counted_buckets_carry_no_policy() {
    let mut unkeyed = person(2, "no-key");
    unkeyed.labels = vec!["Untyped".into()];
    let mut g = graph(
        vec![person(1, "Ada"), unkeyed, event(3, "gql")],
        vec![spoke_at(1, 3), spoke_at(2, 3)],
    );
    g.dangling_rels = 4;
    let plan = build_plan(&g, &keys(), &opts()).unwrap();

    let policy_of = |pred: fn(&EntityKind) -> bool| {
        plan.entities
            .iter()
            .find(|e| pred(&e.kind))
            .unwrap_or_else(|| panic!("no entity matched: {plan:?}"))
            .kind
            .policy()
    };
    assert_eq!(
        policy_of(|k| matches!(k, EntityKind::NodeLabel { .. })),
        Some(UpdatePolicy::Add),
        "a keyed node group carries its policy"
    );
    assert_eq!(
        policy_of(|k| matches!(k, EntityKind::NodeSkipped { .. })),
        None,
        "the unkeyed-skip bucket writes nothing and carries no policy"
    );
    assert_eq!(
        policy_of(|k| matches!(k, EntityKind::RelSkipped { .. })),
        None,
        "the unkeyed-endpoint rel bucket writes nothing and carries no policy"
    );
    assert_eq!(
        policy_of(|k| matches!(k, EntityKind::Dangling)),
        None,
        "the dangling bucket writes nothing and carries no policy"
    );
}

#[test]
fn display_names_cover_every_kind() {
    let kind_name = |kind: EntityKind| kind.display_name();
    assert_eq!(
        kind_name(EntityKind::NodeLabel {
            labels: vec!["Movie".into()],
            keyed_label: "Movie".into(),
            policy: UpdatePolicy::Add,
            statement: String::new(),
            rows: Vec::new(),
        }),
        "Movie"
    );
    assert_eq!(
        kind_name(EntityKind::NodeCreate {
            labels: vec!["Untyped".into()],
            policy: UpdatePolicy::Add,
            statement: String::new(),
            rows: Vec::new(),
        }),
        "Untyped"
    );
    assert_eq!(
        kind_name(EntityKind::NodeSkipped {
            labels: vec!["Untyped".into()]
        }),
        "Untyped (unkeyed)"
    );
    assert_eq!(
        kind_name(EntityKind::Rel {
            rel_type: "ACTED_IN".into(),
            start_label: "Person".into(),
            start_key: vec!["name".into()],
            end_label: "Movie".into(),
            end_key: vec!["title".into()],
            policy: UpdatePolicy::Add,
            statement: String::new(),
            rows: Vec::new(),
        }),
        "(Person)-[:ACTED_IN]->(Movie)"
    );
    assert_eq!(
        kind_name(EntityKind::RelSkipped {
            rel_type: "SPOKE_AT".into(),
            start: "Person".into(),
            end: "Untyped".into(),
        }),
        "(Person)-[:SPOKE_AT]->(Untyped) (unkeyed)"
    );
    assert_eq!(kind_name(EntityKind::Dangling), "(dangling)");
}

/// The identity layout is the one derivation the pre-read columns and the
/// row identity share: a keyed node group reads its key; a keyed rel
/// group reads start, end, then rel keys; an unkeyed CREATE group and the
/// counted buckets have none. A pair rel row's identity is its endpoints.
#[test]
fn identity_layout_is_one_derivation_for_pre_read_and_row_identity() {
    let g = graph(
        vec![person(1, "Ada"), event(3, "gql")],
        vec![spoke_at(1, 3)],
    );
    let plan = build_plan(&g, &keys(), &opts()).unwrap();
    let rel = &plan.entities[2];
    let layout = rel.identity_layout().expect("a keyed rel group");
    assert_eq!(layout.column_count(), 1 + 2 + 1);
    assert_eq!(layout.candidate_var, "r");
    let row = rel.rows().first().unwrap();
    let values = layout.row_values("rel", row).unwrap();
    assert_eq!(
        values,
        [
            &Value::String("Ada".into()),
            &Value::String("acme".into()),
            &Value::String("gql".into()),
            &Value::Int(3)
        ]
    );
    let node = &plan.entities[1];
    assert_eq!(node.identity_layout().unwrap().column_count(), 1);
    // A structural pair group has an empty rel-key run: endpoints only.
    let mut o = opts();
    o.rel_mode = RelMode::Structural;
    let pair_plan = build_plan(&g, &keys(), &o).unwrap();
    let pair = &pair_plan.entities[2];
    let pair_layout = pair.identity_layout().unwrap();
    assert_eq!(pair_layout.column_count(), 3);
    assert_eq!(
        pair_layout
            .row_values("rel", pair.rows().first().unwrap())
            .unwrap()
            .len(),
        3
    );
    // The unkeyed CREATE group has no identity layout.
    let mut bare = person(9, "x");
    bare.labels = vec!["Untyped".into()];
    let mut o = opts();
    o.unkeyed_labels = UnkeyedAction::Create;
    let created = build_plan(&graph(vec![bare], vec![]), &keys(), &o).unwrap();
    assert!(created.entities[0].identity_layout().is_none());
}
