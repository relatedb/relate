//! Push a local subgraph into Neo4j/Aura (ADR-0401) — `pull`'s mirror over
//! the same Query API v2 client and the same credentials file: read the
//! local store for the requested scope, derive merge keys from the store's
//! uniqueness constraints, pre-read the remote's current keyed rows,
//! classify, and send batched parameterized UNWIND statements. Additive by
//! default, idempotent by contract; nothing in V1 deletes.

mod collect;
mod execute;
mod keys;
mod plan;
mod policy;
mod report;
mod statement;
mod wire;

use std::collections::BTreeMap;
use std::time::Instant;

use relate::Database;

use crate::{Credentials, LoadError};
pub use policy::{
    parse_key_spec, parse_updated_at_spec, PolicySpec, RelMode, UnkeyedAction, UpdatePolicy,
};
pub use report::{EntityReport, PushReport};

/// The default `--batch-size` (ADR-0401 §2).
pub const DEFAULT_BATCH_SIZE: usize = 5000;

/// What the push reads from the local store (ADR-0401 §8): exactly one of
/// `--query`, `--labels`, `--types`, or the whole store.
pub use collect::PushScope;

/// Source connection, scope, and every policy knob for [`push`]
/// (ADR-0401's flag list as one structure).
#[derive(Clone)]
pub struct PushOpts {
    /// The remote (`--to`): the same credentials as `pull`'s `--from`.
    pub credentials: Credentials,
    pub scope: PushScope,
    /// `--batch-size` (default [`DEFAULT_BATCH_SIZE`]).
    pub batch_size: usize,
    /// `--nodes-update` (global or per-label).
    pub nodes_update: PolicySpec,
    /// `--rels-update` (global or per-type).
    pub rels_update: PolicySpec,
    /// `--updated-at` candidate property names, in preference order.
    pub updated_at: Vec<String>,
    /// `--rel-mode` (default keyed: resolve per type, else one per pair).
    pub rel_mode: RelMode,
    /// `--merge-key LABEL:PROP[,PROP]` overrides, keyed by label.
    pub merge_keys: BTreeMap<String, Vec<String>>,
    /// `--rel-key TYPE:PROP[,PROP]` overrides, keyed by relationship type
    /// (ADR-0401 §5: the same precedence for rels as `--merge-key` for
    /// nodes — user override, else constraint, else structural).
    pub rel_keys: BTreeMap<String, Vec<String>>,
    /// `--unkeyed-labels` (default skip).
    pub unkeyed_labels: UnkeyedAction,
    /// `--allow-plaintext`: permit Basic auth to a non-loopback `http://`
    /// Query API host (ADR-0122, the shared client policy).
    pub allow_plaintext: bool,
    /// `--dry-run`: print the plan, touch nothing.
    pub dry_run: bool,
}

/// One push's outcome: the completed report, or — when a batch failed — the
/// partial report of exactly what landed, with the failure (ADR-0401 §6).
pub enum PushOutcome {
    Completed(PushReport),
    Partial {
        report: PushReport,
        error: LoadError,
    },
}

/// Push the scoped subgraph of `db` into the remote named by `opts`
/// (ADR-0401): read the store, derive merge keys, plan, then dry-run or
/// execute. A dry run never opens the remote connection.
///
/// The `batch_size >= 1` invariant is established here, once, before any
/// work: the CLI already rejects `--batch-size 0` at parse
/// (`parse_batch_size`) and defaults to `DEFAULT_BATCH_SIZE`, so this is
/// the API-boundary guard for direct `PushOpts` constructors — every
/// interior batch computation consumes the validated
/// [`report::BatchSize`] carrier, never the raw field.
pub fn push(opts: &PushOpts, db: &Database) -> Result<PushOutcome, LoadError> {
    let batch_size = report::BatchSize::new(opts.batch_size)
        .ok_or_else(|| LoadError::Push("PushOpts.batch_size must be at least 1".into()))?;
    let started = Instant::now();
    let graph = collect::read_scope(db, &opts.scope)?;
    let mut conn = db.connect().map_err(|e| LoadError::Engine(e.to_string()))?;
    let keys = keys::read_merge_keys(&mut conn, &opts.merge_keys, &opts.rel_keys)?;
    drop(conn);
    let plan = plan::build_plan(
        &graph,
        &keys,
        &plan::PlanOpts {
            nodes_update: opts.nodes_update.clone(),
            rels_update: opts.rels_update.clone(),
            rel_mode: opts.rel_mode,
            unkeyed_labels: opts.unkeyed_labels,
        },
    )?;
    if opts.dry_run {
        return Ok(PushOutcome::Completed(execute::dry_run_report(
            batch_size, &plan, started,
        )));
    }
    Ok(execute::execute(opts, batch_size, &plan, started))
}
