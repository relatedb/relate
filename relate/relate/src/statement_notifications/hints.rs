//! The `USING INDEX` / `USING JOIN ON` hint notifications (ADR-0363).
//!
//! Live-verified against 2026.10.1 (2026-11-07/08): a `USING INDEX` hint
//! the catalog cannot serve warns `01N31` — one notification per
//! unfulfilled hint, naming the hinted index — and a `USING JOIN ON` hint
//! warns `01N30` per hint. Relate's planner is not cost-based: it never
//! plans a hint-directed join, so every join hint is unfulfillable. An
//! index hint is served by any node index on the hinted label that covers
//! every hinted property and is not a vector index — live fulfills the
//! hint through range, text, point and fulltext indexes alike (an
//! equality, range or string predicate), and never through a vector one.
//! Both kinds are occurrence-scoped (one row per hint, never collapsed),
//! matching live's per-hint reporting.
//!
//! A hint belongs to its own `MATCH` clause and names its own label or
//! join keys — there is no scope whose parts could collide (unlike the
//! 03N95 variable facts) — so unlike that detector there is no
//! accumulator: the hints fire where they are observed, straight into the
//! statement's notification list.

use relate_executor::{IndexFacts, IndexKindSpec};
use relate_model::wire::{Notification, NotificationKind};
use relate_parser::{IndexHint, JoinHint};

/// Fire one `MATCH` clause's hints: each index hint no catalog node index
/// can serve warns `01N31`, and every join hint warns `01N30`.
pub(super) fn observe(
    notifications: &mut Vec<Notification>,
    index_hints: &[IndexHint],
    join_hints: &[JoinHint],
    index_facts: &[IndexFacts],
) {
    for hint in index_hints {
        // Any node index on the hinted label whose property list covers
        // every hinted property (order-insensitive — an exact match or a
        // composite superset) serves the hint, except a vector index:
        // live 2026.10.1 fulfills the hint through range, text, point
        // and fulltext indexes alike for equality, range and string
        // predicates, and never through a vector one (probed 2026-11-08:
        // text/point/fulltext suppress 01N31, vector fires it). A
        // relationship index never serves a `USING INDEX` hint, which is
        // always a node hint; a lookup index carries no properties, so
        // the coverage check already excludes it.
        let served = index_facts.iter().any(|facts| {
            !facts.relationship
                && facts.kind != IndexKindSpec::Vector
                && hint
                    .label
                    .with_str(|label| facts.labels.iter().any(|l| l == label))
                && hint
                    .properties
                    .iter()
                    .all(|p| p.with_str(|prop| facts.properties.iter().any(|x| x == prop)))
        });
        if !served {
            let properties = hint.properties_text();
            notifications.push(Notification::new(
                NotificationKind::HintedIndexNotFound,
                format!(
                    "warn: hinted index does not exist. Unable to create a plan with \
                     'INDEX :{}({})' because the index does not exist.",
                    hint.label, properties
                ),
            ));
        }
    }
    for hint in join_hints {
        let variables = hint
            .variables
            .iter()
            .map(|v| format!("`{v}`"))
            .collect::<Vec<_>>()
            .join(", ");
        notifications.push(Notification::new(
            NotificationKind::JoinHintUnfulfillable,
            format!(
                "warn: join hint unfulfillable. Unable to create a plan with \
                 'JOIN ON {variables}'. Try to change the join key(s) or restructure \
                 your query."
            ),
        ));
    }
}
