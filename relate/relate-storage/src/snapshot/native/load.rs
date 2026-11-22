//! The Parquet read path (ADR-0022): manifest read, per-kind shard load,
//! and the install tail that turns the loaded rows into a graph. Split out
//! of `mod.rs` so the writer and the reader are separate files.

use super::*;

#[cfg(test)]
pub fn load(target: &StoreTarget) -> Result<Graph> {
    Ok(load_with_checkpoint_lsn::<Graph>(target)?.graph)
}

/// Load the snapshot and the checkpoint LSN from the one manifest read,
/// reporting the decoded manifest version marker (ADR-0127 Decision 2) and
/// the ontology document the manifest selects (ADR-0120 §5; `None` when
/// the store holds none). The LSN is 0 when no manifest exists yet.
pub(crate) fn load_with_checkpoint_lsn<G: relate_engine_api::SnapshotGraph>(
    target: &StoreTarget,
) -> Result<LoadedSnapshot<G>> {
    // read_manifest rejects an unsupported format_version and classifies the
    // marker at its gate before anything else inspects the manifest
    // (ADR-0114 Decision 2, ADR-0127 Decision 2).
    let Some((manifest, marker)) = read_manifest(target)? else {
        return Ok(LoadedSnapshot::empty(0));
    };
    let checkpoint_lsn = manifest.checkpoint_lsn;
    // A legacy-store failure names the version where the loader knows
    // it (ADR-0127 Decision 4); a current store keeps its error verbatim.
    let (graph, ontology) = load_manifest_body(target, &manifest)
        .map_err(|error| crate::version::annotate_legacy_failure(error, marker))?;
    // The Parquet codec has no committed schema files (ADR-0649).
    Ok(LoadedSnapshot::loaded(
        graph,
        checkpoint_lsn,
        ontology,
        marker,
    ))
}

/// Load one kind's shards: the pre-shard monolith object when the manifest
/// predates sharding (format ≤ 2 with no parts), else every manifest part,
/// each held to the shard rules over its `[id_lo, id_hi)`;
/// [`Origin::Monolith`] says why the monolith is not. A failure names the
/// object that failed.
/// All per-kind projections derive from `kind` itself.
fn load_kind(
    target: &StoreTarget,
    graph: &dyn relate_engine_api::GraphImport,
    manifest: &Manifest,
    kind: Kind,
) -> Result<()> {
    let load = kind.loader();
    if manifest.format_version < FORMAT_VERSION && kind.parts(manifest).is_empty() {
        let name = kind.legacy_names()[0];
        if let Some(b) = get_parquet_opt(target, name)? {
            load(graph, b, Origin::Monolith)
                .map_err(|e| storage_context(e, format_args!("legacy shard {name}")))?;
        }
        return Ok(());
    }
    for part in kind.parts(manifest) {
        let b = get_parquet(target, &part.path)?;
        load(graph, b, Origin::Part(part.id_lo..part.id_hi))
            .map_err(|e| storage_context(e, format_args!("manifest part {}", part.path)))?;
    }
    Ok(())
}

fn load_manifest_body<G: relate_engine_api::SnapshotGraph>(
    target: &StoreTarget,
    manifest: &Manifest,
) -> Result<(G, Option<String>)> {
    let graph = G::default();
    graph.set_next_node_id(manifest.next_node_id);
    graph.set_next_rel_id(manifest.next_rel_id);
    graph.set_commit_ts(manifest.commit_ts);
    for kind in Kind::ALL {
        load_kind(target, &graph, manifest, kind)?;
    }
    // The load-tail commit-timestamp floor, through the one home of the
    // policy (the graph is fresh, so its live nodes are this load's
    // rows).
    crate::snapshot::ensure_commit_ts(&graph, graph.live_node_count());
    for idx in &manifest.indexes {
        graph.install_named_index(idx.clone())?;
    }
    for constraint in &manifest.constraints {
        graph.install_named_constraint(constraint.clone())?;
    }
    if let Some(theater) = &manifest.theater {
        graph.restore_checkpoint_theater(theater.clone())?;
    } else {
        graph.mark_legacy_unenforced();
    }
    graph.finish_restore();
    let ontology = manifest
        .ontology_path
        .as_deref()
        .map(|path| ontology::load_ontology_document(target, path))
        .transpose()?;
    Ok((graph, ontology))
}
