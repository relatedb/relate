//! `format()`.

use super::super::{FunctionCategory, FunctionMeta};
use crate::catalog_meta::ProcArgMeta;

pub(crate) const ENTRIES: &[FunctionMeta] = &[
    FunctionMeta::described(
        "format",
        FunctionCategory::Temporal,
        "Returns the temporal value as an ISO-formatted string or as a string formatted by the provided pattern.",
        &[
            ProcArgMeta::described(
                "value",
                "A temporal value to be formatted.",
                "DATE | LOCAL TIME | ZONED TIME | LOCAL DATETIME | ZONED DATETIME | DURATION",
            ),
        ],
        "STRING",
    ),
    FunctionMeta::described(
        "format",
        FunctionCategory::Temporal,
        "Returns the temporal value as an ISO-formatted string or as a string formatted by the provided pattern.",
        &[
            ProcArgMeta::described(
                "value",
                "A temporal value to be formatted.",
                "DATE | LOCAL TIME | ZONED TIME | LOCAL DATETIME | ZONED DATETIME | DURATION",
            ),
            ProcArgMeta::described(
                "pattern",
                "A pattern used to format the temporal value. If the pattern is not provided the value will be formatted according to ISO 8601.",
                "STRING",
            ),
        ],
        "STRING",
    ),
];
