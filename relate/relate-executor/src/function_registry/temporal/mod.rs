//! `Temporal` function rows, split by family so no one
//! table carries the whole catalogue: the instant constructors and their
//! clocks, the truncation family, the GQL alias spellings, the duration
//! family, the epoch constructors and `format`. Each sub-table is listed in
//! [`super::FUNCTION_META_TABLES`] beside the other families.

pub(super) mod duration;
pub(super) mod epoch;
pub(super) mod format;
pub(super) mod gql;
pub(super) mod instant;
pub(super) mod truncate;
