//! The checkpoint writer: assemble one generation from the graph's dirty
//! ids and publish it (ADR-0183, ADR-0534). Split out of `mod.rs`, which
//! keeps the reader, the manifest validation and the shared object helpers.

use super::*;

use crate::vfs::{BoundedSink, Refusal};
use crate::SnapshotCodecKind;
use relate_engine_api::{CheckpointTheater, GraphExport};

/// Snapshot the frozen `cut` publishes, its rewrite policy picking the
/// writer: [`Rewrite::Coalesce`] absorbs clean neighbours into every
/// rewritten unit (ADR-0183), [`Rewrite::Incremental`] rewrites only the
/// slots a dirty id seeds and leaves every clean part untouched and unread
/// (ADR-0022 Decision 2). The shard byte budget comes from the
/// configuration.
pub fn checkpoint_at(
    target: &StoreTarget,
    cut: &CheckpointCut<'_>,
    rewrite: Rewrite,
) -> Result<()> {
    write_generation(target, cut, rewrite, shard_target_bytes()?)
}

/// Test-only: the dump at a chosen shard byte target. The budget is set
/// here, inside the Parquet writer — the dump ABI never carries one.
#[cfg(test)]
pub(crate) fn checkpoint_at_with_bytes(
    target: &StoreTarget,
    graph: &dyn relate_engine_api::CheckpointDirtyMarks,
    checkpoint_lsn: u64,
    snapshot_ts: u64,
    target_bytes: u64,
    ontology: Option<&str>,
) -> Result<()> {
    let cut = CheckpointCut::take(graph, checkpoint_lsn, snapshot_ts, ontology)?;
    cut.dump_with_marks(|| write_generation(target, &cut, Rewrite::Coalesce, target_bytes))
}

fn write_generation(
    target: &StoreTarget,
    cut: &CheckpointCut<'_>,
    rewrite: Rewrite,
    target_bytes: u64,
) -> Result<()> {
    if !(1..=MAX_FILE).contains(&target_bytes) {
        return Err(Error::storage(format!(
            "Parquet shard target must be in 1..={MAX_FILE} bytes"
        )));
    }
    crate::codec::ensure_dump_shape(cut.snapshot_ts(), cut.next_node_id(), cut.next_rel_id())?;
    // Validate against the named catalog before creating *any* shard.
    let theater = cut.checkpoint_theater_for_write()?;
    let prev = read_manifest(target)?.map(|(m, _)| m);
    // A kind with no previous parts is not checked by read_manifest. Reject
    // even an unused alias before a new leaf can be created through it.
    if let Some(root) = &target.native_root {
        for kind in Kind::ALL {
            validate_dir(root, kind.dir(), "shard directory", DirAdmit::Optional)?;
        }
    }
    let mut owned = Vec::new();
    let mut ctx = ShardWrite::new(
        target,
        cut,
        cut.snapshot_ts(),
        target_bytes,
        rewrite,
        &mut owned,
    );
    // The publish phase is known statically: a prepare failure cleans up as
    // not-publishing, a publish failure as publishing.
    let prepared = prepare_manifest(&mut ctx, cut, theater, prev.as_ref())
        .map_err(|error| cleanup_unreferenced(target, &owned, Cleanup::NotPublished, error))?;
    put_object(target, MANIFEST_JSON, &prepared.bytes)
        .map_err(|error| cleanup_unreferenced(target, &owned, Cleanup::PublishAttempted, error))?;
    retire_superseded(target, prev.as_ref(), &prepared.manifest, prepared.retired);
    // The shared checkpoint tail (ADR-0481 §4/§5): the mirror publishes
    // after the manifest it reflects.
    crate::schema_files::finish_checkpoint(target, cut, cut.ontology_document())?;
    Ok(())
}

/// The invariant bundle threaded through the shard-write recursion
/// (`rewrite_kind` → `coalesce::rewrite` → `publish_part`): everything
/// except the varying id window and the kind, which each pass passes
/// explicitly.
pub(super) struct ShardWrite<'a> {
    pub(super) target: &'a StoreTarget,
    pub(super) graph: &'a dyn relate_engine_api::GraphExport,
    pub(super) ts: u64,
    pub(super) target_bytes: u64,
    pub(super) rewrite: Rewrite,
    pub(super) owned: &'a mut Vec<String>,
}

impl<'a> ShardWrite<'a> {
    /// The one construction — both kinds' rewrites and the test literals,
    /// so a field is added once.
    pub(super) fn new(
        target: &'a StoreTarget,
        graph: &'a dyn relate_engine_api::GraphExport,
        ts: u64,
        target_bytes: u64,
        rewrite: Rewrite,
        owned: &'a mut Vec<String>,
    ) -> Self {
        Self {
            target,
            graph,
            ts,
            target_bytes,
            rewrite,
            owned,
        }
    }
}

