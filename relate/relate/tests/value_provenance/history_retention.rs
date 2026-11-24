//! ADR-0610's `history()` and the retention that keeps its input whole:
//! the retained walk's ascending list, the ordinary function contracts,
//! the `SHOW FUNCTIONS` row — and the checkpoint/GC/reopen roundtrips
//! that prove the GC horizon drop exempts every version while the
//! opt-in is set and the checkpoint persists the retained chains
//! (`chains/` objects) with real `xmin` preserved. A submodule of
//! `tests/value_provenance.rs` — its own binary is the parent's, so the
//! process-global `RELATE_PREVIEW` opt-in is the parent's too.

use super::*;

// --- ADR-0610's history() — the retained walk's whole ascending list,
// and the retention that keeps it whole across checkpoints and reopens. ---

/// The single `history()` column of a one-row query over `pattern`.
fn history_of(c: &mut relate::Connection, pattern: &str, key: &str) -> Value {
    let result = c
        .query(&format!(
            "MATCH {pattern} RETURN history(element, '{key}') AS h"
        ))
        .unwrap();
    assert_eq!(result.rows.len(), 1, "one {pattern} row");
    result.rows[0][0].clone()
}

/// [`history_of`]'s three-argument form: the same single column, the
/// third argument spelled by the test (`1`, `0`, `null`, …).
fn bounded_history_of(
    c: &mut relate::Connection,
    pattern: &str,
    key: &str,
    max_num: &str,
) -> Value {
    let result = c
        .query(&format!(
            "MATCH {pattern} RETURN history(element, '{key}', {max_num}) AS h"
        ))
        .unwrap();
    assert_eq!(result.rows.len(), 1, "one {pattern} row");
    result.rows[0][0].clone()
}

/// The overwriting fixture's expected history: one event per commit,
/// `values[i]` installed by `sequences[i]`, newest last.
fn k_history(db: &Database, sequences: &[u64], values: &[Value]) -> Value {
    Value::List(
        sequences
            .iter()
            .zip(values)
            .map(|(&ts, value)| event(db, ts, value.clone()))
            .collect::<Vec<_>>()
            .into(),
    )
}

#[test]
fn history_walks_birth_changes_and_removal_in_commit_order() {
    let db = gated();
    let mut c = db.connect().unwrap();
    let birth = committed(&mut c, "CREATE (n:P {k: 1, other: 9})");
    let set = committed(&mut c, "MATCH (n:P) SET n.k = 2");
    let removal = committed(&mut c, "MATCH (n:P) REMOVE n.k");
    let reset = committed(&mut c, "MATCH (n:P) SET n.k = 3");
    // Ascending commitTs, the removal's event carrying null, the whole
    // walk provenance()'s newest event is the tail of.
    assert_eq!(
        history_of(&mut c, "(element:P)", "k"),
        k_history(
            &db,
            &[birth, set, removal, reset],
            &[Value::Int(1), Value::Int(2), Value::Null, Value::Int(3)],
        )
    );
    // A key no version of the element ever carried has an empty history,
    // and the untouched sibling's is its birth alone.
    assert_eq!(
        history_of(&mut c, "(element:P)", "nope"),
        Value::List(Vec::new().into())
    );
    assert_eq!(
        history_of(&mut c, "(element:P)", "other"),
        k_history(&db, &[birth], &[Value::Int(9)])
    );
}

