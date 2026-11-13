//! Temporal truncation rows.

use super::super::{FunctionCategory, FunctionMeta};
use crate::catalog_meta::ProcArgMeta;

pub(crate) const ENTRIES: &[FunctionMeta] = &[
    FunctionMeta::described(
        "date.truncate",
        FunctionCategory::Temporal,
        "Truncates the given temporal value to a `DATE` instant using the specified unit.",
        &[
            ProcArgMeta::described(
                "unit",
                "A string representing one of the following: 'day', 'week', 'month', 'weekYear', 'quarter', 'year', 'decade', 'century', 'millennium'.",
                "STRING",
            ),
            ProcArgMeta::with_default(
                "input",
                "The date to be truncated using either `ZONED DATETIME`, `LOCAL DATETIME`, or `DATE`.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
            ProcArgMeta::with_default(
                "fields",
                "A list of time components smaller than those specified in `unit` to preserve during truncation.",
                "MAP",
                "null",
            ),
        ],
        "DATE",
    ),
    FunctionMeta::described(
        "datetime.truncate",
        FunctionCategory::Temporal,
        "Truncates the given temporal value to a `ZONED DATETIME` instant using the specified unit.",
        &[
            ProcArgMeta::described(
                "unit",
                "A string representing one of the following: 'microsecond', 'millisecond', 'second', 'minute', 'hour', 'day', 'week', 'month', 'weekYear', 'quarter', 'year', 'decade', 'century', 'millennium'.",
                "STRING",
            ),
            ProcArgMeta::with_default(
                "input",
                "The date to be truncated using either `ZONED DATETIME`, `LOCAL DATETIME`, or `DATE`.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
            ProcArgMeta::with_default(
                "fields",
                "A list of time components smaller than those specified in `unit` to preserve during truncation.",
                "MAP",
                "null",
            ),
        ],
        "ZONED DATETIME",
    ),
    FunctionMeta::described(
        "localdatetime.truncate",
        FunctionCategory::Temporal,
        "Truncates the given temporal value to a `LOCAL DATETIME` instant using the specified unit.",
        &[
            ProcArgMeta::described(
                "unit",
                "A string representing one of the following: 'microsecond', 'millisecond', 'second', 'minute', 'hour', 'day', 'week', 'month', 'weekYear', 'quarter', 'year', 'decade', 'century', 'millennium'.",
                "STRING",
            ),
            ProcArgMeta::with_default(
                "input",
                "The date to be truncated using either `ZONED DATETIME`, `LOCAL DATETIME`, or `DATE`.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
            ProcArgMeta::with_default(
                "fields",
                "A list of time components smaller than those specified in `unit` to preserve during truncation.",
                "MAP",
                "null",
            ),
        ],
        "LOCAL DATETIME",
    ),
    FunctionMeta::described(
        "localtime.truncate",
        FunctionCategory::Temporal,
        "Truncates the given temporal value to a `LOCAL TIME` instant using the specified unit.",
        &[
            ProcArgMeta::described(
                "unit",
                "A string representing one of the following: 'day', 'week', 'month', 'weekYear', 'quarter', 'year', 'decade', 'century', 'millennium'.",
                "STRING",
            ),
            ProcArgMeta::with_default(
                "input",
                "The date to be truncated using either `ZONED DATETIME`, `LOCAL DATETIME`, `LOCAL TIME`, or `DATE`.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
            ProcArgMeta::with_default(
                "fields",
                "A list of time components smaller than those specified in `unit` to preserve during truncation.",
                "MAP",
                "null",
            ),
        ],
        "LOCAL TIME",
    ),
    FunctionMeta::described(
        "time.truncate",
        FunctionCategory::Temporal,
        "Truncates the given temporal value to a `ZONED TIME` instant using the specified unit.",
        &[
            ProcArgMeta::described(
                "unit",
                "A string representing one of the following: 'microsecond', 'millisecond', 'second', 'minute', 'hour', 'day'.",
                "STRING",
            ),
            ProcArgMeta::with_default(
                "input",
                "The date to be truncated using either `ZONED DATETIME`, `LOCAL DATETIME`, `ZONED TIME`, or `LOCAL TIME`.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
            ProcArgMeta::with_default(
                "fields",
                "A list of time components smaller than those specified in `unit` to preserve during truncation.",
                "MAP",
                "null",
            ),
        ],
        "ZONED TIME",
    ),
];
