//! Physical redo ops for the WAL (ADR-0012). The types live in
//! `relate-engine-api` so a write engine and a query engine share one
//! encoding. This module is the executor's path to them.

pub use relate_engine_api::redo::*;
