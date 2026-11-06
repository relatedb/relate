//! Browser smoke tests for the wasm-bindgen surface (ADR-0084).
//!
//! Run with `wasm-pack test --headless --firefox surfaces/relate-wasm`
//! (or `--chrome`). Native `cargo test` is a no-op because the crate is
//! `#![cfg(target_arch = "wasm32")]` (ADR-0085).

#![cfg(target_arch = "wasm32")]

use relate_wasm::RelateDb;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;
use web_sys::{Blob, BlobPropertyBag, Url, Worker, WorkerOptions, WorkerType};

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn return_one_round_trip() {
    let mut db = RelateDb::new(None).expect("open :memory:");
    let raw = db.execute("RETURN 1 AS n", None);
    let v: serde_json::Value = serde_json::from_str(&raw).expect("wire envelope");
    assert_eq!(v["columns"], serde_json::json!(["n"]));
    assert_eq!(v["rows"], serde_json::json!([{"n": 1}]));
    assert!(v["error"].is_null(), "unexpected error: {raw}");
}

#[wasm_bindgen_test]
fn read_only_rejects_writes() {
    let mut db = RelateDb::new(Some(true)).expect("open :memory:");
    let raw = db.execute("RETURN 1 AS n", None);
    let v: serde_json::Value = serde_json::from_str(&raw).expect("wire envelope");
    assert!(v["error"].is_null(), "{raw}");
    let raw = db.execute("CREATE ()", None);
    let v: serde_json::Value = serde_json::from_str(&raw).expect("wire envelope");
    let err = v["error"].as_str().unwrap_or("");
    assert!(err.contains("read-only"), "{raw}");
    let reset_err = db.reset().expect_err("read-only reset must not wipe");
    assert!(
        js_error_message(reset_err).contains("read-only"),
        "reset must refuse a read-only handle"
    );

    let mut writable = RelateDb::new(None).expect("open :memory:");
    let raw = writable.execute("CREATE ()", None);
    let v: serde_json::Value = serde_json::from_str(&raw).expect("wire envelope");
    assert!(v["error"].is_null(), "{raw}");
}

#[wasm_bindgen_test]
fn syntax_error_is_wire_envelope_not_panic() {
    let mut db = RelateDb::new(None).expect("open :memory:");
    let raw = db.execute("NOT CYPHER", None);
    let v: serde_json::Value = serde_json::from_str(&raw).expect("wire envelope");
    assert!(v["error"].is_string(), "expected error envelope, got {raw}");
    // A typed engine error returns normally; the wasm-bindgen `&mut self`
    // RefCell guard drops with the call, so a follow-up call must not trip
    // "recursive use of an object detected".
    let raw2 = db.execute("RETURN 2 AS n", None);
    let v2: serde_json::Value = serde_json::from_str(&raw2).expect("wire envelope");
    assert_eq!(v2["rows"], serde_json::json!([{"n": 2}]));
}

#[wasm_bindgen_test]
fn reset_discards_data() {
    let mut db = RelateDb::new(None).expect("open :memory:");
    db.execute("CREATE (:N)", None);
    db.reset().expect("reset opens a fresh :memory:");
    let raw = db.execute("MATCH (n) RETURN count(n) AS c", None);
    let v: serde_json::Value = serde_json::from_str(&raw).expect("wire envelope");
    assert_eq!(v["rows"], serde_json::json!([{"c": 0}]));
}

#[wasm_bindgen_test]
fn execute_script_runs_each_statement() {
    let mut db = RelateDb::new(None).expect("open :memory:");
    let envelopes = db.execute_script(
        "CREATE (:N {s: 'a;b'}); // note ; here\nCREATE (:N);\n// trailing comment",
        None,
    );
    assert_eq!(envelopes.len(), 2, "got {envelopes:?}");
    let raw = db.execute("MATCH (n:N) RETURN count(n) AS c", None);
    let v: serde_json::Value = serde_json::from_str(&raw).expect("wire envelope");
    assert_eq!(v["rows"], serde_json::json!([{"c": 2}]));
    assert!(db.execute_script("  // only a comment ", None).is_empty());
    let failed = db.execute_script("CREATE (:N); RETURN 1 +; CREATE (:N)", None);
    assert_eq!(failed.len(), 2, "stops after the first error: {failed:?}");
}

