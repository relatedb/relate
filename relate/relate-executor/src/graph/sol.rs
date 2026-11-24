//! Speed-of-light research kernels — research-only (speed-of-light
//! kernels): compiled only under the `research` feature, not engine
//! behavior.
//!
//! Hand-written plain-Rust implementations of three graph-benchmark-ldbc
//! queries (q8, q19, q30) over the live store, for measuring the gap
//! between the clause-at-a-time interpreter and direct loops over the
//! typed adjacency. Not engine behavior, not a product surface, not a
//! test gate: quarantined behind the non-default `research` cargo
//! feature (ADR-0476) — absent from every default build, every bundle,
//! and the WASM build.
//!
//! Fidelity notes (what the kernels replicate and what they assume):
//!
//! - Every version chain is read at its LIVE version (`xmax == 0`). The
//!   research graph is quiescent — loaded once, then read-only with no
//!   concurrent writers — so the live version is the snapshot-visible one.
//! - Adjacency chunks are superset lists: each hop's rel is resolved,
//!   checked live, and endpoint-checked, the same checks the executor's
//!   `hop` applies. Label-index candidates are label-checked the way the
//!   engine's scan rechecks them.
//! - q8 compares the birthday STRINGS lexicographically. The data set's
//!   StringDateFormatter birthdays are padded ISO `YYYY-MM-DD`, where
//!   lexicographic order equals `date()` order; the harness verifies this
//!   against the engine's answer (and with a differential count query).
//! - q19's grouping key is the place NAME (the projected value), matching
//!   the engine's GROUP BY on `l.name`, not the place's node id; a place
//!   with no `name` property groups under null exactly as the engine's
//!   projection does (this data set has none).
//! - No per-row `HashSet<RelId>` (the patterns are fixed-length hops, so
//!   rel-uniqueness needs no set), no `Row` materialization, no
//!   tree-walking expression evaluation, no per-walk `dyn BindingSink`.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use relate_core::{NodeId, Value};

use super::research::{live_neighbors, with_node};
use super::Graph;
use relate_ir::Direction;

/// One kernel's answer in the engine-result shape the answers checker
/// already compares: column names plus rows of values.
pub struct SolRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// The label index's candidate ids for interned label `lid`, copied out
/// under one short guard — the same set the engine's label scan copies.
fn label_candidates(graph: &Graph, lid: u32) -> Vec<NodeId> {
    match graph.indexes.label_index.get(&lid) {
        Some(set) => set.iter().copied().collect(),
        None => Vec::new(),
    }
}

/// Whether `node` carries interned label `lid` and its `kid` property is a
/// string containing `needle` (the engine's `CONTAINS`: a missing or
/// non-string property is null, which filters the row out). The string
/// discrimination is core's canonical `Value::as_string` accessor.
fn carries_label_and_contains(
    graph: &Graph,
    node: NodeId,
    lid: u32,
    kid: u32,
    needle: &str,
) -> bool {
    with_node(graph, node, |n| {
        n.labels.contains(&lid)
            && n.props
                .get(kid)
                .and_then(Value::as_string)
                .is_some_and(|s| s.contains(needle))
    })
    .unwrap_or(false)
}

/// Whether `node` carries interned label `lid` and its `kid` property is a
/// padded-ISO string strictly after `cutoff` (the engine's
/// `date(p.birthday) > date("1990-01-01")` on this data set).
fn birthday_after(graph: &Graph, node: NodeId, lid: u32, kid: u32, cutoff: &str) -> bool {
    with_node(graph, node, |n| {
        n.labels.contains(&lid)
            && n.props
                .get(kid)
                .and_then(Value::as_string)
                .is_some_and(|s| s > cutoff)
    })
    .unwrap_or(false)
}

/// Whether `node` carries interned label `lid` — the engine's candidate
/// recheck, the one label check the kernels' walked endpoints apply (an
/// absent node fails it, like every other check).
fn carries_label(graph: &Graph, node: NodeId, lid: u32) -> bool {
    with_node(graph, node, |n| n.labels.contains(&lid)).unwrap_or(false)
}

fn empty(columns: &[&str]) -> SolRows {
    SolRows {
        columns: columns.iter().map(|c| c.to_string()).collect(),
        rows: Vec::new(),
    }
}

/// q19's two-column schema, spelled once: every guard leg's empty-result
/// shape and the final rows' header read it, so adding a projection
/// column is a one-line edit.
const Q19_COLUMNS: &[&str] = &["l.name", "comment_count"];

