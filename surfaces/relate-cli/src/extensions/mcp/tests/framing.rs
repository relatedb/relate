//! The session's framing: the handshake read deadline, the per-batch and
//! whole-import commit policies, and the exit classification inside the
//! frame. Helpers are the tests module's — one kit, not a copy.

use super::*;
// ---------------------------------------------------------------------
// The read deadline
// ---------------------------------------------------------------------

/// A child that holds its pipes and writes nothing: the mount's deadline
/// names the bound instead of blocking the host. The scripted pipe stays
/// open (the test holds the write end) and silent.
#[test]
fn a_silent_child_fails_the_session_on_the_read_deadline() {
    let (reader, _writer) = std::io::pipe().expect("a test pipe opens");
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect_typed(Namespace::Neo4j).unwrap();
    let mut session = Session::new(
        BufReader::new(reader),
        Vec::new(),
        caps(),
        &command("per-batch", false),
    )
    .with_read_deadline(Duration::from_millis(50));
    let error = session.drive(&db, &mut conn).unwrap_err();
    assert_eq!(
        error.to_string(),
        "the extension wrote no MCP request within 50ms"
    );
}

/// The deadline covers only the handshake: a child that initializes and then
/// takes longer than the injected bound before its next request still
/// completes. The scripted writer withholds the second line for several
/// times the millisecond bound — with the shipped ten seconds that delay
/// would be far longer than a test may run, which is exactly why the bound
/// is injectable.
#[test]
fn a_child_may_exceed_the_deadline_after_the_handshake() {
    let deadline = Duration::from_millis(25);
    let (reader, mut writer) = std::io::pipe().expect("a test pipe opens");
    let scripted = std::thread::spawn(move || {
        let handshake = format!("{}\n", initialize(1));
        writer
            .write_all(handshake.as_bytes())
            .expect("the scripted child writes its handshake");
        writer
            .flush()
            .expect("the scripted child flushes its handshake");
        // The child works before its first request: this is not a stall,
        // and the established session's read carries no deadline.
        std::thread::sleep(deadline * 10);
        let follow_up = format!("{}\n", request(2, mcp::METHOD_TOOLS_LIST, json!({})));
        writer
            .write_all(follow_up.as_bytes())
            .expect("the scripted child writes its follow-up request");
        // Dropping the pipe's write end is the child's clean stream end.
    });
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect_typed(Namespace::Neo4j).unwrap();
    let mut session = Session::new(
        BufReader::new(reader),
        Vec::new(),
        caps(),
        &command("per-batch", false),
    )
    .with_read_deadline(deadline);
    let result = session.drive(&db, &mut conn);
    assert!(result.is_ok(), "{result:?}");
    let responses: Vec<Value> = String::from_utf8(session.into_out())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 2, "both requests were answered");
    assert_eq!(responses[1]["id"], json!(2));
    assert!(responses[1]["result"]["tools"].is_array());
    scripted.join().expect("the scripted child finishes");
}

// ---------------------------------------------------------------------
// The commit framing
// ---------------------------------------------------------------------

/// The script that writes one node and then breaks the framing bound: a
/// committed write under per-batch, a rolled-back write under
/// whole-import.
fn write_then_fatal() -> (Vec<String>, Caps) {
    let tight = Caps {
        statement_bytes: 256,
        ..Caps::MOUNT
    };
    let write = call(
        2,
        mcp::WRITE_CYPHER,
        json!({"statement": "CREATE (n:Greet)"}),
    )
    .to_string();
    let oversized = "x".repeat(tight.mcp_request_bytes() as usize + 1);
    (vec![initialize(1).to_string(), write, oversized], tight)
}

#[test]
fn per_batch_keeps_the_committed_calls_after_a_fatal_refusal() {
    let (lines, tight) = write_then_fatal();
    let outcome = run_raw(lines, "per-batch", tight);
    assert!(outcome.result.is_err());
    assert_eq!(node_count(&outcome.db), 1);
}

#[test]
fn whole_import_rolls_the_session_back_after_a_fatal_refusal() {
    let (lines, tight) = write_then_fatal();
    let outcome = run_raw(lines, "whole-import", tight);
    assert!(outcome.result.is_err());
    assert_eq!(node_count(&outcome.db), 0);
}