/// Two layers, pinned separately: the wire envelope
/// (`RelateDb::execute`) always carries the `notifications` array —
/// empty on a plain run, the EXPLAIN plan-time list otherwise — and the
/// recording renderer (`renderResult`,
/// `relate_formatter::recording`) reads only the envelope's
/// `columns`/`rows`. An EXPLAIN envelope has neither since ADR-0359 (the
/// plan rides `QueryResult.plan`), so it renders the `(no rows)` text with
/// no table — while the notification list riding the same envelope is not
/// treated as a malformed envelope.
#[wasm_bindgen_test]
fn wire_envelope_carries_notifications() {
    let mut db = RelateDb::new(None).expect("open :memory:");
    let raw = db.execute("RETURN 1 AS n", None);
    let v: serde_json::Value = serde_json::from_str(&raw).expect("wire envelope");
    assert_eq!(v["notifications"], serde_json::json!([]), "{raw}");

    db.execute("CREATE (:Person), (:City)", None);
    let raw = db.execute("EXPLAIN MATCH (a:Person), (b:City) RETURN a, b", None);
    let v: serde_json::Value = serde_json::from_str(&raw).expect("wire envelope");
    assert!(v["error"].is_null(), "query error: {raw}");
    let notifications = v["notifications"]
        .as_array()
        .expect("the envelope always carries the notifications array");
    assert_eq!(notifications.len(), 1, "{raw}");
    assert_eq!(
        notifications[0]["code"],
        serde_json::json!("Neo.ClientNotification.Statement.CartesianProduct")
    );
    assert_eq!(notifications[0]["gql_status"], serde_json::json!("03N90"));

    let recording = relate_wasm::render_result(vec![raw]).expect("recording");
    let r: serde_json::Value = serde_json::from_str(&recording).expect("recording JSON");
    assert_eq!(r["kind"], "table");
    assert_eq!(
        r["text"],
        serde_json::json!("(no rows)"),
        "the recording renderer reads columns/rows, and EXPLAIN has none: {recording}"
    );
    assert!(
        r["table"].is_null(),
        "an EXPLAIN envelope yields no recording table: {recording}"
    );
}

#[wasm_bindgen_test]
fn render_result_renders_a_recording() {
    let mut db = RelateDb::new(None).expect("open :memory:");
    let envelopes = db.execute_script("CREATE (:N); RETURN 1 AS n", None);
    let raw = relate_wasm::render_result(envelopes).expect("recording");
    let r: serde_json::Value = serde_json::from_str(&raw).expect("recording JSON");
    assert_eq!(r["kind"], "table");
    assert_eq!(
        r["text"],
        "| n |\n|---|\n| 1 |\n1 row · nodes created: 1, labels added: 1"
    );
    assert_eq!(r["table"]["rows"], serde_json::json!([["1"]]));
}

fn js_error_message(err: JsError) -> String {
    let value = JsValue::from(err);
    js_sys::Reflect::get(&value, &JsValue::from_str("message"))
        .ok()
        .and_then(|text| text.as_string())
        .unwrap_or_else(|| format!("{value:?}"))
}

#[wasm_bindgen_test]
async fn opfs_open_on_the_page_requires_a_worker() {
    let err = match RelateDb::open("opfs://page", None).await {
        Ok(_) => panic!("page thread must not open OPFS"),
        Err(err) => err,
    };
    let msg = js_error_message(err);
    assert!(msg.contains("dedicated worker"), "{msg}");
}

#[wasm_bindgen_test]
async fn opfs_open_rejects_a_bad_name_before_touching_storage() {
    let err = match RelateDb::open("opfs://../x", None).await {
        Ok(_) => panic!("bad OPFS name must be rejected"),
        Err(err) => err,
    };
    let msg = js_error_message(err);
    assert!(msg.contains("[A-Za-z0-9_-]"), "{msg}");
}

#[wasm_bindgen_test]
async fn opfs_worker_reopens_a_committed_node() {
    let glues = glue_urls();
    assert!(!glues.is_empty(), "no script URLs to load in the worker");
    let name = format!(
        "s{:08x}",
        (js_sys::Math::random() * 0xffff_ffff_u32 as f64) as u32
    );
    let created = worker_phase(&glues, &name, "create").await;
    assert!(created.ok, "create failed: {}", created.message);
    let read = worker_phase(&glues, &name, "read").await;
    assert!(read.ok, "read failed: {}", read.message);
    let body: serde_json::Value = serde_json::from_str(&read.raw).expect("wire envelope");
    assert!(body["error"].is_null(), "query error: {}", read.raw);
    assert_eq!(body["rows"], serde_json::json!([{ "c": 1 }]));
}

struct Phase {
    ok: bool,
    message: String,
    raw: String,
}

