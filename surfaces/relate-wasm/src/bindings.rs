//! The exported JavaScript surface: `wasm-bindgen` wrappers around
//! `relate::Database` — compiled for wasm32 only, re-exported from the
//! crate root. The OPFS core it opens through lives in `super::opfs`,
//! which compiles on every target for its native unit tests.

use std::sync::Arc;

use relate::{execute_wire, run_script_wire, Connection, Database, OpenMode, OpenOptions};
use relate_formatter::recording;
use relate_storage::Vfs;
use wasm_bindgen::prelude::*;

use crate::opfs::OpfsVfs;

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
    // VFS), then the backing VFS (so sync access handles close last).
    conn: Connection,
    db: Database,
    backing: Backing,
}

/// The handle's backing store — the typed fact [`RelateDb::reset`]
/// branches on, so "no OPFS" never means two things: an in-memory
/// database by construction, a live OPFS store, or a FAILED OPFS
/// reset parked read-only, where every later write fails closed with
/// the read-only refusal instead of silently landing in a memory
/// store the caller cannot see.
enum Backing {
    /// `new`: the handle was built in-memory.
    Memory,
    /// `open`: the handle serves an OPFS store through this VFS.
    Opfs(Arc<OpfsVfs>),
    /// An OPFS reset failed: the handle is parked on a read-only memory
    /// database, and the failure was reported.
    FailedReset,
}

#[wasm_bindgen]
impl RelateDb {
    /// `readOnly` loads once and refuses writes (ADR-0136). Omit it, or pass
    /// false, to keep the writable open. `new RelateDb()` stays writable.
    #[wasm_bindgen(constructor)]
    pub fn new(read_only: Option<bool>) -> Result<RelateDb, JsError> {
        let (conn, db) = memory_db(read_only.unwrap_or(false))?;
        Ok(RelateDb {
            conn,
            db,
            backing: Backing::Memory,
        })
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
            backing: Backing::Opfs(vfs),
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
        // A failed reset parks the handle read-only, so this refusal is
        // also the second-attempt verdict on it.
        if self.db.is_read_only() {
            return Err(JsError::new("database is read-only"));
        }
        // Park on a fresh READ-ONLY in-memory database before dropping
        // the live handle, so the connection drops before the database
        // on both paths — and so that if the OPFS wipe or reopen fails
        // below, the handle stays on a database where every later write
        // fails closed, never silently landing in a memory store the
        // caller cannot see.
        let parked = memory_db(true)?;
        let old_conn = std::mem::replace(&mut self.conn, parked.0);
        let old_db = std::mem::replace(&mut self.db, parked.1);
        drop(old_conn);
        drop(old_db);
        let vfs = match std::mem::replace(&mut self.backing, Backing::FailedReset) {
            // A memory database resets to a fresh writable one: the
            // read-only park was only the drop scaffold.
            Backing::Memory => {
                let fresh = memory_db(false)?;
                self.conn = fresh.0;
                self.db = fresh.1;
                self.backing = Backing::Memory;
                return Ok(());
            }
            Backing::Opfs(vfs) => vfs,
            // Unreachable past the read-only refusal above (a failed
            // reset parks read-only); the arm carries the same verdict
            // so the match needs no panic.
            Backing::FailedReset => return Err(JsError::new("database is read-only")),
        };
        if let Err(err) = self.reopen_wiped(&vfs) {
            // The handle stays parked read-only, on Backing::FailedReset.
            return Err(to_js_err(err));
        }
        self.backing = Backing::Opfs(vfs);
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

fn memory_db(read_only: bool) -> Result<(Connection, Database), JsError> {
    let db = Database::open_with(":memory:", OpenOptions::with_mode(open_mode(read_only)))
        .map_err(to_js_err)?;
    let conn = db.connect().map_err(to_js_err)?;
    Ok((conn, db))
}

fn opfs_name(uri: &str) -> Result<String, JsError> {
    let Some(name) = uri.strip_prefix("opfs://") else {
        return Err(JsError::new(
            "OPFS open requires an opfs://name URI; use new RelateDb() for :memory:",
        ));
    };
    if !(1..=64).contains(&name.len())
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(JsError::new(
            "OPFS name must be 1-64 characters from [A-Za-z0-9_-]",
        ));
    }
    Ok(name.to_string())
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

#[cfg(test)]
mod tests {
    //! The exported surface's own pins (Node runner): the failed-reset
    //! parking contract. The handle is assembled on a fake-slot OPFS
    //! pool — the real JS prepare is stubbed out of unit tests.

    use std::sync::Arc;

    use relate::{Connection, Database, OpenOptions};
    use relate_storage::Vfs;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{Backing, RelateDb};
    use crate::opfs::tests::Pool;
    use crate::opfs::OpfsVfs;

    #[wasm_bindgen_test]
    fn a_failed_reset_parks_the_handle_where_writes_fail_closed() {
        // A failed OPFS reset must not leave the handle on a writable
        // memory database: every later write would land in a store the
        // caller cannot see. The handle parks READ-ONLY instead, so the
        // write fails closed with the engine's authored read-only
        // refusal, and the backing state names what happened.
        let pool = Pool::new(8);
        let vfs: Arc<OpfsVfs> = Arc::new(pool.open());
        let db =
            Database::open_vfs(Arc::clone(&vfs) as Arc<dyn Vfs>, OpenOptions::default()).unwrap();
        let conn = db.connect().unwrap();
        let mut handle = RelateDb {
            conn,
            db,
            backing: Backing::Opfs(Arc::clone(&vfs)),
        };
        // The wipe fails: an untouched slot's flush refuses.
        pool.fail_wipe();
        assert!(handle.reset().is_err(), "the refused flush fails the reset");

        // A subsequent write fails closed with the typed refusal — it
        // never silently lands in the invisible parked memory store.
        let envelope = handle.execute("CREATE (:NeverLanded)", None);
        assert!(
            envelope.contains("database is read-only"),
            "the write fails closed, envelope: {envelope}"
        );
    }

    #[wasm_bindgen_test]
    fn a_memory_database_reset_returns_a_fresh_writable_memory_database() {
        // The Memory arm of the same branch: a reset on an in-memory
        // handle replaces it with a fresh writable one — the read-only
        // park was only the drop scaffold.
        let (conn, db) = super::memory_db(false).unwrap();
        let mut handle = RelateDb {
            conn,
            db,
            backing: Backing::Memory,
        };
        handle.reset().unwrap();
        let envelope = handle.execute("CREATE (:Landed)", None);
        assert!(
            !envelope.contains("database is read-only"),
            "the fresh memory database is writable, envelope: {envelope}"
        );
        let found = handle.execute("MATCH (n:Landed) RETURN count(n) AS c", None);
        assert!(found.contains("\"c\""), "the write landed: {found}");
    }
}