/// One prepared checkpoint generation (ADR-0183): the manifest, its
/// encoded bytes, and the part paths its rewrites retired.
struct Prepared {
    manifest: Manifest,
    bytes: Vec<u8>,
    retired: Vec<String>,
}

/// Prepare one checkpoint generation (ADR-0183, ADR-0534): rewrite both
/// kinds' parts under the store's encoding, publish the ontology object
/// the manifest selects, and assemble and revalidate the manifest —
/// everything before the manifest object itself is put. A failure here
/// cleans up as not-publishing.
fn prepare_manifest(
    ctx: &mut ShardWrite<'_>,
    cut: &CheckpointCut<'_>,
    theater: CheckpointTheater,
    prev: Option<&Manifest>,
) -> Result<Prepared> {
    let ontology = cut.ontology_document();
    let checkpoint_lsn = cut.lsn();
    // One fetch of the checkpoint's dirty marks covers both kinds below.
    let marks = ctx.graph.checkpoint_dirty();
    let (node_parts, retired_nodes) = rewrite_kind(ctx, Kind::Node, prev, &marks)?;
    let (rel_parts, retired_rels) = rewrite_kind(ctx, Kind::Rel, prev, &marks)?;
    let bytes_per_node = ewma_density(prev.map(|m| Kind::Node.bytes_per_row(m)), &node_parts)?;
    let bytes_per_rel = ewma_density(prev.map(|m| Kind::Rel.bytes_per_row(m)), &rel_parts)?;
    // The ontology object publishes before the manifest that selects it
    // (ADR-0120 §5, the ADR-0092/ADR-0089 rules): exclusive create,
    // sync, durable name — and `owned` collects it for the crash and
    // failure cleanup paths.
    let ontology_path = ontology
        .map(|document| ontology::publish_ontology(ctx.target, prev, document, ctx.owned))
        .transpose()?;
    let manifest = Manifest {
        // The Parquet codec writes the wide pair, always (ADR-0553).
        format_version: crate::version::manifest::version_for_write(ontology_path.is_some(), true),
        next_node_id: ctx.graph.next_node_id(),
        next_rel_id: ctx.graph.next_rel_id(),
        checkpoint_lsn,
        commit_ts: ctx.ts,
        node_parts,
        rel_parts,
        bytes_per_node,
        bytes_per_rel,
        codec: SnapshotCodecKind::Parquet,
        indexes: ctx.graph.named_indexes(),
        constraints: ctx.graph.named_constraints(),
        theater: Some(theater),
        ontology_path,
        query_engine: relate_engine_api::MEMORY_QUERY_ENGINE.to_owned(),
        write_engine: relate_engine_api::LOCAL_WRITE_ENGINE.to_owned(),
    };
    // Unchanged (including legacy fixed-name) parts are published again.
    // Recheck every proposed reference after writing, not just new leaves —
    // through the one established-part walk the open's read shares.
    validate_manifest_parts(ctx.target, &manifest)?;
    let bytes = encode_manifest(&manifest)?;
    Ok(Prepared {
        manifest,
        bytes,
        retired: retired_nodes.into_iter().chain(retired_rels).collect(),
    })
}

/// Rewrite one kind's parts, every live id joining the dirty set when
/// this checkpoint is the one-time legacy→wide upgrade (ADR-0553) —
/// extends, never replaces, so a part whose rows were all deleted stays
/// touched and retires instead of republishing them.
fn rewrite_kind(
    ctx: &mut ShardWrite<'_>,
    kind: Kind,
    prev: Option<&Manifest>,
    marks: &relate_engine_api::CheckpointDirty,
) -> Result<(Vec<ShardPart>, Vec<String>)> {
    let parts = prev.map(|m| kind.parts(m).to_vec()).unwrap_or_default();
    let all_ids = kind.all_ids(ctx.graph, ctx.ts);
    let mut dirty = kind.dirty_ids(marks);
    // ADR-0553: the one derivation, from the manifest this rewrite serves —
    // a previous manifest below the wide versions upgrades every part.
    let upgrading = prev.is_some_and(|m| !crate::version::manifest::writes_wide(m.format_version));
    if upgrading {
        dirty.extend(all_ids.iter().copied());
        dirty.sort_unstable();
        dirty.dedup();
    }
    let bytes_per_row = prev.map(|m| kind.bytes_per_row(m)).unwrap_or(0.0);
    let w = window_width(ctx.target_bytes, bytes_per_row);
    // ADR-0183: heal, cover, plan and write id ranges as coalesced units,
    // not per previous part.
    coalesce::rewrite(ctx, kind, parts, &all_ids, &dirty, w)
}