/// q19's grouping state: the count per NAME group (an `IndexMap` —
/// insertion order is the groups' first-seen order, the same
/// insertion-ordered mapping the executor's own GROUP BY folds through),
/// and the per-place name memo (each place's name is read once).
struct Q19Groups {
    counts: IndexMap<Option<String>, i64>,
    names: HashMap<NodeId, Option<String>>,
}

/// One place's NAME grouping key — the engine's projected `l.name`:
/// `Some` for a string name, `None` for a missing or non-string name
/// (the engine's null group, which this data set never produces — noted
/// in the module docs). An absent node is loud, never a fabricated
/// empty-string key: the walk reached the place through live,
/// endpoint-checked adjacency in a quiescent store, so a missing node
/// is an invariant violation.
fn place_name(graph: &Graph, place: NodeId, name_kid: u32) -> Option<String> {
    with_node(graph, place, |n| {
        n.props
            .get(name_kid)
            .and_then(Value::as_string)
            .map(str::to_string)
    })
    .expect("q19 kernel: a walked place is live (adjacency is endpoint-checked)")
}

/// Count one row for `place`: the Place label recheck (the engine's
/// candidate recheck — a non-Place endpoint produces no row, it is not
/// a null-name group), then the memoized NAME grouping key bumped in
/// the insertion-ordered count map (first sighting = first-seen order).
fn bump(graph: &Graph, place: NodeId, place_lid: u32, name_kid: u32, groups: &mut Q19Groups) {
    if !carries_label(graph, place, place_lid) {
        return;
    }
    let name = groups
        .names
        .entry(place)
        .or_insert_with(|| place_name(graph, place, name_kid))
        .clone();
    *groups.counts.entry(name).or_insert(0) += 1;
}

/// q19: `MATCH (c:Comment)-[:commentHasTag]->(t:Tag),
/// (c)-[:commentIsLocatedIn]->(l:Place) WHERE t.name CONTAINS "Copernicus"
/// RETURN l.name, COUNT(c.ID) AS comment_count ORDER BY comment_count DESC
/// LIMIT 1`.
///
/// `tag_first` starts from the Tag scan (the selective side of the
/// filter) and walks `commentHasTag` backwards; `!tag_first` scans
/// Comments — the engine's plan shape — and walks out to the tag and on
/// to the place. Both count one row per (comment, matching tag, place)
/// triple, the rows the engine's pattern produces.
///
/// Returns every group ordered by count DESC (ties: first-seen order) —
/// the harness applies the query's `LIMIT 1`.
pub fn q19(graph: &Graph, tag_first: bool) -> SolRows {
    let (Some(tag_lid), Some(comment_lid), Some(place_lid)) = (
        graph.label_id("Tag"),
        graph.label_id("Comment"),
        graph.label_id("Place"),
    ) else {
        return empty(Q19_COLUMNS);
    };
    // `name` is the one property read (the Tag filter and the place
    // grouping key). `ID` is never read — COUNT(c.ID) is the row count,
    // every counted row's comment carrying a non-null ID on this data
    // set — but the kernel still guards that the key is interned, the shape
    // the row-count-as-count claim assumes.
    let Some(name_kid) = graph.prop_key_id("name") else {
        return empty(Q19_COLUMNS);
    };
    if graph.prop_key_id("ID").is_none() {
        return empty(Q19_COLUMNS);
    }
    let (Some(has_tag), Some(located_in)) = (
        graph.rel_type_id("commentHasTag"),
        graph.rel_type_id("commentIsLocatedIn"),
    ) else {
        return empty(Q19_COLUMNS);
    };
    const NEEDLE: &str = "Copernicus";

    let mut groups = Q19Groups {
        counts: IndexMap::new(),
        names: HashMap::new(),
    };
    // Reused hop buffers.
    let (mut tags, mut comments, mut places) = (Vec::new(), Vec::new(), Vec::new());

    // The fixed second half of the pattern — the comment→place hop and
    // the grouping — shared by both plan shapes; the arms below differ
    // only in the scan side, the filter placement, and the first hop's
    // direction, the contrast the kernel exists to measure.
    let mut walk_places = |comment: NodeId| {
        live_neighbors(graph, comment, located_in, Direction::Outgoing, &mut places);
        for place in places.iter().copied() {
            bump(graph, place, place_lid, name_kid, &mut groups);
        }
    };

    // Count one row per (comment, matching tag, place) hop triple.
    if tag_first {
        for tag in label_candidates(graph, tag_lid) {
            if !carries_label_and_contains(graph, tag, tag_lid, name_kid, NEEDLE) {
                continue;
            }
            // (c)-[:commentHasTag]->(t): c is each rel's start.
            live_neighbors(graph, tag, has_tag, Direction::Incoming, &mut comments);
            for comment in comments.iter().copied() {
                if !carries_label(graph, comment, comment_lid) {
                    continue;
                }
                walk_places(comment);
            }
        }
    } else {
        for comment in label_candidates(graph, comment_lid) {
            live_neighbors(graph, comment, has_tag, Direction::Outgoing, &mut tags);
            for tag in tags.iter().copied() {
                if !carries_label_and_contains(graph, tag, tag_lid, name_kid, NEEDLE) {
                    continue;
                }
                walk_places(comment);
            }
        }
    }

    let mut ranked: Vec<(Option<String>, i64)> = groups
        .counts
        .iter()
        .map(|(name, count)| (name.clone(), *count))
        .collect();
    // ORDER BY comment_count DESC; ties keep first-seen order.
    ranked.sort_by_key(|a| std::cmp::Reverse(a.1));

    SolRows {
        columns: Q19_COLUMNS.iter().map(|c| c.to_string()).collect(),
        rows: ranked
            .into_iter()
            .map(|(name, count)| {
                vec![
                    name.map(|s| Value::String(s.into())).unwrap_or(Value::Null),
                    Value::Int(count),
                ]
            })
            .collect(),
    }
}

