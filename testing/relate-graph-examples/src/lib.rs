//! Differential Cypher suite over the neo4j-graph-examples sandbox datasets
//! (ADR-0144): load each dataset's `data/<name>-50.dump` into a Neo4j EE
//! docker database, pull it into a `relate::Database` through the real
//! `relate pull` binary, then run the dataset's adoc walkthrough
//! queries on both engines and compare the results.
//!
//! Layers:
//! - [`manifest`] — the dataset manifest (core 5, second and third waves)
//! - [`adoc`] — `[source,cypher]` extraction + statement classification
//! - [`diff`] — result normalization + differential comparison
//! - [`cache`] — on-demand, sha-pinned dump/adoc downloads
//! - [`child`] — the child-process lifecycle (pipe drain, deadline poll,
//!   kill-and-reap, the shared wait ceremony)
//! - [`docker`] — the Neo4j EE reference container
//! - [`pull`] — end-to-end `relate pull` invocation
//! - [`reference`] — plain-query execution against Neo4j (Query API v2)
//! - [`report`] — per-query outcome rendering (the run report document, the
//!   apoc capture) over the record model
//! - [`record`] — the result record model (`QueryResult`, `Outcome`,
//!   `Summary`) and the shared per-dataset grouping
//! - [`setup`] (internal) — the pull/setup plumbing of a dataset run's
//!   artifact, pull and open phases (artifacts, CLI pull, opening the
//!   pulled copy; registration is one call made in the suite)
//! - [`sandbox`] — one whole sandbox run (dataset selection, dumps,
//!   reference boot, the bounded per-dataset runs, records out), shared by
//!   the docker-gated integration test and its A/B driver
//! - [`timings`] (internal) — engine-side query timing statistics
//!   (per-dataset p50/p95/max, slowest, the >100 ms outlier itemization)
//! - [`suite`] — one dataset's end-to-end differential run
//! - [`timebox`] — hard per-statement and per-pull-page budgets
//! - [`config`] — the shared env-knob policy (numeric parse-and-default,
//!   boolean flags, the dataset filter)
//! - [`extras`] — plain-Cypher rewrites of the skipped apoc statements,
//!   run differentially alongside the adoc walkthrough
//!
//! The differential run itself lives in `tests/differential.rs`, gated on
//! docker + `RELATE_BIN` + network.

pub mod adoc;
pub mod cache;
pub mod child;
pub mod config;
pub mod diff;
pub mod docker;
pub mod extras;
pub mod manifest;
pub mod pull;
pub mod record;
pub mod reference;
pub mod report;
pub mod sandbox;
mod setup;
pub mod suite;
pub mod timebox;
mod timings;

/// First-seen-order grouping of `(key, value)` pairs — the one generic
/// grouping derivation every per-dataset view (the report's rollup via
/// `record::group_by_dataset`, the timing tables) consumes, so no two
/// views can disagree on grouping or order policy. A cross-sibling
/// algorithm, not record-model semantics, so it lives at the crate root
/// beside the other shared policy.
pub(crate) fn group_first_seen<K: PartialEq, V>(
    items: impl IntoIterator<Item = (K, V)>,
) -> Vec<(K, Vec<V>)> {
    let mut groups: Vec<(K, Vec<V>)> = Vec::new();
    for (key, value) in items {
        match groups.iter_mut().find(|(name, _)| *name == key) {
            Some((_, values)) => values.push(value),
            None => groups.push((key, vec![value])),
        }
    }
    groups
}