/// The tail bound (ADR-0610's `maxNum`): a five-event history where the
/// third argument keeps the last `maxNum` events — still ascending, the
/// same event maps as the two-argument form's tail — 0 keeping none,
/// and a count at the history's length keeping every event. The bound
/// rides the walk (a sliding window over the collected events, never a
/// post-hoc slice of the whole list), so the answers here are the
/// window's, not a materialized list's.
#[test]
fn historys_maxnum_returns_the_last_events_ascending() {
    let db = gated();
    let mut c = db.connect().unwrap();
    // Five events: the birth and four overwrites, one commit each.
    let mut sequences = vec![committed(&mut c, "CREATE (n:P {k: 0})")];
    for i in 1..=4 {
        sequences.push(committed(&mut c, &format!("MATCH (n:P) SET n.k = {i}")));
    }
    let values: Vec<Value> = (0..=4).map(Value::Int).collect();
    // The two-argument form is unchanged: every retained event.
    let whole = k_history(&db, &sequences, &values);
    assert_eq!(history_of(&mut c, "(element:P)", "k"), whole);
    // maxNum at the history's length: every event, ascending.
    assert_eq!(bounded_history_of(&mut c, "(element:P)", "k", "5"), whole);
    // The bounded tails: the last one and the last two, still ascending
    // — never reversed, never re-dated.
    assert_eq!(
        bounded_history_of(&mut c, "(element:P)", "k", "1"),
        k_history(&db, &sequences[4..], &values[4..])
    );
    assert_eq!(
        bounded_history_of(&mut c, "(element:P)", "k", "2"),
        k_history(&db, &sequences[3..], &values[3..])
    );
    // 0 is the empty list, and an explicit null maxNum is the
    // two-argument form.
    assert_eq!(
        bounded_history_of(&mut c, "(element:P)", "k", "0"),
        Value::List(Vec::new().into())
    );
    assert_eq!(
        bounded_history_of(&mut c, "(element:P)", "k", "null"),
        whole
    );
}

#[test]
fn history_answers_relationships_and_the_ordinary_function_contracts() {
    let db = gated();
    let mut c = db.connect().unwrap();
    let birth = committed(&mut c, "CREATE (:A)-[r:KNOWS {since: 2020}]->(:B)");
    let note = committed(&mut c, "MATCH ()-[r:KNOWS]->() SET r.note = 'why'");
    assert_eq!(
        history_of(&mut c, "()-[element:KNOWS]->()", "note"),
        Value::List(vec![event(&db, note, Value::String("why".into()))].into())
    );
    // The other key of the same relationship keeps its birth.
    assert_eq!(
        history_of(&mut c, "()-[element:KNOWS]->()", "since"),
        Value::List(vec![event(&db, birth, Value::Int(2020))].into())
    );
    // Cypher's null propagation answers null.
    let result = c
        .query("MATCH (n:A) RETURN history(null, 'k') AS a, history(n, null) AS b")
        .unwrap();
    assert_eq!(result.rows[0][0], Value::Null);
    assert_eq!(result.rows[0][1], Value::Null);
    // The call contract stands like any builtin's: the row's two-to-three
    // arity gate, the ordinary type rejections — the key's and
    // maxNum's — never a preview advertisement.
    for query in [
        "RETURN history(1)",
        "RETURN history(1, 'k')",
        "RETURN history(1, 'k', 3, 4)",
        "MATCH (n:A) RETURN history(n, 5)",
        "MATCH (n:A) RETURN history(n, 'k', -1)",
        "MATCH (n:A) RETURN history(n, 'k', 1.5)",
        "MATCH (n:A) RETURN history(n, 'k', 'two')",
    ] {
        let error = c
            .query(query)
            .expect_err(&format!("`{query}` must be rejected"))
            .to_string();
        assert!(
            error.contains("InvalidNumberOfArguments") || error.contains("InvalidArgumentType"),
            "`{query}`: the rejection must be an ordinary function error, got {error}"
        );
        assert!(
            !error.contains("RELATE_PREVIEW"),
            "`{query}`: the rejection must not advertise the gated name, got {error}"
        );
    }
    // The maxNum rejections are the argument-type ones, not arity.
    let error = c
        .query("MATCH (n:A) RETURN history(n, 'k', -1) AS h")
        .expect_err("a negative maxNum must be rejected")
        .to_string();
    assert!(
        error.contains("InvalidArgumentType") && error.contains("non-negative INTEGER"),
        "a negative maxNum is the ordinary argument-type error, got {error}"
    );
}