/// q8's one-column schema, spelled once (the `Q19_COLUMNS` twin): every
/// guard leg's empty-result shape and the final rows' header read it.
const Q8_COLUMNS: &[&str] = &["p.ID"];

/// Record the person's ID once (DISTINCT), first-seen order. The
/// caller's birthday gate has already verified the Person label and a
/// live node — no label recheck here, unlike q19's `bump`, whose
/// callers do not guarantee the label.
fn record_person_id(
    graph: &Graph,
    person: NodeId,
    id_kid: u32,
    seen: &mut HashSet<NodeId>,
    distinct: &mut Vec<i64>,
) {
    if !seen.insert(person) {
        return;
    }
    let id = with_node(graph, person, |n| {
        // The Int variant itself, not core's `as_int`: that accessor is
        // a toInteger-ish coercion (an integral Float coerces to Int),
        // while the kernel must discriminate the exact variant the
        // engine projects.
        n.props.get(id_kid).and_then(|v| match v {
            Value::Int(i) => Some(*i),
            _ => None,
        })
    })
    .expect("q8 kernel: a birthday-filtered person is live");
    // A person without an ID property would project null; this data set
    // always carries one (noted in the module docs).
    if let Some(id) = id {
        distinct.push(id);
    }
}

/// q8: `MATCH (p:Person)<-[:hasModerator]-(f:Forum)
/// WHERE date(p.birthday) > date("1990-01-01") AND f.title CONTAINS
/// "Emilio Fernandez" RETURN DISTINCT p.ID`.
///
/// `forum_first` scans Forums, filters the title, and walks to the
/// moderator; `!forum_first` scans Persons, filters the birthday, and
/// walks `hasModerator` backwards into the moderated forums' titles. Both
/// produce the same (person, forum) pairs; DISTINCT keeps each person id
/// once, first-seen order.
pub fn q8(graph: &Graph, forum_first: bool) -> SolRows {
    let (Some(forum_lid), Some(person_lid)) = (graph.label_id("Forum"), graph.label_id("Person"))
    else {
        return empty(Q8_COLUMNS);
    };
    let (Some(title_kid), Some(birthday_kid), Some(id_kid)) = (
        graph.prop_key_id("title"),
        graph.prop_key_id("birthday"),
        graph.prop_key_id("ID"),
    ) else {
        return empty(Q8_COLUMNS);
    };
    let Some(has_moderator) = graph.rel_type_id("hasModerator") else {
        return empty(Q8_COLUMNS);
    };
    const TITLE_NEEDLE: &str = "Emilio Fernandez";
    const BIRTHDAY_CUTOFF: &str = "1990-01-01";

    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut distinct: Vec<i64> = Vec::new();
    let (mut persons, mut forums) = (Vec::new(), Vec::new());

    if forum_first {
        for forum in label_candidates(graph, forum_lid) {
            if !carries_label_and_contains(graph, forum, forum_lid, title_kid, TITLE_NEEDLE) {
                continue;
            }
            // (p)<-[:hasModerator]-(f): p is each rel's end.
            live_neighbors(
                graph,
                forum,
                has_moderator,
                Direction::Outgoing,
                &mut persons,
            );
            for person in persons.iter().copied() {
                if birthday_after(graph, person, person_lid, birthday_kid, BIRTHDAY_CUTOFF) {
                    record_person_id(graph, person, id_kid, &mut seen, &mut distinct);
                }
            }
        }
    } else {
        for person in label_candidates(graph, person_lid) {
            if !birthday_after(graph, person, person_lid, birthday_kid, BIRTHDAY_CUTOFF) {
                continue;
            }
            live_neighbors(
                graph,
                person,
                has_moderator,
                Direction::Incoming,
                &mut forums,
            );
            for forum in forums.iter().copied() {
                if carries_label_and_contains(graph, forum, forum_lid, title_kid, TITLE_NEEDLE) {
                    record_person_id(graph, person, id_kid, &mut seen, &mut distinct);
                    // DISTINCT collapses the rest of this person's forums.
                    break;
                }
            }
        }
    }

    SolRows {
        columns: Q8_COLUMNS.iter().map(|c| c.to_string()).collect(),
        rows: distinct
            .into_iter()
            .map(|id| vec![Value::Int(id)])
            .collect(),
    }
}

