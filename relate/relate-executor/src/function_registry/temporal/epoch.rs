//! Epoch constructors.

use super::super::{FunctionCategory, FunctionMeta};
use crate::catalog_meta::ProcArgMeta;

pub(crate) const ENTRIES: &[FunctionMeta] = &[
    FunctionMeta::described(
        "datetime.fromepoch",
        FunctionCategory::Temporal,
        "Creates a `ZONED DATETIME` given the seconds and nanoseconds since the start of the epoch.",
        &[
            ProcArgMeta::described(
                "seconds",
                "The number of seconds from the UNIX epoch in the UTC time zone.",
                "INTEGER | FLOAT",
            ),
            ProcArgMeta::described(
                "nanoseconds",
                "The number of nanoseconds from the UNIX epoch in the UTC time zone. This can be added to seconds.",
                "INTEGER | FLOAT",
            ),
        ],
        "ZONED DATETIME",
    ),
    FunctionMeta::described(
        "datetime.fromepochmillis",
        FunctionCategory::Temporal,
        "Creates a `ZONED DATETIME` given the milliseconds since the start of the epoch.",
        &[
            ProcArgMeta::described(
                "milliseconds",
                "The number of milliseconds from the UNIX epoch in the UTC time zone.",
                "INTEGER | FLOAT",
            ),
        ],
        "ZONED DATETIME",
    ),
];