#[test]
fn show_functions_lists_the_history_row_under_the_opt_in() {
    let mut c = gated().connect().unwrap();
    let result = c
        .query(
            "SHOW FUNCTIONS YIELD name, category, signature \
             WHERE name = 'history' RETURN name, category, signature",
        )
        .unwrap();
    let [row] = &result.rows[..] else {
        panic!("expected one history row, got {:?}", result.rows)
    };
    assert_eq!(row[0], Value::String("history".into()));
    assert_eq!(row[1], Value::String("Scalar".into()));
    assert_eq!(
        row[2],
        Value::String(
            "history(element :: NODE | RELATIONSHIP, key :: STRING, maxNum = null :: INTEGER) \
             :: LIST<MAP>"
                .into()
        )
    );
}

/// Retention (ADR-0610): the horizon drop exempts every version while
/// the opt-in is set, so five overwrites of one property — each followed
/// by a checkpoint whose GC pass would otherwise drop the superseded
/// versions — still answer from `history()`, in memory and across the
/// checkpoint/reopen with real `xmin` preserved on the restored
/// versions.
#[test]
fn retention_keeps_every_version_across_checkpoints_and_reopen() {
    common::set_preview_once("value-provenance");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    let open =
        || Database::open_with(path, common::store::opts(SnapshotCodecKind::default())).unwrap();
    let mut sequences = Vec::new();
    {
        let db = open();
        let mut c = db.connect().unwrap();
        sequences.push(committed(&mut c, "CREATE (n:Person {name: 'Ada', k: 0})"));
        for i in 1..=5 {
            sequences.push(committed(
                &mut c,
                &format!("MATCH (n:Person) SET n.k = {i}"),
            ));
            // The checkpoint dumps its image, publishes the interval's
            // retained-chain part, and runs the GC pass — the moment that
            // would drop the superseded versions while retention is off.
            db.checkpoint().unwrap();
        }
        // In memory, after five checkpoint/GC cycles: every past value
        // still answers, each with its committing transaction's record.
        let values: Vec<Value> = (0..=5).map(Value::Int).collect();
        assert_eq!(
            history_of(&mut c, "(element:Person)", "k"),
            k_history(&db, &sequences, &values)
        );
    }
    // The reopen: the family's parts replace the image's collapsed rows
    // with the real chains, so the same events answer with the real
    // commitTs of every version — not the restored row's birth ts.
    let db = open();
    let mut c = db.connect().unwrap();
    let values: Vec<Value> = (0..=5).map(Value::Int).collect();
    assert_eq!(
        history_of(&mut c, "(element:Person)", "k"),
        k_history(&db, &sequences, &values)
    );
    // provenance() agrees with history()'s newest event, and the
    // reopened store keeps retaining: a further overwrite lands on the
    // chain beside the recovered ones.
    let sixth = committed(&mut c, "MATCH (n:Person) SET n.k = 6");
    let result = c
        .query("MATCH (element:Person) RETURN provenance(element, 'k') AS p")
        .unwrap();
    assert_eq!(result.rows[0][0], event(&db, sixth, Value::Int(6)));
    let mut with_sixth = sequences.clone();
    with_sixth.push(sixth);
    let mut values = values;
    values.push(Value::Int(6));
    assert_eq!(
        history_of(&mut c, "(element:Person)", "k"),
        k_history(&db, &with_sixth, &values)
    );
}