/// q30: `MATCH (c:Comment)-[:commentHasCreator]->(creator:Person),
/// (c)-[:replyOfPost]->(post:Post)-[:postHasCreator]->(creator)
/// RETURN COUNT(DISTINCT c.ID) > 0 AS has_self_reply`.
///
/// Full distinct-count walk (`early_exit` false), or an exists probe that
/// stops at the first self-reply (`true`) — same boolean answer by
/// construction, a different strategy the engine does not use (its
/// COUNT(DISTINCT ...) is computed fully and compared afterwards).
pub fn q30(graph: &Graph, early_exit: bool) -> SolRows {
    let has_reply = 'walk: {
        let (Some(comment_lid), Some(person_lid), Some(post_lid)) = (
            graph.label_id("Comment"),
            graph.label_id("Person"),
            graph.label_id("Post"),
        ) else {
            break 'walk false;
        };
        // `ID` is never read — COUNT(DISTINCT c.ID) > 0 is the seen-set
        // non-emptiness (every comment carries an ID on this data set) —
        // but the kernel still guards that the key is interned, the q19
        // guard's own spelling.
        if graph.prop_key_id("ID").is_none() {
            break 'walk false;
        }
        let (Some(has_creator), Some(reply_of_post), Some(post_has_creator)) = (
            graph.rel_type_id("commentHasCreator"),
            graph.rel_type_id("replyOfPost"),
            graph.rel_type_id("postHasCreator"),
        ) else {
            break 'walk false;
        };

        let mut seen: HashSet<NodeId> = HashSet::new();
        let (mut creators, mut posts, mut post_creators) = (Vec::new(), Vec::new(), Vec::new());
        'comments: for comment in label_candidates(graph, comment_lid) {
            live_neighbors(
                graph,
                comment,
                has_creator,
                Direction::Outgoing,
                &mut creators,
            );
            if creators.is_empty() {
                continue;
            }
            live_neighbors(
                graph,
                comment,
                reply_of_post,
                Direction::Outgoing,
                &mut posts,
            );
            if posts.is_empty() {
                continue;
            }
            // A comment's ID counts once when ANY of its creators is also
            // the creator of one of its parent posts.
            for creator in creators.iter().copied() {
                if !carries_label(graph, creator, person_lid) {
                    continue;
                }
                for post in posts.iter().copied() {
                    if !carries_label(graph, post, post_lid) {
                        continue;
                    }
                    live_neighbors(
                        graph,
                        post,
                        post_has_creator,
                        Direction::Outgoing,
                        &mut post_creators,
                    );
                    if post_creators.contains(&creator) {
                        seen.insert(comment);
                        if early_exit {
                            break 'walk true;
                        }
                        continue 'comments;
                    }
                }
            }
        }
        // The projected boolean.
        !seen.is_empty()
    };

    SolRows {
        columns: vec!["has_self_reply".into()],
        rows: vec![vec![Value::Bool(has_reply)]],
    }
}
