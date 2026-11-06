//! The one module that names the concrete engines. Every other facade
//! module holds [`LiveQuery`] and [`LiveWrite`]. Each enum has one variant.
//! A later engine adds a variant; call sites stay on the enum and deref to
//! today's engine until they match on the new one.

use std::ops::{Deref, DerefMut};

pub(crate) use relate_executor::InMemoryQueryEngine;
pub(crate) use relate_write_local::LocalWriteEngine;

use relate_storage::{StoreTarget, Wal};

/// The query engine installed in a store.
pub(crate) enum LiveQuery {
    InMemory(InMemoryQueryEngine),
}

impl Deref for LiveQuery {
    type Target = InMemoryQueryEngine;

    fn deref(&self) -> &InMemoryQueryEngine {
        match self {
            Self::InMemory(engine) => engine,
        }
    }
}

impl DerefMut for LiveQuery {
    fn deref_mut(&mut self) -> &mut InMemoryQueryEngine {
        match self {
            Self::InMemory(engine) => engine,
        }
    }
}

/// The write engine installed in a store.
pub(crate) enum LiveWrite {
    Local(LocalWriteEngine),
}

impl Deref for LiveWrite {
    type Target = LocalWriteEngine;

    fn deref(&self) -> &LocalWriteEngine {
        match self {
            Self::Local(engine) => engine,
        }
    }
}

/// The only engine pair the suites run. A second pair adds a row here and a
/// variant above.
pub(crate) const ENGINE_PAIR: (&str, &str) = (
    relate_engine_api::MEMORY_QUERY_ENGINE,
    relate_engine_api::LOCAL_WRITE_ENGINE,
);

pub(crate) fn in_memory(graph: relate_executor::Graph) -> LiveQuery {
    LiveQuery::InMemory(InMemoryQueryEngine::new(graph))
}

pub(crate) fn local_write(store: StoreTarget, wal: Wal) -> LiveWrite {
    LiveWrite::Local(LocalWriteEngine::new(store, wal))
}
