//! `Vfs` over the slot pool.

#[cfg(target_arch = "wasm32")]
use std::collections::BTreeMap;
use std::io::SeekFrom;
use std::sync::{Arc, Mutex};

use relate::Error;
use relate_storage::{Vfs, VfsFile, VfsMeta, VfsOpen};

use super::catalog::{read_catalog, Inner};
use super::path::{check_path, family_keys, is_prefix, list_names};
use super::slot::SyncSlot;

pub(crate) struct OpfsVfs {
    inner: Arc<Mutex<Inner>>,
}

#[cfg(target_arch = "wasm32")]
mod prepare {
    use super::*;

    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;

    use crate::opfs::js::{checked_u32, js_open_err, js_storage, js_u32};
    use crate::opfs::slot::{prepare_opfs, JsSlot, OPFS_SLOT_COUNT};

    impl super::OpfsVfs {
        pub(crate) async fn prepare(name: &str) -> relate::Result<Arc<Self>> {
            let promise = prepare_opfs(name).map_err(js_storage)?;
            let prepared = JsFuture::from(promise).await.map_err(js_open_err)?;
            Self::from_prepared(&prepared)
        }

        fn from_prepared(prepared: &JsValue) -> relate::Result<Arc<Self>> {
            // Wrap each id before the next check. Drop closes every handle that
            // was accepted, including when the slot count is wrong.
            let catalog = Box::new(JsSlot {
                id: js_u32(prepared, "catalog")?,
            });
            let slots_js =
                js_sys::Reflect::get(prepared, &JsValue::from_str("slots")).map_err(js_storage)?;
            let slots_js = js_sys::Array::from(&slots_js);
            let mut slots = Vec::with_capacity(slots_js.length() as usize);
            for (index, id) in slots_js.iter().enumerate() {
                let key = format!("slot {index}");
                let Some(id) = id.as_f64() else {
                    return Err(relate::Error::storage(format!(
                        "OPFS {key} is not a number"
                    )));
                };
                slots.push(Box::new(JsSlot {
                    id: checked_u32(id, &key)?,
                }) as Box<dyn SyncSlot>);
            }
            if slots.len() != OPFS_SLOT_COUNT {
                return Err(relate::Error::storage(format!(
                    "OPFS prepare returned {} slots, expected {OPFS_SLOT_COUNT}",
                    slots.len()
                )));
            }
            Self::open_slots(catalog, slots).map(Arc::new)
        }
    }
}

impl OpfsVfs {
    pub(crate) fn open_slots(
        catalog: Box<dyn SyncSlot>,
        slots: Vec<Box<dyn SyncSlot>>,
    ) -> relate::Result<Self> {
        if slots.is_empty() {
            return Err(Error::storage("OPFS slot pool is empty"));
        }
        let entries = read_catalog(catalog.as_ref())?;
        let mut used = vec![false; slots.len()];
        for slot in entries.values() {
            let index = *slot as usize;
            if index >= slots.len() || used[index] {
                return Err(Error::storage("corrupt OPFS catalog"));
            }
            used[index] = true;
        }
        let freelist = (0..slots.len() as u32)
            .filter(|i| !used[*i as usize])
            .collect();
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                catalog,
                slots,
                entries,
                freelist,
            })),
        })
    }

    /// The JS surface's reset tail (bindings.rs): wipe every slot and
    /// rebind the catalog empty. Wasm32-only — the engine never calls it.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn wipe(&self) -> relate::Result<()> {
        self.with_mut(|inner| {
            let freelist = (0..inner.slots.len() as u32).collect();
            inner.commit(BTreeMap::new(), freelist)?;
            for slot in &inner.slots {
                slot.truncate(0)?;
                slot.flush()?;
            }
            Ok(())
        })
    }

    fn with_mut<T>(&self, f: impl FnOnce(&mut Inner) -> relate::Result<T>) -> relate::Result<T> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }
}

impl Vfs for OpfsVfs {
    fn durable(&self) -> bool {
        true
    }

    fn create_dir_all(&self, _path: &str) -> relate::Result<()> {
        Ok(())
    }

    fn create_dir_all_for_atomic(&self, _path: &str) -> relate::Result<()> {
        self.with_mut(|inner| inner.publish())
    }

    fn list_dir(&self, path: &str) -> relate::Result<Vec<String>> {
        self.with_mut(|inner| Ok(list_names(&inner.entries, path)))
    }

    fn exists(&self, path: &str) -> relate::Result<bool> {
        self.with_mut(|inner| {
            Ok(inner.entries.contains_key(path) || is_prefix(&inner.entries, path))
        })
    }

    fn metadata(&self, path: &str) -> relate::Result<VfsMeta> {
        self.with_mut(|inner| {
            if let Some(slot) = inner.entries.get(path).copied() {
                return Ok(VfsMeta::file(inner.slots[slot as usize].size()?, None));
            }
            if is_prefix(&inner.entries, path) {
                return Ok(VfsMeta::dir());
            }
            Err(Error::io(format!("missing {path}")))
        })
    }