async fn worker_phase(glues: &[String], name: &str, phase: &str) -> Phase {
    let options = BlobPropertyBag::new();
    options.set_type("text/javascript");
    let parts = js_sys::Array::of1(&JsValue::from_str(WORKER_JS));
    let blob = Blob::new_with_str_sequence_and_options(&parts, &options).expect("worker blob");
    let url = Url::create_object_url_with_blob(&blob).expect("blob url");
    let worker_options = WorkerOptions::new();
    worker_options.set_type(WorkerType::Module);
    let worker = Worker::new_with_options(&url, &worker_options).expect("worker");
    let listener = worker.clone();
    let incoming = js_sys::Promise::new(&mut |resolve, _reject| {
        let closure = Closure::once(move |event: web_sys::MessageEvent| {
            let _ = resolve.call1(&JsValue::NULL, &event.data());
        });
        listener.set_onmessage(Some(closure.as_ref().unchecked_ref()));
        closure.forget();
    });
    let payload = js_sys::Object::new();
    let glue_list = js_sys::Array::new();
    for glue in glues {
        glue_list.push(&JsValue::from_str(glue));
    }
    js_sys::Reflect::set(&payload, &"glues".into(), &glue_list).unwrap();
    js_sys::Reflect::set(&payload, &"name".into(), &JsValue::from_str(name)).unwrap();
    js_sys::Reflect::set(&payload, &"phase".into(), &JsValue::from_str(phase)).unwrap();
    worker.post_message(&payload).expect("post to worker");
    let data = JsFuture::from(incoming).await.expect("worker message");
    worker.terminate();
    let _ = Url::revoke_object_url(&url);
    let ok = js_sys::Reflect::get(&data, &"ok".into())
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let message = js_sys::Reflect::get(&data, &"message".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    let raw = js_sys::Reflect::get(&data, &"raw".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    Phase { ok, message, raw }
}

/// Modules the worker can `import()` looking for `RelateDb.open`.
///
/// `wasm-pack test` loads the generated glue with an extensionless import
/// (`from './wasm-bindgen-test'`). The browser records that URL, which does
/// not contain `.js`. `run.js` is the test driver: evaluating it starts the
/// suite, and snippet modules are the OPFS helpers, not the glue.
fn glue_urls() -> Vec<String> {
    let Some(window) = web_sys::window() else {
        return Vec::new();
    };
    let Some(perf) = window.performance() else {
        return Vec::new();
    };
    let entries = perf.get_entries_by_type("resource");
    let mut glue = Vec::new();
    let mut scripts = Vec::new();
    for entry in entries.iter() {
        let Some(name) = js_sys::Reflect::get(&entry, &"name".into())
            .ok()
            .and_then(|value| value.as_string())
        else {
            continue;
        };
        if name.contains("snippets/") {
            continue;
        }
        let file = resource_file(&name);
        if file == "run.js" {
            continue;
        }
        if !file.contains('.') {
            glue.push(name);
        } else if file.ends_with(".js") {
            scripts.push(name);
        }
    }
    glue.append(&mut scripts);
    glue
}

fn resource_file(url: &str) -> &str {
    let path = url.split('?').next().unwrap_or(url);
    path.rsplit('/').next().unwrap_or(path)
}

const WORKER_JS: &str = r#"
self.onmessage = async (event) => {
  const data = event.data;
  try {
    let mod = null;
    let glueUrl = null;
    const errors = [];
    for (const url of data.glues) {
      try {
        const candidate = await import(url);
        if (candidate.RelateDb && typeof candidate.RelateDb.open === 'function') {
          mod = candidate;
          glueUrl = url;
          break;
        }
      } catch (err) {
        errors.push(String(err && err.message || err));
      }
    }
    if (!mod) throw new Error('glue not found: ' + errors.join(' | '));
    // The test runner's init has no default wasm path. A relative path would
    // resolve against this blob worker, so name the sibling of the glue URL.
    if (typeof mod.default === 'function') {
      await mod.default({
        module_or_path: new URL('wasm-bindgen-test_bg.wasm', glueUrl),
      });
    }
    const db = await mod.RelateDb.open('opfs://' + data.name);
    if (data.phase === 'create') {
      const raw = db.execute('CREATE (:Kept {k: 1})', null);
      const body = JSON.parse(raw);
      if (body.error) throw new Error(body.error);
      db.free();
      self.postMessage({ ok: true });
    } else {
      const raw = db.execute('MATCH (n:Kept) RETURN count(n) AS c', null);
      db.free();
      self.postMessage({ ok: true, raw });
    }
  } catch (err) {
    // Firefox's stack is only "fn@url:line:col" and omits the exception
    // text. Chrome's stack already starts with the message.
    const message = (err && err.message) || String(err);
    const stack = (err && err.stack) || '';
    const hidden = stack && stack.indexOf(message) === -1;
    const text = hidden ? message + '\n' + stack : (stack || message);
    self.postMessage({ ok: false, message: text });
  }
};
"#;
