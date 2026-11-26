//! The shared ADR-0486 range-index fixture: the range index, ten
//! `:User {age}` rows, the `> 5` window statement, and the seek line and
//! posting window its plan narrates. The Bolt and HTTP surface tests seed
//! with these, so the two wire pins exercise one shape.

/// The range index and the ten rows, in statement order.
pub const RANGE_INDEX_SEED: [&str; 2] = [
    "CREATE RANGE INDEX user_age FOR (u:User) ON (u.age)",
    "UNWIND range(1, 10) AS i CREATE (:User {age: i})",
];

/// The index-backed statement over [`RANGE_INDEX_SEED`], without a plan
/// prefix.
pub const RANGE_INDEX_MATCH: &str = "MATCH (n:User) WHERE n.age > 5 RETURN n";

/// The seek line the plan carries in `Details`.
pub const RANGE_INDEX_DETAILS: &str = "RANGE INDEX n:User(age) WHERE age > 5, WHERE filter";

/// The posting window the seek carries in `EstimatedRows`.
pub const RANGE_INDEX_ESTIMATED_ROWS: i64 = 5;
