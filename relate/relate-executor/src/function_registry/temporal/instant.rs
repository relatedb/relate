//! Temporal instant constructors and their `.transaction` /
//! `.statement` / `.realtime` clock spellings.

use super::super::{FunctionCategory, FunctionMeta};
use crate::catalog_meta::ProcArgMeta;

pub(crate) const ENTRIES: &[FunctionMeta] = &[
    FunctionMeta::described(
        "date",
        FunctionCategory::Temporal,
        "Creates a `DATE` instant.",
        &[
            ProcArgMeta::with_default(
                "input",
                "Either a string representation of a temporal value, a map containing the single key 'timezone', or a map containing temporal values ('date', 'year', 'month', 'day', 'week', 'dayOfWeek', 'quarter', 'dayOfQuarter', 'ordinalDay') as components.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
            ProcArgMeta::with_default(
                "pattern",
                "A pattern used to parse a date value. If the pattern is not provided the value will be parsed according to default patterns.",
                "STRING",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "DATE",
    ),
    FunctionMeta::described(
        "date.transaction",
        FunctionCategory::Temporal,
        "Returns the current `DATE` instant using the transaction clock.",
        &[
            ProcArgMeta::with_default(
                "timezone",
                "A string value representing a time zone.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "DATE",
    ),
    FunctionMeta::described(
        "date.statement",
        FunctionCategory::Temporal,
        "Returns the current `DATE` instant using the statement clock.",
        &[
            ProcArgMeta::with_default(
                "timezone",
                "A string value representing a time zone.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "DATE",
    ),
    FunctionMeta::described(
        "date.realtime",
        FunctionCategory::Temporal,
        "Returns the current `DATE` instant using the realtime clock.",
        &[
            ProcArgMeta::with_default(
                "timezone",
                "A string value representing a time zone.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "DATE",
    ),
    FunctionMeta::described(
        "datetime",
        FunctionCategory::Temporal,
        "Creates a `ZONED DATETIME` instant.",
        &[
            ProcArgMeta::with_default(
                "input",
                "Either a string representation of a temporal value, a map containing the single key 'timezone', or a map containing temporal values ('year', 'month', 'day', 'hour', 'minute', 'second', 'millisecond', 'microsecond', 'nanosecond', 'timezone', 'epochSeconds', 'epochMillis') as components.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
            ProcArgMeta::with_default(
                "pattern",
                "A pattern used to parse a zoned datetime value. If the pattern is not provided the value will be parsed according to default patterns.",
                "STRING",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "ZONED DATETIME",
    ),
    FunctionMeta::described(
        "datetime.transaction",
        FunctionCategory::Temporal,
        "Returns the current `ZONED DATETIME` instant using the transaction clock.",
        &[
            ProcArgMeta::with_default(
                "timezone",
                "A string value representing a time zone.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "ZONED DATETIME",
    ),
    FunctionMeta::described(
        "datetime.statement",
        FunctionCategory::Temporal,
        "Returns the current `ZONED DATETIME` instant using the statement clock.",
        &[
            ProcArgMeta::with_default(
                "timezone",
                "A string value representing a time zone.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "ZONED DATETIME",
    ),
    FunctionMeta::described(
        "datetime.realtime",
        FunctionCategory::Temporal,
        "Returns the current `ZONED DATETIME` instant using the realtime clock.",
        &[
            ProcArgMeta::with_default(
                "timezone",
                "A string value representing a time zone.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "ZONED DATETIME",
    ),
    FunctionMeta::described(
        "localdatetime",
        FunctionCategory::Temporal,
        "Creates a `LOCAL DATETIME` instant.",
        &[
            ProcArgMeta::with_default(
                "input",
                "Either a string representation of a temporal value, a map containing the single key 'timezone', or a map containing temporal values ('year', 'month', 'day', 'hour', 'minute', 'second', 'millisecond', 'microsecond', 'nanosecond') as components.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
            ProcArgMeta::with_default(
                "pattern",
                "A pattern used to parse a local datetime value. If the pattern is not provided the value will be parsed according to default patterns.",
                "STRING",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "LOCAL DATETIME",
    ),
    FunctionMeta::described(
        "localdatetime.transaction",
        FunctionCategory::Temporal,
        "Returns the current `LOCAL DATETIME` instant using the transaction clock.",
        &[
            ProcArgMeta::with_default(
                "timezone",
                "A string value representing a time zone.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "LOCAL DATETIME",
    ),
    FunctionMeta::described(
        "localdatetime.statement",
        FunctionCategory::Temporal,
        "Returns the current `LOCAL DATETIME` instant using the statement clock.",
        &[
            ProcArgMeta::with_default(
                "timezone",
                "A string value representing a time zone.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "LOCAL DATETIME",
    ),
    FunctionMeta::described(
        "localdatetime.realtime",
        FunctionCategory::Temporal,
        "Returns the current `LOCAL DATETIME` instant using the realtime clock.",
        &[
            ProcArgMeta::with_default(
                "timezone",
                "A string value representing a time zone.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "LOCAL DATETIME",
    ),
    FunctionMeta::described(
        "localtime",
        FunctionCategory::Temporal,
        "Creates a `LOCAL TIME` instant.",
        &[
            ProcArgMeta::with_default(
                "input",
                "Either a string representation of a temporal value, a map containing the single key 'timezone', or a map containing temporal values ('hour, 'minute', 'second', 'millisecond', 'microsecond', 'nanosecond' as components.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
            ProcArgMeta::with_default(
                "pattern",
                "A pattern used to parse a local time value. If the pattern is not provided the value will be parsed according to default patterns.",
                "STRING",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "LOCAL TIME",
    ),
    FunctionMeta::described(
        "localtime.transaction",
        FunctionCategory::Temporal,
        "Returns the current `LOCAL TIME` instant using the transaction clock.",
        &[
            ProcArgMeta::with_default(
                "timezone",
                "A string value representing a time zone.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "LOCAL TIME",
    ),
    FunctionMeta::described(
        "localtime.statement",
        FunctionCategory::Temporal,
        "Returns the current `LOCAL TIME` instant using the statement clock.",
        &[
            ProcArgMeta::with_default(
                "timezone",
                "A string value representing a time zone.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "LOCAL TIME",
    ),
    FunctionMeta::described(
        "localtime.realtime",
        FunctionCategory::Temporal,
        "Returns the current `LOCAL TIME` instant using the realtime clock.",
        &[
            ProcArgMeta::with_default(
                "timezone",
                "A string value representing a time zone.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "LOCAL TIME",
    ),
    FunctionMeta::described(
        "time",
        FunctionCategory::Temporal,
        "Creates a `ZONED TIME` instant.",
        &[
            ProcArgMeta::with_default(
                "input",
                "Either a string representation of a temporal value, a map containing the single key 'timezone', or a map containing temporal values ('hour', 'minute', 'second', 'millisecond', 'microsecond', 'nanosecond', 'timezone') as components.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
            ProcArgMeta::with_default(
                "pattern",
                "A pattern used to parse a zoned time value. If the pattern is not provided the value will be parsed according to default patterns.",
                "STRING",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "ZONED TIME",
    ),
    FunctionMeta::described(
        "time.transaction",
        FunctionCategory::Temporal,
        "Returns the current `ZONED TIME` instant using the transaction clock.",
        &[
            ProcArgMeta::with_default(
                "timezone",
                "A string value representing a time zone.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "ZONED TIME",
    ),
    FunctionMeta::described(
        "time.statement",
        FunctionCategory::Temporal,
        "Returns the current `ZONED TIME` instant using the statement clock.",
        &[
            ProcArgMeta::with_default(
                "timezone",
                "A string value representing a time zone.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "ZONED TIME",
    ),
    FunctionMeta::described(
        "time.realtime",
        FunctionCategory::Temporal,
        "Returns the current `ZONED TIME` instant using the realtime clock.",
        &[
            ProcArgMeta::with_default(
                "timezone",
                "A string value representing a time zone.",
                "ANY",
                "DEFAULT_TEMPORAL_ARGUMENT",
            ),
        ],
        "ZONED TIME",
    ),
];
