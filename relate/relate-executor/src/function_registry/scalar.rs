//! `Scalar`-category function rows: the single-value accessors, entity
//! accessors and value conversions, plus the `Path`-classified
//! shortest-path pair and the `UUID` family (CIP-84's category vocabulary).

use super::{FunctionCategory, FunctionMeta};
use crate::catalog_meta::{DeprecatedBy, ProcArgMeta};

pub(super) const ENTRIES: &[FunctionMeta] = &[
    FunctionMeta {
        name: "id",
        category: FunctionCategory::Scalar,
        description: "Returns the id of a `NODE` or `RELATIONSHIP`.",
        special_signature: None,
        args: &[
            ProcArgMeta::described("input", "A node or a relationship.", "NODE | RELATIONSHIP"),
        ],
        return_type: "INTEGER",
        deprecated_by: Some(DeprecatedBy::identified(
            "elementId",
            "elementId or consider using an application-generated id",
        )),
    },
    FunctionMeta::described(
        "elementId",
        FunctionCategory::Scalar,
        "Returns the element id of a `NODE` or `RELATIONSHIP`.",
        &[
            ProcArgMeta::described(
                "input",
                "An element id of a node or a relationship.",
                "NODE | RELATIONSHIP",
            ),
        ],
        "STRING",
    ),
    FunctionMeta::described(
        "type",
        FunctionCategory::Scalar,
        "Returns a `STRING` representation of the `RELATIONSHIP` type.",
        &[
            ProcArgMeta::described("input", "A relationship.", "RELATIONSHIP"),
        ],
        "STRING",
    ),
    FunctionMeta::described(
        "properties",
        FunctionCategory::Scalar,
        "Returns a `MAP` containing all the properties of a `NODE`, `RELATIONSHIP` or `MAP`.",
        &[
            ProcArgMeta::described(
                "input",
                "An entity to return the properties from.",
                "NODE | RELATIONSHIP | MAP",
            ),
        ],
        "MAP",
    ),
    FunctionMeta::described(
        "provenance",
        FunctionCategory::Scalar,
        "Returns the provenance of a property's current value on a `NODE` or `RELATIONSHIP`: \
         the newest retained event as a `MAP` of `commitTs`, `lsn`, `metadata` and `value`.",
        &[
            ProcArgMeta::described(
                "element",
                "A node or a relationship.",
                "NODE | RELATIONSHIP",
            ),
            ProcArgMeta::described(
                "key",
                "The property key whose provenance is returned.",
                "STRING",
            ),
        ],
        "MAP",
    ),
    FunctionMeta::described(
        "history",
        FunctionCategory::Scalar,
        "Returns the retained events of a property on a `NODE` or `RELATIONSHIP`, ascending \
         `commitTs`, as a `LIST<MAP>` of `commitTs`, `lsn`, `metadata` and `value` — the \
         retained events only, so the oldest need not be the birth. An omitted or `null` \
         `maxNum` returns every retained event; a non-negative integer the last `maxNum`.",
        &[
            ProcArgMeta::described(
                "element",
                "A node or a relationship.",
                "NODE | RELATIONSHIP",
            ),
            ProcArgMeta::described(
                "key",
                "The property key whose event history is returned.",
                "STRING",
            ),
            ProcArgMeta::with_default(
                "maxNum",
                "The number of newest events returned: every retained event when omitted or \
                 `null`, the last `maxNum` when a non-negative integer (0 = none).",
                "INTEGER",
                "null",
            ),
        ],
        "LIST<MAP>",
    ),
    FunctionMeta::described(
        "size",
        FunctionCategory::Scalar,
        "Returns the number of items in a `LIST<ANY>`, the number of Unicode characters in a `STRING` or the dimension of a `VECTOR`.",
        &[
            ProcArgMeta::described(
                "input",
                "A value whose length is to be calculated.",
                "STRING | VECTOR | LIST<ANY>",
            ),
        ],
        "INTEGER",
    ),
    FunctionMeta::described(
        "length",
        FunctionCategory::Scalar,
        "Returns the length of a `PATH`.",
        &[
            ProcArgMeta::described(
                "input",
                "A path whose relationships will be counted.",
                "PATH",
            ),
        ],
        "INTEGER",
    ),
    FunctionMeta::described(
        "head",
        FunctionCategory::Scalar,
        "Returns the first element in a `LIST<ANY>`.",
        &[
            ProcArgMeta::described(
                "list",
                "A list from which the first element will be returned.",
                "LIST<ANY>",
            ),
        ],
        "ANY",
    ),
    FunctionMeta::described(
        "last",
        FunctionCategory::Scalar,
        "Returns the last element in a `LIST<ANY>`.",
        &[
            ProcArgMeta::described(
                "list",
                "A list from which the last element will be returned.",
                "LIST<ANY>",
            ),
        ],
        "ANY",
    ),
    FunctionMeta {
        name: "coalesce",
        category: FunctionCategory::Scalar,
        description: "Returns the first non-null value in a list of expressions.",
        special_signature: Some("(input :: ANY ...) :: ANY"),
        args: &[
            ProcArgMeta::described(
                "input",
                "If this is the first non-`NULL` value, it will be returned.",
                "ANY",
            ),
        ],
        return_type: "ANY",
        deprecated_by: None,
    },
    FunctionMeta::described(
        "uuid",
        FunctionCategory::Uuid,
        "Returns a randomly generated `UUID`.",
        &[],
        "UUID",
    ),
    FunctionMeta::described(
        "uuid",
        FunctionCategory::Uuid,
        "Converts a `STRING` value to a `UUID`.",
        &[
            ProcArgMeta::described(
                "name",
                "The `STRING` must be 32 hexadecimal digits displayed in 5 groups, separated by 4 hyphens.",
                "STRING",
            ),
        ],
        "UUID",
    ),
    FunctionMeta::described(
        "uuid",
        FunctionCategory::Uuid,
        "Converts two `INTEGER` values to a `UUID`.",
        &[
            ProcArgMeta::described("mostSigBits", "The most significant bits.", "INTEGER"),
            ProcArgMeta::described("leastSigBits", "The least significant bits.", "INTEGER"),
        ],
        "UUID",
    ),
    FunctionMeta::described(
        "uuid.mostSignificantBits",
        FunctionCategory::Uuid,
        "Returns the most significant bits from the given `UUID`.",
        &[
            ProcArgMeta::described(
                "uuid",
                "The `UUID` to return the most significant bits from.",
                "UUID",
            ),
        ],
        "INTEGER",
    ),
    FunctionMeta::described(
        "uuid.leastSignificantBits",
        FunctionCategory::Uuid,
        "Returns the least significant bits from the given `UUID`.",
        &[
            ProcArgMeta::described(
                "uuid",
                "The `UUID` to return the least significant bits from.",
                "UUID",
            ),
        ],
        "INTEGER",
    ),
    FunctionMeta::described(
        "randomUUID",
        FunctionCategory::Scalar,
        "Generates a random UUID.",
        &[],
        "STRING",
    ),
    FunctionMeta::described(
        "toInteger",
        FunctionCategory::Scalar,
        "Converts a `BOOLEAN`, `STRING`, `INTEGER` or `FLOAT` value to an `INTEGER` value. For `BOOLEAN` values, true is defined to be 1 and false is defined to be 0.",
        &[
            ProcArgMeta::described(
                "input",
                "A value to be converted into an integer.",
                "BOOLEAN | STRING | INTEGER | FLOAT",
            ),
        ],
        "INTEGER",
    ),
    FunctionMeta::described(
        "toFloat",
        FunctionCategory::Scalar,
        "Converts a `STRING`, `INTEGER` or `FLOAT` value to a `FLOAT` value.",
        &[
            ProcArgMeta::described(
                "input",
                "A value to be converted into a float.",
                "STRING | INTEGER | FLOAT",
            ),
        ],
        "FLOAT",
    ),
    FunctionMeta::described(
        "toBoolean",
        FunctionCategory::Scalar,
        "Converts a `BOOLEAN`, `STRING` or `INTEGER` value to a `BOOLEAN` value. For `INTEGER` values, 0 is defined to be false and any other `INTEGER` is defined to be true.",
        &[
            ProcArgMeta::described(
                "input",
                "A value to be converted into a boolean.",
                "BOOLEAN | STRING | INTEGER",
            ),
        ],
        "BOOLEAN",
    ),
    FunctionMeta::described(
        "toBooleanOrNull",
        FunctionCategory::Scalar,
        "Converts a value to a `BOOLEAN` value, or null if the value cannot be converted.",
        &[
            ProcArgMeta::described(
                "input",
                "A value to be converted into a boolean or null.",
                "ANY",
            ),
        ],
        "BOOLEAN",
    ),
    FunctionMeta::described(
        "toFloatOrNull",
        FunctionCategory::Scalar,
        "Converts a value to a `FLOAT` value, or null if the value cannot be converted.",
        &[
            ProcArgMeta::described(
                "input",
                "A value to be converted into a float or null.",
                "ANY",
            ),
        ],
        "FLOAT",
    ),
    FunctionMeta::described(
        "toIntegerOrNull",
        FunctionCategory::Scalar,
        "Converts a value to an `INTEGER` value, or null if the value cannot be converted.",
        &[
            ProcArgMeta::described(
                "input",
                "A value to be converted into an integer or null.",
                "ANY",
            ),
        ],
        "INTEGER",
    ),
    FunctionMeta::described(
        "startNode",
        FunctionCategory::Scalar,
        "Returns the start `NODE` of a `RELATIONSHIP`.",
        &[
            ProcArgMeta::described("input", "A relationship.", "RELATIONSHIP"),
        ],
        "NODE",
    ),
    FunctionMeta::described(
        "endNode",
        FunctionCategory::Scalar,
        "Returns the end `NODE` of a `RELATIONSHIP`.",
        &[
            ProcArgMeta::described("input", "A relationship.", "RELATIONSHIP"),
        ],
        "NODE",
    ),
    FunctionMeta::described(
        "timestamp",
        FunctionCategory::Scalar,
        "Returns the difference, measured in milliseconds, between the current time and midnight, January 1, 1970 UTC",
        &[],
        "INTEGER",
    ),
    FunctionMeta::described(
        "nullIf",
        FunctionCategory::Scalar,
        "Returns null if the two given parameters are equivalent, otherwise returns the value of the first parameter.",
        &[
            ProcArgMeta::described(
                "v1",
                "A first value to be returned if the second value is not equivalent.",
                "ANY",
            ),
            ProcArgMeta::described(
                "v2",
                "A second value against which the first value is compared.",
                "ANY",
            ),
        ],
        "ANY",
    ),
    FunctionMeta::described(
        "valueType",
        FunctionCategory::Scalar,
        "Returns a `STRING` representation of the most precise value type that the given expression evaluates to.",
        &[
            ProcArgMeta::described("input", "A value to return the type of.", "ANY"),
        ],
        "STRING",
    ),
    FunctionMeta::described(
        "linenumber",
        FunctionCategory::Scalar,
        "Returns the line number that LOAD CSV is currently using.",
        &[],
        "INTEGER",
    ),
    FunctionMeta::described(
        "file",
        FunctionCategory::Scalar,
        "Returns the absolute path of the file that LOAD CSV is using.",
        &[],
        "STRING",
    ),
    FunctionMeta::described(
        "cardinality",
        FunctionCategory::Scalar,
        "Returns the number of keys in a `MAP`, the number of elements in a `LIST`, or the number of nodes and relationships in a `PATH`.",
        &[
            ProcArgMeta::described(
                "input",
                "A map, list or path whose cardinality is to be calculated.",
                "MAP | LIST<ANY> | PATH",
            ),
        ],
        "INTEGER",
    ),
    FunctionMeta::described(
        "char_length",
        FunctionCategory::Scalar,
        "Returns the number of Unicode characters in a `STRING`.",
        &[
            ProcArgMeta::described(
                "input",
                "A string value whose length in characters is to be calculated.",
                "STRING",
            ),
        ],
        "INTEGER",
    ),
    FunctionMeta::described(
        "character_length",
        FunctionCategory::Scalar,
        "Returns the number of Unicode characters in a `STRING`.",
        &[
            ProcArgMeta::described(
                "input",
                "A string value whose length in characters is to be calculated.",
                "STRING",
            ),
        ],
        "INTEGER",
    ),
    FunctionMeta::described(
        "path_length",
        FunctionCategory::Scalar,
        "Returns the length of a `PATH`.",
        &[
            ProcArgMeta::described(
                "input",
                "A path whose relationships will be counted.",
                "PATH",
            ),
        ],
        "INTEGER",
    ),
    FunctionMeta::described(
        "shortestPath",
        FunctionCategory::Path,
        "Returns the shortest path for a given path pattern with a variable-length relationship. If multiple shortest paths exist, one is returned non-deterministically.",
        &[
            ProcArgMeta::described(
                "pathPattern",
                "A path pattern containing a variable-length relationship.",
                "PATH",
            ),
        ],
        "PATH",
    ),
    FunctionMeta::described(
        "allShortestPaths",
        FunctionCategory::List,
        "Returns all shortest paths for a given path pattern with a variable-length relationship.",
        &[
            ProcArgMeta::described(
                "pathPattern",
                "A path pattern containing a variable-length relationship.",
                "PATH",
            ),
        ],
        "LIST<PATH>",
    ),
];