/// A `batch_cypher` call whose second statement fails, under whole-import:
/// the session's one transaction cannot roll back a single call, so the
/// failure ends the run instead of answering a continuation the framing
/// cannot keep. Nothing commits, and no response is written for the doomed
/// call.
#[test]
fn a_failed_batch_under_whole_import_ends_the_run() {
    let mut input = opened();
    input.push(call(
        2,
        mcp::BATCH_CYPHER,
        json!({"statements": [
            {"statement": "CREATE (n:First)"},
            {"statement": "CREATE (n:Second {id: $missing})"},
        ]}),
    ));
    let outcome = run_with(input, "whole-import", caps());
    let message = outcome.result.unwrap_err().to_string();
    assert!(
        message.starts_with("the extension MCP batch failed inside the whole-import transaction: "),
        "{message}"
    );
    assert_eq!(
        outcome.responses.len(),
        1,
        "only the initialize response was written: {}",
        outcome.responses.len()
    );
    assert_eq!(node_count(&outcome.db), 0);
}

/// Under whole-import a batch that succeeds stays inside the session's one
/// transaction and commits with it at the request stream's end.
#[test]
fn a_whole_import_batch_commits_with_the_session() {
    let mut input = opened();
    input.push(call(
        2,
        mcp::BATCH_CYPHER,
        json!({"statements": [
            {"statement": "CREATE (n:First)"},
            {"statement": "CREATE (n:Second)"},
        ]}),
    ));
    let outcome = run_with(input, "whole-import", caps());
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    assert_eq!(node_count(&outcome.db), 2);
}

/// The real mount classifies the child's exit through the session's end
/// hook, which runs inside the framed import. A hook failure (a clean
/// stream followed by a non-zero exit) must therefore fail the run before
/// the whole-import transaction commits — the store stays empty.
#[test]
fn a_failed_exit_is_classified_inside_the_frame() {
    let mut script = String::new();
    script.push_str(&initialize(1).to_string());
    script.push('\n');
    script.push_str(
        &call(
            2,
            mcp::WRITE_CYPHER,
            json!({"statement": "CREATE (n:Greet)"}),
        )
        .to_string(),
    );
    script.push('\n');
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect_typed(Namespace::Neo4j).unwrap();
    let mut session = Session::new(
        BufReader::new(Cursor::new(script.into_bytes())),
        Vec::new(),
        Caps::MOUNT,
        &command("whole-import", false),
    );
    session.set_end(Box::new(|end| {
        assert_eq!(
            end,
            SessionEnd::Complete,
            "a clean stream classifies its exit"
        );
        Err(crate::extensions::protocol::ProtocolError::ExitFailed {
            code: 1,
            stderr: String::new(),
        }
        .into())
    }));
    let result = session.drive(&db, &mut conn);
    assert_eq!(
        result.unwrap_err().to_string(),
        "the extension child exited with 1"
    );
    assert_eq!(node_count(&db), 0);
}

// ---------------------------------------------------------------------
// The whole-run stream budget the framing decides (ADR-0598)
// ---------------------------------------------------------------------

/// Under whole-import framing a session whose request bytes cross the
/// whole-run stream cap fails the run, and the refusal names the framing
/// it guards and the remedy.
#[test]
fn the_stream_cap_is_fatal_under_whole_import_framing() {
    let tight = Caps {
        stream_bytes: 16,
        ..Caps::MOUNT
    };
    let mut input = opened();
    input.push(request(2, mcp::METHOD_TOOLS_LIST, json!({})));
    let outcome = run_with(input, "whole-import", tight);
    assert_eq!(
        outcome.result.unwrap_err().to_string(),
        "the extension MCP request stream exceeds the 16-byte whole-import \
         stream cap; window the import or move the package to per-batch framing"
    );
}

/// A per-batch session retains nothing between calls, so the whole-run
/// stream cap does not apply to it (ADR-0598): however tight, every call
/// streams — the per-call caps and the store's live-graph limit are that
/// session's bounds, never a whole-run byte count.
#[test]
fn the_stream_cap_does_not_bound_a_per_batch_session() {
    let tight = Caps {
        stream_bytes: 16,
        ..Caps::MOUNT
    };
    let mut input = opened();
    for id in 2..12 {
        input.push(request(id, mcp::METHOD_TOOLS_LIST, json!({})));
    }
    let outcome = run_with(input, "per-batch", tight);
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    assert_eq!(
        outcome.responses.len(),
        11,
        "the initialize answer and every tools/list answer streamed"
    );
}