/// Retire superseded and legacy-monolith objects after a successful publish.
/// Windows cannot prove namespace persistence of a renamed manifest after
/// power loss, so older generations are retained there for a possible
/// fallback: [`keeps_superseded`] names that rule.
fn retire_superseded(
    target: &StoreTarget,
    prev: Option<&Manifest>,
    manifest: &Manifest,
    retired_parts: Vec<String>,
) {
    if keeps_superseded(target) {
        return;
    }
    let mut retired: Vec<String> = Vec::new();
    for p in retired_parts {
        if !referenced(manifest, &p) {
            retired.push(p);
        }
    }
    for kind in Kind::ALL {
        if prev.is_none_or(|m| kind.parts(m).is_empty()) {
            retired.extend(kind.legacy_names().map(str::to_string));
        }
    }
    // The superseded ontology object retires only now, after the manifest
    // that no longer selects it has published (ADR-0120 §5).
    if let Some(old) = prev.and_then(|m| m.ontology_path.as_deref()) {
        if manifest.ontology_path.as_deref() != Some(old) {
            retired.push(old.to_owned());
        }
    }
    // One batch keeps a `.rl` retirement to a single catalog generation.
    delete_objects(target, &retired);
}

// Stop serializing before retaining an unbounded schema image in the v3
// manifest. The same 16 MiB limit applies on both checkpoint and recovery.
fn encode_manifest(manifest: &Manifest) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        // The sink's refusal is the canonical manifest-size error — the
        // size class's own too-large constructor, the same error the
        // manifest length checks return — so the one refusal channel
        // returns it directly (ADR-0492's one-channel sink shape).
        let mut writer = BoundedSink::capture(
            &mut bytes,
            MAX_JSON_OBJECT,
            Refusal::Fixed(manifest_too_large()),
        );
        let result = serde_json::to_writer_pretty(&mut writer, manifest);
        writer.finish(
            result,
            |e| Error::storage(e.to_string()),
            // An uncharged capture never refuses an allocation.
            |bytes| Error::storage(format!("buffer allocation of {bytes} bytes failed")),
        )?;
    }
    Ok(bytes)
}

/// How far a failed checkpoint's manifest publish got, so the cleanup knows
/// whether a readable manifest is the failed generation's own.
#[derive(Clone, Copy)]
enum Cleanup {
    /// Nothing was published: an absent manifest proves every owned object
    /// is unreferenced.
    NotPublished,
    /// The publish was attempted and may have returned an error after its
    /// rename: only a readable manifest may prove an object unreferenced.
    PublishAttempted,
}

fn cleanup_unreferenced(
    target: &StoreTarget,
    owned: &[String],
    publishing: Cleanup,
    error: Error,
) -> Error {
    if owned.is_empty() {
        return error;
    }
    // Manifest replacement can return an error *after* the rename. Never
    // remove a part unless a readable manifest proves it is unreferenced.
    let manifest = match read_manifest(target) {
        Ok(Some((m, _))) => Some(m),
        Ok(None) if matches!(publishing, Cleanup::NotPublished) => None,
        _ => return error,
    };
    let mut failures = Vec::new();
    for path in owned {
        if manifest.as_ref().is_none_or(|m| !referenced(m, path)) {
            if let Err(e) = delete_object(target, path) {
                failures.push(format!("{path}: {e}"));
            }
        }
    }
    if failures.is_empty() {
        error
    } else {
        Error::storage(format!(
            "{error}; checkpoint cleanup failed: {}",
            failures.join(", ")
        ))
    }
}

pub(super) fn ewma_density(prev: Option<f64>, parts: &[ShardPart]) -> Result<f64> {
    let mut bytes = 0u64;
    let mut rows = 0u64;
    for p in parts {
        bytes = bytes
            .checked_add(p.bytes)
            .ok_or_else(|| Error::storage("manifest shard byte total overflow"))?;
        rows = rows
            .checked_add(p.rows)
            .ok_or_else(|| Error::storage("manifest shard row total overflow"))?;
    }
    if rows == 0 {
        // No sample this round: an unusable prior cannot survive one.
        return Ok(usable_prior(prev).unwrap_or(0.0));
    }
    let sample = bytes as f64 / rows as f64;
    match usable_prior(prev) {
        Some(prev) => Ok(0.7 * prev + 0.3 * sample),
        None => Ok(sample),
    }
}

/// A density is a usable prior only when it is a real positive measure;
/// anything else (absent, zero, non-finite) starts the series over.
fn usable_prior(prev: Option<f64>) -> Option<f64> {
    prev.filter(|p| *p > 0.0 && p.is_finite())
}
