//! Relate for the browser: a thin `wasm-bindgen` wrapper around
//! `relate::Database`. `new` opens `:memory:`. `open` opens `opfs://`
//! on a sync-access-handle VFS (ADR-0145).
//!
//! `execute` and `executeScript` return the canonical `relate_model::wire`
//! JSON envelope (`{columns, columnTypes, rows, stats, notifications,
//! elapsed_ms, error}`) as JavaScript strings, and `renderResult` renders
//! envelopes as a recording (ADR-0131). Engine errors
//! flow through typed `Result`s; a Rust panic on wasm32 traps the module
//! (`panic = abort` is the wasm target default and `catch_unwind` is a no-op),
//! which JS observes as a `WebAssembly.RuntimeError`. Hosts classify that with
//! `isWasmTrap` (`js/trap.js`, shipped with the bundle): the playground
//! latches a poisoned engine, and a host running the engine in a worker
//! discards that worker.

#![cfg(target_arch = "wasm32")]

mod opfs;

use std::sync::Arc;

use relate::{execute_wire, run_script_wire, Connection, Database, OpenMode, OpenOptions};
use relate_formatter::recording;
use relate_storage::Vfs;
use wasm_bindgen::prelude::*;

use opfs::OpfsVfs;

#[wasm_bindgen(start)]
pub fn __start() {
    console_error_panic_hook::set_once();
}

/// Opaque handle to a Relate database with one connection.
///
/// `new` is in-memory. [`RelateDb::open`] is an OPFS database (ADR-0145).
#[wasm_bindgen]
pub struct RelateDb {
    // Drop the connection, then the database (so its checkpoint still sees the
    // VFS), then the VFS (so sync access handles close last).
    conn: Connection,
    db: Database,
    opfs: Option<Arc<OpfsVfs>>,
}

#[wasm_bindgen]
impl RelateDb {
    /// `readOnly` loads once and refuses writes (ADR-0136). Omit it, or pass
    /// false, to keep the writable open. `new RelateDb()` stays writable.
    #[wasm_bindgen(constructor)]
    pub fn new(read_only: Option<bool>) -> Result<RelateDb, JsError> {
        memory_db(read_only.unwrap_or(false))
    }

    /// Open `opfs://<name>` in a dedicated worker (ADR-0145).
    ///
    /// `<name>` is one segment of 1–64 characters from `[A-Za-z0-9_-]`.
    /// On the page thread this rejects before touching OPFS. `readOnly`
    /// loads once and refuses writes (ADR-0136); omit it to stay writable.
    pub async fn open(uri: &str, read_only: Option<bool>) -> Result<RelateDb, JsError> {
        let read_only = read_only.unwrap_or(false);
        let name = opfs_name(uri)?;
        let vfs = OpfsVfs::prepare(&name).await.map_err(to_js_err)?;
        let db = Database::open_vfs(
            Arc::clone(&vfs) as Arc<dyn Vfs>,
            OpenOptions::with_mode(open_mode(read_only)),
        )
        .map_err(to_js_err)?;
        let conn = db.connect().map_err(to_js_err)?;
        Ok(RelateDb {
            conn,
            db,
            opfs: Some(vfs),
        })
    }

    /// Run one statement (`relate::execute_wire`): its wire envelope.
    pub fn execute(&mut self, query: &str, params_json: Option<String>) -> String {
        execute_wire(&mut self.conn, query, params_json.as_deref()).to_string()
    }

    /// Run a script of one or more statements (`relate::run_script_wire`,
    /// ADR-0131): one envelope per statement run, stopping after the first
    /// error; none for a blank or comment-only script.
    #[wasm_bindgen(js_name = executeScript)]
    pub fn execute_script(&mut self, script: &str, params_json: Option<String>) -> Vec<String> {
        run_script_wire(&mut self.conn, script, params_json.as_deref())
            .iter()
            .map(|envelope| envelope.to_string())
            .collect()
    }

    pub fn reset(&mut self) -> Result<(), JsError> {
        if self.db.is_read_only() {
            return Err(JsError::new("database is read-only"));
        }
        // Park on a fresh in-memory database before dropping the live handle,
        // so the connection drops before the database on both paths.
        let parked = memory_db(false)?;
        let old_conn = std::mem::replace(&mut self.conn, parked.conn);
        let old_db = std::mem::replace(&mut self.db, parked.db);
        drop(old_conn);
        drop(old_db);
        let Some(vfs) = self.opfs.clone() else {
            return Ok(());
        };
        // The parked database is already memory. A failed wipe or reopen
        // must not keep advertising OPFS.
        if let Err(err) = self.reopen_wiped(&vfs) {
            self.opfs = None;
            return Err(to_js_err(err));
        }
        Ok(())
    }
}

impl RelateDb {
    /// Wipe the OPFS store behind `vfs` and reopen it writable, swapping the
    /// fresh handles into `self` (the reset tail of [`RelateDb::reset`]).
    fn reopen_wiped(&mut self, vfs: &Arc<OpfsVfs>) -> relate::Result<()> {
        vfs.wipe()?;
        let db = Database::open_vfs(
            Arc::clone(vfs) as Arc<dyn Vfs>,
            OpenOptions::with_mode(OpenMode::Writable),
        )?;
        let conn = db.connect()?;
        self.db = db;
        self.conn = conn;
        Ok(())
    }
}

fn open_mode(read_only: bool) -> OpenMode {
    if read_only {
        OpenMode::ReadOnly
    } else {
        OpenMode::Writable
    }
}

fn memory_db(read_only: bool) -> Result<RelateDb, JsError> {
    let db = Database::open_with(":memory:", OpenOptions::with_mode(open_mode(read_only)))
        .map_err(to_js_err)?;
    let conn = db.connect().map_err(to_js_err)?;
    Ok(RelateDb {
        conn,
        db,
        opfs: None,
    })
}

fn opfs_name(uri: &str) -> Result<String, JsError> {
    let Some(name) = uri.strip_prefix("opfs://") else {
        return Err(JsError::new(
            "OPFS open requires an opfs://name URI; use new RelateDb() for :memory:",
        ));
    };
    let ok = (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok {
        Ok(name.to_string())
    } else {
        Err(JsError::new(
            "OPFS name must be 1-64 characters from [A-Za-z0-9_-]",
        ))
    }
}

/// Render a script's envelopes (JSON strings, as `executeScript` returns
/// them) as a recording (ADR-0131, ADR-0132): JSON `{kind, text, table,
/// graph, stats}` from `relate_formatter::recording`. Throws on an envelope
/// that is not wire JSON.
#[wasm_bindgen(js_name = renderResult)]
pub fn render_result(envelopes: Vec<String>) -> Result<String, JsError> {
    let values = envelopes
        .iter()
        .map(|text| serde_json::from_str(text))
        .collect::<Result<Vec<serde_json::Value>, _>>()
        .map_err(to_js_err)?;
    recording::render(&values)
        .map(|r| r.to_json().to_string())
        .map_err(to_js_err)
}

fn to_js_err(err: impl std::fmt::Display) -> JsError {
    JsError::new(&err.to_string())
}
