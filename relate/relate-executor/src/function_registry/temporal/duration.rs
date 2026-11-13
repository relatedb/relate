//! Duration constructors and the `duration.*` unit accessors.

use super::super::{FunctionCategory, FunctionMeta};
use crate::catalog_meta::ProcArgMeta;

/// The shared `from`/`to` parameter pair of the five between-instants rows
/// (`duration.between`, `duration_between`, `duration.inMonths`,
/// `duration.inDays`, `duration.inSeconds`): one text, one type list, one
/// order, referenced by each row so the five signatures cannot drift.
const FROM_TO_ARGS: &[ProcArgMeta] = &[            ProcArgMeta::described(
                "from",
                "A temporal instant type (`DATE`, `LOCAL TIME`, `ZONED TIME`, `LOCAL DATETIME`, `ZONED DATETIME`) representing the starting instant.",
                "ANY",
            ),
            ProcArgMeta::described(
                "to",
                "A temporal instant type (`DATE`, `LOCAL TIME`, `ZONED TIME`, `LOCAL DATETIME`, `ZONED DATETIME`) representing the ending instant.",
                "ANY",
            ),];

pub(crate) const ENTRIES: &[FunctionMeta] = &[
    FunctionMeta::described(
        "duration",
        FunctionCategory::Temporal,
        "Creates a `DURATION` value.",
        &[
            ProcArgMeta::described(
                "input",
                "Either a string representation of a duration value or a map optionally containing the following keys: 'years', 'months', 'weeks', 'days', 'hours', 'minutes', 'seconds', 'milliseconds', 'microseconds', or 'nanoseconds'.",
                "ANY",
            ),
            ProcArgMeta::with_default(
                "pattern",
                "A pattern used to parse a duration value. If the pattern is not provided the value will be parsed according to default patterns.",
                "STRING",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "DURATION",
    ),
    FunctionMeta::described(
        "duration.between",
        FunctionCategory::Temporal,
        "Computes the `DURATION` between the `from` instant (inclusive) and the `to` instant (exclusive) in logical units.",
        FROM_TO_ARGS,
        "DURATION",
    ),
    FunctionMeta::described(
        "duration_between",
        FunctionCategory::Temporal,
        "Computes the `DURATION` between the `from` instant (inclusive) and the `to` instant (exclusive) in logical units.",
        FROM_TO_ARGS,
        "DURATION",
    ),
    FunctionMeta::described(
        "duration.inMonths",
        FunctionCategory::Temporal,
        "Computes the `DURATION` between the `from` instant (inclusive) and the `to` instant (exclusive) in months.",
        FROM_TO_ARGS,
        "DURATION",
    ),
    FunctionMeta::described(
        "duration.inDays",
        FunctionCategory::Temporal,
        "Computes the `DURATION` between the `from` instant (inclusive) and the `to` instant (exclusive) in days.",
        FROM_TO_ARGS,
        "DURATION",
    ),
    FunctionMeta::described(
        "duration.inSeconds",
        FunctionCategory::Temporal,
        "Computes the `DURATION` between the `from` instant (inclusive) and the `to` instant (exclusive) in seconds.",
        FROM_TO_ARGS,
        "DURATION",
    ),
];
