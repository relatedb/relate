//! Identities a store records so it reopens with the engines that wrote it.
//! A store written before engines were named reads as the in-memory query
//! engine and the local write engine.

/// Query engine identity stored in a checkpoint manifest.
pub const MEMORY_QUERY_ENGINE: &str = "in-memory";

/// Write engine identity stored in a checkpoint manifest.
pub const LOCAL_WRITE_ENGINE: &str = "local";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryEngineId {
    InMemory,
}

impl QueryEngineId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InMemory => MEMORY_QUERY_ENGINE,
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            MEMORY_QUERY_ENGINE => Some(Self::InMemory),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteEngineId {
    Local,
}

impl WriteEngineId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => LOCAL_WRITE_ENGINE,
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            LOCAL_WRITE_ENGINE => Some(Self::Local),
            _ => None,
        }
    }
}

/// The pair a manifest records. Absent fields mean the historical engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineIds {
    pub query: QueryEngineId,
    pub write: WriteEngineId,
}

impl Default for EngineIds {
    fn default() -> Self {
        Self {
            query: QueryEngineId::InMemory,
            write: WriteEngineId::Local,
        }
    }
}

impl EngineIds {
    pub fn parse(query: &str, write: &str) -> Option<Self> {
        Some(Self {
            query: QueryEngineId::parse(query)?,
            write: WriteEngineId::parse(write)?,
        })
    }
}
