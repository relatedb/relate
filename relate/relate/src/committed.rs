//! The committed schema's open-tail application (ADR-0649): what a
//! file-based store's format defines as engine-read, user-owned,
//! version-controlled schema — extracted and validated by the
//! codec's load, carried by [`CommittedSchema`] — becomes the
//! store's runtime state here, never inside the load. Two reasons
//! the tail is the only right place:
//!
//! - The schema text mirror's open-time verification (ADR-0481 §4)
//!   compares the **checkpointed** state; a committed change applied
//!   inside the load would read as drift and refuse the first open
//!   after any file edit — the exact pull-then-open flow the file
//!   exists for. The tail runs after the verification.
//! - The application is the replay path's own machinery: the catalog
//!   facts go in as the redo ops DDL commits record
//!   (`CreateIndex` + `CatalogIndexDetail`, `SetCurrentGraphType`),
//!   through `Graph::apply_redo` — every invariant the redo
//!   applier enforces (group validation, the theater's derived
//!   constraints, the postings floor) holds for a committed schema
//!   exactly as it holds for a WAL-replayed one. No bespoke
//!   mutation seam exists or is needed.
//!
//! The application splits in two halves, because the fresh open and
//! the follow reload need different orderings around the graph swap:
//! [`apply_facts`] runs the redo group against a graph (pure — no
//! `Shared` state), and [`crate::ontology::install_state`] swaps the
//! document slot and the set/drop refusal flag — the store-level
//! ontology state protocol lives in the module that owns the slot. A fresh open runs facts on the
//! store's own graph, then installs the state; a follow reload runs
//! facts on the **replacement** graph *before* the swap, so a
//! failing application refuses the reload with the old graph still
//! serving — never a half-applied state.

use relate_executor::Graph;

use crate::{Error, Result, Shared};

/// Apply a store's committed schema at the open tail — after the
/// schema mirror verification, after the WAL replay, before the first
/// connection. Idempotent by construction: every open re-applies the
/// file's facts, which override same-named catalog state the way any
/// later commit would, so a machine whose sidecar predates a file
/// edit converges without a checkpoint and a converged machine is a
/// no-op.
pub(crate) fn apply(shared: &Shared, committed: relate_storage::CommittedSchema) -> Result<()> {
    let relate_storage::CommittedSchema {
        document,
        graph_type,
        indexes,
    } = committed;
    apply_facts(&shared.graph, &graph_type, &indexes)?;
    crate::ontology::install_state(shared, document);
    Ok(())
}

/// The graph-facts half: the redo group a DDL commit would record —
/// each index's row then its typed detail (the order the DDL path
/// pushes), then the current graph type, whose derived constraints
/// the redo applier re-derives with it. Pure over the graph: no
/// `Shared` slot is touched, so the follow reload can run it against
/// the replacement graph before the swap.
pub(crate) fn apply_facts(
    graph: &Graph,
    graph_type: &relate_engine_api::graph_type::GraphTypeSpec,
    indexes: &[relate_storage::CommittedIndex],
) -> Result<()> {
    // The catalog redo group is the DDL home's own construction
    // (ADR-0649 §6): the op order, the detail shape, the option
    // resolution and the held-row retirement (the committed file is
    // the source and wins, decision 2 — a file edit that retargets or
    // renames an index overrides the machine's stale row the way any
    // later commit would, instead of failing the open on the catalog's
    // duplicate rules) live with `CREATE INDEX` in relate-executor,
    // under one compiler link. This tail keeps only the application.
    let held = graph.named_indexes();
    let facts: Vec<relate_executor::CommittedIndexFact<'_>> = indexes
        .iter()
        .map(|index| relate_executor::CommittedIndexFact {
            name: &index.name,
            detail: &index.detail,
            options: &index.options,
        })
        .collect();
    let ops = relate_executor::committed_catalog_ops(&held, &facts, graph_type)?;
    // Applied through the redo path's own preflight contract: the
    // group's timestamp advances past the loaded state (the applier's
    // ordering invariant), and nothing is appended to the WAL — the
    // advance is in-memory state, recorded durably only by the next
    // publishing checkpoint, whose dirty tracking the applier already
    // marked. A clean session's no-op exit writes nothing (ADR-0646),
    // so an unchanged sidecar simply re-applies at the same +1 on the
    // next open: the advance does not accumulate without a checkpoint.
    // The overflow is the typed error the commit pipeline gives the
    // same counter (never a panic over a corrupt sidecar's u64::MAX).
    let ts = graph
        .commit_ts()
        .checked_add(1)
        .ok_or_else(|| Error::storage("commit timestamp overflow"))?;
    graph.apply_redo(ops, ts)
}