    fn open(&self, path: &str, opts: VfsOpen) -> relate::Result<Box<dyn VfsFile>> {
        check_path(path)?;
        let inner = Arc::clone(&self.inner);
        let mut guard = inner.lock().unwrap_or_else(|e| e.into_inner());
        let exists = guard.entries.contains_key(path);
        let dir = is_prefix(&guard.entries, path);
        if opts.create_new {
            if exists || dir {
                return Err(Error::io(format!("already exists: {path}")));
            }
            guard.alloc(path)?;
        } else if opts.create {
            if dir {
                return Err(Error::io(format!("already exists: {path}")));
            }
            if !exists {
                guard.alloc(path)?;
            }
        } else if !exists {
            return Err(Error::io(format!("missing {path}")));
        }
        if opts.truncate {
            let slot = guard.require(path)?;
            guard.slots[slot as usize].truncate(0)?;
            guard.slots[slot as usize].flush()?;
        }
        let pos = if opts.append {
            let slot = guard.require(path)?;
            guard.slots[slot as usize].size()?
        } else {
            0
        };
        drop(guard);
        Ok(Box::new(SlotFile {
            inner,
            path: path.to_string(),
            pos,
            append: opts.append,
        }))
    }

    fn remove_file(&self, path: &str) -> relate::Result<()> {
        self.with_mut(|inner| {
            // A missing file is already the wanted state — no catalog write.
            if !inner.entries.contains_key(path) {
                return Ok(());
            }
            inner.stage(|entries, freelist| {
                let slot = entries
                    .remove(path)
                    .expect("the pre-check proved the entry present");
                freelist.push(slot);
                Ok(())
            })
        })
    }

    fn remove_dir_all(&self, path: &str) -> relate::Result<()> {
        self.with_mut(|inner| {
            inner.stage(|entries, freelist| {
                for (key, _suffix) in family_keys(entries, path) {
                    let slot = entries
                        .remove(&key)
                        .expect("the key was just collected from the family");
                    freelist.push(slot);
                }
                Ok(())
            })
        })
    }

    fn rename(&self, from: &str, to: &str) -> relate::Result<()> {
        check_path(from)?;
        check_path(to)?;
        self.with_mut(|inner| {
            if from == to {
                return Ok(());
            }
            // Directories are prefix families (ADR-0145): the rename
            // re-keys the bare entry and every `from/`-prefixed child
            // through the one family decision, composing each new name
            // from the suffix the decision carries — an implicit
            // directory (no bare entry, children only) renames cleanly
            // and a directory rename moves the whole tree (the engine's
            // legacy-sidecar adoption, ADR-0623, renames exactly that
            // directory through this call). A destination key that
            // already exists is replaced and its slot recycled, the
            // same replace semantics a single-entry rename always had.
            inner.stage(|entries, freelist| {
                let moved = family_keys(entries, from);
                if moved.is_empty() {
                    return Err(Error::io(format!("missing {from}")));
                }
                for (key, suffix) in moved {
                    let slot = entries
                        .remove(&key)
                        .expect("the key was just collected from the family");
                    let target = if suffix.is_empty() {
                        to.to_string()
                    } else {
                        format!("{to}/{suffix}")
                    };
                    if let Some(old) = entries.insert(target, slot) {
                        freelist.push(old);
                    }
                }
                Ok(())
            })
        })
    }

    fn sync_dir(&self, _path: &str) -> relate::Result<()> {
        self.with_mut(|inner| inner.publish())
    }
}

struct SlotFile {
    inner: Arc<Mutex<Inner>>,
    path: String,
    pos: u64,
    append: bool,
}

impl SlotFile {
    fn with_slot<T>(
        &self,
        f: impl FnOnce(&dyn SyncSlot) -> relate::Result<T>,
    ) -> relate::Result<T> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let slot = guard.require(&self.path)?;
        f(guard.slots[slot as usize].as_ref())
    }
}

impl VfsFile for SlotFile {
    fn write_all(&mut self, buf: &[u8]) -> relate::Result<()> {
        let mut pos = self.pos;
        self.with_slot(|slot| {
            if self.append {
                pos = slot.size()?;
            }
            slot.write_at(pos, buf)?;
            pos += buf.len() as u64;
            Ok(())
        })?;
        self.pos = pos;
        Ok(())
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> relate::Result<()> {
        let mut filled = 0;
        while filled < buf.len() {
            let n = self.with_slot(|slot| slot.read_at(self.pos, &mut buf[filled..]))?;
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof").into());
            }
            filled += n;
            self.pos += n as u64;
        }
        Ok(())
    }

    fn seek(&mut self, pos: SeekFrom) -> relate::Result<u64> {
        let len = self.with_slot(|slot| slot.size())?;
        let next = match pos {
            SeekFrom::Start(n) => n,
            SeekFrom::End(n) => (len as i64).saturating_add(n).max(0) as u64,
            SeekFrom::Current(n) => (self.pos as i64).saturating_add(n).max(0) as u64,
        };
        self.pos = next;
        Ok(self.pos)
    }

    fn sync_all(&mut self) -> relate::Result<()> {
        self.with_slot(|slot| slot.flush())
    }

    fn durability_barrier(&mut self) -> relate::Result<()> {
        self.sync_all()
    }

    fn set_len(&mut self, len: u64) -> relate::Result<()> {
        self.with_slot(|slot| slot.truncate(len))?;
        self.pos = self.pos.min(len);
        Ok(())
    }

    fn len(&self) -> relate::Result<u64> {
        self.with_slot(|slot| slot.size())
    }
}