/// The checkpoint/reopen matrix for the retained chains — the codec
/// matrix of `records_survive_reopen`: each backend hosts whichever
/// codec set it can. Two overwrites with a checkpoint between each pair
/// prove the chains (not just the records) survive the roundtrip.
fn chains_survive_reopen(db: Database, reopen: impl Fn() -> Database) {
    let mut c = db.connect().unwrap();
    let birth = committed(&mut c, "CREATE (n:Person {name: 'Ada', k: 0})");
    let first = committed(&mut c, "MATCH (n:Person) SET n.k = 1");
    db.checkpoint().unwrap();
    let second = committed(&mut c, "MATCH (n:Person) SET n.k = 2");
    drop(c);
    db.checkpoint().unwrap();
    drop(db);
    let db = reopen();
    let mut c = db.connect().unwrap();
    assert_eq!(
        history_of(&mut c, "(element:Person)", "k"),
        k_history(
            &db,
            &[birth, first, second],
            &[Value::Int(0), Value::Int(1), Value::Int(2)],
        )
    );
}

#[test]
fn retained_chains_roundtrip_every_compiled_codec() {
    common::set_preview_once("value-provenance");
    for codec in SnapshotCodecKind::compiled().iter().copied() {
        let dir = tempfile::tempdir().unwrap();
        let reopen = move || {
            Database::open_with(dir.path().to_str().unwrap(), common::store::opts(codec)).unwrap()
        };
        chains_survive_reopen(reopen(), reopen);
    }
}

#[test]
fn retained_chains_roundtrip_memory_vfs() {
    common::set_preview_once("value-provenance");
    let vfs: Arc<dyn Vfs> = Arc::new(MemoryVfs::new());
    let reopen = || {
        Database::open_vfs(
            Arc::clone(&vfs),
            common::store::opts(SnapshotCodecKind::default()),
        )
        .unwrap()
    };
    chains_survive_reopen(reopen(), reopen);
}

#[test]
fn retained_chains_roundtrip_bundle() {
    common::set_preview_once("value-provenance");
    let dir = tempfile::tempdir().unwrap();
    let uri = dir.path().join("graph.rl");
    let reopen = || {
        Database::open_with(
            uri.to_str().unwrap(),
            common::store::opts(SnapshotCodecKind::default()),
        )
        .unwrap()
    };
    chains_survive_reopen(reopen(), reopen);
}

/// The load's fail-closed posture over the family's own grammar
/// (ADR-0610): a hand-crafted part whose element decoded with no
/// versions — `"versions":[]`, the one shape no honest publish writes —
/// fails the open loudly, naming the part exactly as a parse failure
/// does, never a silent skip and never an empty chain installed for a
/// later walk to panic on. The part is planted over the checkpoint's
/// own family part — its window's `hi` is the manifest's checkpoint
/// LSN, so the load provably covers it (a part past the manifest is
/// skipped as a crashed checkpoint's) — and the byte-level store
/// crafting is the closed side's
/// `an_opt_out_checkpoint_collapses_retained_history` pattern.
#[test]
fn a_part_with_no_versions_fails_the_open_loudly() {
    common::set_preview_once("value-provenance");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    common::store::seed_store(&path, SnapshotCodecKind::Json);
    // One retained chain: an element whose versions array is empty.
    let chains_part = concat!("{\"nodes\":[{\"id\":9,\"versions\":[]}],", "\"rels\":[]}",);
    {
        let vfs = common::store::store_vfs(&path);
        let part = vfs
            .list_dir("chains")
            .unwrap()
            .into_iter()
            .next()
            .expect("the opted-in seed checkpoint published a chains part");
        let chain_name = format!("chains/{part}");
        let mut file = vfs
            .open(&chain_name, relate_storage::VfsOpen::create_truncate())
            .unwrap();
        file.write_all(chains_part.as_bytes()).unwrap();
        file.sync_all().unwrap();
        vfs.sync_dir("chains").unwrap();
    }
    let rendered = common::err_text(Database::open_with(
        path.to_str().unwrap(),
        common::store::opts(SnapshotCodecKind::Json),
    ));
    assert!(
        rendered.contains("failed to restore retained-chain part chains/"),
        "{rendered}"
    );
    assert!(
        rendered.contains("retained-chain restore of an element with no versions"),
        "{rendered}"
    );
}
