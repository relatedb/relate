//! Sync access-handle slots. The real bindings live in `js/opfs.js`.
//! Unit tests stub them: the Node runner cannot load the ESM snippet.

#[cfg(target_arch = "wasm32")]
use super::js::{as_safe, checked_len, js_storage};
#[cfg(target_arch = "wasm32")]
use relate::Error;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

/// Data files prepared for one database. Must match `OPFS_SLOT_COUNT` in
/// `js/opfs.js`.
#[cfg(target_arch = "wasm32")]
pub(crate) const OPFS_SLOT_COUNT: usize = 64;

#[cfg(all(not(test), target_arch = "wasm32"))]
#[wasm_bindgen(module = "/js/opfs.js")]
extern "C" {
    #[wasm_bindgen(catch, js_name = prepareOpfs)]
    pub(crate) fn prepare_opfs(name: &str) -> Result<js_sys::Promise, JsValue>;

    #[wasm_bindgen(catch, js_name = slotSize)]
    fn slot_size(id: u32) -> Result<f64, JsValue>;

    #[wasm_bindgen(catch, js_name = slotTruncate)]
    fn slot_truncate(id: u32, len: f64) -> Result<(), JsValue>;

    #[wasm_bindgen(catch, js_name = slotFlush)]
    fn slot_flush(id: u32) -> Result<(), JsValue>;

    #[wasm_bindgen(catch, js_name = slotClose)]
    fn slot_close(id: u32) -> Result<(), JsValue>;

    #[wasm_bindgen(catch, js_name = slotRead)]
    fn slot_read(id: u32, memory: &JsValue, ptr: u32, len: u32, at: f64) -> Result<f64, JsValue>;

    #[wasm_bindgen(catch, js_name = slotWrite)]
    fn slot_write(id: u32, memory: &JsValue, ptr: u32, len: u32, at: f64) -> Result<f64, JsValue>;
}

/// Unit tests exercise the slot pool through fake slots. The browser smoke
/// links the real module; this build must not, because the Node test runner
/// cannot load the ESM snippet. The native (host) unit tests use the fakes
/// directly through `OpfsVfs::open_slots` and never reach these stubs.
#[cfg(all(test, target_arch = "wasm32"))]
fn unlinked() -> JsValue {
    JsValue::from_str("OPFS bindings are not linked into unit tests")
}

#[cfg(all(test, target_arch = "wasm32"))]
pub(crate) fn prepare_opfs(_: &str) -> Result<js_sys::Promise, JsValue> {
    Err(unlinked())
}

#[cfg(all(test, target_arch = "wasm32"))]
fn slot_size(_: u32) -> Result<f64, JsValue> {
    Err(unlinked())
}

#[cfg(all(test, target_arch = "wasm32"))]
fn slot_truncate(_: u32, _: f64) -> Result<(), JsValue> {
    Err(unlinked())
}

#[cfg(all(test, target_arch = "wasm32"))]
fn slot_flush(_: u32) -> Result<(), JsValue> {
    Err(unlinked())
}

#[cfg(all(test, target_arch = "wasm32"))]
fn slot_close(_: u32) -> Result<(), JsValue> {
    Ok(())
}

#[cfg(all(test, target_arch = "wasm32"))]
fn slot_read(_: u32, _: &JsValue, _: u32, _: u32, _: f64) -> Result<f64, JsValue> {
    Err(unlinked())
}

#[cfg(all(test, target_arch = "wasm32"))]
fn slot_write(_: u32, _: &JsValue, _: u32, _: u32, _: f64) -> Result<f64, JsValue> {
    Err(unlinked())
}

pub(crate) trait SyncSlot: Send + Sync {
    fn read_at(&self, at: u64, buf: &mut [u8]) -> relate::Result<usize>;
    fn write_at(&self, at: u64, buf: &[u8]) -> relate::Result<()>;
    fn flush(&self) -> relate::Result<()>;
    fn size(&self) -> relate::Result<u64>;
    fn truncate(&self, len: u64) -> relate::Result<()>;
}

/// The real sync-access-handle slot, wrapping the id JavaScript prepared.
#[cfg(target_arch = "wasm32")]
pub(crate) struct JsSlot {
    pub(crate) id: u32,
}

#[cfg(target_arch = "wasm32")]
impl Drop for JsSlot {
    fn drop(&mut self) {
        let _ = slot_close(self.id);
    }
}

#[cfg(target_arch = "wasm32")]
impl SyncSlot for JsSlot {
    fn read_at(&self, at: u64, buf: &mut [u8]) -> relate::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let n = slot_read(
            self.id,
            &wasm_bindgen::memory(),
            buf.as_mut_ptr() as u32,
            buf.len() as u32,
            as_safe(at)?,
        )
        .map_err(js_storage)?;
        Ok(checked_len(n)? as usize)
    }

    fn write_at(&self, mut at: u64, mut buf: &[u8]) -> relate::Result<()> {
        while !buf.is_empty() {
            let n = slot_write(
                self.id,
                &wasm_bindgen::memory(),
                buf.as_ptr() as u32,
                buf.len() as u32,
                as_safe(at)?,
            )
            .map_err(js_storage)?;
            let n = checked_len(n)? as usize;
            if n == 0 {
                return Err(Error::storage("OPFS write made no progress"));
            }
            at += n as u64;
            buf = &buf[n..];
        }
        Ok(())
    }

    fn flush(&self) -> relate::Result<()> {
        slot_flush(self.id).map_err(js_storage)
    }

    fn size(&self) -> relate::Result<u64> {
        checked_len(slot_size(self.id).map_err(js_storage)?)
    }

    fn truncate(&self, len: u64) -> relate::Result<()> {
        slot_truncate(self.id, as_safe(len)?).map_err(js_storage)
    }
}
